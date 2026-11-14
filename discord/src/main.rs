//! Operator Discord bot — Rust rewrite of `main.js`.
//!
//! Subcommands (the JS npm scripts):
//! - `operator-discord` / `operator-discord run` — start the bot (`npm start`)
//! - `operator-discord commands sync [--guild=ID[,ID]] [--global] [--clear-global]`
//!   (`npm run commands:sync*`)
//! - `operator-discord doctor` (`npm run doctor`)

mod app;
mod commands;
mod config;
mod discord;
mod lavalink;
mod modules;
mod owners;
mod paths;
mod rest;
mod state;
mod util;

use std::collections::HashSet;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use operator_common::env;
use tokio::task::JoinHandle;
use twilight_cache_inmemory::DefaultInMemoryCache;
use twilight_gateway::{CloseFrame, Event, EventTypeFlags, Intents, Shard, ShardId, StreamExt};
use twilight_http::Client as HttpClient;
use twilight_model::id::Id;
use twilight_model::id::marker::GuildMarker;
use twilight_model::user::CurrentUser;

use crate::app::{App, SharedApp};
use crate::modules::announcements::Announcements;
use crate::modules::autoclean::AutoClean;
use crate::modules::automod::AutoMod;
use crate::modules::fulllogs::{self, FullLogs};
use crate::modules::music::MusicManager;
use crate::modules::verification;
use crate::paths::OperatorPaths;
use crate::rest::RawRest;
use crate::state::StateStore;

const ANNOUNCEMENT_SWEEP_INTERVAL: Duration = Duration::from_secs(30);
/// discord.js waits for the initial GUILD_CREATE burst before emitting `clientReady`.
const READY_GUILD_WAIT: Duration = Duration::from_secs(15);

fn main() -> ExitCode {
    env::load_dotenv();
    // Several crates (twilight, tokio-websockets, hyper-rustls) share one rustls provider.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("Nie mozna uruchomic runtime tokio: {error}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(async {
        match args.first().map(String::as_str) {
            None | Some("run" | "start") => run().await,
            Some("commands") if args.get(1).map(String::as_str) == Some("sync") => {
                commands_sync(&args[2..]).await
            }
            Some("doctor") => Ok(doctor()),
            Some("--help" | "-h" | "help") => {
                print_usage();
                Ok(ExitCode::SUCCESS)
            }
            Some(other) => {
                eprintln!("Nieznana komenda: {other}\n");
                print_usage();
                Ok(ExitCode::FAILURE)
            }
        }
    });
    // Background tasks (Lavalink reconnect loop etc.) must not keep the process alive.
    runtime.shutdown_timeout(Duration::from_secs(2));
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

fn print_usage() {
    println!(
        "Uzycie:
  operator-discord [run]
  operator-discord commands sync
  operator-discord commands sync --guild=123456789012345678
  operator-discord commands sync --guild=123,456
  operator-discord commands sync --global
  operator-discord commands sync --clear-global
  operator-discord doctor"
    );
}

fn bot_token() -> Result<String> {
    env::var("BOT_TOKEN").context("Brak BOT_TOKEN w env.")
}

fn user_tag(user: &CurrentUser) -> String {
    if user.discriminator == 0 {
        user.name.clone()
    } else {
        format!("{}#{:04}", user.name, user.discriminator)
    }
}

fn intents() -> Intents {
    Intents::GUILDS
        | Intents::GUILD_MESSAGES
        | Intents::MESSAGE_CONTENT
        | Intents::GUILD_MEMBERS
        | Intents::GUILD_VOICE_STATES
        | Intents::GUILD_MODERATION
        | Intents::GUILD_MESSAGE_REACTIONS
        | Intents::GUILD_EMOJIS_AND_STICKERS
        | Intents::GUILD_INVITES
        | Intents::GUILD_WEBHOOKS
        | Intents::AUTO_MODERATION_CONFIGURATION
        | Intents::AUTO_MODERATION_EXECUTION
}

// ---------------------------------------------------------------------------------------------
// doctor (scripts/preflight.js)
// ---------------------------------------------------------------------------------------------

