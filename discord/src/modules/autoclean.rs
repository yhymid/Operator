//! AutoClean (port of `src/events/autoclean.js`).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use twilight_model::channel::Message;
use twilight_model::id::Id;
use twilight_model::id::marker::{ChannelMarker, GuildMarker, MessageMarker};

use crate::app::{App, SharedApp};
use crate::config;
use crate::discord::{self, FLAG_COMPONENTS_V2};
use crate::util::cron;
use operator_common::time::to_iso;

#[derive(Default)]
struct Stats {
    runs: u64,
    total_deleted: u64,
    last_run_at: Option<DateTime<Utc>>,
}

struct Schedule {
    cron: croner::Cron,
    timezone: Tz,
    task: JoinHandle<()>,
    warning: Option<JoinHandle<()>>,
}

pub struct AutoClean {
    config: Mutex<Value>,
    running: AtomicBool,
    stats: Mutex<Stats>,
    schedule: Mutex<Option<Schedule>>,
}

// Mirrors the JS diagnostics/snapshot shape; not every field is rendered.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ChannelSettings {
    pub channel_id: String,
    pub enabled: bool,
    pub delete_limit: u64,
    pub delete_pinned: bool,
    pub delete_older_than_days: u64,
    pub whitelist_users: Vec<String>,
    pub whitelist_roles: Vec<String>,
    pub notify_enabled: bool,
    pub notify_minutes: u64,
}

#[derive(Debug, Clone)]
pub struct CleanupResult {
    pub channel_id: String,
    pub channel_name: String,
    pub guild_id: Option<String>,
    pub deleted: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
struct Target {
    channel_id: String,
    guild_id: Option<String>,
    channel_config: Option<Value>,
    consume_queued: bool,
}

pub struct QueuedEntry {
    pub channel_id: String,
    pub queued_at: Option<String>,
    pub queued_by: Option<String>,
}

pub struct StatusSnapshot {
    pub enabled: bool,
    pub permanent_channels: Vec<(String, u64, bool)>,
    pub queued: Vec<QueuedEntry>,
    pub schedule: String,
    pub timezone: String,
    pub next_run_text: String,
    pub next_run_at: Option<String>,
    pub dry_run: bool,
    pub interval_text: String,
    pub delete_older_than_days: u64,
    pub delete_limit: u64,
    pub delete_pinned: bool,
    pub whitelist_users: usize,
    pub whitelist_roles: usize,
    pub notify: String,
    pub log_channel_id: Option<String>,
    pub runs: u64,
    pub total_deleted: u64,
    pub last_run_at: Option<String>,
    pub last_deleted: Value,
    pub last_error: Option<String>,
    pub last_trigger: Option<String>,
    pub last_duration_ms: Value,
}

fn english_unit(value: i64, unit: &str) -> String {
    if value == 1 {
        format!("1 {unit}")
    } else {
        format!("{value} {unit}s")
    }
}

impl AutoClean {
    pub fn new(initial_config: Value) -> Self {
        Self {
            config: Mutex::new(initial_config),
            running: AtomicBool::new(false),
            stats: Mutex::new(Stats::default()),
            schedule: Mutex::new(None),
        }
    }

