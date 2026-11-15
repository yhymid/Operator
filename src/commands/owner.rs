//! Owner-only maintenance commands: `/update`, `/sync`, `/servers`, `/presence`, `/restart`.
//!
//! The bot runs unprivileged, so `/update` only starts `operator-update@<target>.service`
//! (a root oneshot from `scripts/ops/`, allowed for the bot user by a polkit rule) and reports
//! the log file the script writes. A unit keeps running when it restarts the bot itself.

use std::time::Duration;

use anyhow::Result;
use serde_json::json;
use twilight_model::id::Id;
use twilight_model::id::marker::GuildMarker;

use crate::discord::Ix;
use crate::presence::{self, Presence};

const SERVERS_PREFIX: &str = "owner_servers";
/// `/update` waits for the unit within Discord's 15-minute interaction window.
const UPDATE_WAIT: Duration = Duration::from_secs(13 * 60);
const UPDATE_LOG_DIR: &str = "/var/lib/operator-update";
const UPDATE_TARGETS: [&str; 4] = ["ytdlp", "lavalink", "server", "operator"];
/// Reply budget for the log tail (Discord message limit is 2000 characters).
const LOG_TAIL_CHARS: usize = 1500;

async fn owner_only(ix: &Ix<'_>) -> Result<bool> {
    if ix.app.is_owner(ix.user_id()) {
        return Ok(true);
    }
    ix.reply_ephemeral("This command is owner-only.").await?;
    Ok(false)
}

/// Music sessions and the /ytdlp job slot; a restart would cut them off.
async fn busy_reason(ix: &Ix<'_>) -> Option<String> {
    let sessions = ix.app.music.active_sessions().await;
    if sessions > 0 {
        return Some(format!("{sessions} music session(s) are active"));
    }
    if super::ytdlp::busy() {
        return Some("a /ytdlp download is running".to_owned());
    }
    None
}

fn tail(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let start = chars.len().saturating_sub(max);
    let mut out: String = chars[start..].iter().collect();
    if start > 0
        && let Some(newline) = out.find('\n')
    {
        out = out[newline + 1..].to_owned();
    }
    out.replace("```", "'''")
}

async fn systemctl(args: &[&str]) -> Result<std::process::Output> {
    Ok(tokio::process::Command::new("systemctl")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .await?)
}

pub async fn execute_update(ix: &Ix<'_>) -> Result<()> {
    if !owner_only(ix).await? {
        return Ok(());
    }
    let target = ix.opts().string("target").unwrap_or_default();
    if !UPDATE_TARGETS.contains(&target.as_str()) {
        return ix.reply_ephemeral("Unknown target.").await;
    }
    if target == "lavalink"
        && let Some(reason) = busy_reason(ix).await
    {
        return ix
            .reply_ephemeral(format!(
                "Not now: {reason}. Lavalink restarts during the update."
            ))
            .await;
    }
    ix.defer(true).await?;
    let unit = format!("operator-update@{target}.service");
    if target == "operator" {
        // The script replaces this binary and restarts the bot, so do not wait for it.
        let output = systemctl(&["start", "--no-block", &unit]).await?;
        if !output.status.success() {
            return ix
                .edit_content(format!(
                    "Could not start `{unit}`: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ))
                .await;
        }
        return ix
            .edit_content(
                "Bot update started (Telegram, then Discord). Each bot with a newer release \
                 restarts as soon as nothing is playing or downloading, and rolls back by \
                 itself when the new version does not start. Check later with `/health`.",
            )
            .await;
    }
    ix.edit_content(format!("Updating **{target}**…")).await?;
    let started = tokio::time::timeout(UPDATE_WAIT, systemctl(&["start", &unit])).await;
    let log = tokio::fs::read_to_string(format!("{UPDATE_LOG_DIR}/{target}.log"))
        .await
        .unwrap_or_default();
    let log = tail(log.trim(), LOG_TAIL_CHARS);
    let header = match started {
        Err(_) => {
            format!("**{target}**: still running after 13 minutes; it finishes in the background.")
        }
        Ok(Err(error)) => format!("**{target}**: could not run systemctl: {error}"),
        Ok(Ok(output)) if output.status.success() => {
            format!("✅ **{target}** updated (or already up to date).")
        }
        Ok(Ok(output)) => format!(
            "❌ **{target}** update failed{}",
            match String::from_utf8_lossy(&output.stderr).trim() {
                "" => ".".to_owned(),
                err => format!(": {err}"),
            }
        ),
    };
    let body = if log.is_empty() {
        header
    } else {
        format!("{header}\n```\n{log}\n```")
    };
    ix.edit_content(body).await
}

pub async fn execute_sync(ix: &Ix<'_>) -> Result<()> {
    if !owner_only(ix).await? {
        return Ok(());
    }
    ix.defer(true).await?;
    let report = super::sync_now(ix.app).await;
    ix.edit_content(super::format_report(&report).join("\n"))
        .await
}

fn servers_payload(ix: &Ix<'_>) -> serde_json::Value {
    let mut guilds: Vec<(Id<GuildMarker>, String, u64)> = ix
        .app
        .cache
        .iter()
        .guilds()
        .map(|g| (*g.key(), g.name().to_owned(), g.member_count().unwrap_or(0)))
        .collect();
    guilds.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.cmp(&b.1)));
    let mut lines = vec![format!("### Servers ({})", guilds.len())];
    lines.extend(
        guilds
            .iter()
            .take(40)
            .map(|(id, name, members)| format!("**{name}** · {members} members · `{id}`")),
    );
    if guilds.len() > 40 {
        lines.push(format!("… and {} more", guilds.len() - 40));
    }
    let user = ix.user_id();
    let options: Vec<serde_json::Value> = guilds
        .iter()
        .take(25)
        .map(|(id, name, members)| {
            json!({
                "label": name.chars().take(100).collect::<String>(),
                "value": id.to_string(),
                "description": format!("{members} members"),
            })
        })
        .collect();
    let mut components = vec![json!({ "type": 10, "content": lines.join("\n") })];
    if !options.is_empty() {
        components.push(json!({
            "type": 1,
            "components": [{
                "type": 3,
                "custom_id": format!("{SERVERS_PREFIX}:pick:{user}"),
                "placeholder": "Leave a server…",
                "options": options,
            }],
        }));
    }
    json!({
        "flags": crate::discord::FLAG_COMPONENTS_V2 | crate::discord::FLAG_EPHEMERAL,
        "allowed_mentions": { "parse": [] },
        "components": [{ "type": 17, "components": components }],
    })
}

