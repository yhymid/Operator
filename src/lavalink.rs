//! Small Lavalink v4 client: one node, WebSocket event stream with automatic reconnects and the
//! REST endpoints the music module needs (`loadtracks`, `sessions/{id}/players/{guild}`, `info`).
//!
//! Replaces `lavalink-client` from the JS version; queue, repeat, autoplay and filter logic live
//! in `modules::music` and mirror that library's behaviour.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use http::{HeaderName, HeaderValue, Method};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_websockets::{ClientBuilder, Message};

use crate::common::env;
use crate::rest::JsonClient;

pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 2333;
pub const DEFAULT_PASSWORD: &str = "youshallnotpass";
const RECONNECT_DELAY: Duration = Duration::from_secs(10);
/// How long Lavalink keeps the players (and buffers their events) after the WebSocket drops,
/// so a short network break or a reconnect does not stop the music.
const RESUME_TIMEOUT_SECS: u64 = 60;

#[derive(Debug, Clone)]
pub struct NodeConfig {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub authorization: String,
    pub secure: bool,
}

/// `buildLavalinkNodesFromEnv`
pub fn node_config_from_env() -> Result<NodeConfig> {
    if let Some(raw) = env::var("LAVALINK_URL") {
        let parsed =
            url::Url::parse(&raw).with_context(|| format!("Invalid LAVALINK_URL: {raw}"))?;
        let secure = matches!(parsed.scheme(), "https" | "wss");
        let decode = |s: &str| percent_decode(s);
        return Ok(NodeConfig {
            id: Some(parsed.username())
                .filter(|u| !u.is_empty())
                .map_or_else(|| "operator-main".to_owned(), decode),
            host: parsed.host_str().unwrap_or(DEFAULT_HOST).to_owned(),
            port: parsed
                .port()
                .unwrap_or(if secure { 443 } else { DEFAULT_PORT }),
            authorization: parsed
                .password()
                .map_or_else(|| DEFAULT_PASSWORD.to_owned(), decode),
            secure,
        });
    }
    Ok(NodeConfig {
        id: env::var("LAVALINK_NODE_ID").unwrap_or_else(|| "operator-main".to_owned()),
        host: env::var("LAVALINK_HOST").unwrap_or_else(|| DEFAULT_HOST.to_owned()),
        port: Some(env::parse::<f64>("LAVALINK_PORT", f64::from(DEFAULT_PORT)))
            .filter(|p| p.is_finite() && *p > 0.0 && *p <= f64::from(u16::MAX))
            .map_or(DEFAULT_PORT, |p| p as u16),
        authorization: env::var("LAVALINK_PASSWORD").unwrap_or_else(|| DEFAULT_PASSWORD.to_owned()),
        secure: env::bool("LAVALINK_SECURE", false),
    })
}

/// `decodeURIComponent` for URL user-info parts.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let Some(byte) = std::str::from_utf8(&bytes[index + 1..index + 3])
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(byte);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Messages from the node WebSocket, forwarded to the music manager.
#[derive(Debug)]
pub enum NodeEvent {
    Ready { session_id: String, resumed: bool },
    Disconnected { reason: String },
    PlayerUpdate { guild_id: String, state: Value },
    Event { guild_id: String, payload: Value },
}

pub struct Node {
    pub config: NodeConfig,
    rest: JsonClient,
    session_id: RwLock<Option<String>>,
    connected: AtomicBool,
    stats: Mutex<Value>,
    shutdown: AtomicBool,
    wake: tokio::sync::Notify,
    /// Guild id → when a new track was sent to the player (diagnostics: time to TrackStart).
    pending_starts: Mutex<std::collections::HashMap<String, std::time::Instant>>,
    /// Guild id → when and why the last track ended (diagnostics: silence until the next start).
    track_ends: Mutex<std::collections::HashMap<String, (std::time::Instant, String)>>,
    /// Recent track starts/ends on any player (wall clock, last few minutes): a frame-stats
    /// minute that saw one is a track change.
    track_changes: Mutex<std::collections::VecDeque<std::time::SystemTime>>,
}

