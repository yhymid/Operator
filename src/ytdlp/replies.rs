#[allow(clippy::wildcard_imports)]
use super::*;

// ---------------------------------------------------------------------------------------------
// Replies, permissions, help
// ---------------------------------------------------------------------------------------------

pub(super) fn command_argument(text: &str, command: &str) -> String {
    let pattern = format!(r"(?i)^/{command}(?:@[^\s]+)?\s*");
    Regex::new(&pattern)
        .map(|r| r.replace(text, "").trim().to_owned())
        .unwrap_or_default()
}

/// `replyEphemeral(ctx, text)`
pub(super) async fn reply_ephemeral(bot: &Bot, ctx: &Ctx, text: &str) -> Result<Value, ApiError> {
    if !ctx.is_group() {
        return bot.reply(ctx, text, Value::Null).await;
    }
    let mut extra = json!({ "ephemeral_message_parameters": {} });
    if let Some(user) = ctx.sender_id() {
        extra["ephemeral_message_parameters"]["receiver_user_id"] = json!(user);
    }
    if let Some(id) = ctx.message["ephemeral_message_id"].as_i64() {
        extra["reply_parameters"] = json!({ "ephemeral_message_id": id });
    }
    bot.send_message(ctx.chat_id(), text, extra).await
}

/// `replyGlobal(ctx, text, extra)`
pub(super) async fn reply_global(
    bot: &Bot,
    ctx: &Ctx,
    text: &str,
    extra: Value,
) -> Result<Value, ApiError> {
    if ctx.is_group() {
        return bot.send_message(ctx.chat_id(), text, extra).await;
    }
    bot.reply(ctx, text, extra).await
}

/// Admins are matched by numeric user ID only: a username can be released and taken by someone else.
pub(super) fn is_admin(bot: &Bot, ctx: &Ctx) -> bool {
    let user_id = ctx.sender_id().map(|id| id.to_string()).unwrap_or_default();
    let allowed = !user_id.is_empty() && bot.settings.admin_ids.contains(&user_id);
    if !allowed {
        log::info(
            "Odmówiono uprawnień administratora",
            Some(json!({ "userId": user_id })),
        );
    }
    allowed
}

pub(super) fn user_help() -> String {
    [
        "Użytkownika Komendy",
        "/ytdlp <URL> — dodaj materiał do kolejki",
        "/ytdlp queue — pokaż kolejkę",
        "/ytdlp status — pokaż aktywne zadanie i postęp",
    ]
    .join("\n")
}

pub(super) fn admin_help() -> String {
    [
        "/ytdlp_admin cancel <pozycja> — zatrzymaj zadanie (1 = aktywne)",
        "/ytdlp_admin pause — pauza aktywnego i kolejnych zadań",
        "/ytdlp_admin resume — wznow kolejkę",
        "/ytdlp_admin clear — zatrzymaj wszystko",
    ]
    .join("\n")
}

pub(super) fn command_help(bot: &Bot, ctx: &Ctx) -> String {
    if is_admin(bot, ctx) {
        [
            user_help(),
            String::new(),
            "Administratorskie Komendy".to_owned(),
            admin_help(),
        ]
        .join("\n")
    } else {
        user_help()
    }
}
