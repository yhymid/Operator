//! AutoMod (port of `src/events/automod.js`).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use twilight_model::channel::Message;
use twilight_model::guild::Permissions;
use twilight_model::id::Id;
use twilight_model::id::marker::ChannelMarker;
use twilight_model::util::Timestamp;

use crate::app::App;
use crate::config;
use crate::discord::{self, FLAG_COMPONENTS_V2};
use crate::util::jsregex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub rule: &'static str,
    pub details: String,
}

pub struct AutoMod {
    /// `config.automod`
    config: Mutex<Value>,
    history: Mutex<HashMap<String, Vec<(Instant, String)>>>,
}

/// Lowercase + collapse whitespace (`normalizeContent`).
fn normalize_content(value: &str) -> String {
    crate::util::text::compact_whitespace(&value.to_lowercase())
}

/// JS `\W` without the `u` flag is ASCII-only, so Polish letters count as non-word characters.
const JS_NON_WORD: &str = "[^A-Za-z0-9_]";

fn count_mentioned_users(content: &str) -> usize {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"<@!?(\d{15,22})>").expect("static regex"));
    re.captures_iter(content)
        .map(|c| c[1].to_owned())
        .collect::<HashSet<_>>()
        .len()
}

pub struct TestResult {
    pub violation: Option<Violation>,
    pub note: Option<&'static str>,
}

impl AutoMod {
    pub fn new(root_config: &Value) -> Self {
        Self {
            config: Mutex::new(
                root_config
                    .get("automod")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            ),
            history: Mutex::new(HashMap::new()),
        }
    }

