//! Full server logs (port of `src/events/full-logs.js`).
//!
//! Every captured event becomes one JSON line in `<logsDir>/server/<guildId>/<YYYY-MM-DD>.jsonl`
//! (same entry shape as JS) and a compact Components V2 card on the configured log channel.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use twilight_cache_inmemory::DefaultInMemoryCache;
use twilight_model::channel::Channel;
use twilight_model::gateway::event::Event;
use twilight_model::guild::audit_log::{AuditLogEntry, AuditLogEventType};
use twilight_model::guild::{Permissions, Role};
use twilight_model::id::Id;
use twilight_model::id::marker::{ChannelMarker, GuildMarker, UserMarker};
use twilight_model::user::User;

use crate::app::{App, SharedApp};
use crate::config;
use crate::discord::{self, FLAG_COMPONENTS_V2, FLAG_EPHEMERAL};
use crate::util::text::{block, inline, is_snowflake, truncate};
use operator_common::time::{now_iso, parse_iso};

const AUDIT_CACHE_TTL: Duration = Duration::from_millis(1500);
const AUDIT_MATCH_WINDOW_MS: i64 = 30_000;
const RETENTION_SWEEP_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const WRITE_BATCH_DELAY: Duration = Duration::from_millis(25);
const AUDIT_FETCH_LIMIT: u16 = 10;
const MAX_SUMMARY_LENGTH: usize = 260;
const MAX_DETAIL_VALUE_LENGTH: usize = 320;
const MAX_TEXT_DISPLAY_LENGTH: usize = 1700;
const MAX_DETAIL_LINES: usize = 12;

pub const CATEGORIES: [(&str, &str, &str); 17] = [
    ("system", "System", "🧩"),
    ("command", "Command", "⚙️"),
    ("moderation", "Moderation", "🛡️"),
    ("member", "Member", "👤"),
    ("message", "Message", "💬"),
    ("reaction", "Reaction", "✨"),
    ("channel", "Channel", "🧱"),
    ("role", "Role", "🎭"),
    ("thread", "Thread", "🧵"),
    ("voice", "Voice", "🔊"),
    ("invite", "Invite", "🔗"),
    ("emoji", "Emoji", "😀"),
    ("sticker", "Sticker", "🏷️"),
    ("webhook", "Webhook", "🪝"),
    ("guild", "Guild", "🏠"),
    ("audit", "Audit", "📘"),
    ("automod", "AutoMod", "🚨"),
];

pub fn category_keys() -> impl Iterator<Item = &'static str> {
    CATEGORIES.iter().map(|(key, _, _)| *key)
}

pub fn category_meta(key: &str) -> Option<(&'static str, &'static str)> {
    CATEGORIES
        .iter()
        .find(|(k, _, _)| *k == key)
        .map(|(_, label, icon)| (*label, *icon))
}

pub const MIN_RETENTION_DAYS: i64 = 1;
pub const MAX_RETENTION_DAYS: i64 = 365;

pub fn normalize_retention(value: Option<f64>, fallback: i64) -> i64 {
    match value.filter(|v| v.is_finite()) {
        Some(raw) => (raw.round() as i64).clamp(MIN_RETENTION_DAYS, MAX_RETENTION_DAYS),
        None => fallback,
    }
}

pub fn normalize_disabled_categories(values: &[String]) -> Vec<String> {
    let mut out: Vec<String> = values
        .iter()
        .map(|v| v.trim().to_lowercase())
        .filter(|v| category_keys().any(|k| k == v))
        .collect();
    out.sort();
    out.dedup();
    out
}

#[derive(Debug, Clone)]
pub struct LogsState {
    pub enabled: bool,
    pub channel_id: Option<String>,
    pub retention_days: i64,
    pub forward_mode: String,
    pub include_bots: bool,
    pub disabled_categories: Vec<String>,
}

