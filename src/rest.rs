//! Minimal JSON-over-HTTP client (hyper) used for:
//! - Discord routes twilight-http does not model (`PUT /channels/{id}/voice-status`),
//! - the Lavalink v4 REST API.

use anyhow::{Context, Result, bail};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::{Method, Request};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use serde_json::Value;

const DISCORD_API_BASE: &str = "https://discord.com/api/v10";
/// Upper bound for one request including the body. Lavalink calls are made while the music
/// player map is locked, so a stalled node must not block every guild forever.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

type HttpClient = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;

fn build_client(allow_plain_http: bool) -> Result<HttpClient> {
    let builder = hyper_rustls::HttpsConnectorBuilder::new()
        .with_native_roots()
        .context("load native TLS roots")?;
    let connector = if allow_plain_http {
        builder.https_or_http().enable_http1().build()
    } else {
        builder.https_only().enable_http1().build()
    };
    Ok(Client::builder(TokioExecutor::new()).build(connector))
}

/// JSON client bound to a base URL and an `Authorization` header value.
pub struct JsonClient {
    client: HttpClient,
    base: String,
    authorization: String,
    user_agent: String,
    timeout: std::time::Duration,
}

impl JsonClient {
    pub fn new(base: &str, authorization: &str, allow_plain_http: bool) -> Result<Self> {
        Ok(Self {
            client: build_client(allow_plain_http)?,
            base: base.trim_end_matches('/').to_owned(),
            authorization: authorization.to_owned(),
            user_agent: format!(
                "DiscordBot (https://github.com/yhymid/Operator, {})",
                env!("CARGO_PKG_VERSION")
            ),
            timeout: REQUEST_TIMEOUT,
        })
    }

    /// `host[:port]` of the base URL, for diagnostics.
    fn host(&self) -> &str {
        let rest = self
            .base
            .split_once("://")
            .map_or(self.base.as_str(), |(_, r)| r);
        rest.split('/').next().unwrap_or(rest)
    }

    #[cfg(test)]
    fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sends a request; returns the parsed JSON body (`Value::Null` for empty bodies).
    pub async fn request(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
        let method_name = method.as_str().to_owned();
        let mut builder = Request::builder()
            .method(method)
            .uri(format!("{}{path}", self.base))
            .header("Authorization", &self.authorization)
            .header("User-Agent", &self.user_agent);
        let payload = match body {
            Some(body) => {
                builder = builder.header("Content-Type", "application/json");
                Bytes::from(serde_json::to_vec(body)?)
            }
            None => Bytes::new(),
        };
        let request = builder.body(Full::new(payload))?;
        let started = std::time::Instant::now();
        let timing = std::sync::Mutex::new((None, None, None::<String>));
        let exchange = async {
            let response = self.client.request(request).await?;
            let status = response.status();
            let date = response
                .headers()
                .get(http::header::DATE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let ttfb = started.elapsed();
            let bytes = response.into_body().collect().await?.to_bytes();
            *timing.lock().unwrap_or_else(|p| p.into_inner()) = (
                Some(ttfb),
                Some(started.elapsed().saturating_sub(ttfb)),
                date,
            );
            anyhow::Ok((status, bytes))
        };
        let result = tokio::time::timeout(self.timeout, exchange)
            .await
            .with_context(|| format!("request {path} timed out after {:?}", self.timeout))
            .and_then(|inner| inner);
        if crate::common::diagnostics::enabled() {
            let (ttfb, body_time, date) = timing.into_inner().unwrap_or_else(|p| p.into_inner());
            let error = result.as_ref().err().map(|e| format!("{e:#}"));
            crate::common::diagnostics::HttpTiming {
                client: "rest",
                method: &method_name,
                host: self.host(),
                path,
                status: result.as_ref().ok().map(|(s, _)| s.as_u16()),
                ttfb,
                body: body_time,
                total: started.elapsed(),
                bytes: result.as_ref().ok().map(|(_, b)| b.len()),
                date: date.as_deref(),
                error: error.as_deref(),
            }
            .record();
        }
        let (status, bytes) = result?;
        if !status.is_success() {
            let text = String::from_utf8_lossy(&bytes);
            let message = serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|v| v["message"].as_str().map(str::to_owned))
                .unwrap_or_else(|| text.into_owned());
            bail!("{status}: {message}");
        }
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_slice(&bytes)?)
    }
}

/// Downloads a Discord attachment (only `cdn.discordapp.com` / `media.discordapp.net` over
/// HTTPS), refusing bodies over `max_bytes`.
pub async fn download_attachment(url: &str, max_bytes: usize) -> Result<Vec<u8>> {
    let parsed = url::Url::parse(url).context("invalid attachment URL")?;
    if parsed.scheme() != "https"
        || !matches!(
            parsed.host_str(),
            Some("cdn.discordapp.com" | "media.discordapp.net")
        )
    {
        bail!("not a Discord attachment URL");
    }
    let client = build_client(false)?;
    let request = Request::builder()
        .method(Method::GET)
        .uri(parsed.as_str())
        .body(Full::new(Bytes::new()))?;
    let exchange = async {
        let response = client.request(request).await?;
        let status = response.status();
        if !status.is_success() {
            bail!("download failed: {status}");
        }
        let bytes = http_body_util::Limited::new(response.into_body(), max_bytes)
            .collect()
            .await
            .map_err(|error| anyhow::anyhow!("download failed: {error}"))?
            .to_bytes();
        anyhow::Ok(bytes.to_vec())
    };
    tokio::time::timeout(REQUEST_TIMEOUT, exchange)
        .await
        .context("download timed out")?
}

/// Raw Discord REST for routes twilight-http lacks.
pub struct RawRest {
    inner: JsonClient,
}

impl RawRest {
    pub fn new(token: &str) -> Result<Self> {
        Ok(Self {
            inner: JsonClient::new(DISCORD_API_BASE, &format!("Bot {token}"), false)?,
        })
    }

    pub async fn put_json(&self, path: &str, body: &Value) -> Result<()> {
        self.inner.request(Method::PUT, path, Some(body)).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// A peer that accepts the connection but never answers (half-open Lavalink node).
    #[tokio::test]
    async fn request_times_out_on_silent_peer() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        let client = JsonClient::new(&format!("http://{addr}/v4"), "pw", true)
            .unwrap()
            .with_timeout(Duration::from_millis(300));
        let started = std::time::Instant::now();
        let result = client.request(Method::GET, "/info", None).await;
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(5));
        server.abort();
    }
}