impl Node {
    pub fn new(config: NodeConfig) -> Result<Self> {
        let scheme = if config.secure { "https" } else { "http" };
        let rest = JsonClient::new(
            &format!("{scheme}://{}:{}/v4", config.host, config.port),
            &config.authorization,
            !config.secure,
        )?;
        Ok(Self {
            config,
            rest,
            session_id: RwLock::new(None),
            connected: AtomicBool::new(false),
            stats: Mutex::new(Value::Null),
            shutdown: AtomicBool::new(false),
            wake: tokio::sync::Notify::new(),
            pending_starts: Mutex::new(std::collections::HashMap::new()),
            track_ends: Mutex::new(std::collections::HashMap::new()),
            track_changes: Mutex::new(std::collections::VecDeque::new()),
        })
    }

    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    pub fn session_id(&self) -> Option<String> {
        self.session_id
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn stats(&self) -> Value {
        self.stats.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        self.wake.notify_one();
    }

    /// Skips the remaining reconnect delay (`node.reconnect(true)`).
    pub fn reconnect_now(&self) {
        self.wake.notify_one();
    }

    fn ws_url(&self) -> String {
        let scheme = if self.config.secure { "wss" } else { "ws" };
        format!(
            "{scheme}://{}:{}/v4/websocket",
            self.config.host, self.config.port
        )
    }

    /// Runs the WebSocket loop forever (reconnecting every 10 s) and forwards events.
    pub async fn run(
        self: std::sync::Arc<Self>,
        user_id: u64,
        events: mpsc::UnboundedSender<NodeEvent>,
    ) {
        let client_name = format!("Operator/{}", env!("CARGO_PKG_VERSION"));
        loop {
            if self.shutdown.load(Ordering::SeqCst) {
                return;
            }
            match self.connect_once(user_id, &client_name, &events).await {
                Ok(reason) => {
                    self.mark_disconnected(&events, reason);
                }
                Err(error) => {
                    tracing::error!(
                        "Lavalink node error ({}): {}",
                        self.config.id,
                        crate::discord::err_text(&error)
                    );
                    self.mark_disconnected(&events, error.to_string());
                }
            }
            if self.shutdown.load(Ordering::SeqCst) {
                return;
            }
            tokio::select! {
                () = tokio::time::sleep(RECONNECT_DELAY) => {}
                () = self.wake.notified() => {}
            }
        }
    }

    fn mark_disconnected(&self, events: &mpsc::UnboundedSender<NodeEvent>, reason: String) {
        // The session id is kept: the next connect offers it to Lavalink to resume the session.
        let was = self.connected.swap(false, Ordering::SeqCst);
        if was {
            let _ = events.send(NodeEvent::Disconnected { reason });
        }
    }

    async fn connect_once(
        &self,
        user_id: u64,
        client_name: &str,
        events: &mpsc::UnboundedSender<NodeEvent>,
    ) -> Result<String> {
        let header = |name: &'static str, value: &str| -> Result<(HeaderName, HeaderValue)> {
            Ok((HeaderName::from_static(name), HeaderValue::from_str(value)?))
        };
        let mut builder = ClientBuilder::new().uri(&self.ws_url())?;
        let mut headers = vec![
            header("authorization", &self.config.authorization)?,
            header("user-id", &user_id.to_string())?,
            header("client-name", client_name)?,
        ];
        if let Some(previous) = self.session_id() {
            // Lavalink answers `resumed: true` when that session is still waiting for us.
            headers.push(header("session-id", &previous)?);
        }
        for (name, value) in headers {
            builder = builder.add_header(name, value)?;
        }
        let connect_started = std::time::Instant::now();
        let (mut stream, _) = builder.connect().await.context("websocket connect")?;

        while let Some(message) = stream.next().await {
            if self.shutdown.load(Ordering::SeqCst) {
                let _ = stream.send(Message::close(None, "bot shutdown")).await;
                return Ok("bot shutdown".to_owned());
            }
            let message = message?;
            if message.is_close() {
                let reason = message
                    .as_close()
                    .map(|(code, reason)| format!("{} {reason}", u16::from(code)))
                    .unwrap_or_default();
                return Ok(reason);
            }
            let Some(text) = message.as_text() else {
                continue;
            };
            let Ok(payload) = serde_json::from_str::<Value>(text) else {
                continue;
            };
            match payload["op"].as_str().unwrap_or_default() {
                "ready" => {
                    let session_id = payload["sessionId"].as_str().unwrap_or_default().to_owned();
                    *self.session_id.write().unwrap_or_else(|p| p.into_inner()) =
                        Some(session_id.clone());
                    self.connected.store(true, Ordering::SeqCst);
                    if let Err(error) = self.enable_resuming(&session_id).await {
                        tracing::warn!("Lavalink session resuming could not be enabled: {error}");
                    }
                    let resumed = payload["resumed"].as_bool().unwrap_or(false);
                    crate::common::diagnostics::record(
                        "lavalink.connect",
                        serde_json::json!({ "op": self.config.id, "resumed": resumed,
                            "connectMs": crate::common::diagnostics::elapsed_ms(connect_started) }),
                    );
                    let _ = events.send(NodeEvent::Ready {
                        session_id,
                        resumed,
                    });
                }
                "stats" => {
                    self.record_stats(&payload);
                    *self.stats.lock().unwrap_or_else(|p| p.into_inner()) = payload;
                }
                "playerUpdate" => {
                    if crate::common::diagnostics::enabled() {
                        let state = &payload["state"];
                        crate::common::diagnostics::record(
                            "lavalink.player",
                            serde_json::json!({ "guild": payload["guildId"],
                                "pingMs": state["ping"].as_f64().filter(|p| *p >= 0.0),
                                "connected": state["connected"] }),
                        );
                    }
                    let _ = events.send(NodeEvent::PlayerUpdate {
                        guild_id: payload["guildId"].as_str().unwrap_or_default().to_owned(),
                        state: payload["state"].clone(),
                    });
                }
                "event" => {
                    self.record_event(&payload);
                    let _ = events.send(NodeEvent::Event {
                        guild_id: payload["guildId"].as_str().unwrap_or_default().to_owned(),
                        payload,
                    });
                }
                _ => {}
            }
        }
        Ok("stream ended".to_owned())
    }

