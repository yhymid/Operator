//! Slash command manifest, registration (`command-sync.js`) and interaction dispatch
//! (the `interactionCreate` handler of `main.js`).

pub mod automod;
pub mod cache;
pub mod clean;
pub mod diagnostics;
pub mod dm;
pub mod help;
pub mod logs;
pub mod music;
pub mod settings;
pub mod verify;
pub mod ytdlp;

use std::sync::OnceLock;

use anyhow::Result;
use futures_util::future::join_all;
use serde_json::Value;
use twilight_model::application::interaction::{Interaction, InteractionType};
use twilight_model::channel::message::component::ComponentType;
use twilight_model::id::Id;
use twilight_model::id::marker::ApplicationMarker;

use crate::app::SharedApp;
use crate::discord::{self, Ix};
use crate::modules::verification;
use crate::rest::RawRest;

/// Command definitions dumped from the JS manifest (`getApplicationCommandPayload()`).
const MANIFEST: &str = include_str!("../../commands.json");

pub fn payload() -> &'static Value {
    static PAYLOAD: OnceLock<Value> = OnceLock::new();
    PAYLOAD.get_or_init(|| serde_json::from_str(MANIFEST).expect("commands.json is valid JSON"))
}

fn payload_len() -> usize {
    payload().as_array().map_or(0, Vec::len)
}

// ---------------------------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------------------------

pub struct SyncDetail {
    pub guild_id: String,
    pub guild_name: String,
    pub error: Option<String>,
}

pub enum SyncReport {
    Guild {
        payload_count: usize,
        details: Vec<SyncDetail>,
    },
    Global {
        command_count: usize,
        cleared: bool,
    },
}

impl SyncReport {
    pub fn failures(&self) -> usize {
        match self {
            Self::Guild { details, .. } => details.iter().filter(|d| d.error.is_some()).count(),
            Self::Global { .. } => 0,
        }
    }
}

/// `syncGuildCommands`: PUT the manifest to every guild in parallel.
pub async fn sync_guilds(
    raw: &RawRest,
    application_id: Id<ApplicationMarker>,
    guilds: &[(String, String)],
    missing: &[String],
) -> SyncReport {
    let body = payload();
    let results = join_all(guilds.iter().map(|(id, _)| async move {
        let path = format!("/applications/{application_id}/guilds/{id}/commands");
        raw.put_json(&path, body).await
    }))
    .await;
    let mut details: Vec<SyncDetail> = guilds
        .iter()
        .zip(results)
        .map(|((id, name), result)| SyncDetail {
            guild_id: id.clone(),
            guild_name: name.clone(),
            error: result.err().map(|e| discord::err_text(&e)),
        })
        .collect();
    details.extend(missing.iter().map(|id| SyncDetail {
        guild_id: id.clone(),
        guild_name: "unknown".to_owned(),
        error: Some("The bot cannot see this server or is not a member of it.".to_owned()),
    }));
    SyncReport::Guild {
        payload_count: payload_len(),
        details,
    }
}

/// `syncGlobalCommands`
pub async fn sync_global(
    raw: &RawRest,
    application_id: Id<ApplicationMarker>,
    clear: bool,
) -> Result<SyncReport> {
    let empty = Value::Array(Vec::new());
    let body = if clear { &empty } else { payload() };
    raw.put_json(&format!("/applications/{application_id}/commands"), body)
        .await?;
    Ok(SyncReport::Global {
        command_count: body.as_array().map_or(0, Vec::len),
        cleared: clear,
    })
}

/// `formatCommandSyncReport`
pub fn format_report(report: &SyncReport) -> Vec<String> {
    match report {
        SyncReport::Global { cleared: true, .. } => {
            vec!["Global commands were cleared.".to_owned()]
        }
        SyncReport::Global { command_count, .. } => {
            vec![format!("Global commands synced: {command_count}.")]
        }
        SyncReport::Guild {
            payload_count,
            details,
        } => {
            let fail = report.failures();
            let mut lines = vec![format!(
                "Guild sync: commands={payload_count}, servers={}, ok={}, errors={fail}",
                details.len(),
                details.len() - fail
            )];
            for d in details {
                if let Some(error) = &d.error {
                    lines.push(format!("- {} ({}): {error}", d.guild_name, d.guild_id));
                }
            }
            lines
        }
    }
}

