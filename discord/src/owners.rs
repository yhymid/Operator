//! Owner IDs: `OWNER_IDS` → `OWNER_ID` → `config.json#ownerIds` (first non-empty source wins).

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

use crate::util::text::is_snowflake;

#[derive(Debug, Clone, Default)]
pub struct OwnerAccess {
    pub ids: BTreeSet<String>,
    pub invalid: Vec<String>,
    pub source: &'static str,
}

fn normalize(value: &str) -> Option<String> {
    let trimmed = value.trim();
    is_snowflake(trimmed, 16).then(|| trimmed.to_owned())
}

/// Splits on `,`, `;` and whitespace like `/[,\s;]+/`.
pub fn parse_input(raw: &str) -> (Vec<String>, Vec<String>) {
    let mut ids = Vec::new();
    let mut invalid = Vec::new();
    for token in raw
        .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .filter(|t| !t.is_empty())
    {
        match normalize(token) {
            Some(id) if !ids.contains(&id) => ids.push(id),
            Some(_) => {}
            None => invalid.push(token.to_owned()),
        }
    }
    (ids, invalid)
}

fn parse_array(values: &[Value]) -> (Vec<String>, Vec<String>) {
    let mut ids = Vec::new();
    let mut invalid = Vec::new();
    for value in values {
        let raw = crate::config::js_string(value);
        match normalize(&raw) {
            Some(id) if !ids.contains(&id) => ids.push(id),
            Some(_) => {}
            None if !raw.trim().is_empty() => invalid.push(raw.trim().to_owned()),
            None => {}
        }
    }
    (ids, invalid)
}

pub fn resolve(config_path: &Path) -> OwnerAccess {
    let mut invalid = Vec::new();
    for (key, source) in [("OWNER_IDS", "env:OWNER_IDS"), ("OWNER_ID", "env:OWNER_ID")] {
        let Ok(raw) = std::env::var(key) else {
            continue;
        };
        let (ids, bad) = parse_input(&raw);
        invalid.extend(bad);
        if !ids.is_empty() {
            return OwnerAccess {
                ids: ids.into_iter().collect(),
                invalid,
                source,
            };
        }
    }

    if let Ok(Some(config)) = crate::config::read_optional(config_path)
        && let Some(values) = config.get("ownerIds").and_then(Value::as_array)
    {
        let (ids, bad) = parse_array(values);
        invalid.extend(bad);
        if !ids.is_empty() {
            return OwnerAccess {
                ids: ids.into_iter().collect(),
                invalid,
                source: "config:ownerIds",
            };
        }
    }

    OwnerAccess {
        ids: BTreeSet::new(),
        invalid,
        source: "fallback-empty",
    }
}

pub fn is_owner(config_path: &Path, user_id: impl std::fmt::Display) -> bool {
    resolve(config_path).ids.contains(&user_id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mixed_separators() {
        let (ids, invalid) =
            parse_input("780552865445969920, 752986566426624061;abc 780552865445969920");
        assert_eq!(ids, vec!["780552865445969920", "752986566426624061"]);
        assert_eq!(invalid, vec!["abc"]);
    }
}
