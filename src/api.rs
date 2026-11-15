//! Minimal Telegram Bot API client: JSON calls (what Telegraf's `callApi` does) wrapped in the
//! per-chat limiter from `bot.js`, plus the streaming multipart uploader from `cmd/ytdlp.js`.

use std::fmt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::Context;
use bytes::Bytes;
use futures_util::StreamExt;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, StreamBody};
use hyper::Request;
use hyper::body::Frame;
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use regex::Regex;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;

use crate::limiter::{Limiter, Task};
use crate::log;
use crate::settings::Settings;

/// Error returned by the Bot API (`code` set) or by the network (`code` = `None`).
#[derive(Debug, Clone)]
pub struct ApiError {
    pub code: Option<i64>,
    pub retry_after: Option<f64>,
    pub message: String,
}

impl ApiError {
    fn network(message: impl Into<String>) -> Self {
        Self {
            code: None,
            retry_after: None,
            message: message.into(),
        }
    }

    /// `getRetryAfterSeconds(error)`
    pub fn retry_after_seconds(&self) -> Option<f64> {
        if let Some(value) = self.retry_after.filter(|v| v.is_finite() && *v > 0.0) {
            return Some(value);
        }
        retry_after_from_message(&self.message)
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

fn retry_after_from_message(message: &str) -> Option<f64> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE
        .get_or_init(|| Regex::new(r"(?i)retry\s+after\s+(\d+(?:\.\d+)?)").expect("static regex"));
    re.captures(message).and_then(|c| c[1].parse::<f64>().ok())
}

/// `isRateLimitError(error)` for any error produced by this crate.
pub fn is_rate_limit_error(error: &anyhow::Error) -> bool {
    match error.downcast_ref::<ApiError>() {
        Some(api) => api.retry_after_seconds().is_some() || api.code == Some(429),
        None => retry_after_from_message(&format!("{error:#}")).is_some(),
    }
}

type JsonClient = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;
type UploadClient = Client<HttpsConnector<HttpConnector>, BoxBody<Bytes, std::io::Error>>;

fn connector() -> anyhow::Result<HttpsConnector<HttpConnector>> {
    Ok(hyper_rustls::HttpsConnectorBuilder::new()
        .with_native_roots()
        .context("load native TLS roots")?
        .https_or_http()
        .enable_http1()
        .build())
}

const UPLOAD_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const JSON_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const UPLOAD_CHUNK: usize = 256 * 1024;

pub struct Api {
    settings: Arc<Settings>,
    json: JsonClient,
    upload: UploadClient,
    pub limiter: Arc<Limiter>,
}

/// Parses a Bot API envelope `{ ok, result, error_code, description, parameters }`.
fn parse_envelope(status: u16, bytes: &[u8], upload: bool) -> Result<Value, ApiError> {
    let parsed: Value = serde_json::from_slice(bytes).map_err(|_| {
        ApiError::network(if upload {
            format!("Telegram zwrócił nieprawidłową odpowiedź ({status}).")
        } else {
            format!("invalid json response body ({status})")
        })
    })?;
    if parsed["ok"] == Value::Bool(true) {
        return Ok(parsed["result"].clone());
    }
    let code = parsed["error_code"].as_i64().or(Some(i64::from(status)));
    let description = parsed["description"].as_str().map(str::to_owned);
    let message = if upload {
        format!(
            "Telegram API: {}",
            description.unwrap_or_else(|| format!("HTTP {status}"))
        )
    } else {
        // Telegraf's TelegramError: `${error_code}: ${description}`
        format!(
            "{}: {}",
            code.unwrap_or_default(),
            description.unwrap_or_default()
        )
    };
    Err(ApiError {
        code,
        retry_after: parsed["parameters"]["retry_after"].as_f64(),
        message,
    })
}

impl Api {
    pub fn new(settings: Arc<Settings>) -> anyhow::Result<Arc<Self>> {
        let json = Client::builder(TokioExecutor::new()).build(connector()?);
        // A fresh socket per multipart upload, like `agent: false` + `Connection: close` in JS.
        let upload = Client::builder(TokioExecutor::new())
            .pool_max_idle_per_host(0)
            .build(connector()?);
        let limiter = Limiter::new(
            settings.chat_min_interval_ms,
            settings.rate_limit_retries,
            settings.rate_limit_jitter_ms,
        );
        Ok(Arc::new(Self {
            settings,
            json,
            upload,
            limiter,
        }))
    }

