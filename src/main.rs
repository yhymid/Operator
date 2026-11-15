//! Operator Telegram bot — Rust rewrite of `bot.js` (Telegraf long polling + `/ytdlp`).

mod api;
mod bot;
mod common;
mod limiter;
mod log;
mod queue;
mod settings;
mod status;
mod ytdlp;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::api::{Api, ApiError};
use crate::bot::Bot;
use crate::queue::Queue;
use crate::settings::Settings;

/// Long-poll timeout requested from Telegram (Telegraf default) and our HTTP margin on top.
const POLL_TIMEOUT_SECS: u64 = 50;
const POLL_HTTP_TIMEOUT: Duration = Duration::from_secs(POLL_TIMEOUT_SECS + 20);
/// How long a SIGINT/SIGTERM waits for a running download before exiting
/// (the systemd unit uses `TimeoutStopSec=30`).
const SHUTDOWN_GRACE: Duration = Duration::from_secs(25);
/// Update handlers running at once; polling waits for a free slot when all are busy.
const MAX_CONCURRENT_HANDLERS: usize = 32;

fn main() -> ExitCode {
    crate::common::env::load_dotenv();
    log::init();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            log::error(
                "Nie udało się uruchomić bota",
                Some(json!(error.to_string())),
            );
            return ExitCode::FAILURE;
        }
    };
    let code = runtime.block_on(run());
    runtime.shutdown_timeout(Duration::from_secs(1));
    code
}

