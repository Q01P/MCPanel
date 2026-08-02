//! JSON-RPC over stdio: request/response correlation, the MCP `initialize`
//! handshake, and stdout routing.
//!
//! stdout is the protocol channel; stderr is logs. Every stdout line that is
//! a JSON-RPC object (has `"jsonrpc"`) goes to the dispatcher; anything else
//! — servers misbehave — falls back into the log buffer instead of being
//! lost. Responses resolve pending oneshots by id; server→client requests
//! get a polite `-32601`; server notifications fan out to the UI stream.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, oneshot};
use tracing::debug;

use crate::error::{AppError, AppResult};
use crate::mcp::stream::{BoundedForwarder, CHANNEL_CAPACITY, StreamEvent, json_candidate};

/// Advertised MCP protocol version.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

const NOTIFICATION_CAPACITY: usize = 256;

/// What the `initialize` handshake yields, kept for the UI.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ServerHandshake {
    pub protocol_version: String,
    pub capabilities: Value,
    pub server_info: Value,
}

struct ClientInner {
    next_id: AtomicI64,
    pending: DashMap<i64, oneshot::Sender<AppResult<Value>>>,
    writer_tx: mpsc::Sender<String>,
    request_timeout: Duration,
    /// Set by the router when stdout reaches EOF; requests fail fast instead
    /// of timing out against a dead server.
    closed: AtomicBool,
}

/// Cloneable request handle for one running server.
#[derive(Clone)]
pub struct McpClient {
    inner: Arc<ClientInner>,
}

/// Everything `connect` hands back: the client plus the two receive-side
/// streams the router produces.
pub struct ProtocolHandle {
    pub client: McpClient,
    /// Non-protocol stdout lines, routed into the log buffer.
    pub stdout_logs: mpsc::Receiver<StreamEvent>,
    /// Server notifications, for UI fan-out.
    pub notifications: mpsc::Receiver<Value>,
}

/// Wire a protocol client onto a child's stdin and its pumped stdout lines
/// (from [`crate::mcp::stream::attach`]). Spawns the writer and router tasks.
pub fn connect(
    stdin: tokio::process::ChildStdin,
    stdout: mpsc::Receiver<StreamEvent>,
    request_timeout: Duration,
) -> ProtocolHandle {
    let (writer_tx, writer_rx) = mpsc::channel::<String>(64);
    let (logs_tx, logs_rx) = mpsc::channel(CHANNEL_CAPACITY);
    let (notif_tx, notif_rx) = mpsc::channel(NOTIFICATION_CAPACITY);

    let inner = Arc::new(ClientInner {
        next_id: AtomicI64::new(1),
        pending: DashMap::new(),
        writer_tx,
        request_timeout,
        closed: AtomicBool::new(false),
    });

    tokio::spawn(write_frames(stdin, writer_rx));
    tokio::spawn(route(stdout, Arc::clone(&inner), logs_tx, notif_tx));

    ProtocolHandle {
        client: McpClient { inner },
        stdout_logs: logs_rx,
        notifications: notif_rx,
    }
}

impl McpClient {
    /// Send a request and await its correlated response, bounded by the
    /// per-request timeout.
    pub async fn request(&self, method: &str, params: Value) -> AppResult<Value> {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err(AppError::ConnectionClosed);
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.inner.pending.insert(id, tx);

        let frame =
            json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string();
        if self.inner.writer_tx.send(frame).await.is_err() {
            self.inner.pending.remove(&id);
            return Err(AppError::ConnectionClosed);
        }

        match tokio::time::timeout(self.inner.request_timeout, rx).await {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(_)) => Err(AppError::ConnectionClosed),
            Err(_) => {
                self.inner.pending.remove(&id);
                Err(AppError::Timeout(method.to_string()))
            }
        }
    }

    /// Fire-and-forget notification (no id, no response).
    pub async fn notify(&self, method: &str, params: Value) -> AppResult<()> {
        let frame = json!({ "jsonrpc": "2.0", "method": method, "params": params }).to_string();
        self.inner
            .writer_tx
            .send(frame)
            .await
            .map_err(|_| AppError::ConnectionClosed)
    }

    /// The MCP handshake, owned by Rust: `initialize` → await response →
    /// `notifications/initialized`. Only after this may the UI issue tool
    /// calls. On timeout the caller tears the process tree down and marks
    /// the server Errored (supervisor wiring, T7).
    pub async fn handshake(&self) -> AppResult<ServerHandshake> {
        let result = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {
                        "name": "mcpanel",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                }),
            )
            .await?;
        self.notify("notifications/initialized", json!({})).await?;

        Ok(ServerHandshake {
            protocol_version: result
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            capabilities: result.get("capabilities").cloned().unwrap_or(json!({})),
            server_info: result.get("serverInfo").cloned().unwrap_or(json!({})),
        })
    }
}