    fn method_url(&self, method: &str) -> String {
        format!(
            "{}/bot{}/{method}",
            self.settings.api_root, self.settings.token
        )
    }

    /// Unthrottled JSON request.
    pub async fn raw_call(
        &self,
        method: &str,
        payload: &Value,
        timeout: Duration,
    ) -> Result<Value, ApiError> {
        let body = serde_json::to_vec(payload).map_err(|e| ApiError::network(e.to_string()))?;
        let request = Request::post(self.method_url(method))
            .header("Content-Type", "application/json")
            .body(Full::new(Bytes::from(body)))
            .map_err(|e| ApiError::network(e.to_string()))?;
        let exchange = async {
            let response = self
                .json
                .request(request)
                .await
                .map_err(|e| ApiError::network(format!("request to {method} failed: {e}")))?;
            let status = response.status().as_u16();
            let bytes = response
                .into_body()
                .collect()
                .await
                .map_err(|e| ApiError::network(format!("request to {method} failed: {e}")))?
                .to_bytes();
            parse_envelope(status, &bytes, false)
        };
        match tokio::time::timeout(timeout, exchange).await {
            Ok(result) => result,
            Err(_) => Err(ApiError::network(format!("request to {method} timed out"))),
        }
    }

    /// Telegraf `callApi` as patched in `bot.js`: calls with a `chat_id` go through the
    /// per-chat limiter with method-based priorities; progress edits are coalesced.
    pub async fn call(self: &Arc<Self>, method: &str, payload: Value) -> Result<Value, ApiError> {
        let chat_id = match &payload["chat_id"] {
            Value::Null => return self.raw_call(method, &payload, JSON_TIMEOUT).await,
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        let lower = method.to_ascii_lowercase();
        let is_status_edit = lower.starts_with("editmessage");
        let is_media_send = [
            "sendvideo",
            "senddocument",
            "sendaudio",
            "sendanimation",
            "sendvoice",
            "sendvideonote",
            "sendphoto",
        ]
        .iter()
        .any(|m| lower.starts_with(m));
        let priority = if is_media_send {
            0
        } else if is_status_edit {
            20
        } else if method == "sendChatAction" {
            30
        } else {
            10
        };
        let dedupe_key = match &payload["message_id"] {
            Value::Null => None,
            id if is_status_edit => Some(format!("{method}:{chat_id}:{id}")),
            _ => None,
        };
        let api = self.clone();
        let method_owned = method.to_owned();
        let task: Task = Box::new(move || {
            let api = api.clone();
            let method = method_owned.clone();
            let payload = payload.clone();
            Box::pin(async move { api.raw_call(&method, &payload, JSON_TIMEOUT).await })
        });
        self.limiter
            .schedule(chat_id, task, priority, dedupe_key, method)
            .await
    }

    /// `uploadLocalFile(ctx, method, filePath, fields)` — scheduled with priority 0.
    pub async fn upload_file(
        self: &Arc<Self>,
        chat_id: i64,
        method: &str,
        path: &Path,
        fields: Vec<(&'static str, String)>,
    ) -> Result<Value, ApiError> {
        let api = self.clone();
        let method_owned = method.to_owned();
        let path = path.to_owned();
        let task: Task = Box::new(move || {
            let api = api.clone();
            let method = method_owned.clone();
            let path = path.clone();
            let fields = fields.clone();
            Box::pin(async move { api.upload_now(chat_id, &method, &path, &fields).await })
        });
        self.limiter
            .schedule(chat_id.to_string(), task, 0, None, method)
            .await
    }

    async fn upload_now(
        &self,
        chat_id: i64,
        method: &str,
        path: &Path,
        fields: &[(&'static str, String)],
    ) -> Result<Value, ApiError> {
        let io_error = |e: std::io::Error| ApiError::network(e.to_string());
        let boundary = format!(
            "----operator-{}-{:x}",
            chrono_millis(),
            rand::random::<u64>()
        );
        let file_field = if method == "sendVideo" {
            "video"
        } else {
            "document"
        };
        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let file_size = tokio::fs::metadata(path).await.map_err(io_error)?.len();
        let mut head = String::new();
        let chat = chat_id.to_string();
        for (name, value) in std::iter::once(("chat_id", chat.as_str()))
            .chain(fields.iter().map(|(n, v)| (*n, v.as_str())))
        {
            head.push_str(&format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            ));
        }
        head.push_str(&format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{file_field}\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        ));
        let tail = format!("\r\n--{boundary}--\r\n");
        let content_length = head.len() as u64 + file_size + tail.len() as u64;
        log::debug(
            "Natywny upload HTTPS",
            Some(
                json!({ "method": method, "fileSize": file_size, "contentLength": content_length }),
            ),
        );

        let file = tokio::fs::File::open(path).await.map_err(io_error)?;
        let activity = Arc::new(AtomicU64::new(chrono_millis()));
        let file_stream = futures_util::stream::unfold(Some(file), |state| async move {
            let mut file = state?;
            let mut buffer = vec![0u8; UPLOAD_CHUNK];
            match file.read(&mut buffer).await {
                Ok(0) => None,
                Ok(n) => {
                    buffer.truncate(n);
                    Some((Ok(Bytes::from(buffer)), Some(file)))
                }
                Err(error) => Some((Err(error), None)),
            }
        });
        let touch = activity.clone();
        let stream = futures_util::stream::once(async move { Ok(Bytes::from(head)) })
            .chain(file_stream)
            .chain(futures_util::stream::once(
                async move { Ok(Bytes::from(tail)) },
            ))
            .map(move |chunk| {
                touch.store(chrono_millis(), Ordering::Relaxed);
                chunk.map(Frame::data)
            });
        let body = BodyExt::boxed(StreamBody::new(stream));
        let request = Request::post(self.method_url(method))
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .header("Content-Length", content_length)
            .header("Connection", "close")
            .body(body)
            .map_err(|e| ApiError::network(e.to_string()))?;

        let exchange = async {
            let response = self
                .upload
                .request(request)
                .await
                .map_err(|e| ApiError::network(e.to_string()))?;
            let status = response.status().as_u16();
            let bytes = response
                .into_body()
                .collect()
                .await
                .map_err(|e| ApiError::network(e.to_string()))?
                .to_bytes();
            parse_envelope(status, &bytes, true)
        };
        tokio::pin!(exchange);
        // Socket inactivity timeout (`timeout: 10 min` on the Node request).
        let mut ticker = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                result = &mut exchange => return result,
                _ = ticker.tick() => {
                    let idle = chrono_millis().saturating_sub(activity.load(Ordering::Relaxed));
                    if idle > UPLOAD_IDLE_TIMEOUT.as_millis() as u64 {
                        return Err(ApiError::network("Przekroczono limit czasu uploadu Telegrama."));
                    }
                }
            }
        }
    }
}

fn chrono_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelopes_and_rate_limits() {
        let ok = parse_envelope(200, br#"{"ok":true,"result":{"x":1}}"#, false).unwrap();
        assert_eq!(ok["x"], 1);
        let err = parse_envelope(
            429,
            br#"{"ok":false,"error_code":429,"description":"Too Many Requests: retry after 7","parameters":{"retry_after":7}}"#,
            false,
        )
        .unwrap_err();
        assert_eq!(err.message, "429: Too Many Requests: retry after 7");
        assert_eq!(err.retry_after_seconds(), Some(7.0));
        assert!(is_rate_limit_error(&anyhow::Error::new(err)));
        let upload = parse_envelope(
            400,
            br#"{"ok":false,"error_code":400,"description":"Bad"}"#,
            true,
        )
        .unwrap_err();
        assert_eq!(upload.message, "Telegram API: Bad");
        assert!(!is_rate_limit_error(&anyhow::Error::new(upload)));
        assert_eq!(retry_after_from_message("Retry After 2.5 s"), Some(2.5));
    }
}