    fn cfg(&self) -> Value {
        self.config
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn load_config(&self, app: &App) -> Result<Value, config::ConfigError> {
        let value = config::read(&app.paths.config_path)?;
        *self.config.lock().unwrap_or_else(|p| p.into_inner()) = value.clone();
        Ok(value)
    }

    fn timezone(cfg: &Value) -> Tz {
        config::text(cfg, &["timezone"])
            .and_then(|tz| tz.parse().ok())
            .unwrap_or(chrono_tz::Europe::Warsaw)
    }

    fn timezone_name(cfg: &Value) -> String {
        config::text(cfg, &["timezone"]).unwrap_or_else(|| "Europe/Warsaw".to_owned())
    }

    pub fn schedule_pattern(cfg: &Value) -> String {
        if let Some(cron) = config::text(cfg, &["cronSchedule"]) {
            return cron;
        }
        let minutes = config::int(cfg, &["deleteIntervalMinutes"]);
        if minutes > 0 {
            return format!("*/{minutes} * * * *");
        }
        let hours = config::int(cfg, &["deleteIntervalHours"]);
        if hours > 0 {
            return format!("0 */{hours} * * *");
        }
        let days = config::int(cfg, &["deleteIntervalDays"]);
        if days > 0 {
            return format!("0 0 */{days} * *");
        }
        "0 3 * * 0".to_owned()
    }

    pub fn interval_text(cfg: &Value) -> String {
        for (key, unit) in [
            ("deleteIntervalMinutes", "minute"),
            ("deleteIntervalHours", "hour"),
            ("deleteIntervalDays", "day"),
        ] {
            let value = config::int(cfg, &[key]);
            if value > 0 {
                return english_unit(value, unit);
            }
        }
        "configured interval".to_owned()
    }

    fn next_run(&self) -> Option<DateTime<Utc>> {
        let schedule = self.schedule.lock().unwrap_or_else(|p| p.into_inner());
        let schedule = schedule.as_ref()?;
        cron::next_after(&schedule.cron, schedule.timezone, Utc::now())
    }

    pub fn next_run_text(&self) -> String {
        let schedule = self.schedule.lock().unwrap_or_else(|p| p.into_inner());
        let Some(schedule) = schedule.as_ref() else {
            return "Brak harmonogramu".to_owned();
        };
        match cron::next_after(&schedule.cron, schedule.timezone, Utc::now()) {
            Some(next) => next
                .with_timezone(&schedule.timezone)
                .format("%d.%m.%Y %H:%M:%S")
                .to_string(),
            None => "N/A".to_owned(),
        }
    }

    pub fn next_run_iso(&self) -> Option<String> {
        self.next_run().map(to_iso)
    }

    /// (Re)starts the cron job. Errors only when config.json is invalid.
    pub fn start(&self, app: &SharedApp) -> Result<(), config::ConfigError> {
        let cfg = self.load_config(app)?;
        self.stop();

        if !config::not_false(&cfg, &["enabled"]) {
            self.update_next_run_for_all_guilds(app);
            tracing::info!("AutoClean jest wylaczony.");
            return Ok(());
        }

        let pattern = Self::schedule_pattern(&cfg);
        let timezone = Self::timezone(&cfg);
        let parsed = match cron::parse(&pattern) {
            Ok(parsed) => parsed,
            Err(error) => {
                tracing::error!("AutoClean: niepoprawny CRON {pattern}: {error}");
                return Ok(());
            }
        };

        let loop_app = app.clone();
        let loop_cron = parsed.clone();
        let task = tokio::spawn(async move {
            loop {
                let Some(next) = cron::next_after(&loop_cron, timezone, Utc::now()) else {
                    return;
                };
                let wait = (next - Utc::now()).to_std().unwrap_or_default();
                tokio::time::sleep(wait).await;
                loop_app.autoclean.run_cleanup(&loop_app, "cron").await;
            }
        });

        *self.schedule.lock().unwrap_or_else(|p| p.into_inner()) = Some(Schedule {
            cron: parsed,
            timezone,
            task,
            warning: None,
        });

        self.update_next_run_for_all_guilds(app);
        self.schedule_next_warning(app);
        tracing::info!("CRON ustawiony: {pattern} ({})", Self::timezone_name(&cfg));
        Ok(())
    }

    pub fn stop(&self) {
        if let Some(schedule) = self
            .schedule
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            schedule.task.abort();
            if let Some(warning) = schedule.warning {
                warning.abort();
            }
        }
    }

