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

    #[cfg(test)]
    fn with_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Sends a request; returns the parsed JSON body (`Value::Null` for empty bodies).
    pub async fn request(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value> {
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
        let exchange = async {
            let response = self.client.request(request).await?;
            let status = response.status();
            let bytes = response.into_body().collect().await?.to_bytes();
            anyhow::Ok((status, bytes))
        };
        let (status, bytes) = tokio::time::timeout(self.timeout, exchange)
            .await
            .with_context(|| format!("request {path} timed out after {:?}", self.timeout))??;
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