pub fn resolve_logs_state(app: &App, guild_id: &str) -> LogsState {
    let state = app.state.logs(guild_id);
    LogsState {
        enabled: config::is_true(&state, &["enabled"]),
        channel_id: state["channelId"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| state["channelId"].as_u64().map(|n| n.to_string())),
        retention_days: normalize_retention(
            state.get("retentionDays").map(config::js_to_number),
            30,
        ),
        forward_mode: state["forwardMode"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or("all")
            .to_owned(),
        include_bots: config::is_true(&state, &["includeBots"]),
        disabled_categories: normalize_disabled_categories(&config::string_list(
            &state,
            &["disabledCategories"],
        )),
    }
}

pub fn is_valid_day_key(value: &str) -> bool {
    let value = value.trim();
    value.len() == 10 && NaiveDate::parse_from_str(value, "%Y-%m-%d").is_ok()
}

// ---------------------------------------------------------------------------------------------
// Rendering helpers (pure, mirror the JS functions of the same names)
// ---------------------------------------------------------------------------------------------

fn as_at_user(tag: Option<&str>) -> String {
    let raw = inline(tag.unwrap_or_default(), 64);
    if raw.is_empty() {
        "@unknown".to_owned()
    } else if raw.starts_with('@') {
        raw
    } else {
        format!("@{raw}")
    }
}

fn format_role_ref(name: Option<&str>) -> String {
    match name {
        None => "@role".to_owned(),
        Some(name) => {
            let raw = inline(name, 64);
            if raw.starts_with('@') {
                raw
            } else {
                format!("@{raw}")
            }
        }
    }
}

fn simplify_channel_name(name: Option<&str>) -> String {
    let mut raw = inline(name.unwrap_or_default(), 64);
    if raw.is_empty() {
        return "channel".to_owned();
    }
    let parts: Vec<&str> = raw
        .split(':')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() >= 2 {
        raw = parts[parts.len() - 1].to_owned();
    }
    let trimmed: String = raw
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .to_owned();
    let collapsed = crate::util::text::compact_whitespace(&trimmed);
    if collapsed.is_empty() {
        "channel".to_owned()
    } else {
        collapsed
    }
}

fn format_channel_ref(name: Option<&str>) -> String {
    format!("#{}", simplify_channel_name(name).trim_start_matches('#'))
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn mention_user(id: Option<&str>, tag: Option<&str>) -> String {
    match id.map(str::trim) {
        Some(id) if is_snowflake(id, 15) => format!("<@{id}>"),
        _ => as_at_user(tag),
    }
}

fn mention_role(id: Option<&str>, name: Option<&str>) -> String {
    match id.map(str::trim) {
        Some(id) if is_snowflake(id, 15) => format!("<@&{id}>"),
        _ => format_role_ref(name),
    }
}

fn mention_channel(id: Option<&str>, name: Option<&str>) -> String {
    match id.map(str::trim) {
        Some(id) if is_snowflake(id, 15) => format!("<#{id}>"),
        _ => format_channel_ref(name),
    }
}

fn has_known_actor(actor: &Value) -> bool {
    if actor.is_null() {
        return false;
    }
    str_field(actor, "id").is_some_and(|id| is_snowflake(id, 15))
        || !inline(str_field(actor, "tag").unwrap_or_default(), 80).is_empty()
}

fn format_actor_mention(actor: &Value) -> String {
    if !has_known_actor(actor) {
        return "unknown".to_owned();
    }
    mention_user(str_field(actor, "id"), str_field(actor, "tag"))
}

fn strip_snowflakes(value: &str) -> String {
    static PAREN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static BARE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let paren = PAREN.get_or_init(|| regex::Regex::new(r"\(\d{15,22}\)").expect("static regex"));
    let bare = BARE.get_or_init(|| regex::Regex::new(r"\b\d{15,22}\b").expect("static regex"));
    let stripped = paren.replace_all(value, "");
    let stripped = bare.replace_all(&stripped, "");
    crate::util::text::compact_whitespace(&stripped)
}

fn sanitize_detail_line(input: &str) -> Option<String> {
    let line = inline(input, 500);
    if line.is_empty() {
        return None;
    }
    static KV: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let kv =
        KV.get_or_init(|| regex::Regex::new(r"(?i)^([a-z0-9_]+)=(.+)$").expect("static regex"));
    if let Some(captures) = kv.captures(&line) {
        let key = captures[1].to_lowercase();
        if key == "id"
            || key.ends_with("_id")
            || key == "content"
            || key == "before"
            || key == "after"
        {
            return None;
        }
        let human = strip_snowflakes(&captures[2]);
        if human.is_empty() {
            return None;
        }
        return Some(truncate(&format!("{key}={human}"), 130));
    }
    let stripped = strip_snowflakes(&line);
    (!stripped.is_empty()).then(|| truncate(&stripped, 130))
}

fn format_action_label(action: &str) -> String {
    let raw = action.trim().to_lowercase();
    if raw.is_empty() {
        return "event".to_owned();
    }
    if raw == "state_update" || raw == "update" || raw.ends_with("_update") {
        return "changed".to_owned();
    }
    match raw.as_str() {
        "entry_create" => "recorded".to_owned(),
        "slash_execute" => "executed".to_owned(),
        "ban_add" => "ban".to_owned(),
        "ban_remove" => "unban".to_owned(),
        _ => {
            let label = inline(&raw.replace('_', " "), 48);
            if label.is_empty() {
                "event".to_owned()
            } else {
                label
            }
        }
    }
}

fn detail_suffix(details: &Value) -> String {
    details
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find_map(sanitize_detail_line)
        .map(|first| format!(" ({})", truncate(&first, 78)))
        .unwrap_or_default()
}

fn serialize_attachment_line(attachment: &Value) -> Option<String> {
    if let Some(text) = attachment.as_str() {
        return Some(inline(text, 240));
    }
    let name = inline(
        str_field(attachment, "name")
            .or_else(|| str_field(attachment, "fileName"))
            .or_else(|| str_field(attachment, "id"))
            .unwrap_or_default(),
        100,
    );
    let url = str_field(attachment, "url")
        .or_else(|| str_field(attachment, "proxyURL"))
        .map(|u| inline(u, 600))
        .and_then(|u| url::Url::parse(&u).ok().map(|u| u.to_string()));
    match (name.is_empty(), url) {
        (false, Some(url)) => Some(format!("{name}: {}", truncate(&url, 240))),
        (false, None) => Some(name),
        (true, Some(url)) => Some(truncate(&url, 240)),
        (true, None) => None,
    }
}

fn build_message_delete_detail(entry: &Value) -> String {
    let target = &entry["target"];
    let metadata = &entry["metadata"];
    let owner = mention_user(
        str_field(target, "ownerId").or_else(|| str_field(metadata, "ownerId")),
        str_field(target, "ownerTag"),
    );
    let content = inline(str_field(metadata, "content").unwrap_or_default(), 320);
    let content = if content.is_empty() {
        "[empty]".to_owned()
    } else {
        content
    };
    let reason = inline(str_field(metadata, "reason").unwrap_or_default(), 180).replace('"', "'");
    let files: Vec<String> = metadata["attachments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serialize_attachment_line)
        .collect();
    let mut parts = vec![
        format!("author={owner}"),
        format!("content=\"{}\"", content.replace('"', "'")),
    ];
    if !files.is_empty() {
        parts.push(format!("files={}", truncate(&files.join(", "), 520)));
    }
    if !reason.is_empty() {
        parts.push(format!("reason=\"{reason}\""));
    }
    truncate(&parts.join("; "), 980)
}

pub fn build_compact_log_line(entry: &Value, icon: &str) -> String {
    let actor_value = &entry["actor"];
    let actor = format_actor_mention(actor_value);
    let known = has_known_actor(actor_value);
    let target = &entry["target"];
    let location = &entry["location"];
    let metadata = &entry["metadata"];
    let category = str_field(entry, "category")
        .unwrap_or_default()
        .to_lowercase();
    let action = str_field(entry, "action")
        .unwrap_or_default()
        .to_lowercase();
    let suffix = detail_suffix(&entry["details"]);
    let location_channel = || {
        mention_channel(
            str_field(location, "channelId"),
            str_field(location, "channelName"),
        )
    };

    match category.as_str() {
        "voice" => {
            let from = mention_channel(str_field(metadata, "beforeChannel"), None);
            let to = mention_channel(
                str_field(metadata, "afterChannel").or_else(|| str_field(location, "channelId")),
                str_field(location, "channelName"),
            );
            match action.as_str() {
                "join" => format!("{icon} {actor} joined to {to}"),
                "leave" => format!("{icon} {actor} left from {from}"),
                "move" => {
                    let member = mention_user(
                        str_field(target, "id").or_else(|| str_field(metadata, "userId")),
                        str_field(target, "tag"),
                    );
                    if known {
                        format!("{icon} {actor} moved {member} from {from} to {to}")
                    } else {
                        format!("{icon} {member} moved from {from} to {to} (actor unknown)")
                    }
                }
                _ => {
                    let at = location_channel();
                    if known {
                        format!("{icon} {actor} updated voice in {at}{suffix}")
                    } else {
                        format!("{icon} voice updated in {at} (actor unknown){suffix}")
                    }
                }
            }
        }
        "role" => {
            let role = mention_role(str_field(target, "id"), str_field(target, "name"));
            match (action.as_str(), known) {
                ("create", true) => format!("{icon} {actor} created {role}"),
                ("create", false) => format!("{icon} created {role} (actor unknown)"),
                ("delete", true) => format!("{icon} {actor} deleted {role}"),
                ("delete", false) => format!("{icon} deleted {role} (actor unknown)"),
                (_, true) => format!("{icon} {actor} updated {role}{suffix}"),
                (_, false) => format!("{icon} updated {role} (actor unknown){suffix}"),
            }
        }
        "command" => {
            let name = str_field(target, "name").map_or("/unknown".to_owned(), |n| format!("/{n}"));
            format!("{icon} {actor} executed {name} in {}", location_channel())
        }
        "message" => {
            let at = location_channel();
            match action.as_str() {
                "delete" => {
                    let detail = build_message_delete_detail(entry);
                    if known {
                        format!("{icon} {actor} deleted message in {at} ({detail})")
                    } else {
                        format!("{icon} deleted message in {at} (actor=unknown; {detail})")
                    }
                }
                "bulk_delete" if known => format!("{icon} {actor} bulk-deleted messages in {at}"),
                "bulk_delete" => format!("{icon} bulk-deleted messages in {at} (actor unknown)"),
                "edit" | "update" => format!("{icon} {actor} edited message in {at}"),
                _ => format!("{icon} {actor} updated message in {at}"),
            }
        }
        "member" => {
            let member = mention_user(str_field(target, "id"), str_field(target, "tag"));
            match (action.as_str(), known) {
                ("join", _) => format!("{icon} {member} joined server"),
                ("leave", _) => format!("{icon} {member} left server"),
                ("remove", _) => {
                    format!("{icon} {member} left or was removed from server (actor unknown)")
                }
                ("kick", true) => format!("{icon} {member} was kicked by {actor}"),
                ("kick", false) => format!("{icon} {member} was kicked (actor unknown)"),
                ("ban", true) => format!("{icon} {member} was banned by {actor}"),
                ("ban", false) => format!("{icon} {member} was banned (actor unknown)"),
                (_, true) => format!("{icon} {actor} updated {member}{suffix}"),
                (_, false) => format!("{icon} updated {member} (actor unknown){suffix}"),
            }
        }
        "moderation" if action == "ban_add" || action == "ban_remove" => {
            let member = mention_user(str_field(target, "id"), str_field(target, "tag"));
            let verb = if action == "ban_add" {
                "banned"
            } else {
                "unbanned"
            };
            if known {
                format!("{icon} {actor} {verb} {member}")
            } else {
                format!("{icon} {verb} {member} (actor unknown)")
            }
        }
        "channel" => {
            let channel = mention_channel(
                str_field(target, "id").or_else(|| str_field(location, "channelId")),
                str_field(target, "name").or_else(|| str_field(location, "channelName")),
            );
            match (action.as_str(), known) {
                ("create", true) => format!("{icon} {actor} created {channel}"),
                ("create", false) => format!("{icon} created {channel} (actor unknown)"),
                ("delete", true) => format!("{icon} {actor} deleted {channel}"),
                ("delete", false) => format!("{icon} deleted {channel} (actor unknown)"),
                (_, true) => format!("{icon} {actor} updated {channel}{suffix}"),
                (_, false) => format!("{icon} updated {channel} (actor unknown){suffix}"),
            }
        }
        _ => {
            let label = format_action_label(str_field(entry, "action").unwrap_or("event"));
            if known {
                format!("{icon} {actor} {label}{suffix}")
            } else {
                format!("{icon} {label} (actor unknown){suffix}")
            }
        }
    }
}

fn v2_line_payload(entry: &Value, extra_flags: u64, header: Option<(&str, &str)>) -> Value {
    let icon = str_field(entry, "category")
        .and_then(category_meta)
        .map_or("🧩", |(_, icon)| icon);
    let line = truncate(
        &build_compact_log_line(entry, icon),
        MAX_TEXT_DISPLAY_LENGTH,
    );
    let mut components = Vec::new();
    if let Some((title, note)) = header {
        components.push(discord::text(title));
        components.push(discord::text(note));
    }
    components.push(discord::text(line));
    json!({
        "flags": FLAG_COMPONENTS_V2 | extra_flags,
        "allowed_mentions": { "parse": [] },
        "components": [discord::container(components)]
    })
}

fn format_state_delta(before: &str, after: &str) -> Option<String> {
    let before = inline(before, 80);
    let after = inline(after, 80);
    let before = if before.is_empty() {
        "none".to_owned()
    } else {
        before
    };
    let after = if after.is_empty() {
        "none".to_owned()
    } else {
        after
    };
    (before != after).then(|| format!("{before} -> {after}"))
}

fn delta_bool(before: bool, after: bool) -> Option<String> {
    format_state_delta(&before.to_string(), &after.to_string())
}

fn actor_payload(user: Option<&User>) -> Value {
    match user {
        Some(user) => {
            json!({ "id": user.id.to_string(), "tag": discord::user_tag(user), "bot": user.bot })
        }
        None => Value::Null,
    }
}

fn reason_text(reason: &str) -> String {
    let text = inline(reason, 180);
    if text.is_empty() {
        "none".to_owned()
    } else {
        text
    }
}

fn humanize_permission(name: &str) -> String {
    name.split('_')
        .filter(|p| !p.is_empty())
        .map(|part| {
            let lower = part.to_lowercase();
            let mut chars = lower.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn permission_names(perms: Permissions) -> Vec<String> {
    perms
        .iter_names()
        .map(|(name, _)| humanize_permission(name))
        .collect()
}

fn format_list_with_limit(values: &[String], max: usize) -> String {
    let list: Vec<String> = values
        .iter()
        .map(|v| inline(v, 48))
        .filter(|v| !v.is_empty())
        .collect();
    if list.len() <= max {
        return list.join(", ");
    }
    format!("{}, +{} more", list[..max].join(", "), list.len() - max)
}

fn role_hex(role: &Role) -> String {
    format!("#{:06x}", role.colors.primary_color)
}

fn channel_kind_number(channel: &Channel) -> u8 {
    u8::from(channel.kind)
}

// ---------------------------------------------------------------------------------------------
// "Before" snapshots taken from the cache prior to applying an event
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CachedMessageLite {
    pub content: String,
    pub author_id: Id<UserMarker>,
    pub attachments: Vec<Value>,
}

/// (name, icon, banner, verification level)
pub type GuildBefore = (String, Option<String>, Option<String>, u8);
/// (channel, self_mute, self_deaf, server mute, server deaf)
pub type VoiceBefore = (Option<Id<ChannelMarker>>, bool, bool, bool, bool);
type DeliverySender = mpsc::UnboundedSender<(Id<ChannelMarker>, Value)>;

#[derive(Debug, Clone)]
pub enum Before {
    None,
    Message(Option<CachedMessageLite>),
    Channel(Option<Box<Channel>>),
    Role(Option<Role>),
    Member(Option<(Vec<String>, Option<String>)>),
    Guild(Option<GuildBefore>),
    Emojis(Vec<(String, String, bool)>),
    Stickers(Vec<(String, String, String, u8)>),
    Voice(Option<VoiceBefore>),
}

pub fn capture_before(cache: &DefaultInMemoryCache, event: &Event) -> Before {
    match event {
        Event::MessageUpdate(update) => Before::Message(cached_message(cache, update.id)),
        Event::MessageDelete(delete) => Before::Message(cached_message(cache, delete.id)),
        Event::ChannelUpdate(update) => {
            Before::Channel(cache.channel(update.id).map(|c| Box::new(c.clone())))
        }
        Event::ThreadUpdate(update) => {
            Before::Channel(cache.channel(update.id).map(|c| Box::new(c.clone())))
        }
        Event::ChannelDelete(delete) => {
            Before::Channel(cache.channel(delete.id).map(|c| Box::new(c.clone())))
        }
        Event::RoleUpdate(update) => {
            Before::Role(cache.role(update.role.id).map(|r| r.resource().clone()))
        }
        Event::RoleDelete(delete) => {
            Before::Role(cache.role(delete.role_id).map(|r| r.resource().clone()))
        }
        Event::MemberUpdate(update) => {
            Before::Member(cache.member(update.guild_id, update.user.id).map(|m| {
                (
                    m.roles().iter().map(ToString::to_string).collect(),
                    m.nick().map(str::to_owned),
                )
            }))
        }
        Event::GuildUpdate(update) => Before::Guild(cache.guild(update.id).map(|g| {
            (
                g.name().to_owned(),
                g.icon().map(ToString::to_string),
                g.afk_channel_id().map(|c| c.to_string()),
                u8::from(g.verification_level()),
            )
        })),
        Event::GuildEmojisUpdate(update) => Before::Emojis(
            cache
                .guild_emojis(update.guild_id)
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| cache.emoji(*id))
                        .map(|e| (e.id().to_string(), e.name().to_owned(), e.animated()))
                        .collect()
                })
                .unwrap_or_default(),
        ),
        Event::GuildStickersUpdate(update) => Before::Stickers(
            cache
                .guild_stickers(update.guild_id)
                .map(|ids| {
                    ids.iter()
                        .filter_map(|id| cache.sticker(*id))
                        .map(|s| {
                            (
                                s.id().to_string(),
                                s.name().to_owned(),
                                s.description().to_owned(),
                                u8::from(s.format_type()),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default(),
        ),
        Event::VoiceStateUpdate(update) => Before::Voice(update.guild_id.and_then(|guild_id| {
            cache.voice_state(update.user_id, guild_id).map(|v| {
                (
                    Some(v.channel_id()),
                    v.mute(),
                    v.deaf(),
                    v.self_mute(),
                    v.self_deaf(),
                )
            })
        })),
        _ => Before::None,
    }
}

fn cached_message(
    cache: &DefaultInMemoryCache,
    id: Id<twilight_model::id::marker::MessageMarker>,
) -> Option<CachedMessageLite> {
    cache.message(id).map(|m| CachedMessageLite {
        content: m.content().to_owned(),
        author_id: m.author(),
        attachments: m
            .attachments()
            .iter()
            .map(|a| {
                json!({
                    "id": a.id.to_string(),
                    "name": inline(&a.filename, 120),
                    "url": inline(&a.url, 260),
                    "size": a.size,
                    "contentType": a.content_type.as_deref().map(|c| inline(c, 100)),
                })
            })
            .collect(),
    })
}

// ---------------------------------------------------------------------------------------------
// FullLogs runtime
// ---------------------------------------------------------------------------------------------

#[derive(Clone)]
struct AuditCached {
    at: Instant,
    entries: Vec<AuditLogEntry>,
    users: HashMap<Id<UserMarker>, User>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditStatus {
    Matched,
    Miss,
    NoAccess,
    Error,
}

pub struct AuditMatch {
    pub entry: Option<AuditLogEntry>,
    pub executor: Option<User>,
    pub status: AuditStatus,
}

fn actor_resolution(status: AuditStatus) -> &'static str {
    match status {
        AuditStatus::Matched => "audit",
        AuditStatus::NoAccess => "audit_unavailable",
        AuditStatus::Error => "audit_error",
        _ => "unknown",
    }
}

fn actor_resolution_detail(status: AuditStatus) -> Option<String> {
    match status {
        AuditStatus::Matched => None,
        AuditStatus::NoAccess => Some("actor=unknown (audit unavailable)".to_owned()),
        AuditStatus::Error => Some("actor=unknown (audit lookup failed)".to_owned()),
        _ => Some("actor=unknown".to_owned()),
    }
}

pub struct Diagnostics {
    pub started: bool,
    pub logs_dir: PathBuf,
    pub logs_dir_exists: bool,
    pub can_view_audit_log: Option<bool>,
    pub delivery_queue_size: usize,
    pub write_queue_size: usize,
    pub buffered_writes: usize,
    pub audit_cache_entries: usize,
    pub audit_fetch_inflight: usize,
    pub retention_timer_active: bool,
    pub retention_sweep_active: bool,
}

pub struct FullLogs {
    logs_dir: PathBuf,
    started: AtomicBool,
    buffers: Mutex<HashMap<String, Vec<(PathBuf, String)>>>,
    write_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    writes_inflight: AtomicUsize,
    delivery: Mutex<HashMap<String, DeliverySender>>,
    delivery_pending: Arc<AtomicUsize>,
    audit_cache: Mutex<HashMap<String, AuditCached>>,
    audit_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    audit_inflight: AtomicUsize,
    retention_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    retention_active: AtomicBool,
}

impl FullLogs {
    pub fn new(logs_dir: &Path) -> Self {
        Self {
            logs_dir: logs_dir.to_path_buf(),
            started: AtomicBool::new(false),
            buffers: Mutex::new(HashMap::new()),
            write_locks: Mutex::new(HashMap::new()),
            writes_inflight: AtomicUsize::new(0),
            delivery: Mutex::new(HashMap::new()),
            delivery_pending: Arc::new(AtomicUsize::new(0)),
            audit_cache: Mutex::new(HashMap::new()),
            audit_locks: Mutex::new(HashMap::new()),
            audit_inflight: AtomicUsize::new(0),
            retention_task: Mutex::new(None),
            retention_active: AtomicBool::new(false),
        }
    }

    pub fn start(&self, app: &SharedApp) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Err(error) = std::fs::create_dir_all(&self.logs_dir) {
            tracing::warn!(
                "FullLogs mkdir error ({}): {error}",
                self.logs_dir.display()
            );
        }
        let task_app = app.clone();
        let handle = tokio::spawn(async move {
            task_app
                .fulllogs
                .run_retention_sweep(&task_app, "startup")
                .await;
            let mut interval = tokio::time::interval(RETENTION_SWEEP_INTERVAL);
            interval.tick().await;
            loop {
                interval.tick().await;
                task_app
                    .fulllogs
                    .run_retention_sweep(&task_app, "interval")
                    .await;
            }
        });
        *self
            .retention_task
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(handle);
        tracing::info!("Full logs module started.");
    }

    pub async fn stop(&self) {
        if !self.started.swap(false, Ordering::SeqCst) {
            return;
        }
        if let Some(task) = self
            .retention_task
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            task.abort();
        }
        self.flush_all().await;
        self.audit_cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        tracing::info!("Full logs module stopped.");
    }

    pub fn is_started(&self) -> bool {
        self.started.load(Ordering::SeqCst)
    }

    pub fn can_view_audit_log(app: &App, guild_id: Id<GuildMarker>) -> bool {
        discord::bot_root_permissions(app, guild_id)
            .is_some_and(|p| discord::has(p, Permissions::VIEW_AUDIT_LOG))
    }

    pub fn diagnostics(&self, app: &App, guild_id: Option<Id<GuildMarker>>) -> Diagnostics {
        let buffers = self.buffers.lock().unwrap_or_else(|p| p.into_inner());
        let buffered = match guild_id {
            Some(id) => buffers.get(&id.to_string()).map_or(0, Vec::len),
            None => buffers.values().map(Vec::len).sum(),
        };
        Diagnostics {
            started: self.is_started(),
            logs_dir: self.logs_dir.clone(),
            logs_dir_exists: self.logs_dir.exists(),
            can_view_audit_log: guild_id.map(|g| Self::can_view_audit_log(app, g)),
            delivery_queue_size: self.delivery_pending.load(Ordering::SeqCst),
            write_queue_size: self.writes_inflight.load(Ordering::SeqCst),
            buffered_writes: buffered,
            audit_cache_entries: self
                .audit_cache
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .len(),
            audit_fetch_inflight: self.audit_inflight.load(Ordering::SeqCst),
            retention_timer_active: self
                .retention_task
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_some(),
            retention_sweep_active: self.retention_active.load(Ordering::SeqCst),
        }
    }

    // --- entry pipeline ----------------------------------------------------------------------

    /// `logGuildEvent`. Returns `true` when the entry was written/forwarded.
    pub fn log(
        &self,
        app: &SharedApp,
        guild_id: Id<GuildMarker>,
        input: Value,
        force: bool,
    ) -> bool {
        if !self.is_started() && !force {
            return false;
        }
        let guild = guild_id.to_string();
        let state = resolve_logs_state(app, &guild);
        let Some(channel_id) = state
            .channel_id
            .as_deref()
            .and_then(discord::parse_id::<ChannelMarker>)
            .filter(|_| state.enabled)
        else {
            return false;
        };

        let details: Vec<Value> = input["details"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|s| !s.is_empty())
            .take(MAX_DETAIL_LINES)
            .map(|line| Value::String(inline(line, MAX_DETAIL_VALUE_LENGTH)))
            .collect();
        let summary = inline(
            str_field(&input, "summary").unwrap_or_default(),
            MAX_SUMMARY_LENGTH,
        );
        let entry = json!({
            "ts": now_iso(),
            "guildId": guild,
            "category": str_field(&input, "category").unwrap_or("system"),
            "action": str_field(&input, "action").unwrap_or("event"),
            "actor": input.get("actor").cloned().unwrap_or(Value::Null),
            "target": input.get("target").cloned().unwrap_or(Value::Null),
            "location": input.get("location").cloned().unwrap_or(Value::Null),
            "summary": if summary.is_empty() { "Event captured.".to_owned() } else { summary },
            "details": details,
            "metadata": input.get("metadata").cloned().filter(|m| !m.is_null()).unwrap_or_else(|| json!({})),
        });

        let category = entry["category"]
            .as_str()
            .unwrap_or("system")
            .to_lowercase();
        if !force
            && category_keys().any(|k| k == category)
            && state.disabled_categories.contains(&category)
        {
            return false;
        }

        let actor = &entry["actor"];
        if !actor.is_null() {
            if str_field(actor, "id") == Some(app.bot_id.to_string().as_str()) {
                return false;
            }
            if !state.include_bots && actor["bot"].as_bool() == Some(true) {
                return false;
            }
        }

        self.write_json_line(app, &guild, &entry);
        self.enqueue_delivery(app, &guild, channel_id, entry);
        true
    }

    fn day_path(&self, guild_id: &str, day: &str) -> PathBuf {
        self.logs_dir.join(guild_id).join(format!("{day}.jsonl"))
    }

    fn write_json_line(&self, app: &SharedApp, guild_id: &str, entry: &Value) {
        let day = entry["ts"]
            .as_str()
            .map_or_else(|| now_iso()[..10].to_owned(), |ts| ts[..10].to_owned());
        let path = self.day_path(guild_id, &day);
        let line = format!("{entry}\n");
        let first = {
            let mut buffers = self.buffers.lock().unwrap_or_else(|p| p.into_inner());
            let buffer = buffers.entry(guild_id.to_owned()).or_default();
            buffer.push((path, line));
            buffer.len() == 1
        };
        if first {
            let app = app.clone();
            let guild = guild_id.to_owned();
            tokio::spawn(async move {
                tokio::time::sleep(WRITE_BATCH_DELAY).await;
                app.fulllogs.flush_guild(&guild).await;
            });
        }
    }

    fn write_lock(&self, guild_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.write_locks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(guild_id.to_owned())
            .or_default()
            .clone()
    }

    async fn flush_guild(&self, guild_id: &str) {
        let lock = self.write_lock(guild_id);
        let _guard = lock.lock().await;
        let buffered = self
            .buffers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(guild_id)
            .unwrap_or_default();
        if buffered.is_empty() {
            return;
        }
        self.writes_inflight.fetch_add(1, Ordering::SeqCst);
        let mut grouped: Vec<(PathBuf, String)> = Vec::new();
        for (path, line) in buffered {
            match grouped.last_mut() {
                Some((last, payload)) if *last == path => payload.push_str(&line),
                _ => grouped.push((path, line)),
            }
        }
        for (path, payload) in grouped {
            if let Err(error) = append(&path, &payload).await {
                tracing::warn!("FullLogs disk write error ({guild_id}): {error}");
            }
        }
        self.writes_inflight.fetch_sub(1, Ordering::SeqCst);
    }

    async fn flush_all(&self) {
        let guilds: Vec<String> = self
            .buffers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .cloned()
            .collect();
        for guild in guilds {
            self.flush_guild(&guild).await;
        }
    }

    fn enqueue_delivery(
        &self,
        app: &SharedApp,
        guild_id: &str,
        channel_id: Id<ChannelMarker>,
        entry: Value,
    ) {
        let mut delivery = self.delivery.lock().unwrap_or_else(|p| p.into_inner());
        let sender = delivery.entry(guild_id.to_owned()).or_insert_with(|| {
            let (tx, mut rx) = mpsc::unbounded_channel::<(Id<ChannelMarker>, Value)>();
            let app = app.clone();
            let pending = Arc::clone(&self.delivery_pending);
            let guild = guild_id.to_owned();
            tokio::spawn(async move {
                while let Some((channel, entry)) = rx.recv().await {
                    let payload = v2_line_payload(&entry, 0, None);
                    if let Err(error) = discord::send_message(&app, channel, &payload).await {
                        tracing::warn!("FullLogs delivery error ({guild}/{channel}): {error}");
                    }
                    pending.fetch_sub(1, Ordering::SeqCst);
                }
            });
            tx
        });
        self.delivery_pending.fetch_add(1, Ordering::SeqCst);
        if sender.send((channel_id, entry)).is_err() {
            self.delivery_pending.fetch_sub(1, Ordering::SeqCst);
        }
    }

    // --- commands support --------------------------------------------------------------------

    pub fn send_test_log(
        &self,
        app: &SharedApp,
        guild_id: Id<GuildMarker>,
        user: Option<&User>,
    ) -> bool {
        self.log(
            app,
            guild_id,
            json!({
                "category": "system",
                "action": "test",
                "actor": actor_payload(user),
                "target": { "kind": "guild", "id": guild_id.to_string() },
                "summary": "Manual test entry from /logs mode:test.",
                "details": [
                    format!("runtime=rust/{}", env!("CARGO_PKG_VERSION")),
                    format!("components_v2={FLAG_COMPONENTS_V2}"),
                ],
                "metadata": { "source": "command:test" }
            }),
            true,
        )
    }

    async fn latest_saved_entry(&self, guild_id: &str) -> Option<(Value, String)> {
        let dir = self.logs_dir.join(guild_id);
        let mut files: Vec<String> = std::fs::read_dir(&dir)
            .ok()?
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".jsonl") && is_valid_day_key(&n[..n.len() - 6]))
            .collect();
        files.sort();
        files.reverse();
        for name in files {
            let Ok(raw) = tokio::fs::read_to_string(dir.join(&name)).await else {
                continue;
            };
            for line in raw.lines().rev().map(str::trim).filter(|l| !l.is_empty()) {
                if let Ok(parsed) = serde_json::from_str::<Value>(line)
                    && parsed.is_object()
                {
                    return Some((parsed, name.trim_end_matches(".jsonl").to_owned()));
                }
            }
        }
        None
    }

    pub async fn preview_payload(&self, guild_id: Id<GuildMarker>) -> Value {
        let guild = guild_id.to_string();
        self.flush_guild(&guild).await;
        let latest = self.latest_saved_entry(&guild).await;
        let (entry, note) = match latest {
            Some((entry, day)) => (
                entry,
                format!("Podglad bazuje na ostatnim zapisanym wpisie z dnia `{day}`."),
            ),
            None => (
                json!({
                    "ts": now_iso(),
                    "guildId": guild,
                    "category": "system",
                    "action": "preview",
                    "actor": null,
                    "target": { "kind": "guild", "id": guild },
                    "location": null,
                    "summary": "Preview sample entry.",
                    "details": ["source=preview", "note=Brak zapisanych wpisow, pokazuje przykladowy format."],
                    "metadata": { "source": "preview:fallback" }
                }),
                "Brak zapisanych wpisow. Pokazuje przykladowy format loga.".to_owned(),
            ),
        };
        v2_line_payload(&entry, FLAG_EPHEMERAL, Some(("## Logs Preview", &note)))
    }

    /// Returns `(path, attachment name)` when the day file exists.
    pub async fn export_day(
        &self,
        guild_id: Id<GuildMarker>,
        day: &str,
    ) -> anyhow::Result<Option<(PathBuf, String)>> {
        if !is_valid_day_key(day) {
            anyhow::bail!("Niepoprawny format dnia. Uzyj YYYY-MM-DD.");
        }
        let guild = guild_id.to_string();
        self.flush_guild(&guild).await;
        let path = self.day_path(&guild, day);
        Ok(path
            .exists()
            .then(|| (path, format!("{guild}-{day}.jsonl"))))
    }

    /// Returns `(removed days, olderThanDays)`.
    pub async fn purge(
        &self,
        guild_id: Id<GuildMarker>,
        day: Option<&str>,
        older_than_days: Option<i64>,
    ) -> anyhow::Result<Vec<String>> {
        let guild = guild_id.to_string();
        self.flush_guild(&guild).await;
        if let Some(day) = day {
            if !is_valid_day_key(day) {
                anyhow::bail!("Niepoprawny format dnia. Uzyj YYYY-MM-DD.");
            }
            let path = self.day_path(&guild, day);
            if !path.exists() {
                return Ok(Vec::new());
            }
            tokio::fs::remove_file(&path).await?;
            return Ok(vec![day.to_owned()]);
        }
        let older = older_than_days.unwrap_or(1).max(1);
        let today = Utc::now().date_naive();
        let cutoff = today - chrono::Duration::days(older);
        let mut removed = Vec::new();
        let Ok(entries) = std::fs::read_dir(self.logs_dir.join(&guild)) else {
            return Ok(removed);
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(day) = name.strip_suffix(".jsonl").filter(|d| is_valid_day_key(d)) else {
                continue;
            };
            let Ok(date) = NaiveDate::parse_from_str(day, "%Y-%m-%d") else {
                continue;
            };
            if date >= cutoff {
                continue;
            }
            match tokio::fs::remove_file(entry.path()).await {
                Ok(()) => removed.push(day.to_owned()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        removed.sort();
        Ok(removed)
    }

    async fn run_retention_sweep(&self, app: &App, trigger: &str) {
        if self.retention_active.swap(true, Ordering::SeqCst) {
            return;
        }
        let mut removed = 0;
        if let Ok(entries) = std::fs::read_dir(&self.logs_dir) {
            for entry in entries.flatten().filter(|e| e.path().is_dir()) {
                let guild = entry.file_name().to_string_lossy().into_owned();
                let retention = resolve_logs_state(app, &guild).retention_days;
                let cutoff = Utc::now().timestamp_millis() - retention * 86_400_000;
                let Ok(files) = std::fs::read_dir(entry.path()) else {
                    continue;
                };
                for file in files.flatten() {
                    let name = file.file_name().to_string_lossy().into_owned();
                    let Some(day) = name.strip_suffix(".jsonl").filter(|d| is_valid_day_key(d))
                    else {
                        continue;
                    };
                    let Some(ts) = parse_iso(day) else { continue };
                    if ts.timestamp_millis() >= cutoff {
                        continue;
                    }
                    if std::fs::remove_file(file.path()).is_ok() {
                        removed += 1;
                    }
                }
            }
        }
        if removed > 0 {
            tracing::info!("FullLogs retention sweep ({trigger}) removed files: {removed}");
        }
        self.retention_active.store(false, Ordering::SeqCst);
    }

    // --- audit log ----------------------------------------------------------------------------

    async fn fetch_audit(
        &self,
        app: &App,
        guild_id: Id<GuildMarker>,
        kind: AuditLogEventType,
    ) -> Result<AuditCached, AuditStatus> {
        if !Self::can_view_audit_log(app, guild_id) {
            return Err(AuditStatus::NoAccess);
        }
        let key = format!("{guild_id}:{}", u16::from(kind));
        let lock = self
            .audit_locks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(key.clone())
            .or_default()
            .clone();
        let _guard = lock.lock().await;
        if let Some(cached) = self
            .audit_cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&key)
            && cached.at.elapsed() < AUDIT_CACHE_TTL
        {
            return Ok(cached.clone());
        }

        self.audit_inflight.fetch_add(1, Ordering::SeqCst);
        let result = async {
            let log = app
                .http
                .audit_log(guild_id)
                .action_type(kind)
                .limit(AUDIT_FETCH_LIMIT)
                .await?
                .model()
                .await?;
            anyhow::Ok(log)
        }
        .await;
        self.audit_inflight.fetch_sub(1, Ordering::SeqCst);

        match result {
            Ok(log) => {
                let mut entries = log.entries;
                entries.sort_by_key(|e| std::cmp::Reverse(e.id.get()));
                let cached = AuditCached {
                    at: Instant::now(),
                    entries,
                    users: log.users.into_iter().map(|u| (u.id, u)).collect(),
                };
                self.audit_cache
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(key, cached.clone());
                Ok(cached)
            }
            Err(error) => {
                tracing::warn!(
                    "FullLogs audit fetch failed ({guild_id}/{}): {error}",
                    u16::from(kind)
                );
                Err(AuditStatus::Error)
            }
        }
    }

    pub async fn resolve_audit(
        &self,
        app: &App,
        guild_id: Id<GuildMarker>,
        kind: AuditLogEventType,
        target_id: Option<u64>,
        predicate: impl Fn(&AuditLogEntry) -> bool,
    ) -> AuditMatch {
        let fetched = match self.fetch_audit(app, guild_id, kind).await {
            Ok(fetched) => fetched,
            Err(status) => {
                return AuditMatch {
                    entry: None,
                    executor: None,
                    status,
                };
            }
        };
        let now = Utc::now().timestamp_millis();
        let entry = fetched.entries.iter().find(|entry| {
            let created = snowflake_ms(entry.id.get());
            if now - created > AUDIT_MATCH_WINDOW_MS {
                return false;
            }
            if let Some(target) = target_id
                && entry.target_id.map(|t| t.get()) != Some(target)
            {
                return false;
            }
            predicate(entry)
        });
        match entry {
            Some(entry) => AuditMatch {
                executor: entry.user_id.and_then(|u| fetched.users.get(&u).cloned()),
                entry: Some(entry.clone()),
                status: AuditStatus::Matched,
            },
            None => AuditMatch {
                entry: None,
                executor: None,
                status: AuditStatus::Miss,
            },
        }
    }

    // --- event handling -------------------------------------------------------------------------

    pub async fn handle_event(&self, app: &SharedApp, event: &Event, before: Before) {
        if !self.is_started() {
            return;
        }
        if let Err(error) = self.handle_event_inner(app, event, before).await {
            tracing::warn!("FullLogs handler error ({:?}): {error}", event.kind());
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn handle_event_inner(
        &self,
        app: &SharedApp,
        event: &Event,
        before: Before,
    ) -> anyhow::Result<()> {
        let channel_name = |id: Option<Id<ChannelMarker>>| -> Value {
            id.and_then(|id| app.cache.channel(id).and_then(|c| c.name.clone()))
                .map_or(Value::Null, Value::String)
        };
        let user_from_cache = |id: Id<UserMarker>| app.cache.user(id).map(|u| u.clone());

        match event {
            Event::InteractionCreate(interaction) => {
                let Some(guild_id) = interaction.guild_id else {
                    return Ok(());
                };
                let Some(
                    twilight_model::application::interaction::InteractionData::ApplicationCommand(
                        data,
                    ),
                ) = &interaction.data
                else {
                    return Ok(());
                };
                let user = interaction
                    .member
                    .as_ref()
                    .and_then(|m| m.user.as_ref())
                    .or(interaction.user.as_ref());
                let Some(user) = user else { return Ok(()) };
                let channel_id = interaction.channel.as_ref().map(|c| c.id);
                let name = interaction.channel.as_ref().and_then(|c| c.name.clone());
                self.log(app, guild_id, json!({
                    "category": "command",
                    "action": "slash_execute",
                    "actor": actor_payload(Some(user)),
                    "target": { "kind": "command", "name": data.name },
                    "location": { "channelId": channel_id.map(|c| c.to_string()), "channelName": name },
                    "summary": format!("/{} executed", data.name),
                    "details": [
                        format!("command=/{}", data.name),
                        format!("channel=#{}", name.as_deref().unwrap_or("unknown")),
                        format!("interaction_id={}", interaction.id),
                    ],
                    "metadata": {
                        "commandName": data.name,
                        "channelId": channel_id.map(|c| c.to_string()),
                        "interactionId": interaction.id.to_string()
                    }
                }), false);
            }

            Event::MessageUpdate(update) => {
                let Some(guild_id) = update.guild_id else {
                    return Ok(());
                };
                let Before::Message(Some(old)) = before else {
                    return Ok(());
                };
                if update.author.id == app.bot_id {
                    return Ok(());
                }
                let before_text = block(&old.content, MAX_DETAIL_VALUE_LENGTH);
                let after_text = block(&update.content, MAX_DETAIL_VALUE_LENGTH);
                let changed = before_text != after_text;
                let empty = |s: &str| {
                    if s.is_empty() {
                        "[empty]".to_owned()
                    } else {
                        s.to_owned()
                    }
                };
                let mut details = vec![format!("message_id={}", update.id)];
                if changed {
                    details.push(format!("before={}", empty(&before_text)));
                    details.push(format!("after={}", empty(&after_text)));
                }
                self.log(app, guild_id, json!({
                    "category": "message",
                    "action": "edit",
                    "actor": actor_payload(Some(&update.author)),
                    "target": { "kind": "message", "id": update.id.to_string() },
                    "location": { "channelId": update.channel_id.to_string(), "channelName": channel_name(Some(update.channel_id)) },
                    "summary": if changed { "Message content edited." } else { "Message updated (non-content fields)." },
                    "details": details,
                    "metadata": {
                        "messageId": update.id.to_string(),
                        "channelId": update.channel_id.to_string(),
                        "before": before_text,
                        "after": after_text,
                        "contentChanged": changed
                    }
                }), false);
            }

            Event::MessageDelete(delete) => {
                let Some(guild_id) = delete.guild_id else {
                    return Ok(());
                };
                let Before::Message(Some(old)) = before else {
                    return Ok(());
                };
                if old.author_id == app.bot_id {
                    return Ok(());
                }
                let author = user_from_cache(old.author_id);
                let channel_id = delete.channel_id;
                let audit = self
                    .resolve_audit(
                        app,
                        guild_id,
                        AuditLogEventType::MessageDelete,
                        Some(old.author_id.get()),
                        |entry| {
                            let options = entry.options.as_ref();
                            if let Some(ch) = options.and_then(|o| o.channel_id)
                                && ch != channel_id
                            {
                                return false;
                            }
                            if let Some(count) = options
                                .and_then(|o| o.count.as_deref())
                                .and_then(|c| c.parse::<f64>().ok())
                                && count != 1.0
                            {
                                return false;
                            }
                            true
                        },
                    )
                    .await;
                let deleter = audit.executor.clone();
                let content = block(&old.content, MAX_DETAIL_VALUE_LENGTH);
                let author_tag = author.as_ref().map(discord::user_tag);
                let attachments_list = old
                    .attachments
                    .iter()
                    .map(|a| {
                        str_field(a, "name")
                            .or_else(|| str_field(a, "url"))
                            .or_else(|| str_field(a, "id"))
                            .unwrap_or_default()
                            .to_owned()
                    })
                    .collect::<Vec<_>>();
                let reason = audit.entry.as_ref().and_then(|e| e.reason.clone());
                let details: Vec<Option<String>> = vec![
                    Some(format!("message_id={}", delete.id)),
                    Some(format!(
                        "author={} ({})",
                        author_tag
                            .clone()
                            .unwrap_or_else(|| old.author_id.to_string()),
                        old.author_id
                    )),
                    Some(if content.is_empty() {
                        "content=[empty]".to_owned()
                    } else {
                        format!("content={content}")
                    }),
                    Some(if attachments_list.is_empty() {
                        "attachments=[none]".to_owned()
                    } else {
                        format!("attachments={}", attachments_list.join(", "))
                    }),
                    actor_resolution_detail(audit.status),
                    reason
                        .as_deref()
                        .map(|r| format!("reason={}", reason_text(r))),
                ];
                self.log(app, guild_id, json!({
                    "category": "message",
                    "action": "delete",
                    "actor": actor_payload(deleter.as_ref()),
                    "target": {
                        "kind": "message",
                        "id": delete.id.to_string(),
                        "ownerId": old.author_id.to_string(),
                        "ownerTag": author_tag
                    },
                    "location": { "channelId": channel_id.to_string(), "channelName": channel_name(Some(channel_id)) },
                    "summary": match &deleter {
                        Some(d) => format!("Message deleted by moderator ({}).", discord::user_tag(d)),
                        None => "Message deleted. Actor unknown.".to_owned(),
                    },
                    "details": details.into_iter().flatten().collect::<Vec<_>>(),
                    "metadata": {
                        "messageId": delete.id.to_string(),
                        "channelId": channel_id.to_string(),
                        "ownerId": old.author_id.to_string(),
                        "ownerTag": author_tag,
                        "deletedBy": deleter.as_ref().map(|d| d.id.to_string()),
                        "actorResolution": actor_resolution(audit.status),
                        "reason": reason,
                        "content": content,
                        "attachments": old.attachments
                    }
                }), false);
            }

            Event::MessageDeleteBulk(bulk) => {
                let Some(guild_id) = bulk.guild_id else {
                    return Ok(());
                };
                let audit = self
                    .resolve_audit(
                        app,
                        guild_id,
                        AuditLogEventType::MessageBulkDelete,
                        None,
                        |_| true,
                    )
                    .await;
                let executor = audit.executor.clone();
                let name = channel_name(Some(bulk.channel_id));
                let reason = audit.entry.as_ref().and_then(|e| e.reason.clone());
                let mut details = vec![
                    format!("count={}", bulk.ids.len()),
                    format!("channel=#{}", name.as_str().unwrap_or("unknown")),
                ];
                if let Some(reason) = &reason {
                    details.push(format!("reason={}", reason_text(reason)));
                }
                self.log(app, guild_id, json!({
                    "category": "message",
                    "action": "bulk_delete",
                    "actor": actor_payload(executor.as_ref()),
                    "target": { "kind": "channel", "id": bulk.channel_id.to_string(), "name": name },
                    "location": { "channelId": bulk.channel_id.to_string(), "channelName": name },
                    "summary": format!("{} messages deleted in bulk.", bulk.ids.len()),
                    "details": details,
                    "metadata": {
                        "count": bulk.ids.len(),
                        "channelId": bulk.channel_id.to_string(),
                        "deletedBy": executor.as_ref().map(|e| e.id.to_string()),
                        "reason": reason
                    }
                }), false);
            }

            Event::ReactionAdd(_) | Event::ReactionRemove(_) => {
                let (reaction, added) = match event {
                    Event::ReactionAdd(r) => (&r.0, true),
                    Event::ReactionRemove(r) => (&r.0, false),
                    _ => return Ok(()),
                };
                let Some(guild_id) = reaction.guild_id else {
                    return Ok(());
                };
                if reaction.user_id == app.bot_id
                    || app.cache.message(reaction.message_id).is_none()
                {
                    return Ok(());
                }
                let user = reaction
                    .member
                    .as_ref()
                    .map(|m| m.user.clone())
                    .or_else(|| user_from_cache(reaction.user_id));
                let Some(user) = user else { return Ok(()) };
                use twilight_model::channel::message::EmojiReactionType;
                let (identifier, display) = match &reaction.emoji {
                    EmojiReactionType::Custom { id, name, animated } => {
                        let name = name.clone().unwrap_or_default();
                        (
                            format!("{}{name}:{id}", if *animated { "a:" } else { "" }),
                            format!("<{}:{name}:{id}>", if *animated { "a" } else { "" }),
                        )
                    }
                    EmojiReactionType::Unicode { name } => (name.clone(), name.clone()),
                };
                let verb = if added { "added" } else { "removed" };
                self.log(app, guild_id, json!({
                    "category": "reaction",
                    "action": if added { "add" } else { "remove" },
                    "actor": actor_payload(Some(&user)),
                    "target": { "kind": "message", "id": reaction.message_id.to_string() },
                    "location": { "channelId": reaction.channel_id.to_string(), "channelName": channel_name(Some(reaction.channel_id)) },
                    "summary": format!("Reaction {verb}: {display}."),
                    "details": [format!("emoji={identifier}"), format!("message_id={}", reaction.message_id)],
                    "metadata": {
                        "emoji": identifier,
                        "channelId": reaction.channel_id.to_string(),
                        "messageId": reaction.message_id.to_string()
                    }
                }), false);
            }

            Event::MemberAdd(member) => {
                let user = &member.member.user;
                let created = snowflake_ms(user.id.get()) / 1000;
                let member_count = app
                    .cache
                    .guild(member.guild_id)
                    .and_then(|g| g.member_count());
                self.log(app, member.guild_id, json!({
                    "category": "member",
                    "action": "join",
                    "actor": actor_payload(Some(user)),
                    "target": { "kind": "member", "id": user.id.to_string(), "tag": discord::user_tag(user) },
                    "summary": "Member joined the server.",
                    "details": [
                        format!("account_created=<t:{created}:F>"),
                        format!("member_count={}", member_count.map_or("unknown".to_owned(), |c| c.to_string())),
                    ],
                    "metadata": { "userId": user.id.to_string(), "memberCount": member_count }
                }), false);
            }

            Event::MemberRemove(member) => {
                let user = &member.user;
                let (kick, ban) = tokio::join!(
                    self.resolve_audit(
                        app,
                        member.guild_id,
                        AuditLogEventType::MemberKick,
                        Some(user.id.get()),
                        |_| true
                    ),
                    self.resolve_audit(
                        app,
                        member.guild_id,
                        AuditLogEventType::MemberBanAdd,
                        Some(user.id.get()),
                        |_| true
                    ),
                );
                let (action, audit) = if ban.entry.is_some() {
                    ("ban", &ban)
                } else if kick.entry.is_some() {
                    ("kick", &kick)
                } else if ban.status == AuditStatus::Miss && kick.status == AuditStatus::Miss {
                    ("leave", &ban)
                } else {
                    ("remove", &ban)
                };
                let status = if audit.entry.is_some() {
                    AuditStatus::Matched
                } else if ban.status != AuditStatus::Miss {
                    ban.status
                } else {
                    kick.status
                };
                let reason = audit.entry.as_ref().and_then(|e| e.reason.clone());
                let actor = if audit.entry.is_some() {
                    audit.executor.clone()
                } else {
                    None
                };
                let summary = match action {
                    "ban" => "Member banned.",
                    "kick" => "Member kicked.",
                    "leave" => "Member left the server.",
                    _ => "Member left or was removed from the server. Actor unknown.",
                };
                let details: Vec<String> = [
                    Some(format!("user={} ({})", discord::user_tag(user), user.id)),
                    actor_resolution_detail(status),
                    reason
                        .as_deref()
                        .map(|r| format!("reason={}", reason_text(r))),
                ]
                .into_iter()
                .flatten()
                .collect();
                self.log(app, member.guild_id, json!({
                    "category": "member",
                    "action": action,
                    "actor": actor_payload(actor.as_ref()),
                    "target": { "kind": "member", "id": user.id.to_string(), "tag": discord::user_tag(user) },
                    "summary": summary,
                    "details": details,
                    "metadata": {
                        "userId": user.id.to_string(),
                        "reason": reason,
                        "actorId": actor.as_ref().map(|a| a.id.to_string()),
                        "actorResolution": actor_resolution(status),
                        "action": action
                    }
                }), false);
            }

            Event::MemberUpdate(update) => {
                let Before::Member(Some((old_roles, old_nick))) = before else {
                    return Ok(());
                };
                let nick_delta = format_state_delta(
                    old_nick.as_deref().unwrap_or_default(),
                    update.nick.as_deref().unwrap_or_default(),
                );
                let new_roles: Vec<String> = update.roles.iter().map(ToString::to_string).collect();
                let added: Vec<&String> = new_roles
                    .iter()
                    .filter(|r| !old_roles.contains(r))
                    .collect();
                let removed: Vec<&String> = old_roles
                    .iter()
                    .filter(|r| !new_roles.contains(r))
                    .collect();
                if nick_delta.is_none() && added.is_empty() && removed.is_empty() {
                    return Ok(());
                }
                let role_name = |id: &String| {
                    discord::parse_id(id)
                        .and_then(|rid| app.cache.role(rid).map(|r| r.name.clone()))
                };
                let mut details = Vec::new();
                if let Some(delta) = &nick_delta {
                    details.push(format!("nickname={delta}"));
                }
                if !added.is_empty() {
                    details.push(format!(
                        "roles_added={}",
                        added
                            .iter()
                            .map(|r| format_role_ref(role_name(r).as_deref()))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                if !removed.is_empty() {
                    details.push(format!(
                        "roles_removed={}",
                        removed
                            .iter()
                            .map(|r| format_role_ref(role_name(r).as_deref()))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                let user = &update.user;
                self.log(app, update.guild_id, json!({
                    "category": "member",
                    "action": "update",
                    "actor": actor_payload(Some(user)),
                    "target": { "kind": "member", "id": user.id.to_string(), "tag": discord::user_tag(user) },
                    "summary": "Member profile/roles updated.",
                    "details": details,
                    "metadata": {
                        "userId": user.id.to_string(),
                        "nicknameDelta": nick_delta,
                        "addedRoles": added,
                        "removedRoles": removed
                    }
                }), false);
            }

            Event::BanAdd(_) | Event::BanRemove(_) => {
                let (ban_guild, ban_user, added) = match event {
                    Event::BanAdd(b) => (b.guild_id, &b.user, true),
                    Event::BanRemove(b) => (b.guild_id, &b.user, false),
                    _ => return Ok(()),
                };
                let kind = if added {
                    AuditLogEventType::MemberBanAdd
                } else {
                    AuditLogEventType::MemberBanRemove
                };
                let audit = self
                    .resolve_audit(app, ban_guild, kind, Some(ban_user.id.get()), |_| true)
                    .await;
                let actor = audit.executor.clone();
                let tag = discord::user_tag(ban_user);
                let reason = audit.entry.as_ref().and_then(|e| e.reason.clone());
                let verb = if added { "added" } else { "removed" };
                let summary = if actor.is_some() {
                    format!("Ban {verb} for {tag}.")
                } else {
                    format!("Ban {verb} for {tag}. Actor unknown.")
                };
                let details: Vec<String> = [
                    Some(format!("target={tag} ({})", ban_user.id)),
                    actor_resolution_detail(audit.status),
                    reason
                        .as_deref()
                        .map(|r| format!("reason={}", reason_text(r))),
                ]
                .into_iter()
                .flatten()
                .collect();
                self.log(
                    app,
                    ban_guild,
                    json!({
                        "category": "moderation",
                        "action": if added { "ban_add" } else { "ban_remove" },
                        "actor": actor_payload(actor.as_ref()),
                        "target": { "kind": "member", "id": ban_user.id.to_string(), "tag": tag },
                        "summary": summary,
                        "details": details,
                        "metadata": {
                            "userId": ban_user.id.to_string(),
                            "actorId": actor.as_ref().map(|a| a.id.to_string()),
                            "actorResolution": actor_resolution(audit.status),
                            "reason": reason
                        }
                    }),
                    false,
                );
            }

            Event::ChannelCreate(channel) => {
                let Some(guild_id) = channel.guild_id else {
                    return Ok(());
                };
                let audit = self
                    .resolve_audit(
                        app,
                        guild_id,
                        AuditLogEventType::ChannelCreate,
                        Some(channel.id.get()),
                        |_| true,
                    )
                    .await;
                let actor = audit.executor.clone();
                let name = channel.name.clone();
                self.log(app, guild_id, json!({
                    "category": "channel",
                    "action": "create",
                    "actor": actor_payload(actor.as_ref()),
                    "target": { "kind": "channel", "id": channel.id.to_string(), "name": name },
                    "summary": format!("Channel created: #{}.", display_name(name.as_deref())),
                    "details": [
                        format!("channel_id={}", channel.id),
                        format!("type={}", channel_kind_number(channel)),
                        format!("parent_id={}", channel.parent_id.map_or("none".to_owned(), |p| p.to_string())),
                    ],
                    "metadata": {
                        "channelId": channel.id.to_string(),
                        "type": channel_kind_number(channel),
                        "parentId": channel.parent_id.map(|p| p.to_string()),
                        "actorId": actor.as_ref().map(|a| a.id.to_string())
                    }
                }), false);
            }

            Event::ChannelDelete(channel) => {
                let Some(guild_id) = channel.guild_id else {
                    return Ok(());
                };
                let audit = self
                    .resolve_audit(
                        app,
                        guild_id,
                        AuditLogEventType::ChannelDelete,
                        Some(channel.id.get()),
                        |_| true,
                    )
                    .await;
                let actor = audit.executor.clone();
                let name = channel.name.clone();
                let reason = audit.entry.as_ref().and_then(|e| e.reason.clone());
                let details: Vec<String> = [
                    Some(format!("channel_id={}", channel.id)),
                    Some(format!("type={}", channel_kind_number(channel))),
                    actor_resolution_detail(audit.status),
                    reason
                        .as_deref()
                        .map(|r| format!("reason={}", reason_text(r))),
                ]
                .into_iter()
                .flatten()
                .collect();
                self.log(app, guild_id, json!({
                    "category": "channel",
                    "action": "delete",
                    "actor": actor_payload(actor.as_ref()),
                    "target": { "kind": "channel", "id": channel.id.to_string(), "name": name },
                    "summary": if actor.is_some() {
                        format!("Channel deleted: #{}.", display_name(name.as_deref()))
                    } else {
                        format!("Channel deleted: #{}. Actor unknown.", display_name(name.as_deref()))
                    },
                    "details": details,
                    "metadata": {
                        "channelId": channel.id.to_string(),
                        "actorId": actor.as_ref().map(|a| a.id.to_string()),
                        "actorResolution": actor_resolution(audit.status),
                        "reason": reason
                    }
                }), false);
            }

            Event::ChannelUpdate(update) => {
                let Some(guild_id) = update.guild_id else {
                    return Ok(());
                };
                let Before::Channel(Some(old)) = before else {
                    return Ok(());
                };
                let new = &update.0;
                let mut details = Vec::new();
                if let Some(d) = format_state_delta(
                    old.name.as_deref().unwrap_or_default(),
                    new.name.as_deref().unwrap_or_default(),
                ) {
                    details.push(format!("name={d}"));
                }
                if let Some(d) = format_state_delta(
                    old.topic.as_deref().unwrap_or_default(),
                    new.topic.as_deref().unwrap_or_default(),
                ) {
                    details.push(format!("topic={d}"));
                }
                if let Some(d) = format_state_delta(&opt_bool(old.nsfw), &opt_bool(new.nsfw)) {
                    details.push(format!("nsfw={d}"));
                }
                if let Some(d) = format_state_delta(
                    &opt_num(old.rate_limit_per_user),
                    &opt_num(new.rate_limit_per_user),
                ) {
                    details.push(format!("slowmode_sec={d}"));
                }
                if let Some(d) = format_state_delta(
                    &old.parent_id.map(|p| p.to_string()).unwrap_or_default(),
                    &new.parent_id.map(|p| p.to_string()).unwrap_or_default(),
                ) {
                    details.push(format!("parent_id={d}"));
                }
                if details.is_empty() {
                    return Ok(());
                }
                let audit = self
                    .resolve_audit(
                        app,
                        guild_id,
                        AuditLogEventType::ChannelUpdate,
                        Some(new.id.get()),
                        |_| true,
                    )
                    .await;
                let actor = audit.executor.clone();
                self.log(app, guild_id, json!({
                    "category": "channel",
                    "action": "update",
                    "actor": actor_payload(actor.as_ref()),
                    "target": { "kind": "channel", "id": new.id.to_string(), "name": new.name },
                    "summary": format!("Channel updated: #{}.", display_name(new.name.as_deref())),
                    "details": details,
                    "metadata": {
                        "channelId": new.id.to_string(),
                        "actorId": actor.as_ref().map(|a| a.id.to_string()),
                        "reason": audit.entry.as_ref().and_then(|e| e.reason.clone()),
                        "changes": details
                    }
                }), false);
            }

            Event::ThreadCreate(thread) => {
                let Some(guild_id) = thread.guild_id else {
                    return Ok(());
                };
                let archived = thread.thread_metadata.as_ref().is_some_and(|m| m.archived);
                self.log(app, guild_id, json!({
                    "category": "thread",
                    "action": "create",
                    "actor": thread.owner_id.map_or(Value::Null, |o| json!({ "id": o.to_string(), "tag": null, "bot": false })),
                    "target": { "kind": "thread", "id": thread.id.to_string(), "name": thread.name },
                    "location": { "channelId": thread.parent_id.map(|p| p.to_string()), "channelName": channel_name(thread.parent_id) },
                    "summary": format!("Thread created: {}.", display_name(thread.name.as_deref())),
                    "details": [
                        format!("thread_id={}", thread.id),
                        format!("parent_id={}", thread.parent_id.map_or("unknown".to_owned(), |p| p.to_string())),
                        format!("archived={archived}"),
                    ],
                    "metadata": { "threadId": thread.id.to_string(), "parentId": thread.parent_id.map(|p| p.to_string()) }
                }), false);
            }

            Event::ThreadDelete(thread) => {
                let name = app.cache.channel(thread.id).and_then(|c| c.name.clone());
                self.log(app, thread.guild_id, json!({
                    "category": "thread",
                    "action": "delete",
                    "target": { "kind": "thread", "id": thread.id.to_string(), "name": name },
                    "location": { "channelId": thread.parent_id.to_string(), "channelName": channel_name(Some(thread.parent_id)) },
                    "summary": format!("Thread deleted: {}.", display_name(name.as_deref())),
                    "details": [format!("thread_id={}", thread.id), format!("parent_id={}", thread.parent_id)],
                    "metadata": { "threadId": thread.id.to_string(), "parentId": thread.parent_id.to_string() }
                }), false);
            }

            Event::ThreadUpdate(update) => {
                let Some(guild_id) = update.guild_id else {
                    return Ok(());
                };
                let Before::Channel(Some(old)) = before else {
                    return Ok(());
                };
                let new = &update.0;
                let meta = |c: &Channel| {
                    c.thread_metadata
                        .as_ref()
                        .map(|m| (m.archived, m.locked))
                        .unwrap_or_default()
                };
                let (old_archived, old_locked) = meta(&old);
                let (new_archived, new_locked) = meta(new);
                let mut details = Vec::new();
                if let Some(d) = format_state_delta(
                    old.name.as_deref().unwrap_or_default(),
                    new.name.as_deref().unwrap_or_default(),
                ) {
                    details.push(format!("name={d}"));
                }
                if let Some(d) = delta_bool(old_archived, new_archived) {
                    details.push(format!("archived={d}"));
                }
                if let Some(d) = delta_bool(old_locked, new_locked) {
                    details.push(format!("locked={d}"));
                }
                if details.is_empty() {
                    return Ok(());
                }
                self.log(app, guild_id, json!({
                    "category": "thread",
                    "action": "update",
                    "target": { "kind": "thread", "id": new.id.to_string(), "name": new.name },
                    "location": { "channelId": new.parent_id.map(|p| p.to_string()), "channelName": channel_name(new.parent_id) },
                    "summary": format!("Thread updated: {}.", display_name(new.name.as_deref())),
                    "details": details,
                    "metadata": { "threadId": new.id.to_string(), "parentId": new.parent_id.map(|p| p.to_string()), "changes": details }
                }), false);
            }

            Event::RoleCreate(created) => {
                let role = &created.role;
                let audit = self
                    .resolve_audit(
                        app,
                        created.guild_id,
                        AuditLogEventType::RoleCreate,
                        Some(role.id.get()),
                        |_| true,
                    )
                    .await;
                let actor = audit.executor.clone();
                self.log(app, created.guild_id, json!({
                    "category": "role",
                    "action": "create",
                    "actor": actor_payload(actor.as_ref()),
                    "target": { "kind": "role", "id": role.id.to_string(), "name": role.name },
                    "summary": format!("Role created: {}.", role.name),
                    "details": [
                        format!("role_id={}", role.id),
                        format!("color={}", role_hex(role)),
                        format!("hoist={}", role.hoist),
                        format!("mentionable={}", role.mentionable),
                    ],
                    "metadata": { "roleId": role.id.to_string(), "actorId": actor.as_ref().map(|a| a.id.to_string()) }
                }), false);
            }

            Event::RoleDelete(deleted) => {
                let Before::Role(old) = before else {
                    return Ok(());
                };
                let name = old.as_ref().map(|r| r.name.clone());
                let position = old
                    .as_ref()
                    .map_or("unknown".to_owned(), |r| r.position.to_string());
                let audit = self
                    .resolve_audit(
                        app,
                        deleted.guild_id,
                        AuditLogEventType::RoleDelete,
                        Some(deleted.role_id.get()),
                        |_| true,
                    )
                    .await;
                let actor = audit.executor.clone();
                let reason = audit.entry.as_ref().and_then(|e| e.reason.clone());
                let label = name.clone().unwrap_or_else(|| "role".to_owned());
                let details: Vec<String> = [
                    Some(format!("role_id={}", deleted.role_id)),
                    Some(format!("position={position}")),
                    actor_resolution_detail(audit.status),
                    reason
                        .as_deref()
                        .map(|r| format!("reason={}", reason_text(r))),
                ]
                .into_iter()
                .flatten()
                .collect();
                self.log(app, deleted.guild_id, json!({
                    "category": "role",
                    "action": "delete",
                    "actor": actor_payload(actor.as_ref()),
                    "target": { "kind": "role", "id": deleted.role_id.to_string(), "name": name },
                    "summary": if actor.is_some() { format!("Role deleted: {label}.") } else { format!("Role deleted: {label}. Actor unknown.") },
                    "details": details,
                    "metadata": {
                        "roleId": deleted.role_id.to_string(),
                        "actorId": actor.as_ref().map(|a| a.id.to_string()),
                        "actorResolution": actor_resolution(audit.status),
                        "reason": reason
                    }
                }), false);
            }

            Event::RoleUpdate(update) => {
                let Before::Role(Some(old)) = before else {
                    return Ok(());
                };
                let new = &update.role;
                let mut details = Vec::new();
                let mut changes = 0;
                if let Some(d) = format_state_delta(&old.name, &new.name) {
                    details.push(format!("Name: {d}"));
                    changes += 1;
                }
                if let Some(d) = format_state_delta(&role_hex(&old), &role_hex(new)) {
                    details.push(format!("Color: {d}"));
                    changes += 1;
                }
                let hoist = delta_bool(old.hoist, new.hoist);
                let mentionable = delta_bool(old.mentionable, new.mentionable);
                if hoist.is_some() || mentionable.is_some() {
                    let mut parts = Vec::new();
                    if let Some(d) = hoist {
                        parts.push(format!("hoist={d}"));
                        changes += 1;
                    }
                    if let Some(d) = mentionable {
                        parts.push(format!("mentionable={d}"));
                        changes += 1;
                    }
                    details.push(format!("Display: {}", parts.join(", ")));
                }
                if let Some(d) =
                    format_state_delta(&old.position.to_string(), &new.position.to_string())
                {
                    details.push(format!("Position: {d}"));
                    changes += 1;
                }
                let added = permission_names(new.permissions - old.permissions);
                let removed = permission_names(old.permissions - new.permissions);
                if !added.is_empty() {
                    details.push(format!(
                        "Permissions (+): {}",
                        format_list_with_limit(&added, 5)
                    ));
                    changes += 1;
                }
                if !removed.is_empty() {
                    details.push(format!(
                        "Permissions (-): {}",
                        format_list_with_limit(&removed, 5)
                    ));
                    changes += 1;
                }
                if details.is_empty() {
                    return Ok(());
                }
                let audit = self
                    .resolve_audit(
                        app,
                        update.guild_id,
                        AuditLogEventType::RoleUpdate,
                        Some(new.id.get()),
                        |_| true,
                    )
                    .await;
                let actor = audit.executor.clone();
                let reason = audit.entry.as_ref().and_then(|e| e.reason.clone());
                let mut all_details = details.clone();
                if let Some(reason) = &reason {
                    all_details.push(format!("Reason: {}", reason_text(reason)));
                }
                self.log(app, update.guild_id, json!({
                    "category": "role",
                    "action": "update",
                    "actor": actor_payload(actor.as_ref()),
                    "target": { "kind": "role", "id": new.id.to_string(), "name": new.name },
                    "summary": format!("Role updated: {} ({changes} change{}).", new.name, if changes == 1 { "" } else { "s" }),
                    "details": all_details,
                    "metadata": {
                        "roleId": new.id.to_string(),
                        "actorId": actor.as_ref().map(|a| a.id.to_string()),
                        "reason": reason,
                        "changes": details
                    }
                }), false);
            }

            Event::VoiceStateUpdate(update) => {
                let Some(guild_id) = update.guild_id else {
                    return Ok(());
                };
                let user = update
                    .member
                    .as_ref()
                    .map(|m| m.user.clone())
                    .or_else(|| user_from_cache(update.user_id));
                let Some(user) = user else { return Ok(()) };
                let Before::Voice(old) = before else {
                    return Ok(());
                };
                let (old_channel, old_mute, old_deaf, old_self_mute, old_self_deaf) =
                    old.unwrap_or((None, false, false, false, false));
                let new_channel = update.channel_id;
                let label = |id: Option<Id<ChannelMarker>>| match id {
                    None => "none".to_owned(),
                    Some(id) => simplify_channel_name(
                        app.cache
                            .channel(id)
                            .and_then(|c| c.name.clone())
                            .as_deref(),
                    ),
                };
                let before_name = label(old_channel);
                let after_name = label(new_channel);
                let action = match (old_channel, new_channel) {
                    (None, Some(_)) => "join",
                    (Some(_), None) => "leave",
                    (Some(a), Some(b)) if a != b => "move",
                    _ => "state_update",
                };
                let mut details = Vec::new();
                if action != "state_update" {
                    details.push(format!("from={before_name}"));
                    details.push(format!("to={after_name}"));
                }
                if let Some(d) = delta_bool(old_mute, update.mute) {
                    details.push(format!("Server mute: {d}"));
                }
                if let Some(d) = delta_bool(old_deaf, update.deaf) {
                    details.push(format!("Server deaf: {d}"));
                }
                if let Some(d) = delta_bool(old_self_mute, update.self_mute) {
                    details.push(format!("Self mute: {d}"));
                }
                if let Some(d) = delta_bool(old_self_deaf, update.self_deaf) {
                    details.push(format!("Self deaf: {d}"));
                }
                if details.is_empty() {
                    return Ok(());
                }
                let mut actor = actor_payload(Some(&user));
                let mut status = AuditStatus::Matched;
                if action == "move" {
                    let audit = self
                        .resolve_audit(
                            app,
                            guild_id,
                            AuditLogEventType::MemberMove,
                            Some(user.id.get()),
                            |entry| {
                                entry
                                    .options
                                    .as_ref()
                                    .and_then(|o| o.count.as_deref())
                                    .and_then(|c| c.parse::<f64>().ok())
                                    .is_none_or(|c| c >= 1.0)
                            },
                        )
                        .await;
                    match audit.executor {
                        Some(executor) => actor = actor_payload(Some(&executor)),
                        None => {
                            actor = Value::Null;
                            status = audit.status;
                        }
                    }
                }
                let location_name = if action == "move" {
                    format!("{before_name} -> {after_name}")
                } else if after_name != "none" {
                    after_name.clone()
                } else {
                    before_name.clone()
                };
                let summary = match action {
                    "join" => "User joined voice channel.".to_owned(),
                    "leave" => "User left voice channel.".to_owned(),
                    "move" if !actor.is_null() => "User moved between voice channels.".to_owned(),
                    "move" => "User moved between voice channels. Actor unknown.".to_owned(),
                    _ => format!("Voice state changed for {}.", discord::user_tag(&user)),
                };
                let mut all_details = details.clone();
                if action == "move"
                    && let Some(detail) = actor_resolution_detail(status)
                {
                    all_details.push(detail);
                }
                self.log(app, guild_id, json!({
                    "category": "voice",
                    "action": action,
                    "actor": actor,
                    "target": { "kind": "member", "id": user.id.to_string(), "tag": discord::user_tag(&user) },
                    "location": {
                        "channelId": new_channel.or(old_channel).map(|c| c.to_string()),
                        "channelName": location_name
                    },
                    "summary": summary,
                    "details": all_details,
                    "metadata": {
                        "userId": user.id.to_string(),
                        "beforeChannel": old_channel.map(|c| c.to_string()),
                        "afterChannel": new_channel.map(|c| c.to_string()),
                        "changes": details,
                        "actorResolution": if action == "move" { actor_resolution(status) } else { "event" }
                    }
                }), false);
            }

            Event::InviteCreate(invite) => {
                let expires = (invite.max_age > 0)
                    .then(|| {
                        DateTime::<Utc>::from_timestamp_micros(invite.created_at.as_micros()).map(
                            |c| {
                                operator_common::time::to_iso(
                                    c + chrono::Duration::seconds(invite.max_age as i64),
                                )
                            },
                        )
                    })
                    .flatten()
                    .unwrap_or_else(|| "never".to_owned());
                let inviter = invite.inviter.as_ref();
                self.log(app, invite.guild_id, json!({
                    "category": "invite",
                    "action": "create",
                    "actor": actor_payload(inviter),
                    "target": { "kind": "invite", "id": invite.code },
                    "location": { "channelId": invite.channel_id.to_string(), "channelName": channel_name(Some(invite.channel_id)) },
                    "summary": format!("Invite created: {}.", invite.code),
                    "details": [
                        format!("code={}", invite.code),
                        format!("max_uses={}", invite.max_uses),
                        format!("temporary={}", invite.temporary),
                        format!("expires_at={expires}"),
                    ],
                    "metadata": {
                        "code": invite.code,
                        "channelId": invite.channel_id.to_string(),
                        "inviterId": inviter.map(|i| i.id.to_string())
                    }
                }), false);
            }

            Event::InviteDelete(invite) => {
                let name = channel_name(Some(invite.channel_id));
                self.log(app, invite.guild_id, json!({
                    "category": "invite",
                    "action": "delete",
                    "target": { "kind": "invite", "id": invite.code },
                    "location": { "channelId": invite.channel_id.to_string(), "channelName": name },
                    "summary": format!("Invite deleted: {}.", invite.code),
                    "details": [format!("code={}", invite.code), format!("channel=#{}", name.as_str().unwrap_or("unknown"))],
                    "metadata": { "code": invite.code, "channelId": invite.channel_id.to_string() }
                }), false);
            }

            Event::GuildEmojisUpdate(update) => {
                let Before::Emojis(old) = before else {
                    return Ok(());
                };
                let new: Vec<(String, String, bool)> = update
                    .emojis
                    .iter()
                    .map(|e| (e.id.to_string(), e.name.clone(), e.animated))
                    .collect();
                let old_ids: HashSet<&String> = old.iter().map(|(id, _, _)| id).collect();
                let new_ids: HashSet<&String> = new.iter().map(|(id, _, _)| id).collect();
                for (id, name, animated) in new.iter().filter(|(id, _, _)| !old_ids.contains(id)) {
                    self.log(
                        app,
                        update.guild_id,
                        json!({
                            "category": "emoji", "action": "create",
                            "target": { "kind": "emoji", "id": id, "name": name },
                            "summary": format!("Emoji created: {name}."),
                            "details": [format!("emoji_id={id}"), format!("animated={animated}")],
                            "metadata": { "emojiId": id, "name": name }
                        }),
                        false,
                    );
                }
                for (id, name, _) in old.iter().filter(|(id, _, _)| !new_ids.contains(id)) {
                    self.log(
                        app,
                        update.guild_id,
                        json!({
                            "category": "emoji", "action": "delete",
                            "target": { "kind": "emoji", "id": id, "name": name },
                            "summary": format!("Emoji deleted: {name}."),
                            "details": [format!("emoji_id={id}")],
                            "metadata": { "emojiId": id, "name": name }
                        }),
                        false,
                    );
                }
                for (id, name, _) in &new {
                    let Some((_, old_name, _)) = old.iter().find(|(old_id, _, _)| old_id == id)
                    else {
                        continue;
                    };
                    let Some(delta) = format_state_delta(old_name, name) else {
                        continue;
                    };
                    self.log(
                        app,
                        update.guild_id,
                        json!({
                            "category": "emoji", "action": "update",
                            "target": { "kind": "emoji", "id": id, "name": name },
                            "summary": "Emoji renamed.",
                            "details": [format!("name={delta}")],
                            "metadata": { "emojiId": id, "nameDelta": delta }
                        }),
                        false,
                    );
                }
            }

            Event::GuildStickersUpdate(update) => {
                let Before::Stickers(old) = before else {
                    return Ok(());
                };
                let new: Vec<(String, String, String, u8)> = update
                    .stickers
                    .iter()
                    .map(|s| {
                        (
                            s.id.to_string(),
                            s.name.clone(),
                            s.description.clone().unwrap_or_default(),
                            u8::from(s.format_type),
                        )
                    })
                    .collect();
                let old_ids: HashSet<&String> = old.iter().map(|s| &s.0).collect();
                let new_ids: HashSet<&String> = new.iter().map(|s| &s.0).collect();
                for (id, name, _, format) in new.iter().filter(|s| !old_ids.contains(&s.0)) {
                    self.log(
                        app,
                        update.guild_id,
                        json!({
                            "category": "sticker", "action": "create",
                            "target": { "kind": "sticker", "id": id, "name": name },
                            "summary": format!("Sticker created: {name}."),
                            "details": [format!("sticker_id={id}"), format!("format={format}")],
                            "metadata": { "stickerId": id, "name": name }
                        }),
                        false,
                    );
                }
                for (id, name, _, _) in old.iter().filter(|s| !new_ids.contains(&s.0)) {
                    self.log(
                        app,
                        update.guild_id,
                        json!({
                            "category": "sticker", "action": "delete",
                            "target": { "kind": "sticker", "id": id, "name": name },
                            "summary": format!("Sticker deleted: {name}."),
                            "details": [format!("sticker_id={id}")],
                            "metadata": { "stickerId": id, "name": name }
                        }),
                        false,
                    );
                }
                for (id, name, description, _) in &new {
                    let Some((_, old_name, old_description, _)) = old.iter().find(|s| &s.0 == id)
                    else {
                        continue;
                    };
                    let mut details = Vec::new();
                    if let Some(d) = format_state_delta(old_name, name) {
                        details.push(format!("name={d}"));
                    }
                    if let Some(d) = format_state_delta(old_description, description) {
                        details.push(format!("description={d}"));
                    }
                    if details.is_empty() {
                        continue;
                    }
                    self.log(
                        app,
                        update.guild_id,
                        json!({
                            "category": "sticker", "action": "update",
                            "target": { "kind": "sticker", "id": id, "name": name },
                            "summary": format!("Sticker updated: {name}."),
                            "details": details,
                            "metadata": { "stickerId": id, "changes": details }
                        }),
                        false,
                    );
                }
            }

            Event::WebhooksUpdate(update) => {
                let name = channel_name(Some(update.channel_id));
                self.log(app, update.guild_id, json!({
                    "category": "webhook",
                    "action": "update",
                    "target": { "kind": "channel", "id": update.channel_id.to_string(), "name": name },
                    "location": { "channelId": update.channel_id.to_string(), "channelName": name },
                    "summary": format!("Webhook list changed in #{}.", display_name(name.as_str())),
                    "details": [format!("channel_id={}", update.channel_id)],
                    "metadata": { "channelId": update.channel_id.to_string() }
                }), false);
            }

            Event::GuildUpdate(update) => {
                let Before::Guild(Some((old_name, old_icon, old_afk, old_verification))) = before
                else {
                    return Ok(());
                };
                let mut details = Vec::new();
                if let Some(d) = format_state_delta(&old_name, &update.name) {
                    details.push(format!("name={d}"));
                }
                if let Some(d) = format_state_delta(
                    old_icon.as_deref().unwrap_or_default(),
                    &update.icon.map(|i| i.to_string()).unwrap_or_default(),
                ) {
                    details.push(format!("icon_hash={d}"));
                }
                if let Some(d) = format_state_delta(
                    old_afk.as_deref().unwrap_or_default(),
                    &update
                        .afk_channel_id
                        .map(|c| c.to_string())
                        .unwrap_or_default(),
                ) {
                    details.push(format!("afk_channel={d}"));
                }
                if let Some(d) = format_state_delta(
                    &old_verification.to_string(),
                    &u8::from(update.verification_level).to_string(),
                ) {
                    details.push(format!("verification_level={d}"));
                }
                if details.is_empty() {
                    return Ok(());
                }
                let audit = self
                    .resolve_audit(app, update.id, AuditLogEventType::GuildUpdate, None, |_| {
                        true
                    })
                    .await;
                let actor = audit.executor.clone();
                self.log(app, update.id, json!({
                    "category": "guild",
                    "action": "update",
                    "actor": actor_payload(actor.as_ref()),
                    "target": { "kind": "guild", "id": update.id.to_string(), "name": update.name },
                    "summary": format!("Guild updated: {}.", update.name),
                    "details": details,
                    "metadata": {
                        "guildId": update.id.to_string(),
                        "actorId": actor.as_ref().map(|a| a.id.to_string()),
                        "reason": audit.entry.as_ref().and_then(|e| e.reason.clone()),
                        "changes": details
                    }
                }), false);
            }

            Event::AutoModerationActionExecution(execution) => {
                let user = user_from_cache(execution.user_id);
                let action_type = u8::from(execution.action.kind);
                let trigger = u8::from(execution.rule_trigger_type);
                let details: Vec<String> = [
                    Some(format!("rule_id={}", execution.rule_id)),
                    Some(format!("rule_trigger={trigger}")),
                    Some(format!("action_type={action_type}")),
                    execution
                        .matched_keyword
                        .as_deref()
                        .map(|k| format!("keyword={}", inline(k, 80))),
                    (!execution.content.is_empty()).then(|| {
                        format!(
                            "content={}",
                            block(&execution.content, MAX_DETAIL_VALUE_LENGTH)
                        )
                    }),
                ]
                .into_iter()
                .flatten()
                .collect();
                self.log(
                    app,
                    execution.guild_id,
                    json!({
                        "category": "automod",
                        "action": "execution",
                        "actor": actor_payload(user.as_ref()),
                        "target": {
                            "kind": "member",
                            "id": execution.user_id.to_string(),
                            "tag": user.as_ref().map(discord::user_tag)
                        },
                        "location": {
                            "channelId": execution.channel_id.map(|c| c.to_string()),
                            "channelName": channel_name(execution.channel_id)
                        },
                        "summary": format!("AutoMod action executed: {action_type}."),
                        "details": details,
                        "metadata": {
                            "ruleId": execution.rule_id.to_string(),
                            "channelId": execution.channel_id.map(|c| c.to_string()),
                            "userId": execution.user_id.to_string(),
                            "actionType": action_type,
                            "matchedKeyword": execution.matched_keyword
                        }
                    }),
                    false,
                );
            }

            _ => {}
        }
        Ok(())
    }
}

fn display_name(name: Option<&str>) -> String {
    let text = inline(name.unwrap_or_default(), 80);
    if text.is_empty() {
        "unknown".to_owned()
    } else {
        text
    }
}

fn opt_bool(value: Option<bool>) -> String {
    value.map(|v| v.to_string()).unwrap_or_default()
}

fn opt_num(value: Option<u16>) -> String {
    value.map(|v| v.to_string()).unwrap_or_default()
}

/// Milliseconds since the Unix epoch encoded in a Discord snowflake.
pub fn snowflake_ms(id: u64) -> i64 {
    ((id >> 22) + 1_420_070_400_000) as i64
}

async fn append(path: &Path, payload: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    file.write_all(payload.as_bytes()).await?;
    file.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_lines_match_js_shapes() {
        let entry = json!({
            "category": "voice", "action": "join",
            "actor": { "id": "123456789012345678", "tag": "kuba", "bot": false },
            "location": { "channelId": "223456789012345678", "channelName": "Pogaduchy" },
            "metadata": { "afterChannel": "223456789012345678" },
            "details": []
        });
        assert_eq!(
            build_compact_log_line(&entry, "🔊"),
            "🔊 <@123456789012345678> joined to <#223456789012345678>"
        );

        let deleted = json!({
            "category": "message", "action": "delete", "actor": null,
            "target": { "ownerId": "123456789012345678" },
            "location": { "channelId": "223456789012345678" },
            "metadata": { "content": "hej \"tam\"", "attachments": [] },
            "details": []
        });
        assert_eq!(
            build_compact_log_line(&deleted, "💬"),
            "💬 deleted message in <#223456789012345678> (actor=unknown; author=<@123456789012345678>; content=\"hej 'tam'\")"
        );

        let generic = json!({
            "category": "system", "action": "test",
            "actor": null, "details": ["source=preview 123456789012345678", "id=5"]
        });
        assert_eq!(
            build_compact_log_line(&generic, "🧩"),
            "🧩 test (actor unknown) (source=preview)"
        );
    }

    #[test]
    fn helpers() {
        assert_eq!(simplify_channel_name(Some("🔊・Lobby")), "Lobby");
        assert_eq!(simplify_channel_name(Some("team: general")), "general");
        assert_eq!(format_state_delta("a", "a"), None);
        assert_eq!(format_state_delta("", "b").as_deref(), Some("none -> b"));
        assert_eq!(humanize_permission("VIEW_AUDIT_LOG"), "View Audit Log");
        assert!(is_valid_day_key("2026-02-28"));
        assert!(!is_valid_day_key("2026-02-30"));
        assert_eq!(normalize_retention(Some(1000.0), 30), 365);
        assert_eq!(snowflake_ms(0), 1_420_070_400_000);
    }
}
