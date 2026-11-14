#[allow(clippy::wildcard_imports)]
use super::*;

// ---------------------------------------------------------------------------------------------
// Stale updates
// ---------------------------------------------------------------------------------------------

pub(super) fn message_age_seconds(ctx: &Ctx, now_ms: u64) -> Option<f64> {
    let date = ctx.message_date().filter(|d| d.is_finite() && *d > 0.0)?;
    Some((now_ms as f64 / 1000.0 - date).max(0.0))
}

/// `ignoreStaleTelegramMessage(ctx, source)`
pub(super) fn ignore_stale(bot: &Bot, ctx: &Ctx, source: &str) -> bool {
    let max = bot.settings.max_message_age_secs as f64;
    let Some(age) = message_age_seconds(ctx, crate::queue::now_ms()).filter(|a| *a > max) else {
        return false;
    };
    log::warn(
        "Pominięto opóźnioną aktualizację Telegrama",
        Some(json!({
            "source": source,
            "updateId": ctx.update_id,
            "messageDate": ctx.message["date"],
            "ageSeconds": age.round() as i64,
            "maxAgeSeconds": bot.settings.max_message_age_secs,
            "chatId": ctx.chat_id(),
            "userId": ctx.sender_id(),
        })),
    );
    true
}
// ---------------------------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------------------------

pub(super) fn cancel_text(outcome: &CancelOutcome, position: &str) -> Option<String> {
    match outcome {
        CancelOutcome::CancelRequested => Some(format!("Zatrzymuję pozycję {position}.")),
        CancelOutcome::Canceled => Some(format!("Anulowano pozycję {position}.")),
        CancelOutcome::NotPending(_) => None,
    }
}

/// `/ytdlp_admin`
pub async fn handle_admin(bot: &Arc<Bot>, ctx: &Ctx) -> Result<()> {
    if ignore_stale(bot, ctx, "/ytdlp_admin") {
        return Ok(());
    }
    if !is_admin(bot, ctx) {
        reply_global(bot, ctx, "🚫 Brak uprawnień administratora.", Value::Null).await?;
        return Ok(());
    }
    let argument = command_argument(ctx.text().unwrap_or_default(), "ytdlp_admin");
    let mut parts = argument.split_whitespace();
    let action = parts.next().unwrap_or_default().to_lowercase();
    let rest: Vec<&str> = parts.collect();
    log::info(
        "Odebrano /ytdlp_admin",
        Some(json!({
            "action": if action.is_empty() { Value::Null } else { json!(action) },
            "args": rest,
            "chatId": ctx.chat_id(),
            "userId": ctx.sender_id(),
        })),
    );
    let queue = &bot.queue;
    let text = match action.as_str() {
        "" => {
            reply_ephemeral(bot, ctx, &admin_help()).await?;
            return Ok(());
        }
        "pause" => {
            queue.pause();
            format!(
                "⏸️ Pauza. Zatrzymano aktywne pobieranie i wstrzymano kolejkę.\nOczekujących: {}",
                queue.pending_count()
            )
        }
        "resume" => {
            queue.resume();
            format!(
                "▶️ Kolejka wznowiona. Aktywne: {}, oczekujące: {}.",
                queue.active_count(),
                queue.pending_count()
            )
        }
        "clear" => {
            let count = queue.clear();
            format!(
                "🗑️ Zatrzymano/anulowano {count} {} w kolejce.",
                if count == 1 { "zadanie" } else { "zadań" }
            )
        }
        "cancel" => match rest.first() {
            None => "Podaj pozycję, np. `/ytdlp_admin cancel 1`".to_owned(),
            Some(requested) => match job_at_position(requested, queue) {
                None => format!("⚠️ Nie znaleziono pozycji {requested}. Użyj /ytdlp queue."),
                Some(job) => {
                    let outcome = queue.cancel(&job);
                    match (&outcome, cancel_text(&outcome, requested)) {
                        (_, Some(text)) => format!("❌ {text}"),
                        (CancelOutcome::NotPending(state), None) if *state != JobState::Queued => {
                            format!("⚠️ Pozycja {requested} ma już stan: {}.", state.as_str())
                        }
                        _ => format!("⚠️ Nie udało się zatrzymać pozycji {requested}."),
                    }
                }
            },
        },
        _ => admin_help(),
    };
    reply_global(bot, ctx, &text, Value::Null).await?;
    Ok(())
}

