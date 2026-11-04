use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use futures_util::{Stream, StreamExt};
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::OnceCell;
use tracing::{info, warn};

use commonthread_runtime::embedded::Embedded;

use super::config::{CommonthreadProviderConfig, Endpoint};

/// The built-in server and its state directory, kept until [`shutdown`].
static BUILT_IN: std::sync::Mutex<Option<(Embedded, tempfile::TempDir)>> =
    std::sync::Mutex::new(None);

/// Stop the built-in server's agents, if tabpilot started one.
pub async fn shutdown() {
    let built_in = BUILT_IN.lock().expect("built-in server lock").take();
    if let Some((server, dir)) = built_in {
        match tokio::time::timeout(std::time::Duration::from_secs(5), server.stop()).await {
            Ok(Ok(())) => info!("stopped built-in commonthread server"),
            Ok(Err(err)) => {
                warn!(error = %format!("{err:#}"), "built-in commonthread server failed")
            }
            Err(_) => warn!("built-in commonthread server did not stop in time"),
        }
        drop(dir);
    }
}

/// The server requests go to, chosen on first use.
#[derive(Debug, Clone)]
pub(super) struct Server {
    pub endpoint: Endpoint,
    pub built_in: bool,
}

/// A journal entry from `GET /v1/sessions/{id}/events`.
#[derive(Debug, Deserialize)]
pub(super) struct Event {
    pub id: i64,
    pub kind: String,
    #[serde(default)]
    pub data: Value,
}

/// An HTTP error reported by the server's `{"error":{"code","message"}}` envelope.
#[derive(Debug)]
pub(super) struct ApiError {
    pub status: StatusCode,
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "commonthread {} {}: {}",
            self.status.as_u16(),
            self.code,
            self.message
        )
    }
}

impl std::error::Error for ApiError {}

#[derive(Clone)]
pub(super) struct CommonthreadApi {
    http: reqwest::Client,
    shared: Option<Endpoint>,
    server: Arc<OnceCell<Server>>,
}

