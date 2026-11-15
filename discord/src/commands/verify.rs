//! `/verify` (port of `src/cmds/verify.js`).

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use chrono::Utc;
use serde_json::{Map, Value, json};
use twilight_http::request::AuditLogReason;
use twilight_model::channel::ChannelType;
use twilight_model::guild::{Member, Permissions};
use twilight_model::id::Id;
use twilight_model::id::marker::{
    ChannelMarker, GuildMarker, MessageMarker, RoleMarker, UserMarker,
};

use crate::app::App;
use crate::discord::{self, Ix};
use crate::modules::verification::add_one_month_iso;
use operator_common::time::{now_iso, parse_iso};

pub const VERIFY_BUTTON_CUSTOM_ID: &str = "verify_button_v1";
const VERIFY_PANEL_TITLE: &str = "Verification";
const VERIFY_PANEL_BODY: &str =
    "Click the button below to get the Verified role for one calendar month.";

fn panel_payload() -> Value {
    json!({
        "flags": discord::FLAG_COMPONENTS_V2,
        "components": [{
            "type": 17,
            "components": [
                { "type": 10, "content": format!("## {VERIFY_PANEL_TITLE}") },
                { "type": 10, "content": VERIFY_PANEL_BODY },
                { "type": 1, "components": [
                    { "type": 2, "style": 1, "custom_id": VERIFY_BUTTON_CUSTOM_ID, "label": "Verify" }
                ]}
            ]
        }]
    })
}

fn dedupe_ids(values: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .iter()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty() && seen.insert(v.clone()))
        .collect()
}

fn format_user_list(ids: &[String]) -> String {
    let ids = dedupe_ids(ids);
    if ids.is_empty() {
        return "none".to_owned();
    }
    let preview = ids
        .iter()
        .take(5)
        .map(|id| format!("<@{id}>"))
        .collect::<Vec<_>>()
        .join(", ");
    if ids.len() <= 5 {
        preview
    } else {
        format!("{preview} and {} more", ids.len() - 5)
    }
}

/// `getVerificationStateSafe`
struct SafeState {
    enabled: bool,
    role_id: Option<String>,
    panel_channel: Option<String>,
    panel_message: Option<String>,
    panel_custom: Option<String>,
    exceptions: Vec<String>,
    expirations: Map<String, Value>,
    challenges: Map<String, Value>,
    lockouts: Map<String, Value>,
    last_sweep_at: Option<String>,
}

