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
                    let _ = events.send(NodeEvent::Ready {
                        session_id,
                        resumed: payload["resumed"].as_bool().unwrap_or(false),
                    });
                }
                "stats" => {
                    *self.stats.lock().unwrap_or_else(|p| p.into_inner()) = payload;
                }
                "playerUpdate" => {
                    let _ = events.send(NodeEvent::PlayerUpdate {
                        guild_id: payload["guildId"].as_str().unwrap_or_default().to_owned(),
                        state: payload["state"].clone(),
                    });
                }
                "event" => {
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
        self.rest
            .request(
                Method::GET,
                &format!("/loadtracks?identifier={encoded}"),
                None,
            )
            .await
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
        self.rest
            .request(
                Method::PATCH,
                &format!("/sessions/{session}/players/{guild_id}?noReplace={no_replace}"),
                Some(body),
            )
            .await
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

/// `{ track: { encoded } }` body for a PATCH.
pub fn track_body(track: &Value) -> Value {
    json!({ "encoded": track["encoded"] })
}
