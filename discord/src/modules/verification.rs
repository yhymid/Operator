//! Verify button + captcha modal + role expiry sweeps (port of the verification parts of
//! `main.js` and `src/core/verification.js`).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use rand::RngExt;
use serde_json::{Map, Value, json};
use twilight_http::request::AuditLogReason;
use twilight_model::guild::{Member, Permissions};
use twilight_model::id::Id;
use twilight_model::id::marker::{GuildMarker, RoleMarker, UserMarker};

use crate::app::{App, SharedApp};
use crate::discord::{self, Ix};
use operator_common::time::{add_one_calendar_month, parse_iso, to_iso};

pub const CAPTCHA_MODAL_ID: &str = "verify_captcha_modal_v1";
pub const CAPTCHA_INPUT_ID: &str = "verify_captcha_input_v1";
const CAPTCHA_TTL: Duration = Duration::minutes(5);
const LOCKOUT: Duration = Duration::hours(1);
pub const SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10 * 60);
const FULL_SWEEP_EVERY: u64 = 72;
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

pub fn generate_code(length: usize) -> String {
    let mut rng = rand::rng();
    (0..length.max(1))
        .map(|_| CODE_ALPHABET[rng.random_range(0..CODE_ALPHABET.len())] as char)
        .collect()
}

pub fn add_one_month_iso(base: DateTime<Utc>) -> String {
    to_iso(add_one_calendar_month(base))
}

fn lockout_until(app: &App, guild: &str, user: &str) -> Option<DateTime<Utc>> {
    app.state
        .prune_expired_verification_lockouts(guild, Utc::now());
    app.state
        .verification_lockout(guild, user)
        .and_then(|iso| parse_iso(&iso))
        .filter(|until| *until > Utc::now())
}

fn locked_text(until: DateTime<Utc>) -> String {
    format!(
        "Weryfikacja jest tymczasowo zablokowana. Sprobuj ponownie <t:{}:R>.",
        until.timestamp()
    )
}

async fn fetch_member(
    app: &App,
    guild_id: Id<GuildMarker>,
    user_id: Id<UserMarker>,
) -> Option<Member> {
    match app.http.guild_member(guild_id, user_id).await {
        Ok(response) => response.model().await.ok(),
        Err(_) => None,
    }
}

/// Role position from cache, falling back to a REST fetch of guild roles.
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

fn bot_can_assign(app: &App, guild_id: Id<GuildMarker>, role_position: i64) -> bool {
    let Some(perms) = discord::bot_root_permissions(app, guild_id) else {
        return false;
    };
    discord::has(perms, Permissions::MANAGE_ROLES)
        && role_position < discord::highest_role_position(app, guild_id, app.bot_id)
}

