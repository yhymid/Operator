//! `/settings` (port of `src/cmds/settings.js`).

use anyhow::Result;
use serde_json::{Value, json};
use twilight_model::guild::Permissions;

use crate::app::App;
use crate::config;
use crate::discord::{self, FLAG_EPHEMERAL, Ix};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Boolean,
    Str,
    Integer,
    Channel,
    Role,
    User,
    Info,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Boolean => "boolean",
            Self::Str => "string",
            Self::Integer => "integer",
            Self::Channel => "channel",
            Self::Role => "role",
            Self::User => "user",
            Self::Info => "info",
        }
    }
}

struct OptionDef {
    key: &'static str,
    kind: Kind,
    path: &'static [&'static str],
    description: &'static str,
}

const fn opt(
    key: &'static str,
    kind: Kind,
    path: &'static [&'static str],
    description: &'static str,
) -> OptionDef {
    OptionDef {
        key,
        kind,
        path,
        description,
    }
}

const AUTOCLEAN_OPTIONS: &[OptionDef] = &[
    opt(
        "enabled",
        Kind::Boolean,
        &["enabled"],
        "Turns all of AutoClean on or off",
    ),
    opt(
        "cron_schedule",
        Kind::Str,
        &["cronSchedule"],
        "Cleanup cron schedule",
    ),
    opt(
        "timezone",
        Kind::Str,
        &["timezone"],
        "Timezone cron, np. Europe/Warsaw",
    ),
    opt(
        "delete_messages_older_than_days",
        Kind::Integer,
        &["deleteMessagesOlderThanDays"],
        "Delete messages older than X days",
    ),
    opt(
        "delete_limit",
        Kind::Integer,
        &["deleteLimit"],
        "Limit of deleted messages per channel",
    ),
    opt(
        "delete_pinned",
        Kind::Boolean,
        &["deletePinned"],
        "Whether to delete pinned messages",
    ),
    opt(
        "dry_run",
        Kind::Boolean,
        &["dryRun"],
        "Test mode without deleting",
    ),
    opt(
        "log_channel",
        Kind::Channel,
        &["logChannelId"],
        "AutoClean log channel",
    ),
    opt(
        "notify_enabled",
        Kind::Boolean,
        &["notifyBeforeDelete", "enabled"],
        "Enables warnings before a cleanup",
    ),
    opt(
        "notify_minutes_before",
        Kind::Integer,
        &["notifyBeforeDelete", "minutesBefore"],
        "How many minutes before a cleanup to send a warning",
    ),
    opt(
        "notify_message",
        Kind::Str,
        &["notifyBeforeDelete", "message"],
        "Text of the pre-cleanup warning",
    ),
];

const AUTOMOD_OPTIONS: &[OptionDef] = &[
    opt(
        "enabled",
        Kind::Boolean,
        &["automod", "enabled"],
        "Turns AutoMod on or off",
    ),
    opt(
        "log_channel",
        Kind::Channel,
        &["automod", "logChannelId"],
        "AutoMod log channel",
    ),
    opt(
        "block_invites",
        Kind::Boolean,
        &["automod", "rules", "blockInvites"],
        "Block Discord invite links",
    ),
    opt(
        "block_links",
        Kind::Boolean,
        &["automod", "rules", "blockLinks"],
        "Block links outside the allowlist",
    ),
    opt(
        "max_mentions",
        Kind::Integer,
        &["automod", "rules", "maxMentions"],
        "Maximum number of mentions",
    ),
    opt(
        "caps_enabled",
        Kind::Boolean,
        &["automod", "rules", "caps", "enabled"],
        "Enables the caps spam filter",
    ),
    opt(
        "caps_min_length",
        Kind::Integer,
        &["automod", "rules", "caps", "minLength"],
        "Minimum letters for caps spam",
    ),
    opt(
        "caps_ratio",
        Kind::Str,
        &["automod", "rules", "caps", "ratio"],
        "Share of capital letters, e.g. 0.75",
    ),
    opt(
        "duplicate_enabled",
        Kind::Boolean,
        &["automod", "rules", "duplicate", "enabled"],
        "Enables the duplicate filter",
    ),
    opt(
        "duplicate_window_seconds",
        Kind::Integer,
        &["automod", "rules", "duplicate", "windowSeconds"],
        "Time window for duplicates",
    ),
    opt(
        "duplicate_max_repeats",
        Kind::Integer,
        &["automod", "rules", "duplicate", "maxRepeats"],
        "Maximum number of repeats",
    ),
    opt(
        "delete_message",
        Kind::Boolean,
        &["automod", "actions", "delete"],
        "Whether to delete the message on a match",
    ),
    opt(
        "timeout_minutes",
        Kind::Integer,
        &["automod", "actions", "timeoutMinutes"],
        "Timeout after a violation",
    ),
    opt(
        "warn_in_channel",
        Kind::Boolean,
        &["automod", "actions", "warnInChannel"],
        "Whether to send a warning in the channel",
    ),
    opt(
        "warn_template",
        Kind::Str,
        &["automod", "actions", "warnTemplate"],
        "Warning template",
    ),
    opt(
        "warn_delete_after_seconds",
        Kind::Integer,
        &["automod", "actions", "warnDeleteAfterSeconds"],
        "Seconds before the warning is deleted",
    ),
];