async fn run() -> ExitCode {
    let settings = match Settings::from_env() {
        Ok(settings) => Arc::new(settings),
        Err(error) => {
            // `throw new Error('Brak TELEGRAM_BOT_TOKEN w pliku .env')` at startup.
            eprintln!("Error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let api = match Api::new(settings.clone()) {
        Ok(api) => api,
        Err(error) => {
            log::error(
                "Nie udało się uruchomić bota",
                Some(log::error_value(&error)),
            );
            return ExitCode::FAILURE;
        }
    };
    let queue = Queue::new(
        "ytdlp",
        settings.queue_concurrency,
        settings.queue_max_pending,
        settings.queue_max_per_user,
    );

    // `bot.launch()`: getMe → deleteWebhook → long polling.
    let me = match api.raw_call("getMe", &json!({}), POLL_HTTP_TIMEOUT).await {
        Ok(me) => me,
        Err(error) => return launch_failed(&error),
    };
    let bot = Arc::new(Bot {
        api: api.clone(),
        settings: settings.clone(),
        queue: queue.clone(),
        username: me["username"].as_str().unwrap_or_default().to_owned(),
    });
    queue.set_runner(ytdlp::runner(Arc::downgrade(&bot)));
    if std::env::var("ADMIN_USERNAMES").is_ok_and(|v| !v.trim().is_empty()) {
        log::warn(
            "ADMIN_USERNAMES nie jest już obsługiwane - ustaw ADMIN_IDS (numeryczne ID użytkowników).",
            Some(json!({ "adminIds": settings.admin_ids.len() })),
        );
    }
    log::info(
        "Zarejestrowano komendę /ytdlp",
        Some(json!({
            "queue": queue.name,
            "concurrency": queue.concurrency,
            "maxPending": queue.max_pending,
        })),
    );
    if let Err(error) = api
        .raw_call("deleteWebhook", &json!({}), POLL_HTTP_TIMEOUT)
        .await
    {
        return launch_failed(&error);
    }
    // In the JS bot this ran only after polling stopped (Telegraf 4.16 `launch()` resolves
    // on stop); here the command menu is configured as soon as the bot is up.
    let menu_bot = bot.clone();
    tokio::spawn(async move { ytdlp::configure_command_menu(&menu_bot).await });
    log::info(
        "Bot uruchomiony. Oczekiwanie na wiadomości…",
        Some(json!({
            "developerMode": log::developer_mode(),
            "handlerTimeout": settings.handler_timeout_ms,
            "apiRoot": settings.api_root,
        })),
    );

    let shutdown = crate::common::shutdown::signal();
    tokio::pin!(shutdown);
    let mut offset: i64 = 0;
    let handler_slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_HANDLERS));
    let outcome = loop {
        let request = json!({ "timeout": POLL_TIMEOUT_SECS, "offset": offset });
        let poll = api.raw_call("getUpdates", &request, POLL_HTTP_TIMEOUT);
        let updates = tokio::select! {
            signal = &mut shutdown => break Ok(signal),
            result = poll => result,
        };
        let updates = match updates {
            Ok(Value::Array(updates)) => updates,
            Ok(_) => Vec::new(),
            Err(error) => {
                // Telegraf retries network errors, 429 and 5xx; anything else stops the bot.
                let retryable = error.code.is_none_or(|code| code == 429 || code >= 500);
                if !retryable {
                    break Err(error);
                }
                let wait = error.retry_after.unwrap_or(5.0);
                log::debug(
                    "Failed to fetch updates, retrying",
                    Some(json!({ "retryAfter": wait, "error": error.message })),
                );
                tokio::select! {
                    signal = &mut shutdown => break Ok(signal),
                    () = tokio::time::sleep(Duration::from_secs_f64(wait)) => continue,
                }
            }
        };
        if let Some(last) = updates.last().and_then(|u| u["update_id"].as_i64()) {
            offset = last + 1;
        }
        // Each update runs in its own task; polling continues right away, so a slow handler
        // (size probe, Telegram rate limits) in one chat does not hold up the others. The
        // semaphore bounds how many handlers run at once and slows polling down when full.
        let timeout = Duration::from_millis(settings.handler_timeout_ms);
        for update in updates {
            let Ok(permit) = handler_slots.clone().acquire_owned().await else {
                break;
            };
            let bot = bot.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let id = update["update_id"].clone();
                let label = format!(
                    "Błąd obsługi aktualizacji {}",
                    if id.is_null() {
                        "unknown".to_owned()
                    } else {
                        id.to_string()
                    }
                );
                let mut handle = tokio::spawn(bot::handle_update(bot, update));
                match tokio::time::timeout(timeout, &mut handle).await {
                    Ok(Ok(Ok(()))) => {}
                    Ok(Ok(Err(error))) => log::error(&label, Some(log::error_value(&error))),
                    Ok(Err(join)) => log::error(&label, Some(json!(join.to_string()))),
                    // The handler keeps running in the background, as with Telegraf's pTimeout.
                    Err(_) => log::error(
                        &label,
                        Some(json!(format!(
                            "Promise timed out after {} milliseconds",
                            timeout.as_millis()
                        ))),
                    ),
                }
            });
        }
    };

    match outcome {
        Ok(signal) => {
            log::info(&format!("Zatrzymywanie bota ({signal})…"), None);
            // `polling.syncUpdateOffset()`: confirm handled updates so they are not redelivered.
            let _ = api
                .raw_call(
                    "getUpdates",
                    &json!({ "offset": offset, "limit": 1 }),
                    Duration::from_secs(10),
                )
                .await;
            queue.stop_starting();
            let deadline = tokio::time::Instant::now() + SHUTDOWN_GRACE;
            // Updates already taken from Telegram are confirmed: let their handlers finish.
            while handler_slots.available_permits() < MAX_CONCURRENT_HANDLERS
                && tokio::time::Instant::now() < deadline
            {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            while queue.active_count() > 0 && tokio::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            ExitCode::SUCCESS
        }
        Err(error) => launch_failed(&error),
    }
}

fn launch_failed(error: &ApiError) -> ExitCode {
    log::error(
        "Nie udało się uruchomić bota",
        Some(json!({ "name": "TelegramError", "message": error.message })),
    );
    ExitCode::FAILURE
}
