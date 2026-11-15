//! Music playback over Lavalink v4 (port of `src/core/music.js`).
//!
//! Queue, repeat, autoplay, filters, idle disconnect and voice handling follow the behaviour of
//! `lavalink-client` 2.x as configured by the JS bot (autoSkip, autoMove, autoReconnect,
//! destroyAfterMs, maxErrorsPerTime, maxPreviousTracks = 25).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use rand::seq::SliceRandom;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use twilight_model::gateway::payload::incoming::{VoiceServerUpdate, VoiceStateUpdate};
use twilight_model::gateway::payload::outgoing::UpdateVoiceState;
use twilight_model::id::Id;
use twilight_model::id::marker::{ChannelMarker, GuildMarker};
use unicode_normalization::UnicodeNormalization;

use crate::app::App;
use crate::common::env;
use crate::lavalink::{self, Node, NodeEvent, SearchResult};
use crate::util::text::compact_whitespace;

mod controls;
mod events;
mod matching;
mod persistence;
mod search;
mod voice;

pub use matching::*;

/// A bot restart continues sessions saved at most this long ago.
const RESTART_RESUME_MAX_AGE_MS: i64 = 10 * 60 * 1000;
const NORMALIZATION_MAX_AMPLITUDE: f64 = 0.6;
const ECHO_LENGTH_SECS: f64 = 0.3;
const ECHO_DECAY: f64 = 0.4;
const DEFAULT_SEARCH_SOURCE: &str = "ytmsearch";
const DEFAULT_SPOTIFY_MIRROR_SOURCES: [&str; 3] = ["ytsearch", "ytmsearch", "scsearch"];
const DEFAULT_RECOVERY_SOURCES: [&str; 3] = ["ytsearch", "ytmsearch", "scsearch"];
const IDLE_DISCONNECT_MS: f64 = 2.0 * 60.0 * 1000.0;
/// Leave the voice channel when no human listener is left for this long (0 = never).
const ALONE_DISCONNECT_MS: f64 = 60.0 * 1000.0;
const TRACK_RECOVERY_COOLDOWN: Duration = Duration::from_secs(90);
const MAX_VOICE_CHANNEL_STATUS_LENGTH: usize = 120;
const MAX_PREVIOUS_TRACKS: usize = 25;

// lavalink-client 2.x filter presets.
const NIGHTCORE: (f64, f64, f64) = (
    1.289_999_523_162_842,
    1.289_999_523_162_842,
    0.936_599_952_316_284_2,
);
const VAPORWAVE: (f64, f64, f64) = (0.850_000_023_841_857_9, 0.800_000_011_920_929, 1.0);
const ROTATION_HZ: f64 = 0.2;
const LOW_PASS_SMOOTHING: f64 = 8.0;
const BASSBOOST_MEDIUM: [f64; 15] = [
    0.6, 0.67, 0.67, 0.4, -0.5, 0.15, -0.45, 0.23, 0.35, 0.45, 0.55, -0.6, 0.55, -0.5, -0.75,
];
const BASSBOOST_MULTIPLIER: f64 = 0.1875;
const VOCAL_EQ: [f64; 15] = [
    -0.08, -0.06, -0.04, 0.02, 0.08, 0.12, 0.15, 0.14, 0.10, 0.06, 0.04, 0.02, 0.0, -0.02, -0.04,
];
const TREBLE_EQ: [f64; 15] = [
    -0.10, -0.08, -0.06, -0.03, 0.0, 0.02, 0.05, 0.08, 0.12, 0.16, 0.18, 0.18, 0.16, 0.13, 0.10,
];

fn recoverable_error_regex() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"(?i)(requires.*login|sign\s*in|forbidden|status code[:\s]*403|video player configuration|age[-\s]?restricted|not available|unavailable|something went wrong while looking up the track|failed to retrieve secret from spotify)")
            .expect("static regex")
    })
}

// ---------------------------------------------------------------------------------------------
// Pure helpers (same names/semantics as the JS functions)
// ---------------------------------------------------------------------------------------------

pub fn truncate_inline(value: &str, max: usize) -> String {
    let normalized = compact_whitespace(value);
    if normalized.chars().count() <= max {
        return normalized;
    }
    let cut: String = normalized.chars().take(max.saturating_sub(3)).collect();
    format!("{}...", cut.trim_end())
}

