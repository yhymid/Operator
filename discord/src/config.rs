//! `config.json` reading, validation and atomic writes.
//!
//! The config is kept as a `serde_json::Value` (with preserved key order) so unknown keys such as
//! `_INSTRUKCJA` survive a round-trip through `/settings` or `/automod rule add`, exactly like the
//! JS implementation which mutates the parsed object in place.

use std::path::Path;

use serde_json::{Map, Value};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0}")]
    Io(String),
    #[error("Config file {path} contains invalid JSON: {message}")]
    Parse { path: String, message: String },
    #[error("Config file {path} is invalid:\n{preview}")]
    Validation {
        path: String,
        preview: String,
        issues: Vec<String>,
    },
}

impl ConfigError {
    fn validation(path: &Path, issues: Vec<String>) -> Self {
        let mut preview = issues
            .iter()
            .take(8)
            .map(|issue| format!("- {issue}"))
            .collect::<Vec<_>>()
            .join("\n");
        if issues.len() > 8 {
            preview.push_str(&format!("\n- +{} more issue(s)", issues.len() - 8));
        }
        Self::Validation {
            path: path.display().to_string(),
            preview,
            issues,
        }
    }
}

/// Same as JS `formatConfigError(error, { prefix })`.
pub fn format_config_error(error: &ConfigError, prefix: &str) -> String {
    format!("{prefix}{error}")
}

pub fn read(path: &Path) -> Result<Value, ConfigError> {
    let raw = std::fs::read_to_string(path).map_err(|error| {
        ConfigError::Io(format!(
            "ENOENT or unreadable config {}: {error}",
            path.display()
        ))
    })?;
    let parsed: Value = serde_json::from_str(&raw).map_err(|error| ConfigError::Parse {
        path: path.display().to_string(),
        message: error.to_string(),
    })?;
    assert_valid(&parsed, path)?;
    Ok(parsed)
}

/// Returns `Ok(None)` when the file does not exist (`allowMissing` in JS).
pub fn read_optional(path: &Path) -> Result<Option<Value>, ConfigError> {
    if !path.exists() {
        return Ok(None);
    }
    read(path).map(Some)
}

/// Serialises read-modify-write cycles of `config.json` inside this process: two commands
/// changing different options at once must not overwrite each other's change.
pub fn write_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Re-reads the file, applies `change` and writes it back, all under `write_lock`.
pub fn update(path: &Path, change: impl FnOnce(&mut Value)) -> Result<(), ConfigError> {
    let _guard = write_lock();
    let mut value = read(path)?;
    change(&mut value);
    write(path, &value)
}

pub fn write(path: &Path, value: &Value) -> Result<(), ConfigError> {
    assert_valid(value, path)?;
    operator_common::fs::write_json_atomic(path, value)
        .map_err(|error| ConfigError::Io(format!("{error:#}")))
}

pub fn assert_valid(config: &Value, path: &Path) -> Result<(), ConfigError> {
    let issues = validate(config);
    if issues.is_empty() {
        Ok(())
    } else {
        Err(ConfigError::validation(path, issues))
    }
}

// ---------------------------------------------------------------------------------------------
// Validation (mirrors src/core/config.js)
// ---------------------------------------------------------------------------------------------

struct Issues(Vec<String>);

impl Issues {
    fn add(&mut self, field: &str, message: &str) {
        self.0.push(format!("{field} {message}"));
    }

    fn boolean(&mut self, value: Option<&Value>, field: &str) {
        if let Some(value) = value
            && !value.is_boolean()
        {
            self.add(field, "must be a boolean.");
        }
    }

    fn string(&mut self, value: Option<&Value>, field: &str, allow_empty: bool) {
        let Some(value) = value else { return };
        match value.as_str() {
            None => self.add(field, "must be a string."),
            Some(text) if !allow_empty && text.trim().is_empty() => {
                self.add(field, "must not be empty.")
            }
            _ => {}
        }
    }

    fn id_string(&mut self, value: Option<&Value>, field: &str, allow_empty: bool) {
        let Some(value) = value else { return };
        match value.as_str() {
            None => self.add(field, "must be a string ID."),
            Some(text) if !allow_empty && text.trim().is_empty() => {
                self.add(field, "must not be empty.")
            }
            _ => {}
        }
    }

