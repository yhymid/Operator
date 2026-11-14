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
use twilight_model::gateway::OpCode;
use twilight_model::gateway::payload::incoming::{VoiceServerUpdate, VoiceStateUpdate};
use twilight_model::gateway::payload::outgoing::UpdateVoiceState;
use twilight_model::gateway::payload::outgoing::update_presence::{
    UpdatePresence, UpdatePresencePayload,
};
use twilight_model::gateway::presence::Status;
use twilight_model::id::Id;
use twilight_model::id::marker::{ChannelMarker, GuildMarker};
use unicode_normalization::UnicodeNormalization;

use crate::app::App;
use crate::lavalink::{self, Node, NodeEvent, SearchResult};
use crate::util::text::compact_whitespace;
use operator_common::env;

const DEFAULT_SEARCH_SOURCE: &str = "ytmsearch";
const DEFAULT_SPOTIFY_MIRROR_SOURCES: [&str; 3] = ["ytsearch", "ytmsearch", "scsearch"];
const DEFAULT_RECOVERY_SOURCES: [&str; 3] = ["ytsearch", "ytmsearch", "scsearch"];
const SPOTIFY_PRIMARY_SOURCE_NAMES: [&str; 2] = ["youtube", "youtubemusic"];
const SPOTIFY_FALLBACK_SOURCE_NAMES: [&str; 1] = ["soundcloud"];
const SPOTIFY_MIRROR_MIN_SCORE: i64 = 70;
const SPOTIFY_MIRROR_MIN_TITLE_OVERLAP: f64 = 0.46;
const SPOTIFY_MIRROR_MIN_AUTHOR_OVERLAP: f64 = 0.24;
const SPOTIFY_MIRROR_MAX_DURATION_DIFF_SEC: i64 = 22;
const SPOTIFY_MIRROR_MAX_DURATION_DIFF_RATIO: f64 = 0.14;
const MATCH_VARIANT_MARKERS: [&str; 14] = [
    "tribute",
    "cover",
    "karaoke",
    "remix",
    "bootleg",
    "edit",
    "mashup",
    "slowed",
    "sped",
    "nightcore",
    "instrumental",
    "8d",
    "reverb",
    "live",
];
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

/// `(query, source)` candidates; `source = None` means "default source / raw URL".
fn youtube_resolve_candidates(raw: &str) -> Vec<(String, Option<String>)> {
    let raw = raw.trim().to_owned();
    let Some(id) = extract_youtube_video_id(&raw) else {
        return vec![(raw, None)];
    };
    let candidates = vec![
        (raw, None),
        (format!("https://www.youtube.com/watch?v={id}"), None),
        (id.clone(), Some("ytsearch".to_owned())),
        (id, Some("ytmsearch".to_owned())),
    ];
    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter(|c| seen.insert(c.clone()))
        .collect()
}

fn parse_csv_list(value: Option<String>, fallback: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in value
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
    {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    if out.is_empty() {
        fallback.iter().map(|s| (*s).to_owned()).collect()
    } else {
        out
    }
}

fn info_str<'a>(track: &'a Value, key: &str) -> &'a str {
    track["info"][key].as_str().unwrap_or_default()
}

fn or_unknown(value: &str) -> &str {
    if value.is_empty() { "unknown" } else { value }
}

fn duration_ms(track: &Value) -> f64 {
    track["info"]["length"]
        .as_f64()
        .or_else(|| track["info"]["duration"].as_f64())
        .unwrap_or(0.0)
}

fn js_round(value: f64) -> i64 {
    (value + 0.5).floor() as i64
}

fn duration_sec(track: &Value) -> i64 {
    js_round(duration_ms(track) / 1000.0)
}

pub fn is_spotify_track(track: &Value) -> bool {
    info_str(track, "sourceName").eq_ignore_ascii_case("spotify")
        || info_str(track, "uri")
            .to_lowercase()
            .contains("open.spotify.com/")
}

fn track_isrc(track: &Value) -> String {
    compact_whitespace(info_str(track, "isrc"))
}

fn track_seed(track: &Value) -> String {
    let author = info_str(track, "author");
    let author = if author.eq_ignore_ascii_case("unknown artist") {
        ""
    } else {
        author
    };
    compact_whitespace(&format!("{author} {}", info_str(track, "title")))
}

pub fn normalize_for_match(value: &str) -> String {
    let lowered = value.to_lowercase();
    let stripped: String = lowered
        .nfkd()
        .filter(|c| !('\u{0300}'..='\u{036f}').contains(c))
        .collect();
    let mut out = String::new();
    let mut gap = false;
    for c in stripped.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.push(c);
        } else {
            gap = true;
        }
    }
    out
}

