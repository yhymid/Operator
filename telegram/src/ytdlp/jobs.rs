#[allow(clippy::wildcard_imports)]
use super::*;

/// Queue runner: `processYtdlp(ctx, url, status, updateStatus, job)`.
pub fn runner(bot: Weak<Bot>) -> Runner {
    Arc::new(move |job: Arc<Job>| {
        let bot = bot.clone();
        Box::pin(async move {
            if let Some(bot) = bot.upgrade() {
                process_job(&bot, &job).await;
            }
        })
    })
}

pub(super) async fn process_job(bot: &Arc<Bot>, job: &Arc<Job>) {
    let started = Instant::now();
    let queued_ms = job
        .started_at_ms()
        .map(|s| s.saturating_sub(job.created_at_ms));
    log::info(
        "Rozpoczynam zadanie /ytdlp",
        Some(json!({
            "jobId": job.id,
            "chatId": job.ctx.chat_id(),
            "userId": job.ctx.sender_id(),
            "queuedDurationMs": queued_ms,
            "estimatedSizeBytes": job.estimated_size,
            "priority": job.priority.is_finite().then_some(job.priority),
        })),
    );
    let mut temp_dir: Option<PathBuf> = None;
    let result = deliver(bot, job, &mut temp_dir).await;
    if result.is_err() && job.cancel_requested() {
        // `on_cancel` already replaced the status with the cancellation notice.
        log::info("Zadanie /ytdlp anulowane", Some(json!({ "jobId": job.id })));
    } else if let Err(error) = result {
        let message = error_text(&error);
        log::error(
            "Obsługa /ytdlp zakończona błędem",
            Some(json!({
                "jobId": job.id,
                "durationMs": started.elapsed().as_millis() as u64,
                "queuedDurationMs": queued_ms,
                "error": log::error_value(&error),
            })),
        );
        job.status
            .set(format!(
                "Nie udało się pobrać materiału: {}",
                utf16_prefix(&message, 500)
            ))
            .await;
    }
    if let Some(dir) = temp_dir {
        let _ = tokio::fs::remove_dir_all(dir).await;
    }
}

pub(super) async fn delete_status(bot: &Bot, job: &Job) {
    if let Some(message_id) = job.status_message_id() {
        let _ = bot
            .api
            .call(
                "deleteMessage",
                json!({ "chat_id": job.ctx.chat_id(), "message_id": message_id }),
            )
            .await;
    }
}

pub(super) async fn send_by_url(
    bot: &Bot,
    job: &Job,
    method: &str,
    field: &str,
    url: &str,
) -> Result<()> {
    let mut payload = json!({ "chat_id": job.ctx.chat_id() });
    payload[field] = json!(url);
    if method == "sendVideo" {
        payload["supports_streaming"] = json!(true);
    }
    bot.api.call(method, payload).await?;
    Ok(())
}