fn doctor() -> ExitCode {
    let paths = OperatorPaths::from_env();
    let mut warnings = Vec::new();
    let mut errors = Vec::new();
    if env::var("BOT_TOKEN").is_none() {
        errors.push("Brak BOT_TOKEN w env.".to_owned());
    }
    if let Err(error) = config::read(&paths.config_path) {
        errors.push(config::format_config_error(&error, "Config: "));
    }
    let user_dir = paths.data_dir.join("user");
    let dirs = [
        &paths.data_dir,
        &paths.logs_dir,
        &paths.server_cache_dir,
        &paths.music_queue_dir,
        &user_dir,
        &paths.server_logs_dir,
    ];
    for dir in dirs {
        let check = std::fs::create_dir_all(dir).and_then(|()| {
            let probe = dir.join(".operator-write-test");
            std::fs::write(&probe, b"")?;
            std::fs::remove_file(probe)
        });
        if let Err(error) = check {
            errors.push(format!(
                "Katalogi runtime nie sa gotowe do zapisu: {} ({error})",
                dir.display()
            ));
            break;
        }
    }
    let owners = owners::resolve(&paths.config_path);
    if owners.ids.is_empty() {
        warnings.push("Brak OWNER_IDS. Komendy owner-only beda niedostepne.".to_owned());
    }
    if !owners.invalid.is_empty() {
        warnings.push(format!(
            "Pominiete niepoprawne OWNER_IDS: {}",
            owners.invalid.join(", ")
        ));
    }
    let mut lines = vec![
        "Operator preflight".to_owned(),
        format!(
            "- Runtime: Rust (operator-discord {})",
            env!("CARGO_PKG_VERSION")
        ),
        format!("- Config: {}", paths.config_path.display()),
        format!("- Data dir: {}", paths.data_dir.display()),
        format!("- Logs dir: {}", paths.logs_dir.display()),
        format!(
            "- Sync komend na starcie: {}",
            if paths.sync_commands_on_startup {
                "tak"
            } else {
                "nie"
            }
        ),
    ];
    for (title, items) in [("Ostrzezenia:", &warnings), ("Bledy:", &errors)] {
        if !items.is_empty() {
            lines.push(String::new());
            lines.push(title.to_owned());
            lines.extend(items.iter().map(|i| format!("- {i}")));
        }
    }
    println!("{}", lines.join("\n"));
    if errors.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

// ---------------------------------------------------------------------------------------------
// commands sync (scripts/register-commands.js) — REST only, no gateway login needed
// ---------------------------------------------------------------------------------------------

async fn commands_sync(args: &[String]) -> Result<ExitCode> {
    operator_common::logging::init(false);
    let mut guild_ids: Vec<String> = Vec::new();
    let (mut global, mut clear_global) = (false, false);
    for arg in args {
        match arg.as_str() {
            "--global" => global = true,
            "--clear-global" => clear_global = true,
            "--help" | "-h" => {
                print_usage();
                return Ok(ExitCode::SUCCESS);
            }
            other => {
                if let Some(raw) = other.strip_prefix("--guild=") {
                    for id in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                        if !guild_ids.iter().any(|g| g == id) {
                            guild_ids.push(id.to_owned());
                        }
                    }
                }
            }
        }
    }
    let token = bot_token()?;
    let http = HttpClient::new(token.clone());
    let raw = RawRest::new(&token)?;
    let application_id = http
        .current_user_application()
        .await
        .context("Sync komend nieudany")?
        .model()
        .await?
        .id;

    let report = if clear_global || global {
        commands::sync_global(&raw, application_id, clear_global)
            .await
            .context("Sync komend nieudany")?
    } else {
        let mut guilds = Vec::new();
        let mut missing = Vec::new();
        if guild_ids.is_empty() {
            let mut after = None;
            loop {
                let mut request = http.current_user_guilds().limit(200);
                if let Some(after) = after {
                    request = request.after(after);
                }
                let page = request.await?.models().await?;
                let len = page.len();
                after = page.last().map(|g| g.id);
                guilds.extend(page.into_iter().map(|g| (g.id.to_string(), g.name)));
                if len < 200 {
                    break;
                }
            }
        } else {
            for id in guild_ids {
                let fetched = match discord::parse_id::<GuildMarker>(&id) {
                    Some(gid) => match http.guild(gid).await {
                        Ok(response) => response.model().await.ok(),
                        Err(_) => None,
                    },
                    None => None,
                };
                match fetched {
                    Some(guild) => guilds.push((id, guild.name)),
                    None => missing.push(id),
                }
            }
        }
        commands::sync_guilds(&raw, application_id, &guilds, &missing).await
    };
    println!("{}", commands::format_report(&report).join("\n"));
    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------------------------
// run (main.js)
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct Runners {
    verification: Option<JoinHandle<()>>,
    announcements: Option<JoinHandle<()>>,
}

async fn run() -> Result<ExitCode> {
    operator_common::logging::init(false);
    let paths = OperatorPaths::from_env();
    let root_config = match config::read(&paths.config_path) {
        Ok(cfg) => cfg,
        Err(error) => {
            tracing::error!(
                "{}",
                config::format_config_error(&error, "Startup aborted. ")
            );
            return Ok(ExitCode::FAILURE);
        }
    };
    let startup_verbose = config::is_true(&root_config, &["startupVerboseLogs"]);

    let token = bot_token()?;
    let http = Arc::new(HttpClient::new(token.clone()));
    let current_user = http.current_user().await?.model().await?;
    let application_id = http.current_user_application().await?.model().await?.id;
    let state = StateStore::new(&paths.server_cache_dir)?;

    let mut shard = Shard::new(ShardId::ONE, token.clone(), intents());
    let app: SharedApp = Arc::new(App {
        raw: RawRest::new(&token)?,
        http,
        cache: DefaultInMemoryCache::default(),
        application_id,
        bot_id: current_user.id,
        bot_tag: user_tag(&current_user),
        state,
        sender: shard.sender(),
        startup_verbose,
        autoclean: AutoClean::new(root_config.clone()),
        automod: AutoMod::new(&root_config),
        fulllogs: FullLogs::new(&paths.server_logs_dir),
        music: MusicManager::new(&paths.music_queue_dir),
        announcements: Announcements::new(),
        shutting_down: AtomicBool::new(false),
        paths,
    });
    let runners = Arc::new(Mutex::new(Runners::default()));
    let started = Arc::new(AtomicBool::new(false));

    let shutdown = operator_common::shutdown::signal();
    tokio::pin!(shutdown);
    let reason = loop {
        let item = tokio::select! {
            reason = &mut shutdown => break reason,
            item = shard.next_event(EventTypeFlags::all()) => item,
        };
        let Some(item) = item else {
            break "gateway closed";
        };
        let event = match item {
            Ok(event) => event,
            Err(error) => {
                tracing::error!("Discord error: {error}");
                continue;
            }
        };
        // Old state for the audit logs must be captured before the cache applies the event.
        let before = fulllogs::capture_before(&app.cache, &event);
        app.cache.update(&event);

        if let Event::Ready(ready) = &event
            && !started.swap(true, Ordering::SeqCst)
        {
            let expected: HashSet<Id<GuildMarker>> = ready.guilds.iter().map(|g| g.id).collect();
            tokio::spawn(on_ready(app.clone(), runners.clone(), expected));
        }
        tokio::spawn(handle_event(app.clone(), started.clone(), event, before));
    };

    graceful_shutdown(&app, &runners, reason).await;
    shard.close(CloseFrame::NORMAL);
    Ok(ExitCode::SUCCESS)
}

async fn handle_event(
    app: SharedApp,
    started: Arc<AtomicBool>,
    event: Event,
    before: fulllogs::Before,
) {
    if app.shutting_down.load(Ordering::SeqCst) {
        return;
    }
    app.fulllogs.handle_event(&app, &event, before).await;
    match event {
        Event::MessageCreate(message) => {
            if !message.author.bot {
                app.automod.handle_message(&app, &message).await;
            }
        }
        Event::VoiceStateUpdate(update) => app.music.handle_voice_state(&app, &update).await,
        Event::VoiceServerUpdate(update) => app.music.handle_voice_server(&update).await,
        // discord.js only emits guildCreate for guilds joined after clientReady.
        Event::GuildCreate(guild) if started.load(Ordering::SeqCst) => {
            let name = app.guild_name(guild.id());
            app.state
                .mark_guild_online(&guild.id().to_string(), name.as_deref(), "guildCreate");
        }
        // `unavailable: true` is an outage (guildUnavailable in discord.js), not a removal.
        Event::GuildDelete(guild) if guild.unavailable != Some(true) => {
            app.music.stop(&app, guild.id).await;
            app.state
                .mark_guild_offline(&guild.id.to_string(), None, "guildDelete");
        }
        Event::InteractionCreate(interaction) => {
            commands::dispatch(app.clone(), (*interaction).0).await;
        }
        _ => {}
    }
}

fn mark_all_guilds(app: &App, online: bool, reason: &str, silent: bool) {
    let ids = app.guild_ids();
    for id in &ids {
        let name = app.guild_name(*id);
        if online {
            app.state
                .mark_guild_online(&id.to_string(), name.as_deref(), reason);
        } else {
            app.state
                .mark_guild_offline(&id.to_string(), name.as_deref(), reason);
        }
    }
    if !silent {
        tracing::info!(
            "Zapisano status \"{}\" dla {} serwerow.",
            if online { "online" } else { "offline" },
            ids.len()
        );
    }
}

/// `client.once('clientReady')`
async fn on_ready(
    app: SharedApp,
    runners: Arc<Mutex<Runners>>,
    expected: HashSet<Id<GuildMarker>>,
) {
    let deadline = tokio::time::Instant::now() + READY_GUILD_WAIT;
    while tokio::time::Instant::now() < deadline
        && expected.iter().any(|id| app.cache.guild(*id).is_none())
    {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    tracing::info!("Bot zalogowany jako {}", app.bot_tag);
    if app.startup_verbose {
        tracing::info!("Serwery: {}", app.guild_count());
    }
    let owners = owners::resolve(&app.paths.config_path);
    if owners.ids.is_empty() {
        tracing::warn!(
            "Brak OWNER_IDS w env ani ownerIds w config.json. Komendy owner-only beda niedostepne."
        );
    } else if app.startup_verbose {
        tracing::info!(
            "Owner access loaded from {}. IDs: {}",
            owners.source,
            owners.ids.len()
        );
    }
    if !owners.invalid.is_empty() {
        tracing::warn!(
            "Pominieto niepoprawne OWNER_IDS: {}",
            owners.invalid.join(", ")
        );
    }

    app.state.migrate_legacy_server_cache();
    commands::ytdlp::cleanup_expired_media().await;
    mark_all_guilds(&app, true, "startup", !app.startup_verbose);
    commands::register_on_startup(&app).await;
    if let Err(error) = app.autoclean.start(&app) {
        tracing::error!("AutoClean start failed: {error}");
    }
    if let Err(error) = app.automod.start(&app) {
        tracing::error!("AutoMod start failed: {error}");
    }
    app.fulllogs.start(&app);
    app.music.initialize(&app);
    verification::cleanup_challenges_on_startup(&app);
    let verification_runner = verification::start_sweep_runner(&app);
    verification::run_sweep(&app, "startup", true).await;
    app.announcements.run_sweep(&app).await;
    let sweep_app = app.clone();
    let announcement_runner = tokio::spawn(async move {
        let mut interval = tokio::time::interval(ANNOUNCEMENT_SWEEP_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            sweep_app.announcements.run_sweep(&sweep_app).await;
        }
    });
    {
        let mut guard = runners.lock().unwrap_or_else(|p| p.into_inner());
        guard.verification = Some(verification_runner);
        guard.announcements = Some(announcement_runner);
    }
    tracing::info!(
        "Start zakonczony. Serwery: {}. Nastepne czyszczenie: {}",
        app.guild_count(),
        app.autoclean.next_run_text()
    );
}

/// `gracefulShutdown`
async fn graceful_shutdown(app: &SharedApp, runners: &Mutex<Runners>, reason: &str) {
    if app.shutting_down.swap(true, Ordering::SeqCst) {
        return;
    }
    tracing::info!("Wylaczanie bota ({reason})...");
    {
        let mut guard = runners.lock().unwrap_or_else(|p| p.into_inner());
        for handle in [guard.verification.take(), guard.announcements.take()]
            .into_iter()
            .flatten()
        {
            handle.abort();
        }
    }
    app.music.shutdown(app).await;
    app.fulllogs.stop().await;
    mark_all_guilds(app, false, reason, false);
    app.autoclean.stop();
}
