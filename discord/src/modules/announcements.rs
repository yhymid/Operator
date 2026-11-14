//! Scheduled webhook announcements (port of `src/core/announcements.js`).
//!
//! Items, profiles and webhook records are kept as JSON objects in the guild state file, with the
//! same field names as the JS version, so existing state keeps working after the switch.

use std::collections::BTreeSet;

use anyhow::{Result, anyhow, bail};
use chrono::{NaiveDate, TimeZone, Utc};
use rand::RngExt;
use serde_json::{Map, Value, json};
use twilight_http::request::AuditLogReason;
use twilight_model::id::Id;
use twilight_model::id::marker::{ChannelMarker, GuildMarker, MessageMarker, WebhookMarker};

use crate::app::App;
use crate::discord;
use operator_common::time::{now_iso, parse_iso};

pub const DEFAULT_WEBHOOK_NAME: &str = "Operator Announcements";
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const MESSAGE_CONTENT_LIMIT: usize = 2000;
const EMBED_DESCRIPTION_LIMIT: usize = 4096;
const SPLIT_HINTS: [&str; 8] = ["\n\n", "\n", ". ", "! ", "? ", "; ", ", ", " "];

/// `undefined` / `null` / value semantics of `normalizeOptionalText`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opt {
    /// Option not given (`undefined`).
    Unset,
    /// `-` or `clear` (`null`).
    Clear,
    Set(String),
}

impl Opt {
    pub fn is_unset(&self) -> bool {
        matches!(self, Self::Unset)
    }

    pub fn is_set(&self) -> bool {
        matches!(self, Self::Set(_))
    }

    /// `value || null`
    pub fn to_json(&self) -> Value {
        match self {
            Self::Set(v) if !v.is_empty() => Value::String(v.clone()),
            _ => Value::Null,
        }
    }
}

pub fn generate_id() -> String {
    let millis = Utc::now().timestamp_millis().max(0) as u64;
    let mut rng = rand::rng();
    let suffix: String = (0..6)
        .map(|_| char::from_digit(rng.random_range(0..36u32), 36).unwrap_or('0'))
        .collect();
    format!("ann_{}_{suffix}", to_base36(millis))
}

fn to_base36(mut value: u64) -> String {
    if value == 0 {
        return "0".to_owned();
    }
    let mut digits = Vec::new();
    while value > 0 {
        digits.push(char::from_digit((value % 36) as u32, 36).unwrap_or('0'));
        value /= 36;
    }
    digits.iter().rev().collect()
}

pub fn generate_code() -> String {
    let mut rng = rand::rng();
    (0..4)
        .map(|_| CODE_ALPHABET[rng.random_range(0..CODE_ALPHABET.len())] as char)
        .collect()
}

/// `YYYY-MM-DD HH:mm` in Polish time (Europe/Warsaw, DST-aware) → UTC ISO string.
pub fn parse_datetime(input: &str) -> Result<String> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"^(\d{4})-(\d{2})-(\d{2})[ T](\d{2}):(\d{2})$").expect("static regex")
    });
    let raw = input.trim();
    let caps = re
        .captures(raw)
        .ok_or_else(|| anyhow!("Use the date format `YYYY-MM-DD HH:mm` (Polish time)."))?;
    let num = |i: usize| caps[i].parse::<u32>().unwrap_or(u32::MAX);
    let date = NaiveDate::from_ymd_opt(num(1) as i32, num(2), num(3))
        .and_then(|d| d.and_hms_opt(num(4), num(5), 0))
        .ok_or_else(|| anyhow!("Invalid date or time."))?;
    // An hour skipped by the spring DST change does not exist; an ambiguous autumn hour takes
    // the first occurrence.
    let local = chrono_tz::Europe::Warsaw
        .from_local_datetime(&date)
        .earliest()
        .ok_or_else(|| anyhow!("This time does not exist in Polish time (DST change)."))?;
    Ok(operator_common::time::to_iso(local.with_timezone(&Utc)))
}

pub fn normalize_optional_text(value: Option<String>) -> Opt {
    let Some(value) = value else {
        return Opt::Unset;
    };
    let raw = value.trim();
    if raw.is_empty() {
        Opt::Unset
    } else if raw == "-" || raw.eq_ignore_ascii_case("clear") {
        Opt::Clear
    } else {
        Opt::Set(raw.to_owned())
    }
}

