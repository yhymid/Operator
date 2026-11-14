//! Log cards: one Components V2 container per event, laid out like the big logging bots
//! (Quark, Dyno): a coloured accent per kind of event, a title, the member's avatar, "who /
//! where / by whom" lines, message content as quotes (before/after for edits), the remaining
//! details as `Key: value` lines and a footer with the ID and the time of the event.

#[allow(clippy::wildcard_imports)]
use super::*;

const RED: u32 = 0xED4245;
const GREEN: u32 = 0x57F287;
const YELLOW: u32 = 0xFEE75C;
const BLURPLE: u32 = 0x5865F2;
const GREY: u32 = 0x99AAB5;

/// Quoted message text in a card.
const MAX_QUOTE_LENGTH: usize = 900;
const MAX_CARD_DETAILS: usize = 8;
/// Components V2 allows 4000 characters of text per message; keep a margin for the footer.
const MAX_CARD_TEXT: usize = 3600;

/// Detail keys that the card already shows elsewhere (or that only matter in the JSONL file).
const HIDDEN_DETAIL_KEYS: &[&str] = &[
    "message_id",
    "author",
    "user",
    "content",
    "before",
    "after",
    "attachments",
    "reason",
    "actor_resolution",
    "source",
];

fn category_label(category: &str) -> String {
    category_meta(category).map_or_else(
        || {
            let mut chars = category.chars();
            chars.next().map_or_else(
                || "Event".to_owned(),
                |first| first.to_uppercase().collect::<String>() + chars.as_str(),
            )
        },
        |(name, _)| name.to_owned(),
    )
}

pub(super) fn card_title(category: &str, action: &str) -> String {
    let fixed = match (category, action) {
        ("message", "delete") => "Message Deleted",
        ("message", "bulk_delete") => "Messages Bulk Deleted",
        ("message", "edit" | "update") => "Message Edited",
        ("member", "join") => "Member Joined",
        ("member", "leave" | "remove") => "Member Left",
        ("member", "kick") => "Member Kicked",
        ("member", "ban") | ("moderation", "ban_add") => "Member Banned",
        ("moderation", "ban_remove") => "Member Unbanned",
        ("member", "roles_update") => "Roles Updated",
        ("member", "nickname_update") => "Nickname Changed",
        ("member", _) => "Member Updated",
        ("moderation", "timeout") => "Member Timed Out",
        ("moderation", "timeout_remove") => "Timeout Removed",
        ("voice", "join") => "Joined Voice Channel",
        ("voice", "leave") => "Left Voice Channel",
        ("voice", "move") => "Moved Voice Channel",
        ("voice", _) => "Voice State Updated",
        ("reaction", "add") => "Reaction Added",
        ("reaction", "remove") => "Reaction Removed",
        ("command", _) => "Command Used",
        ("automod", _) => "AutoMod Action",
        ("system", "test") => "Test Entry",
        ("system", "preview") => "Preview",
        _ => "",
    };
    if !fixed.is_empty() {
        return fixed.to_owned();
    }
    let label = category_label(category);
    match action {
        "create" => format!("{label} Created"),
        "delete" => format!("{label} Deleted"),
        "edit" => format!("{label} Edited"),
        a if a == "update" || a.ends_with("_update") => format!("{label} Updated"),
        a => format!("{label}: {}", a.replace('_', " ")),
    }
}

pub(super) fn card_color(category: &str, action: &str) -> u32 {
    match action {
        "delete" | "bulk_delete" | "leave" | "remove" | "kick" | "ban" | "ban_add" | "timeout" => {
            RED
        }
        "create" | "join" | "add" | "ban_remove" | "timeout_remove" => GREEN,
        "edit" | "update" | "move" => YELLOW,
        a if a.ends_with("_update") => YELLOW,
        _ if category == "voice" || category == "command" => BLURPLE,
        _ => GREY,
    }
}

/// `> ` before every line, so multi-line content stays inside the quote.
fn quote(text: &str) -> String {
    let text = truncate(text.trim(), MAX_QUOTE_LENGTH);
    if text.is_empty() {
        return "> *(empty)*".to_owned();
    }
    text.lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn humanize_key(key: &str) -> String {
    let words = key.replace('_', " ");
    let mut chars = words.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + chars.as_str()
    })
}

fn detail_lines(entry: &Value) -> Vec<String> {
    entry["details"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|line| {
            let line = inline(line, MAX_DETAIL_VALUE_LENGTH);
            match line.split_once('=') {
                Some((key, value))
                    if !key.is_empty()
                        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') =>
                {
                    let key = key.to_lowercase();
                    if HIDDEN_DETAIL_KEYS.contains(&key.as_str()) || value.trim().is_empty() {
                        return None;
                    }
                    Some(format!("**{}:** {}", humanize_key(&key), value.trim()))
                }
                _ => (!line.is_empty()).then_some(line),
            }
        })
        .take(MAX_CARD_DETAILS)
        .collect()
}