/// Inline keyboard callbacks (`ytdlp:*`).
pub async fn handle_callback(bot: &Arc<Bot>, ctx: &Ctx) -> Result<()> {
    static DATA: OnceLock<Regex> = OnceLock::new();
    let data = ctx.callback["data"].as_str().unwrap_or_default();
    let Some(captures) = re(
        &DATA,
        r"^ytdlp:(queue|status|pause|resume|clear|cancel)(?::(.+))?$",
    )
    .captures(data) else {
        return Ok(());
    };
    let action = captures[1].to_owned();
    let requested = captures
        .get(2)
        .map_or("undefined".to_owned(), |m| m.as_str().to_owned());
    let callback_id = ctx.callback["id"].clone();
    let answer = |text: Option<String>, alert: bool| {
        let bot = bot.clone();
        let callback_id = callback_id.clone();
        async move {
            let mut payload = json!({ "callback_query_id": callback_id });
            if let Some(text) = text {
                payload["text"] = json!(text);
            }
            if alert {
                payload["show_alert"] = json!(true);
            }
            let _ = bot.api.call("answerCallbackQuery", payload).await;
        }
    };
    let edit = |status_only: bool| {
        let bot = bot.clone();
        let ctx = ctx.clone();
        async move {
            let text = format_queue_overview(&bot.queue, status_only);
            let extra = queue_keyboard(&bot, &ctx);
            let message_id = ctx.callback["message"]["message_id"].clone();
            if !message_id.is_null() {
                let mut payload = json!({
                    "chat_id": ctx.chat_id(),
                    "message_id": message_id,
                    "text": text,
                });
                payload["reply_markup"] = extra["reply_markup"].clone();
                match bot.api.call("editMessageText", payload).await {
                    Ok(_) => return Ok(()),
                    Err(error)
                        if error
                            .message
                            .to_lowercase()
                            .contains("message is not modified") =>
                    {
                        return Ok(());
                    }
                    Err(error) => log::debug(
                        "Nie udało się odświeżyć wiadomości kolejki",
                        Some(json!(error.message)),
                    ),
                }
            }
            bot.reply(&ctx, &text, extra).await.map(|_| ())
        }
    };

    if action == "queue" || action == "status" {
        answer(None, false).await;
        edit(action == "status").await?;
        return Ok(());
    }
    if !is_admin(bot, ctx) {
        answer(Some("Brak uprawnień administratora.".to_owned()), true).await;
        return Ok(());
    }
    let queue = &bot.queue;
    match action.as_str() {
        "pause" => {
            queue.pause();
            answer(
                Some("Pauza aktywnego pobierania i kolejki.".to_owned()),
                false,
            )
            .await;
        }
        "resume" => {
            queue.resume();
            answer(Some("Kolejka wznowiona.".to_owned()), false).await;
        }
        "clear" => {
            let count = queue.clear();
            answer(Some(format!("Zatrzymano/anulowano: {count}.")), false).await;
        }
        _ => {
            let outcome = job_at_position(&requested, queue).map(|job| queue.cancel(&job));
            match outcome.as_ref().and_then(|o| cancel_text(o, &requested)) {
                Some(text) => answer(Some(text), false).await,
                None => answer(Some(format!("Nie znaleziono pozycji {requested}.")), true).await,
            }
        }
    }
    edit(false).await?;
    Ok(())
}

/// `/ytdlp`
pub async fn handle_ytdlp(bot: &Arc<Bot>, ctx: &Ctx) -> Result<()> {
    if ignore_stale(bot, ctx, "/ytdlp") {
        return Ok(());
    }
    let requested_at = Instant::now();
    log::info(
        "Odebrano /ytdlp",
        Some(json!({
            "updateId": ctx.update_id,
            "chatId": ctx.chat_id(),
            "userId": ctx.sender_id(),
            "raw": ctx.text(),
        })),
    );
    let argument = command_argument(ctx.text().unwrap_or_default(), "ytdlp");
    if argument.is_empty() {
        reply_ephemeral(bot, ctx, &command_help(bot, ctx)).await?;
        return Ok(());
    }
    let subcommand = argument
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase();
    match subcommand.as_str() {
        "queue" => return handle_queue_command(bot, ctx, false).await,
        "status" => return handle_queue_command(bot, ctx, true).await,
        "cancel" | "pause" | "resume" | "clear" => {
            reply_global(
                bot,
                ctx,
                "Ta opcja jest dostępna tylko przez /ytdlp_admin.",
                Value::Null,
            )
            .await?;
            return Ok(());
        }
        _ => {}
    }
    match normalize_url(&argument) {
        Ok(url) => enqueue(bot, ctx, url, requested_at).await,
        Err(message) => {
            log::warn("Niepoprawny URL w /ytdlp", Some(json!(message)));
            reply_global(bot, ctx, &message, Value::Null).await?;
            Ok(())
        }
    }
}

/// Plain text messages with a supported short-video link.
pub async fn handle_detected_media(bot: &Arc<Bot>, ctx: &Ctx) -> Result<()> {
    let Some(url) = extract_auto_media_url(ctx.text().unwrap_or_default()) else {
        return Ok(());
    };
    if ignore_stale(bot, ctx, "auto-media-link") {
        return Ok(());
    }
    log::info(
        "Wykryto link multimedialny bez komendy",
        Some(
            json!({ "updateId": ctx.update_id, "chatId": ctx.chat_id(), "userId": ctx.sender_id() }),
        ),
    );
    enqueue(bot, ctx, url, Instant::now()).await
}