async fn write_frames(mut stdin: tokio::process::ChildStdin, mut rx: mpsc::Receiver<String>) {
    while let Some(frame) = rx.recv().await {
        if stdin.write_all(frame.as_bytes()).await.is_err()
            || stdin.write_all(b"\n").await.is_err()
            || stdin.flush().await.is_err()
        {
            return; // stdin gone — the router notices via stdout EOF
        }
    }
}

async fn route(
    mut stdout: mpsc::Receiver<StreamEvent>,
    inner: Arc<ClientInner>,
    logs_tx: mpsc::Sender<StreamEvent>,
    notif_tx: mpsc::Sender<Value>,
) {
    let mut logs = BoundedForwarder::new(logs_tx);

    while let Some(event) = stdout.recv().await {
        match event {
            StreamEvent::Line(line) => match parse_frame(&line) {
                Some(frame) => dispatch(&inner, &notif_tx, frame).await,
                // Not JSON-RPC → log buffer; a gone log receiver is fine,
                // protocol routing continues regardless.
                None => {
                    let _ = logs.offer(line);
                }
            },
            // A dropped stdout line may have been a response; the pending
            // request will hit its timeout. Surface the gap in the logs.
            StreamEvent::Dropped(n) => logs.lose(n),
        }
    }

    // stdout EOF: fail all in-flight requests and everything after them.
    inner.closed.store(true, Ordering::SeqCst);
    let ids: Vec<i64> = inner.pending.iter().map(|entry| *entry.key()).collect();
    for id in ids {
        if let Some((_, tx)) = inner.pending.remove(&id) {
            let _ = tx.send(Err(AppError::ConnectionClosed));
        }
    }
    logs.finish().await;
}

/// A stdout line is protocol iff it parses to a JSON object with `"jsonrpc"`
/// — directly, or after trimming garbage before the first `{`.
fn parse_frame(line: &str) -> Option<Value> {
    let parse = |s: &str| {
        serde_json::from_str::<Value>(s)
            .ok()
            .filter(|v| v.get("jsonrpc").is_some())
    };
    parse(line).or_else(|| json_candidate(line).and_then(parse))
}

async fn dispatch(inner: &Arc<ClientInner>, notif_tx: &mpsc::Sender<Value>, frame: Value) {
    let id = frame.get("id");
    let is_response = frame.get("result").is_some() || frame.get("error").is_some();

    match (id, is_response) {
        (Some(id), true) => {
            let Some(id) = id.as_i64() else { return };
            let Some((_, tx)) = inner.pending.remove(&id) else {
                debug!(target: "app::protocol", id, "response for unknown or timed-out request");
                return;
            };
            let outcome = match frame.get("error") {
                Some(error) => Err(AppError::Rpc {
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                }),
                None => Ok(frame.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = tx.send(outcome);
        }
        // Server→client request (e.g. roots/list): reply politely so the
        // server isn't left waiting.
        (Some(id), false) if frame.get("method").is_some() => {
            let reply = json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": "method not supported by MCPanel" },
            })
            .to_string();
            let _ = inner.writer_tx.send(reply).await;
        }
        // Server notification → UI stream; advisory, dropped under pressure.
        (None, _) if frame.get("method").is_some() => {
            let _ = notif_tx.try_send(frame);
        }
        _ => debug!(target: "app::protocol", "unroutable jsonrpc frame"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_frame_accepts_protocol_and_salvages_garbage_prefixes() {
        assert!(parse_frame(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#).is_some());
        assert!(parse_frame(r#"junk>>{"jsonrpc":"2.0","id":1,"result":{}}"#).is_some());
        assert!(parse_frame(r#"{"not":"jsonrpc"}"#).is_none());
        assert!(parse_frame("plain log line").is_none());
    }
}
