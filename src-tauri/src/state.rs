use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use dashmap::DashMap;
use serde::ser::Serializer;
use serde_json::Value;
use tokio::sync::{broadcast, watch};
use tokio_util::sync::CancellationToken;

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
    /// renders this as a dropped-lines marker.
    LogGap {
        server_id: ServerId,
        stream: LogStream,
        dropped: u64,
    },
    /// A server-initiated JSON-RPC notification, forwarded verbatim.
    Notification { server_id: ServerId, payload: Value },
    /// Notifications lost to backpressure on the advisory channel — the
    /// notification analogue of [`AppEvent::LogGap`].
    NotificationGap { server_id: ServerId, dropped: u64 },
}

// serde's `rc` feature is deliberately off; log lines stay `Arc<str>` across
// threads (spec: never clone Strings for log data), serialized as plain str.
fn serialize_arc_str<S: Serializer>(line: &Arc<str>, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(line)
}

/// Registry entry for a managed server; `runtime` is present exactly while
/// the status is `Running` — installed when the handshake succeeds, cleared
/// by the first non-`Running` status write (crash included) — and `start`
/// from `try_begin_start` until the runtime is installed (or the start
/// settles without one).
pub struct ServerEntry {
    pub status: ServerStatus,
    pub runtime: Option<RunningServer>,
    pub start: Option<StartHandle>,
}

/// Observer side of an in-flight start: `stop`/`remove` cancel the token and
/// wait for `settled` before re-inspecting the registry.
#[derive(Clone)]
pub struct StartHandle {
    pub cancel: CancellationToken,
    /// Flips to true once the start task has written the final status.
    pub settled: watch::Receiver<bool>,
}

/// Held by the start task for its whole run. Settles on drop, so every exit
/// path — including panics — signals waiters, and only after the final
/// status write.
pub struct StartGuard {
    cancel: CancellationToken,
    settled: watch::Sender<bool>,
}

impl StartGuard {
    pub fn token(&self) -> &CancellationToken {
        &self.cancel
    }
}

impl Drop for StartGuard {
    fn drop(&mut self) {
        let _ = self.settled.send(true);
    }
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
    /// Serializes config read-modify-write cycles (`update`, `set_secret`,
    /// `delete_secret`): `with_db` releases the connection between the read
    /// and the write, so without this two concurrent mutations lose one
    /// side's changes (e.g. an update clobbering a fresh secret marker).
    config_write: Arc<tokio::sync::Mutex<()>>,
}

