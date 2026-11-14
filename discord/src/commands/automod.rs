//! `/automod` (port of `src/cmds/automod.js`).

use std::collections::HashSet;

use anyhow::Result;
use serde_json::{Value, json};
use twilight_model::guild::Permissions;

use crate::app::App;
use crate::config::{self, ConfigError};
use crate::discord::Ix;
use crate::util::jsregex;
use crate::util::text::compact_whitespace;

const RULE_TYPES: [(&str, &str, &str); 3] = [
    ("word", "Blocked words", "blockedWords"),
    ("regex", "Blocked regex", "blockedRegex"),
    ("allowed_domain", "Allowed domains", "allowedDomains"),
];
const TEST_TYPES: [&str; 3] = ["live", "word", "regex"];

fn rule_meta(kind: &str) -> Option<(&'static str, &'static str)> {
    RULE_TYPES
        .iter()
        .find(|(k, _, _)| *k == kind)
        .map(|(_, label, key)| (*label, *key))
}

fn normalize_rule_value(kind: &str, value: &str) -> Result<String, String> {
    match kind {
        "word" => {
            let normalized = compact_whitespace(value).to_lowercase();
            if normalized.is_empty() {
                return Err("Reguła słowa nie może być pusta.".to_owned());
            }
            Ok(normalized)
        }
        "regex" => {
            let normalized = value.trim();
            if normalized.is_empty() {
                return Err("Reguła regex nie może być pusta.".to_owned());
            }
            jsregex::compile(normalized, false)
                .map_err(|error| format!("Niepoprawny regex: {error}"))?;
            Ok(normalized.to_owned())
        }
        "allowed_domain" => {
            let raw = value.trim().to_lowercase();
            if raw.is_empty() {
                return Err("Dozwolona domena nie może być pusta.".to_owned());
            }
            let candidate = if raw.contains("://") {
                raw
            } else {
                format!("https://{raw}")
            };
            let invalid = || "Niepoprawna domena. Użyj np. `example.com`.".to_owned();
            let host = url::Url::parse(&candidate)
                .ok()
                .and_then(|u| u.host_str().map(str::to_lowercase))
                .ok_or_else(invalid)?;
            let normalized = host.trim_end_matches('.').to_owned();
            if normalized.is_empty() {
                return Err(invalid());
            }
            Ok(normalized)
        }
        other => Err(format!("Nieznany typ reguły: {other}")),
    }
}

fn sanitize_list(kind: &str, values: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .iter()
        .filter_map(|v| normalize_rule_value(kind, v).ok())
        .filter(|v| {
            seen.insert(if kind == "regex" {
                v.clone()
            } else {
                v.to_lowercase()
            })
        })
        .collect()
}

fn managed_rules(cfg: &Value) -> Vec<(&'static str, Vec<String>)> {
    RULE_TYPES
        .iter()
        .map(|(kind, _, key)| {
            (
                *kind,
                sanitize_list(kind, &config::string_list(cfg, &["automod", "rules", key])),
            )
        })
        .collect()
}

fn rule_preview(values: &[String], limit: usize) -> String {
    if values.is_empty() {
        return "brak".to_owned();
    }
    let preview = values
        .iter()
        .take(limit)
        .map(|v| format!("- {v}"))
        .collect::<Vec<_>>()
        .join("\n");
    if values.len() <= limit {
        preview
    } else {
        format!("{preview}\n- ... i jeszcze {}", values.len() - limit)
    }
}

fn rule_list_lines(rules: &[(&str, Vec<String>)], filter: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for (kind, values) in rules
        .iter()
        .filter(|(k, _)| filter == "all" || *k == filter)
    {
        let (label, _) = rule_meta(kind).unwrap_or(("?", "?"));
        lines.push(format!("{label} ({}):", values.len()));
        lines.push(rule_preview(values, 10));
        lines.push(String::new());
    }
    let mut out: Vec<String> = Vec::new();
    for line in lines {
        if line.is_empty() && out.last().is_some_and(String::is_empty) {
            continue;
        }
        out.push(line);
    }
    out
}

fn js_num(value: &Value, path: &[&str]) -> String {
    let n = config::number(value, path);
    let n = if n.is_finite() { n } else { 0.0 };
    config::js_string(&json!(n))
}