/// `handleVerifyButtonInteraction`. Returns without replying when the button is not ours.
pub async fn handle_button(ix: &Ix<'_>) -> Result<()> {
    let app = ix.app;
    let Some(guild_id) = ix.guild_id() else {
        return Ok(());
    };
    let guild = guild_id.to_string();
    let verification = app.state.verification(&guild);
    let panel = &verification["panel"];
    if verification["enabled"] != Value::Bool(true) {
        return Ok(());
    }
    let custom_id = panel["customId"].as_str().unwrap_or_default();
    if custom_id.is_empty() || ix.custom_id() != Some(custom_id) {
        return Ok(());
    }
    let panel_message = panel["messageId"].as_str().unwrap_or_default();
    if panel_message.is_empty()
        || ix.message_id().map(|m| m.to_string()).as_deref() != Some(panel_message)
    {
        return Ok(());
    }

    let Some(role_id) = verification["roleId"]
        .as_str()
        .filter(|r| !r.is_empty())
        .and_then(discord::parse_id::<RoleMarker>)
    else {
        return ix
            .reply_ephemeral("Rola weryfikacyjna nie jest skonfigurowana.")
            .await;
    };

    let user_id = ix.user_id();
    let Some(member) = fetch_member(app, guild_id, user_id).await else {
        return ix
            .reply_ephemeral("Nie udalo sie pobrac Twoich danych na serwerze.")
            .await;
    };
    if member.roles.contains(&role_id) {
        return ix
            .reply_ephemeral("Masz juz aktywna role weryfikacyjna.")
            .await;
    }

    let user = user_id.to_string();
    if let Some(until) = lockout_until(app, &guild, &user) {
        return ix.reply_ephemeral(locked_text(until)).await;
    }

    let Some(position) = role_position(app, guild_id, role_id).await else {
        return ix
            .reply_ephemeral("Skonfigurowana rola weryfikacyjna nie istnieje.")
            .await;
    };
    if !bot_can_assign(app, guild_id, position) {
        return ix
            .reply_ephemeral("Bot nie moze nadac roli weryfikacyjnej.")
            .await;
    }

    let now = Utc::now();
    app.state.prune_expired_verification_challenges(&guild, now);
    let existing = app.state.verification_challenge(&guild, &user);
    let active_code = existing.as_ref().and_then(|challenge| {
        let expires = parse_iso(challenge["expiresAt"].as_str().unwrap_or_default())?;
        let code = challenge["code"].as_str().filter(|c| !c.is_empty())?;
        (expires > now).then(|| code.to_owned())
    });
    let code = match active_code {
        Some(code) => code,
        None => {
            let code = generate_code(6);
            app.state.save_verification_challenge(
                &guild,
                &user,
                &role_id.to_string(),
                &code,
                &to_iso(now + CAPTCHA_TTL),
            );
            code
        }
    };

    let length = code.chars().count();
    ix.show_modal(json!({
        "custom_id": CAPTCHA_MODAL_ID,
        "title": "Weryfikacja",
        "components": [{
            "type": 1,
            "components": [{
                "type": 4,
                "custom_id": CAPTCHA_INPUT_ID,
                "label": format!("Przepisz kod: {code}"),
                "style": 1,
                "required": true,
                "min_length": length,
                "max_length": length
            }]
        }]
    }))
    .await
}

/// `handleVerifyCaptchaModal`
pub async fn handle_modal(ix: &Ix<'_>) -> Result<()> {
    if ix.custom_id() != Some(CAPTCHA_MODAL_ID) {
        return Ok(());
    }
    let app = ix.app;
    let Some(guild_id) = ix.guild_id() else {
        return Ok(());
    };
    let guild = guild_id.to_string();
    let user_id = ix.user_id();
    let user = user_id.to_string();

    if let Some(until) = lockout_until(app, &guild, &user) {
        return ix.reply_ephemeral(locked_text(until)).await;
    }

    let now = Utc::now();
    app.state.prune_expired_verification_challenges(&guild, now);
    let Some(challenge) = app.state.verification_challenge(&guild, &user) else {
        return ix
            .reply_ephemeral("Captcha wygasla. Kliknij przycisk weryfikacji ponownie.")
            .await;
    };
    let expires = parse_iso(challenge["expiresAt"].as_str().unwrap_or_default());
    if expires.is_none_or(|e| Utc::now() > e) {
        app.state.remove_verification_challenge(&guild, &user);
        return ix
            .reply_ephemeral("Captcha wygasla. Kliknij przycisk weryfikacji ponownie.")
            .await;
    }

    let answer = ix
        .modal_text(CAPTCHA_INPUT_ID)
        .unwrap_or_default()
        .trim()
        .to_uppercase();
    if Some(answer.as_str()) != challenge["code"].as_str() {
        let until = Utc::now() + LOCKOUT;
        app.state
            .set_verification_lockout(&guild, &user, &to_iso(until));
        app.state.remove_verification_challenge(&guild, &user);
        return ix
            .reply_ephemeral(format!(
                "Zly kod. Weryfikacja zablokowana na 1 godzine. Sprobuj ponownie <t:{}:R>.",
                until.timestamp()
            ))
            .await;
    }

    let role_id = challenge["roleId"]
        .as_str()
        .and_then(discord::parse_id::<RoleMarker>);
    let member = fetch_member(app, guild_id, user_id).await;
    let position = match role_id {
        Some(role) => role_position(app, guild_id, role).await,
        None => None,
    };
    let bot_present = app.cache.member(guild_id, app.bot_id).is_some();
    let (Some(member), Some(role_id), Some(position), true) =
        (member, role_id, position, bot_present)
    else {
        app.state.remove_verification_challenge(&guild, &user);
        return ix
            .reply_ephemeral("Nie mozna dokonczyc weryfikacji w tej chwili.")
            .await;
    };

    if member.roles.contains(&role_id) {
        app.state.remove_verification_challenge(&guild, &user);
        return ix
            .reply_ephemeral("Masz juz aktywna role weryfikacyjna.")
            .await;
    }
    if !bot_can_assign(app, guild_id, position) {
        app.state.remove_verification_challenge(&guild, &user);
        return ix
            .reply_ephemeral("Bot nie moze nadac roli weryfikacyjnej.")
            .await;
    }

    let added = app
        .http
        .add_guild_member_role(guild_id, user_id, role_id)
        .reason("Verify button + captcha")
        .await;
    match added {
        Ok(_) => {
            let expiry = add_one_calendar_month(Utc::now());
            app.state
                .set_user_verification_expiry(&guild, &user, &to_iso(expiry));
            app.state.remove_verification_challenge(&guild, &user);
            ix.reply_ephemeral(format!(
                "Weryfikacja zakonczona. Rola wygasa <t:{}:f>.",
                expiry.timestamp()
            ))
            .await
        }
        Err(error) => {
            tracing::error!("Verify captcha assign error: {error}");
            app.state.remove_verification_challenge(&guild, &user);
            ix.reply_ephemeral("Nie udalo sie nadac roli po sprawdzeniu captcha.")
                .await
        }
    }
}

