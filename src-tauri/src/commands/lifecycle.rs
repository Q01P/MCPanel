//! Server lifecycle orchestration behind the IPC commands: composes the
//! supervisor (T3), stream pipelines (T4), protocol client (T5), and config
//! store (T6) into `Stopped → Starting → Initializing → Running / Errored`.
//! Plain functions over `AppState` so the whole lifecycle is testable
//! without a webview.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::{mpsc, watch};

use crate::db::{self, EnvValue, NewServer, ServerRecord};
use crate::error::{AppError, AppResult};
use crate::mcp::process::{ManagedChild, ProcessConfig, SHUTDOWN_GRACE, spawn};
use crate::mcp::protocol::{DEFAULT_REQUEST_TIMEOUT, connect};
use crate::mcp::stream::{StreamEvent, attach};
use crate::state::{AppEvent, AppState, LogStream, RunningServer, ServerId, ServerStatus};

/// After the grace-period hard kill, how long we wait for the exit waiter to
/// confirm death before giving up (it should be near-instant).
const KILL_CONFIRM_TIMEOUT: Duration = Duration::from_secs(5);

pub async fn list(state: &AppState) -> AppResult<Vec<ServerOverview>> {
    let records = state.with_db(db::list_servers).await?;
    Ok(records
        .into_iter()
        .map(|record| ServerOverview {
            status: state.status(record.id),
            record,
        })
        .collect())
}

/// A config record joined with its live status — what the server list shows.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ServerOverview {
    #[serde(flatten)]
    pub record: ServerRecord,
    pub status: ServerStatus,
}

pub async fn add(state: &AppState, new: NewServer) -> AppResult<ServerRecord> {
    state.with_db(move |conn| db::insert_server(conn, &new)).await
}

pub async fn update(state: &AppState, record: ServerRecord) -> AppResult<()> {
    state
        .with_db(move |conn| db::update_server(conn, &record))
        .await
}

/// Stops the server first if it is running, then deletes its config.
pub async fn remove(state: &AppState, id: ServerId) -> AppResult<()> {
    stop(state, id).await?;
    state.with_db(move |conn| db::delete_server(conn, id)).await
}

pub async fn start(state: &AppState, id: ServerId) -> AppResult<()> {
    start_with_timeout(state, id, DEFAULT_REQUEST_TIMEOUT).await
}

/// `request_timeout` also bounds the handshake — injectable for tests.
pub async fn start_with_timeout(
    state: &AppState,
    id: ServerId,
    request_timeout: Duration,
) -> AppResult<()> {
    if !state.try_begin_start(id) {
        return Ok(()); // already starting or running — toggles are idempotent
    }
    match run_startup(state, id, request_timeout).await {
        Ok(()) => Ok(()),
        Err(error) => {
            match &error {
                // Unknown server: no ghost Errored entry, just back to absent.
                AppError::ServerNotFound(_) => state.set_status(id, ServerStatus::Stopped),
                _ => state.set_status(
                    id,
                    ServerStatus::Errored {
                        message: error.to_string(),
                    },
                ),
            }
            Err(error)
        }
    }
}

async fn run_startup(
    state: &AppState,
    id: ServerId,
    request_timeout: Duration,
) -> AppResult<()> {
    let record = state.with_db(move |conn| db::get_server(conn, id)).await?;

    // Secret resolution hits the OS credential store (blocking) and happens
    // just-in-time — the resolved values exist only in the child's spawn
    // config, never in state, DB, or logs.
    let env_record = record.clone();
    let env = tokio::task::spawn_blocking(move || crate::secrets::resolve_env(&env_record))
        .await
        .map_err(|join| AppError::Io(std::io::Error::other(join)))??;

    let config = ProcessConfig {
        command: record.command.clone(),
        args: record.args.clone(),
        env,
        cwd: record.cwd.clone().map(Into::into),
    };

    let mut managed = spawn(&config)?;
    let streams = attach(&mut managed.child)?;
    let stdin = managed
        .child
        .stdin
        .take()
        .ok_or_else(|| std::io::Error::other("child stdin not piped"))?;
    let ManagedChild { pid, mut child, kill } = managed;

    let proto = connect(stdin, streams.stdout, request_timeout);
    state.set_status(id, ServerStatus::Initializing);

    // Logs and notifications flow to the UI from here on — including
    // anything printed while the handshake is still in flight.
    tokio::spawn(fan_out_lines(state.clone(), id, LogStream::Stderr, streams.stderr));
    tokio::spawn(fan_out_lines(state.clone(), id, LogStream::Stdout, proto.stdout_logs));
    tokio::spawn(fan_out_notifications(state.clone(), id, proto.notifications));

    let handshake = match proto.client.handshake().await {
        Ok(handshake) => handshake,
        Err(error) => {
            // Handshake failure/timeout ⇒ tear the process tree down.
            kill.kill_now();
            let _ = child.wait().await;
            return Err(error);
        }
    };

    let stopping = Arc::new(AtomicBool::new(false));
    let (exit_tx, exit_rx) = watch::channel(false);
    state.install_runtime(
        id,
        RunningServer {
            pid,
            client: proto.client,
            kill,
            handshake,
            stopping: Arc::clone(&stopping),
            exited: exit_rx,
        },
    );
    state.set_status(id, ServerStatus::Running);

    // Exit waiter: owns the Child, reaps it, and settles the final status.
    let waiter_state = state.clone();
    tokio::spawn(async move {
        let result = child.wait().await;
        let _ = exit_tx.send(true);
        if stopping.load(Ordering::SeqCst) {
            waiter_state.set_status(id, ServerStatus::Stopped);
        } else {
            let detail = match result {
                Ok(status) => status.to_string(),
                Err(error) => error.to_string(),
            };
            waiter_state.set_status(
                id,
                ServerStatus::Errored {
                    message: format!("server exited unexpectedly ({detail})"),
                },
            );
        }
    });

    Ok(())
}