/// Run a blocking closure on the blocking pool. A panicked task surfaces as
/// `AppError::Internal`, not `Io` — the frontend matches on codes, and "our
/// task died" must stay distinguishable from a real I/O failure.
pub async fn blocking<T, F>(f: F) -> AppResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> AppResult<T> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|join| AppError::Internal(join.to_string()))?
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
            config_write: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Hold this guard across a config read-modify-write; see `config_write`.
    pub async fn lock_config(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.config_write.lock().await
    }

    /// Private on purpose: handing the raw connection out would invite
    /// callers to bypass the `spawn_blocking` rule `with_db` enforces.
    fn db(&self) -> Arc<Mutex<rusqlite::Connection>> {
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
    /// removes the entry so the registry only holds live servers; any other
    /// non-`Running` status drops the runtime, so a crashed server's stale
    /// handles can't be reached through `runtime()` (`Running` itself is only
    /// ever set atomically by [`Self::try_install_runtime`]).
    pub fn set_status(&self, id: ServerId, status: ServerStatus) {
        if status == ServerStatus::Stopped {
            self.registry.remove(&id);
        } else {
            self.registry
                .entry(id)
                .and_modify(|entry| {
                    entry.status = status.clone();
                    if status != ServerStatus::Running {
                        entry.runtime = None;
                    }
                })
                .or_insert_with(|| ServerEntry {
                    status: status.clone(),
                    runtime: None,
                    start: None,
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
    /// `Starting` (broadcast) and a fresh [`StartHandle`] is installed; the
    /// returned guard must live for the whole start attempt — its drop marks
    /// the start settled.
    pub fn try_begin_start(&self, id: ServerId) -> Option<StartGuard> {
        let cancel = CancellationToken::new();
        let (settled_tx, settled_rx) = watch::channel(false);
        let handle = StartHandle {
            cancel: cancel.clone(),
            settled: settled_rx,
        };
        match self.registry.entry(id) {
            dashmap::mapref::entry::Entry::Occupied(mut occupied) => {
                if !matches!(occupied.get().status, ServerStatus::Errored { .. }) {
                    return None;
                }
                let entry = occupied.get_mut();
                entry.status = ServerStatus::Starting;
                entry.runtime = None;
                entry.start = Some(handle);
            }
            dashmap::mapref::entry::Entry::Vacant(vacant) => {
                vacant.insert(ServerEntry {
                    status: ServerStatus::Starting,
                    runtime: None,
                    start: Some(handle),
                });
            }
        }
        self.publish(AppEvent::StatusChanged {
            server_id: id,
            status: ServerStatus::Starting,
        });
        Some(StartGuard {
            cancel,
            settled: settled_tx,
        })
    }

    /// The in-flight start for `id`, if any — present during
    /// Starting/Initializing until the runtime is installed.
    pub fn start_handle(&self, id: ServerId) -> Option<StartHandle> {
        self.registry.get(&id).and_then(|entry| entry.start.clone())
    }

    /// Promote a successfully-handshaken start to Running. Decided atomically
    /// under the entry lock: succeeds only while the entry still holds an
    /// uncancelled start handle, so a concurrent `stop`/`remove` (which
    /// cancels first) can never be resurrected by a late install. On failure
    /// the caller owns the child and must kill it.
    pub fn try_install_runtime(&self, id: ServerId, runtime: RunningServer) -> bool {
        let installed = match self.registry.entry(id) {
            dashmap::mapref::entry::Entry::Occupied(mut occupied) => {
                let entry = occupied.get_mut();
                match &entry.start {
                    Some(start) if !start.cancel.is_cancelled() => {
                        entry.runtime = Some(runtime);
                        entry.start = None;
                        entry.status = ServerStatus::Running;
                        true
                    }
                    _ => false,
                }
            }
            dashmap::mapref::entry::Entry::Vacant(_) => false,
        };
        if installed {
            self.publish(AppEvent::StatusChanged {
                server_id: id,
                status: ServerStatus::Running,
            });
        }
        installed
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
        blocking(move || {
            let conn = db
                .lock()
                .map_err(|_| AppError::Internal("db lock poisoned".into()))?;
            f(&conn)
        })
        .await
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
            AppEvent::StatusChanged {
                server_id: 1,
                status: ServerStatus::Starting
            }
        ));

        state.set_status(1, ServerStatus::Stopped);
        assert_eq!(state.status(1), ServerStatus::Stopped);
        assert!(state.registry.is_empty());
    }

    #[test]
    fn start_guard_settles_on_drop_and_blocks_double_start() {
        let state = state();

        let guard = state.try_begin_start(1).expect("first claim");
        assert_eq!(state.status(1), ServerStatus::Starting);
        let handle = state.start_handle(1).expect("handle during Starting");
        assert!(!*handle.settled.borrow());

        // A second claim while a start is in flight must fail.
        assert!(state.try_begin_start(1).is_none());

        drop(guard);
        assert!(*handle.settled.borrow(), "drop settles the start");
    }

    #[test]
    fn stopped_clears_the_start_handle() {
        let state = state();
        let _guard = state.try_begin_start(1).expect("claim");
        assert!(state.start_handle(1).is_some());

        state.set_status(1, ServerStatus::Stopped);
        assert!(state.start_handle(1).is_none());
        assert!(state.registry.is_empty());
    }

    #[test]
    fn errored_entry_can_be_reclaimed_with_a_fresh_handle() {
        let state = state();
        let guard = state.try_begin_start(1).expect("claim");
        guard.token().cancel();
        state.set_status(
            1,
            ServerStatus::Errored {
                message: "boom".into(),
            },
        );
        drop(guard);

        let fresh = state.try_begin_start(1).expect("reclaim after Errored");
        assert!(
            !fresh.token().is_cancelled(),
            "reclaim must install a fresh, uncancelled token"
        );
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
            status: ServerStatus::Errored {
                message: "handshake timeout".into(),
            },
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
