//! Per-guild state files (`<dataDir>/server/<guildId>.json`), byte-compatible with the JS store.
//!
//! Semantics follow `src/core/state.js`: the file is deep-merged onto the default shape when
//! loaded, patches are deep-merged on save (arrays and scalars replace), and selected map keys
//! (verification expirations/challenges/lockouts, announcement webhooks/profiles) are replaced
//! wholesale so deleted entries really disappear.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};

use operator_common::time::{now_iso, parse_iso};

pub struct StateStore {
    dir: PathBuf,
    cache: Mutex<HashMap<String, Value>>,
}

pub fn default_state(guild_id: &str) -> Value {
    json!({
        "version": 1,
        "guildId": guild_id,
        "guildName": null,
        "presence": {
            "status": "offline",
            "reason": null,
            "lastSeenAt": null,
            "lastOnlineAt": null,
            "lastOfflineAt": null
        },
        "runtime": {
            "bootCount": 0,
            "lastBootAt": null,
            "lastExitReason": null
        },
        "autoclean": {
            "nextRunAt": null,
            "lastRunAt": null,
            "lastTrigger": null,
            "lastDeleted": 0,
            "lastError": null,
            "lastDurationMs": null,
            "queuedChannels": []
        },
        "verification": {
            "enabled": false,
            "roleId": null,
            "panel": { "channelId": null, "messageId": null, "customId": null },
            "exceptions": [],
            "expirations": {},
            "challenges": {},
            "lockouts": {},
            "lastSweepAt": null,
            "lastFullSweepAt": null
        },
        "announcements": {
            "webhooks": {},
            "profiles": {},
            "items": [],
            "lastSweepAt": null
        },
        "logs": {
            "enabled": false,
            "channelId": null,
            "retentionDays": 30,
            "forwardMode": "all",
            "includeBots": false,
            "disabledCategories": []
        }
    })
}

/// JS `deepMergeState`: objects merge recursively, everything else replaces.
pub fn deep_merge(base: &mut Value, patch: &Value) {
    let (Some(base_map), Some(patch_map)) = (base.as_object_mut(), patch.as_object()) else {
        *base = patch.clone();
        return;
    };
    for (key, value) in patch_map {
        match base_map.get_mut(key) {
            Some(existing) if existing.is_object() && value.is_object() => {
                deep_merge(existing, value)
            }
            _ => {
                base_map.insert(key.clone(), value.clone());
            }
        }
    }
}

/// Merges `patch` into `section`, replacing (not merging) the keys listed in `replace`.
fn merge_section(state: &mut Value, section: &str, patch: &Value, replace: &[&str]) {
    let mut deep = patch.clone();
    let mut replaced = Map::new();
    if let Some(map) = deep.as_object_mut() {
        for key in replace {
            if let Some(value) = map.remove(*key) {
                replaced.insert((*key).to_owned(), value);
            }
        }
    }
    if !state.get(section).is_some_and(Value::is_object) {
        state[section] = Value::Object(Map::new());
    }
    let target = &mut state[section];
    deep_merge(target, &deep);
    if let Some(target_map) = target.as_object_mut() {
        for (key, value) in replaced {
            target_map.insert(key, value);
        }
    }
}

fn timestamp_ok_and_after(raw: Option<&Value>, now: DateTime<Utc>) -> bool {
    raw.and_then(Value::as_str)
        .and_then(parse_iso)
        .is_some_and(|ts| ts > now)
}

