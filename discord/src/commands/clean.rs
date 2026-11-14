//! `/clean` (port of `src/cmds/clean.js`).

use anyhow::Result;
use serde_json::Value;

use crate::discord::{self, Ix};
use crate::modules::autoclean::QueuedEntry;
use crate::util::text::pl_datetime;
use operator_common::time::parse_iso;

/// `formatStoredDate(value, timezone)`
pub fn format_stored_date(value: Option<&str>, timezone: &str) -> String {
    let Some(value) = value.filter(|v| !v.is_empty()) else {
        return "none".to_owned();
    };
    let Some(parsed) = parse_iso(value) else {
        return value.to_owned();
    };
    let tz = timezone
        .parse::<chrono_tz::Tz>()
        .unwrap_or(chrono_tz::Europe::Warsaw);
    pl_datetime(parsed, tz)
}

fn format_permanent(channels: &[(String, u64, bool)]) -> String {
    if channels.is_empty() {
        return "none".to_owned();
    }
    channels
        .iter()
        .map(|(id, limit, pinned)| {
            let mut detail = Vec::new();
            if *limit > 0 {
                detail.push(format!("limit={limit}"));
            }
            if *pinned {
                detail.push("pinned=yes".to_owned());
            }
            if detail.is_empty() {
                format!("<#{id}>")
            } else {
                format!("<#{id}> ({})", detail.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_queued(entries: &[QueuedEntry], timezone: &str) -> String {
    if entries.is_empty() {
        return "none".to_owned();
    }
    entries
        .iter()
        .map(|entry| {
            let mut parts = vec![format!("<#{}>", entry.channel_id)];
            if let Some(at) = entry.queued_at.as_deref().filter(|s| !s.is_empty()) {
                parts.push(format!("queued={}", format_stored_date(Some(at), timezone)));
            }
            if let Some(by) = entry.queued_by.as_deref().filter(|s| !s.is_empty()) {
                parts.push(format!("by=<@{by}>"));
            }
            parts.join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn js_value_text(value: &Value, fallback: &str) -> String {
    match value {
        Value::Null => fallback.to_owned(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

pub async fn execute(ix: &Ix<'_>) -> Result<()> {
    let Some(guild_id) = ix.guild_id() else {
        return ix
            .reply_ephemeral("This command only works in a server.")
            .await;
    };
    if !ix.app.is_owner(ix.user_id()) {
        return ix.reply_ephemeral("This command is owner-only.").await;
    }
    let guild = guild_id.to_string();
    let mode = ix.opts().lower("mode");
    let cleaner = &ix.app.autoclean;

    if mode == "status" {
        let s = cleaner.status_snapshot(ix.app, &guild);
        let lines = [
            format!(
                "AutoClean: {}",
                if s.enabled { "enabled" } else { "disabled" }
            ),
            format!("CRON: {}", s.schedule),
            format!("Timezone: {}", s.timezone),
            format!("Next cleanup: {}", s.next_run_text),
            format!(
                "Next run cache: {}",
                format_stored_date(s.next_run_at.as_deref(), &s.timezone)
            ),
            format!("Dry run: {}", if s.dry_run { "yes" } else { "no" }),
            format!("Interval: {}", s.interval_text),
            format!("Minimum message age: {} days", s.delete_older_than_days),
            format!(
                "Delete limit: {}",
                if s.delete_limit > 0 {
                    s.delete_limit.to_string()
                } else {
                    "none".to_owned()
                }
            ),
            format!(
                "Delete pinned: {}",
                if s.delete_pinned { "yes" } else { "no" }
            ),
            format!("Whitelist users: {}", s.whitelist_users),
            format!("Whitelist roles: {}", s.whitelist_roles),
            format!("Notify before delete: {}", s.notify),
            format!(
                "Log channel: {}",
                s.log_channel_id
                    .as_deref()
                    .map_or("none".to_owned(), |c| format!("<#{c}>"))
            ),
            format!(
                "Permanent channels from config ({}): {}",
                s.permanent_channels.len(),
                format_permanent(&s.permanent_channels)
            ),
            format!("Queued one-time ({}):", s.queued.len()),
            format_queued(&s.queued, &s.timezone),
            format!(
                "Last cleanup: {}",
                format_stored_date(s.last_run_at.as_deref(), &s.timezone)
            ),
            format!(
                "Ostatni trigger: {}",
                s.last_trigger.as_deref().unwrap_or("none")
            ),
            format!("Last deleted: {}", js_value_text(&s.last_deleted, "0")),
            format!(
                "Last run time: {} ms",
                js_value_text(&s.last_duration_ms, "none")
            ),
            format!("Last error: {}", s.last_error.as_deref().unwrap_or("none")),
            format!("Runs in this session: {}", s.runs),
            format!("Deleted in this session: {}", s.total_deleted),
        ];
        return ix.reply_ephemeral(lines.join("\n")).await;
    }

    let channel = ix
        .interaction
        .channel
        .as_ref()
        .filter(|c| discord::is_text_based(c.kind));
    let Some(channel) = channel else {
        return ix
            .reply_ephemeral("This channel does not support text messages.")
            .await;
    };
    let channel_id = channel.id;

    match mode.as_str() {
        "set" => {
            let already = cleaner.queue_channel(
                ix.app,
                &guild,
                &channel_id.to_string(),
                &ix.user_id().to_string(),
            );
            let status = cleaner.status_snapshot(ix.app, &guild);
            let next = format_stored_date(status.next_run_at.as_deref(), &status.timezone);
            ix.reply_ephemeral(if already {
                format!(
                    "<#{channel_id}> is already queued for a one-time cleanup. Next run: {next}."
                )
            } else {
                format!(
                    "Added <#{channel_id}> to a one-time cleanup at the next run. Next run: {next}."
                )
            })
            .await
        }
        "reset" => {
            let removed = cleaner.reset_queued_channel(ix.app, &guild, &channel_id.to_string());
            ix.reply_ephemeral(if removed {
                format!("Removed <#{channel_id}> from the one-time cleanup queue.")
            } else {
                format!("<#{channel_id}> was not queued for a one-time cleanup.")
            })
            .await
        }
        "now" => {
            ix.defer(true).await?;
            match cleaner
                .run_cleanup_for_channel(ix.app, channel_id, "slash")
                .await
            {
                None => {
                    ix.edit_content("A cleanup is already running. Try again in a moment.")
                        .await
                }
                Some(result) => match result.error {
                    Some(error) => ix.edit_content(format!("Cleanup error: {error}")).await,
                    None => {
                        ix.edit_content(format!(
                            "Cleaned <#{channel_id}>. Deleted {} messages.",
                            result.deleted
                        ))
                        .await
                    }
                },
            }
        }
        _ => {
            ix.reply_ephemeral(format!("Unknown clean mode: {mode}."))
                .await
        }
    }
}
