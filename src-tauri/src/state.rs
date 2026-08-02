use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use dashmap::DashMap;
use serde::ser::Serializer;
use serde_json::Value;
use tokio::sync::{broadcast, watch};

use crate::error::{AppError, AppResult};
use crate::mcp::process::KillHandle;
use crate::mcp::protocol::{McpClient, ServerHandshake};

/// UI event fan-out capacity; lagged SSE subscribers drop stale events
/// (`RecvError::Lagged`) instead of back-pressuring the producers.
const EVENT_CAPACITY: usize = 1024;

pub type ServerId = i64;

/// Lifecycle per §3: the UI may only issue tool calls in `Running`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ServerStatus {
    Stopped,
    Starting,
    Initializing,
    Running,
    Errored { message: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LogStream {
    Stdout,
    Stderr,
}

/// Everything the UI observes over `/sse` (and later Tauri events).
#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppEvent {
    StatusChanged {
        server_id: ServerId,
        status: ServerStatus,
    },
    Log {
        server_id: ServerId,
        stream: LogStream,
        #[serde(serialize_with = "serialize_arc_str")]
        line: Arc<str>,
    },
    /// Lines lost to backpressure or the line-length cap; the log viewer
    /// renders this as a dropped-lines marker (T11).
    LogGap {
        server_id: ServerId,
        stream: LogStream,
        dropped: u64,
    },
    /// A server-initiated JSON-RPC notification, forwarded verbatim.
    Notification { server_id: ServerId, payload: Value },
}

// serde's `rc` feature is deliberately off; log lines stay `Arc<str>` across
// threads (spec: never clone Strings for log data), serialized as plain str.
fn serialize_arc_str<S: Serializer>(line: &Arc<str>, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(line)
}

/// Registry entry for a managed server; `runtime` is present from the
/// moment the handshake succeeds until the exit waiter observes death.
pub struct ServerEntry {
    pub status: ServerStatus,
    pub runtime: Option<RunningServer>,
}

/// Cheaply clonable handles to a live server. The `Child` itself is owned by
/// the exit-waiter task; everything here is safe to hand around.
#[derive(Clone)]
pub struct RunningServer {
    pub pid: u32,
    pub client: McpClient,
    pub kill: KillHandle,
    pub handshake: ServerHandshake,
    /// Set by `stop` before signalling, so the exit waiter can tell a
    /// requested stop (→ Stopped) from a crash (→ Errored).
    pub stopping: Arc<AtomicBool>,
    /// Flips to true when the exit waiter reaps the child.
    pub exited: watch::Receiver<bool>,
}

/// Shared application state; cheap to clone, everything inside is shared.
///
/// The SQLite connection is a single `Mutex<Connection>` (rusqlite is sync;
/// no pool crate in the pinned set) — all access goes through
/// `spawn_blocking` per the concurrency rules, where blocking on the mutex
/// is fine.
#[derive(Clone)]
pub struct AppState {
    registry: Arc<DashMap<ServerId, ServerEntry>>,
    db: Arc<Mutex<rusqlite::Connection>>,
    events: broadcast::Sender<AppEvent>,
}

impl AppState {
    /// Takes an opened connection; opening the real config DB (path,
    /// migrations) is `db/`'s job in T6.
    pub fn new(db: rusqlite::Connection) -> Self {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        Self {
            registry: Arc::new(DashMap::new()),
            db: Arc::new(Mutex::new(db)),
            events,
        }
    }

    pub fn db(&self) -> Arc<Mutex<rusqlite::Connection>> {
        Arc::clone(&self.db)
    }

    /// Subscribe to the UI event stream (SSE handler, tests).
    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent> {
        self.events.subscribe()
    }

    /// Publish an event; senders never fail — with no subscribers the event
    /// is simply dropped.
    pub fn publish(&self, event: AppEvent) {
        let _ = self.events.send(event);
    }

    /// Current status; servers absent from the registry are `Stopped`.
    pub fn status(&self, id: ServerId) -> ServerStatus {
        self.registry
            .get(&id)
            .map(|entry| entry.status.clone())
            .unwrap_or(ServerStatus::Stopped)
    }

