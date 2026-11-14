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
        "Włącza lub wyłącza cały AutoClean",
    ),
    opt(
        "cron_schedule",
        Kind::Str,
        &["cronSchedule"],
        "Cron wykonywania czyszczenia",
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
        "Usuwaj wiadomości starsze niż X dni",
    ),
    opt(
        "delete_limit",
        Kind::Integer,
        &["deleteLimit"],
        "Limit usuwanych wiadomości na kanał",
    ),
    opt(
        "delete_pinned",
        Kind::Boolean,
        &["deletePinned"],
        "Czy usuwać przypięte wiadomości",
    ),
    opt(
        "dry_run",
        Kind::Boolean,
        &["dryRun"],
        "Tryb testowy bez usuwania",
    ),
    opt(
        "log_channel",
        Kind::Channel,
        &["logChannelId"],
        "Kanał logów AutoClean",
    ),
    opt(
        "notify_enabled",
        Kind::Boolean,
        &["notifyBeforeDelete", "enabled"],
        "Włącza ostrzeżenia przed czyszczeniem",
    ),
    opt(
        "notify_minutes_before",
        Kind::Integer,
        &["notifyBeforeDelete", "minutesBefore"],
        "Ile minut przed czyszczeniem wysłać ostrzeżenie",
    ),
    opt(
        "notify_message",
        Kind::Str,
        &["notifyBeforeDelete", "message"],
        "Treść ostrzeżenia przed czyszczeniem",
    ),
];

const AUTOMOD_OPTIONS: &[OptionDef] = &[
    opt(
        "enabled",
        Kind::Boolean,
        &["automod", "enabled"],
        "Włącza lub wyłącza AutoMod",
    ),
    opt(
        "log_channel",
        Kind::Channel,
        &["automod", "logChannelId"],
        "Kanał logów AutoMod",
    ),
    opt(
        "block_invites",
        Kind::Boolean,
        &["automod", "rules", "blockInvites"],
        "Blokowanie linków zaproszeń Discord",
    ),
    opt(
        "block_links",
        Kind::Boolean,
        &["automod", "rules", "blockLinks"],
        "Blokowanie linków spoza allowlisty",
    ),
    opt(
        "max_mentions",
        Kind::Integer,
        &["automod", "rules", "maxMentions"],
        "Maksymalna liczba mentionów",
    ),
    opt(
        "caps_enabled",
        Kind::Boolean,
        &["automod", "rules", "caps", "enabled"],
        "Włącza filtr caps spam",
    ),
    opt(
        "caps_min_length",
        Kind::Integer,
        &["automod", "rules", "caps", "minLength"],
        "Minimalna liczba liter dla caps spam",
    ),
    opt(
        "caps_ratio",
        Kind::Str,
        &["automod", "rules", "caps", "ratio"],
        "Procent wielkich liter, np. 0.75",
    ),
    opt(
        "duplicate_enabled",
        Kind::Boolean,
        &["automod", "rules", "duplicate", "enabled"],
        "Włącza filtr duplikatów",
    ),
    opt(
        "duplicate_window_seconds",
        Kind::Integer,
        &["automod", "rules", "duplicate", "windowSeconds"],
        "Okno czasowe dla duplikatów",
    ),
    opt(
        "duplicate_max_repeats",
        Kind::Integer,
        &["automod", "rules", "duplicate", "maxRepeats"],
        "Maksymalna liczba powtórek",
    ),
    opt(
        "delete_message",
        Kind::Boolean,
        &["automod", "actions", "delete"],
        "Czy usuwać wiadomość po trafieniu",
    ),
    opt(
        "timeout_minutes",
        Kind::Integer,
        &["automod", "actions", "timeoutMinutes"],
        "Timeout po wykryciu naruszenia",
    ),
    opt(
        "warn_in_channel",
        Kind::Boolean,
        &["automod", "actions", "warnInChannel"],
        "Czy wysłać ostrzeżenie na kanale",
    ),
    opt(
        "warn_template",
        Kind::Str,
        &["automod", "actions", "warnTemplate"],
        "Template ostrzeżenia",
    ),
    opt(
        "warn_delete_after_seconds",
        Kind::Integer,
        &["automod", "actions", "warnDeleteAfterSeconds"],
        "Po ilu sekundach usunąć ostrzeżenie",
    ),
];