fn sweep_patch(now_iso: &str, full_scan: bool) -> Value {
    let mut patch = Map::new();
    patch.insert("lastSweepAt".into(), json!(now_iso));
    if full_scan {
        patch.insert("lastFullSweepAt".into(), json!(now_iso));
    }
    Value::Object(patch)
}

async fn remove_role(
    app: &App,
    guild_id: Id<GuildMarker>,
    user_id: Id<UserMarker>,
    role_id: Id<RoleMarker>,
    trigger: &str,
) -> bool {
    let reason = format!("Verify expiry ({trigger})");
    match app
        .http
        .remove_guild_member_role(guild_id, user_id, role_id)
        .reason(&reason)
        .await
    {
        Ok(_) => true,
        Err(error) => {
            tracing::error!("Nie mozna usunac verify na {guild_id}/{user_id}: {error}");
            false
        }
    }
}

/// Outcome of examining one tracked member; mirrors the JS loop bodies.
#[allow(clippy::too_many_arguments)]
async fn process_member(
    app: &App,
    guild_id: Id<GuildMarker>,
    role_id: Id<RoleMarker>,
    user_id: Id<UserMarker>,
    exceptions: &HashSet<String>,
    expirations: &mut Map<String, Value>,
    now: DateTime<Utc>,
    trigger: &str,
) {
    let key = user_id.to_string();
    if exceptions.contains(&key) {
        return;
    }
    let Some(current) = expirations.get(&key).cloned() else {
        expirations.insert(key, json!(add_one_month_iso(now)));
        return;
    };
    let Some(expiry) = current.as_str().and_then(parse_iso) else {
        expirations.insert(key, json!(add_one_month_iso(now)));
        return;
    };
    if expiry <= now && remove_role(app, guild_id, user_id, role_id, trigger).await {
        expirations.remove(&key);
    }
}

