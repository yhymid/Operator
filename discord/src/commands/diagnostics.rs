//! `/health` and `/debug` (port of `src/cmds/diagnostics.js`).

use anyhow::Result;
use serde_json::{Value, json};
use twilight_model::channel::ChannelType;
use twilight_model::guild::Permissions;
use twilight_model::id::Id;
use twilight_model::id::marker::{ChannelMarker, GuildMarker};

use crate::app::App;
use crate::discord::{self, FLAG_COMPONENTS_V2, FLAG_EPHEMERAL, Ix};
use crate::modules::fulllogs::resolve_logs_state;
use crate::modules::music;
use crate::owners;
use crate::util::text::{inline, truncate};

type Checks = &'static [(Permissions, &'static str)];

const GUILD_CHECKS: Checks = &[
    (Permissions::VIEW_AUDIT_LOG, "View Audit Log"),
    (Permissions::MANAGE_ROLES, "Manage Roles"),
    (Permissions::MODERATE_MEMBERS, "Moderate Members"),
];
const TEXT_CHECKS: Checks = &[
    (Permissions::VIEW_CHANNEL, "View Channel"),
    (Permissions::SEND_MESSAGES, "Send Messages"),
    (Permissions::READ_MESSAGE_HISTORY, "Read Message History"),
];
const VOICE_CHECKS: Checks = &[
    (Permissions::VIEW_CHANNEL, "View Channel"),
    (Permissions::CONNECT, "Connect"),
    (Permissions::SPEAK, "Speak"),
];

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Severity {
    Ok,
    Warning,
    Critical,
}

fn badge(level: Severity) -> &'static str {
    match level {
        Severity::Critical => "❌ Critical",
        Severity::Warning => "⚠️ Warning",
        Severity::Ok => "✅ OK",
    }
}

struct Issue(Severity, String);

fn issue(severity: Severity, message: impl AsRef<str>) -> Issue {
    Issue(severity, inline(message.as_ref(), 220))
}

fn overall(issues: &[Issue]) -> Severity {
    issues.iter().map(|i| i.0).max().unwrap_or(Severity::Ok)
}