pub fn is_http_url(input: &str) -> bool {
    url::Url::parse(input.trim()).is_ok_and(|u| u.scheme() == "http" || u.scheme() == "https")
}

pub fn extract_youtube_video_id(input: &str) -> Option<String> {
    if !is_http_url(input) {
        return None;
    }
    let parsed = url::Url::parse(input.trim()).ok()?;
    let host = parsed.host_str().unwrap_or_default().to_lowercase();
    let path = parsed.path();
    let first_segment = |rest: &str| {
        rest.trim_start_matches('/')
            .split('/')
            .next()
            .map(str::to_owned)
            .filter(|s| !s.is_empty())
    };
    if host == "youtu.be" {
        return first_segment(path);
    }
    if host.ends_with("youtube.com") || host.ends_with("youtube-nocookie.com") {
        if path == "/watch" {
            return parsed
                .query_pairs()
                .find(|(k, _)| k == "v")
                .map(|(_, v)| v.trim().to_owned())
                .filter(|v| !v.is_empty());
        }
        if let Some(rest) = path.strip_prefix("/shorts/") {
            return first_segment(rest);
        }
        if let Some(rest) = path.strip_prefix("/embed/") {
            return first_segment(rest);
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct PublicTrack {
    pub title: String,
    pub duration_sec: i64,
    pub url: String,
    pub raw: Value,
}

pub fn normalize_track(track: &Value) -> PublicTrack {
    let author = info_str(track, "author");
    let title = info_str(track, "title");
    let parts: Vec<&str> = [
        if !author.is_empty() && author != "Unknown artist" {
            author
        } else {
            ""
        },
        if title.is_empty() {
            "Unknown track"
        } else {
            title
        },
    ]
    .into_iter()
    .filter(|p| !p.is_empty())
    .collect();
    let joined = compact_whitespace(&parts.join(" - "));
    PublicTrack {
        title: if joined.is_empty() {
            "Unknown track".to_owned()
        } else {
            joined
        },
        duration_sec: duration_sec(track),
        url: info_str(track, "uri").to_owned(),
        raw: track.clone(),
    }
}

fn playlist_title(result: &SearchResult) -> String {
    let title = result
        .playlist_title
        .clone()
        .or_else(|| result.plugin_info["albumName"].as_str().map(str::to_owned))
        .or_else(|| result.plugin_info["identifier"].as_str().map(str::to_owned))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "Playlist".to_owned());
    compact_whitespace(&title)
}

// ---------------------------------------------------------------------------------------------
// Player state
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Repeat {
    #[default]
    Off,
    Track,
    Queue,
}

impl Repeat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Track => "track",
            Self::Queue => "queue",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "track" => Self::Track,
            "queue" => Self::Queue,
            _ => Self::Off,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Timescale {
    #[default]
    Normal,
    Nightcore,
    Vaporwave,
}

/// App-level filter flags (`createDefaultFilterState`).
#[derive(Debug, Clone)]
pub struct FilterFlags {
    pub bassboost: bool,
    pub nightcore: bool,
    pub vaporwave: bool,
    pub eight_d: bool,
    pub karaoke: bool,
    pub soft_limiter: bool,
    pub eq_preset: String,
}

impl Default for FilterFlags {
    fn default() -> Self {
        Self {
            bassboost: false,
            nightcore: false,
            vaporwave: false,
            eight_d: false,
            karaoke: false,
            soft_limiter: false,
            eq_preset: "flat".to_owned(),
        }
    }
}

#[derive(Clone)]
struct Player {
    instance: u64,
    guild_id: Id<GuildMarker>,
    voice_channel_id: Option<Id<ChannelMarker>>,
    current: Option<Value>,
    tracks: VecDeque<Value>,
    previous: VecDeque<Value>,
    playing: bool,
    paused: bool,
    volume: i64,
    repeat: Repeat,
    last_position: i64,
    last_position_at: Option<Instant>,
    connected: bool,
    ping: Option<i64>,
    voice_session: Option<String>,
    voice_token: Option<String>,
    voice_endpoint: Option<String>,
    node_session: Option<String>,
    flags: FilterFlags,
    timescale: Timescale,
    rotation: bool,
    karaoke: bool,
    low_pass: bool,
    normalization: bool,
    echo: bool,
    equalizer: Vec<f64>,
    skipped: bool,
    destroying: bool,
    reconnecting: bool,
    autoplay: bool,
    previous_volume: Option<i64>,
    recovery: HashMap<String, Instant>,
    errors: VecDeque<Instant>,
    idle_generation: u64,
    alone_generation: u64,
}

impl Player {
    fn new(
        instance: u64,
        guild_id: Id<GuildMarker>,
        channel: Id<ChannelMarker>,
        volume: i64,
    ) -> Self {
        Self {
            instance,
            guild_id,
            voice_channel_id: Some(channel),
            current: None,
            tracks: VecDeque::new(),
            previous: VecDeque::new(),
            playing: false,
            paused: false,
            volume,
            repeat: Repeat::Off,
            last_position: 0,
            last_position_at: None,
            connected: false,
            ping: None,
            voice_session: None,
            voice_token: None,
            voice_endpoint: None,
            node_session: None,
            flags: FilterFlags::default(),
            timescale: Timescale::Normal,
            rotation: false,
            karaoke: false,
            low_pass: false,
            normalization: false,
            echo: false,
            equalizer: Vec::new(),
            skipped: false,
            destroying: false,
            reconnecting: false,
            autoplay: false,
            previous_volume: None,
            recovery: HashMap::new(),
            errors: VecDeque::new(),
            idle_generation: 0,
            alone_generation: 0,
        }
    }

    fn position_ms(&self) -> i64 {
        match self.last_position_at {
            Some(at) if self.playing && !self.paused => {
                let position = self.last_position + at.elapsed().as_millis() as i64;
                let length = self.current.as_ref().map_or(0.0, duration_ms) as i64;
                if length > 0 {
                    position.min(length)
                } else {
                    position
                }
            }
            _ => self.last_position,
        }
    }

    fn filters_body(&self) -> Value {
        let mut filters = serde_json::Map::new();
        let timescale = match self.timescale {
            Timescale::Normal => None,
            Timescale::Nightcore => Some(NIGHTCORE),
            Timescale::Vaporwave => Some(VAPORWAVE),
        };
        if let Some((speed, pitch, rate)) = timescale {
            filters.insert(
                "timescale".into(),
                json!({ "speed": speed, "pitch": pitch, "rate": rate }),
            );
        }
        if self.karaoke {
            filters.insert(
                "karaoke".into(),
                json!({ "level": 1, "monoLevel": 1, "filterBand": 220, "filterWidth": 100 }),
            );
        }
        if self.rotation {
            filters.insert("rotation".into(), json!({ "rotationHz": ROTATION_HZ }));
        }
        if self.low_pass {
            filters.insert("lowPass".into(), json!({ "smoothing": LOW_PASS_SMOOTHING }));
        }
        // LavaDSPX plugin filters.
        let mut plugin_filters = serde_json::Map::new();
        if self.normalization {
            plugin_filters.insert(
                "normalization".into(),
                json!({ "maxAmplitude": NORMALIZATION_MAX_AMPLITUDE, "adaptive": true }),
            );
        }
        if self.echo {
            plugin_filters.insert(
                "echo".into(),
                json!({ "echoLength": ECHO_LENGTH_SECS, "decay": ECHO_DECAY }),
            );
        }
        if !plugin_filters.is_empty() {
            filters.insert("pluginFilters".into(), Value::Object(plugin_filters));
        }
        if !self.equalizer.is_empty() {
            let bands: Vec<Value> = self
                .equalizer
                .iter()
                .enumerate()
                .map(|(band, gain)| json!({ "band": band, "gain": gain }))
                .collect();
            filters.insert("equalizer".into(), Value::Array(bands));
        }
        Value::Object(filters)
    }

    fn voice_body(&self) -> Option<Value> {
        Some(json!({
            "token": self.voice_token.clone()?,
            "endpoint": self.voice_endpoint.clone()?,
            "sessionId": self.voice_session.clone()?,
            "channelId": self.voice_channel_id.map(|c| c.to_string()),
        }))
    }

    pub(crate) fn has_music(&self) -> bool {
        self.current.is_some() || !self.tracks.is_empty() || self.playing
    }

    /// `setEQ`: merges bands by index.
    fn set_eq(&mut self, bands: &[f64]) {
        if self.equalizer.len() < bands.len() {
            self.equalizer.resize(bands.len(), 0.0);
        }
        self.equalizer[..bands.len()].copy_from_slice(bands);
    }

    /// `queueTrackEnd` from lavalink-client.
    fn queue_track_end(&mut self) {
        if let Some(current) = self.current.take() {
            self.previous.push_front(current.clone());
            self.previous.truncate(MAX_PREVIOUS_TRACKS);
            if self.repeat == Repeat::Queue {
                self.tracks.push_back(current);
            }
        }
        self.current = self.tracks.pop_front();
    }

    fn status_text(&self) -> Option<String> {
        let current = self.current.as_ref()?;
        if !self.playing && !self.paused {
            return None;
        }
        let track = normalize_track(current);
        let prefix = if self.paused { "Paused: " } else { "Playing: " };
        Some(truncate_inline(
            &format!("{prefix}{}", track.title),
            MAX_VOICE_CHANNEL_STATUS_LENGTH,
        ))
    }
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub guild_id: Id<GuildMarker>,
    pub voice_channel_id: Option<Id<ChannelMarker>>,
    pub current_track: Option<PublicTrack>,
    pub queue: Vec<PublicTrack>,
    pub playing: bool,
    pub paused: bool,
    pub volume: i64,
    pub position_sec: i64,
    pub repeat_mode: Repeat,
    pub muted: bool,
    pub autoplay: bool,
    pub filters: FilterFlags,
}

fn snapshot_of(player: &Player) -> Option<Snapshot> {
    let current = player.current.as_ref().map(normalize_track);
    if current.is_none() && player.tracks.is_empty() && !player.playing {
        return None;
    }
    Some(Snapshot {
        guild_id: player.guild_id,
        voice_channel_id: player.voice_channel_id,
        current_track: current,
        queue: player.tracks.iter().map(normalize_track).collect(),
        playing: player.playing,
        paused: player.paused,
        volume: player.volume,
        position_sec: js_round(player.position_ms() as f64 / 1000.0),
        repeat_mode: player.repeat,
        muted: player.volume == 0,
        autoplay: player.autoplay,
        filters: player.flags.clone(),
    })
}

pub struct Session {
    pub guild_id: Id<GuildMarker>,
    pub instance: u64,
    pub created_player: bool,
}

pub struct EnqueueResult {
    pub started: bool,
    pub position: usize,
    pub track: PublicTrack,
}

pub struct EnqueueManyResult {
    pub added: usize,
    pub started: bool,
    pub started_track: Option<PublicTrack>,
}

pub struct Resolved {
    pub playlist: bool,
    pub playlist_title: Option<String>,
    pub tracks: Vec<PublicTrack>,
}

pub struct ReconnectResult {
    pub voice_channel_id: Id<ChannelMarker>,
    pub node_connected: bool,
}

// Mirrors the JS diagnostics/snapshot shape; not every field is rendered.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct NodeDiagnostics {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub secure: bool,
    pub connected: bool,
    pub session_id: Option<String>,
    pub players: i64,
    pub playing_players: i64,
    pub cpu_load: f64,
    pub system_load: f64,
    pub memory_used: i64,
}

// Mirrors the JS diagnostics/snapshot shape; not every field is rendered.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct GuildSession {
    pub voice_channel_id: Option<Id<ChannelMarker>>,
    pub node_id: String,
    pub node_connected: bool,
    pub player_connected: bool,
    pub ws_ping: Option<i64>,
    pub current_track_title: Option<String>,
    pub queue_size: usize,
    pub playing: bool,
    pub paused: bool,
    pub status_text: Option<String>,
    pub cached_status_text: Option<String>,
    pub has_saved_queue: bool,
}