const VERIFY_OPTIONS: &[OptionDef] = &[
    opt(
        "enabled",
        Kind::Boolean,
        &[],
        "Turns the verify system on or off",
    ),
    opt("role", Kind::Role, &[], "Role given after verify"),
    opt("exceptions", Kind::Info, &[], "Verify exception list"),
    opt(
        "exception_add",
        Kind::User,
        &[],
        "Adds a user to the verify exceptions",
    ),
    opt(
        "exception_remove",
        Kind::User,
        &[],
        "Removes a user from the verify exceptions",
    ),
];

const SECTIONS: [(&str, &str, &[OptionDef]); 3] = [
    ("autoclean", "AutoClean", AUTOCLEAN_OPTIONS),
    ("automod", "AutoMod", AUTOMOD_OPTIONS),
    ("verify", "Verify", VERIFY_OPTIONS),
];

fn normalize_option_key(value: &str) -> String {
    let lower = value.trim().to_lowercase();
    let mut out = String::new();
    let mut in_sep = false;
    for c in lower.chars() {
        if c.is_whitespace() || c == '.' || c == '-' {
            if !in_sep {
                out.push('_');
            }
            in_sep = true;
        } else {
            in_sep = false;
            out.push(c);
        }
    }
    out
}

/// `formatValue`
pub fn format_value(value: &Value) -> String {
    match value {
        Value::Null => "none".to_owned(),
        Value::String(s) if s.is_empty() => "none".to_owned(),
        Value::Bool(b) => if *b { "yes" } else { "no" }.to_owned(),
        Value::Array(items) if items.is_empty() => "[]".to_owned(),
        Value::Array(items) => items
            .iter()
            .map(config::js_string)
            .collect::<Vec<_>>()
            .join(", "),
        Value::Object(_) => format!(
            "```json\n{}\n```",
            serde_json::to_string_pretty(value).unwrap_or_default()
        ),
        other => config::js_string(other),
    }
}

pub fn truncate_value(text: &str, max: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() <= max {
        return text.to_owned();
    }
    format!(
        "{}...[truncated]",
        String::from_utf16_lossy(&units[..max.saturating_sub(14)])
    )
}

/// `Number(x || 0) || 0` as a JSON number.
fn num(cfg: &Value, path: &[&str]) -> Value {
    let value = config::number(cfg, path);
    let value = if value.is_finite() { value } else { 0.0 };
    if value.fract() == 0.0 {
        json!(value as i64)
    } else {
        json!(value)
    }
}

fn channel_mention(cfg: &Value, path: &[&str]) -> Value {
    match config::text(cfg, path) {
        Some(id) => json!(format!("<#{id}>")),
        None => json!("none"),
    }
}