pub(super) fn estimate_line(estimated: Option<f64>) -> String {
    estimated.map_or(String::new(), |size| {
        format!("\n📦 Szacowany rozmiar: {}", format_bytes(size))
    })
}

pub(super) fn refused_text(queue: &Queue, error: AddError) -> String {
    match error {
        AddError::Full => format!(
            "⚠️ Kolejka jest pełna (maksymalnie {} oczekujących zadań). Spróbuj ponownie później.",
            queue.max_pending
        ),
        AddError::UserLimit => format!(
            "⚠️ Masz już {} zadań w kolejce. Poczekaj, aż któreś się skończy.",
            queue.max_per_user
        ),
    }
}

/// Refuses links to local/private addresses before yt-dlp sees them.
pub(super) async fn check_target(url: &str) -> Result<(), &'static str> {
    let parsed = url::Url::parse(url).map_err(|_| "Podaj poprawny adres URL.")?;
    let host = parsed.host_str().unwrap_or_default();
    let port = parsed.port_or_known_default().unwrap_or(443);
    operator_common::net::ensure_public_host(host, port).await
}

/// `enqueueYtdlp(ctx, url)`
pub(super) async fn enqueue(
    bot: &Arc<Bot>,
    ctx: &Ctx,
    url: String,
    requested_at: Instant,
) -> Result<()> {
    let queue = &bot.queue;
    if let Err(message) = check_target(&url).await {
        log::warn(
            "Odrzucono adres /ytdlp",
            Some(json!({ "reason": message, "chatId": ctx.chat_id(), "userId": ctx.sender_id() })),
        );
        reply_global(bot, ctx, &format!("⚠️ {message}"), Value::Null).await?;
        return Ok(());
    }
    // Before the size probe: a refused job must not cost a yt-dlp run.
    if let Err(error) = queue.check_capacity(ctx.sender_id()) {
        log::warn(
            "Nie udało się dodać /ytdlp do kolejki",
            Some(
                json!({ "error": format!("{error:?}"), "chatId": ctx.chat_id(), "userId": ctx.sender_id() }),
            ),
        );
        reply_global(bot, ctx, &refused_text(queue, error), Value::Null).await?;
        return Ok(());
    }
    let first = if bot.settings.size_probe {
        "🔎 Sprawdzam rozmiar materiału…"
    } else {
        "⏳ Przyjmuję zadanie do kolejki…"
    };
    let status = reply_global(bot, ctx, first, Value::Null).await?;
    let updater = StatusUpdater::new(
        bot.api.clone(),
        ctx.chat_id(),
        status["message_id"].as_i64(),
    );
    let estimated = bot.estimate_download_size(&url).await;
    if bot.settings.size_probe {
        updater
            .set(match estimated {
                Some(size) => format!(
                    "📦 Szacowany rozmiar: {}\n⏳ Dodaję do kolejki…",
                    format_bytes(size)
                ),
                None => "⚠️ Nie udało się ustalić rozmiaru.\n⏳ Dodaję do kolejki…".to_owned(),
            })
            .await;
    }
    let job = match queue.add(NewJob {
        ctx: ctx.clone(),
        url: url.clone(),
        estimated_size: estimated,
        status: updater.clone(),
        status_message_id: status["message_id"].as_i64(),
    }) {
        Ok(job) => job,
        Err(error) => {
            log::warn(
                "Nie udało się dodać /ytdlp do kolejki",
                Some(json!({ "error": format!("{error:?}"), "chatId": ctx.chat_id() })),
            );
            updater.set(refused_text(queue, error)).await;
            return Ok(());
        }
    };
    let position = queue.position(&job);
    let state = job.state();
    if state == JobState::Queued {
        updater
            .set(queue_status_text(
                position,
                queue.pending_count(),
                estimated,
            ))
            .await;
    } else {
        updater
            .set(format!("⏳ Pobieram materiał…{}", estimate_line(estimated)))
            .await;
    }
    log::info(
        "Dodano /ytdlp do kolejki",
        Some(json!({
            "jobId": job.id,
            "queue": queue.name,
            "state": state.as_str(),
            "position": position,
            "pendingCount": queue.pending_count(),
            "queueSize": queue.active_count() + queue.pending_count(),
            "chatId": ctx.chat_id(),
            "userId": ctx.sender_id(),
            "estimatedSizeBytes": estimated,
            "priority": estimated,
            "enqueueDurationMs": requested_at.elapsed().as_millis() as u64,
        })),
    );
    Ok(())
}