// Mirrors the JS diagnostics/snapshot shape; not every field is rendered.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Diagnostics {
    pub initialized: bool,
    pub usable: bool,
    pub default_search_source: String,
    pub recovery_sources: Vec<String>,
    pub default_volume: i64,
    pub idle_disconnect_ms: f64,
    pub configured_node: Option<lavalink::NodeConfig>,
    pub config_error: Option<String>,
    pub uses_url: bool,
    pub has_explicit_host: bool,
    pub has_explicit_port: bool,
    pub has_explicit_password: bool,
    pub has_explicit_secure: bool,
    pub node: Option<NodeDiagnostics>,
    pub active_players: usize,
    pub voice_status_channels: usize,
    pub queue_state_dir: PathBuf,
    pub queue_state_dir_exists: bool,
    pub legacy_presence_cleared: bool,
    pub guild_session: Option<GuildSession>,
}

// ---------------------------------------------------------------------------------------------
// Manager
// ---------------------------------------------------------------------------------------------

pub struct MusicManager {
    node: Option<Arc<Node>>,
    config_error: Option<String>,
    players: tokio::sync::Mutex<HashMap<Id<GuildMarker>, Player>>,
    app: OnceLock<Weak<App>>,
    default_search_source: String,
    spotify_mirror_sources: Vec<String>,
    recovery_sources: Vec<String>,
    default_volume: i64,
    idle_disconnect_ms: f64,
    alone_disconnect_ms: f64,
    error_threshold_ms: f64,
    error_max_amount: f64,
    queue_state_dir: PathBuf,
    initialized: AtomicBool,
    next_instance: AtomicU64,
    voice_statuses: Mutex<HashMap<Id<ChannelMarker>, String>>,
    voice_sync: tokio::sync::Mutex<()>,
    legacy_presence_cleared: AtomicBool,
}