fn status_lines(app: &App) -> Vec<String> {
    let cfg = app.automod.cfg();
    let count = |path: &[&str]| cfg_array_len(&cfg, path);
    let yes = |path: &[&str]| {
        if config::is_true(&cfg, path) {
            "tak"
        } else {
            "nie"
        }
    };
    let max_mentions = config::number(&cfg, &["rules", "maxMentions"]);
    let list = |key: &str| config::string_list(&cfg, &["rules", key]);
    let preview = |key: &str| rule_preview(&list(key), 3).replace('\n', "; ");
    vec![
        format!("Enabled: {}", yes(&["enabled"])),
        format!("Blocked words: {}", count(&["rules", "blockedWords"])),
        format!("Blocked regex: {}", count(&["rules", "blockedRegex"])),
        format!("Allowed domains: {}", count(&["rules", "allowedDomains"])),
        format!("Block invites: {}", yes(&["rules", "blockInvites"])),
        format!("Block links: {}", yes(&["rules", "blockLinks"])),
        format!(
            "Max mentions: {}",
            if max_mentions.is_finite() && max_mentions != 0.0 {
                js_num(&cfg, &["rules", "maxMentions"])
            } else {
                "off".to_owned()
            }
        ),
        format!(
            "Caps rule: {}",
            if config::is_true(&cfg, &["rules", "caps", "enabled"]) {
                format!(
                    "on (min={}, ratio={})",
                    js_num(&cfg, &["rules", "caps", "minLength"]),
                    js_num(&cfg, &["rules", "caps", "ratio"])
                )
            } else {
                "off".to_owned()
            }
        ),
        format!(
            "Duplicate rule: {}",
            if config::is_true(&cfg, &["rules", "duplicate", "enabled"]) {
                format!(
                    "on ({}x / {}s)",
                    js_num(&cfg, &["rules", "duplicate", "maxRepeats"]),
                    js_num(&cfg, &["rules", "duplicate", "windowSeconds"])
                )
            } else {
                "off".to_owned()
            }
        ),
        format!("Delete message: {}", yes(&["actions", "delete"])),
        format!(
            "Timeout minutes: {}",
            js_num(&cfg, &["actions", "timeoutMinutes"])
        ),
        format!("Warn in channel: {}", yes(&["actions", "warnInChannel"])),
        format!("Exempt users: {}", count(&["exemptUsers"])),
        format!("Exempt roles: {}", count(&["exemptRoles"])),
        format!(
            "Log channel: {}",
            config::text(&cfg, &["logChannelId"]).map_or("brak".to_owned(), |c| format!("<#{c}>"))
        ),
        String::new(),
        "Managed rule preview:".to_owned(),
        format!("Words: {}", preview("blockedWords")),
        format!("Regex: {}", preview("blockedRegex")),
        format!("Domains: {}", preview("allowedDomains")),
    ]
}