pub fn normalize_color(value: Option<String>) -> Result<Opt> {
    match normalize_optional_text(value) {
        Opt::Set(raw) => {
            let raw = raw.strip_prefix('#').unwrap_or(&raw);
            if raw.len() != 6 || !raw.chars().all(|c| c.is_ascii_hexdigit()) {
                bail!("The color must be `#RRGGBB` or `RRGGBB`.");
            }
            Ok(Opt::Set(format!("#{}", raw.to_uppercase())))
        }
        other => Ok(other),
    }
}

pub fn normalize_profile_name(value: Option<String>) -> Result<Opt> {
    match normalize_optional_text(value) {
        Opt::Set(raw) => {
            let mut out = String::new();
            let mut last_dash = false;
            for c in raw.to_lowercase().chars() {
                if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' {
                    if c == '-' {
                        if last_dash {
                            continue;
                        }
                        last_dash = true;
                    } else {
                        last_dash = false;
                    }
                    out.push(c);
                } else if !last_dash {
                    out.push('-');
                    last_dash = true;
                }
            }
            let out = out.trim_matches(|c| c == '-' || c == '_').to_owned();
            let len = out.chars().count();
            if !(2..=32).contains(&len) {
                bail!(
                    "After normalization the profile name must be 2-32 characters: letters, digits, `-` or `_`."
                );
            }
            Ok(Opt::Set(out))
        }
        other => Ok(other),
    }
}

pub fn normalize_mode(value: Option<String>) -> Result<Opt> {
    match normalize_optional_text(value) {
        Opt::Set(raw) => {
            let raw = raw.to_lowercase();
            if !["message", "embed", "components"].contains(&raw.as_str()) {
                bail!("The send mode must be one of: `message`, `embed`, `components`.");
            }
            Ok(Opt::Set(raw))
        }
        other => Ok(other),
    }
}

pub fn normalize_url(value: Option<String>) -> Result<Opt> {
    match normalize_optional_text(value) {
        Opt::Set(raw) => {
            let url = url::Url::parse(&raw)
                .map_err(|_| anyhow!("The button URL must be a valid http or https address."))?;
            if url.scheme() != "http" && url.scheme() != "https" {
                bail!("The button URL must start with `http://` or `https://`.");
            }
            Ok(Opt::Set(url.to_string()))
        }
        other => Ok(other),
    }
}

fn value_text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

pub fn stored_message_ids(item: &Value) -> Vec<String> {
    let mut ids: Vec<String> = item["messageIds"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(value_text)
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    if let Some(first) = value_text(&item["messageId"]).filter(|s| !s.trim().is_empty()) {
        ids.insert(0, first.trim().to_owned());
    }
    let mut seen = BTreeSet::new();
    ids.retain(|id| seen.insert(id.clone()));
    ids
}

/// `ensureAnnouncementStateShape`
#[derive(Debug, Clone, Default)]
pub struct AnnouncementState {
    pub webhooks: Map<String, Value>,
    pub profiles: Map<String, Value>,
    pub items: Vec<Value>,
}

pub fn ensure_shape(state: &Value) -> AnnouncementState {
    let mut items: Vec<Value> = state["items"].as_array().cloned().unwrap_or_default();
    for item in &mut items {
        let ids = stored_message_ids(item);
        let Some(obj) = item.as_object_mut() else {
            continue;
        };
        let has_message_id = obj.get("messageId").is_some_and(|v| !is_falsy(v));
        if !has_message_id && let Some(first) = ids.first() {
            obj.insert("messageId".into(), Value::String(first.clone()));
        }
        let has_delivery = obj.get("deliveryChannelId").is_some_and(|v| !is_falsy(v));
        if !has_delivery && !ids.is_empty() {
            let channel = obj
                .get("channelId")
                .filter(|v| !is_falsy(v))
                .cloned()
                .unwrap_or(Value::Null);
            obj.insert("deliveryChannelId".into(), channel);
        }
        obj.insert("messageIds".into(), json!(ids));
    }
    AnnouncementState {
        webhooks: state["webhooks"].as_object().cloned().unwrap_or_default(),
        profiles: state["profiles"].as_object().cloned().unwrap_or_default(),
        items,
    }
}

pub fn is_falsy(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::String(s) => s.is_empty(),
        Value::Number(n) => n.as_f64() == Some(0.0),
        _ => false,
    }
}

