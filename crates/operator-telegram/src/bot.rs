//! Update context and routing (the Telegraf parts of `bot.js` we rely on).

use std::sync::{Arc, OnceLock};

use regex::Regex;
use serde_json::{Map, Value, json};

use crate::api::{Api, ApiError};
use crate::log;
use crate::queue::Queue;
use crate::settings::Settings;
use crate::ytdlp;

/// The subset of Telegraf's `ctx` used by the bot.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub update_id: i64,
    pub message: Value,
    pub callback: Value,
}

impl Ctx {
    pub fn new(update: &Value) -> Self {
        Self {
            update_id: update["update_id"].as_i64().unwrap_or_default(),
            message: update["message"].clone(),
            callback: update["callback_query"].clone(),
        }
    }

    /// `ctx.msg`: the message or the callback's message.
    fn msg(&self) -> &Value {
        if self.message.is_object() {
            &self.message
        } else {
            &self.callback["message"]
        }
    }

    pub fn chat_id(&self) -> Option<i64> {
        self.msg()["chat"]["id"].as_i64()
    }

    pub fn chat_type(&self) -> Option<&str> {
        self.msg()["chat"]["type"].as_str()
    }

    pub fn is_group(&self) -> bool {
        matches!(self.chat_type(), Some("group" | "supergroup"))
    }

    fn from(&self) -> &Value {
        if self.message.is_object() {
            &self.message["from"]
        } else {
            &self.callback["from"]
        }
    }

    pub fn sender_id(&self) -> Option<i64> {
        self.from()["id"].as_i64()
    }

    pub fn sender_username(&self) -> Option<&str> {
        self.from()["username"].as_str()
    }

    pub fn text(&self) -> Option<&str> {
        self.message["text"].as_str()
    }

    pub fn message_date(&self) -> Option<f64> {
        self.message["date"].as_f64()
    }

    /// `getThreadId(ctx)`: thread id of an accessible topic message.
    pub fn thread_id(&self) -> Option<i64> {
        let msg = self.msg();
        let accessible = msg["date"].as_i64().is_some_and(|d| d != 0);
        (accessible && msg["is_topic_message"] == Value::Bool(true))
            .then(|| msg["message_thread_id"].as_i64())
            .flatten()
    }
}

pub struct Bot {
    pub api: Arc<Api>,
    pub settings: Arc<Settings>,
    pub queue: Arc<Queue>,
    pub username: String,
}

fn merge(target: &mut Map<String, Value>, extra: Value) {
    if let Value::Object(extra) = extra {
        target.extend(extra);
    }
}

impl Bot {
    /// `ctx.telegram.sendMessage(chatId, text, extra)`
    pub async fn send_message(
        &self,
        chat_id: Option<i64>,
        text: &str,
        extra: Value,
    ) -> Result<Value, ApiError> {
        let mut payload = Map::new();
        payload.insert("chat_id".into(), json!(chat_id));
        payload.insert("text".into(), json!(text));
        merge(&mut payload, extra);
        self.api.call("sendMessage", Value::Object(payload)).await
    }

    /// `ctx.reply(text, extra)` (adds `message_thread_id` for topic messages).
    pub async fn reply(&self, ctx: &Ctx, text: &str, extra: Value) -> Result<Value, ApiError> {
        let mut payload = Map::new();
        if let Some(thread) = ctx.thread_id() {
            payload.insert("message_thread_id".into(), json!(thread));
        }
        merge(&mut payload, extra);
        self.send_message(ctx.chat_id(), text, Value::Object(payload))
            .await
    }

    /// `ctx.sendChatAction(action)`
    pub async fn send_chat_action(&self, ctx: &Ctx, action: &str) -> Result<Value, ApiError> {
        let mut payload = json!({ "chat_id": ctx.chat_id(), "action": action });
        if let Some(thread) = ctx.thread_id() {
            payload["message_thread_id"] = json!(thread);
        }
        self.api.call("sendChatAction", payload).await
    }
}

