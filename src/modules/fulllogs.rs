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
use crate::common::time::{now_iso, parse_iso};
use crate::config;
use crate::discord::{self, FLAG_COMPONENTS_V2, FLAG_EPHEMERAL};
use crate::util::text::{block, inline, is_snowflake, truncate};

mod card;
mod events;

const AUDIT_CACHE_TTL: Duration = Duration::from_millis(1500);
const AUDIT_MATCH_WINDOW_MS: i64 = 30_000;
const RETENTION_SWEEP_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
const WRITE_BATCH_DELAY: Duration = Duration::from_millis(25);
const AUDIT_FETCH_LIMIT: u16 = 10;
const MAX_SUMMARY_LENGTH: usize = 260;
const MAX_DETAIL_VALUE_LENGTH: usize = 320;
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

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn has_known_actor(actor: &Value) -> bool {
    if actor.is_null() {
        return false;
    }
    str_field(actor, "id").is_some_and(|id| is_snowflake(id, 15))
        || !inline(str_field(actor, "tag").unwrap_or_default(), 80).is_empty()
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
            json!({
                "id": user.id.to_string(),
                "tag": discord::user_tag(user),
                "bot": user.bot,
                "avatar": card::avatar_url(user),
            })
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
type DeliverySender = mpsc::Sender<(Id<ChannelMarker>, Value)>;

/// Discord-channel deliveries waiting per guild. Entries are already saved to the JSONL file,
/// so during a raid the overflow is only missing from the log channel, not lost.
const DELIVERY_QUEUE_LIMIT: usize = 1000;

#[derive(Debug, Clone)]
pub enum Before {
    None,
    Message(Option<CachedMessageLite>),
    Channel(Option<Box<Channel>>),
    Role(Option<Role>),
    /// (roles, nickname, timeout end in unix seconds)
    Member(Option<(Vec<String>, Option<String>, Option<i64>)>),
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
                    m.communication_disabled_until().map(|t| t.as_secs()),
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
    delivery_dropped: std::sync::atomic::AtomicU64,
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
            delivery_dropped: std::sync::atomic::AtomicU64::new(0),
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
            let (tx, mut rx) = mpsc::channel::<(Id<ChannelMarker>, Value)>(DELIVERY_QUEUE_LIMIT);
            let app = app.clone();
            let pending = Arc::clone(&self.delivery_pending);
            let guild = guild_id.to_owned();
            tokio::spawn(async move {
                while let Some((channel, entry)) = rx.recv().await {
                    let payload = card::card_payload(&entry, 0, None);
                    if let Err(error) = discord::send_message(&app, channel, &payload).await {
                        tracing::warn!("FullLogs delivery error ({guild}/{channel}): {error}");
                    }
                    pending.fetch_sub(1, Ordering::SeqCst);
                }
            });
            tx
        });
        self.delivery_pending.fetch_add(1, Ordering::SeqCst);
        match sender.try_send((channel_id, entry)) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.delivery_pending.fetch_sub(1, Ordering::SeqCst);
                let dropped = self.delivery_dropped.fetch_add(1, Ordering::SeqCst) + 1;
                if dropped == 1 || dropped.is_multiple_of(100) {
                    tracing::warn!(
                        "FullLogs: delivery queue for {guild_id} is full ({DELIVERY_QUEUE_LIMIT}); \
                         skipped {dropped} entries in the channel (they are in the log file)"
                    );
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.delivery_pending.fetch_sub(1, Ordering::SeqCst);
            }
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
                "summary": "Manual test entry from /logs test.",
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
                format!("The preview uses the last saved entry from `{day}`."),
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
                    "details": ["source=preview", "note=No saved entries, showing a sample format."],
                    "metadata": { "source": "preview:fallback" }
                }),
                "No saved entries. Showing a sample log format.".to_owned(),
            ),
        };
        card::card_payload(&entry, FLAG_EPHEMERAL, Some(("## Logs Preview", &note)))
    }

    /// Returns `(path, attachment name)` when the day file exists.
    pub async fn export_day(
        &self,
        guild_id: Id<GuildMarker>,
        day: &str,
    ) -> anyhow::Result<Option<(PathBuf, String)>> {
        if !is_valid_day_key(day) {
            anyhow::bail!("Invalid day format. Use YYYY-MM-DD.");
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
                anyhow::bail!("Invalid day format. Use YYYY-MM-DD.");
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
        match event {
            Event::InteractionCreate(_) => self.on_interaction_create(app, event, before).await,
            Event::MessageUpdate(_) => self.on_message_update(app, event, before).await,
            Event::MessageDelete(_) => self.on_message_delete(app, event, before).await,
            Event::MessageDeleteBulk(_) => self.on_message_delete_bulk(app, event, before).await,
            Event::ReactionAdd(_) | Event::ReactionRemove(_) => {
                self.on_reaction(app, event, before).await
            }
            Event::MemberAdd(_) => self.on_member_add(app, event, before).await,
            Event::MemberRemove(_) => self.on_member_remove(app, event, before).await,
            Event::MemberUpdate(_) => self.on_member_update(app, event, before).await,
            Event::BanAdd(_) | Event::BanRemove(_) => self.on_ban(app, event, before).await,
            Event::ChannelCreate(_) => self.on_channel_create(app, event, before).await,
            Event::ChannelDelete(_) => self.on_channel_delete(app, event, before).await,
            Event::ChannelUpdate(_) => self.on_channel_update(app, event, before).await,
            Event::ThreadCreate(_) => self.on_thread_create(app, event, before).await,
            Event::ThreadDelete(_) => self.on_thread_delete(app, event, before).await,
            Event::ThreadUpdate(_) => self.on_thread_update(app, event, before).await,
            Event::RoleCreate(_) => self.on_role_create(app, event, before).await,
            Event::RoleDelete(_) => self.on_role_delete(app, event, before).await,
            Event::RoleUpdate(_) => self.on_role_update(app, event, before).await,
            Event::VoiceStateUpdate(_) => self.on_voice_state_update(app, event, before).await,
            Event::InviteCreate(_) => self.on_invite_create(app, event, before).await,
            Event::InviteDelete(_) => self.on_invite_delete(app, event, before).await,
            Event::GuildEmojisUpdate(_) => self.on_guild_emojis_update(app, event, before).await,
            Event::GuildStickersUpdate(_) => {
                self.on_guild_stickers_update(app, event, before).await
            }
            Event::WebhooksUpdate(_) => self.on_webhooks_update(app, event, before).await,
            Event::GuildUpdate(_) => self.on_guild_update(app, event, before).await,
            Event::AutoModerationActionExecution(_) => {
                self.on_auto_moderation_action_execution(app, event, before)
                    .await
            }
            _ => Ok(()),
        }
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