fn s<'a>(item: &'a Value, key: &str) -> &'a str {
    item[key].as_str().unwrap_or_default()
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Byte index for a UTF-16 offset (clamped to char boundaries).
fn byte_at_utf16(text: &str, units: usize) -> usize {
    let mut count = 0;
    for (index, c) in text.char_indices() {
        if count >= units {
            return index;
        }
        count += c.len_utf16();
    }
    text.len()
}

fn pick_split_index(text: &str, max: usize) -> usize {
    if utf16_len(text) <= max {
        return text.len();
    }
    let min_units = max * 55 / 100;
    let window_end = byte_at_utf16(text, max);
    let window = &text[..window_end];
    for marker in SPLIT_HINTS {
        if let Some(index) = window.rfind(marker)
            && utf16_len(&text[..index]) >= min_units
        {
            return if marker.trim().is_empty() {
                index + 1
            } else {
                index + marker.len()
            };
        }
    }
    window_end
}

pub fn split_text(raw: &str, max: usize) -> Vec<String> {
    let text = raw.trim();
    if text.is_empty() {
        return vec![String::new()];
    }
    let max = max.max(1);
    if utf16_len(text) <= max {
        return vec![text.to_owned()];
    }
    let mut chunks = Vec::new();
    let mut rest = text.to_owned();
    while utf16_len(&rest) > max {
        let split = pick_split_index(&rest, max).max(rest.chars().next().map_or(1, char::len_utf8));
        let head = rest[..split].trim_end();
        chunks.push(if head.is_empty() {
            rest[..byte_at_utf16(&rest, max)].to_owned()
        } else {
            head.to_owned()
        });
        rest = rest[split..].trim_start().to_owned();
    }
    if !rest.is_empty() {
        chunks.push(rest);
    }
    chunks
}

pub fn effective_send_mode(item: &Value) -> String {
    let mode = s(item, "sendMode");
    if !mode.is_empty() {
        return mode.to_owned();
    }
    if !s(item, "title").trim().is_empty() || !s(item, "color").trim().is_empty() {
        "embed".to_owned()
    } else {
        "message".to_owned()
    }
}

/// `buildAnnouncementPayloadParts` → raw webhook execute bodies.
pub fn payload_parts(item: &Value) -> Result<Vec<Value>> {
    let content = s(item, "content").trim();
    let title = s(item, "title").trim();
    let color = s(item, "color").trim();
    let send_mode = effective_send_mode(item);
    let embed_needed = send_mode == "embed"
        || (send_mode == "components" && (!title.is_empty() || !color.is_empty()));
    let limit = if embed_needed {
        EMBED_DESCRIPTION_LIMIT
    } else {
        MESSAGE_CONTENT_LIMIT
    };
    let button_label = s(item, "buttonLabel").trim();
    let button_url = s(item, "buttonURL").trim();
    if send_mode == "components" && (button_label.is_empty() || button_url.is_empty()) {
        bail!("The `components` mode needs `button_label` and `button_url`.");
    }
    let username = Some(s(item, "username"))
        .filter(|u| !u.is_empty())
        .unwrap_or(DEFAULT_WEBHOOK_NAME);

    Ok(split_text(content, limit)
        .into_iter()
        .map(|chunk| {
            let mut payload = Map::new();
            payload.insert(
                "content".into(),
                Value::String(if embed_needed {
                    String::new()
                } else {
                    chunk.clone()
                }),
            );
            payload.insert("username".into(), Value::String(username.to_owned()));
            let avatar = s(item, "avatarURL");
            if !avatar.is_empty() {
                payload.insert("avatar_url".into(), Value::String(avatar.to_owned()));
            }
            if embed_needed {
                let mut embed = Map::new();
                if !title.is_empty() {
                    embed.insert("title".into(), Value::String(title.to_owned()));
                }
                if !chunk.is_empty() {
                    embed.insert("description".into(), Value::String(chunk.clone()));
                }
                if !color.is_empty()
                    && let Ok(parsed) = u32::from_str_radix(color.trim_start_matches('#'), 16)
                {
                    embed.insert("color".into(), json!(parsed));
                }
                payload.insert("embeds".into(), json!([embed]));
            }
            if send_mode == "components" {
                payload.insert(
                    "components".into(),
                    json!([{ "type": 1, "components": [
                        { "type": 2, "style": 5, "label": button_label, "url": button_url }
                    ]}]),
                );
            }
            Value::Object(payload)
        })
        .collect())
}