/// `envNumber(name, fallback)`
fn env_number(name: &str, fallback: f64) -> f64 {
    match std::env::var(name) {
        Ok(raw) if raw.trim().is_empty() => 0.0,
        Ok(raw) => raw
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .unwrap_or(fallback),
        Err(_) => fallback,
    }
}

impl MusicManager {
    pub fn new(queue_state_dir: &Path) -> Self {
        let (node, config_error) = match lavalink::node_config_from_env().and_then(Node::new) {
            Ok(node) => (Some(Arc::new(node)), None),
            Err(error) => (None, Some(error.to_string())),
        };
        let default_search_source = env::var("LAVALINK_SEARCH_SOURCE")
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_SEARCH_SOURCE.to_owned());
        Self {
            node,
            config_error,
            players: tokio::sync::Mutex::new(HashMap::new()),
            app: OnceLock::new(),
            default_search_source,
            spotify_mirror_sources: parse_csv_list(
                env::var("SPOTIFY_MIRROR_SOURCES"),
                &DEFAULT_SPOTIFY_MIRROR_SOURCES,
            ),
            recovery_sources: parse_csv_list(
                env::var("MUSIC_RECOVERY_SOURCES"),
                &DEFAULT_RECOVERY_SOURCES,
            ),
            // 100 %: Lavalink passes Opus sources through untouched; any other volume makes it
            // decode and re-encode every track.
            default_volume: js_round(env_number("LAVALINK_VOLUME", 100.0)),
            idle_disconnect_ms: env_number("MUSIC_IDLE_DISCONNECT_MS", IDLE_DISCONNECT_MS),
            alone_disconnect_ms: env_number("MUSIC_ALONE_DISCONNECT_MS", ALONE_DISCONNECT_MS),
            error_threshold_ms: env_number("MUSIC_ERROR_THRESHOLD_MS", 120_000.0),
            error_max_amount: env_number("MUSIC_ERROR_MAX_AMOUNT", 30.0),
            queue_state_dir: queue_state_dir.to_path_buf(),
            initialized: AtomicBool::new(false),
            next_instance: AtomicU64::new(1),
            voice_statuses: Mutex::new(HashMap::new()),
            voice_sync: tokio::sync::Mutex::new(()),
            legacy_presence_cleared: AtomicBool::new(false),
        }
    }

    fn app(&self) -> Option<Arc<App>> {
        self.app.get().and_then(Weak::upgrade)
    }

    fn node(&self) -> Result<&Arc<Node>> {
        self.node
            .as_ref()
            .ok_or_else(|| anyhow!("No Lavalink node available."))
    }

    fn usable(&self) -> bool {
        self.node.as_ref().is_some_and(|n| n.connected())
    }

    fn ensure_ready(&self) -> Result<()> {
        if !self.initialized.load(Ordering::SeqCst) || !self.usable() {
            bail!(
                "Lavalink is not connected. Check LAVALINK_HOST/LAVALINK_PORT/LAVALINK_PASSWORD and that Lavalink v4 is running."
            );
        }
        Ok(())
    }

    /// `initializeProviders`: starts the node connection and the event loop.
    pub fn initialize(&self, app: &Arc<App>) {
        if self.app.set(Arc::downgrade(app)).is_err() {
            return;
        }
        let Some(node) = self.node.clone() else {
            tracing::error!(
                "Music provider unavailable: Lavalink ({})",
                self.config_error.as_deref().unwrap_or("invalid config")
            );
            self.refresh_status();
            return;
        };
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(node.run(app.bot_id.get(), tx));
        tokio::spawn(event_loop(Arc::downgrade(app), rx));
        self.initialized.store(true, Ordering::SeqCst);
        self.refresh_status();
    }

    // --- sessions ------------------------------------------------------------------------------

    pub async fn ensure_session(
        &self,
        app: &App,
        guild_id: Id<GuildMarker>,
        voice_channel_id: Id<ChannelMarker>,
    ) -> Result<Session> {
        self.ensure_ready()?;
        let mut players = self.players.lock().await;
        if let Some(existing) = players.get(&guild_id)
            && let Some(current_channel) = existing.voice_channel_id
            && current_channel != voice_channel_id
        {
            if existing.has_music() {
                bail!("The bot is already playing music in <#{current_channel}>.");
            }
            drop(players);
            self.destroy(app, guild_id, "voice channel changed").await;
            players = self.players.lock().await;
        }

        let created_player = !players.contains_key(&guild_id);
        if created_player {
            let instance = self.next_instance.fetch_add(1, Ordering::SeqCst);
            players.insert(
                guild_id,
                Player::new(instance, guild_id, voice_channel_id, self.default_volume),
            );
        }
        let Some(player) = players.get_mut(&guild_id) else {
            bail!("The music session expired.");
        };
        player.idle_generation += 1;
        let session = Session {
            guild_id,
            instance: player.instance,
            created_player,
        };
        if !player.connected {
            player.voice_channel_id = Some(voice_channel_id);
            if let Err(error) = Self::send_voice(app, guild_id, Some(voice_channel_id)) {
                drop(players);
                self.cleanup_fresh_session(app, &session, "music connect failed")
                    .await;
                return Err(error);
            }
        }
        Ok(session)
    }

    /// `cleanupFreshSession`
    pub async fn cleanup_fresh_session(&self, app: &App, session: &Session, reason: &str) -> bool {
        if !session.created_player {
            return false;
        }
        let channel = {
            let players = self.players.lock().await;
            match players.get(&session.guild_id) {
                Some(player) if player.instance == session.instance => player.voice_channel_id,
                _ => return false,
            }
        };
        self.clear_voice_status(app, channel).await;
        self.destroy(app, session.guild_id, reason).await;
        true
    }

    async fn play_current(&self, player: &mut Player, no_replace: bool) -> Result<()> {
        if player.current.is_none() {
            player.current = player.tracks.pop_front();
        }
        let Some(current) = player.current.clone() else {
            return Ok(());
        };
        let node = self.node()?;
        let mut body = json!({
            "track": lavalink::track_body(&current),
            "volume": player.volume,
            "paused": false,
        });
        if player.node_session != node.session_id() {
            body["filters"] = player.filters_body();
            if let Some(voice) = player.voice_body() {
                body["voice"] = voice;
            }
        }
        node.update_player(&player.guild_id.to_string(), &body, no_replace)
            .await?;
        player.node_session = node.session_id();
        player.playing = true;
        player.paused = false;
        player.last_position = 0;
        player.last_position_at = Some(Instant::now());
        Ok(())
    }

    pub async fn enqueue(&self, session: &Session, track: &PublicTrack) -> Result<EnqueueResult> {
        let mut players = self.players.lock().await;
        let player = players
            .get_mut(&session.guild_id)
            .ok_or_else(|| anyhow!("The music session expired."))?;
        let was_idle = !player.playing && player.current.is_none();
        let position = player.tracks.len() + 1;
        player.tracks.push_back(track.raw.clone());
        if was_idle {
            self.play_current(player, false).await?;
        }
        Ok(EnqueueResult {
            started: was_idle,
            position,
            track: normalize_track(&track.raw),
        })
    }

    pub async fn enqueue_many(
        &self,
        session: &Session,
        tracks: &[PublicTrack],
    ) -> Result<EnqueueManyResult> {
        if tracks.is_empty() {
            bail!("No tracks found to add.");
        }
        let mut players = self.players.lock().await;
        let player = players
            .get_mut(&session.guild_id)
            .ok_or_else(|| anyhow!("The music session expired."))?;
        let was_idle = !player.playing && player.current.is_none();
        player.tracks.extend(tracks.iter().map(|t| t.raw.clone()));
        if was_idle {
            self.play_current(player, false).await?;
        }
        Ok(EnqueueManyResult {
            added: tracks.len(),
            started: was_idle,
            started_track: was_idle.then(|| normalize_track(&tracks[0].raw)),
        })
    }

    pub async fn snapshot(&self, guild_id: Id<GuildMarker>) -> Option<Snapshot> {
        self.players
            .lock()
            .await
            .get(&guild_id)
            .and_then(snapshot_of)
    }

    pub async fn diagnostics(&self, guild_id: Option<Id<GuildMarker>>) -> Diagnostics {
        let players = self.players.lock().await;
        let node = self.node.as_ref().map(|node| {
            let stats = node.stats();
            NodeDiagnostics {
                id: node.config.id.clone(),
                host: node.config.host.clone(),
                port: node.config.port,
                secure: node.config.secure,
                connected: node.connected(),
                session_id: node.session_id(),
                players: stats["players"].as_i64().unwrap_or(0),
                playing_players: stats["playingPlayers"].as_i64().unwrap_or(0),
                cpu_load: stats["cpu"]["lavalinkLoad"].as_f64().unwrap_or(0.0),
                system_load: stats["cpu"]["systemLoad"].as_f64().unwrap_or(0.0),
                memory_used: stats["memory"]["used"].as_i64().unwrap_or(0),
            }
        });
        let set = |name: &str| env::var(name).is_some();
        let guild_session = guild_id.and_then(|gid| players.get(&gid)).map(|player| {
            let snapshot = snapshot_of(player);
            GuildSession {
                voice_channel_id: player.voice_channel_id,
                node_id: self
                    .node
                    .as_ref()
                    .map(|n| n.config.id.clone())
                    .unwrap_or_default(),
                node_connected: self.usable(),
                player_connected: player.connected,
                ws_ping: player.ping,
                current_track_title: snapshot
                    .as_ref()
                    .and_then(|s| s.current_track.as_ref().map(|t| t.title.clone())),
                queue_size: snapshot.as_ref().map_or(0, |s| s.queue.len()),
                playing: snapshot.as_ref().is_some_and(|s| s.playing),
                paused: snapshot.as_ref().is_some_and(|s| s.paused),
                status_text: player.status_text(),
                cached_status_text: player.voice_channel_id.and_then(|c| self.cached_status(c)),
                has_saved_queue: self.queue_state_path(player.guild_id).exists(),
            }
        });
        Diagnostics {
            initialized: self.initialized.load(Ordering::SeqCst),
            usable: self.usable(),
            default_search_source: self.default_search_source.clone(),
            recovery_sources: self.recovery_sources.clone(),
            default_volume: self.default_volume,
            idle_disconnect_ms: self.idle_disconnect_ms,
            configured_node: self.node.as_ref().map(|n| n.config.clone()),
            config_error: self.config_error.clone(),
            uses_url: set("LAVALINK_URL"),
            has_explicit_host: set("LAVALINK_HOST"),
            has_explicit_port: set("LAVALINK_PORT"),
            has_explicit_password: set("LAVALINK_PASSWORD"),
            has_explicit_secure: set("LAVALINK_SECURE"),
            node,
            active_players: players.len(),
            voice_status_channels: self
                .voice_statuses
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .len(),
            queue_state_dir: self.queue_state_dir.clone(),
            queue_state_dir_exists: self.queue_state_dir.exists(),
            legacy_presence_cleared: self.legacy_presence_cleared.load(Ordering::SeqCst),
            guild_session,
        }
    }
}