fn user_ref(id: Option<&str>, tag: Option<&str>) -> Option<String> {
    let tag = tag.map(|t| inline(t, 80)).filter(|t| !t.is_empty());
    match (id.filter(|id| is_snowflake(id, 15)), tag) {
        (Some(id), Some(tag)) => Some(format!("<@{id}> ({tag})")),
        (Some(id), None) => Some(format!("<@{id}>")),
        (None, Some(tag)) => Some(tag),
        (None, None) => None,
    }
}

/// The main subject of the event: `(label, value, id for the footer, user id)`.
fn subject(entry: &Value) -> Option<(&'static str, String, Option<String>, Option<String>)> {
    let target = &entry["target"];
    let id = str_field(target, "id").map(str::to_owned);
    let name = str_field(target, "name").map(|n| inline(n, 100));
    match str_field(target, "kind").unwrap_or_default() {
        "member" => {
            let value = user_ref(id.as_deref(), str_field(target, "tag"))?;
            Some(("Member", value, id.clone(), id))
        }
        "message" => {
            let owner = str_field(target, "ownerId")
                .or_else(|| str_field(&entry["metadata"], "ownerId"))
                .map(str::to_owned)
                .or_else(|| {
                    // Edits and reactions: the actor is the author.
                    (str_field(entry, "category") == Some("message"))
                        .then(|| str_field(&entry["actor"], "id").map(str::to_owned))
                        .flatten()
                });
            let tag = str_field(target, "ownerTag").or_else(|| {
                (owner.as_deref() == str_field(&entry["actor"], "id"))
                    .then(|| str_field(&entry["actor"], "tag"))
                    .flatten()
            });
            let value = user_ref(owner.as_deref(), tag)?;
            Some(("Author", value, owner.clone(), owner))
        }
        "role" => Some((
            "Role",
            match (&id, &name) {
                (Some(id), Some(name)) => format!("<@&{id}> ({name})"),
                (Some(id), None) => format!("<@&{id}>"),
                (None, Some(name)) => name.clone(),
                (None, None) => return None,
            },
            id,
            None,
        )),
        "channel" | "thread" => Some((
            if str_field(target, "kind") == Some("thread") {
                "Thread"
            } else {
                "Channel"
            },
            match (&id, &name) {
                (Some(id), Some(name)) => format!("<#{id}> (#{name})"),
                (Some(id), None) => format!("<#{id}>"),
                (None, Some(name)) => format!("#{name}"),
                (None, None) => return None,
            },
            id,
            None,
        )),
        "command" => Some(("Command", format!("/{}", name?), None, None)),
        "invite" => Some(("Invite", format!("discord.gg/{}", id.clone()?), None, None)),
        "emoji" => Some(("Emoji", name.or_else(|| id.clone())?, id, None)),
        "sticker" => Some(("Sticker", name.or_else(|| id.clone())?, id, None)),
        "guild" => Some((
            "Server",
            name.unwrap_or_else(|| "this server".to_owned()),
            id,
            None,
        )),
        _ => None,
    }
}

fn unix_time(entry: &Value) -> Option<i64> {
    str_field(entry, "ts")
        .and_then(operator_common::time::parse_iso)
        .map(operator_common::time::unix_seconds)
}