    pub fn cfg(&self) -> Value {
        self.config
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// `automod.start()`: reload config and prune history.
    pub fn start(&self, app: &App) -> Result<(), config::ConfigError> {
        let root = config::read(&app.paths.config_path)?;
        *self.config.lock().unwrap_or_else(|p| p.into_inner()) =
            root.get("automod").cloned().unwrap_or_else(|| json!({}));
        self.prune_history();
        Ok(())
    }

    fn prune_history(&self) {
        let window = config::int(&self.cfg(), &["rules", "duplicate", "windowSeconds"]);
        let mut history = self.history.lock().unwrap_or_else(|p| p.into_inner());
        if window <= 0 {
            history.clear();
            return;
        }
        let window = Duration::from_secs(window as u64);
        history.retain(|_, entries| {
            entries.retain(|(ts, _)| ts.elapsed() <= window);
            !entries.is_empty()
        });
    }

    /// Returns `true` when the message was handled (violation applied).
    pub async fn handle_message(&self, app: &App, message: &Message) -> bool {
        let cfg = self.cfg();
        if !config::is_true(&cfg, &["enabled"]) {
            return false;
        }
        let Some(guild_id) = message.guild_id else {
            return false;
        };
        if message.author.bot || message.member.is_none() {
            return false;
        }
        if self.is_exempt(app, &cfg, message) {
            return false;
        }

        let mentions = message
            .mentions
            .iter()
            .map(|m| m.id)
            .collect::<HashSet<_>>()
            .len();
        let Some(violation) = self.detect(
            &cfg["rules"],
            &message.content,
            mentions,
            Some(&format!("{guild_id}:{}", message.author.id)),
        ) else {
            return false;
        };

        self.apply_violation(app, &cfg, message, &violation).await;
        true
    }

    fn is_exempt(&self, app: &App, cfg: &Value, message: &Message) -> bool {
        let Some(guild_id) = message.guild_id else {
            return false;
        };
        let member_roles = message
            .member
            .as_ref()
            .map(|m| m.roles.clone())
            .unwrap_or_default();

        // Administrator check uses guild-level permissions from cache.
        if let Ok(perms) = app.cache.permissions().root(message.author.id, guild_id)
            && perms.contains(Permissions::ADMINISTRATOR)
        {
            return true;
        }

        let author = message.author.id.to_string();
        if config::string_list(cfg, &["exemptUsers"]).contains(&author) {
            return true;
        }
        let exempt_roles = config::string_list(cfg, &["exemptRoles"]);
        member_roles
            .iter()
            .any(|role| exempt_roles.contains(&role.to_string()))
    }

    /// `detectViolation`. `rules` is `config.automod.rules` or an override for `/automod test`.
    /// `duplicate_key` is `None` to skip duplicate tracking.
    pub fn detect(
        &self,
        rules: &Value,
        content: &str,
        mention_count: usize,
        duplicate_key: Option<&str>,
    ) -> Option<Violation> {
        if content.trim().is_empty() {
            return None;
        }
        let normalized = normalize_content(content);

        for word in config::string_list(rules, &["blockedWords"]) {
            let trimmed = word.trim();
            if trimmed.is_empty() {
                continue;
            }
            let pattern = format!(
                "(^|{JS_NON_WORD}){}({JS_NON_WORD}|$)",
                jsregex::escape(&trimmed.to_lowercase())
            );
            if jsregex::compile(&pattern, true)
                .ok()
                .and_then(|re| re.is_match(&normalized).ok())
                == Some(true)
            {
                return Some(Violation {
                    rule: "blocked_word",
                    details: format!("blocked word: {trimmed}"),
                });
            }
        }

        for pattern in config::string_list(rules, &["blockedRegex"]) {
            match jsregex::compile(&pattern, true) {
                Ok(re) => {
                    if re.is_match(content).unwrap_or(false) {
                        return Some(Violation {
                            rule: "blocked_regex",
                            details: format!("blocked regex: {pattern}"),
                        });
                    }
                }
                Err(error) => tracing::warn!("Automod invalid regex \"{pattern}\": {error}"),
            }
        }

        if config::is_true(rules, &["blockInvites"]) {
            static INVITE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
            let re = INVITE.get_or_init(|| {
                regex::Regex::new(r"(?i)(discord(?:app)?\.com/invite|discord\.gg)/")
                    .expect("static regex")
            });
            if re.is_match(content) {
                return Some(Violation {
                    rule: "invite_link",
                    details: "Discord invite detected".to_owned(),
                });
            }
        }

        if config::is_true(rules, &["blockLinks"]) {
            static LINK: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
            let re = LINK
                .get_or_init(|| regex::Regex::new(r"(?i)https?://[^\s<]+").expect("static regex"));
            let allowed: Vec<String> = config::string_list(rules, &["allowedDomains"])
                .into_iter()
                .map(|d| d.to_lowercase())
                .collect();
            for found in re.find_iter(content) {
                let Ok(url) = url::Url::parse(found.as_str()) else {
                    continue;
                };
                let Some(host) = url.host_str().map(str::to_lowercase) else {
                    continue;
                };
                let ok = allowed
                    .iter()
                    .any(|d| host == *d || host.ends_with(&format!(".{d}")));
                if !ok {
                    return Some(Violation {
                        rule: "blocked_link",
                        details: format!("link: {host}"),
                    });
                }
            }
        }

        let max_mentions = config::int(rules, &["maxMentions"]);
        if max_mentions > 0 && mention_count as i64 > max_mentions {
            return Some(Violation {
                rule: "mention_spam",
                details: format!("mentions={mention_count}"),
            });
        }

        if config::is_true(rules, &["caps", "enabled"]) {
            let letters: Vec<char> = content.chars().filter(char::is_ascii_alphabetic).collect();
            let upper = letters.iter().filter(|c| c.is_ascii_uppercase()).count();
            let min_length = config::int(rules, &["caps", "minLength"]);
            let ratio = config::number(rules, &["caps", "ratio"]);
            if letters.len() as i64 >= min_length
                && !letters.is_empty()
                && upper as f64 / letters.len() as f64 >= ratio
            {
                return Some(Violation {
                    rule: "caps_spam",
                    details: format!("caps={upper}/{}", letters.len()),
                });
            }
        }

        if let Some(key) = duplicate_key
            && config::is_true(rules, &["duplicate", "enabled"])
        {
            return self.track_duplicate(key, &normalized, &rules["duplicate"]);
        }
        None
    }

    fn track_duplicate(&self, key: &str, normalized: &str, duplicate: &Value) -> Option<Violation> {
        if normalized.is_empty() {
            return None;
        }
        let max_repeats = config::int(duplicate, &["maxRepeats"]);
        let window = config::int(duplicate, &["windowSeconds"]);
        if max_repeats <= 1 || window <= 0 {
            return None;
        }
        let window = Duration::from_secs(window as u64);
        let mut history = self.history.lock().unwrap_or_else(|p| p.into_inner());
        let entries = history.entry(key.to_owned()).or_default();
        entries.retain(|(ts, _)| ts.elapsed() <= window);
        entries.push((Instant::now(), normalized.to_owned()));
        let repeats = entries.iter().filter(|(_, c)| c == normalized).count() as i64;
        (repeats >= max_repeats).then(|| Violation {
            rule: "duplicate_spam",
            details: format!("repeats={repeats}/{max_repeats}"),
        })
    }

    async fn apply_violation(
        &self,
        app: &App,
        cfg: &Value,
        message: &Message,
        violation: &Violation,
    ) {
        let guild_id = message.guild_id.expect("guild message");
        let actions = &cfg["actions"];

        if config::is_true(actions, &["delete"])
            && let Err(error) = app
                .http
                .delete_message(message.channel_id, message.id)
                .await
        {
            tracing::warn!("Automod delete failed ({guild_id}/{}): {error}", message.id);
        }

        let timeout_minutes = config::int(actions, &["timeoutMinutes"]);
        if timeout_minutes > 0 && self.moderatable(app, guild_id, message.author.id) {
            let until = chrono::Utc::now() + chrono::Duration::minutes(timeout_minutes);
            let reason = format!("AutoMod: {}", violation.rule);
            let timestamp = Timestamp::from_micros(until.timestamp_micros()).ok();
            use twilight_http::request::AuditLogReason;
            if let Err(error) = app
                .http
                .update_guild_member(guild_id, message.author.id)
                .communication_disabled_until(timestamp)
                .reason(&reason)
                .await
            {
                tracing::warn!(
                    "Automod timeout failed ({guild_id}/{}): {error}",
                    message.author.id
                );
            }
        }

        if config::is_true(actions, &["warnInChannel"]) {
            let template = actions["warnTemplate"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("Wiadomosc od {user} zostala usunieta przez AutoMod ({rule}).");
            let text = template
                .replace("{user}", &format!("<@{}>", message.author.id))
                .replace("{rule}", violation.rule);
            if let Ok(response) = app
                .http
                .create_message(message.channel_id)
                .content(&text)
                .await
                && let Ok(warning) = response.model().await
            {
                let delete_after = config::int(actions, &["warnDeleteAfterSeconds"]);
                if delete_after > 0 {
                    let http = Arc::clone(&app.http);
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_secs(delete_after as u64)).await;
                        let _ = http.delete_message(warning.channel_id, warning.id).await;
                    });
                }
            }
        }