/// Graceful stop per spec: signal the tree, wait the grace period, then hard
/// kill. The exit waiter settles the Stopped status; this returns once the
/// child is confirmed dead.
pub async fn stop(state: &AppState, id: ServerId) -> AppResult<()> {
    let Some(runtime) = state.runtime(id) else {
        // Not running; clear a lingering Errored entry back to Stopped.
        if state.status(id) != ServerStatus::Stopped {
            state.set_status(id, ServerStatus::Stopped);
        }
        return Ok(());
    };

    runtime.stopping.store(true, Ordering::SeqCst);
    runtime.kill.signal_graceful();

    let mut exited = runtime.exited.clone();
    let graceful = tokio::time::timeout(SHUTDOWN_GRACE, exited.wait_for(|done| *done))
        .await
        .is_ok();
    if !graceful {
        runtime.kill.kill_now();
        let confirmed = tokio::time::timeout(KILL_CONFIRM_TIMEOUT, exited.wait_for(|done| *done))
            .await
            .is_ok();
        if !confirmed {
            return Err(AppError::Timeout(format!("stopping server {id}")));
        }
    }
    Ok(())
}

/// Store a secret value in the OS credential store and mark the key as
/// secret in the server's env config. The value itself never touches the DB.
pub async fn set_secret(
    state: &AppState,
    id: ServerId,
    key: String,
    value: String,
) -> AppResult<()> {
    let mut record = state.with_db(move |conn| db::get_server(conn, id)).await?;

    let name = record.name.clone();
    let store_key = key.clone();
    tokio::task::spawn_blocking(move || crate::secrets::store_secret(&name, &store_key, &value))
        .await
        .map_err(|join| AppError::Io(std::io::Error::other(join)))??;

    if record.env.get(&key) != Some(&EnvValue::Secret) {
        record.env.insert(key, EnvValue::Secret);
        state
            .with_db(move |conn| db::update_server(conn, &record))
            .await?;
    }
    Ok(())
}

/// Remove a secret from the credential store and drop its env marker.
pub async fn delete_secret(state: &AppState, id: ServerId, key: String) -> AppResult<()> {
    let mut record = state.with_db(move |conn| db::get_server(conn, id)).await?;

    let name = record.name.clone();
    let delete_key = key.clone();
    tokio::task::spawn_blocking(move || crate::secrets::delete_secret(&name, &delete_key))
        .await
        .map_err(|join| AppError::Io(std::io::Error::other(join)))??;

    if record.env.remove(&key).is_some() {
        state
            .with_db(move |conn| db::update_server(conn, &record))
            .await?;
    }
    Ok(())
}

async fn fan_out_lines(
    state: AppState,
    server_id: ServerId,
    stream: LogStream,
    mut rx: mpsc::Receiver<StreamEvent>,
) {
    while let Some(event) = rx.recv().await {
        state.publish(match event {
            StreamEvent::Line(line) => AppEvent::Log {
                server_id,
                stream,
                line,
            },
            StreamEvent::Dropped(dropped) => AppEvent::LogGap {
                server_id,
                stream,
                dropped,
            },
        });
    }
}

async fn fan_out_notifications(
    state: AppState,
    server_id: ServerId,
    mut rx: mpsc::Receiver<serde_json::Value>,
) {
    while let Some(payload) = rx.recv().await {
        state.publish(AppEvent::Notification { server_id, payload });
    }
}