    fn string_array(&mut self, value: Option<&Value>, field: &str, allow_empty_strings: bool) {
        let Some(value) = value else { return };
        let Some(items) = value.as_array() else {
            self.add(field, "must be an array.");
            return;
        };
        for (index, item) in items.iter().enumerate() {
            let item_field = format!("{field}[{index}]");
            match item.as_str() {
                None => self.add(&item_field, "must be a string."),
                Some(text) if !allow_empty_strings && text.trim().is_empty() => {
                    self.add(&item_field, "must not be empty.")
                }
                _ => {}
            }
        }
    }

    fn integer(&mut self, value: Option<&Value>, field: &str, min: Option<i64>, max: Option<i64>) {
        let Some(value) = value else { return };
        let Some(number) = as_js_integer(value) else {
            self.add(field, "must be an integer.");
            return;
        };
        if let Some(min) = min
            && number < min
        {
            self.add(field, &format!("must be >= {min}."));
        }
        if let Some(max) = max
            && number > max
        {
            self.add(field, &format!("must be <= {max}."));
        }
    }

    fn number(&mut self, value: Option<&Value>, field: &str, min: f64, max: f64) {
        let Some(value) = value else { return };
        let Some(number) = value.as_f64().filter(|n| n.is_finite()) else {
            self.add(field, "must be a finite number.");
            return;
        };
        if number < min {
            self.add(field, &format!("must be >= {}.", js_number(min)));
        }
        if number > max {
            self.add(field, &format!("must be <= {}.", js_number(max)));
        }
    }

    fn object<'a>(
        &mut self,
        value: Option<&'a Value>,
        field: &str,
    ) -> Option<&'a Map<String, Value>> {
        let value = value?;
        match value.as_object() {
            Some(map) => Some(map),
            None => {
                self.add(field, "must be an object.");
                None
            }
        }
    }
}

/// `Number.isInteger` semantics: `5` and `5.0` are integers, `5.5` is not.
fn as_js_integer(value: &Value) -> Option<i64> {
    if let Some(number) = value.as_i64() {
        return Some(number);
    }
    value
        .as_f64()
        .filter(|n| n.is_finite() && n.fract() == 0.0)
        .map(|n| n as i64)
}

fn js_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{}", value as i64)
    } else {
        value.to_string()
    }
}