pub async fn execute_servers(ix: &Ix<'_>) -> Result<()> {
    if !owner_only(ix).await? {
        return Ok(());
    }
    ix.reply(servers_payload(ix)).await
}

/// `/servers` select menu and the leave confirmation; `true` when the component was ours.
pub async fn handle_component(ix: &Ix<'_>) -> Result<bool> {
    let Some(custom_id) = ix.custom_id() else {
        return Ok(false);
    };
    let parts: Vec<&str> = custom_id.split(':').collect();
    if parts.first() != Some(&SERVERS_PREFIX) {
        return Ok(false);
    }
    if parts.last() != Some(&ix.user_id().to_string().as_str()) || !ix.app.is_owner(ix.user_id()) {
        ix.reply_ephemeral("This panel belongs to the bot owner.")
            .await?;
        return Ok(true);
    }
    let user = ix.user_id();
    match parts.get(1).copied() {
        Some("pick") => {
            let Some(guild) = ix
                .component_values()
                .first()
                .and_then(|v| crate::discord::parse_id::<GuildMarker>(v))
            else {
                return Ok(true);
            };
            let name = ix
                .app
                .guild_name(guild)
                .unwrap_or_else(|| guild.to_string());
            ix.update(json!({
                "flags": crate::discord::FLAG_COMPONENTS_V2,
                "components": [{
                    "type": 17,
                    "accent_color": 0xED4245,
                    "components": [
                        { "type": 10, "content": format!("Leave **{name}** (`{guild}`)? The bot loses its data access there.") },
                        { "type": 1, "components": [
                            { "type": 2, "style": 4, "label": "Leave", "custom_id": format!("{SERVERS_PREFIX}:leave:{guild}:{user}") },
                            { "type": 2, "style": 2, "label": "Cancel", "custom_id": format!("{SERVERS_PREFIX}:list:{user}") },
                        ]},
                    ],
                }],
            }))
            .await?;
        }
        Some("leave") => {
            let Some(guild) = parts
                .get(2)
                .and_then(|v| crate::discord::parse_id::<GuildMarker>(v))
            else {
                return Ok(true);
            };
            let name = ix
                .app
                .guild_name(guild)
                .unwrap_or_else(|| guild.to_string());
            let text = match ix.app.http.leave_guild(guild).await {
                Ok(_) => {
                    tracing::info!("Left guild {guild} ({name}) on owner request");
                    format!("Left **{name}**.")
                }
                Err(error) => format!("Could not leave **{name}**: {error}"),
            };
            ix.update(json!({
                "flags": crate::discord::FLAG_COMPONENTS_V2,
                "components": [{ "type": 17, "components": [{ "type": 10, "content": text }] }],
            }))
            .await?;
        }
        Some("list") => ix.update(servers_payload(ix)).await?,
        _ => {}
    }
    Ok(true)
}

pub async fn execute_presence(ix: &Ix<'_>) -> Result<()> {
    if !owner_only(ix).await? {
        return Ok(());
    }
    let opts = ix.opts();
    let kind = opts.string("type").unwrap_or_default();
    let text = opts.string("text").unwrap_or_default();
    let presence = if kind == "clear" || text.trim().is_empty() {
        Presence::default()
    } else if presence::KINDS.contains(&kind.as_str()) {
        Presence { kind, text }
    } else {
        Presence {
            kind: "playing".to_owned(),
            text,
        }
    };
    if let Err(error) = presence::save(&ix.app.paths.data_dir, &presence) {
        return ix
            .reply_ephemeral(format!("Could not save the status: {error}"))
            .await;
    }
    presence::apply(ix.app);
    let reply = if presence.kind.is_empty() {
        "Status cleared.".to_owned()
    } else {
        format!(
            "Status set: **{}** {}",
            presence.kind,
            presence
                .text
                .trim()
                .chars()
                .take(presence::MAX_TEXT)
                .collect::<String>()
        )
    };
    ix.reply_ephemeral(reply).await
}

pub async fn execute_restart(ix: &Ix<'_>) -> Result<()> {
    if !owner_only(ix).await? {
        return Ok(());
    }
    let force = ix.opts().boolean("force") == Some(true);
    if !force && let Some(reason) = busy_reason(ix).await {
        return ix
            .reply_ephemeral(format!(
                "Not restarting: {reason}. Use `force:true` to restart anyway."
            ))
            .await;
    }
    ix.reply_ephemeral("Restarting… The bot is back in a few seconds.")
        .await?;
    tracing::info!("Restart requested by {}", ix.user_id());
    // systemd (Restart=always) starts the bot again after the graceful shutdown.
    ix.app.restart.notify_one();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_tail_keeps_whole_lines_and_no_code_fences() {
        let log = "first line\nsecond ```x```\nthird";
        assert_eq!(tail(log, 100), "first line\nsecond '''x'''\nthird");
        assert_eq!(tail(log, 12), "third");
    }
}