fn non_empty(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn safe_state(app: &App, guild_id: &str) -> SafeState {
    let v = app.state.verification(guild_id);
    let map = |key: &str| v[key].as_object().cloned().unwrap_or_default();
    SafeState {
        enabled: v["enabled"] == Value::Bool(true),
        role_id: non_empty(&v["roleId"]),
        panel_channel: non_empty(&v["panel"]["channelId"]),
        panel_message: non_empty(&v["panel"]["messageId"]),
        panel_custom: non_empty(&v["panel"]["customId"]),
        exceptions: dedupe_ids(&crate::config::string_list(&v, &["exceptions"])),
        expirations: map("expirations"),
        challenges: map("challenges"),
        lockouts: map("lockouts"),
        last_sweep_at: non_empty(&v["lastSweepAt"]),
    }
}

/// Returns `(state text, valid)`.
async fn inspect_panel(
    app: &App,
    channel: Option<&str>,
    message: Option<&str>,
) -> (&'static str, bool) {
    let (Some(channel), Some(message)) = (channel, message) else {
        return ("none", false);
    };
    let channel_id = discord::parse_id::<ChannelMarker>(channel);
    let channel_ok = channel_id
        .and_then(|id| {
            app.cache
                .channel(id)
                .map(|c| discord::is_text_based(c.kind))
        })
        .unwrap_or(false);
    if !channel_ok {
        return ("mismatch (channel does not exist)", false);
    }
    let Some(message_id) = discord::parse_id::<MessageMarker>(message) else {
        return ("mismatch (message does not exist)", false);
    };
    let Some(channel_id) = channel_id else {
        return ("mismatch (channel does not exist)", false);
    };
    match app.http.message(channel_id, message_id).await {
        Ok(_) => ("ok", true),
        Err(_) => ("mismatch (message does not exist)", false),
    }
}

async fn status_lines(app: &App, guild_id: &str) -> Vec<String> {
    let s = safe_state(app, guild_id);
    let (panel_state, _) =
        inspect_panel(app, s.panel_channel.as_deref(), s.panel_message.as_deref()).await;
    vec![
        format!("Enabled: {}", if s.enabled { "yes" } else { "no" }),
        format!(
            "Role: {}",
            s.role_id
                .as_deref()
                .map_or("none".to_owned(), |r| format!("<@&{r}>"))
        ),
        format!(
            "Panel channel: {}",
            s.panel_channel
                .as_deref()
                .map_or("none".to_owned(), |c| format!("<#{c}>"))
        ),
        format!(
            "Panel messageId: {}",
            s.panel_message.as_deref().unwrap_or("none")
        ),
        format!("Panel status: {panel_state}"),
        format!("Custom ID: {}", s.panel_custom.as_deref().unwrap_or("none")),
        format!(
            "Exceptions: {}{}",
            s.exceptions.len(),
            if s.exceptions.is_empty() {
                String::new()
            } else {
                format!(" ({})", format_user_list(&s.exceptions))
            }
        ),
        format!("Active expiries: {}", s.expirations.len()),
        format!("Active challenges: {}", s.challenges.len()),
        format!("Active lockouts: {}", s.lockouts.len()),
        format!(
            "Last sweep: {}",
            s.last_sweep_at.as_deref().unwrap_or("never")
        ),
    ]
}

async fn role_position(
    app: &App,
    guild_id: Id<GuildMarker>,
    role_id: Id<RoleMarker>,
) -> Option<i64> {
    if let Some(position) = discord::role_position(app, role_id) {
        return Some(position);
    }
    let roles = app.http.roles(guild_id).await.ok()?.models().await.ok()?;
    roles
        .into_iter()
        .find(|r| r.id == role_id)
        .map(|r| r.position)
}

/// `validateManageableVerifyRole`
fn validate_manageable(
    app: &App,
    guild_id: Id<GuildMarker>,
    position: i64,
) -> Result<(), &'static str> {
    if app.cache.member(guild_id, app.bot_id).is_none() {
        return Err("Cannot check the bot's permissions.");
    }
    let perms = discord::bot_root_permissions(app, guild_id).unwrap_or(Permissions::empty());
    if !discord::has(perms, Permissions::MANAGE_ROLES) {
        return Err("The bot lacks the Manage Roles permission.");
    }
    if position >= discord::highest_role_position(app, guild_id, app.bot_id) {
        return Err("Cannot give or remove this role (it is at or above the bot's highest role).");
    }
    Ok(())
}

fn channel_permission_issue(app: &App, channel: Id<ChannelMarker>) -> Option<String> {
    let perms = discord::bot_channel_permissions(app, channel)?;
    let mut missing = Vec::new();
    if !discord::has(perms, Permissions::VIEW_CHANNEL) {
        missing.push("View Channel");
    }
    if !discord::has(perms, Permissions::SEND_MESSAGES) {
        missing.push("Send Messages");
    }
    if !discord::has(perms, Permissions::READ_MESSAGE_HISTORY) {
        missing.push("Read Message History");
    }
    (!missing.is_empty()).then(|| missing.join(", "))
}

fn channel_kind(app: &App, id: Id<ChannelMarker>) -> Option<ChannelType> {
    app.cache.channel(id).map(|c| c.kind)
}

/// `resolvePanelChannel`
fn resolve_panel_channel(
    ix: &Ix<'_>,
    state: &SafeState,
) -> Option<(Id<ChannelMarker>, ChannelType)> {
    if let Some(explicit) = ix.opts().channel("channel") {
        return Some(explicit);
    }
    if let Some(stored) = state
        .panel_channel
        .as_deref()
        .and_then(discord::parse_id::<ChannelMarker>)
        && let Some(kind) = channel_kind(ix.app, stored).filter(|k| discord::is_text_based(*k))
    {
        return Some((stored, kind));
    }
    ix.interaction.channel.as_ref().map(|c| (c.id, c.kind))
}