pub fn validate(config: &Value) -> Vec<String> {
    let mut issues = Issues(Vec::new());
    let Some(root) = config.as_object() else {
        issues.add("config", "must be a JSON object.");
        return issues.0;
    };
    let get = |key: &str| root.get(key);

    issues.boolean(get("enabled"), "enabled");
    issues.boolean(get("startupVerboseLogs"), "startupVerboseLogs");
    issues.string(get("cronSchedule"), "cronSchedule", false);
    if let Some(schedule) = get("cronSchedule").and_then(Value::as_str)
        && !schedule.trim().is_empty()
        && let Err(error) = crate::util::cron::parse(schedule)
    {
        issues.add(
            "cronSchedule",
            &format!("must be a valid cron expression ({error})."),
        );
    }
    if let Some(timezone) = get("timezone") {
        match timezone.as_str() {
            None => issues.add("timezone", "must be a string."),
            Some(text) if text.trim().is_empty() => issues.add("timezone", "must not be empty."),
            Some(text) if text.parse::<chrono_tz::Tz>().is_err() => issues.add(
                "timezone",
                "must be a valid IANA timezone, e.g. Europe/Warsaw.",
            ),
            _ => {}
        }
    }
    issues.integer(
        get("deleteMessagesOlderThanDays"),
        "deleteMessagesOlderThanDays",
        Some(0),
        None,
    );
    issues.integer(get("deleteLimit"), "deleteLimit", Some(0), None);
    issues.integer(
        get("deleteIntervalMinutes"),
        "deleteIntervalMinutes",
        Some(0),
        None,
    );
    issues.integer(
        get("deleteIntervalHours"),
        "deleteIntervalHours",
        Some(0),
        None,
    );
    issues.integer(
        get("deleteIntervalDays"),
        "deleteIntervalDays",
        Some(0),
        None,
    );
    issues.boolean(get("deletePinned"), "deletePinned");
    issues.boolean(get("dryRun"), "dryRun");
    issues.id_string(get("logChannelId"), "logChannelId", true);
    issues.string_array(get("ownerIds"), "ownerIds", false);

    if let Some(notify) = issues.object(get("notifyBeforeDelete"), "notifyBeforeDelete") {
        issues.boolean(notify.get("enabled"), "notifyBeforeDelete.enabled");
        issues.integer(
            notify.get("minutesBefore"),
            "notifyBeforeDelete.minutesBefore",
            Some(0),
            None,
        );
        issues.string(notify.get("message"), "notifyBeforeDelete.message", false);
    }

    if let Some(whitelist) = issues.object(get("whitelist"), "whitelist") {
        issues.string_array(whitelist.get("users"), "whitelist.users", false);
        issues.string_array(whitelist.get("roles"), "whitelist.roles", false);
    }

    if let Some(channels) = get("channels") {
        match channels.as_array() {
            None => issues.add("channels", "must be an array."),
            Some(items) => {
                for (index, channel) in items.iter().enumerate() {
                    let field = format!("channels[{index}]");
                    let Some(channel) = channel.as_object() else {
                        issues.add(&field, "must be an object.");
                        continue;
                    };
                    issues.id_string(channel.get("id"), &format!("{field}.id"), false);
                    issues.boolean(channel.get("enabled"), &format!("{field}.enabled"));
                    issues.integer(
                        channel.get("deleteLimit"),
                        &format!("{field}.deleteLimit"),
                        Some(0),
                        None,
                    );
                    issues.integer(
                        channel.get("deleteMessagesOlderThanDays"),
                        &format!("{field}.deleteMessagesOlderThanDays"),
                        Some(0),
                        None,
                    );
                    issues.boolean(
                        channel.get("deletePinned"),
                        &format!("{field}.deletePinned"),
                    );
                    issues.id_string(channel.get("guildId"), &format!("{field}.guildId"), true);
                }
            }
        }
    }

    if let Some(automod) = issues.object(get("automod"), "automod") {
        issues.boolean(automod.get("enabled"), "automod.enabled");
        issues.id_string(automod.get("logChannelId"), "automod.logChannelId", true);
        issues.string_array(automod.get("exemptUsers"), "automod.exemptUsers", false);
        issues.string_array(automod.get("exemptRoles"), "automod.exemptRoles", false);

        if let Some(rules) = issues.object(automod.get("rules"), "automod.rules") {
            issues.string_array(
                rules.get("blockedWords"),
                "automod.rules.blockedWords",
                false,
            );
            issues.string_array(
                rules.get("blockedRegex"),
                "automod.rules.blockedRegex",
                false,
            );
            if let Some(patterns) = rules.get("blockedRegex").and_then(Value::as_array) {
                for (index, pattern) in patterns.iter().enumerate() {
                    let Some(pattern) = pattern.as_str().filter(|p| !p.trim().is_empty()) else {
                        continue;
                    };
                    if crate::util::jsregex::compile(pattern, false).is_err() {
                        issues.add(
                            &format!("automod.rules.blockedRegex[{index}]"),
                            "must be a valid regular expression.",
                        );
                    }
                }
            }
            issues.string_array(
                rules.get("allowedDomains"),
                "automod.rules.allowedDomains",
                false,
            );
            issues.boolean(rules.get("blockInvites"), "automod.rules.blockInvites");
            issues.boolean(rules.get("blockLinks"), "automod.rules.blockLinks");
            issues.integer(
                rules.get("maxMentions"),
                "automod.rules.maxMentions",
                Some(0),
                None,
            );

            if let Some(caps) = issues.object(rules.get("caps"), "automod.rules.caps") {
                issues.boolean(caps.get("enabled"), "automod.rules.caps.enabled");
                issues.integer(
                    caps.get("minLength"),
                    "automod.rules.caps.minLength",
                    Some(0),
                    None,
                );
                issues.number(caps.get("ratio"), "automod.rules.caps.ratio", 0.0, 1.0);
            }
            if let Some(duplicate) =
                issues.object(rules.get("duplicate"), "automod.rules.duplicate")
            {
                issues.boolean(duplicate.get("enabled"), "automod.rules.duplicate.enabled");
                issues.integer(
                    duplicate.get("windowSeconds"),
                    "automod.rules.duplicate.windowSeconds",
                    Some(0),
                    None,
                );
                issues.integer(
                    duplicate.get("maxRepeats"),
                    "automod.rules.duplicate.maxRepeats",
                    Some(0),
                    None,
                );
            }
        }

        if let Some(actions) = issues.object(automod.get("actions"), "automod.actions") {
            issues.boolean(actions.get("delete"), "automod.actions.delete");
            issues.integer(
                actions.get("timeoutMinutes"),
                "automod.actions.timeoutMinutes",
                Some(0),
                None,
            );
            issues.boolean(
                actions.get("warnInChannel"),
                "automod.actions.warnInChannel",
            );
            issues.string(
                actions.get("warnTemplate"),
                "automod.actions.warnTemplate",
                false,
            );
            issues.integer(
                actions.get("warnDeleteAfterSeconds"),
                "automod.actions.warnDeleteAfterSeconds",
                Some(0),
                None,
            );
        }
    }

    issues.0
}