fn cfg_array_len(cfg: &Value, path: &[&str]) -> usize {
    config::at(cfg, path)
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

enum Mutation {
    Changed(String, usize),
    Unchanged(&'static str, String, usize),
}

fn mutate_rule(app: &App, kind: &str, raw: &str, action: &str) -> Result<Mutation, String> {
    let value = normalize_rule_value(kind, raw)?;
    let _guard = config::write_lock();
    let mut cfg = config::read(&app.paths.config_path).map_err(|e| e.to_string())?;
    let mut rules = managed_rules(&cfg);
    let Some((_, current)) = rules.iter_mut().find(|(k, _)| *k == kind) else {
        return Err(format!("Nieznany typ reguły: {kind}"));
    };
    let key = |v: &str| {
        if kind == "regex" {
            v.to_owned()
        } else {
            v.to_lowercase()
        }
    };
    let existing = current.iter().position(|v| key(v) == key(&value));
    match (action, existing) {
        ("add", Some(_)) => return Ok(Mutation::Unchanged("exists", value, current.len())),
        ("remove", None) => return Ok(Mutation::Unchanged("missing", value, current.len())),
        ("add", None) => current.push(value.clone()),
        ("remove", Some(index)) => {
            current.remove(index);
        }
        _ => return Err(format!("Nieznana akcja reguły: {action}")),
    }
    let count = current.len();
    if !cfg["automod"].is_object() {
        cfg["automod"] = json!({});
    }
    if !cfg["automod"]["rules"].is_object() {
        cfg["automod"]["rules"] = json!({});
    }
    for (kind, values) in &rules {
        if let Some((_, key)) = rule_meta(kind) {
            cfg["automod"]["rules"][key] = json!(values);
        }
    }
    config::write(&app.paths.config_path, &cfg).map_err(|e| e.to_string())?;
    app.automod
        .start(app)
        .map_err(|e: ConfigError| e.to_string())?;
    Ok(Mutation::Changed(value, count))
}

pub async fn execute(ix: &Ix<'_>) -> Result<()> {
    if !ix.in_guild() {
        return ix
            .reply_ephemeral("Ta komenda dziala tylko na serwerze.")
            .await;
    }
    if !ix.member_has(Permissions::ADMINISTRATOR) {
        return ix.reply_ephemeral("Brak uprawnień Administratora.").await;
    }
    let app = ix.app;
    let opts = ix.opts();
    let group = opts.group;
    let sub = opts.subcommand.unwrap_or_default();

    match (group, sub) {
        (None, "status") => ix.reply_ephemeral(status_lines(app).join("\n")).await,
        (Some("rule"), "list") => {
            let kind = opts
                .string("type")
                .unwrap_or_else(|| "all".to_owned())
                .trim()
                .to_lowercase();
            if kind != "all" && rule_meta(&kind).is_none() {
                return ix
                    .reply_ephemeral(format!("Nieznany typ reguł: `{kind}`."))
                    .await;
            }
            match config::read(&app.paths.config_path) {
                Ok(cfg) => {
                    let header = if kind == "all" {
                        "AutoMod rules (all managed lists):".to_owned()
                    } else {
                        format!("AutoMod rules: {}", rule_meta(&kind).map_or("?", |m| m.0))
                    };
                    let mut lines = vec![header, String::new()];
                    lines.extend(rule_list_lines(&managed_rules(&cfg), &kind));
                    ix.reply_ephemeral(lines.join("\n")).await
                }
                Err(error) => {
                    ix.reply_ephemeral(config::format_config_error(
                        &error,
                        "Nie można odczytać config.json. ",
                    ))
                    .await
                }
            }
        }
        (Some("rule"), "add" | "remove") => {
            let kind = opts
                .string("type")
                .unwrap_or_default()
                .trim()
                .to_lowercase();
            let raw = opts.string("value").unwrap_or_default();
            let Some((label, _)) = rule_meta(&kind) else {
                return ix
                    .reply_ephemeral(format!("Nieznany typ reguł: `{kind}`."))
                    .await;
            };
            match mutate_rule(app, &kind, &raw, sub) {
                Ok(Mutation::Unchanged(reason, value, count)) => {
                    let message = if reason == "exists" {
                        format!("{label}: `{value}` już istnieje. Nic nie zmieniono.")
                    } else {
                        format!("{label}: `{value}` nie istnieje. Nic nie usunięto.")
                    };
                    ix.reply_ephemeral(format!("{message}\nAktualna liczba wpisów: {count}"))
                        .await
                }
                Ok(Mutation::Changed(value, count)) => {
                    let verb = if sub == "add" { "Dodano" } else { "Usunieto" };
                    ix.reply_ephemeral(format!(
                        "{verb} regułę {label}: `{value}`\nAktualna liczba wpisów: {count}"
                    ))
                    .await
                }
                Err(error) => {
                    ix.reply_ephemeral(format!("Zmiana reguły nie powiodła się. {error}"))
                        .await
                }
            }
        }
        (None, "test") => {
            let content = opts.string("content").unwrap_or_default();
            let kind = opts
                .string("type")
                .unwrap_or_else(|| "live".to_owned())
                .trim()
                .to_lowercase();
            if !TEST_TYPES.contains(&kind.as_str()) {
                return ix
                    .reply_ephemeral(format!("Nieznany typ testu: `{kind}`."))
                    .await;
            }
            let mut candidate = None;
            if kind != "live" {
                let Some(raw) = opts.string("value").filter(|v| !v.is_empty()) else {
                    return ix
                        .reply_ephemeral("Dla testu konkretnej reguły podaj też `value`.")
                        .await;
                };
                match normalize_rule_value(&kind, &raw) {
                    Ok(value) => candidate = Some(value),
                    Err(error) => return ix.reply_ephemeral(error).await,
                }
            }
            let result = app
                .automod
                .test_content(&content, &kind, candidate.as_deref());
            let mut lines = Vec::new();
            if kind == "live" {
                lines.push("AutoMod test: aktywne reguły".to_owned());
                lines.push(format!("Content: `{content}`"));
                match &result.violation {
                    Some(v) => {
                        lines.push(format!("Matched: {}", v.rule));
                        lines.push(format!("Details: {}", v.details));
                    }
                    None => lines.push("Result: brak dopasowania w aktywnych regułach.".to_owned()),
                }
            } else {
                let (label, _) = rule_meta(&kind).unwrap_or(("?", "?"));
                lines.push(format!("AutoMod test: {label}"));
                lines.push(format!("Candidate: `{}`", candidate.unwrap_or_default()));
                lines.push(format!("Content: `{content}`"));
                match &result.violation {
                    Some(v) => {
                        lines.push(format!("Result: trafia w tę regułę ({}).", v.rule));
                        lines.push(format!("Details: {}", v.details));
                    }
                    None => lines.push("Result: ta treść nie trafia w tę regułę.".to_owned()),
                }
            }
            if let Some(note) = result.note {
                lines.push(format!("Note: {note}"));
            }
            ix.reply_ephemeral(lines.join("\n")).await
        }
        _ => ix.reply_ephemeral("Nieznana komenda automod.").await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rule_normalization() {
        assert_eq!(
            normalize_rule_value("word", "  Foo   Bar ").unwrap(),
            "foo bar"
        );
        assert!(normalize_rule_value("regex", "(").is_err());
        assert_eq!(
            normalize_rule_value("allowed_domain", "HTTPS://Example.COM./x").unwrap(),
            "example.com"
        );
        assert_eq!(
            normalize_rule_value("allowed_domain", "sub.example.org").unwrap(),
            "sub.example.org"
        );
        assert_eq!(
            sanitize_list("word", &["A".into(), "a".into(), " ".into()]),
            vec!["a"]
        );
    }

    #[test]
    fn list_lines_collapse_blank_lines() {
        let rules = vec![
            ("word", vec![]),
            ("regex", vec!["x".to_owned()]),
            ("allowed_domain", vec![]),
        ];
        let lines = rule_list_lines(&rules, "all");
        assert_eq!(lines[0], "Blocked words (0):");
        assert_eq!(lines.last().map(String::as_str), Some(""));
    }
}