async fn send_or_resend_panel(
    ix: &Ix<'_>,
    guild_id: Id<GuildMarker>,
    role_id: Id<RoleMarker>,
    role_position: i64,
    channel: Option<(Id<ChannelMarker>, ChannelType)>,
    reason: &str,
) -> Result<()> {
    let app = ix.app;
    let guild = guild_id.to_string();
    let before = safe_state(app, &guild);
    ix.defer(true).await?;

    if let Err(error) = validate_manageable(app, guild_id, role_position) {
        return ix.edit_content(error).await;
    }
    let Some((channel_id, _)) = channel.filter(|(_, kind)| discord::is_text_based(*kind)) else {
        return ix
            .edit_content("This channel does not support text messages.")
            .await;
    };
    if let Some(issue) = channel_permission_issue(app, channel_id) {
        return ix
            .edit_content(format!(
                "The bot cannot send the panel to this channel. Missing: {issue}."
            ))
            .await;
    }
    let sent = match discord::send_message(app, channel_id, &panel_payload()).await {
        Ok(message) => message,
        Err(error) => {
            return ix
                .edit_content(format!(
                    "Could not send the verify panel: {}",
                    discord::err_text(&error)
                ))
                .await;
        }
    };

    let role = role_id.to_string();
    let role_changed = before.role_id.as_deref().is_some_and(|r| r != role);
    let mut patch = json!({
        "enabled": true,
        "roleId": role,
        "panel": {
            "channelId": channel_id.to_string(),
            "messageId": sent.id.to_string(),
            "customId": VERIFY_BUTTON_CUSTOM_ID
        },
    });
    patch["challenges"] = if role_changed {
        json!({})
    } else {
        Value::Object(before.challenges.clone())
    };
    app.state.save_verification(&guild, patch);

    let deleted_previous = match (&before.panel_channel, &before.panel_message) {
        (Some(channel), Some(message)) if *message != sent.id.to_string() => {
            match (
                discord::parse_id::<ChannelMarker>(channel),
                discord::parse_id::<MessageMarker>(message),
            ) {
                (Some(c), Some(m)) => app.http.delete_message(c, m).await.is_ok(),
                _ => false,
            }
        }
        _ => false,
    };

    let mut lines = vec![if reason == "setup" {
        format!("Verify panel created in <#{channel_id}> for <@&{role_id}>.")
    } else {
        format!("Verify panel resent in <#{channel_id}> for <@&{role_id}>.")
    }];
    if deleted_previous {
        lines.push("The previous panel was deleted so no dead button is left behind.".to_owned());
    }
    if role_changed {
        lines.push("Active challenges were cleared because the verify role changed.".to_owned());
    }
    lines.push(String::new());
    lines.extend(status_lines(app, &guild).await);
    ix.edit_content(lines.join("\n")).await
}

async fn fetch_member(
    app: &App,
    guild_id: Id<GuildMarker>,
    user_id: Id<UserMarker>,
) -> Option<Member> {
    app.http
        .guild_member(guild_id, user_id)
        .await
        .ok()?
        .model()
        .await
        .ok()
}

#[derive(Default)]
struct CleanupCounts {
    removed_exceptions: usize,
    removed_expirations: usize,
    repaired_expirations: usize,
    removed_challenges: usize,
    removed_lockouts: usize,
    removed_roles: usize,
    cleared_panel: bool,
}