/// `registerSlashCommands` run from the Ready handler.
pub async fn register_on_startup(app: &SharedApp) {
    if !app.paths.sync_commands_on_startup {
        if app.startup_verbose {
            tracing::info!(
                "Skipping slash command sync on startup (OPERATOR_SYNC_COMMANDS_ON_STARTUP=0)."
            );
        }
        return;
    }
    let guilds: Vec<(String, String)> = app
        .guild_ids()
        .into_iter()
        .map(|id| {
            (
                id.to_string(),
                app.guild_name(id).unwrap_or_else(|| "unknown".to_owned()),
            )
        })
        .collect();
    let report = sync_guilds(&app.raw, app.application_id, &guilds, &[]).await;
    if report.failures() > 0 || app.startup_verbose {
        for line in format_report(&report) {
            tracing::info!("{line}");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------------------------

pub async fn dispatch(app: SharedApp, interaction: Interaction) {
    let ix = Ix::new(&app, &interaction);
    match interaction.kind {
        InteractionType::MessageComponent => {
            if let Err(error) = dispatch_component(&ix).await {
                tracing::error!("Component interaction error: {}", discord::err_text(&error));
                if !ix.replied() && !ix.deferred() {
                    let _ = ix
                        .reply_ephemeral("An error occurred while handling the panel.")
                        .await;
                }
            }
        }
        InteractionType::ModalSubmit => {
            if let Err(error) = verification::handle_modal(&ix).await {
                tracing::error!("Verify captcha modal error: {}", discord::err_text(&error));
                if !ix.replied() && !ix.deferred() {
                    let _ = ix
                        .reply_ephemeral("An error occurred while checking the captcha.")
                        .await;
                }
            }
        }
        InteractionType::ApplicationCommandAutocomplete => {
            // No command has autocomplete options; answer stale clients with an empty list.
            let _ = ix.autocomplete(Vec::new()).await;
        }
        InteractionType::ApplicationCommand => {
            if let Err(error) = dispatch_command(&ix).await {
                tracing::error!("Interaction error: {}", discord::err_text(&error));
                let text = "An error occurred while running the command.";
                let _ = if !ix.replied() && !ix.deferred() {
                    ix.reply_ephemeral(text).await
                } else {
                    ix.edit_content(text).await
                };
            }
        }
        _ => {}
    }
}

async fn dispatch_component(ix: &Ix<'_>) -> Result<()> {
    let kind = ix.component_kind();
    if !matches!(
        kind,
        Some(ComponentType::Button | ComponentType::TextSelectMenu)
    ) {
        return Ok(());
    }
    if help::handle_component(ix).await? {
        return Ok(());
    }
    if music::handle_component(ix).await? {
        return Ok(());
    }
    if kind == Some(ComponentType::Button) {
        verification::handle_button(ix).await?;
    }
    Ok(())
}

async fn dispatch_command(ix: &Ix<'_>) -> Result<()> {
    let Some(name) = ix.command().map(|c| c.name.as_str()) else {
        return Ok(());
    };
    match name {
        "clean" => clean::execute(ix).await,
        "help" => help::execute(ix).await,
        "cache" => cache::execute(ix).await,
        "verify" => verify::execute(ix).await,
        "automod" => automod::execute(ix).await,
        "settings" => settings::execute(ix).await,
        "logs" => logs::execute(ix).await,
        "health" => diagnostics::execute_health(ix).await,
        "debug" => diagnostics::execute_debug(ix).await,
        "dm" => dm::execute(ix).await,
        "ytdlp" => ytdlp::execute(ix).await,
        music if music::COMMANDS.contains(&music) => music::execute(ix, music).await,
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every music command in the manifest is routed to `music::execute`, and nothing else is.
    #[test]
    fn music_commands_are_dispatched() {
        let names: Vec<&str> = payload()
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        for command in music::COMMANDS {
            assert!(
                names.contains(&command),
                "{command} missing from commands.json"
            );
        }
        assert!(
            !names.contains(&"music"),
            "/music was replaced by standalone commands"
        );
    }

    #[test]
    fn manifest_matches_js() {
        let names: Vec<&str> = payload()
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "help",
                "settings",
                "clean",
                "cache",
                "play",
                "pause",
                "resume",
                "skip",
                "stop",
                "queue",
                "nowplaying",
                "volume",
                "remove",
                "move",
                "seek",
                "clear",
                "loop",
                "reconnect",
                "verify",
                "automod",
                "logs",
                "health",
                "debug",
                "dm",
                "ytdlp"
            ]
        );
    }

    #[test]
    fn report_formatting() {
        let report = SyncReport::Guild {
            payload_count: 12,
            details: vec![
                SyncDetail {
                    guild_id: "1".into(),
                    guild_name: "A".into(),
                    error: None,
                },
                SyncDetail {
                    guild_id: "2".into(),
                    guild_name: "B".into(),
                    error: Some("x".into()),
                },
            ],
        };
        assert_eq!(
            format_report(&report),
            [
                "Guild sync: commands=12, servers=2, ok=1, errors=1",
                "- B (2): x"
            ]
        );
    }
}