        self.send_log(app, cfg, message, violation).await;
    }

    /// discord.js `member.moderatable`: not owner, bot has Moderate Members and a higher role.
    fn moderatable(
        &self,
        app: &App,
        guild_id: Id<twilight_model::id::marker::GuildMarker>,
        user_id: Id<twilight_model::id::marker::UserMarker>,
    ) -> bool {
        let Some(guild) = app.cache.guild(guild_id) else {
            return false;
        };
        if guild.owner_id() == user_id {
            return false;
        }
        drop(guild);
        if let Ok(perms) = app.cache.permissions().root(user_id, guild_id)
            && perms.contains(Permissions::ADMINISTRATOR)
        {
            return false;
        }
        let Some(bot) = discord::bot_root_permissions(app, guild_id) else {
            return false;
        };
        discord::has(bot, Permissions::MODERATE_MEMBERS)
            && discord::highest_role_position(app, guild_id, app.bot_id)
                > discord::highest_role_position(app, guild_id, user_id)
    }

    async fn send_log(&self, app: &App, cfg: &Value, message: &Message, violation: &Violation) {
        let Some(log_channel) = config::text(cfg, &["logChannelId"])
            .and_then(|id| discord::parse_id::<ChannelMarker>(&id))
        else {
            return;
        };
        let guild_id = message.guild_id.expect("guild message");
        let guild_name = app.guild_name(guild_id).unwrap_or_default();
        let channel_name = app
            .cache
            .channel(message.channel_id)
            .and_then(|c| c.name.clone())
            .unwrap_or_default();
        let preview: String = message.content.chars().take(500).collect();
        let preview = if preview.is_empty() {
            "[no text]".to_owned()
        } else {
            preview
        };
        let lines = [
            "AutoMod hit".to_owned(),
            format!("Guild: {guild_name} ({guild_id})"),
            format!("Channel: #{channel_name} ({})", message.channel_id),
            format!(
                "User: {} ({})",
                discord::user_tag(&message.author),
                message.author.id
            ),
            format!("Rule: {}", violation.rule),
            format!("Details: {}", violation.details),
            format!("Content: {preview}"),
        ];
        let payload = json!({
            "flags": FLAG_COMPONENTS_V2,
            "components": [discord::container(vec![
                discord::text("## AutoMod Hit"),
                discord::text(lines.join("\n")),
            ])]
        });
        if let Err(error) = discord::send_message(app, log_channel, &payload).await {
            tracing::warn!("Automod log failed: {error}");
        }
    }

    /// `/automod test` (`testContent`).
    pub fn test_content(&self, content: &str, mode: &str, value: Option<&str>) -> TestResult {
        let cfg = self.cfg();
        let mentions = count_mentioned_users(content);
        let violation = match mode {
            "word" => self.detect(
                &json!({ "blockedWords": value.map(|v| vec![v]).unwrap_or_default() }),
                content,
                mentions,
                None,
            ),
            "regex" => self.detect(
                &json!({ "blockedRegex": value.map(|v| vec![v]).unwrap_or_default() }),
                content,
                mentions,
                None,
            ),
            _ => self.detect(&cfg["rules"], content, mentions, None),
        };
        TestResult {
            violation,
            note: config::is_true(&cfg, &["rules", "duplicate", "enabled"]).then_some(
                "Duplicate rule nie jest symulowana w /automod test bez historii wiadomosci.",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn automod(rules: Value) -> AutoMod {
        AutoMod::new(&json!({ "automod": { "enabled": true, "rules": rules } }))
    }

    #[test]
    fn blocked_word_uses_ascii_word_boundaries() {
        let am = automod(json!({ "blockedWords": ["kot"] }));
        let rules = am.cfg()["rules"].clone();
        assert!(am.detect(&rules, "Mam KOT w domu", 0, None).is_some());
        assert!(am.detect(&rules, "kotek", 0, None).is_none());
        // JS \W treats Polish letters as non-word characters.
        assert!(am.detect(&rules, "kotą", 0, None).is_some());
    }

    #[test]
    fn links_invites_caps_and_duplicates() {
        let am = automod(json!({
            "blockInvites": true,
            "blockLinks": true,
            "allowedDomains": ["example.com"],
            "caps": { "enabled": true, "minLength": 5, "ratio": 0.8 },
            "duplicate": { "enabled": true, "windowSeconds": 30, "maxRepeats": 3 }
        }));
        let rules = am.cfg()["rules"].clone();
        assert_eq!(
            am.detect(&rules, "join discord.gg/abc", 0, None)
                .unwrap()
                .rule,
            "invite_link"
        );
        assert!(
            am.detect(&rules, "see https://sub.example.com/x", 0, None)
                .is_none()
        );
        assert_eq!(
            am.detect(&rules, "see https://evil.org", 0, None)
                .unwrap()
                .details,
            "link: evil.org"
        );
        assert_eq!(
            am.detect(&rules, "HELLO WORLD", 0, None).unwrap().rule,
            "caps_spam"
        );
        assert!(am.detect(&rules, "spam", 0, Some("g:u")).is_none());
        assert!(am.detect(&rules, "spam", 0, Some("g:u")).is_none());
        assert_eq!(
            am.detect(&rules, "spam", 0, Some("g:u")).unwrap().rule,
            "duplicate_spam"
        );
    }

    #[test]
    fn mention_counting() {
        assert_eq!(
            count_mentioned_users(
                "<@123456789012345678> <@!123456789012345678> <@223456789012345678>"
            ),
            2
        );
    }
}