async fn run_cleanup_state(app: &App, guild_id: Id<GuildMarker>) -> CleanupCounts {
    let guild = guild_id.to_string();
    let now = Utc::now();
    let state = safe_state(app, &guild);
    let role_id = state
        .role_id
        .as_deref()
        .and_then(discord::parse_id::<RoleMarker>);
    let role_exists = match role_id {
        Some(id) => role_position(app, guild_id, id).await.is_some(),
        None => false,
    };
    let role_id = role_id.filter(|_| role_exists);
    let members: Option<HashMap<String, Member>> =
        discord::fetch_all_members(app, guild_id).await.map(|list| {
            list.into_iter()
                .map(|m| (m.user.id.to_string(), m))
                .collect()
        });
    let missing = |user: &str| members.as_ref().is_some_and(|m| !m.contains_key(user));
    let has_role = |member: &Member| role_id.is_some_and(|r| member.roles.contains(&r));
    let mut counts = CleanupCounts::default();

    let mut exceptions = Vec::new();
    for user in &state.exceptions {
        if missing(user) {
            counts.removed_exceptions += 1;
        } else {
            exceptions.push(user.clone());
        }
    }
    let exception_set: HashSet<&String> = exceptions.iter().collect();

    let alive = |iso: Option<&str>| iso.and_then(parse_iso).is_some_and(|t| t > now);
    let mut challenges = Map::new();
    for (user, challenge) in &state.challenges {
        if !alive(challenge["expiresAt"].as_str()) || exception_set.contains(user) || missing(user)
        {
            counts.removed_challenges += 1;
        } else {
            challenges.insert(user.clone(), challenge.clone());
        }
    }
    let mut lockouts = Map::new();
    for (user, until) in &state.lockouts {
        if !alive(until.as_str()) || exception_set.contains(user) || missing(user) {
            counts.removed_lockouts += 1;
        } else {
            lockouts.insert(user.clone(), until.clone());
        }
    }

    let mut expirations = Map::new();
    for (user, expiry) in &state.expirations {
        if exception_set.contains(user) || missing(user) {
            counts.removed_expirations += 1;
            continue;
        }
        let member = members.as_ref().and_then(|m| m.get(user));
        if role_id.is_some() && member.is_some_and(|m| !has_role(m)) {
            counts.removed_expirations += 1;
            continue;
        }
        let Some(expiry_at) = expiry.as_str().and_then(parse_iso) else {
            counts.removed_expirations += 1;
            continue;
        };
        if expiry_at <= now {
            if let (Some(role), Some(member)) = (role_id, member)
                && has_role(member)
                && app
                    .http
                    .remove_guild_member_role(guild_id, member.user.id, role)
                    .reason("Verify cleanup_state expiry")
                    .await
                    .is_ok()
            {
                counts.removed_roles += 1;
            }
            counts.removed_expirations += 1;
            continue;
        }
        expirations.insert(user.clone(), expiry.clone());
    }

    if role_id.is_some()
        && let Some(members) = &members
    {
        for (user, member) in members {
            if !has_role(member) || exception_set.contains(user) {
                continue;
            }
            let existing_alive = expirations
                .get(user)
                .and_then(Value::as_str)
                .is_some_and(|iso| alive(Some(iso)));
            if existing_alive {
                continue;
            }
            expirations.insert(user.clone(), json!(add_one_month_iso(now)));
            counts.repaired_expirations += 1;
        }
    }

    let mut panel = json!({
        "channelId": state.panel_channel,
        "messageId": state.panel_message,
        "customId": state.panel_custom,
    });
    let (_, valid) = inspect_panel(
        app,
        state.panel_channel.as_deref(),
        state.panel_message.as_deref(),
    )
    .await;
    if state.panel_channel.is_some() && state.panel_message.is_some() && !valid {
        panel = json!({ "channelId": null, "messageId": null, "customId": null });
        counts.cleared_panel = true;
    }

    app.state.save_verification(
        &guild,
        json!({
            "panel": panel,
            "exceptions": exceptions,
            "expirations": expirations,
            "challenges": challenges,
            "lockouts": lockouts,
            "lastSweepAt": now_iso(),
        }),
    );
    counts
}

async fn reply_with_status(ix: &Ix<'_>, guild: &str, notes: Vec<String>) -> Result<()> {
    let mut lines = notes;
    lines.push(String::new());
    lines.extend(status_lines(ix.app, guild).await);
    ix.reply_ephemeral(lines.join("\n")).await
}