impl StateStore {
    pub fn new(dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            cache: Mutex::new(HashMap::new()),
        })
    }

    fn file_path(&self, guild_id: &str) -> PathBuf {
        self.dir.join(format!("{guild_id}.json"))
    }

    fn read_file(&self, path: &Path) -> Option<Value> {
        match operator_common::fs::read_json::<Value>(path) {
            Ok(value) => value,
            Err(error) => {
                tracing::error!("Nie mozna odczytac cache {}: {error:#}", path.display());
                // Keep an unparsable file: the next write would replace it with defaults.
                if error.chain().any(|e| e.is::<serde_json::Error>()) {
                    let backup = path.with_extension(format!(
                        "json.corrupt-{}",
                        Utc::now().format("%Y%m%dT%H%M%S%.3fZ")
                    ));
                    match std::fs::rename(path, &backup) {
                        Ok(()) => {
                            tracing::error!("Uszkodzony cache przeniesiony do {}", backup.display())
                        }
                        Err(error) => tracing::error!(
                            "Nie mozna zachowac uszkodzonego cache {}: {error}",
                            path.display()
                        ),
                    }
                }
                None
            }
        }
    }

    fn with_cached<R>(&self, guild_id: &str, f: impl FnOnce(&mut Value) -> R) -> R {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let entry = cache.entry(guild_id.to_owned()).or_insert_with(|| {
            let mut state = default_state(guild_id);
            if let Some(current) = self.read_file(&self.file_path(guild_id)) {
                deep_merge(&mut state, &current);
            }
            state
        });
        f(entry)
    }

    #[cfg(test)]
    pub fn load(&self, guild_id: &str) -> Value {
        self.with_cached(guild_id, |state| state.clone())
    }

    /// One top-level section (`Null` when missing) without cloning the whole guild state;
    /// `logs` is read on every gateway event by the full-logs module.
    fn section(&self, guild_id: &str, key: &str) -> Value {
        self.with_cached(guild_id, |state| {
            state.get(key).cloned().unwrap_or(Value::Null)
        })
    }

    /// Applies `mutate` to the cached state and persists it atomically.
    pub fn update(&self, guild_id: &str, mutate: impl FnOnce(&mut Value)) -> Value {
        let path = self.file_path(guild_id);
        self.with_cached(guild_id, |state| {
            mutate(state);
            if state.get("guildId").is_none_or(|v| v.is_null() || v == "") {
                state["guildId"] = Value::String(guild_id.to_owned());
            }
            if let Err(error) = operator_common::fs::write_json_atomic(&path, state) {
                tracing::error!("Nie mozna zapisac cache {}: {error:#}", path.display());
            }
            state.clone()
        })
    }

    fn save_section(
        &self,
        guild_id: &str,
        section: &str,
        patch: &Value,
        replace: &[&str],
    ) -> Value {
        self.update(guild_id, |state| {
            merge_section(state, section, patch, replace)
        })
    }

    // --- presence --------------------------------------------------------------------------

    pub fn mark_guild_online(&self, guild_id: &str, guild_name: Option<&str>, reason: &str) {
        let ts = now_iso();
        self.update(guild_id, |state| {
            let boot_count = state["runtime"]["bootCount"].as_i64().unwrap_or(0) + 1;
            let name = guild_name
                .map(|n| Value::String(n.to_owned()))
                .unwrap_or_else(|| state["guildName"].clone());
            deep_merge(
                state,
                &json!({
                    "guildId": guild_id,
                    "guildName": name,
                    "presence": { "status": "online", "reason": reason, "lastSeenAt": ts, "lastOnlineAt": ts },
                    "runtime": { "bootCount": boot_count, "lastBootAt": ts }
                }),
            );
        });
    }

    pub fn mark_guild_offline(&self, guild_id: &str, guild_name: Option<&str>, reason: &str) {
        let ts = now_iso();
        self.update(guild_id, |state| {
            let name = guild_name
                .map(|n| Value::String(n.to_owned()))
                .unwrap_or_else(|| state["guildName"].clone());
            deep_merge(
                state,
                &json!({
                    "guildId": guild_id,
                    "guildName": name,
                    "presence": { "status": "offline", "reason": reason, "lastSeenAt": ts, "lastOfflineAt": ts },
                    "runtime": { "lastExitReason": reason }
                }),
            );
        });
    }

    /// Renames legacy `<name>-<guildId>.json` files to `<guildId>.json`.
    pub fn migrate_legacy_server_cache(&self) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let pattern = regex::Regex::new(r"^(.+)-(\d+)\.json$").expect("static regex");
        let (mut migrated, mut removed) = (0, 0);
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(captures) = pattern.captures(&name) else {
                continue;
            };
            let legacy = entry.path();
            let target = self.file_path(&captures[2]);
            if legacy == target {
                continue;
            }
            if target.exists() {
                if std::fs::remove_file(&legacy).is_ok() {
                    removed += 1;
                }
            } else if std::fs::rename(&legacy, &target).is_ok() {
                migrated += 1;
            }
        }
        if migrated > 0 || removed > 0 {
            tracing::info!("Migracja cache legacy: przeniesiono={migrated}, usunieto={removed}");
        }
    }

    // --- autoclean -------------------------------------------------------------------------

    pub fn autoclean(&self, guild_id: &str) -> Value {
        self.section(guild_id, "autoclean")
    }

    pub fn save_autoclean(&self, guild_id: &str, patch: Value) {
        self.save_section(guild_id, "autoclean", &patch, &[]);
    }

    pub fn mark_cleanup_result(
        &self,
        guild_id: &str,
        trigger: &str,
        deleted: u64,
        error: Option<&str>,
        duration_ms: u64,
    ) {
        self.save_autoclean(
            guild_id,
            json!({
                "lastRunAt": now_iso(),
                "lastTrigger": trigger,
                "lastDeleted": deleted,
                "lastError": error,
                "lastDurationMs": duration_ms
            }),
        );
    }

    pub fn set_next_run_at(&self, guild_id: &str, iso: Option<&str>) {
        self.save_autoclean(guild_id, json!({ "nextRunAt": iso }));
    }

    pub fn queued_autoclean_channels(&self, guild_id: &str) -> Vec<Value> {
        self.autoclean(guild_id)["queuedChannels"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    pub fn add_queued_autoclean_channel(
        &self,
        guild_id: &str,
        channel_id: &str,
        queued_by: Option<&str>,
    ) {
        let mut queued = self.queued_autoclean_channels(guild_id);
        let queued_at = now_iso();
        if let Some(existing) = queued
            .iter_mut()
            .find(|entry| entry["channelId"].as_str() == Some(channel_id))
        {
            let at = existing["queuedAt"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or(queued_at);
            let by = existing["queuedBy"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| queued_by.map(str::to_owned));
            *existing = json!({ "channelId": channel_id, "queuedAt": at, "queuedBy": by });
        } else {
            queued.push(
                json!({ "channelId": channel_id, "queuedAt": queued_at, "queuedBy": queued_by }),
            );
        }
        self.save_autoclean(guild_id, json!({ "queuedChannels": queued }));
    }

    pub fn remove_queued_autoclean_channels(&self, guild_id: &str, channel_ids: &[String]) {
        let queued: Vec<Value> = self
            .queued_autoclean_channels(guild_id)
            .into_iter()
            .filter(|entry| {
                let id = entry["channelId"].as_str().unwrap_or_default();
                !channel_ids.iter().any(|c| c == id)
            })
            .collect();
        self.save_autoclean(guild_id, json!({ "queuedChannels": queued }));
    }

    // --- verification ----------------------------------------------------------------------

    pub fn verification(&self, guild_id: &str) -> Value {
        self.section(guild_id, "verification")
    }

    /// Merges `patch` into `verification`; `expirations`, `challenges` and `lockouts` replace.
    pub fn save_verification(&self, guild_id: &str, patch: Value) -> Value {
        self.save_section(
            guild_id,
            "verification",
            &patch,
            &["expirations", "challenges", "lockouts"],
        )["verification"]
            .clone()
    }

    fn verification_map(&self, guild_id: &str, key: &str) -> Map<String, Value> {
        self.verification(guild_id)[key]
            .as_object()
            .cloned()
            .unwrap_or_default()
    }

    pub fn verification_exceptions(&self, guild_id: &str) -> Vec<String> {
        crate::config::string_list(&self.verification(guild_id), &["exceptions"])
    }

    pub fn add_verification_exception(&self, guild_id: &str, user_id: &str) {
        let mut list = self.verification_exceptions(guild_id);
        if !list.iter().any(|id| id == user_id) {
            list.push(user_id.to_owned());
        }
        self.save_verification(guild_id, json!({ "exceptions": list }));
    }

    pub fn remove_verification_exception(&self, guild_id: &str, user_id: &str) {
        let list: Vec<String> = self
            .verification_exceptions(guild_id)
            .into_iter()
            .filter(|id| id != user_id)
            .collect();
        self.save_verification(guild_id, json!({ "exceptions": list }));
    }

    pub fn set_user_verification_expiry(&self, guild_id: &str, user_id: &str, iso: &str) {
        let mut map = self.verification_map(guild_id, "expirations");
        map.insert(user_id.to_owned(), Value::String(iso.to_owned()));
        self.save_verification(guild_id, json!({ "expirations": map }));
    }

    pub fn verification_challenge(&self, guild_id: &str, user_id: &str) -> Option<Value> {
        self.verification_map(guild_id, "challenges")
            .get(user_id)
            .filter(|v| v.is_object())
            .cloned()
    }

    pub fn save_verification_challenge(
        &self,
        guild_id: &str,
        user_id: &str,
        role_id: &str,
        code: &str,
        expires_at: &str,
    ) {
        let mut map = self.verification_map(guild_id, "challenges");
        map.insert(
            user_id.to_owned(),
            json!({ "roleId": role_id, "code": code, "attempts": 0, "expiresAt": expires_at }),
        );
        self.save_verification(guild_id, json!({ "challenges": map }));
    }

    pub fn remove_verification_challenge(&self, guild_id: &str, user_id: &str) {
        let mut map = self.verification_map(guild_id, "challenges");
        map.remove(user_id);
        self.save_verification(guild_id, json!({ "challenges": map }));
    }

    pub fn prune_expired_verification_challenges(&self, guild_id: &str, now: DateTime<Utc>) {
        let current = self.verification_map(guild_id, "challenges");
        if current.is_empty() {
            return;
        }
        let next: Map<String, Value> = current
            .iter()
            .filter(|(_, challenge)| timestamp_ok_and_after(challenge.get("expiresAt"), now))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if next.len() != current.len() {
            self.save_verification(guild_id, json!({ "challenges": next }));
        }
    }

    pub fn verification_lockout(&self, guild_id: &str, user_id: &str) -> Option<String> {
        self.verification_map(guild_id, "lockouts")
            .get(user_id)
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    pub fn set_verification_lockout(&self, guild_id: &str, user_id: &str, until_iso: &str) {
        let mut map = self.verification_map(guild_id, "lockouts");
        map.insert(user_id.to_owned(), Value::String(until_iso.to_owned()));
        self.save_verification(guild_id, json!({ "lockouts": map }));
    }

    pub fn prune_expired_verification_lockouts(&self, guild_id: &str, now: DateTime<Utc>) {
        let current = self.verification_map(guild_id, "lockouts");
        if current.is_empty() {
            return;
        }
        let next: Map<String, Value> = current
            .iter()
            .filter(|(_, until)| timestamp_ok_and_after(Some(until), now))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if next.len() != current.len() {
            self.save_verification(guild_id, json!({ "lockouts": next }));
        }
    }

    // --- announcements ---------------------------------------------------------------------

    pub fn announcements(&self, guild_id: &str) -> Value {
        self.section(guild_id, "announcements")
    }

    /// `webhooks` and `profiles` replace; `webhookToken` is stripped from items.
    pub fn save_announcements(&self, guild_id: &str, mut patch: Value) {
        if let Some(items) = patch.get_mut("items").and_then(Value::as_array_mut) {
            for item in items.iter_mut() {
                if let Some(map) = item.as_object_mut() {
                    map.remove("webhookToken");
                }
            }
        }
        self.save_section(guild_id, "announcements", &patch, &["webhooks", "profiles"]);
    }

    // --- logs ------------------------------------------------------------------------------

    pub fn logs(&self, guild_id: &str) -> Value {
        self.section(guild_id, "logs")
    }

    pub fn save_logs(&self, guild_id: &str, patch: Value) {
        self.save_section(guild_id, "logs", &patch, &[]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(tag: &str) -> (StateStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!("operator-state-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (StateStore::new(&dir).unwrap(), dir)
    }

    #[test]
    fn merge_keeps_unknown_and_replaces_maps() {
        let (store, dir) = temp_store("merge");
        std::fs::write(
            dir.join("1.json"),
            r#"{"guildId":"1","custom":{"keep":true},"verification":{"expirations":{"a":"x","b":"y"}}}"#,
        )
        .unwrap();

        let state = store.load("1");
        assert_eq!(state["custom"]["keep"], true);
        assert_eq!(state["logs"]["retentionDays"], 30);

        let mut map = Map::new();
        map.insert("b".into(), "y".into());
        store.save_verification("1", json!({ "expirations": map, "enabled": true }));
        let verification = store.verification("1");
        assert!(verification["expirations"].get("a").is_none());
        assert_eq!(verification["enabled"], true);
        assert_eq!(verification["panel"]["channelId"], Value::Null);

        let on_disk: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("1.json")).unwrap()).unwrap();
        assert_eq!(on_disk["verification"]["enabled"], true);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn queued_channels_and_lockouts() {
        let (store, dir) = temp_store("queue");
        store.add_queued_autoclean_channel("2", "10", Some("99"));
        store.add_queued_autoclean_channel("2", "10", None);
        assert_eq!(store.queued_autoclean_channels("2").len(), 1);
        store.remove_queued_autoclean_channels("2", &["10".into()]);
        assert!(store.queued_autoclean_channels("2").is_empty());

        store.set_verification_lockout("2", "5", "2000-01-01T00:00:00.000Z");
        store.prune_expired_verification_lockouts("2", Utc::now());
        assert!(store.verification_lockout("2", "5").is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A state file that fails to parse (truncated by a full disk, edited by hand) must not be
    /// silently replaced by defaults on the next write: it holds the verification roles,
    /// webhook tokens and announcement schedule of the guild.
    #[test]
    fn corrupt_state_file_is_preserved() {
        let (store, dir) = temp_store("corrupt");
        let corrupt = r#"{"guildId":"3","verification":{"enabled":true,"roleId":"42""#;
        std::fs::write(dir.join("3.json"), corrupt).unwrap();

        store.mark_guild_online("3", Some("Guild"), "test");

        let kept: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy() != "3.json")
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .collect();
        assert!(
            kept.iter().any(|raw| raw == corrupt),
            "the unreadable 3.json was overwritten without a backup"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