    fn schedule_next_warning(&self, app: &SharedApp) {
        let cfg = self.cfg();
        let mut guard = self.schedule.lock().unwrap_or_else(|p| p.into_inner());
        let Some(schedule) = guard.as_mut() else {
            return;
        };
        if let Some(previous) = schedule.warning.take() {
            previous.abort();
        }
        if !config::is_true(&cfg, &["notifyBeforeDelete", "enabled"]) {
            return;
        }
        let minutes = config::int(&cfg, &["notifyBeforeDelete", "minutesBefore"]);
        if minutes <= 0 {
            return;
        }
        let Some(next) = cron::next_after(&schedule.cron, schedule.timezone, Utc::now()) else {
            return;
        };
        let fire_at = next - chrono::Duration::minutes(minutes);
        let Ok(delay) = (fire_at - Utc::now()).to_std() else {
            return;
        };
        let app = app.clone();
        schedule.warning = Some(tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            app.autoclean.send_pre_delete_warnings(&app).await;
            // Re-arm for the following run once this one is past.
            tokio::time::sleep(Duration::from_secs(minutes as u64 * 60 + 1)).await;
            app.autoclean.schedule_next_warning(&app);
        }));
    }

    fn update_next_run_for_all_guilds(&self, app: &App) {
        let next = self.next_run_iso();
        for guild_id in app.guild_ids() {
            app.state
                .set_next_run_at(&guild_id.to_string(), next.as_deref());
        }
    }

    // --- targets ---------------------------------------------------------------------------

    fn cached_channel_guild(app: &App, channel_id: &str) -> Option<String> {
        let id = discord::parse_id::<ChannelMarker>(channel_id)?;
        app.cache
            .channel(id)
            .and_then(|c| c.guild_id)
            .map(|g| g.to_string())
    }

    fn channel_belongs_to_guild(
        app: &App,
        channel_id: &str,
        guild_id: Option<&str>,
        channel_cfg: &Value,
    ) -> bool {
        let Some(guild_id) = guild_id else {
            return true;
        };
        if let Some(configured) = config::text(channel_cfg, &["guildId"]) {
            return configured == guild_id;
        }
        if let Some(cached) = Self::cached_channel_guild(app, channel_id) {
            return cached == guild_id;
        }
        app.guild_count() == 1
    }

    fn resolve_guild_id(
        app: &App,
        channel_id: &str,
        channel_cfg: Option<&Value>,
    ) -> Option<String> {
        if let Some(configured) = channel_cfg.and_then(|c| config::text(c, &["guildId"])) {
            return Some(configured);
        }
        if let Some(cached) = Self::cached_channel_guild(app, channel_id) {
            return Some(cached);
        }
        if app.guild_count() == 1 {
            return app.guild_ids().first().map(|g| g.to_string());
        }
        None
    }

    fn configured_entries(&self, app: &App, cfg: &Value, guild_id: Option<&str>) -> Vec<Target> {
        cfg.get("channels")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|channel| config::not_false(channel, &["enabled"]))
            .filter_map(|channel| {
                let id = config::js_string(channel.get("id")?);
                Self::channel_belongs_to_guild(app, &id, guild_id, channel).then(|| Target {
                    guild_id: guild_id
                        .map(str::to_owned)
                        .or_else(|| Self::resolve_guild_id(app, &id, Some(channel))),
                    channel_id: id,
                    channel_config: Some(channel.clone()),
                    consume_queued: false,
                })
            })
            .collect()
    }

    pub fn queued_entries(&self, app: &App, guild_id: Option<&str>) -> Vec<QueuedEntry> {
        let guilds: Vec<String> = match guild_id {
            Some(id) => vec![id.to_owned()],
            None => app.guild_ids().iter().map(ToString::to_string).collect(),
        };
        let mut out = Vec::new();
        for guild in guilds {
            for entry in app.state.queued_autoclean_channels(&guild) {
                let Some(channel_id) = entry["channelId"].as_str().filter(|s| !s.is_empty()) else {
                    continue;
                };
                out.push(QueuedEntry {
                    channel_id: channel_id.to_owned(),
                    queued_at: entry["queuedAt"].as_str().map(str::to_owned),
                    queued_by: entry["queuedBy"].as_str().map(str::to_owned),
                });
            }
        }
        out
    }

    fn pending_targets(&self, app: &App, cfg: &Value, guild_id: Option<&str>) -> Vec<Target> {
        let mut order: Vec<String> = Vec::new();
        let mut targets: BTreeMap<String, Target> = BTreeMap::new();
        for target in self.configured_entries(app, cfg, guild_id) {
            if !targets.contains_key(&target.channel_id) {
                order.push(target.channel_id.clone());
            }
            targets.insert(target.channel_id.clone(), target);
        }

        let guilds: Vec<String> = match guild_id {
            Some(id) => vec![id.to_owned()],
            None => app.guild_ids().iter().map(ToString::to_string).collect(),
        };
        for guild in guilds {
            for entry in self.queued_entries(app, Some(&guild)) {
                if let Some(existing) = targets.get_mut(&entry.channel_id) {
                    existing.consume_queued = true;
                    continue;
                }
                order.push(entry.channel_id.clone());
                targets.insert(
                    entry.channel_id.clone(),
                    Target {
                        guild_id: Some(guild.clone()),
                        channel_id: entry.channel_id,
                        channel_config: None,
                        consume_queued: true,
                    },
                );
            }
        }

        order
            .into_iter()
            .filter_map(|id| targets.remove(&id))
            .collect()
    }

    pub fn effective_channel_settings(
        &self,
        channel_id: &str,
        provided: Option<&Value>,
    ) -> ChannelSettings {
        let cfg = self.cfg();
        let from_config = cfg
            .get("channels")
            .and_then(Value::as_array)
            .and_then(|channels| {
                channels
                    .iter()
                    .find(|c| c.get("id").map(config::js_string).as_deref() == Some(channel_id))
            })
            .cloned();
        let channel = provided
            .cloned()
            .or(from_config)
            .unwrap_or_else(|| json!({}));

        let pick = |key: &str| -> u64 {
            channel
                .get(key)
                .filter(|v| !v.is_null())
                .or_else(|| cfg.get(key).filter(|v| !v.is_null()))
                .map_or(0.0, config::js_to_number)
                .max(0.0) as u64
        };

        ChannelSettings {
            channel_id: channel_id.to_owned(),
            enabled: config::not_false(&channel, &["enabled"]),
            delete_limit: pick("deleteLimit"),
            delete_pinned: config::is_true(&channel, &["deletePinned"])
                || config::is_true(&cfg, &["deletePinned"]),
            delete_older_than_days: pick("deleteMessagesOlderThanDays"),
            whitelist_users: config::string_list(&cfg, &["whitelist", "users"]),
            whitelist_roles: config::string_list(&cfg, &["whitelist", "roles"]),
            notify_enabled: config::is_true(&cfg, &["notifyBeforeDelete", "enabled"]),
            notify_minutes: config::int(&cfg, &["notifyBeforeDelete", "minutesBefore"]).max(0)
                as u64,
        }
    }

    // --- queue management (/clean set|reset) ------------------------------------------------

    /// Returns `true` when the channel was already queued.
    pub fn queue_channel(
        &self,
        app: &App,
        guild_id: &str,
        channel_id: &str,
        queued_by: &str,
    ) -> bool {
        if self
            .queued_entries(app, Some(guild_id))
            .iter()
            .any(|e| e.channel_id == channel_id)
        {
            return true;
        }
        app.state
            .add_queued_autoclean_channel(guild_id, channel_id, Some(queued_by));
        false
    }

    /// Returns `true` when the channel was removed from the queue.
    pub fn reset_queued_channel(&self, app: &App, guild_id: &str, channel_id: &str) -> bool {
        if !self
            .queued_entries(app, Some(guild_id))
            .iter()
            .any(|e| e.channel_id == channel_id)
        {
            return false;
        }
        app.state
            .remove_queued_autoclean_channels(guild_id, &[channel_id.to_owned()]);
        true
    }

    // --- running ---------------------------------------------------------------------------

    fn try_begin(&self) -> bool {
        self.running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    pub async fn run_cleanup(&self, app: &SharedApp, trigger: &str) -> Option<Vec<CleanupResult>> {
        if !self.try_begin() {
            tracing::warn!("Czyszczenie juz trwa.");
            return None;
        }
        let result = self.run_cleanup_inner(app, trigger).await;
        self.running.store(false, Ordering::SeqCst);
        Some(result)
    }

    async fn run_cleanup_inner(&self, app: &SharedApp, trigger: &str) -> Vec<CleanupResult> {
        let cfg = match self.load_config(app) {
            Ok(cfg) => cfg,
            Err(error) => {
                tracing::error!("AutoClean: {error}");
                return Vec::new();
            }
        };
        if !config::not_false(&cfg, &["enabled"]) {
            tracing::warn!("AutoClean jest wylaczony.");
            return Vec::new();
        }

        let targets = self.pending_targets(app, &cfg, None);
        if targets.is_empty() {
            tracing::warn!("Brak aktywnych kanalow do czyszczenia.");
            return Vec::new();
        }

        {
            let mut stats = self.stats.lock().unwrap_or_else(|p| p.into_inner());
            stats.runs += 1;
            stats.last_run_at = Some(Utc::now());
        }

        let mut results = Vec::new();
        let mut consumed: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for target in &targets {
            let result = self
                .cleanup_channel_by_id(
                    app,
                    &target.channel_id,
                    target.channel_config.as_ref(),
                    trigger,
                )
                .await;
            results.push(result);
            if target.consume_queued
                && let Some(guild) = &target.guild_id
            {
                consumed
                    .entry(guild.clone())
                    .or_default()
                    .insert(target.channel_id.clone());
            }
            tokio::time::sleep(Duration::from_millis(1200)).await;
        }

        for (guild, channels) in consumed {
            app.state.remove_queued_autoclean_channels(
                &guild,
                &channels.into_iter().collect::<Vec<_>>(),
            );
        }

        let deleted: u64 = results.iter().map(|r| r.deleted).sum();
        self.stats
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .total_deleted += deleted;
        tracing::info!("Czyszczenie zakonczone. Usunieto {deleted} wiadomosci.");
        self.send_cleanup_log(app, &results, trigger).await;
        self.update_next_run_for_all_guilds(app);
        self.schedule_next_warning(app);
        results
    }

    /// `/clean mode:now` – returns `None` when another cleanup is running.
    pub async fn run_cleanup_for_channel(
        &self,
        app: &SharedApp,
        channel_id: Id<ChannelMarker>,
        trigger: &str,
    ) -> Option<CleanupResult> {
        if !self.try_begin() {
            tracing::warn!("Czyszczenie juz trwa.");
            return None;
        }
        if let Err(error) = self.load_config(app) {
            tracing::error!("AutoClean: {error}");
        }
        {
            let mut stats = self.stats.lock().unwrap_or_else(|p| p.into_inner());
            stats.runs += 1;
            stats.last_run_at = Some(Utc::now());
        }
        let result = self
            .cleanup_channel_by_id(app, &channel_id.to_string(), None, trigger)
            .await;
        self.stats
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .total_deleted += result.deleted;
        self.send_cleanup_log(app, std::slice::from_ref(&result), trigger)
            .await;
        self.update_next_run_for_all_guilds(app);
        self.schedule_next_warning(app);
        self.running.store(false, Ordering::SeqCst);
        Some(result)
    }

    async fn cleanup_channel_by_id(
        &self,
        app: &App,
        channel_id: &str,
        channel_cfg: Option<&Value>,
        trigger: &str,
    ) -> CleanupResult {
        let started = std::time::Instant::now();
        let mut result = CleanupResult {
            channel_id: channel_id.to_owned(),
            channel_name: "unknown".to_owned(),
            guild_id: None,
            deleted: 0,
            error: None,
        };

        let channel = match discord::parse_id::<ChannelMarker>(channel_id) {
            Some(id) => match app.http.channel(id).await {
                Ok(response) => response.model().await.ok(),
                Err(error) => {
                    result.error = Some(error.to_string());
                    None
                }
            },
            None => None,
        };
        let Some(channel) = channel.filter(|c| discord::is_text_based(c.kind)) else {
            let message = result
                .error
                .take()
                .unwrap_or_else(|| "Kanal nie istnieje lub nie jest tekstowy".to_owned());
            tracing::error!("Blad kanalu {channel_id}: {message}");
            result.error = Some(message);
            return result;
        };

        result.channel_name = channel.name.clone().unwrap_or_else(|| "unknown".to_owned());
        result.guild_id = channel.guild_id.map(|g| g.to_string());
        let settings = self.effective_channel_settings(channel_id, channel_cfg);

        let outcome = async {
            let candidates = self
                .fetch_messages_to_delete(app, channel.id, channel.guild_id, &settings)
                .await?;
            let deleted = self
                .delete_messages(app, channel.id, &result.channel_name, candidates)
                .await;
            self.send_auto_info_message(app, channel.id, &result.channel_name)
                .await;
            anyhow::Ok(deleted)
        }
        .await;

        match outcome {
            Ok(deleted) => result.deleted = deleted,
            Err(error) => {
                tracing::error!("Blad kanalu {}: {error}", channel.id);
                result.error = Some(error.to_string());
            }
        }

        if let Some(guild_id) = &result.guild_id {
            app.state.mark_cleanup_result(
                guild_id,
                trigger,
                result.deleted,
                result.error.as_deref(),
                started.elapsed().as_millis() as u64,
            );
        }
        result
    }

    fn should_delete(
        &self,
        app: &App,
        guild_id: Option<Id<GuildMarker>>,
        message: &Message,
        settings: &ChannelSettings,
        min_created_ms: Option<i64>,
    ) -> bool {
        if !settings.delete_pinned && message.pinned {
            return false;
        }
        let author = message.author.id.to_string();
        if settings.whitelist_users.contains(&author) {
            return false;
        }
        if !settings.whitelist_roles.is_empty()
            && let Some(guild_id) = guild_id
            && let Some(roles) = discord::member_roles(app, guild_id, message.author.id)
            && roles
                .iter()
                .any(|r| settings.whitelist_roles.contains(&r.to_string()))
        {
            return false;
        }
        if let Some(min) = min_created_ms
            && message.timestamp.as_micros() / 1000 > min
        {
            return false;
        }
        true
    }

    async fn fetch_messages_to_delete(
        &self,
        app: &App,
        channel_id: Id<ChannelMarker>,
        guild_id: Option<Id<GuildMarker>>,
        settings: &ChannelSettings,
    ) -> anyhow::Result<Vec<(Id<MessageMarker>, i64)>> {
        let mut out = Vec::new();
        let mut before: Option<Id<MessageMarker>> = None;
        let min_created = (settings.delete_older_than_days > 0).then(|| {
            Utc::now().timestamp_millis() - settings.delete_older_than_days as i64 * 86_400_000
        });

        loop {
            let batch: Vec<Message> = match before {
                Some(before) => {
                    app.http
                        .channel_messages(channel_id)
                        .before(before)
                        .limit(100)
                        .await?
                        .models()
                        .await?
                }
                None => {
                    app.http
                        .channel_messages(channel_id)
                        .limit(100)
                        .await?
                        .models()
                        .await?
                }
            };
            if batch.is_empty() {
                break;
            }
            for message in &batch {
                if !self.should_delete(app, guild_id, message, settings, min_created) {
                    continue;
                }
                out.push((message.id, message.timestamp.as_micros() / 1000));
                if settings.delete_limit > 0 && out.len() as u64 >= settings.delete_limit {
                    return Ok(out);
                }
            }
            before = batch.last().map(|m| m.id);
            if before.is_none() || batch.len() < 100 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Ok(out)
    }

    async fn delete_messages(
        &self,
        app: &App,
        channel_id: Id<ChannelMarker>,
        channel_name: &str,
        messages: Vec<(Id<MessageMarker>, i64)>,
    ) -> u64 {
        if messages.is_empty() {
            return 0;
        }
        if config::is_true(&self.cfg(), &["dryRun"]) {
            tracing::warn!(
                "[DRY RUN] Pominieto usuwanie {} wiadomosci z #{channel_name}",
                messages.len()
            );
            return 0;
        }

        let now = Utc::now().timestamp_millis();
        let two_weeks = 14 * 86_400_000;
        let (fresh, old): (Vec<_>, Vec<_>) = messages
            .into_iter()
            .partition(|(_, created)| now - created < two_weeks);

        let mut deleted = 0;
        for chunk in fresh.chunks(100) {
            let ids: Vec<Id<MessageMarker>> = chunk.iter().map(|(id, _)| *id).collect();
            let bulk_ok = if ids.len() == 1 {
                app.http.delete_message(channel_id, ids[0]).await.is_ok()
            } else {
                app.http.delete_messages(channel_id, &ids).await.is_ok()
            };
            if bulk_ok {
                deleted += ids.len() as u64;
            } else {
                for id in ids {
                    if app.http.delete_message(channel_id, id).await.is_ok() {
                        deleted += 1;
                        tokio::time::sleep(Duration::from_millis(700)).await;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(900)).await;
        }

        for (id, _) in old {
            if app.http.delete_message(channel_id, id).await.is_ok() {
                deleted += 1;
                tokio::time::sleep(Duration::from_millis(900)).await;
            }
        }
        deleted
    }

    async fn send_auto_info_message(
        &self,
        app: &App,
        channel_id: Id<ChannelMarker>,
        channel_name: &str,
    ) {
        let text = format!(
            "This channel automatically clears messages every {}.",
            Self::interval_text(&self.cfg())
        );
        let payload = json!({
            "flags": FLAG_COMPONENTS_V2,
            "components": [discord::container(vec![discord::text(text)])]
        });
        if let Err(error) = discord::send_message(app, channel_id, &payload).await {
            tracing::error!(
                "Nie udalo sie wyslac Components v2 na #{channel_name}: {error} payload={payload}"
            );
        }
    }

    async fn send_pre_delete_warnings(&self, app: &App) {
        let cfg = self.cfg();
        if !config::is_true(&cfg, &["notifyBeforeDelete", "enabled"]) {
            return;
        }
        let targets = self.pending_targets(app, &cfg, None);
        if targets.is_empty() {
            return;
        }
        let minutes = config::int(&cfg, &["notifyBeforeDelete", "minutesBefore"]);
        let template = config::at(&cfg, &["notifyBeforeDelete", "message"])
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("Za {minutes} minut kanal zostanie wyczyszczony.");
        let text = template.replace("{minutes}", &minutes.to_string());

        for target in targets {
            let Some(channel_id) = discord::parse_id::<ChannelMarker>(&target.channel_id) else {
                continue;
            };
            let Ok(response) = app.http.channel(channel_id).await else {
                continue;
            };
            let Ok(channel) = response.model().await else {
                continue;
            };
            if !discord::is_text_based(channel.kind) {
                continue;
            }
            if let Err(error) = app.http.create_message(channel_id).content(&text).await {
                tracing::warn!(
                    "Nie udalo sie wyslac notifyBeforeDelete na #{}: {error}",
                    channel.name.as_deref().unwrap_or(&target.channel_id)
                );
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    async fn send_cleanup_log(&self, app: &App, results: &[CleanupResult], trigger: &str) {
        let Some(log_channel) = config::text(&self.cfg(), &["logChannelId"])
            .and_then(|id| discord::parse_id::<ChannelMarker>(&id))
        else {
            return;
        };
        let deleted: u64 = results.iter().map(|r| r.deleted).sum();
        let failed = results.iter().filter(|r| r.error.is_some()).count();
        let mut lines = vec![
            "AutoClean report".to_owned(),
            format!("Trigger: {trigger}"),
            format!("Deleted: {deleted}"),
            format!("Errors: {failed}"),
            format!("Channels: {}", results.len()),
        ];
        for item in results.iter().take(10) {
            let suffix = item
                .error
                .as_ref()
                .map(|e| format!(" error={e}"))
                .unwrap_or_default();
            lines.push(format!(
                "- #{} ({}) deleted={}{suffix}",
                item.channel_name, item.channel_id, item.deleted
            ));
        }
        let payload = json!({
            "flags": FLAG_COMPONENTS_V2,
            "components": [discord::container(vec![
                discord::text("## AutoClean Report"),
                discord::text(lines.join("\n")),
            ])]
        });
        if let Err(error) = discord::send_message(app, log_channel, &payload).await {
            tracing::warn!("Nie udalo sie wyslac logu AutoClean: {error}");
        }
    }

    pub fn status_snapshot(&self, app: &App, guild_id: &str) -> StatusSnapshot {
        if let Err(error) = self.load_config(app) {
            tracing::warn!("AutoClean status: {error}");
        }
        let cfg = self.cfg();
        let configured = self.configured_entries(app, &cfg, Some(guild_id));
        let queued = self.queued_entries(app, Some(guild_id));
        let persisted = app.state.autoclean(guild_id);
        let stats = self.stats.lock().unwrap_or_else(|p| p.into_inner());
        let global_limit = config::int(&cfg, &["deleteLimit"]).max(0) as u64;
        let global_pinned = config::is_true(&cfg, &["deletePinned"]);

        StatusSnapshot {
            enabled: config::not_false(&cfg, &["enabled"]),
            permanent_channels: configured
                .iter()
                .map(|t| {
                    let channel_cfg = t.channel_config.clone().unwrap_or_default();
                    let limit = channel_cfg
                        .get("deleteLimit")
                        .filter(|v| !v.is_null())
                        .map_or(global_limit, |v| config::js_to_number(v).max(0.0) as u64);
                    let pinned = config::is_true(&channel_cfg, &["deletePinned"]) || global_pinned;
                    (t.channel_id.clone(), limit, pinned)
                })
                .collect(),
            queued,
            schedule: Self::schedule_pattern(&cfg),
            timezone: Self::timezone_name(&cfg),
            next_run_text: self.next_run_text(),
            next_run_at: persisted["nextRunAt"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .or_else(|| self.next_run_iso()),
            dry_run: config::is_true(&cfg, &["dryRun"]),
            interval_text: Self::interval_text(&cfg),
            delete_older_than_days: config::int(&cfg, &["deleteMessagesOlderThanDays"]).max(0)
                as u64,
            delete_limit: global_limit,
            delete_pinned: global_pinned,
            whitelist_users: config::string_list(&cfg, &["whitelist", "users"]).len(),
            whitelist_roles: config::string_list(&cfg, &["whitelist", "roles"]).len(),
            notify: if config::is_true(&cfg, &["notifyBeforeDelete", "enabled"]) {
                format!(
                    "{} min",
                    config::int(&cfg, &["notifyBeforeDelete", "minutesBefore"])
                )
            } else {
                "off".to_owned()
            },
            log_channel_id: config::text(&cfg, &["logChannelId"]),
            runs: stats.runs,
            total_deleted: stats.total_deleted,
            last_run_at: persisted["lastRunAt"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .or_else(|| stats.last_run_at.map(to_iso)),
            last_deleted: if persisted["lastDeleted"].is_null() {
                json!(0)
            } else {
                persisted["lastDeleted"].clone()
            },
            last_error: persisted["lastError"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            last_trigger: persisted["lastTrigger"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            last_duration_ms: persisted["lastDurationMs"].clone(),
        }
    }
}