pub async fn run_sweep(app: &App, trigger: &str, full_scan: bool) {
    let now = Utc::now();
    let now_iso = to_iso(now);
    for guild_id in app.guild_ids() {
        let guild = guild_id.to_string();
        let verification = app.state.verification(&guild);
        let role_id = verification["roleId"]
            .as_str()
            .filter(|r| !r.is_empty())
            .and_then(discord::parse_id::<RoleMarker>);
        let Some(role_id) = role_id.filter(|_| verification["enabled"] == Value::Bool(true)) else {
            app.state
                .save_verification(&guild, sweep_patch(&now_iso, full_scan));
            continue;
        };
        if verification["challenges"]
            .as_object()
            .is_some_and(|m| !m.is_empty())
        {
            app.state.prune_expired_verification_challenges(&guild, now);
        }
        if verification["lockouts"]
            .as_object()
            .is_some_and(|m| !m.is_empty())
        {
            app.state.prune_expired_verification_lockouts(&guild, now);
        }
        if role_position(app, guild_id, role_id).await.is_none() {
            app.state
                .save_verification(&guild, sweep_patch(&now_iso, full_scan));
            continue;
        }

        let exceptions: HashSet<String> =
            crate::config::string_list(&verification, &["exceptions"])
                .into_iter()
                .collect();
        let mut expirations = verification["expirations"]
            .as_object()
            .cloned()
            .unwrap_or_default();

        if full_scan {
            let Some(members) = discord::fetch_all_members(app, guild_id).await else {
                app.state
                    .save_verification(&guild, sweep_patch(&now_iso, true));
                continue;
            };
            let present: HashMap<String, bool> = members
                .iter()
                .map(|m| (m.user.id.to_string(), m.roles.contains(&role_id)))
                .collect();
            for member in &members {
                let key = member.user.id.to_string();
                if !member.roles.contains(&role_id) {
                    expirations.remove(&key);
                    continue;
                }
                process_member(
                    app,
                    guild_id,
                    role_id,
                    member.user.id,
                    &exceptions,
                    &mut expirations,
                    now,
                    trigger,
                )
                .await;
            }
            expirations.retain(|user, _| present.contains_key(user));
        } else {
            let tracked: Vec<String> = expirations.keys().cloned().collect();
            for key in tracked {
                let Some(user_id) = discord::parse_id::<UserMarker>(&key) else {
                    expirations.remove(&key);
                    continue;
                };
                let Some(member) = fetch_member(app, guild_id, user_id).await else {
                    expirations.remove(&key);
                    continue;
                };
                if !member.roles.contains(&role_id) {
                    expirations.remove(&key);
                    continue;
                }
                process_member(
                    app,
                    guild_id,
                    role_id,
                    user_id,
                    &exceptions,
                    &mut expirations,
                    now,
                    trigger,
                )
                .await;
            }
        }

        let mut patch = sweep_patch(&now_iso, full_scan);
        patch["expirations"] = Value::Object(expirations);
        app.state.save_verification(&guild, patch);
    }
}

pub fn cleanup_challenges_on_startup(app: &App) {
    let now = Utc::now();
    for guild_id in app.guild_ids() {
        let guild = guild_id.to_string();
        let verification = app.state.verification(&guild);
        if verification["enabled"] != Value::Bool(true) {
            continue;
        }
        if verification["challenges"]
            .as_object()
            .is_some_and(|m| !m.is_empty())
        {
            app.state.prune_expired_verification_challenges(&guild, now);
        }
        if verification["lockouts"]
            .as_object()
            .is_some_and(|m| !m.is_empty())
        {
            app.state.prune_expired_verification_lockouts(&guild, now);
        }
    }
}

/// `startVerificationSweepRunner`: every 10 minutes, a full member scan every 72nd run.
pub fn start_sweep_runner(app: &SharedApp) -> tokio::task::JoinHandle<()> {
    let app = app.clone();
    tokio::spawn(async move {
        let counter = AtomicU64::new(0);
        let mut interval = tokio::time::interval(SWEEP_INTERVAL);
        interval.tick().await;
        loop {
            interval.tick().await;
            let count = counter.fetch_add(1, Ordering::SeqCst) + 1;
            let full = count.is_multiple_of(FULL_SWEEP_EVERY);
            run_sweep(&app, if full { "interval-full" } else { "interval" }, full).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_use_alphabet() {
        let code = generate_code(6);
        assert_eq!(code.len(), 6);
        assert!(code.bytes().all(|b| CODE_ALPHABET.contains(&b)));
    }

    #[test]
    fn month_rollover_matches_js() {
        let base = parse_iso("2026-01-31T10:00:00.000Z").unwrap();
        assert_eq!(add_one_month_iso(base), "2026-02-28T10:00:00.000Z");
    }
}