fn verify_snapshot(app: &App, guild_id: &str) -> Vec<(&'static str, Value)> {
    let verify = app.state.verification(guild_id);
    let panel = &verify["panel"];
    let exceptions = config::string_list(&verify, &["exceptions"]);
    let count = |key: &str| verify[key].as_object().map_or(0, |m| m.len());
    let text_or = |value: &Value, fallback: &str| {
        value
            .as_str()
            .filter(|s| !s.is_empty())
            .map_or_else(|| fallback.to_owned(), str::to_owned)
    };
    vec![
        ("enabled", json!(verify["enabled"] == Value::Bool(true))),
        (
            "role",
            json!(
                verify["roleId"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map_or("none".to_owned(), |r| format!("<@&{r}> ({r})"))
            ),
        ),
        (
            "panel_channel",
            json!(
                panel["channelId"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map_or("none".to_owned(), |c| format!("<#{c}> ({c})"))
            ),
        ),
        (
            "panel_message_id",
            json!(text_or(&panel["messageId"], "none")),
        ),
        (
            "panel_custom_id",
            json!(text_or(&panel["customId"], "none")),
        ),
        ("exceptions_count", json!(exceptions.len())),
        (
            "exceptions",
            json!(if exceptions.is_empty() {
                "none".to_owned()
            } else {
                exceptions
                    .iter()
                    .map(|id| format!("<@{id}> ({id})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            }),
        ),
        ("active_expirations", json!(count("expirations"))),
        ("active_challenges", json!(count("challenges"))),
        ("active_lockouts", json!(count("lockouts"))),
        (
            "last_sweep",
            json!(text_or(&verify["lastSweepAt"], "never")),
        ),
    ]
}

fn section_snapshot(
    app: &App,
    section: &str,
    cfg: &Value,
    guild_id: &str,
) -> Vec<(&'static str, Value)> {
    match section {
        "autoclean" => vec![
            ("enabled", json!(config::not_false(cfg, &["enabled"]))),
            (
                "cron_schedule",
                json!(
                    config::text(cfg, &["cronSchedule"]).unwrap_or_else(|| "0 3 * * 0".to_owned())
                ),
            ),
            (
                "timezone",
                json!(
                    config::text(cfg, &["timezone"]).unwrap_or_else(|| "Europe/Warsaw".to_owned())
                ),
            ),
            (
                "delete_messages_older_than_days",
                num(cfg, &["deleteMessagesOlderThanDays"]),
            ),
            ("delete_limit", num(cfg, &["deleteLimit"])),
            (
                "delete_pinned",
                json!(config::is_true(cfg, &["deletePinned"])),
            ),
            ("dry_run", json!(config::is_true(cfg, &["dryRun"]))),
            ("log_channel", channel_mention(cfg, &["logChannelId"])),
            (
                "notify_enabled",
                json!(config::is_true(cfg, &["notifyBeforeDelete", "enabled"])),
            ),
            (
                "notify_minutes_before",
                num(cfg, &["notifyBeforeDelete", "minutesBefore"]),
            ),
            (
                "notify_message",
                config::text(cfg, &["notifyBeforeDelete", "message"])
                    .map_or(Value::Null, Value::String),
            ),
            (
                "channels_enabled",
                json!(cfg["channels"].as_array().map_or(0, |c| {
                    c.iter()
                        .filter(|i| i["enabled"] != Value::Bool(false))
                        .count()
                })),
            ),
        ],
        "automod" => vec![
            (
                "enabled",
                json!(config::is_true(cfg, &["automod", "enabled"])),
            ),
            (
                "log_channel",
                channel_mention(cfg, &["automod", "logChannelId"]),
            ),
            (
                "block_invites",
                json!(config::is_true(cfg, &["automod", "rules", "blockInvites"])),
            ),
            (
                "block_links",
                json!(config::is_true(cfg, &["automod", "rules", "blockLinks"])),
            ),
            (
                "max_mentions",
                num(cfg, &["automod", "rules", "maxMentions"]),
            ),
            (
                "caps_enabled",
                json!(config::is_true(
                    cfg,
                    &["automod", "rules", "caps", "enabled"]
                )),
            ),
            (
                "caps_min_length",
                num(cfg, &["automod", "rules", "caps", "minLength"]),
            ),
            (
                "caps_ratio",
                num(cfg, &["automod", "rules", "caps", "ratio"]),
            ),
            (
                "duplicate_enabled",
                json!(config::is_true(
                    cfg,
                    &["automod", "rules", "duplicate", "enabled"]
                )),
            ),
            (
                "duplicate_window_seconds",
                num(cfg, &["automod", "rules", "duplicate", "windowSeconds"]),
            ),
            (
                "duplicate_max_repeats",
                num(cfg, &["automod", "rules", "duplicate", "maxRepeats"]),
            ),
            (
                "delete_message",
                json!(config::is_true(cfg, &["automod", "actions", "delete"])),
            ),
            (
                "timeout_minutes",
                num(cfg, &["automod", "actions", "timeoutMinutes"]),
            ),
            (
                "warn_in_channel",
                json!(config::is_true(
                    cfg,
                    &["automod", "actions", "warnInChannel"]
                )),
            ),
            (
                "warn_template",
                config::text(cfg, &["automod", "actions", "warnTemplate"])
                    .map_or(Value::Null, Value::String),
            ),
            (
                "warn_delete_after_seconds",
                num(cfg, &["automod", "actions", "warnDeleteAfterSeconds"]),
            ),
        ],
        _ => verify_snapshot(app, guild_id),
    }
}

fn option_current(app: &App, section: &str, cfg: &Value, def: &OptionDef, guild_id: &str) -> Value {
    if section != "verify" {
        return config::at(cfg, def.path).cloned().unwrap_or(Value::Null);
    }
    let verify = app.state.verification(guild_id);
    match def.key {
        "enabled" => json!(verify["enabled"] == Value::Bool(true)),
        "role" => verify["roleId"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map_or(Value::Null, |s| json!(s)),
        _ => json!(app.state.verification_exceptions(guild_id)),
    }
}

fn format_option(section: &str, def: &OptionDef, value: &Value) -> String {
    let truthy =
        |v: &Value| !matches!(v, Value::Null | Value::Bool(false)) && v.as_str() != Some("");
    match (section, def.kind, def.key) {
        (_, Kind::Channel, _) => {
            if truthy(value) {
                format!("<#{}>", config::js_string(value))
            } else {
                "none".to_owned()
            }
        }
        ("verify", Kind::Role, _) => {
            if truthy(value) {
                format!("<@&{}>", config::js_string(value))
            } else {
                "none".to_owned()
            }
        }
        ("verify", _, "exceptions" | "exception_add" | "exception_remove") => {
            let ids: Vec<String> = value
                .as_array()
                .into_iter()
                .flatten()
                .map(config::js_string)
                .collect();
            if ids.is_empty() {
                "none".to_owned()
            } else {
                ids.iter()
                    .map(|id| format!("<@{id}> ({id})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        }
        _ => format_value(value),
    }
}

fn embed(title: &str, description: Option<&str>, fields: Vec<Value>) -> Value {
    let mut embed = json!({
        "color": 0x2f9e44,
        "title": title,
        "timestamp": discord::embed_timestamp_now(),
        "fields": fields,
    });
    if let Some(description) = description {
        embed["description"] = json!(description);
    }
    embed
}

fn snapshot_fields(snapshot: &[(&str, Value)]) -> Vec<Value> {
    snapshot
        .iter()
        .map(|(key, value)| json!({ "name": key, "value": truncate_value(&format_value(value), 1024), "inline": false }))
        .collect()
}

async fn reply_embed(ix: &Ix<'_>, embed: Value) -> Result<()> {
    ix.reply(json!({ "embeds": [embed], "flags": FLAG_EPHEMERAL }))
        .await
}

fn option_keys(options: &[OptionDef]) -> String {
    options.iter().map(|o| o.key).collect::<Vec<_>>().join(", ")
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
    let guild = guild_id.to_string();
    let app = ix.app;
    let opts = ix.opts();
    let mode = opts.subcommand.unwrap_or_default().to_owned();
    let section = opts.string("section");
    let option_key = normalize_option_key(&opts.string("option").unwrap_or_default());
    let cfg = match config::read(&app.paths.config_path) {
        Ok(cfg) => cfg,
        Err(error) => {
            return ix
                .reply_ephemeral(config::format_config_error(
                    &error,
                    "Cannot read config.json. ",
                ))
                .await;
        }
    };
    let find_section = |name: &str| SECTIONS.iter().find(|(key, _, _)| *key == name);

    if mode == "show" {
        let Some(section) = section else {
            let fields: Vec<Value> = SECTIONS
                .iter()
                .map(|(key, label, _)| {
                    let snapshot = section_snapshot(app, key, &cfg, &guild);
                    let enabled = snapshot
                        .iter()
                        .find(|(k, _)| *k == "enabled")
                        .map_or("n/a".to_owned(), |(_, v)| format_value(v));
                    json!({
                        "name": label,
                        "value": format!("Status: {enabled}\nUse `/settings show section:{key}`"),
                        "inline": false,
                    })
                })
                .collect();
            let mut e = embed(
                "Operator Settings",
                Some("Overview of the main settings sections."),
                fields,
            );
            e["footer"] = json!({ "text": "Sections: autoclean, automod, verify" });
            return reply_embed(ix, e).await;
        };
        let Some((key, label, options)) = find_section(&section) else {
            return ix
                .reply_ephemeral(format!("Unknown section: {section}"))
                .await;
        };
        if option_key.is_empty() {
            let snapshot = section_snapshot(app, key, &cfg, &guild);
            let e = embed(
                &format!("{label} Settings"),
                Some(&format!("Available options: {}", option_keys(options))),
                snapshot_fields(&snapshot),
            );
            return reply_embed(ix, e).await;
        }
        if *key == "autoclean" && option_key == "channel_settings" {
            let channel = opts
                .channel("channel_value")
                .or_else(|| ix.interaction.channel.as_ref().map(|c| (c.id, c.kind)))
                .filter(|(_, kind)| discord::is_text_based(*kind));
            let Some((channel_id, _)) = channel else {
                return ix
                    .reply_ephemeral(
                        "Provide `channel_value` or run the command in a text channel.",
                    )
                    .await;
            };
            let s = app
                .autoclean
                .effective_channel_settings(&channel_id.to_string(), None);
            let snapshot = vec![
                ("enabled", json!(if s.enabled { "yes" } else { "no" })),
                (
                    "delete_limit",
                    if s.delete_limit > 0 {
                        json!(s.delete_limit)
                    } else {
                        json!("none")
                    },
                ),
                (
                    "delete_pinned",
                    json!(if s.delete_pinned { "yes" } else { "no" }),
                ),
                (
                    "delete_messages_older_than_days",
                    json!(s.delete_older_than_days),
                ),
                ("whitelist_users", json!(s.whitelist_users.len())),
                ("whitelist_roles", json!(s.whitelist_roles.len())),
                (
                    "notify_before_delete",
                    json!(if s.notify_enabled {
                        format!("{} min", s.notify_minutes)
                    } else {
                        "off".to_owned()
                    }),
                ),
            ];
            let e = embed(
                "AutoClean Channel Settings",
                Some(&format!("Channel: <#{channel_id}>")),
                snapshot_fields(&snapshot),
            );
            return reply_embed(ix, e).await;
        }
        let Some(def) = options.iter().find(|o| o.key == option_key) else {
            return ix
                .reply_ephemeral(format!(
                    "Unknown option `{option_key}`. Available: {}, channel_settings",
                    option_keys(options)
                ))
                .await;
        };
        let current = option_current(app, key, &cfg, def, &guild);
        let e = embed(
            &format!("{label} / {option_key}"),
            None,
            vec![
                json!({ "name": "Type", "value": truncate_value(def.kind.name(), 1024), "inline": true }),
                json!({ "name": "Current", "value": truncate_value(&format_option(key, def, &current), 1024), "inline": true }),
                json!({ "name": "Description", "value": truncate_value(def.description, 1024), "inline": false }),
            ],
        );
        return reply_embed(ix, e).await;
    }

    if mode != "set" {
        return ix
            .reply_ephemeral(format!("Unknown settings mode: {mode}."))
            .await;
    }
    let Some(section) = section else {
        return ix.reply_ephemeral("Provide `section`.").await;
    };
    if option_key.is_empty() {
        return ix.reply_ephemeral("Provide `option`.").await;
    }
    let Some((key, _, options)) = find_section(&section) else {
        return ix
            .reply_ephemeral(format!("Unknown section: {section}"))
            .await;
    };
    let Some(def) = options.iter().find(|o| o.key == option_key) else {
        return ix
            .reply_ephemeral(format!(
                "Unknown option `{option_key}`. Available: {}",
                option_keys(options)
            ))
            .await;
    };

    let string_value = opts.string("string_value").unwrap_or_default();
    let next: Result<Value, &str> = match def.kind {
        Kind::Boolean => opts
            .boolean("boolean_value")
            .map(Value::Bool)
            .ok_or("This option needs `boolean_value`."),
        Kind::Integer => opts
            .integer("integer_value")
            .map(|v| json!(v))
            .ok_or("This option needs `integer_value`."),
        Kind::Str => {
            let raw = string_value.trim();
            if raw.is_empty() {
                Err("This option needs `string_value`.")
            } else if def.path == ["automod", "rules", "caps", "ratio"] {
                match raw.parse::<f64>() {
                    Ok(parsed) if parsed.is_finite() && (0.0..=1.0).contains(&parsed) => {
                        Ok(json!(parsed))
                    }
                    _ => Err("caps_ratio must be a number from 0 to 1."),
                }
            } else {
                Ok(json!(raw))
            }
        }
        Kind::Channel => match opts.channel("channel_value") {
            Some((id, _)) => Ok(json!(id.to_string())),
            None if string_value.trim().eq_ignore_ascii_case("clear") => Ok(json!("")),
            None => Err("This option needs `channel_value` or `string_value: clear`."),
        },
        Kind::Role => match opts.role("role_value") {
            Some(role) => Ok(json!(role.id.to_string())),
            None if string_value.trim().eq_ignore_ascii_case("clear") => Ok(Value::Null),
            None => Err("This option needs `role_value` or `string_value: clear`."),
        },
        Kind::User => opts
            .user("user_value")
            .map(|u| json!(u.id.to_string()))
            .ok_or("This option needs `user_value`."),
        Kind::Info => Err("Unsupported option type."),
    };
    let next = match next {
        Ok(next) => next,
        Err(message) => return ix.reply_ephemeral(message).await,
    };

    if *key == "verify" {
        match def.key {
            "enabled" => {
                app.state
                    .save_verification(&guild, json!({ "enabled": next == Value::Bool(true) }));
            }
            "role" => {
                let role = if next.as_str().is_some_and(|s| !s.is_empty()) {
                    next.clone()
                } else {
                    Value::Null
                };
                app.state
                    .save_verification(&guild, json!({ "roleId": role }));
            }
            "exception_add" => app
                .state
                .add_verification_exception(&guild, &config::js_string(&next)),
            "exception_remove" => app
                .state
                .remove_verification_exception(&guild, &config::js_string(&next)),
            _ => {}
        }
    } else {
        // Re-read under the lock: `cfg` is from before the awaits above.
        if let Err(error) = config::update(&app.paths.config_path, |updated| {
            config::set_path(updated, def.path, next);
        }) {
            return ix
                .reply_ephemeral(config::format_config_error(
                    &error,
                    "Cannot write config.json. ",
                ))
                .await;
        }
    }

    let reload = match *key {
        "autoclean" => app.autoclean.start(app),
        "automod" => app.automod.start(app),
        _ => Ok(()),
    };
    if let Err(error) = reload {
        return ix
            .reply_ephemeral(config::format_config_error(
                &error,
                "The setting was saved, but the module could not reload the config. ",
            ))
            .await;
    }
    let updated = match config::read(&app.paths.config_path) {
        Ok(cfg) => cfg,
        Err(error) => {
            return ix
                .reply_ephemeral(config::format_config_error(
                    &error,
                    "The setting was saved, but config.json could not be read back. ",
                ))
                .await;
        }
    };
    let current = option_current(app, key, &updated, def, &guild);
    ix.reply_ephemeral(format!(
        "Set `{section}.{option_key}` to: {}",
        format_option(key, def, &current)
    ))
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn option_keys_normalize() {
        assert_eq!(normalize_option_key(" Dry Run "), "dry_run");
        assert_eq!(normalize_option_key("caps.ratio"), "caps_ratio");
        assert_eq!(normalize_option_key("a - b"), "a_b");
    }

    #[test]
    fn values_format_like_js() {
        assert_eq!(format_value(&json!(true)), "yes");
        assert_eq!(format_value(&json!("")), "none");
        assert_eq!(format_value(&json!([1, "a"])), "1, a");
        assert_eq!(format_value(&json!([])), "[]");
        assert_eq!(
            truncate_value(&"x".repeat(1100), 1024)
                .encode_utf16()
                .count(),
            1024
        );
    }
}