pub fn reference(item: &Value) -> String {
    let ids = stored_message_ids(item);
    if s(item, "status") == "sent" && !ids.is_empty() {
        return ids[0].clone();
    }
    for key in ["code", "id"] {
        if let Some(text) = value_text(&item[key]).filter(|t| !t.is_empty()) {
            return text;
        }
    }
    "unknown".to_owned()
}

pub fn matches_reference(item: &Value, raw: &str) -> bool {
    let normalized = raw.trim().to_uppercase();
    let code = value_text(&item["code"])
        .filter(|c| !c.is_empty())
        .or_else(|| value_text(&item["id"]))
        .unwrap_or_default()
        .trim()
        .to_uppercase();
    normalized == code
        || stored_message_ids(item)
            .iter()
            .any(|id| id.to_uppercase() == normalized)
}

// ---------------------------------------------------------------------------------------------
// Webhooks
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct WebhookRef {
    pub id: Id<WebhookMarker>,
    pub token: String,
    pub channel_id: Option<Id<ChannelMarker>>,
}

fn clone_record(
    record: &Value,
    channel_id: Option<&str>,
) -> Option<(String, String, Option<String>)> {
    let id = value_text(&record["id"]).filter(|v| !v.is_empty())?;
    let token = value_text(&record["token"]).filter(|v| !v.is_empty())?;
    Some((id, token, channel_id.map(str::to_owned)))
}

fn find_stored_record(
    webhooks: &Map<String, Value>,
    channel_id: Option<&str>,
    webhook_id: Option<&str>,
) -> Option<(String, String, Option<String>)> {
    if let Some(channel) = channel_id
        && let Some(direct) = webhooks
            .get(channel)
            .and_then(|r| clone_record(r, Some(channel)))
        && webhook_id.is_none_or(|w| w == direct.0)
    {
        return Some(direct);
    }
    if let Some(webhook) = webhook_id {
        for (channel, record) in webhooks {
            if let Some(cloned) = clone_record(record, Some(channel))
                && cloned.0 == webhook
            {
                return Some(cloned);
            }
        }
    }
    None
}

async fn fetch_with_token(
    app: &App,
    webhook_id: &str,
    token: &str,
    context: &str,
) -> Option<WebhookRef> {
    let id = discord::parse_id::<WebhookMarker>(webhook_id)?;
    let result = async {
        let webhook = app.http.webhook(id).token(token).await?.model().await?;
        anyhow::Ok(webhook)
    }
    .await;
    match result {
        Ok(webhook) => Some(WebhookRef {
            id: webhook.id,
            token: token.to_owned(),
            channel_id: webhook.channel_id,
        }),
        Err(error) => {
            tracing::warn!(
                "Announcement webhook fetch failed ({context}): {}",
                discord::err_text(&error)
            );
            None
        }
    }
}

struct Resolved {
    webhook: Option<WebhookRef>,
    state: AnnouncementState,
}

async fn resolve_webhook(
    app: &App,
    guild_id: &str,
    channel_id: Option<&str>,
    webhook_id: Option<&str>,
    legacy_token: Option<&str>,
) -> Resolved {
    let mut current = ensure_shape(&app.state.announcements(guild_id));
    let stored = find_stored_record(&current.webhooks, channel_id, webhook_id);
    let context = format!(
        "{guild_id}/{}/{}",
        channel_id.unwrap_or("unknown-channel"),
        webhook_id.unwrap_or("unknown-webhook")
    );

    if let Some((id, token, stored_channel)) = stored
        && let Some(mut webhook) = fetch_with_token(app, &id, &token, &context).await
    {
        if let Some(channel) = stored_channel
            .as_deref()
            .or(channel_id)
            .and_then(discord::parse_id)
        {
            webhook.channel_id = Some(channel);
        }
        return Resolved {
            webhook: Some(webhook),
            state: current,
        };
    }

    if let (Some(webhook_id), Some(token)) = (webhook_id, legacy_token)
        && let Some(mut webhook) =
            fetch_with_token(app, webhook_id, token, &format!("{context}/legacy")).await
    {
        let resolved_channel = channel_id
            .and_then(discord::parse_id)
            .or(webhook.channel_id);
        if let Some(channel) = resolved_channel {
            current.webhooks.insert(
                channel.to_string(),
                json!({ "id": webhook.id.to_string(), "token": token }),
            );
            app.state
                .save_announcements(guild_id, json!({ "webhooks": current.webhooks }));
        }
        webhook.channel_id = resolved_channel;
        return Resolved {
            webhook: Some(webhook),
            state: current,
        };
    }

    Resolved {
        webhook: None,
        state: current,
    }
}