// ---------------------------------------------------------------------------------------------
// Typed accessors with the JS fallbacks (`Number(x || 0) || 0`, `x === true`, ...)
// ---------------------------------------------------------------------------------------------

/// Walks `path` through nested objects.
pub fn at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter()
        .try_fold(value, |current, key| current.get(*key))
}

/// `value === true`
pub fn is_true(value: &Value, path: &[&str]) -> bool {
    at(value, path).and_then(Value::as_bool) == Some(true)
}

/// `value !== false`
pub fn not_false(value: &Value, path: &[&str]) -> bool {
    at(value, path).and_then(Value::as_bool) != Some(false)
}

/// `Number(value || 0) || 0`
pub fn number(value: &Value, path: &[&str]) -> f64 {
    at(value, path).map_or(0.0, js_to_number)
}

pub fn int(value: &Value, path: &[&str]) -> i64 {
    number(value, path) as i64
}

/// Non-empty trimmed string or `None`.
pub fn text(value: &Value, path: &[&str]) -> Option<String> {
    at(value, path)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// `Array.isArray(x) ? x.map(String) : []`
pub fn string_list(value: &Value, path: &[&str]) -> Vec<String> {
    at(value, path)
        .and_then(Value::as_array)
        .map(|items| items.iter().map(js_string).collect())
        .unwrap_or_default()
}

/// `Number(value)` with JS coercion for the shapes found in config/state files.
pub fn js_to_number(value: &Value) -> f64 {
    match value {
        Value::Number(n) => n.as_f64().unwrap_or(0.0),
        Value::Bool(true) => 1.0,
        Value::String(s) => s.trim().parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// `String(value)` for JSON scalars.
pub fn js_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "null".to_owned(),
        other => other.to_string(),
    }
}

/// Sets `path` creating intermediate objects (JS `setPathValue`).
pub fn set_path(root: &mut Value, path: &[&str], new_value: Value) {
    let mut current = root;
    for key in &path[..path.len() - 1] {
        if !current.get(*key).is_some_and(Value::is_object)
            && let Some(map) = current.as_object_mut()
        {
            map.insert((*key).to_owned(), Value::Object(Map::new()));
        }
        current = current
            .get_mut(*key)
            .expect("intermediate object was just ensured");
    }
    if let Some(map) = current.as_object_mut() {
        map.insert(path[path.len() - 1].to_owned(), new_value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn example_config_is_valid() {
        let raw = include_str!("../deploy/config.example.json");
        let value: Value = serde_json::from_str(raw).unwrap();
        assert!(validate(&value).is_empty(), "{:?}", validate(&value));
    }

    #[test]
    fn reports_js_style_issues() {
        let value = json!({
            "deleteLimit": -1,
            "dryRun": "yes",
            "cronSchedule": "nope",
            "timezone": "Mars/Base",
            "automod": { "rules": { "caps": { "ratio": 2 }, "blockedRegex": ["("] } }
        });
        let issues = validate(&value);
        assert!(issues.contains(&"deleteLimit must be >= 0.".to_owned()));
        assert!(issues.contains(&"dryRun must be a boolean.".to_owned()));
        assert!(
            issues
                .iter()
                .any(|i| i.starts_with("cronSchedule must be a valid cron expression"))
        );
        assert!(
            issues.contains(
                &"timezone must be a valid IANA timezone, e.g. Europe/Warsaw.".to_owned()
            )
        );
        assert!(issues.contains(&"automod.rules.caps.ratio must be <= 1.".to_owned()));
        assert!(issues.contains(
            &"automod.rules.blockedRegex[0] must be a valid regular expression.".to_owned()
        ));
    }

    #[test]
    fn set_path_creates_objects() {
        let mut value = json!({ "automod": 5 });
        set_path(
            &mut value,
            &["automod", "rules", "caps", "ratio"],
            json!(0.5),
        );
        assert_eq!(value["automod"]["rules"]["caps"]["ratio"], json!(0.5));
    }
}