pub(super) async fn upload(
    bot: &Bot,
    job: &Job,
    method: &str,
    path: &Path,
    fields: Vec<(&'static str, String)>,
) -> Result<()> {
    let chat_id = job
        .ctx
        .chat_id()
        .ok_or_else(|| anyhow!("Brak chat_id dla uploadu."))?;
    bot.api.upload_file(chat_id, method, path, fields).await?;
    Ok(())
}

pub(super) const DOCUMENT_CAPTION: &str =
    "Materiał w załączniku (Telegram nie przyjął go jako film).";

pub(super) fn warn_error(message: &str, error: &anyhow::Error) {
    log::warn(message, Some(json!(error_text(error))));
}

/// Stops a running job between steps once `/ytdlp_admin cancel|clear` was used: the signal
/// only reaches a process that is running at that moment (not probes or uploads).
pub(super) fn ensure_not_canceled(job: &Job) -> Result<()> {
    if job.cancel_requested() {
        bail!("Zadanie {} zostało anulowane.", job.id);
    }
    Ok(())
}

pub(super) async fn deliver(
    bot: &Arc<Bot>,
    job: &Arc<Job>,
    temp_dir: &mut Option<PathBuf>,
) -> Result<()> {
    let started = Instant::now();
    let url = job.url.clone();
    job.set_status_text("Pobieranie materiału…");
    job.status
        .set(format!(
            "⏳ Pobieram materiał…{}",
            estimate_line(job.estimated_size)
        ))
        .await;
    let _ = bot.send_chat_action(&job.ctx, "upload_video").await;
    let dir = make_temp_dir().await?;
    *temp_dir = Some(dir.clone());
    log::debug(
        "Utworzono katalog tymczasowy",
        Some(json!({ "tempDir": dir, "url": url, "jobId": job.id })),
    );

    let interval = bot.settings.progress_interval_ms;
    let last_update: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let progress_job = job.clone();
    let on_progress: Arc<dyn Fn(Progress) + Send + Sync> = Arc::new(move |progress: Progress| {
        let mut last = last_update.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        let recent = last.is_some_and(|t| (now - t).as_secs_f64() * 1000.0 < interval);
        if recent && progress.percent < 100.0 {
            return;
        }
        *last = Some(now);
        let text = progress_text(&progress);
        progress_job.set_status_text(text.trim_start_matches('⏬').trim_start());
        progress_job.status.set_detached(text);
    });
    let file_path = bot.download(&url, &dir, on_progress, job).await?;
    ensure_not_canceled(job)?;
    let file_path = bot.ensure_compatible(&file_path, job).await?;
    ensure_not_canceled(job)?;
    let size = tokio::fs::metadata(&file_path).await?.len() as f64;
    if size > bot.settings.max_upload_bytes {
        log::warn(
            "Plik przekracza limit Telegrama; publikuję na stronie",
            Some(json!({
                "sizeBytes": size,
                "maxUploadBytes": bot.settings.max_upload_bytes,
                "jobId": job.id,
            })),
        );
        let page = publish_to_website(bot, &file_path).await?;
        job.set_status_text("Opublikowano na stronie");
        job.status
            .set(format!(
                "✅ Plik ma {} i przekracza limit Telegrama.\n🌐 Player: {page} (ważny {} h)\n🗑️ Na stronie jest przycisk usuwania pliku.",
                format_bytes(size),
                bot.settings.website_ttl_hours
            ))
            .await;
        return Ok(());
    }

    job.status
        .set(format!(
            "✅ Pobrano {}\n📤 Wysyłam plik…",
            format_bytes(size)
        ))
        .await;
    job.set_status_text("Wysyłka pliku…");
    let extension = extname(&file_path);
    log::info(
        "Rozpoczynam upload do Telegrama",
        Some(json!({
            "sizeBytes": size,
            "extension": extension,
            "chatId": job.ctx.chat_id(),
            "jobId": job.id,
        })),
    );
    let youtube = is_youtube(&url);
    if VIDEO_EXTENSIONS.contains(&extension.as_str()) {
        if bot.settings.direct_url_first && !youtube {
            let attempt: Result<()> = async {
                let direct = bot.resolve_direct_url(&url).await?;
                log::info(
                    "Wysyłam materiał przez bezpośredni URL (bez lokalnego uploadu)",
                    Some(json!({ "chatId": job.ctx.chat_id(), "jobId": job.id })),
                );
                send_by_url(bot, job, "sendVideo", "video", &direct).await
            }
            .await;
            match attempt {
                Ok(()) => {
                    delete_status(bot, job).await;
                    log::info(
                        "Wysyłka URL-em zakończona pomyślnie",
                        Some(json!({
                            "chatId": job.ctx.chat_id(),
                            "jobId": job.id,
                            "durationMs": started.elapsed().as_millis() as u64,
                        })),
                    );
                    return Ok(());
                }
                Err(error) if is_rate_limit_error(&error) => return Err(error),
                Err(error) => warn_error(
                    "Wysyłka URL-em nie powiodła się; próbuję lokalnego uploadu",
                    &error,
                ),
            }
        }
        let video = upload(
            bot,
            job,
            "sendVideo",
            &file_path,
            vec![("supports_streaming", "true".to_owned())],
        )
        .await;
        if let Err(video_error) = video {
            if is_rate_limit_error(&video_error) {
                return Err(video_error);
            }
            let caption = vec![("caption", DOCUMENT_CAPTION.to_owned())];
            if youtube {
                warn_error(
                    "sendVideo YouTube nie powiódł się; ponawiam jako dokument lokalny",
                    &video_error,
                );
                upload(bot, job, "sendDocument", &file_path, caption).await?;
            } else {
                warn_error(
                    "sendVideo nie powiódł się; próbuję wysyłki URL-em",
                    &video_error,
                );
                let direct: Result<()> = async {
                    let direct = bot.resolve_direct_url(&url).await?;
                    send_by_url(bot, job, "sendVideo", "video", &direct).await
                }
                .await;
                if let Err(direct_error) = direct {
                    if is_rate_limit_error(&direct_error) {
                        return Err(direct_error);
                    }
                    warn_error(
                        "Wysyłka URL-em nie powiodła się; ponawiam jako dokument lokalny",
                        &direct_error,
                    );
                    upload(bot, job, "sendDocument", &file_path, caption).await?;
                }
            }
        }
    } else if let Err(document_error) =
        upload(bot, job, "sendDocument", &file_path, Vec::new()).await
    {
        if is_rate_limit_error(&document_error) {
            return Err(document_error);
        }
        warn_error(
            "sendDocument nie powiódł się; próbuję wysyłki URL-em",
            &document_error,
        );
        let direct = bot.resolve_direct_url(&url).await?;
        send_by_url(bot, job, "sendDocument", "document", &direct).await?;
    }
    delete_status(bot, job).await;
    log::info(
        "Upload zakończony pomyślnie",
        Some(json!({
            "chatId": job.ctx.chat_id(),
            "jobId": job.id,
            "durationMs": started.elapsed().as_millis() as u64,
        })),
    );
    Ok(())
}

/// `configureCommandMenu(bot)`
pub async fn configure_command_menu(bot: &Bot) {
    let user = json!([{ "command": "ytdlp", "description": "Pobierz wideo, kolejka lub status", "is_ephemeral": true }]);
    let mut admin = user.clone();
    if let Some(list) = admin.as_array_mut() {
        list.push(json!({ "command": "ytdlp_admin", "description": "Zarządzanie kolejką", "is_ephemeral": true }));
    }
    let mut calls = vec![bot.api.call("setMyCommands", json!({ "commands": user }))];
    for raw in &bot.settings.admin_ids {
        let numeric = raw.strip_prefix('-').unwrap_or(raw);
        if numeric.is_empty() || !numeric.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Ok(chat_id) = raw.parse::<i64>() else {
            continue;
        };
        calls.push(bot.api.call(
            "setMyCommands",
            json!({ "commands": admin, "scope": { "type": "chat", "chat_id": chat_id } }),
        ));
    }
    let results = futures_util::future::join_all(calls).await;
    if let Some(error) = results.into_iter().find_map(Result::err) {
        log::warn(
            "Nie udało się skonfigurować menu komend Telegrama",
            Some(json!(error.message)),
        );
    }
}