async fn get_or_create_webhook(
    app: &App,
    guild_id: &str,
    channel_id: Id<ChannelMarker>,
) -> Result<WebhookRef> {
    let channel = channel_id.to_string();
    let resolved = resolve_webhook(app, guild_id, Some(&channel), None, None).await;
    if let Some(webhook) = resolved.webhook {
        return Ok(webhook);
    }
    let created = app
        .http
        .create_webhook(channel_id, DEFAULT_WEBHOOK_NAME)
        .reason("Operator scheduled announcements")
        .await?
        .model()
        .await?;
    let token = created
        .token
        .clone()
        .ok_or_else(|| anyhow!("Discord did not return a webhook token."))?;
    let mut webhooks = resolved.state.webhooks;
    webhooks.insert(
        channel.clone(),
        json!({ "id": created.id.to_string(), "token": token }),
    );
    app.state
        .save_announcements(guild_id, json!({ "webhooks": webhooks }));
    Ok(WebhookRef {
        id: created.id,
        token,
        channel_id: Some(channel_id),
    })
}

async fn execute(app: &App, webhook: &WebhookRef, payload: &Value) -> Result<String> {
    let body = serde_json::to_vec(payload)?;
    let message = app
        .http
        .execute_webhook(webhook.id, &webhook.token)
        .payload_json(&body)
        .wait()
        .await?
        .model()
        .await?;
    Ok(message.id.to_string())
}

async fn edit(
    app: &App,
    webhook: &WebhookRef,
    message_id: &str,
    payload: &Value,
) -> Result<String> {
    let id = discord::parse_id::<MessageMarker>(message_id)
        .ok_or_else(|| anyhow!("Invalid messageId: {message_id}"))?;
    let mut payload = payload.clone();
    if let Some(obj) = payload.as_object_mut() {
        // Webhook message edits cannot change the author.
        obj.remove("username");
        obj.remove("avatar_url");
    }
    let body = serde_json::to_vec(&payload)?;
    let message = app
        .http
        .update_webhook_message(webhook.id, &webhook.token, id)
        .payload_json(&body)
        .await?
        .model()
        .await?;
    Ok(message.id.to_string())
}