/// Telegraf `bot.command(name)` matching: a `bot_command` entity at offset 0, optional
/// `@botname` compared case-insensitively. Returns the command name.
pub fn matched_command(ctx: &Ctx, bot_username: &str) -> Option<String> {
    let text = ctx.text()?;
    let entity = &ctx.message["entities"][0];
    if entity["type"] != "bot_command" || entity["offset"].as_i64() != Some(0) {
        return None;
    }
    let len = entity["length"].as_u64()? as usize;
    let units: Vec<u16> = text.encode_utf16().collect();
    let head = String::from_utf16_lossy(&units[..len.min(units.len())]);
    let mut parts = head.split('@');
    let command_part = parts.next().filter(|p| !p.is_empty())?;
    if let Some(to) = parts.next()
        && !to.is_empty()
        && to.to_lowercase() != bot_username.to_lowercase()
    {
        return None;
    }
    command_part.strip_prefix('/').map(str::to_owned)
}

fn callback_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^ytdlp:(queue|status|pause|resume|clear|cancel:.+)$").expect("static regex")
    })
}

fn update_type(update: &Value) -> Option<String> {
    if update["message"].is_object() {
        return Some("message".to_owned());
    }
    update
        .as_object()?
        .keys()
        .find(|k| *k != "update_id")
        .cloned()
}

/// Runs the middleware chain for one update.
pub async fn handle_update(bot: Arc<Bot>, update: Value) -> anyhow::Result<()> {
    let ctx = Ctx::new(&update);
    let mut details = Map::new();
    details.insert("updateId".into(), update["update_id"].clone());
    details.insert("type".into(), json!(update_type(&update)));
    if let Some(chat) = ctx.chat_id() {
        details.insert("chatId".into(), json!(chat));
    }
    if let Some(user) = ctx.sender_id() {
        details.insert("userId".into(), json!(user));
    }
    if let Some(text) = ctx.text() {
        details.insert("text".into(), json!(text));
    }
    log::debug("Odebrano aktualizację", Some(Value::Object(details)));
    let started = std::time::Instant::now();
    let result = route(&bot, &ctx).await;
    log::debug(
        "Zakończono obsługę aktualizacji",
        Some(
            json!({ "updateId": update["update_id"], "durationMs": started.elapsed().as_millis() as u64 }),
        ),
    );
    result
}

async fn route(bot: &Arc<Bot>, ctx: &Ctx) -> anyhow::Result<()> {
    if ctx.message.is_object() {
        if ctx.text().is_none() {
            return Ok(());
        }
        match matched_command(ctx, &bot.username).as_deref() {
            Some("start") => {
                log::info(
                    "Komenda /start",
                    Some(json!({ "chatId": ctx.chat_id(), "userId": ctx.sender_id() })),
                );
                bot.reply(
                    ctx,
                    "Cześć! Jestem operatorem. Użyj /ytdlp <URL>, aby pobrać materiał.",
                    Value::Null,
                )
                .await?;
                return Ok(());
            }
            Some("ytdlp") => return ytdlp::handle_ytdlp(bot, ctx).await,
            Some("ytdlp_admin") => return ytdlp::handle_admin(bot, ctx).await,
            _ => {}
        }
        return ytdlp::handle_detected_media(bot, ctx).await;
    }
    if let Some(data) = ctx.callback["data"].as_str()
        && callback_regex().is_match(data)
    {
        return ytdlp::handle_callback(bot, ctx).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(text: &str, length: usize) -> Ctx {
        Ctx::new(&json!({
            "update_id": 1,
            "message": {
                "message_id": 5, "date": 1, "text": text,
                "chat": { "id": -100, "type": "supergroup" },
                "from": { "id": 7, "username": "Someone" },
                "entities": [{ "type": "bot_command", "offset": 0, "length": length }],
                "is_topic_message": true, "message_thread_id": 9
            }
        }))
    }

    #[test]
    fn command_matching() {
        assert_eq!(
            matched_command(&message("/ytdlp x", 6), "OpBot").as_deref(),
            Some("ytdlp")
        );
        assert_eq!(
            matched_command(&message("/ytdlp@opbot x", 12), "OpBot").as_deref(),
            Some("ytdlp")
        );
        assert_eq!(
            matched_command(&message("/ytdlp@other x", 12), "OpBot"),
            None
        );
        assert_eq!(
            matched_command(&message("/ytdlp_admin", 12), "OpBot").as_deref(),
            Some("ytdlp_admin")
        );
        let ctx = message("/start", 6);
        assert_eq!(ctx.thread_id(), Some(9));
        assert!(ctx.is_group());
        assert_eq!(ctx.sender_username(), Some("Someone"));
    }
}