    // --- REST ------------------------------------------------------------------------------------

    pub async fn load_tracks(&self, identifier: &str) -> Result<Value> {
        let encoded: String = url::form_urlencoded::byte_serialize(identifier.as_bytes()).collect();
        let started = std::time::Instant::now();
        let result = self
            .rest
            .request(
                Method::GET,
                &format!("/loadtracks?identifier={encoded}"),
                None,
            )
            .await;
        if crate::common::diagnostics::enabled() {
            // `ytsearch` / `spsearch` / ... for searches, the host for links.
            let kind = match identifier.split_once(':') {
                Some((prefix, rest)) if !rest.starts_with("//") && prefix.len() <= 12 => {
                    prefix.to_owned()
                }
                _ => url::Url::parse(identifier)
                    .ok()
                    .and_then(|u| {
                        u.host_str()
                            .map(|h| h.trim_start_matches("www.").to_owned())
                    })
                    .unwrap_or_else(|| "other".to_owned()),
            };
            let (load_type, count) = match &result {
                Ok(v) => (
                    v["loadType"].as_str().unwrap_or_default().to_owned(),
                    match &v["data"] {
                        Value::Array(items) => items.len(),
                        Value::Object(map) => map
                            .get("tracks")
                            .and_then(Value::as_array)
                            .map_or(1, Vec::len),
                        _ => 0,
                    },
                ),
                Err(_) => ("request_failed".to_owned(), 0),
            };
            crate::common::diagnostics::record(
                "lavalink.load",
                serde_json::json!({ "op": kind, "loadType": load_type, "tracks": count,
                    "loadMs": crate::common::diagnostics::elapsed_ms(started),
                    "ok": result.is_ok() && load_type != "error" }),
            );
        }
        result
    }

    pub async fn update_player(
        &self,
        guild_id: &str,
        body: &Value,
        no_replace: bool,
    ) -> Result<Value> {
        let session = self
            .session_id()
            .ok_or_else(|| anyhow!("The Lavalink node has no active session."))?;
        let new_track = body["track"]["encoded"].is_string();
        if new_track && crate::common::diagnostics::enabled() {
            self.pending_starts
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(guild_id.to_owned(), std::time::Instant::now());
        }
        self.rest
            .request(
                Method::PATCH,
                &format!("/sessions/{session}/players/{guild_id}?noReplace={no_replace}"),
                Some(body),
            )
            .await
    }