async fn delete_parts(app: &App, webhook: &WebhookRef, ids: &[String]) -> Result<()> {
    for raw in ids {
        let id = discord::parse_id::<MessageMarker>(raw)
            .ok_or_else(|| anyhow!("Invalid messageId: {raw}"))?;
        app.http
            .delete_webhook_message(webhook.id, &webhook.token, id)
            .await?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Delivery {
    pub mode: &'static str,
    pub webhook_id: String,
    pub delivery_channel_id: Option<String>,
    pub message_id: Option<String>,
    pub message_ids: Vec<String>,
}

pub async fn send_now(app: &App, guild_id: Id<GuildMarker>, item: &Value) -> Result<Delivery> {
    let channel = discord::parse_id::<ChannelMarker>(s(item, "channelId"))
        .filter(|id| {
            app.cache
                .channel(*id)
                .is_some_and(|c| c.guild_id == Some(guild_id) && discord::is_text_based(c.kind))
        })
        .ok_or_else(|| {
            anyhow!("The target channel does not exist or does not support messages.")
        })?;
    let webhook = get_or_create_webhook(app, &guild_id.to_string(), channel).await?;
    let mut ids = Vec::new();
    for payload in payload_parts(item)? {
        match execute(app, &webhook, &payload).await {
            Ok(id) => ids.push(id),
            Err(error) => {
                // The sweep retries failed sends: remove the parts already posted so a retry
                // does not leave a half-duplicated announcement.
                if !ids.is_empty()
                    && let Err(cleanup) = delete_parts(app, &webhook, &ids).await
                {
                    tracing::warn!("Announcement partial cleanup failed: {cleanup:#}");
                }
                return Err(error);
            }
        }
    }
    Ok(Delivery {
        mode: "sent",
        webhook_id: webhook.id.to_string(),
        delivery_channel_id: Some(channel.to_string()),
        message_id: ids.first().cloned(),
        message_ids: ids,
    })
}

pub async fn edit_message(
    app: &App,
    guild_id: Id<GuildMarker>,
    item: &Value,
    previous: &Value,
) -> Result<Delivery> {
    let previous_ids = stored_message_ids(previous);
    let webhook_id = value_text(&previous["webhookId"]).filter(|v| !v.is_empty());
    let Some(webhook_id) = webhook_id.filter(|_| !previous_ids.is_empty()) else {
        bail!("Missing webhook data or messageId to edit.");
    };
    let delivery_channel = value_text(&previous["deliveryChannelId"])
        .filter(|v| !v.is_empty())
        .or_else(|| value_text(&previous["channelId"]).filter(|v| !v.is_empty()));
    let guild = guild_id.to_string();
    let resolved = resolve_webhook(
        app,
        &guild,
        delivery_channel.as_deref(),
        Some(&webhook_id),
        previous["webhookToken"].as_str(),
    )
    .await;
    let webhook = resolved
        .webhook
        .ok_or_else(|| anyhow!("Could not recover the webhook of an already sent announcement."))?;

    let current_channel = value_text(&item["channelId"]).unwrap_or_default();
    let previous_channel = delivery_channel.clone().unwrap_or_default();
    if current_channel == previous_channel {
        let payloads = payload_parts(item)?;
        let shared = previous_ids.len().min(payloads.len());
        let mut ids = Vec::new();
        for index in 0..shared {
            ids.push(edit(app, &webhook, &previous_ids[index], &payloads[index]).await?);
        }
        for payload in &payloads[shared..] {
            ids.push(execute(app, &webhook, payload).await?);
        }
        if payloads.len() < previous_ids.len() {
            delete_parts(app, &webhook, &previous_ids[payloads.len()..]).await?;
        }
        return Ok(Delivery {
            mode: "edited_in_place",
            webhook_id: webhook.id.to_string(),
            delivery_channel_id: Some(previous_channel).filter(|c| !c.is_empty()),
            message_id: ids.first().cloned(),
            message_ids: ids,
        });
    }

    let sent = send_now(app, guild_id, item).await?;
    if let Err(error) = delete_parts(app, &webhook, &previous_ids).await {
        let rollback = json!({
            "messageId": sent.message_id,
            "messageIds": sent.message_ids,
            "webhookId": sent.webhook_id,
            "deliveryChannelId": sent.delivery_channel_id,
        });
        let _ = delete_message(app, &guild, &rollback).await;
        bail!(
            "Switched the channel, but could not delete the previous post: {}",
            discord::err_text(&error)
        );
    }
    Ok(Delivery {
        mode: "resent",
        ..sent
    })
}

pub async fn delete_message(app: &App, guild_id: &str, item: &Value) -> Result<()> {
    let ids = stored_message_ids(item);
    let Some(webhook_id) = value_text(&item["webhookId"]).filter(|v| !v.is_empty()) else {
        return Ok(());
    };
    if ids.is_empty() {
        return Ok(());
    }
    let channel = value_text(&item["deliveryChannelId"])
        .filter(|v| !v.is_empty())
        .or_else(|| value_text(&item["channelId"]).filter(|v| !v.is_empty()));
    let resolved = resolve_webhook(
        app,
        guild_id,
        channel.as_deref(),
        Some(&webhook_id),
        item["webhookToken"].as_str(),
    )
    .await;
    let webhook = resolved
        .webhook
        .ok_or_else(|| anyhow!("Could not recover the webhook to delete the sent announcement."))?;
    delete_parts(app, &webhook, &ids).await
}

/// `applyAnnouncementDelivery`-like update used by the sweep.
pub fn apply_delivery(item: &mut Value, sent: &Delivery, timestamp: &str, preserve_sent_at: bool) {
    let Some(obj) = item.as_object_mut() else {
        return;
    };
    obj.insert("status".into(), json!("sent"));
    if !preserve_sent_at || obj.get("sentAt").is_none_or(is_falsy) {
        obj.insert("sentAt".into(), json!(timestamp));
    }
    obj.insert("updatedAt".into(), json!(timestamp));
    let webhook = Some(sent.webhook_id.clone())
        .filter(|w| !w.is_empty())
        .map(Value::String)
        .or_else(|| obj.get("webhookId").filter(|v| !is_falsy(v)).cloned())
        .unwrap_or(Value::Null);
    obj.insert("webhookId".into(), webhook);
    let delivery = sent
        .delivery_channel_id
        .clone()
        .map(Value::String)
        .or_else(|| obj.get("channelId").filter(|v| !is_falsy(v)).cloned())
        .or_else(|| {
            obj.get("deliveryChannelId")
                .filter(|v| !is_falsy(v))
                .cloned()
        })
        .unwrap_or(Value::Null);
    obj.insert("deliveryChannelId".into(), delivery);
    obj.insert("messageId".into(), json!(sent.message_id));
    let ids = if sent.message_ids.is_empty() {
        sent.message_id.iter().cloned().collect()
    } else {
        sent.message_ids.clone()
    };
    obj.insert("messageIds".into(), json!(ids));
    obj.insert("lastError".into(), Value::Null);
}

// Mirrors the JS diagnostics/snapshot shape; not every field is rendered.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct SweepStats {
    pub joined: bool,
    pub guilds: usize,
    pub changed_guilds: usize,
    pub sent: usize,
    pub failed: usize,
    pub last_sweep_at: String,
}

pub struct Announcements {
    sweep_lock: tokio::sync::Mutex<SweepStats>,
    /// Held by `/announce` and by the sweep for a guild while they read, send and save the
    /// announcement state: without it a sweep saving its stale copy dropped `/announce` edits,
    /// and `send_now` racing the sweep could post the same announcement twice.
    pub edit_lock: tokio::sync::Mutex<()>,
}

impl Default for Announcements {
    fn default() -> Self {
        Self::new()
    }
}

impl Announcements {
    pub fn new() -> Self {
        Self {
            sweep_lock: tokio::sync::Mutex::new(SweepStats::default()),
            edit_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// `runAnnouncementSweep`: concurrent callers join the sweep already in flight.
    pub async fn run_sweep(&self, app: &App) -> SweepStats {
        if let Ok(mut guard) = self.sweep_lock.try_lock() {
            let stats = perform_sweep(app).await;
            *guard = stats.clone();
            return stats;
        }
        let guard = self.sweep_lock.lock().await;
        SweepStats {
            joined: true,
            ..guard.clone()
        }
    }
}

/// Sweeps that may fail to send an announcement before it is marked `failed`.
const MAX_SEND_ATTEMPTS: u64 = 3;

async fn perform_sweep(app: &App) -> SweepStats {
    let now = Utc::now();
    let sweep_time = operator_common::time::to_iso(now);
    let mut stats = SweepStats {
        last_sweep_at: sweep_time.clone(),
        ..SweepStats::default()
    };
    for guild_id in app.guild_ids() {
        stats.guilds += 1;
        let guild = guild_id.to_string();
        let _edit = app.announcements.edit_lock.lock().await;
        let mut state = ensure_shape(&app.state.announcements(&guild));
        let mut mutated = false;
        for item in &mut state.items {
            if s(item, "status") != "scheduled" {
                continue;
            }
            let Some(run_at) = parse_iso(s(item, "runAt")) else {
                continue;
            };
            if run_at > now {
                continue;
            }
            match send_now(app, guild_id, item).await {
                Ok(sent) => {
                    if let Some(obj) = item.as_object_mut() {
                        obj.insert("status".into(), json!("sent"));
                        obj.insert("sentAt".into(), json!(now_iso()));
                        obj.insert("webhookId".into(), json!(sent.webhook_id));
                        let delivery = sent
                            .delivery_channel_id
                            .clone()
                            .map(Value::String)
                            .or_else(|| obj.get("channelId").cloned())
                            .unwrap_or(Value::Null);
                        obj.insert("deliveryChannelId".into(), delivery);
                        obj.insert("messageId".into(), json!(sent.message_id));
                        obj.insert("messageIds".into(), json!(sent.message_ids));
                        obj.insert("lastError".into(), Value::Null);
                    }
                    mutated = true;
                    stats.sent += 1;
                }
                Err(error) => {
                    let text = discord::err_text(&error);
                    // A transient error (Discord 5xx, network) gets retried on the next sweeps.
                    let attempts = item["sendAttempts"].as_u64().unwrap_or(0) + 1;
                    let give_up = attempts >= MAX_SEND_ATTEMPTS;
                    if let Some(obj) = item.as_object_mut() {
                        if give_up {
                            obj.insert("status".into(), json!("failed"));
                        }
                        obj.insert("sendAttempts".into(), json!(attempts));
                        obj.insert("lastError".into(), json!(text));
                    }
                    mutated = true;
                    if give_up {
                        stats.failed += 1;
                    }
                    tracing::error!(
                        "Announcement send error ({guild}/{}): {text}",
                        s(item, "id")
                    );
                }
            }
        }
        if mutated {
            app.state.save_announcements(
                &guild,
                json!({ "items": state.items, "lastSweepAt": sweep_time }),
            );
            stats.changed_guilds += 1;
        }
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizers() {
        assert_eq!(normalize_optional_text(None), Opt::Unset);
        assert_eq!(normalize_optional_text(Some("  ".into())), Opt::Unset);
        assert_eq!(normalize_optional_text(Some("CLEAR".into())), Opt::Clear);
        assert_eq!(
            normalize_color(Some("2f9e44".into())).unwrap(),
            Opt::Set("#2F9E44".into())
        );
        assert!(normalize_color(Some("#12345".into())).is_err());
        assert_eq!(
            normalize_profile_name(Some("  Moje Ogłoszenia!! ".into())).unwrap(),
            Opt::Set("moje-og-oszenia".into())
        );
        assert!(normalize_profile_name(Some("!".into())).is_err());
        assert_eq!(
            parse_datetime("2026-04-20 18:30").unwrap(),
            "2026-04-20T16:30:00.000Z"
        );
        assert_eq!(
            parse_datetime("2026-01-10 18:30").unwrap(),
            "2026-01-10T17:30:00.000Z"
        );
        assert!(
            parse_datetime("2026-03-29 02:30").is_err(),
            "skipped by DST"
        );
        assert_eq!(
            parse_datetime("2026-10-25 02:30").unwrap(),
            "2026-10-25T00:30:00.000Z"
        );
        assert!(parse_datetime("2026-02-30 10:00").is_err());
        assert!(normalize_url(Some("ftp://x".into())).is_err());
    }

    #[test]
    fn splitting_prefers_paragraphs() {
        let text = format!("{}\n\n{}", "a".repeat(1500), "b".repeat(1000));
        let parts = split_text(&text, 2000);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], "a".repeat(1500));
        assert_eq!(parts[1], "b".repeat(1000));
        let long = "x".repeat(4500);
        assert_eq!(split_text(&long, 2000).len(), 3);
    }

    #[test]
    fn payload_shape() {
        let item = json!({ "content": "hej", "title": "T", "color": "#00FF00" });
        let parts = payload_parts(&item).unwrap();
        assert_eq!(parts[0]["content"], "");
        assert_eq!(parts[0]["username"], DEFAULT_WEBHOOK_NAME);
        assert_eq!(parts[0]["embeds"][0]["color"], 0x00ff00);
        let bad = json!({ "content": "x", "sendMode": "components" });
        assert!(payload_parts(&bad).is_err());
    }

    #[test]
    fn references() {
        let item = json!({ "code": "AB12", "status": "scheduled", "messageIds": [] });
        assert!(matches_reference(&item, "ab12"));
        assert_eq!(reference(&item), "AB12");
        let sent = json!({ "code": "AB12", "status": "sent", "messageId": "99", "messageIds": ["99", "100"] });
        assert_eq!(reference(&sent), "99");
        assert!(matches_reference(&sent, "100"));
        assert_eq!(to_base36(35), "z");
        assert!(generate_id().starts_with("ann_"));
    }
}