/// Builds the card for one stored log entry. `header` (title, note) is used by `/logs preview`.
pub(super) fn card_payload(entry: &Value, extra_flags: u64, header: Option<(&str, &str)>) -> Value {
    let category = str_field(entry, "category")
        .unwrap_or("system")
        .to_lowercase();
    let action = str_field(entry, "action").unwrap_or("event").to_lowercase();
    let icon = category_meta(&category).map_or("🧩", |(_, icon)| icon);
    let metadata = &entry["metadata"];
    let location = &entry["location"];

    let mut head = vec![format!("### {icon} {}", card_title(&category, &action))];
    let subject = subject(entry);
    if let Some((label, value, _, _)) = &subject {
        head.push(format!("**{label}:** {value}"));
    }
    let subject_user = subject.as_ref().and_then(|s| s.3.clone());
    let subject_channel = subject
        .as_ref()
        .filter(|s| matches!(s.0, "Channel" | "Thread"))
        .and_then(|s| s.2.clone());
    if let Some(channel) = str_field(location, "channelId")
        .or_else(|| str_field(metadata, "afterChannel"))
        .filter(|c| is_snowflake(c, 15) && Some(*c) != subject_channel.as_deref())
    {
        head.push(format!("**Channel:** <#{channel}>"));
    }
    if category == "voice"
        && action == "move"
        && let Some(from) = str_field(metadata, "beforeChannel").filter(|c| is_snowflake(c, 15))
    {
        head.push(format!("**From:** <#{from}>"));
    }
    let actor = &entry["actor"];
    let actor_id = str_field(actor, "id");
    if has_known_actor(actor) && actor_id != subject_user.as_deref() {
        let by = user_ref(actor_id, str_field(actor, "tag")).unwrap_or_default();
        let label = if action == "delete" && category == "message" {
            "Deleted by"
        } else {
            "By"
        };
        head.push(format!("**{label}:** {by}"));
    }

    let mut body = Vec::new();
    if category == "message" {
        match action.as_str() {
            "edit" | "update" if metadata["contentChanged"].as_bool() != Some(false) => {
                body.push(if metadata["uncached"].as_bool() == Some(true) {
                    "**Before**\n> *(not cached)*".to_owned()
                } else {
                    format!(
                        "**Before**\n{}",
                        quote(str_field(metadata, "before").unwrap_or_default())
                    )
                });
                body.push(format!(
                    "**After**\n{}",
                    quote(str_field(metadata, "after").unwrap_or_default())
                ));
            }
            "delete" if metadata["uncached"].as_bool() == Some(true) => {
                body.push(
                    "**Content**\n> *(not cached: sent before the bot started or too long ago)*"
                        .to_owned(),
                );
            }
            "delete" => {
                body.push(format!(
                    "**Content**\n{}",
                    quote(str_field(metadata, "content").unwrap_or_default())
                ));
            }
            _ => {}
        }
        let files: Vec<String> = metadata["attachments"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(serialize_attachment_line)
            .collect();
        if !files.is_empty() {
            body.push(format!("**Attachments:** {}", files.join(", ")));
        }
    }
    if let Some(reason) = str_field(metadata, "reason")
        .map(|r| inline(r, 300))
        .filter(|r| !r.is_empty())
    {
        body.push(format!("**Reason:** {reason}"));
    }
    body.extend(detail_lines(entry));
    if body.is_empty()
        && subject.is_none()
        && let Some(summary) = str_field(entry, "summary").filter(|s| !s.is_empty())
    {
        body.push(inline(summary, MAX_SUMMARY_LENGTH));
    }

    let mut footer = Vec::new();
    if let Some(id) = subject.as_ref().and_then(|s| s.2.clone()) {
        footer.push(format!("ID: {id}"));
    }
    if category == "message"
        && matches!(action.as_str(), "edit" | "update")
        && let (Some(guild), Some(channel), Some(message)) = (
            str_field(entry, "guildId"),
            str_field(location, "channelId").or_else(|| str_field(metadata, "channelId")),
            str_field(metadata, "messageId"),
        )
    {
        footer.push(format!(
            "[Jump to message](https://discord.com/channels/{guild}/{channel}/{message})"
        ));
    }
    if let Some(ts) = unix_time(entry) {
        footer.push(format!("<t:{ts}:f>"));
    }

    let mut components = Vec::new();
    if let Some((title, note)) = header {
        components.push(discord::text(format!("{title}\n{note}")));
        components.push(json!({ "type": 14, "divider": true }));
    }
    let head_text = truncate(&head.join("\n"), 1200);
    match str_field(actor, "avatar")
        .filter(|_| actor_id.is_some() && actor_id == subject_user.as_deref())
        .or_else(|| str_field(&entry["target"], "avatar"))
    {
        Some(avatar) => components.push(json!({
            "type": 9,
            "components": [discord::text(head_text)],
            "accessory": { "type": 11, "media": { "url": avatar } },
        })),
        None => components.push(discord::text(head_text)),
    }
    if !body.is_empty() {
        components.push(discord::text(truncate(&body.join("\n"), MAX_CARD_TEXT)));
    }
    if !footer.is_empty() {
        components.push(discord::text(format!("-# {}", footer.join(" · "))));
    }
    json!({
        "flags": FLAG_COMPONENTS_V2 | extra_flags,
        "allowed_mentions": { "parse": [] },
        "components": [{
            "type": 17,
            "accent_color": card_color(&category, &action),
            "components": components,
        }],
    })
}

/// CDN URL of a user's avatar (the default avatar when none is set).
pub(super) fn avatar_url(user: &User) -> String {
    match user.avatar {
        Some(hash) => format!(
            "https://cdn.discordapp.com/avatars/{}/{hash}.png?size=128",
            user.id
        ),
        None => format!(
            "https://cdn.discordapp.com/embed/avatars/{}.png",
            (user.id.get() >> 22) % 6
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(payload: &Value) -> String {
        fn walk(v: &Value, out: &mut Vec<String>) {
            if let Some(c) = v.get("content").and_then(Value::as_str) {
                out.push(c.to_owned());
            }
            for child in v
                .get("components")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                walk(child, out);
            }
        }
        let mut out = Vec::new();
        walk(&payload["components"][0], &mut out);
        out.join("\n")
    }

    #[test]
    fn message_edit_card_shows_before_after_and_jump_link() {
        let entry = json!({
            "ts": "2026-11-15T10:00:00.000Z", "guildId": "100000000000000001",
            "category": "message", "action": "edit",
            "actor": { "id": "200000000000000002", "tag": "alice", "avatar": "https://cdn/a.png" },
            "target": { "kind": "message", "id": "300000000000000003" },
            "location": { "channelId": "400000000000000004" },
            "details": ["message_id=300000000000000003", "before=hi", "after=hello"],
            "metadata": { "messageId": "300000000000000003", "channelId": "400000000000000004",
                          "before": "hi", "after": "hello\nworld", "contentChanged": true }
        });
        let card = card_payload(&entry, 0, None);
        let text = texts(&card);
        println!("{text}\n");
        assert_eq!(card["components"][0]["accent_color"], YELLOW);
        assert!(
            text.starts_with("### 💬 Message Edited\n**Author:** <@200000000000000002> (alice)")
        );
        assert!(text.contains("**Before**\n> hi\n**After**\n> hello\n> world"));
        assert!(
            text.contains("/channels/100000000000000001/400000000000000004/300000000000000003")
        );
        assert!(
            !text.contains("**By:**"),
            "the author edited it, no separate actor line"
        );
        assert_eq!(
            card["components"][0]["components"][0]["type"], 9,
            "avatar section"
        );
    }

    #[test]
    fn deleted_message_names_the_moderator_only_when_known() {
        let mut entry = json!({
            "ts": "2026-11-15T10:00:00.000Z", "guildId": "100000000000000001",
            "category": "message", "action": "delete",
            "actor": null,
            "target": { "kind": "message", "id": "300000000000000003",
                        "ownerId": "200000000000000002", "ownerTag": "alice" },
            "location": { "channelId": "400000000000000004" },
            "details": ["message_id=1", "author=alice", "content=spam", "attachments=[none]",
                        "actor_resolution=not_found"],
            "metadata": { "content": "spam", "attachments": [] }
        });
        let text = texts(&card_payload(&entry, 0, None));
        println!("{text}\n");
        assert!(!text.contains("Deleted by") && !text.contains("unknown"));
        assert!(text.contains("**Content**\n> spam"));
        assert!(!text.contains("Actor resolution"));
        entry["actor"] = json!({ "id": "500000000000000005", "tag": "mod" });
        let text = texts(&card_payload(&entry, 0, None));
        assert!(text.contains("**Deleted by:** <@500000000000000005> (mod)"));
    }

    #[test]
    fn member_and_role_cards() {
        let join = json!({
            "ts": "2026-11-15T10:00:00.000Z", "category": "member", "action": "join",
            "actor": null,
            "target": { "kind": "member", "id": "200000000000000002", "tag": "bob" },
            "details": ["account_created=<t:1700000000:F>", "member_count=42"],
            "metadata": {}
        });
        let card = card_payload(&join, 0, None);
        let text = texts(&card);
        println!("{text}\n");
        assert_eq!(card["components"][0]["accent_color"], GREEN);
        assert!(text.contains("**Account created:** <t:1700000000:F>\n**Member count:** 42"));
        assert!(text.contains("-# ID: 200000000000000002 · <t:"));

        let role = json!({
            "category": "role", "action": "update",
            "actor": { "id": "500000000000000005", "tag": "admin" },
            "target": { "kind": "role", "id": "600000000000000006", "name": "VIP" },
            "details": ["name=Old -> VIP", "color=#000000 -> #ff0000"],
            "metadata": {}
        });
        let text = texts(&card_payload(&role, 0, None));
        println!("{text}");
        assert!(text.starts_with("### 🎭 Role Updated\n**Role:** <@&600000000000000006> (VIP)\n**By:** <@500000000000000005> (admin)"));
        assert!(text.contains("**Name:** Old -> VIP"));
    }

    #[test]
    fn titles_and_colors() {
        assert_eq!(card_title("moderation", "ban_add"), "Member Banned");
        assert_eq!(card_title("sticker", "create"), "Sticker Created");
        assert_eq!(card_title("webhook", "update"), "Webhook Updated");
        assert_eq!(card_color("channel", "delete"), RED);
        assert_eq!(card_color("voice", "state_update"), YELLOW);
        assert_eq!(card_color("command", "slash_execute"), BLURPLE);
    }
}