    /// `lavalink.event` per player event; TrackStart also gets the time since the track was
    /// sent (`playToStartMs`: source lookup + first audio from the node).
    fn record_event(&self, payload: &Value) {
        use crate::common::diagnostics;
        if !diagnostics::enabled() {
            return;
        }
        let kind = payload["type"].as_str().unwrap_or_default();
        let guild = payload["guildId"].as_str().unwrap_or_default();
        let mut out = serde_json::json!({ "op": kind, "guild": guild });
        if matches!(kind, "TrackStartEvent" | "TrackEndEvent") {
            let now = std::time::SystemTime::now();
            let mut changes = self.track_changes.lock().unwrap_or_else(|p| p.into_inner());
            changes.push_back(now);
            while changes.front().is_some_and(|t| {
                now.duration_since(*t).unwrap_or_default() > std::time::Duration::from_secs(180)
            }) {
                changes.pop_front();
            }
        }
        match kind {
            "TrackStartEvent" => {
                let sent = self
                    .pending_starts
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(guild);
                out["playToStartMs"] = serde_json::json!(sent.map(diagnostics::elapsed_ms));
                out["source"] = payload["track"]["info"]["sourceName"].clone();
                out["isStream"] = payload["track"]["info"]["isStream"].clone();
                out["lengthMs"] = payload["track"]["info"]["length"].clone();
                // Which exact upload played (a playlist may hold a cover or a kids' version).
                for key in ["title", "author", "identifier", "uri"] {
                    out[key] = payload["track"]["info"][key].clone();
                }
                diagnostics::record("lavalink.track_start", out.clone());
                let ended = self
                    .track_ends
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(guild);
                // A start long after the queue ran out is a new session, not a gap.
                if let Some((at, reason)) =
                    ended.filter(|(at, _)| at.elapsed() < std::time::Duration::from_secs(120))
                {
                    // Silence between two tracks: next track lookup + first audio. Long gaps
                    // after `loadFailed` mean tracks were skipped on the way.
                    diagnostics::record(
                        "lavalink.track_gap",
                        serde_json::json!({ "op": reason, "guild": guild,
                                            "gapMs": diagnostics::elapsed_ms(at) }),
                    );
                }
                return;
            }
            "TrackEndEvent" => {
                out["reason"] = payload["reason"].clone();
                let reason = payload["reason"].as_str().unwrap_or("unknown");
                // `replaced` is followed by the new track at once; `stopped`/`cleanup` by nothing.
                if matches!(reason, "finished" | "loadFailed" | "replaced") {
                    self.track_ends
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .insert(
                            guild.to_owned(),
                            (std::time::Instant::now(), reason.to_owned()),
                        );
                } else {
                    self.track_ends
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .remove(guild);
                }
            }
            "TrackExceptionEvent" => {
                self.pending_starts
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(guild);
                out["severity"] = payload["exception"]["severity"].clone();
                out["error"] = payload["exception"]["message"].clone();
                out["cause"] = serde_json::json!(payload["exception"]["cause"].as_str().map(|c| {
                    c.lines()
                        .next()
                        .unwrap_or_default()
                        .chars()
                        .take(300)
                        .collect::<String>()
                }));
                out["ok"] = serde_json::json!(false);
            }
            "TrackStuckEvent" => {
                out["thresholdMs"] = payload["thresholdMs"].clone();
                out["ok"] = serde_json::json!(false);
            }
            "WebSocketClosedEvent" => {
                out["code"] = payload["code"].clone();
                out["reason"] = payload["reason"].clone();
                out["byRemote"] = payload["byRemote"].clone();
            }
            _ => {}
        }
        diagnostics::record("lavalink.event", out);
    }

    async fn enable_resuming(&self, session: &str) -> Result<()> {
        self.rest
            .request(
                Method::PATCH,
                &format!("/sessions/{session}"),
                Some(&serde_json::json!({ "resuming": true, "timeout": RESUME_TIMEOUT_SECS })),
            )
            .await?;
        Ok(())
    }

    /// Lyrics of the track playing in `guild_id` (LavaLyrics plugin). `None` when no lyrics
    /// were found (204); an error when nothing plays or the plugin is not installed (404).
    pub async fn lyrics(&self, guild_id: &str) -> Result<Option<Value>> {
        let session = self
            .session_id()
            .ok_or_else(|| anyhow!("The Lavalink node has no active session."))?;
        let lyrics = self
            .rest
            .request(
                Method::GET,
                &format!("/sessions/{session}/players/{guild_id}/track/lyrics"),
                None,
            )
            .await?;
        Ok((!lyrics.is_null()).then_some(lyrics))
    }

    pub async fn destroy_player(&self, guild_id: &str) -> Result<()> {
        let Some(session) = self.session_id() else {
            return Ok(());
        };
        self.rest
            .request(
                Method::DELETE,
                &format!("/sessions/{session}/players/{guild_id}"),
                None,
            )
            .await?;
        Ok(())
    }
}

/// Normalised `player.search()` result of lavalink-client.
#[derive(Debug, Clone, Default)]
pub struct SearchResult {
    pub load_type: String,
    pub tracks: Vec<Value>,
    pub playlist_title: Option<String>,
    pub plugin_info: Value,
}