fn issue_lines(issues: &[Issue]) -> Vec<String> {
    if issues.is_empty() {
        return vec!["No red flags.".to_owned()];
    }
    issues
        .iter()
        .map(|i| format!("{} {}", badge(i.0), i.1))
        .collect()
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

/// Permission snapshot: `None` = unavailable (with reason).
struct PermSnapshot {
    available: bool,
    ok: Vec<&'static str>,
    missing: Vec<&'static str>,
    reason: &'static str,
}

fn check(perms: Option<Permissions>, checks: Checks, reason: &'static str) -> PermSnapshot {
    match perms {
        None => PermSnapshot {
            available: false,
            ok: Vec::new(),
            missing: checks.iter().map(|c| c.1).collect(),
            reason,
        },
        Some(perms) => {
            let (ok, missing): (Vec<&(Permissions, &str)>, Vec<_>) = checks
                .iter()
                .partition(|entry| discord::has(perms, entry.0));
            PermSnapshot {
                available: true,
                ok: ok.into_iter().map(|c| c.1).collect(),
                missing: missing.into_iter().map(|c| c.1).collect(),
                reason,
            }
        }
    }
}

fn channel_snapshot(app: &App, channel: Option<Id<ChannelMarker>>, checks: Checks) -> PermSnapshot {
    let Some(channel) = channel.filter(|c| app.cache.channel(*c).is_some()) else {
        return check(None, checks, "channel unavailable");
    };
    let guild = app.cache.channel(channel).and_then(|c| c.guild_id);
    if guild.is_none_or(|g| app.cache.member(g, app.bot_id).is_none()) {
        return check(None, checks, "bot member unavailable");
    }
    check(
        discord::bot_channel_permissions(app, channel),
        checks,
        "cannot read permissions",
    )
}

fn render(snapshot: Option<&PermSnapshot>) -> String {
    let Some(s) = snapshot else {
        return "no data".to_owned();
    };
    if !s.available {
        return s.reason.to_owned();
    }
    if s.missing.is_empty() {
        let ok = if s.ok.is_empty() {
            "required permissions present".to_owned()
        } else {
            s.ok.join(", ")
        };
        return format!("OK ({ok})");
    }
    format!("Missing: {}", s.missing.join(", "))
}

fn mention(channel: Option<Id<ChannelMarker>>, fallback: &str) -> String {
    channel.map_or_else(|| fallback.to_owned(), |c| format!("<#{c}>"))
}

fn build_panel(title: &str, sections: &[(&str, Vec<String>)]) -> Value {
    let mut components = vec![json!({ "type": 10, "content": format!("## {title}") })];
    for (section_title, lines) in sections {
        let mut content = vec![format!("### {section_title}")];
        content.extend(
            lines
                .iter()
                .filter(|l| !l.is_empty())
                .map(|l| inline(l, 1800)),
        );
        components.push(json!({ "type": 10, "content": content.join("\n") }));
    }
    json!({
        "flags": FLAG_COMPONENTS_V2 | FLAG_EPHEMERAL,
        "allowed_mentions": { "parse": [] },
        "components": [{ "type": 17, "components": components }],
    })
}

struct MusicSnapshot {
    diagnostics: music::Diagnostics,
    issues: Vec<Issue>,
    lines: Vec<String>,
}

async fn inspect_music(app: &App, guild_id: Option<Id<GuildMarker>>) -> MusicSnapshot {
    let d = app.music.diagnostics(guild_id).await;
    let connected_nodes = usize::from(d.node.as_ref().is_some_and(|n| n.connected));
    let mut issues = Vec::new();
    if !d.initialized || !d.usable || connected_nodes == 0 {
        issues.push(issue(
            Severity::Critical,
            "Lavalink is offline. /play will not work until the node is back up.",
        ));
    }
    // The default password is only a problem when it does not match: then Lavalink refuses
    // the connection. A connected node already proves it is right.
    if !d.has_explicit_password && connected_nodes == 0 {
        issues.push(issue(
            Severity::Warning,
            "LAVALINK_PASSWORD is not set; the default password is used. Check that it matches application.yml.",
        ));
    }
    if let Some(session) = &d.guild_session
        && session.voice_channel_id.is_some()
        && !session.player_connected
    {
        issues.push(issue(
            Severity::Warning,
            "The bot has an active music session, but the player does not look connected to voice.",
        ));
    }
    if !d.queue_state_dir_exists {
        issues.push(issue(Severity::Warning, "The queue cache directory does not exist. Queue restore cannot save state until it is created."));
    }
    let session = d.guild_session.as_ref();
    let voice_summary = match session.and_then(|s| s.voice_channel_id.map(|c| (s, c))) {
        Some((s, channel)) => format!(
            "<#{channel}> | {}",
            if s.playing {
                "playing"
            } else if s.paused {
                "paused"
            } else {
                "idle"
            }
        ),
        None => "the bot is not in a voice channel right now".to_owned(),
    };
    let total_nodes = usize::from(d.node.is_some() || d.configured_node.is_some());
    let nodes_line = match &d.node {
        Some(n) => format!(
            "Nodes: {} {}:{} | {} | players {}/{} | {}",
            n.id,
            n.host,
            n.port,
            if n.connected { "online" } else { "offline" },
            n.playing_players,
            n.players,
            if n.cpu_load > 0.0 {
                format!("{}% cpu", (n.cpu_load * 100.0).round())
            } else {
                "cpu ?".to_owned()
            }
        ),
        None => "Nodes: none".to_owned(),
    };
    let lines = vec![
        format!(
            "Lavalink: {} ({connected_nodes}/{total_nodes} nodes connected)",
            if d.usable { "online" } else { "offline" }
        ),
        format!(
            "Search source: {} | Volume: {}%",
            d.default_search_source, d.default_volume
        ),
        format!(
            "Recovery: {}",
            if d.recovery_sources.is_empty() {
                "none".to_owned()
            } else {
                d.recovery_sources.join(", ")
            }
        ),
        format!("Voice: {voice_summary}"),
        match session.and_then(|s| s.current_track_title.as_ref().map(|t| (s, t))) {
            Some((s, title)) => format!(
                "Current track: {} | queue: {}",
                inline(title, 120),
                s.queue_size
            ),
            None => "Current track: none".to_owned(),
        },
        format!(
            "Channel voice status: {}",
            session
                .and_then(|s| s
                    .status_text
                    .clone()
                    .or_else(|| s.cached_status_text.clone()))
                .unwrap_or_else(|| "none".to_owned())
        ),
        format!(
            "Saved queue: {}",
            yes_no(session.is_some_and(|s| s.has_saved_queue))
        ),
        format!(
            "Config source: {}",
            if d.uses_url {
                "LAVALINK_URL"
            } else {
                "HOST/PORT env or fallback"
            }
        ),
        format!(
            "Config vars: host={} | port={} | password={} | secure={}",
            if d.has_explicit_host {
                "set"
            } else {
                "default"
            },
            if d.has_explicit_port {
                "set"
            } else {
                "default"
            },
            if d.has_explicit_password {
                "set"
            } else {
                "default"
            },
            if d.has_explicit_secure {
                "set"
            } else {
                "default"
            }
        ),
        nodes_line,
    ];
    MusicSnapshot {
        diagnostics: d,
        issues,
        lines,
    }
}

struct Section {
    issues: Vec<Issue>,
    lines: Vec<String>,
}

fn inspect_logs(app: &App, guild_id: Id<GuildMarker>) -> Section {
    let d = app.fulllogs.diagnostics(app, Some(guild_id));
    let state = resolve_logs_state(app, &guild_id.to_string());
    let channel = state
        .channel_id
        .as_deref()
        .and_then(discord::parse_id::<ChannelMarker>);
    let channel_kind = channel.and_then(|c| app.cache.channel(c).map(|ch| ch.kind));
    let perms = state
        .channel_id
        .as_ref()
        .map(|_| channel_snapshot(app, channel.filter(|_| channel_kind.is_some()), TEXT_CHECKS));
    let mut issues = Vec::new();
    if !d.started {
        issues.push(issue(
            Severity::Critical,
            "The FullLogs module is not running.",
        ));
    }
    if !state.enabled {
        issues.push(issue(
            Severity::Warning,
            "Logs are disabled on this server.",
        ));
    }
    if state.enabled && state.channel_id.is_none() {
        issues.push(issue(
            Severity::Critical,
            "Logs are enabled, but no log channel is set.",
        ));
    }
    if state.enabled && state.channel_id.is_some() && channel_kind.is_none() {
        issues.push(issue(
            Severity::Critical,
            "The configured log channel does not exist or the bot cannot fetch it.",
        ));
    }
    if state.enabled && channel_kind.is_some_and(|k| !discord::is_text_based(k)) {
        issues.push(issue(
            Severity::Critical,
            "The log channel exists but does not support text messages.",
        ));
    }
    if state.enabled
        && let Some(p) = perms.as_ref().filter(|p| !p.missing.is_empty())
    {
        issues.push(issue(
            Severity::Critical,
            format!(
                "The bot cannot send logs to {}. Missing: {}.",
                mention(channel.filter(|_| channel_kind.is_some()), "channel"),
                p.missing.join(", ")
            ),
        ));
    }
    if state.enabled && d.can_view_audit_log == Some(false) {
        issues.push(issue(
            Severity::Warning,
            "Missing View Audit Log. The actor in moderation logs will often be unknown.",
        ));
    }
    if !d.logs_dir_exists {
        issues.push(issue(Severity::Warning, "The log file directory does not exist. It is created on the first write, or disk access is missing."));
    }
    let lines = vec![
        format!("Runtime: {}", if d.started { "running" } else { "stopped" }),
        format!(
            "State: {} | channel: {}",
            if state.enabled { "enabled" } else { "disabled" },
            state
                .channel_id
                .as_deref()
                .map_or("none".to_owned(), |c| format!("<#{c}>"))
        ),
        if state.channel_id.is_some() {
            format!("Log channel perms: {}", render(perms.as_ref()))
        } else {
            "Log channel perms: none (channel not set)".to_owned()
        },
        format!(
            "Audit log access: {}",
            yes_no(d.can_view_audit_log == Some(true))
        ),
        format!(
            "Retention: {} days | Include bots: {} | Forward: {}",
            state.retention_days,
            yes_no(state.include_bots),
            if state.forward_mode.is_empty() {
                "all"
            } else {
                &state.forward_mode
            }
        ),
        format!(
            "Queues: delivery={} | write={} | buffered={}",
            d.delivery_queue_size, d.write_queue_size, d.buffered_writes
        ),
        format!(
            "Audit cache: cached={} | inflight={}",
            d.audit_cache_entries, d.audit_fetch_inflight
        ),
        format!(
            "Disk: {} | {}",
            if d.logs_dir_exists {
                "OK"
            } else {
                "no directory"
            },
            d.logs_dir.display()
        ),
        format!(
            "Retention worker: timer={} | active sweep={}",
            yes_no(d.retention_timer_active),
            yes_no(d.retention_sweep_active)
        ),
    ];
    Section { issues, lines }
}

fn inspect_permissions(
    ix: &Ix<'_>,
    music: &music::Diagnostics,
    picked: Option<(Id<ChannelMarker>, ChannelType)>,
) -> Section {
    let app = ix.app;
    let Some(guild_id) = ix.guild_id() else {
        return Section {
            issues: Vec::new(),
            lines: Vec::new(),
        };
    };
    let state = resolve_logs_state(app, &guild_id.to_string());
    let target = picked.or_else(|| ix.interaction.channel.as_ref().map(|c| (c.id, c.kind)));
    let bot_present = app.cache.member(guild_id, app.bot_id).is_some();
    let guild_snapshot = check(
        discord::bot_root_permissions(app, guild_id).filter(|_| bot_present),
        GUILD_CHECKS,
        "cannot read permissions",
    );
    let target_checks = if target.is_some_and(|(_, k)| discord::is_voice_based(k)) {
        VOICE_CHECKS
    } else {
        TEXT_CHECKS
    };
    let target_snapshot = channel_snapshot(app, target.map(|t| t.0), target_checks);

    let voice = picked
        .filter(|(_, k)| discord::is_voice_based(*k))
        .map(|(id, _)| id)
        .or_else(|| {
            music
                .guild_session
                .as_ref()
                .and_then(|s| s.voice_channel_id)
        })
        .or_else(|| {
            app.cache
                .voice_state(ix.user_id(), guild_id)
                .map(|v| v.channel_id())
        });
    let voice_snapshot = voice.map(|v| channel_snapshot(app, Some(v), VOICE_CHECKS));
    let log_channel = state
        .channel_id
        .as_deref()
        .and_then(discord::parse_id::<ChannelMarker>);
    let log_snapshot = state
        .channel_id
        .as_ref()
        .map(|_| channel_snapshot(app, log_channel, TEXT_CHECKS));

    let target_label = mention(target.map(|t| t.0), "current channel");
    let log_label = state
        .channel_id
        .as_deref()
        .map_or("channel".to_owned(), |c| format!("<#{c}>"));
    let mut issues = Vec::new();
    if !target_snapshot.missing.is_empty() {
        issues.push(issue(
            Severity::Warning,
            format!(
                "Channel {target_label} is missing: {}.",
                target_snapshot.missing.join(", ")
            ),
        ));
    }
    if state.enabled
        && let Some(s) = log_snapshot.as_ref().filter(|s| !s.missing.is_empty())
    {
        issues.push(issue(
            Severity::Critical,
            format!(
                "The log channel {log_label} is missing: {}.",
                s.missing.join(", ")
            ),
        ));
    }
    if let (Some(v), Some(s)) = (voice, voice_snapshot.as_ref())
        && !s.missing.is_empty()
    {
        issues.push(issue(
            Severity::Critical,
            format!("Voice <#{v}> is missing: {}.", s.missing.join(", ")),
        ));
    }
    if state.enabled && guild_snapshot.missing.contains(&"View Audit Log") {
        issues.push(issue(
            Severity::Warning,
            "View Audit Log is missing server-wide, so moderation logs will be incomplete.",
        ));
    }
    if guild_snapshot.missing.contains(&"Manage Roles") {
        issues.push(issue(
            Severity::Warning,
            "Manage Roles is missing server-wide, so verify and role automation will be limited.",
        ));
    }
    let lines = vec![
        format!("Guild-wide: {}", render(Some(&guild_snapshot))),
        format!(
            "Diagnostics channel {target_label}: {}",
            render(Some(&target_snapshot))
        ),
        if state.channel_id.is_some() {
            format!("Log channel {log_label}: {}", render(log_snapshot.as_ref()))
        } else {
            "Log channel: not configured".to_owned()
        },
        match voice {
            Some(v) => format!("Voice <#{v}>: {}", render(voice_snapshot.as_ref())),
            None => "Voice: no active channel to check".to_owned(),
        },
    ];
    Section { issues, lines }
}

fn runtime_section(app: &App) -> Section {
    let owners = owners::resolve(&app.paths.config_path);
    let paths = &app.paths;
    let lines = vec![
        "Discord gateway: online".to_owned(),
        format!(
            "Runtime: Rust (operator-discord {})",
            env!("CARGO_PKG_VERSION")
        ),
        "twilight: 0.18 | Lavalink: built-in v4 client".to_owned(),
        format!(
            "Owner access: {}",
            if owners.ids.is_empty() {
                "owner IDs not configured".to_owned()
            } else {
                format!("{} IDs from {}", owners.ids.len(), owners.source)
            }
        ),
        format!("Config: {}", paths.config_path.display()),
        format!("Data dir: {}", paths.data_dir.display()),
        format!("Logs dir: {}", paths.logs_dir.display()),
        format!(
            "Sync commands on startup: {}",
            yes_no(paths.sync_commands_on_startup)
        ),
    ];
    let mut issues = Vec::new();
    if owners.ids.is_empty() {
        issues.push(issue(
            Severity::Warning,
            "OWNER_IDS is missing. Owner-only commands will be unavailable.",
        ));
    }
    if !owners.invalid.is_empty() {
        issues.push(issue(
            Severity::Warning,
            format!("Skipped invalid OWNER_IDS: {}.", owners.invalid.join(", ")),
        ));
    }
    Section { issues, lines }
}

pub async fn execute_health(ix: &Ix<'_>) -> Result<()> {
    let Some(guild_id) = ix.guild_id() else {
        return ix
            .reply_ephemeral("This command only works in a server.")
            .await;
    };
    let app = ix.app;
    let music = inspect_music(app, Some(guild_id)).await;
    let logs = inspect_logs(app, guild_id);
    let perms = inspect_permissions(ix, &music.diagnostics, None);
    let runtime = runtime_section(app);
    let music_line = music_summary(&music.diagnostics);
    let logs_line = logs_summary(app, guild_id);
    let mut issues = Vec::new();
    issues.extend(runtime.issues);
    issues.extend(music.issues);
    issues.extend(logs.issues);
    issues.extend(perms.issues);
    ix.reply(health_panel(&app.bot_tag, &music_line, &logs_line, &issues))
        .await
}

fn music_summary(d: &music::Diagnostics) -> String {
    let online = d.usable && d.node.as_ref().is_some_and(|n| n.connected);
    let lavalink = if online {
        "Lavalink online"
    } else {
        "Lavalink offline"
    };
    let voice = match d
        .guild_session
        .as_ref()
        .and_then(|s| s.voice_channel_id.map(|c| (s, c)))
    {
        Some((s, channel)) => {
            let channel = format!("<#{channel}>");
            match s.current_track_title.as_deref() {
                Some(title) if s.playing || s.paused => format!(
                    "{} in {channel}: {} (queue: {})",
                    if s.paused { "paused" } else { "playing" },
                    inline(title, 80),
                    s.queue_size
                ),
                _ => format!("idle in {channel}"),
            }
        }
        None => "not in a voice channel".to_owned(),
    };
    format!("{lavalink} · {voice}")
}

fn logs_summary(app: &App, guild_id: Id<GuildMarker>) -> String {
    let state = resolve_logs_state(app, &guild_id.to_string());
    if !state.enabled {
        return "disabled".to_owned();
    }
    let Some(channel) = state
        .channel_id
        .as_deref()
        .and_then(discord::parse_id::<ChannelMarker>)
    else {
        return "enabled, but no channel is set".to_owned();
    };
    let perms = channel_snapshot(app, Some(channel), TEXT_CHECKS);
    let access = if perms.available && perms.missing.is_empty() {
        "permissions OK".to_owned()
    } else if perms.available {
        format!("missing: {}", perms.missing.join(", "))
    } else {
        perms.reason.to_owned()
    };
    format!("enabled in <#{channel}> · {access}")
}

/// Short overview: one status line, one line per module, and the problems (if any).
/// Paths, node details, retention and queues are in `/debug`.
fn health_panel(bot_tag: &str, music: &str, logs: &str, issues: &[Issue]) -> Value {
    let level = overall(issues);
    let status = match (level, issues.len()) {
        (Severity::Ok, _) => "✅ Everything works".to_owned(),
        (_, 1) => format!("{} 1 problem", badge(level)),
        (_, n) => format!("{} {n} problems", badge(level)),
    };
    let mut content = vec![
        format!("## Health · {status}"),
        format!(
            "**Bot** · online as {bot_tag} · v{}",
            env!("CARGO_PKG_VERSION")
        ),
        format!("**Music** · {music}"),
        format!("**Logs** · {logs}"),
    ];
    if level != Severity::Ok {
        content.push(String::new());
        content.extend(issues.iter().map(|i| format!("{} {}", badge(i.0), i.1)));
    }
    json!({
        "flags": FLAG_COMPONENTS_V2 | FLAG_EPHEMERAL,
        "allowed_mentions": { "parse": [] },
        "components": [{
            "type": 17,
            "components": [{ "type": 10, "content": truncate(&content.join("\n"), 3800) }],
        }],
    })
}

pub async fn execute_debug(ix: &Ix<'_>) -> Result<()> {
    let Some(guild_id) = ix.guild_id() else {
        return ix
            .reply(build_panel(
                "Debug",
                &[(
                    "Info",
                    vec!["This command only works in a server.".to_owned()],
                )],
            ))
            .await;
    };
    let app = ix.app;
    let opts = ix.opts();
    let mode = opts.subcommand.unwrap_or_default().to_owned();
    let picked = opts.channel("channel");
    match mode.as_str() {
        "music" => {
            let music = inspect_music(app, Some(guild_id)).await;
            let runtime = runtime_section(app);
            let mut issues = runtime.issues;
            issues.extend(music.issues);
            let mut state = vec![format!("Rating: {}", badge(overall(&issues)))];
            state.extend(music.lines);
            ix.reply(build_panel(
                "Debug Music",
                &[("State", state), ("Red Flags", issue_lines(&issues))],
            ))
            .await
        }
        "permissions" => {
            let music = inspect_music(app, Some(guild_id)).await;
            let perms = inspect_permissions(ix, &music.diagnostics, picked);
            ix.reply(build_panel(
                "Debug Permissions",
                &[
                    ("State", perms.lines),
                    ("Red Flags", issue_lines(&perms.issues)),
                ],
            ))
            .await
        }
        "logs" => {
            let logs = inspect_logs(app, guild_id);
            ix.reply(build_panel(
                "Debug Logs",
                &[
                    ("State", logs.lines),
                    ("Red Flags", issue_lines(&logs.issues)),
                ],
            ))
            .await
        }
        _ => {
            ix.reply(build_panel(
                "Debug",
                &[("Info", vec![format!("Unknown debug mode: {mode}.")])],
            ))
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_panel_is_short() {
        let ok = health_panel(
            "Operator#1204",
            "Lavalink online · not in a voice channel",
            "enabled in <#1> · permissions OK",
            &[],
        );
        let text = ok["components"][0]["components"][0]["content"]
            .as_str()
            .unwrap();
        println!("{text}");
        assert_eq!(text.lines().count(), 4);
        assert!(text.starts_with("## Health · ✅ Everything works"));

        let bad = health_panel(
            "Operator#1204",
            "Lavalink offline · not in a voice channel",
            "disabled",
            &[issue(Severity::Critical, "Lavalink is offline.")],
        );
        let text = bad["components"][0]["components"][0]["content"]
            .as_str()
            .unwrap();
        println!("{text}");
        assert!(text.starts_with("## Health · ❌ Critical 1 problem"));
        assert!(text.ends_with("❌ Critical Lavalink is offline."));
    }
}