fn tokens(value: &str) -> Vec<String> {
    normalize_for_match(value)
        .split(' ')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

fn token_overlap_ratio(left: &str, right: &str) -> f64 {
    let left_tokens = tokens(left);
    let right_tokens: HashSet<String> = tokens(right).into_iter().collect();
    if left_tokens.is_empty() || right_tokens.is_empty() {
        return 0.0;
    }
    let matches = left_tokens
        .iter()
        .filter(|t| right_tokens.contains(*t))
        .count();
    matches as f64 / left_tokens.len() as f64
}

fn has_variant_marker(value: &str) -> bool {
    let normalized = normalize_for_match(value);
    !normalized.is_empty() && MATCH_VARIANT_MARKERS.iter().any(|m| normalized.contains(m))
}

fn is_likely_preview_length(original: i64, candidate: i64) -> bool {
    original > 0 && candidate > 0 && original >= 120 && (25..=70).contains(&candidate)
}

fn is_preview_track(track: &Value) -> bool {
    track["pluginInfo"]["isPreview"] == Value::Bool(true)
        || track["info"]["isPreview"] == Value::Bool(true)
}

fn is_mirror_candidate_source(source: &str) -> bool {
    let source = source.to_lowercase();
    SPOTIFY_PRIMARY_SOURCE_NAMES.contains(&source.as_str())
        || SPOTIFY_FALLBACK_SOURCE_NAMES.contains(&source.as_str())
}

pub fn score_mirrored_candidate(original: &Value, candidate: &Value) -> i64 {
    if candidate["info"].is_null() || is_spotify_track(candidate) {
        return -100_000;
    }
    if candidate["info"]["isStream"] == Value::Bool(true) {
        return -5000;
    }
    if is_preview_track(candidate) {
        return -100_000;
    }
    let original_title = info_str(original, "title");
    let original_author = info_str(original, "author");
    let candidate_title = info_str(candidate, "title");
    let candidate_author = info_str(candidate, "author");
    let original_duration = duration_sec(original);
    let candidate_duration = duration_sec(candidate);

    let title_overlap = token_overlap_ratio(original_title, candidate_title);
    let author_overlap = token_overlap_ratio(original_author, candidate_author);
    let mut score = js_round(title_overlap * 65.0) + js_round(author_overlap * 45.0);

    let normalized_original = normalize_for_match(original_title);
    let normalized_candidate = normalize_for_match(candidate_title);
    if !normalized_original.is_empty() && normalized_original == normalized_candidate {
        score += 30;
    }
    if !normalized_original.is_empty() && normalized_candidate.contains(&normalized_original) {
        score += 10;
    }

    if original_duration > 0 && candidate_duration > 0 {
        if is_likely_preview_length(original_duration, candidate_duration) {
            return -10_000;
        }
        let diff = (original_duration - candidate_duration).abs();
        score += match diff {
            0..=2 => 25,
            3..=5 => 20,
            6..=10 => 14,
            11..=20 => 8,
            21..=35 => 3,
            36..=55 => -8,
            _ => -20,
        };
    }

    if !has_variant_marker(&format!("{original_title} {original_author}"))
        && has_variant_marker(&format!("{candidate_title} {candidate_author}"))
    {
        score -= 30;
    }

    match info_str(candidate, "sourceName").to_lowercase().as_str() {
        "deezer" => score += 14,
        "soundcloud" => score += 3,
        "youtube" | "youtubemusic" => score -= 8,
        _ => {}
    }
    if author_overlap < 0.25 && !original_author.is_empty() {
        score -= 15;
    }
    if title_overlap < 0.35 {
        score -= 20;
    }
    if !original_author.is_empty() && author_overlap < 0.2 {
        score -= 25;
    }
    if title_overlap < 0.25 {
        score -= 30;
    }
    score
}

fn passes_spotify_threshold(original: &Value, candidate: &Value, score: i64) -> bool {
    let title_overlap =
        token_overlap_ratio(info_str(original, "title"), info_str(candidate, "title"));
    let author_overlap =
        token_overlap_ratio(info_str(original, "author"), info_str(candidate, "author"));
    let original_duration = duration_sec(original);
    let candidate_duration = duration_sec(candidate);
    let diff = (original_duration - candidate_duration).abs();
    let ratio = if original_duration > 0 {
        diff as f64 / original_duration as f64
    } else {
        1.0
    };
    score >= SPOTIFY_MIRROR_MIN_SCORE
        && title_overlap >= SPOTIFY_MIRROR_MIN_TITLE_OVERLAP
        && (info_str(original, "author").is_empty()
            || author_overlap >= SPOTIFY_MIRROR_MIN_AUTHOR_OVERLAP)
        && original_duration > 0
        && candidate_duration > 0
        && diff <= SPOTIFY_MIRROR_MAX_DURATION_DIFF_SEC
        && ratio <= SPOTIFY_MIRROR_MAX_DURATION_DIFF_RATIO
}

pub fn pick_mirrored_track(tracks: &[Value], original: &Value) -> Option<Value> {
    let spotify_uri = info_str(original, "uri");
    let original_is_spotify = is_spotify_track(original);
    let mut ranked: Vec<(i64, usize)> = tracks
        .iter()
        .enumerate()
        .filter(|(_, c)| !c["info"].is_null() && !is_spotify_track(c))
        .filter(|(_, c)| !is_preview_track(c))
        .filter(|(_, c)| spotify_uri.is_empty() || info_str(c, "uri") != spotify_uri)
        .filter(|(_, c)| {
            !original_is_spotify || is_mirror_candidate_source(info_str(c, "sourceName"))
        })
        .map(|(index, c)| (score_mirrored_candidate(original, c), index))
        .collect();
    // Stable sort, descending (Array.prototype.sort is stable as well).
    ranked.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    let (score, index) = *ranked.first()?;
    let best = &tracks[index];
    if original_is_spotify && !passes_spotify_threshold(original, best, score) {
        return None;
    }
    if !original_is_spotify && score < 55 {
        return None;
    }
    Some(best.clone())
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

    fn has_music(&self) -> bool {
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
        let prefix = if self.paused { "Pauza: " } else { "Gra: " };
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
            default_volume: js_round(env_number("LAVALINK_VOLUME", 80.0)),
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
            .ok_or_else(|| anyhow!("Brak dostepnego node Lavalink."))
    }

    fn usable(&self) -> bool {
        self.node.as_ref().is_some_and(|n| n.connected())
    }

    fn ensure_ready(&self) -> Result<()> {
        if !self.initialized.load(Ordering::SeqCst) || !self.usable() {
            bail!(
                "Lavalink nie jest polaczony. Sprawdz LAVALINK_HOST/LAVALINK_PORT/LAVALINK_PASSWORD i czy Lavalink v4 dziala."
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

    // --- voice gateway -------------------------------------------------------------------------

    fn send_voice(
        app: &App,
        guild_id: Id<GuildMarker>,
        channel: Option<Id<ChannelMarker>>,
    ) -> Result<()> {
        app.sender
            .command(&UpdateVoiceState::new(guild_id, channel, true, false))
            .map_err(|e| anyhow!("Nie udalo sie wyslac voice state: {e}"))
    }

    async fn push_voice(&self, player: &mut Player) {
        let Ok(node) = self.node() else { return };
        let Some(voice) = player.voice_body() else {
            return;
        };
        let mut body = json!({ "voice": voice });
        if player.node_session != node.session_id() {
            // Fresh player on this node session: restore volume, filters and the track.
            body["volume"] = json!(player.volume);
            body["filters"] = player.filters_body();
            if let Some(current) = &player.current {
                body["track"] = lavalink::track_body(current);
                body["position"] = json!(player.position_ms());
                body["paused"] = json!(player.paused);
            }
        }
        match node
            .update_player(&player.guild_id.to_string(), &body, false)
            .await
        {
            Ok(_) => player.node_session = node.session_id(),
            Err(error) => tracing::warn!(
                "Lavalink voice update error guild={}: {error}",
                player.guild_id
            ),
        }
    }

    /// Gateway `VOICE_STATE_UPDATE`: the bot's own state drives reconnect/autoMove; anyone's
    /// join/leave re-checks whether the bot is left alone in its channel.
    pub async fn handle_voice_state(&self, app: &Arc<App>, update: &VoiceStateUpdate) {
        let Some(guild_id) = update.0.guild_id else {
            return;
        };
        if update.0.user_id == app.bot_id {
            self.handle_own_voice_state(app, guild_id, update).await;
        }
        self.check_alone(app, guild_id).await;
    }

    /// Number of non-bot members in `channel` according to the gateway cache. Users missing from
    /// the cache count as listeners, so an incomplete cache never makes the bot leave.
    fn human_listeners(app: &App, channel: Id<ChannelMarker>) -> usize {
        app.cache.voice_channel_states(channel).map_or(0, |states| {
            states
                .filter(|state| {
                    let user = state.user_id();
                    user != app.bot_id && app.cache.user(user).is_none_or(|u| !u.bot)
                })
                .count()
        })
    }

    async fn check_alone(&self, app: &Arc<App>, guild_id: Id<GuildMarker>) {
        if self.alone_disconnect_ms <= 0.0 {
            return;
        }
        let generation = {
            let mut players = self.players.lock().await;
            let Some(player) = players.get_mut(&guild_id) else {
                return;
            };
            let alone = player
                .voice_channel_id
                .is_some_and(|channel| Self::human_listeners(app, channel) == 0);
            // Any change invalidates a pending timer; a new one starts only while alone.
            player.alone_generation += 1;
            if !alone {
                return;
            }
            player.alone_generation
        };
        let delay = Duration::from_millis(self.alone_disconnect_ms as u64);
        let app = app.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let still_alone = app
                .music
                .players
                .lock()
                .await
                .get(&guild_id)
                .is_some_and(|p| {
                    p.alone_generation == generation
                        && p.voice_channel_id
                            .is_some_and(|channel| Self::human_listeners(&app, channel) == 0)
                });
            if still_alone {
                tracing::info!("Music: nikt nie slucha, rozlaczam guild={guild_id}");
                app.music
                    .destroy(&app, guild_id, "alone in voice channel")
                    .await;
            }
        });
    }

    async fn handle_own_voice_state(
        &self,
        app: &App,
        guild_id: Id<GuildMarker>,
        update: &VoiceStateUpdate,
    ) {
        let mut players = self.players.lock().await;
        let Some(player) = players.get_mut(&guild_id) else {
            return;
        };
        player.voice_session = Some(update.0.session_id.clone());
        match update.0.channel_id {
            None => {
                if player.destroying {
                    return;
                }
                if player.reconnecting {
                    player.reconnecting = false;
                    return;
                }
                // onDisconnect.autoReconnect
                if let Some(channel) = player.voice_channel_id
                    && let Err(error) = Self::send_voice(app, guild_id, Some(channel))
                {
                    tracing::warn!("Music auto reconnect failed guild={guild_id}: {error}");
                }
            }
            Some(channel) if Some(channel) != player.voice_channel_id => {
                // autoMove
                player.voice_channel_id = Some(channel);
                drop(players);
                self.refresh_status();
            }
            Some(_) => {}
        }
    }

    pub async fn handle_voice_server(&self, update: &VoiceServerUpdate) {
        let mut players = self.players.lock().await;
        let Some(player) = players.get_mut(&update.guild_id) else {
            return;
        };
        player.voice_token = Some(update.token.clone());
        player.voice_endpoint = update.endpoint.clone();
        self.push_voice(player).await;
    }

    // --- voice channel status ------------------------------------------------------------------

    fn clear_legacy_presence(&self, app: &App) {
        let presence = UpdatePresence {
            d: UpdatePresencePayload {
                activities: Vec::new(),
                afk: false,
                since: None,
                status: Status::Online,
            },
            op: OpCode::PresenceUpdate,
        };
        match app.sender.command(&presence) {
            Ok(()) => self.legacy_presence_cleared.store(true, Ordering::SeqCst),
            Err(error) => tracing::warn!("Music presence clear failed: {error}"),
        }
    }

    async fn set_voice_status(
        app: &App,
        channel: Id<ChannelMarker>,
        status: Option<&str>,
    ) -> Result<()> {
        app.raw
            .put_json(
                &format!("/channels/{channel}/voice-status"),
                &json!({ "status": status.map(|s| truncate_inline(s, MAX_VOICE_CHANNEL_STATUS_LENGTH)) }),
            )
            .await
    }

    fn cached_status(&self, channel: Id<ChannelMarker>) -> Option<String> {
        self.voice_statuses
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&channel)
            .cloned()
    }

    fn store_status(&self, channel: Id<ChannelMarker>, status: Option<String>) {
        let mut statuses = self
            .voice_statuses
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        match status {
            Some(text) => {
                statuses.insert(channel, text);
            }
            None => {
                statuses.remove(&channel);
            }
        }
    }

    async fn sync_voice_statuses(&self, app: &App) {
        let _guard = self.voice_sync.lock().await;
        if !self.legacy_presence_cleared.load(Ordering::SeqCst) {
            self.clear_legacy_presence(app);
        }
        let desired: HashMap<Id<ChannelMarker>, String> = {
            let players = self.players.lock().await;
            players
                .values()
                .filter_map(|p| Some((p.voice_channel_id?, p.status_text()?)))
                .collect()
        };
        let mut channels: Vec<Id<ChannelMarker>> = self
            .voice_statuses
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .copied()
            .collect();
        for channel in desired.keys() {
            if !channels.contains(channel) {
                channels.push(*channel);
            }
        }
        for channel in channels {
            let next = desired.get(&channel).cloned();
            if self.cached_status(channel) == next {
                continue;
            }
            match Self::set_voice_status(app, channel, next.as_deref()).await {
                Ok(()) => self.store_status(channel, next),
                Err(error) => {
                    tracing::warn!("Music voice status update failed channel={channel}: {error}");
                }
            }
        }
    }

    /// `refreshMusicPresence`: serialised background sync of voice channel statuses.
    pub fn refresh_status(&self) {
        let Some(app) = self.app() else { return };
        tokio::spawn(async move {
            app.music.sync_voice_statuses(&app).await;
        });
    }

    async fn clear_voice_status(&self, app: &App, channel: Option<Id<ChannelMarker>>) {
        let Some(channel) = channel else { return };
        let _guard = self.voice_sync.lock().await;
        if !self.legacy_presence_cleared.load(Ordering::SeqCst) {
            self.clear_legacy_presence(app);
        }
        match Self::set_voice_status(app, channel, None).await {
            Ok(()) => self.store_status(channel, None),
            Err(error) => {
                tracing::warn!("Music voice status clear failed channel={channel}: {error}");
            }
        }
    }

    // --- searching -----------------------------------------------------------------------------

    async fn search(
        &self,
        query: &str,
        source: Option<&str>,
        throw_on_empty: bool,
    ) -> Result<SearchResult> {
        lavalink::search(self.node()?, query, source, throw_on_empty).await
    }

    fn mirror_candidates(&self, track: &Value) -> Vec<(String, String)> {
        let isrc = track_isrc(track);
        let seed = track_seed(track);
        let exact = compact_whitespace(&format!(
            "{} {}",
            compact_whitespace(info_str(track, "author")),
            compact_whitespace(info_str(track, "title"))
        ));
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for source in &self.spotify_mirror_sources {
            let source = source.trim().to_lowercase();
            if source.is_empty() {
                continue;
            }
            if source.ends_with("isrc") {
                if !isrc.is_empty() && seen.insert(format!("{source}|isrc|{isrc}")) {
                    out.push((source.clone(), isrc.clone()));
                }
                continue;
            }
            if (source == "dzsearch" || source == "scsearch")
                && !exact.is_empty()
                && seen.insert(format!("{source}|exact|{}", exact.to_lowercase()))
            {
                out.push((source.clone(), exact.clone()));
            }
            if !seed.is_empty() && seen.insert(format!("{source}|seed|{}", seed.to_lowercase())) {
                out.push((source.clone(), seed.clone()));
            }
        }
        out
    }

    async fn resolve_spotify_mirror(&self, track: &Value) -> Option<Value> {
        for (source, query) in self.mirror_candidates(track) {
            match self.search(&query, Some(&source), false).await {
                Ok(result) => {
                    if let Some(mirrored) = pick_mirrored_track(&result.tracks, track) {
                        return Some(mirrored);
                    }
                }
                Err(error) => {
                    tracing::warn!("Spotify mirror lookup failed ({source}:{query}): {error}");
                }
            }
        }
        None
    }

    async fn mirror_spotify_tracks(
        &self,
        tracks: Vec<Value>,
        playlist_mode: bool,
    ) -> Result<Vec<Value>> {
        let mut out = Vec::with_capacity(tracks.len());
        let mut replaced = 0;
        let mut dropped = 0;
        for track in tracks {
            if !is_spotify_track(&track) {
                out.push(track);
                continue;
            }
            if let Some(mirrored) = self.resolve_spotify_mirror(&track).await {
                out.push(mirrored);
                replaced += 1;
                continue;
            }
            if playlist_mode {
                dropped += 1;
                tracing::warn!(
                    "Spotify mirror dropped track (no valid candidate) title=\"{}\" author=\"{}\"",
                    or_unknown(info_str(&track, "title")),
                    or_unknown(info_str(&track, "author"))
                );
                continue;
            }
            bail!(
                "Nie udalo sie zmirrorowac utworu Spotify do zrodla odtwarzania. Sprobuj innego utworu albo ponow za chwile."
            );
        }
        if replaced > 0 {
            tracing::info!("Spotify mirror replaced tracks: {replaced}");
        }
        if dropped > 0 {
            tracing::warn!("Spotify mirror dropped {dropped} track(s) from playlist");
        }
        Ok(out)
    }

    /// `resolveTracks`
    pub async fn resolve_tracks(&self, query: &str) -> Result<Resolved> {
        self.ensure_ready()?;
        let raw = compact_whitespace(query);
        if raw.is_empty() {
            bail!("Podaj link lub fraze do wyszukania.");
        }
        let mut result: Option<SearchResult> = None;
        if !is_http_url(&raw) {
            result = Some(
                self.search(&raw, Some(&self.default_search_source), true)
                    .await?,
            );
        } else {
            let mut last_error = None;
            for (candidate, source) in youtube_resolve_candidates(&raw) {
                let source = source.unwrap_or_else(|| self.default_search_source.clone());
                match self.search(&candidate, Some(&source), true).await {
                    Ok(found) if !found.tracks.is_empty() => {
                        result = Some(found);
                        break;
                    }
                    Ok(_) => {}
                    Err(error) => last_error = Some(error),
                }
            }
            if result.is_none()
                && let Some(error) = last_error
            {
                return Err(error);
            }
        }
        let Some(result) = result.filter(|r| !r.tracks.is_empty()) else {
            bail!("Nie znaleziono utworu.");
        };
        let playlist = result.load_type == "playlist";
        let prepared = self
            .mirror_spotify_tracks(result.tracks.clone(), playlist)
            .await?;
        if prepared.is_empty() {
            bail!(if playlist {
                "Nie udalo sie zmirrorowac utworow z tej playlisty Spotify do odtwarzalnych zrodel."
            } else {
                "Nie znaleziono odtwarzalnego wariantu utworu."
            });
        }
        Ok(Resolved {
            playlist,
            playlist_title: playlist.then(|| playlist_title(&result)),
            tracks: prepared.iter().map(normalize_track).collect(),
        })
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
                bail!("Bot odtwarza juz muzyke na kanale <#{current_channel}>.");
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
            bail!("Sesja muzyki wygasla.");
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
            .ok_or_else(|| anyhow!("Sesja muzyki wygasla."))?;
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
            bail!("Nie znaleziono utworow do dodania.");
        }
        let mut players = self.players.lock().await;
        let player = players
            .get_mut(&session.guild_id)
            .ok_or_else(|| anyhow!("Sesja muzyki wygasla."))?;
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

    // --- controls ------------------------------------------------------------------------------

    /// Mutates an active player (`getPlayerForGuild`), sends the returned PATCH body and rolls the
    /// local state back when Lavalink rejects it. Returns the fresh snapshot.
    async fn with_active(
        &self,
        guild_id: Id<GuildMarker>,
        mutate: impl FnOnce(&mut Player) -> Option<Value>,
    ) -> Result<Option<Snapshot>> {
        let mut players = self.players.lock().await;
        let Some(player) = players.get_mut(&guild_id).filter(|p| p.has_music()) else {
            return Ok(None);
        };
        let backup = player.clone();
        if let Some(body) = mutate(player) {
            let node = self.node()?;
            if let Err(error) = node
                .update_player(&guild_id.to_string(), &body, false)
                .await
            {
                *player = backup;
                return Err(error);
            }
        }
        Ok(snapshot_of(player))
    }

    /// `pause` / `resume` / `togglePause` (`None` toggles).
    pub async fn set_paused(
        &self,
        guild_id: Id<GuildMarker>,
        paused: Option<bool>,
    ) -> Result<Option<Snapshot>> {
        let has_current = self
            .players
            .lock()
            .await
            .get(&guild_id)
            .is_some_and(|p| p.has_music() && p.current.is_some());
        if !has_current {
            return Ok(None);
        }
        let result = self
            .with_active(guild_id, |player| {
                let next = paused.unwrap_or(!player.paused);
                if next == player.paused {
                    return None;
                }
                if next {
                    player.last_position = player.position_ms();
                } else {
                    player.last_position_at = Some(Instant::now());
                }
                player.paused = next;
                Some(json!({ "paused": next }))
            })
            .await;
        self.refresh_status();
        result
    }

    pub async fn adjust_volume(
        &self,
        guild_id: Id<GuildMarker>,
        delta: i64,
    ) -> Result<Option<Snapshot>> {
        let default = self.default_volume;
        self.with_active(guild_id, |player| {
            let current = if player.volume != 0 {
                player.volume
            } else {
                default
            };
            if current > 0 {
                player.previous_volume = Some(current);
            }
            player.volume = (current + delta).clamp(0, 150);
            Some(json!({ "volume": player.volume }))
        })
        .await
    }

    pub async fn toggle_mute(&self, guild_id: Id<GuildMarker>) -> Result<Option<Snapshot>> {
        let default = self.default_volume;
        self.with_active(guild_id, |player| {
            if player.volume > 0 {
                player.previous_volume = Some(player.volume);
                player.volume = 0;
            } else {
                let fallback = if default != 0 { default } else { 80 };
                player.volume = player
                    .previous_volume
                    .filter(|v| *v != 0)
                    .unwrap_or(fallback)
                    .clamp(1, 150);
            }
            Some(json!({ "volume": player.volume }))
        })
        .await
    }

    pub async fn cycle_loop(&self, guild_id: Id<GuildMarker>) -> Result<Option<Snapshot>> {
        self.with_active(guild_id, |player| {
            player.repeat = match player.repeat {
                Repeat::Off => Repeat::Track,
                Repeat::Track => Repeat::Queue,
                Repeat::Queue => Repeat::Off,
            };
            None
        })
        .await
    }

    pub async fn toggle_autoplay(&self, guild_id: Id<GuildMarker>) -> bool {
        let mut players = self.players.lock().await;
        players.get_mut(&guild_id).is_some_and(|player| {
            player.autoplay = !player.autoplay;
            player.autoplay
        })
    }

    pub async fn toggle_bassboost(&self, guild_id: Id<GuildMarker>) -> Result<Option<Snapshot>> {
        let active = self
            .players
            .lock()
            .await
            .get(&guild_id)
            .is_some_and(|p| p.flags.bassboost);
        self.apply_eq_preset(guild_id, if active { "flat" } else { "bassboost" })
            .await
    }

    pub async fn apply_eq_preset(
        &self,
        guild_id: Id<GuildMarker>,
        preset: &str,
    ) -> Result<Option<Snapshot>> {
        let preset = preset.to_lowercase();
        self.with_active(guild_id, |player| {
            let mut changed = false;
            if preset == "nightcore" {
                if !player.flags.nightcore {
                    player.timescale = Timescale::Nightcore;
                    changed = true;
                }
                player.flags.nightcore = true;
                player.flags.vaporwave = false;
                player.flags.eq_preset = "nightcore".to_owned();
                player.flags.bassboost = false;
                return changed.then(|| json!({ "filters": player.filters_body() }));
            }
            if player.flags.nightcore {
                player.timescale = if player.timescale == Timescale::Nightcore {
                    Timescale::Normal
                } else {
                    Timescale::Nightcore
                };
                player.flags.nightcore = false;
                changed = true;
            }
            match preset.as_str() {
                "flat" => {
                    player.set_eq(&[0.0; 15]);
                    player.flags.bassboost = false;
                    player.flags.eq_preset = "flat".to_owned();
                    changed = true;
                }
                "bassboost" => {
                    let bands: Vec<f64> = BASSBOOST_MEDIUM
                        .iter()
                        .map(|g| g * BASSBOOST_MULTIPLIER)
                        .collect();
                    player.set_eq(&bands);
                    player.flags.bassboost = true;
                    player.flags.eq_preset = "bassboost".to_owned();
                    changed = true;
                }
                "vocal" | "treble" => {
                    player.set_eq(if preset == "vocal" {
                        &VOCAL_EQ
                    } else {
                        &TREBLE_EQ
                    });
                    player.flags.bassboost = false;
                    player.flags.eq_preset.clone_from(&preset);
                    changed = true;
                }
                _ => {}
            }
            changed.then(|| json!({ "filters": player.filters_body() }))
        })
        .await
    }

    pub async fn toggle_fx(&self, guild_id: Id<GuildMarker>, fx: &str) -> Result<Option<Snapshot>> {
        let fx = fx.to_lowercase();
        self.with_active(guild_id, |player| {
            fn toggle_vaporwave(player: &mut Player) {
                player.timescale = if player.timescale == Timescale::Vaporwave {
                    Timescale::Normal
                } else {
                    Timescale::Vaporwave
                };
            }
            match fx.as_str() {
                "vaporwave" => {
                    toggle_vaporwave(player);
                    player.flags.vaporwave = !player.flags.vaporwave;
                    if player.flags.vaporwave {
                        player.flags.nightcore = false;
                    }
                }
                "8d" | "eightd" => {
                    player.rotation = !player.rotation;
                    player.flags.eight_d = !player.flags.eight_d;
                }
                "karaoke" => {
                    player.karaoke = !player.karaoke;
                    player.flags.karaoke = !player.flags.karaoke;
                }
                "softlimiter" => {
                    player.low_pass = !player.low_pass;
                    player.flags.soft_limiter = !player.flags.soft_limiter;
                }
                "off" => {
                    if player.flags.vaporwave {
                        toggle_vaporwave(player);
                    }
                    if player.flags.eight_d {
                        player.rotation = !player.rotation;
                    }
                    if player.flags.karaoke {
                        player.karaoke = !player.karaoke;
                    }
                    if player.flags.soft_limiter {
                        player.low_pass = !player.low_pass;
                    }
                    player.flags.vaporwave = false;
                    player.flags.eight_d = false;
                    player.flags.karaoke = false;
                    player.flags.soft_limiter = false;
                }
                _ => return None,
            }
            Some(json!({ "filters": player.filters_body() }))
        })
        .await
    }

    /// Removes the track at 1-based `position` after the current one.
    pub async fn remove_from_queue(
        &self,
        guild_id: Id<GuildMarker>,
        position: i64,
    ) -> Result<Option<PublicTrack>> {
        if position <= 0 {
            bail!("Pozycja w kolejce musi byc liczba >= 1.");
        }
        let mut players = self.players.lock().await;
        let Some(player) = players.get_mut(&guild_id).filter(|p| p.has_music()) else {
            return Ok(None);
        };
        Ok(player
            .tracks
            .remove((position - 1) as usize)
            .map(|t| normalize_track(&t)))
    }

    pub async fn clear_queue(&self, guild_id: Id<GuildMarker>) -> Option<usize> {
        let mut players = self.players.lock().await;
        let player = players.get_mut(&guild_id).filter(|p| p.has_music())?;
        let removed = player.tracks.len();
        player.tracks.clear();
        Some(removed)
    }

    pub async fn shuffle(&self, guild_id: Id<GuildMarker>) -> Option<Snapshot> {
        let mut players = self.players.lock().await;
        let player = players.get_mut(&guild_id).filter(|p| p.has_music())?;
        if player.tracks.len() > 1 {
            player.tracks.make_contiguous().shuffle(&mut rand::rng());
        }
        snapshot_of(player)
    }

    pub async fn skip(&self, guild_id: Id<GuildMarker>) -> Result<Option<PublicTrack>> {
        let mut players = self.players.lock().await;
        let Some(player) = players.get_mut(&guild_id) else {
            return Ok(None);
        };
        let Some(current) = player.current.as_ref().map(normalize_track) else {
            return Ok(None);
        };
        player.skipped = true;
        let node = self.node()?;
        let body = json!({ "track": { "encoded": null }, "paused": false });
        if let Err(error) = node
            .update_player(&guild_id.to_string(), &body, false)
            .await
        {
            tracing::warn!("Music skip error guild={guild_id}: {error}");
        }
        player.paused = false;
        drop(players);
        self.refresh_status();
        Ok(Some(current))
    }

    /// `stop`: destroys the session and clears every per-guild state.
    pub async fn stop(&self, app: &App, guild_id: Id<GuildMarker>) -> bool {
        let channel = self
            .players
            .lock()
            .await
            .get(&guild_id)
            .map(|p| p.voice_channel_id);
        let Some(channel) = channel else {
            self.refresh_status();
            return false;
        };
        self.clear_voice_status(app, channel).await;
        self.destroy(app, guild_id, "music stop").await;
        true
    }

    async fn destroy(&self, app: &App, guild_id: Id<GuildMarker>, reason: &str) {
        let Some(mut player) = self.players.lock().await.remove(&guild_id) else {
            return;
        };
        player.destroying = true;
        if let Err(error) = Self::send_voice(app, guild_id, None) {
            tracing::warn!("Music disconnect error guild={guild_id}: {error}");
        }
        if let Ok(node) = self.node()
            && let Err(error) = node.destroy_player(&guild_id.to_string()).await
        {
            tracing::warn!("Music destroy error guild={guild_id} reason=\"{reason}\": {error}");
        }
        self.refresh_status();
    }

    pub async fn reconnect_session(
        &self,
        app: &App,
        guild_id: Id<GuildMarker>,
    ) -> Result<Option<ReconnectResult>> {
        let channel = {
            let players = self.players.lock().await;
            let Some(player) = players.get(&guild_id).filter(|p| p.has_music() || p.paused) else {
                return Ok(None);
            };
            player.voice_channel_id
        };
        let channel =
            channel.ok_or_else(|| anyhow!("Brak zapisanego kanalu glosowego dla tej sesji."))?;
        let node = self.node()?;
        if !node.connected() {
            node.reconnect_now();
            let started = Instant::now();
            while !node.connected() && started.elapsed() < Duration::from_secs(3) {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            if !node.connected() {
                bail!("Lavalink node nadal jest offline po probie reconnect.");
            }
        }
        if let Some(player) = self.players.lock().await.get_mut(&guild_id) {
            player.reconnecting = true;
            player.idle_generation += 1;
        }
        let _ = Self::send_voice(app, guild_id, None);
        Self::send_voice(app, guild_id, Some(channel))?;
        self.refresh_status();
        Ok(Some(ReconnectResult {
            voice_channel_id: channel,
            node_connected: node.connected(),
        }))
    }

    // --- saved queues --------------------------------------------------------------------------

    fn queue_state_path(&self, guild_id: Id<GuildMarker>) -> PathBuf {
        self.queue_state_dir.join(format!("{guild_id}.json"))
    }

    /// Returns the number of saved tracks.
    pub async fn save_queue_state(&self, guild_id: Id<GuildMarker>) -> Result<usize> {
        let snapshot = self.snapshot(guild_id).await;
        let Some(snapshot) = snapshot.filter(|s| s.current_track.is_some()) else {
            bail!("Brak aktywnej kolejki do zapisania.");
        };
        let tracks: Vec<Value> = snapshot
            .current_track
            .iter()
            .chain(snapshot.queue.iter())
            .filter(|t| !t.url.is_empty())
            .map(|t| json!({ "url": t.url, "title": t.title }))
            .collect();
        if tracks.is_empty() {
            bail!("Brak utworow z linkiem do zapisania.");
        }
        let count = tracks.len();
        let payload = json!({
            "guildId": snapshot.guild_id.to_string(),
            "voiceChannelId": snapshot.voice_channel_id.map(|c| c.to_string()).unwrap_or_default(),
            "savedAt": chrono::Utc::now().timestamp_millis(),
            "volume": if snapshot.volume != 0 { snapshot.volume } else { self.default_volume },
            "repeatMode": snapshot.repeat_mode.as_str(),
            "autoplay": snapshot.autoplay,
            "tracks": tracks,
        });
        // JSON.stringify(payload, null, 2): two-space indent, no trailing newline.
        let bytes = serde_json::to_vec_pretty(&payload)?;
        std::fs::create_dir_all(&self.queue_state_dir)?;
        operator_common::fs::write_atomic(&self.queue_state_path(guild_id), &bytes)?;
        Ok(count)
    }

    fn load_saved_queue_state(&self, guild_id: Id<GuildMarker>) -> Result<Value> {
        let path = self.queue_state_path(guild_id);
        if !path.exists() {
            bail!("Brak zapisanego stanu kolejki.");
        }
        let mut parsed: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
        let tracks: Vec<Value> = parsed["tracks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|e| e["url"].as_str().is_some_and(|u| !u.trim().is_empty()))
            .cloned()
            .collect();
        if tracks.is_empty() {
            bail!("Zapisana kolejka jest pusta.");
        }
        parsed["tracks"] = Value::Array(tracks);
        Ok(parsed)
    }

    /// Returns the number of restored tracks.
    pub async fn restore_queue_state(
        &self,
        app: &App,
        guild_id: Id<GuildMarker>,
        voice_channel_id: Id<ChannelMarker>,
    ) -> Result<usize> {
        let state = self.load_saved_queue_state(guild_id)?;
        let session = self.ensure_session(app, guild_id, voice_channel_id).await?;
        let mut restored = Vec::new();
        for entry in state["tracks"].as_array().into_iter().flatten() {
            let url = entry["url"].as_str().unwrap_or_default();
            match self.resolve_tracks(url).await {
                Ok(resolved) => restored.extend(resolved.tracks.into_iter().next()),
                Err(error) => tracing::warn!(
                    "Restore queue skip guild={guild_id} url=\"{url}\" error={error}"
                ),
            }
        }
        if restored.is_empty() {
            bail!("Nie udalo sie odtworzyc zadnego utworu z zapisu.");
        }
        self.enqueue_many(&session, &restored).await?;
        let volume = state["volume"]
            .as_f64()
            .filter(|v| *v != 0.0)
            .map_or(self.default_volume, js_round)
            .clamp(1, 150);
        let mut players = self.players.lock().await;
        if let Some(player) = players.get_mut(&guild_id) {
            self.node()?
                .update_player(&guild_id.to_string(), &json!({ "volume": volume }), false)
                .await?;
            player.volume = volume;
            player.repeat = Repeat::parse(state["repeatMode"].as_str().unwrap_or_default());
            player.autoplay = state["autoplay"] == Value::Bool(true);
        }
        Ok(restored.len())
    }

    pub async fn shutdown(&self, app: &App) {
        let guilds: Vec<Id<GuildMarker>> = self.players.lock().await.keys().copied().collect();
        for guild_id in guilds {
            self.destroy(app, guild_id, "bot shutdown").await;
        }
        if let Some(node) = &self.node {
            node.shutdown();
        }
    }

    // --- node events ---------------------------------------------------------------------------

    fn node_id(&self) -> &str {
        self.node
            .as_ref()
            .map_or("unknown", |n| n.config.id.as_str())
    }

    async fn on_node_event(&self, app: &Arc<App>, event: NodeEvent) {
        match event {
            NodeEvent::Ready {
                session_id,
                resumed,
            } => {
                tracing::info!("Music provider ready: Lavalink node {}", self.node_id());
                tracing::debug!("Lavalink session={session_id} resumed={resumed}");
                self.refresh_status();
            }
            NodeEvent::Disconnected { reason } => {
                tracing::warn!(
                    "Lavalink node disconnected ({}): {}",
                    self.node_id(),
                    if reason.is_empty() {
                        "unknown"
                    } else {
                        &reason
                    }
                );
                for player in self.players.lock().await.values_mut() {
                    player.connected = false;
                }
            }
            NodeEvent::PlayerUpdate { guild_id, state } => {
                let Some(guild_id) = crate::discord::parse_id::<GuildMarker>(&guild_id) else {
                    return;
                };
                if let Some(player) = self.players.lock().await.get_mut(&guild_id) {
                    player.last_position =
                        state["position"].as_i64().unwrap_or(player.last_position);
                    player.last_position_at = Some(Instant::now());
                    player.connected = state["connected"].as_bool().unwrap_or(false);
                    player.ping = state["ping"].as_i64().filter(|p| *p >= 0);
                }
            }
            NodeEvent::Event { guild_id, payload } => {
                let Some(guild_id) = crate::discord::parse_id::<GuildMarker>(&guild_id) else {
                    return;
                };
                self.on_track_event(app, guild_id, payload).await;
            }
        }
    }

    async fn on_track_event(&self, app: &Arc<App>, guild_id: Id<GuildMarker>, payload: Value) {
        let track = payload["track"].clone();
        let title = or_unknown(info_str(&track, "title")).to_owned();
        match payload["type"].as_str().unwrap_or_default() {
            "TrackStartEvent" => {
                if let Some(player) = self.players.lock().await.get_mut(&guild_id) {
                    player.idle_generation += 1;
                    player.recovery.remove(&recovery_key(&track));
                    player.playing = true;
                }
                tracing::info!("Music track start guild={guild_id} title=\"{title}\"");
                self.refresh_status();
            }
            "TrackEndEvent" => {
                let reason = payload["reason"].as_str().unwrap_or_default();
                self.on_track_end(app, guild_id, track, reason).await;
            }
            "TrackExceptionEvent" => {
                let message = payload["exception"]["message"]
                    .as_str()
                    .unwrap_or("unknown");
                tracing::warn!("Music track error guild={guild_id} title=\"{title}\": {message}");
                let too_many = self
                    .players
                    .lock()
                    .await
                    .get_mut(&guild_id)
                    .is_some_and(|player| {
                        let window = Duration::from_millis(self.error_threshold_ms.max(0.0) as u64);
                        player.errors.retain(|at| at.elapsed() < window);
                        player.errors.push_back(Instant::now());
                        self.error_max_amount > 0.0
                            && player.errors.len() as f64 >= self.error_max_amount
                    });
                if too_many {
                    self.destroy(app, guild_id, "TrackErrorMaxTracksErroredPerTime")
                        .await;
                    return;
                }
                let app = app.clone();
                tokio::spawn(async move {
                    if let Err(error) = app.music.recover_track(guild_id, &track, &payload).await {
                        tracing::warn!("Music recovery error guild={guild_id}: {error}");
                    }
                });
            }
            "TrackStuckEvent" => {
                tracing::warn!("Music track stuck guild={guild_id} title=\"{title}\"");
                let mut players = self.players.lock().await;
                let Some(player) = players.get_mut(&guild_id) else {
                    return;
                };
                let Ok(node) = self.node() else { return };
                if player.tracks.is_empty() && player.repeat == Repeat::Off {
                    let body = json!({ "track": { "encoded": null } });
                    if node
                        .update_player(&guild_id.to_string(), &body, false)
                        .await
                        .is_ok()
                    {
                        return;
                    }
                    drop(players);
                    self.on_queue_end(app, guild_id, Some(track)).await;
                    return;
                }
                player.queue_track_end();
                if player.current.is_none() {
                    drop(players);
                    self.on_queue_end(app, guild_id, Some(track)).await;
                } else if let Err(error) = self.play_current(player, false).await {
                    tracing::warn!("Music play after stuck failed guild={guild_id}: {error}");
                }
            }
            "WebSocketClosedEvent" => {
                tracing::warn!(
                    "Lavalink voice websocket closed guild={guild_id} code={} reason=\"{}\"",
                    payload["code"],
                    payload["reason"].as_str().unwrap_or_default()
                );
            }
            _ => {}
        }
    }

    async fn on_track_end(
        &self,
        app: &Arc<App>,
        guild_id: Id<GuildMarker>,
        track: Value,
        reason: &str,
    ) {
        if reason == "replaced" {
            return;
        }
        let mut players = self.players.lock().await;
        let Some(player) = players.get_mut(&guild_id) else {
            return;
        };
        if player.destroying {
            return;
        }
        if reason == "loadFailed"
            || reason == "cleanup"
            || player.repeat != Repeat::Track
            || player.skipped
        {
            player.queue_track_end();
        }
        player.skipped = false;
        if player.current.is_none() {
            drop(players);
            self.on_queue_end(app, guild_id, Some(track)).await;
            return;
        }
        if let Err(error) = self.play_current(player, true).await {
            tracing::warn!("Music autoskip play failed guild={guild_id}: {error}");
        }
    }

    /// `queueEnd` of lavalink-client + the bot's autoplay function and idle timer.
    async fn on_queue_end(
        &self,
        app: &Arc<App>,
        guild_id: Id<GuildMarker>,
        last_track: Option<Value>,
    ) {
        let autoplay = {
            let mut players = self.players.lock().await;
            let Some(player) = players.get_mut(&guild_id) else {
                return;
            };
            player.current = None;
            player.playing = false;
            player.autoplay
        };
        if autoplay && let Some(track) = &last_track {
            self.handle_autoplay(guild_id, track).await;
            let mut players = self.players.lock().await;
            if let Some(player) = players.get_mut(&guild_id)
                && player.current.is_none()
                && !player.tracks.is_empty()
            {
                player.queue_track_end();
                match self.play_current(player, true).await {
                    Ok(()) => return,
                    Err(error) => tracing::warn!("Autoplay play failed guild={guild_id}: {error}"),
                }
            }
        }
        if let Some(track) = last_track
            && let Some(player) = self.players.lock().await.get_mut(&guild_id)
        {
            player.previous.push_front(track);
            player.previous.truncate(MAX_PREVIOUS_TRACKS);
        }
        self.schedule_idle_destroy(app, guild_id).await;
        self.refresh_status();
    }

    async fn handle_autoplay(&self, guild_id: Id<GuildMarker>, track: &Value) {
        let seed = compact_whitespace(&format!(
            "{} {}",
            info_str(track, "author"),
            info_str(track, "title")
        ));
        if seed.is_empty() {
            return;
        }
        let result = match self
            .search(&seed, Some(&self.default_search_source), false)
            .await
        {
            Ok(result) => result,
            Err(error) => {
                tracing::warn!("Autoplay error guild={guild_id}: {error}");
                return;
            }
        };
        let current_uri = info_str(track, "uri");
        let Some(next) = result
            .tracks
            .iter()
            .find(|c| info_str(c, "uri") != current_uri)
            .or_else(|| result.tracks.first())
            .cloned()
        else {
            return;
        };
        let title = or_unknown(info_str(&next, "title")).to_owned();
        if let Some(player) = self.players.lock().await.get_mut(&guild_id) {
            player.tracks.push_back(next);
            tracing::info!("Autoplay dodal utwor guild={guild_id} title=\"{title}\"");
        }
    }

    async fn schedule_idle_destroy(&self, app: &Arc<App>, guild_id: Id<GuildMarker>) {
        let generation = {
            let mut players = self.players.lock().await;
            let Some(player) = players.get_mut(&guild_id) else {
                return;
            };
            player.idle_generation += 1;
            player.idle_generation
        };
        let delay = Duration::from_millis(self.idle_disconnect_ms.max(0.0) as u64);
        let app = app.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let idle = app
                .music
                .players
                .lock()
                .await
                .get(&guild_id)
                .is_some_and(|p| {
                    p.idle_generation == generation
                        && !p.playing
                        && p.current.is_none()
                        && p.tracks.is_empty()
                });
            if idle {
                app.music
                    .destroy(&app, guild_id, "empty queue idle timeout")
                    .await;
            }
        });
    }

    /// `handleTrackErrorRecovery`
    async fn recover_track(
        &self,
        guild_id: Id<GuildMarker>,
        track: &Value,
        payload: &Value,
    ) -> Result<()> {
        let text = format!(
            "{} {}",
            payload["exception"]["message"].as_str().unwrap_or_default(),
            payload["exception"]["cause"].as_str().unwrap_or_default()
        );
        if !recoverable_error_regex().is_match(&text)
            || !info_str(track, "sourceName").eq_ignore_ascii_case("spotify")
        {
            return Ok(());
        }
        {
            let mut players = self.players.lock().await;
            let Some(player) = players.get_mut(&guild_id) else {
                return Ok(());
            };
            let key = recovery_key(track);
            if player
                .recovery
                .get(&key)
                .is_some_and(|at| at.elapsed() < TRACK_RECOVERY_COOLDOWN)
            {
                return Ok(());
            }
            player.recovery.insert(key, Instant::now());
        }
        let seed = track_seed(track);
        let isrc = track_isrc(track);
        if seed.is_empty() {
            return Ok(());
        }
        let title = compact_whitespace(info_str(track, "title"));
        let author = compact_whitespace(info_str(track, "author"));
        for source in &self.recovery_sources {
            let isrc_source = source.to_lowercase().ends_with("isrc");
            let mut queries = Vec::new();
            if isrc_source {
                if !isrc.is_empty() {
                    queries.push(isrc.clone());
                }
            } else if source == "scsearch" {
                if !title.is_empty() && !author.is_empty() {
                    queries.push(format!("\"{author}\" \"{title}\""));
                }
            } else if source == "dzsearch" && !title.is_empty() && !author.is_empty() {
                queries.push(format!("{author} {title}"));
            }
            if !isrc_source {
                queries.push(seed.clone());
            }
            for query in queries.into_iter().filter(|q| !q.is_empty()) {
                let result = match self.search(&query, Some(source), false).await {
                    Ok(result) => result,
                    Err(error) => {
                        tracing::warn!(
                            "Music recovery lookup failed guild={guild_id} source={source} query=\"{query}\": {error}"
                        );
                        continue;
                    }
                };
                let Some(candidate) = pick_mirrored_track(&result.tracks, track) else {
                    continue;
                };
                let candidate_title = or_unknown(info_str(&candidate, "title")).to_owned();
                let mut players = self.players.lock().await;
                let Some(player) = players.get_mut(&guild_id) else {
                    return Ok(());
                };
                player.tracks.push_front(candidate);
                if !player.playing && player.current.is_none() {
                    self.play_current(player, false).await?;
                }
                tracing::warn!(
                    "Music recovery queued fallback guild={guild_id} source={source} scoreQuery=\"{query}\" title=\"{candidate_title}\""
                );
                return Ok(());
            }
        }
        Ok(())
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
