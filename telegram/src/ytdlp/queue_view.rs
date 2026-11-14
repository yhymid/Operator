#[allow(clippy::wildcard_imports)]
use super::*;

// ---------------------------------------------------------------------------------------------
// Queue overview
// ---------------------------------------------------------------------------------------------

pub(super) fn queue_job_line(job: &Arc<Job>, queue: &Queue) -> String {
    let size = job
        .estimated_size
        .map_or("rozmiar ?".to_owned(), format_bytes);
    if job.state() == JobState::Running {
        return format!(
            "▶️ {} — {}\n   {} • {size}",
            job.id,
            job.status_text().unwrap_or_else(|| "w trakcie".to_owned()),
            short_url(&job.url)
        );
    }
    let position = queue
        .position(job)
        .map_or("?".to_owned(), |p| p.to_string());
    format!(
        "⏳ {} — pozycja {position}\n   {} • {size}",
        job.id,
        short_url(&job.url)
    )
}

pub(super) fn format_queue_overview(queue: &Queue, status_only: bool) -> String {
    let jobs = queue.active_and_pending();
    let active = queue.active_jobs();
    let paused = queue.paused();
    if jobs.is_empty() {
        return format!(
            "📥 Kolejka /ytdlp jest pusta.{}",
            if paused {
                "\n⏸️ Stan: wstrzymana"
            } else {
                ""
            }
        );
    }
    let mut lines = vec![
        if status_only {
            "📊 Status /ytdlp"
        } else {
            "📥 Kolejka /ytdlp"
        }
        .to_owned(),
        format!(
            "Stan: {} • aktywne: {} • oczekujące: {}",
            if paused {
                "⏸️ wstrzymana"
            } else {
                "▶️ aktywna"
            },
            active.len(),
            queue.pending_count()
        ),
    ];
    let visible = if status_only { &active } else { &jobs };
    if visible.is_empty() {
        lines.push("Brak aktywnego zadania.".to_owned());
    } else {
        lines.extend(visible.iter().map(|job| queue_job_line(job, queue)));
    }
    lines.join("\n")
}

pub(super) fn button(text: impl Into<String>, data: impl Into<String>) -> Value {
    json!({ "text": text.into(), "callback_data": data.into() })
}

/// `queueKeyboard(queue, ctx)` → `{ reply_markup: { inline_keyboard } }`
pub(super) fn queue_keyboard(bot: &Bot, ctx: &Ctx) -> Value {
    let queue = &bot.queue;
    let mut rows = vec![vec![
        button("📥 Queue", "ytdlp:queue"),
        button("📊 Status", "ytdlp:status"),
    ]];
    if is_admin(bot, ctx) {
        let paused = queue.paused();
        rows.push(vec![
            button(
                if paused {
                    "▶️ Wznów"
                } else {
                    "⏸️ Pauza"
                },
                if paused {
                    "ytdlp:resume"
                } else {
                    "ytdlp:pause"
                },
            ),
            button("🗑️ Wyczyść", "ytdlp:clear"),
        ]);
        for job in queue.active_and_pending() {
            let position = queue
                .position(&job)
                .map_or("null".to_owned(), |p| p.to_string());
            rows.push(vec![button(
                format!("❌ Anuluj pozycję {position}"),
                format!("ytdlp:cancel:{position}"),
            )]);
        }
    }
    json!({ "reply_markup": { "inline_keyboard": rows } })
}

pub(super) fn job_at_position(raw: &str, queue: &Queue) -> Option<Arc<Job>> {
    let position = crate::settings::js_number(Some(raw));
    if !position.is_finite() || position.fract() != 0.0 || position < 1.0 {
        return None;
    }
    queue
        .active_and_pending()
        .into_iter()
        .nth(position as usize - 1)
}

pub(super) async fn handle_queue_command(bot: &Bot, ctx: &Ctx, status_only: bool) -> Result<()> {
    log::info(
        if status_only {
            "Odebrano /ytdlp status"
        } else {
            "Odebrano /ytdlp queue"
        },
        Some(
            json!({ "updateId": ctx.update_id, "chatId": ctx.chat_id(), "userId": ctx.sender_id() }),
        ),
    );
    let text = format_queue_overview(&bot.queue, status_only);
    reply_global(bot, ctx, &text, queue_keyboard(bot, ctx)).await?;
    Ok(())
}