    /// Update the registry and broadcast the change to the UI. `Stopped`
    /// removes the entry so the registry only holds live servers.
    pub fn set_status(&self, id: ServerId, status: ServerStatus) {
        if status == ServerStatus::Stopped {
            self.registry.remove(&id);
        } else {
            self.registry
                .entry(id)
                .and_modify(|entry| entry.status = status.clone())
                .or_insert_with(|| ServerEntry {
                    status: status.clone(),
                    runtime: None,
                });
        }
        self.publish(AppEvent::StatusChanged {
            server_id: id,
            status,
        });
    }

    /// Atomically claim the right to start `id`: succeeds when the server is
    /// absent (Stopped) or Errored, fails while any start/run is in flight —
    /// so concurrent toggles can't double-spawn. On success the status is
    /// `Starting` and has been broadcast.
    pub fn try_begin_start(&self, id: ServerId) -> bool {
        match self.registry.entry(id) {
            dashmap::mapref::entry::Entry::Occupied(mut occupied) => {
                if !matches!(occupied.get().status, ServerStatus::Errored { .. }) {
                    return false;
                }
                let entry = occupied.get_mut();
                entry.status = ServerStatus::Starting;
                entry.runtime = None;
            }
            dashmap::mapref::entry::Entry::Vacant(vacant) => {
                vacant.insert(ServerEntry {
                    status: ServerStatus::Starting,
                    runtime: None,
                });
            }
        }
        self.publish(AppEvent::StatusChanged {
            server_id: id,
            status: ServerStatus::Starting,
        });
        true
    }

    pub fn install_runtime(&self, id: ServerId, runtime: RunningServer) {
        self.registry
            .entry(id)
            .and_modify(|entry| entry.runtime = Some(runtime.clone()))
            .or_insert(ServerEntry {
                status: ServerStatus::Initializing,
                runtime: Some(runtime),
            });
    }

    pub fn runtime(&self, id: ServerId) -> Option<RunningServer> {
        self.registry
            .get(&id)
            .and_then(|entry| entry.runtime.clone())
    }

    /// Run a closure against the shared SQLite connection on the blocking
    /// pool — never on a runtime thread (concurrency rules, spec §3).
    pub async fn with_db<T, F>(&self, f: F) -> AppResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&rusqlite::Connection) -> AppResult<T> + Send + 'static,
    {
        let db = self.db();
        tokio::task::spawn_blocking(move || {
            let conn = db
                .lock()
                .map_err(|_| AppError::Io(std::io::Error::other("db lock poisoned")))?;
            f(&conn)
        })
        .await
        .map_err(|join| AppError::Io(std::io::Error::other(join)))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> AppState {
        AppState::new(rusqlite::Connection::open_in_memory().unwrap())
    }

    #[test]
    fn unknown_servers_are_stopped() {
        assert_eq!(state().status(42), ServerStatus::Stopped);
    }

    #[test]
    fn set_status_updates_registry_and_broadcasts() {
        let state = state();
        let mut events = state.subscribe();

        state.set_status(1, ServerStatus::Starting);
        assert_eq!(state.status(1), ServerStatus::Starting);
        assert!(matches!(
            events.try_recv().unwrap(),
            AppEvent::StatusChanged { server_id: 1, status: ServerStatus::Starting }
        ));

        state.set_status(1, ServerStatus::Stopped);
        assert_eq!(state.status(1), ServerStatus::Stopped);
        assert!(state.registry.is_empty());
    }

    #[test]
    fn events_serialize_to_stable_ui_shape() {
        let event = AppEvent::Log {
            server_id: 7,
            stream: LogStream::Stderr,
            line: Arc::from("boot ok"),
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            serde_json::json!({
                "type": "log",
                "server_id": 7,
                "stream": "stderr",
                "line": "boot ok",
            })
        );

        let event = AppEvent::StatusChanged {
            server_id: 7,
            status: ServerStatus::Errored { message: "handshake timeout".into() },
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            serde_json::json!({
                "type": "status_changed",
                "server_id": 7,
                "status": { "state": "errored", "message": "handshake timeout" },
            })
        );
    }
}