const VERIFY_OPTIONS: &[OptionDef] = &[
    opt(
        "enabled",
        Kind::Boolean,
        &[],
        "Włącza lub wyłącza system verify",
    ),
    opt("role", Kind::Role, &[], "Rola nadawana po verify"),
    opt("exceptions", Kind::Info, &[], "Lista wyjątków verify"),
    opt(
        "exception_add",
        Kind::User,
        &[],
        "Dodaje usera do wyjątków verify",
    ),
    opt(
        "exception_remove",
        Kind::User,
        &[],
        "Usuwa usera z wyjątków verify",
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
        Value::Null => "brak".to_owned(),
        Value::String(s) if s.is_empty() => "brak".to_owned(),
        Value::Bool(b) => if *b { "tak" } else { "nie" }.to_owned(),
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
        "{}...[obcięto]",
        String::from_utf16_lossy(&units[..max - 15])
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
        None => json!("brak"),
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
                    .map_or("brak".to_owned(), |r| format!("<@&{r}> ({r})"))
            ),
        ),
        (
            "panel_channel",
            json!(
                panel["channelId"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map_or("brak".to_owned(), |c| format!("<#{c}> ({c})"))
            ),
        ),
        (
            "panel_message_id",
            json!(text_or(&panel["messageId"], "brak")),
        ),
        (
            "panel_custom_id",
            json!(text_or(&panel["customId"], "brak")),
        ),
        ("exceptions_count", json!(exceptions.len())),
        (
            "exceptions",
            json!(if exceptions.is_empty() {
                "brak".to_owned()
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
            json!(text_or(&verify["lastSweepAt"], "nigdy")),
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
                "brak".to_owned()
            }
        }
        ("verify", Kind::Role, _) => {
            if truthy(value) {
                format!("<@&{}>", config::js_string(value))
            } else {
                "brak".to_owned()
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
                "brak".to_owned()
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
            .reply_ephemeral("Ta komenda dziala tylko na serwerze.")
            .await;
    };
    if !ix.member_has(Permissions::ADMINISTRATOR) {
        return ix.reply_ephemeral("Brak uprawnień Administratora.").await;
    }
    let guild = guild_id.to_string();
    let app = ix.app;
    let opts = ix.opts();
    let mode = opts.lower("mode");
    let section = opts.string("section");
    let option_key = normalize_option_key(&opts.string("option").unwrap_or_default());
    let cfg = match config::read(&app.paths.config_path) {
        Ok(cfg) => cfg,
        Err(error) => {
            return ix
                .reply_ephemeral(config::format_config_error(
                    &error,
                    "Nie można odczytać config.json. ",
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
                        "value": format!("Status: {enabled}\nUżyj `/settings mode:show section:{key}`"),
                        "inline": false,
                    })
                })
                .collect();
            let mut e = embed(
                "Operator Settings",
                Some("Podgląd najważniejszych sekcji ustawień."),
                fields,
            );
            e["footer"] = json!({ "text": "Sekcje: autoclean, automod, verify" });
            return reply_embed(ix, e).await;
        };
        let Some((key, label, options)) = find_section(&section) else {
            return ix
                .reply_ephemeral(format!("Nieznana sekcja: {section}"))
                .await;
        };
        if option_key.is_empty() {
            let snapshot = section_snapshot(app, key, &cfg, &guild);
            let e = embed(
                &format!("{label} Settings"),
                Some(&format!("Dostępne opcje: {}", option_keys(options))),
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
                    .reply_ephemeral("Podaj `channel_value` albo użyj komendy na kanale tekstowym.")
                    .await;
            };
            let s = app
                .autoclean
                .effective_channel_settings(&channel_id.to_string(), None);
            let snapshot = vec![
                ("enabled", json!(if s.enabled { "tak" } else { "nie" })),
                (
                    "delete_limit",
                    if s.delete_limit > 0 {
                        json!(s.delete_limit)
                    } else {
                        json!("brak")
                    },
                ),
                (
                    "delete_pinned",
                    json!(if s.delete_pinned { "tak" } else { "nie" }),
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
                Some(&format!("Kanał: <#{channel_id}>")),
                snapshot_fields(&snapshot),
            );
            return reply_embed(ix, e).await;
        }
        let Some(def) = options.iter().find(|o| o.key == option_key) else {
            return ix
                .reply_ephemeral(format!(
                    "Nieznana opcja `{option_key}`. Dostępne: {}, channel_settings",
                    option_keys(options)
                ))
                .await;
        };
        let current = option_current(app, key, &cfg, def, &guild);
        let e = embed(
            &format!("{label} / {option_key}"),
            None,
            vec![
                json!({ "name": "Typ", "value": truncate_value(def.kind.name(), 1024), "inline": true }),
                json!({ "name": "Aktualnie", "value": truncate_value(&format_option(key, def, &current), 1024), "inline": true }),
                json!({ "name": "Opis", "value": truncate_value(def.description, 1024), "inline": false }),
            ],
        );
        return reply_embed(ix, e).await;
    }

    if mode != "set" {
        return ix
            .reply_ephemeral(format!("Nieznany mode settings: {mode}."))
            .await;
    }
    let Some(section) = section else {
        return ix.reply_ephemeral("Dla mode=set podaj `section`.").await;
    };
    if option_key.is_empty() {
        return ix.reply_ephemeral("Dla mode=set podaj `option`.").await;
    }
    let Some((key, _, options)) = find_section(&section) else {
        return ix
            .reply_ephemeral(format!("Nieznana sekcja: {section}"))
            .await;
    };
    let Some(def) = options.iter().find(|o| o.key == option_key) else {
        return ix
            .reply_ephemeral(format!(
                "Nieznana opcja `{option_key}`. Dostępne: {}",
                option_keys(options)
            ))
            .await;
    };

    let string_value = opts.string("string_value").unwrap_or_default();
    let next: Result<Value, &str> = match def.kind {
        Kind::Boolean => opts
            .boolean("boolean_value")
            .map(Value::Bool)
            .ok_or("Ta opcja wymaga `boolean_value`."),
        Kind::Integer => opts
            .integer("integer_value")
            .map(|v| json!(v))
            .ok_or("Ta opcja wymaga `integer_value`."),
        Kind::Str => {
            let raw = string_value.trim();
            if raw.is_empty() {
                Err("Ta opcja wymaga `string_value`.")
            } else if def.path == ["automod", "rules", "caps", "ratio"] {
                match raw.parse::<f64>() {
                    Ok(parsed) if parsed.is_finite() && (0.0..=1.0).contains(&parsed) => {
                        Ok(json!(parsed))
                    }
                    _ => Err("caps_ratio musi być liczbą od 0 do 1."),
                }
            } else {
                Ok(json!(raw))
            }
        }
        Kind::Channel => match opts.channel("channel_value") {
            Some((id, _)) => Ok(json!(id.to_string())),
            None if string_value.trim().eq_ignore_ascii_case("clear") => Ok(json!("")),
            None => Err("Ta opcja wymaga `channel_value` albo `string_value: clear`."),
        },
        Kind::Role => match opts.role("role_value") {
            Some(role) => Ok(json!(role.id.to_string())),
            None if string_value.trim().eq_ignore_ascii_case("clear") => Ok(Value::Null),
            None => Err("Ta opcja wymaga `role_value` albo `string_value: clear`."),
        },
        Kind::User => opts
            .user("user_value")
            .map(|u| json!(u.id.to_string()))
            .ok_or("Ta opcja wymaga `user_value`."),
        Kind::Info => Err("Nieobsługiwany typ opcji."),
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
        let mut updated = cfg.clone();
        config::set_path(&mut updated, def.path, next);
        if let Err(error) = config::write(&app.paths.config_path, &updated) {
            return ix
                .reply_ephemeral(config::format_config_error(
                    &error,
                    "Nie można zapisać config.json. ",
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
                "Ustawienie zapisano, ale moduł nie mógł przeładować configu. ",
            ))
            .await;
    }
    let updated = match config::read(&app.paths.config_path) {
        Ok(cfg) => cfg,
        Err(error) => {
            return ix
                .reply_ephemeral(config::format_config_error(
                    &error,
                    "Ustawienie zapisano, ale nie udało się ponownie odczytać config.json. ",
                ))
                .await;
        }
    };
    let current = option_current(app, key, &updated, def, &guild);
    ix.reply_ephemeral(format!(
        "Zmieniono `{section}.{option_key}` na: {}",
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
        assert_eq!(format_value(&json!(true)), "tak");
        assert_eq!(format_value(&json!("")), "brak");
        assert_eq!(format_value(&json!([1, "a"])), "1, a");
        assert_eq!(format_value(&json!([])), "[]");
        assert_eq!(
            truncate_value(&"x".repeat(1100), 1024)
                .encode_utf16()
                .count(),
            1009 + 12
        );
    }
}