fn recovery_key(track: &Value) -> String {
    [
        info_str(track, "sourceName").to_lowercase(),
        info_str(track, "identifier").to_owned(),
        info_str(track, "uri").to_owned(),
        track_seed(track).to_lowercase(),
    ]
    .join("|")
}

async fn event_loop(app: Weak<App>, mut events: mpsc::UnboundedReceiver<NodeEvent>) {
    while let Some(event) = events.recv().await {
        let Some(app) = app.upgrade() else { return };
        app.music.on_node_event(&app, event).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verified against Lavalink 4.2.2 + LavaDSPX 0.0.5: this body is accepted and echoed
    /// back; without the plugin Lavalink ignores `pluginFilters` and keeps the other filters.
    #[test]
    fn plugin_filters_body() {
        let mut player = Player::new(1, Id::new(1), Id::new(2), 100);
        assert_eq!(player.filters_body(), json!({}));
        player.normalization = true;
        player.echo = true;
        assert_eq!(
            player.filters_body()["pluginFilters"],
            json!({
                "normalization": { "maxAmplitude": 0.6, "adaptive": true },
                "echo": { "echoLength": 0.3, "decay": 0.4 }
            })
        );
    }

    fn track(title: &str, author: &str, length: i64, source: &str) -> Value {
        json!({
            "encoded": "x",
            "info": {
                "title": title,
                "author": author,
                "length": length,
                "sourceName": source,
                "uri": format!("https://{source}/{title}")
            },
            "pluginInfo": {}
        })
    }

    #[test]
    fn match_normalization() {
        assert_eq!(
            normalize_for_match("Zażółć  GĘŚLĄ-jaźń!"),
            // `ł` has no NFKD decomposition, so (as in JS) it becomes a separator.
            "zazo c gesla jazn"
        );
        assert!((token_overlap_ratio("Hello World", "world hello extra") - 1.0).abs() < 1e-9);
        assert!(has_variant_marker("Song (Live)"));
    }

    #[test]
    fn mirror_picks_best_candidate() {
        let original = track("Blinding Lights", "The Weeknd", 200_000, "spotify");
        let candidates = vec![
            track("Blinding Lights (cover)", "Someone", 200_000, "youtube"),
            track("Blinding Lights", "The Weeknd", 201_000, "youtube"),
            track("Blinding Lights", "The Weeknd", 30_000, "youtube"),
        ];
        let picked = pick_mirrored_track(&candidates, &original).unwrap();
        assert_eq!(picked["info"]["length"], 201_000);
        assert!(pick_mirrored_track(&candidates[..1], &original).is_none());
    }

    #[test]
    fn youtube_ids() {
        assert_eq!(
            extract_youtube_video_id("https://youtu.be/abc123?t=1").as_deref(),
            Some("abc123")
        );
        assert_eq!(
            extract_youtube_video_id("https://www.youtube.com/watch?v=xyz").as_deref(),
            Some("xyz")
        );
        assert_eq!(
            extract_youtube_video_id("https://youtube.com/shorts/s1/x").as_deref(),
            Some("s1")
        );
        assert_eq!(youtube_resolve_candidates("https://youtu.be/abc").len(), 4);
        assert_eq!(
            youtube_resolve_candidates("https://example.com/a.mp3").len(),
            1
        );
    }

    #[test]
    fn track_titles() {
        let p = normalize_track(&track("Song", "Unknown artist", 61_500, "http"));
        assert_eq!(p.title, "Song");
        assert_eq!(p.duration_sec, 62);
        assert_eq!(truncate_inline("a  b   c", 80), "a b c");
        assert_eq!(truncate_inline(&"x".repeat(10), 8), "xxxxx...");
    }

    #[test]
    fn csv_lists() {
        assert_eq!(
            parse_csv_list(Some(" A, b,a ,".into()), &["z"]),
            vec!["a", "b"]
        );
        assert_eq!(parse_csv_list(None, &["z"]), vec!["z"]);
    }

    #[test]
    fn queue_track_end_respects_repeat_queue() {
        let mut player = Player::new(1, Id::new(1), Id::new(2), 80);
        player.current = Some(track("a", "x", 1000, "youtube"));
        player.tracks.push_back(track("b", "x", 1000, "youtube"));
        player.repeat = Repeat::Queue;
        player.queue_track_end();
        assert_eq!(info_str(player.current.as_ref().unwrap(), "title"), "b");
        assert_eq!(info_str(&player.tracks[0], "title"), "a");
        assert_eq!(player.previous.len(), 1);
    }

    #[test]
    fn filter_bodies() {
        let mut player = Player::new(1, Id::new(1), Id::new(2), 80);
        assert_eq!(player.filters_body(), json!({}));
        player.timescale = Timescale::Nightcore;
        player.rotation = true;
        player.set_eq(&[0.0; 15]);
        let body = player.filters_body();
        assert_eq!(body["rotation"]["rotationHz"], 0.2);
        assert_eq!(body["equalizer"].as_array().unwrap().len(), 15);
        assert!(body["timescale"]["speed"].as_f64().unwrap() > 1.28);
    }
}