impl CommonthreadApi {
    pub(super) fn new(config: &CommonthreadProviderConfig) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(5))
                .build()
                .context("failed to build Commonthread HTTP client")?,
            shared: config.shared.clone(),
            server: Arc::default(),
        })
    }

    pub(super) fn server(&self) -> Option<&Server> {
        self.server.get()
    }

    /// The configured shared server while it is running, else a built-in one.
    async fn endpoint(&self) -> Result<&Endpoint> {
        let server = self
            .server
            .get_or_try_init(|| async {
                if let Some(shared) = &self.shared {
                    let probe = self
                        .http
                        .get(format!("{}/v1/client", shared.url))
                        .bearer_auth(&shared.token)
                        .send()
                        .await;
                    match probe {
                        Err(err) if err.is_connect() => {
                            warn!(url = shared.url, "commonthread server is not running");
                        }
                        _ => {
                            return Ok(Server {
                                endpoint: shared.clone(),
                                built_in: false,
                            });
                        }
                    }
                }
                let dir = tempfile::Builder::new()
                    .prefix("tabpilot-commonthread-")
                    .tempdir()
                    .context("failed to create the built-in server's directory")?;
                let server = Embedded::start("tabpilot", dir.path())
                    .await
                    .context("failed to start the built-in commonthread server")?;
                info!(url = server.url, "started built-in commonthread server");
                let endpoint = Endpoint {
                    url: server.url.clone(),
                    token: server.token.clone(),
                };
                *BUILT_IN.lock().expect("built-in server lock") = Some((server, dir));
                anyhow::Ok(Server {
                    endpoint,
                    built_in: true,
                })
            })
            .await?;
        Ok(&server.endpoint)
    }

    pub(super) async fn get(&self, path: &str) -> Result<Value> {
        self.call(Method::GET, path, None).await
    }

    /// Send a mutation. Each call is one intended action with a fresh
    /// idempotency key; a transport failure is retried once with the same key,
    /// so an uncertain delivery is never executed twice.
    pub(super) async fn post(&self, path: &str, body: Option<Value>) -> Result<Value> {
        self.call(Method::POST, path, body).await
    }

    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let endpoint = self.endpoint().await?;
        let key = (method != Method::GET).then(idempotency_key);
        let mut retried = false;
        loop {
            let mut request = self
                .http
                .request(method.clone(), format!("{}{path}", endpoint.url))
                .bearer_auth(&endpoint.token)
                .timeout(std::time::Duration::from_secs(15));
            if let Some(key) = &key {
                request = request.header("Idempotency-Key", key);
            }
            if let Some(body) = &body {
                request = request.json(body);
            }
            let response = match request.send().await {
                Ok(response) => response,
                Err(err) if !retried && (err.is_connect() || err.is_timeout()) => {
                    retried = true;
                    continue;
                }
                Err(err) => return Err(err).with_context(|| format!("{method} {path} failed")),
            };
            return decode(response).await;
        }
    }

    /// Close an idle session's worker in the background; history stays on the server.
    pub(super) fn close(&self, session_id: String) {
        let api = self.clone();
        tokio::spawn(async move {
            let path = format!("/v1/sessions/{session_id}/close");
            if let Err(err) = api.post(&path, None).await {
                info!(session = session_id, error = %format!("{err:#}"), "session close skipped");
            }
        });
    }

    /// Stream session events after `after` until the server closes the stream.
    pub(super) async fn events(
        &self,
        session_id: &str,
        after: i64,
    ) -> Result<impl Stream<Item = Result<Event>> + Send + 'static> {
        let endpoint = self.endpoint().await?;
        let response = self
            .http
            .get(format!("{}/v1/sessions/{session_id}/events", endpoint.url))
            .bearer_auth(&endpoint.token)
            .header("Accept", "text/event-stream")
            .header("Last-Event-ID", after.to_string())
            .send()
            .await
            .context("failed to open Commonthread event stream")?;
        if !response.status().is_success() {
            decode(response).await?;
            bail!("Commonthread event stream was refused");
        }
        let bytes = response.bytes_stream();
        Ok(futures_util::stream::unfold(
            (bytes, SseParser::default()),
            |(mut bytes, mut parser)| async move {
                loop {
                    if let Some(data) = parser.next_data() {
                        let event = serde_json::from_str(&data)
                            .context("invalid Commonthread event payload");
                        return Some((event, (bytes, parser)));
                    }
                    match bytes.next().await? {
                        Ok(chunk) => parser.push(&chunk),
                        Err(err) => {
                            let err = anyhow::Error::from(err).context("event stream failed");
                            return Some((Err(err), (bytes, parser)));
                        }
                    }
                }
            },
        ))
    }
}

async fn decode(response: reqwest::Response) -> Result<Value> {
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .with_context(|| format!("invalid Commonthread response ({status})"))?;
    if status.is_success() {
        return Ok(body);
    }
    let error = &body["error"];
    Err(ApiError {
        status,
        code: error["code"].as_str().unwrap_or("unknown").to_owned(),
        message: error["message"].as_str().unwrap_or_default().to_owned(),
    }
    .into())
}

fn idempotency_key() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    format!(
        "tabpilot-{}-{nanos:x}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// Collects `data:` lines of server-sent events; keepalive comments and the
/// `id`/`event` fields are ignored because the JSON payload repeats them.
#[derive(Default)]
struct SseParser {
    buffer: Vec<u8>,
    data: String,
}

impl SseParser {
    fn push(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
    }

    fn next_data(&mut self) -> Option<String> {
        while let Some(end) = self.buffer.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buffer.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.data.is_empty() {
                    return Some(std::mem::take(&mut self.data));
                }
            } else if let Some(value) = line.strip_prefix("data:") {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(value.strip_prefix(' ').unwrap_or(value));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_parser_handles_split_chunks_comments_and_multiline_data() {
        let mut parser = SseParser::default();
        parser.push(b": keepalive\n\nid: 3\nevent: turn.fin");
        assert_eq!(parser.next_data(), None);
        parser.push(b"ished\ndata: {\"id\":3,\r\ndata: \"kind\":\"x\"}\n\n");
        assert_eq!(parser.next_data().unwrap(), "{\"id\":3,\n\"kind\":\"x\"}");
        assert_eq!(parser.next_data(), None);
    }

    #[test]
    fn idempotency_keys_are_unique_visible_ascii() {
        let (a, b) = (idempotency_key(), idempotency_key());
        assert_ne!(a, b);
        assert!(a.len() <= 128 && a.bytes().all(|c| c.is_ascii_graphic()));
    }
}