/// Mirrors lavalink-client `node.search({ query, source }, requester, throwOnEmpty)`.
pub async fn search(
    node: &Node,
    query: &str,
    source: Option<&str>,
    throw_on_empty: bool,
) -> Result<SearchResult> {
    if !node.connected() {
        bail!("No available Lavalink node (not connected).");
    }
    let raw = query.trim();
    let is_url = raw.starts_with("http://") || raw.starts_with("https://");
    let identifier = match source {
        Some(source) if !is_url && !source.is_empty() => format!("{source}:{raw}"),
        _ => raw.to_owned(),
    };
    let response = node.load_tracks(&identifier).await?;
    let load_type = response["loadType"].as_str().unwrap_or("empty").to_owned();
    let data = &response["data"];
    let tracks: Vec<Value> = match load_type.as_str() {
        "playlist" => data["tracks"].as_array().cloned().unwrap_or_default(),
        "track" => vec![data.clone()],
        "search" => match data {
            Value::Array(items) => items.clone(),
            Value::Null => Vec::new(),
            other => vec![other.clone()],
        },
        _ => Vec::new(),
    };
    if throw_on_empty && (load_type == "empty" || tracks.is_empty()) {
        bail!("Nothing found");
    }
    let playlist_title = (load_type == "playlist")
        .then(|| {
            data["info"]["name"]
                .as_str()
                .or_else(|| data["pluginInfo"]["name"].as_str())
                .map(str::to_owned)
        })
        .flatten();
    Ok(SearchResult {
        plugin_info: if load_type == "playlist" {
            data["pluginInfo"].clone()
        } else {
            response["pluginInfo"].clone()
        },
        load_type,
        tracks,
        playlist_title,
    })
}

impl Node {
    /// `lavalink.stats` (sent by Lavalink every minute). `frameStats` cover the last minute
    /// (Lavalink `AudioLossCounter`): `nulled` = 20 ms frames the player had no audio for,
    /// `deficit` = 3000 - sent - nulled (positive: the sender fell behind).
    /// `lavalink.frames_lost` is split by `op`: `track_change` when a track started or ended in
    /// that minute (silence while the next track loads), `mid_track` otherwise (stutter).
    fn record_stats(&self, payload: &Value) {
        use crate::common::diagnostics;
        if !diagnostics::enabled() {
            return;
        }
        let changes: Vec<u64> = self
            .track_changes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .filter_map(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .collect();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        record_stats(payload, changed_in_last_minute(&changes, now));
    }
}

/// Lavalink reports the previous wall-clock minute; a change up to 5 s around it counts (the
/// silence of a switch can straddle the minute boundary).
fn changed_in_last_minute(changes_ms: &[u64], now_ms: u64) -> bool {
    let end = now_ms / 60_000 * 60_000;
    let start = end.saturating_sub(60_000);
    changes_ms
        .iter()
        .any(|t| *t + 5_000 >= start && *t < end + 5_000)
}

fn record_stats(payload: &Value, track_changed: bool) {
    use crate::common::diagnostics;
    let frames = &payload["frameStats"];
    let nulled = frames["nulled"].as_i64().unwrap_or(0);
    let deficit = frames["deficit"].as_i64().unwrap_or(0);
    let lost = nulled + deficit;
    diagnostics::record(
        "lavalink.stats",
        serde_json::json!({
            "players": payload["players"],
            "playingPlayers": payload["playingPlayers"],
            "memUsedMb": payload["memory"]["used"].as_f64().map(|b| (b / 1_048_576.0).round()),
            "lavalinkLoad": payload["cpu"]["lavalinkLoad"],
            "systemLoad": payload["cpu"]["systemLoad"],
            "framesSent": frames["sent"],
            "framesNulled": frames["nulled"],
            "framesDeficit": frames["deficit"],
        }),
    );
    if lost > 0 {
        diagnostics::record(
            "lavalink.frames_lost",
            serde_json::json!({ "op": if track_changed { "track_change" } else { "mid_track" },
                                "nulled": frames["nulled"], "deficit": frames["deficit"],
                                "sent": frames["sent"],
                                "lostMs": (nulled + deficit.max(0)) * 20 }),
        );
    }
}

/// `{ track: { encoded } }` body for a PATCH.
pub fn track_body(track: &Value) -> Value {
    json!({ "encoded": track["encoded"] })
}

#[cfg(test)]
mod tests {
    use super::changed_in_last_minute;

    #[test]
    fn frame_stats_minute_matches_track_changes() {
        // Stats at 10:13:26 cover 10:12:00-10:13:00.
        let min = 60_000;
        let now = 613 * min + 26_000;
        assert!(changed_in_last_minute(&[612 * min + 30_000], now));
        assert!(changed_in_last_minute(&[613 * min + 2_000], now)); // straddles the boundary
        assert!(changed_in_last_minute(&[612 * min - 3_000], now));
        assert!(!changed_in_last_minute(&[613 * min + 20_000], now)); // after the window
        assert!(!changed_in_last_minute(&[611 * min], now));
        assert!(!changed_in_last_minute(&[], now));
    }
}