pub async fn execute(ix: &Ix<'_>) -> Result<()> {
    let Some(guild_id) = ix.guild_id() else {
        return ix
            .reply_ephemeral("This command only works in a server.")
            .await;
    };
    if !ix.member_has(Permissions::ADMINISTRATOR) {
        return ix
            .reply_ephemeral("Administrator permission required.")
            .await;
    }
    let app = ix.app;
    let guild = guild_id.to_string();
    let opts = ix.opts();
    let mode = match opts.subcommand.unwrap_or_default() {
        "resend" => "resend_panel",
        "expire" => "expire_user",
        "cleanup" => "cleanup_state",
        other => other,
    }
    .to_owned();

    match mode.as_str() {
        "setup" => {
            let Some(role) = opts.role("role") else {
                return ix.reply_ephemeral("Provide the `role` option.").await;
            };
            let channel = resolve_panel_channel(ix, &safe_state(app, &guild));
            send_or_resend_panel(ix, guild_id, role.id, role.position, channel, "setup").await
        }
        "status" => {
            let lines = status_lines(app, &guild).await;
            ix.reply_ephemeral(lines.join("\n")).await
        }
        "resend_panel" => {
            let state = safe_state(app, &guild);
            let Some(role_raw) = state.role_id.clone() else {
                return ix
                    .reply_ephemeral(
                        "No verify role is configured. First use `/verify setup role:<role>`.",
                    )
                    .await;
            };
            let role = discord::parse_id::<RoleMarker>(&role_raw);
            let position = match role {
                Some(id) => role_position(app, guild_id, id).await,
                None => None,
            };
            let (Some(role), Some(position)) = (role, position) else {
                return ix
                    .reply_ephemeral(format!(
                        "The configured verify role (`{role_raw}`) does not exist."
                    ))
                    .await;
            };
            let channel = resolve_panel_channel(ix, &state);
            send_or_resend_panel(ix, guild_id, role, position, channel, "resend_panel").await
        }
        "exception_add" => {
            let Some(user) = opts.user("user") else {
                return ix.reply_ephemeral("Provide the `user` option.").await;
            };
            let member = fetch_member(app, guild_id, user.id).await;
            let state = safe_state(app, &guild);
            let uid = user.id.to_string();
            let mut exceptions = state.exceptions.clone();
            exceptions.push(uid.clone());
            let (mut expirations, mut challenges, mut lockouts) = (
                state.expirations.clone(),
                state.challenges.clone(),
                state.lockouts.clone(),
            );
            expirations.remove(&uid);
            challenges.remove(&uid);
            lockouts.remove(&uid);
            app.state.save_verification(
                &guild,
                json!({
                    "exceptions": dedupe_ids(&exceptions),
                    "expirations": expirations,
                    "challenges": challenges,
                    "lockouts": lockouts,
                }),
            );
            let mut notes = vec![format!("Added <@{uid}> to the verify exceptions.")];
            let role = state
                .role_id
                .as_deref()
                .and_then(discord::parse_id::<RoleMarker>);
            if let (Some(member), Some(role)) = (&member, role)
                && !member.roles.contains(&role)
            {
                notes.push("Note: the user does not have the verify role now, and the exception does not give it automatically.".to_owned());
            }
            reply_with_status(ix, &guild, notes).await
        }
        "exception_remove" => {
            let Some(user) = opts.user("user") else {
                return ix.reply_ephemeral("Provide the `user` option.").await;
            };
            let state = safe_state(app, &guild);
            let member = fetch_member(app, guild_id, user.id).await;
            let role = state
                .role_id
                .as_deref()
                .and_then(discord::parse_id::<RoleMarker>);
            let has_verify_role =
                matches!((&member, role), (Some(m), Some(r)) if m.roles.contains(&r));
            let uid = user.id.to_string();
            let exceptions: Vec<String> = state
                .exceptions
                .iter()
                .filter(|id| **id != uid)
                .cloned()
                .collect();
            let mut expirations = state.expirations.clone();
            if has_verify_role {
                expirations.insert(uid.clone(), json!(add_one_month_iso(Utc::now())));
            } else {
                expirations.remove(&uid);
            }
            app.state.save_verification(
                &guild,
                json!({ "exceptions": exceptions, "expirations": expirations }),
            );
            let notes = vec![
                format!("Removed <@{uid}> from the verify exceptions."),
                if has_verify_role {
                    "The user kept the verify role and got a new expiry starting now.".to_owned()
                } else {
                    "If the user does not have the verify role, nothing was given automatically."
                        .to_owned()
                },
            ];
            reply_with_status(ix, &guild, notes).await
        }
        "expire_user" => {
            let Some(user) = opts.user("user") else {
                return ix.reply_ephemeral("Provide the `user` option.").await;
            };
            let state = safe_state(app, &guild);
            let member = fetch_member(app, guild_id, user.id).await;
            let role_id = state
                .role_id
                .as_deref()
                .and_then(discord::parse_id::<RoleMarker>);
            let role = match role_id {
                Some(id) => role_position(app, guild_id, id).await.map(|p| (id, p)),
                None => None,
            };
            let mut removed_role = false;
            let mut removal_issue: Option<String> = None;
            if let (Some((role, position)), Some(member)) = (role, &member)
                && member.roles.contains(&role)
            {
                match validate_manageable(app, guild_id, position) {
                    Err(error) => removal_issue = Some(error.to_owned()),
                    Ok(()) => match app
                        .http
                        .remove_guild_member_role(guild_id, user.id, role)
                        .reason("Verify expire_user")
                        .await
                    {
                        Ok(_) => removed_role = true,
                        Err(error) => {
                            removal_issue = Some(format!(
                                "Could not remove the role: {}",
                                discord::api_error_message(&error)
                            ));
                        }
                    },
                }
            }
            let uid = user.id.to_string();
            let (mut expirations, mut challenges, mut lockouts) = (
                state.expirations.clone(),
                state.challenges.clone(),
                state.lockouts.clone(),
            );
            expirations.remove(&uid);
            challenges.remove(&uid);
            lockouts.remove(&uid);
            app.state.save_verification(
                &guild,
                json!({ "expirations": expirations, "challenges": challenges, "lockouts": lockouts }),
            );
            let mut notes = vec![
                format!("Expired verify for <@{uid}>."),
                if removed_role {
                    "The verify role was removed immediately.".to_owned()
                } else if role.is_some() {
                    "The cached state was cleared. The role was not removed because the user did not have it or it could not be done now.".to_owned()
                } else {
                    "The cached state was cleared. The configured verify role does not exist."
                        .to_owned()
                },
            ];
            if state.exceptions.contains(&uid) {
                notes.push("Note: the user is still on the exception list. If they should go through normal verify, also use `exception_remove`.".to_owned());
            }
            if let Some(issue) = removal_issue {
                notes.push(format!("Warning: {issue}"));
            }
            reply_with_status(ix, &guild, notes).await
        }
        "cleanup_state" => {
            let c = run_cleanup_state(app, guild_id).await;
            let notes = vec![
                "Verify cleanup finished.".to_owned(),
                format!(
                    "Removed exceptions for users who left: {}",
                    c.removed_exceptions
                ),
                format!("Removed stale expiries: {}", c.removed_expirations),
                format!("Fixed missing expiries: {}", c.repaired_expirations),
                format!("Removed stale challenges: {}", c.removed_challenges),
                format!("Removed stale lockouts: {}", c.removed_lockouts),
                format!("Roles removed after expiry: {}", c.removed_roles),
                format!(
                    "Panel cleanup: {}",
                    if c.cleared_panel {
                        "cleared dead references"
                    } else {
                        "no changes"
                    }
                ),
            ];
            reply_with_status(ix, &guild, notes).await
        }
        _ => {
            ix.reply_ephemeral(format!("Unknown verify mode: {mode}."))
                .await
        }
    }
}
