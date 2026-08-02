//! Server lifecycle orchestration behind the IPC commands: composes the
//! supervisor (T3), stream pipelines (T4), protocol client (T5), and config
//! store (T6) into `Stopped → Starting → Initializing → Running / Errored`.
//! Plain functions over `AppState` so the whole lifecycle is testable
//! without a webview.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::db::{self, EnvValue, NewServer, ServerRecord};
use crate::error::{AppError, AppResult};
use crate::mcp::process::{ManagedChild, ProcessConfig, SHUTDOWN_GRACE, spawn};
use crate::mcp::protocol::{DEFAULT_REQUEST_TIMEOUT, connect};
use crate::mcp::stream::{StreamEvent, attach};
use crate::state::{AppEvent, AppState, LogStream, RunningServer, ServerId, ServerStatus};

/// After the grace-period hard kill, how long we wait for the exit waiter to
/// confirm death before giving up (it should be near-instant).
const KILL_CONFIRM_TIMEOUT: Duration = Duration::from_secs(5);

/// Bound on waiting for a cancelled start to settle: covers a SIGKILL, the
/// reap, and margin — never the 30 s handshake, which the cancel interrupts.
const CANCEL_SETTLE_TIMEOUT: Duration = Duration::from_secs(10);

/// How `run_startup` ended when it didn't fail: the server reached Running,
/// or a concurrent stop/remove cancelled the attempt (and any spawned child
/// is already dead).
enum StartOutcome {
    Running,
    Cancelled,
}

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
    state
        .with_db(move |conn| db::insert_server(conn, &new))
        .await
}

pub async fn update(state: &AppState, record: ServerRecord) -> AppResult<()> {
    let id = record.id;
    let old = state.with_db(move |conn| db::get_server(conn, id)).await?;
    // Secret keys whose marker disappears in this update would leave their
    // keyring entries orphaned — collect them now, delete after the DB write.
    let removed: Vec<String> = old
        .env
        .iter()
        .filter(|(key, value)| {
            **value == EnvValue::Secret && record.env.get(*key) != Some(&EnvValue::Secret)
        })
        .map(|(key, _)| key.clone())
        .collect();
    state
        .with_db(move |conn| db::update_server(conn, &record))
        .await?;
    if !removed.is_empty() {
        tokio::task::spawn_blocking(move || {
            crate::secrets::delete_keys_best_effort(id, removed.iter());
        })
        .await
        .map_err(|join| AppError::Io(std::io::Error::other(join)))?;
    }
    Ok(())
}

/// Stops the server first (cancelling an in-flight start if there is one),
/// deletes its keyring entries, then its config row. A start that read the
/// row before the delete registered itself before the delete too, so the
/// post-delete backstop stop catches it; one whose claim lands after the
/// delete fails `get_server` and self-cleans.
///
/// Consciously left open: a `set_secret` racing this can recreate a keyring
/// entry after cleanup — unreachable garbage (ids are never reused), same
/// class as pre-migration orphans.
pub async fn remove(state: &AppState, id: ServerId) -> AppResult<()> {
    stop(state, id).await?;
    let record = state.with_db(move |conn| db::get_server(conn, id)).await?;
    tokio::task::spawn_blocking(move || crate::secrets::delete_server_secrets(&record))
        .await
        .map_err(|join| AppError::Io(std::io::Error::other(join)))?;
    state
        .with_db(move |conn| db::delete_server(conn, id))
        .await?;
    stop(state, id).await
}

pub async fn start(state: &AppState, id: ServerId) -> AppResult<()> {
    start_with_timeout(state, id, DEFAULT_REQUEST_TIMEOUT).await
}

/// Launch-time sweep: start every server marked `auto_start`, concurrently.
/// Failures mark the individual server Errored (visible in the UI) but never
/// abort the sweep or the launch.
pub async fn start_auto_servers(state: &AppState) {
    let records = match state.with_db(db::list_servers).await {
        Ok(records) => records,
        Err(error) => {
            tracing::warn!(target: "app::lifecycle", %error, "auto-start sweep could not list servers");
            return;
        }
    };
    let starts = records
        .into_iter()
        .filter(|record| record.auto_start)
        .map(|record| {
            let state = state.clone();
            async move {
                if let Err(error) = start(&state, record.id).await {
                    tracing::warn!(
                        target: "app::lifecycle",
                        id = record.id,
                        name = %record.name,
                        %error,
                        "auto-start failed"
                    );
                }
            }
        });
    futures_join_all(starts).await;
}

/// Minimal join_all — awaiting each spawned handle in turn; the tasks run
/// concurrently from the moment they're spawned.
async fn futures_join_all<F: Future<Output = ()> + Send + 'static>(
    futures: impl Iterator<Item = F>,
) {
    let handles: Vec<_> = futures.map(tokio::spawn).collect();
    for handle in handles {
        let _ = handle.await;
    }
}

/// `request_timeout` also bounds the handshake — injectable for tests.
pub async fn start_with_timeout(
    state: &AppState,
    id: ServerId,
    request_timeout: Duration,
) -> AppResult<()> {
    let Some(guard) = state.try_begin_start(id) else {
        return Ok(()); // already starting or running — toggles are idempotent
    };
    // `guard` lives to the end of this scope: it settles (unblocking any
    // waiting stop/remove) only after the final status below is written.
    match run_startup(state, id, request_timeout, guard.token()).await {
        Ok(StartOutcome::Running) => Ok(()),
        Ok(StartOutcome::Cancelled) => {
            state.set_status(id, ServerStatus::Stopped);
            Ok(()) // the stop won; toggles are idempotent
        }
        Err(_) if guard.token().is_cancelled() => {
            // Teardown artifact of a concurrent stop — the stop's outcome
            // (Stopped) wins over the startup error.
            state.set_status(id, ServerStatus::Stopped);
            Ok(())
        }
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
    cancel: &CancellationToken,
) -> AppResult<StartOutcome> {
    // Cancellation points bracket every await: before the child exists a
    // cancel simply abandons the attempt; from spawn onward the select on
    // the handshake (which fires immediately if the token is already
    // cancelled) kills the tree. The kill handle stays local — this task is
    // the only owner of the child until the runtime is installed.
    let record = tokio::select! {
        _ = cancel.cancelled() => return Ok(StartOutcome::Cancelled),
        record = state.with_db(move |conn| db::get_server(conn, id)) => record?,
    };

    // Secret resolution hits the OS credential store (blocking) and happens
    // just-in-time — the resolved values exist only in the child's spawn
    // config, never in state, DB, or logs. An abandoned resolve finishes
    // harmlessly on the blocking pool.
    let env_record = record.clone();
    let resolve = tokio::task::spawn_blocking(move || crate::secrets::resolve_env(&env_record));
    let env = tokio::select! {
        _ = cancel.cancelled() => return Ok(StartOutcome::Cancelled),
        joined = resolve => joined.map_err(|join| AppError::Io(std::io::Error::other(join)))??,
    };

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
    let ManagedChild {
        pid,
        mut child,
        kill,
    } = managed;

    let proto = connect(stdin, streams.stdout, request_timeout);
    state.set_status(id, ServerStatus::Initializing);

    // Logs and notifications flow to the UI from here on — including
    // anything printed while the handshake is still in flight.
    tokio::spawn(fan_out_lines(
        state.clone(),
        id,
        LogStream::Stderr,
        streams.stderr,
    ));
    tokio::spawn(fan_out_lines(
        state.clone(),
        id,
        LogStream::Stdout,
        proto.stdout_logs,
    ));
    tokio::spawn(fan_out_notifications(
        state.clone(),
        id,
        proto.notifications,
    ));

    let handshake = tokio::select! {
        _ = cancel.cancelled() => {
            // Cancelled with a live child ⇒ tear the process tree down here;
            // no runtime exists yet, so nobody else can.
            kill.kill_now();
            let _ = tokio::time::timeout(KILL_CONFIRM_TIMEOUT, child.wait()).await;
            return Ok(StartOutcome::Cancelled);
        }
        handshake = proto.client.handshake() => match handshake {
            Ok(handshake) => handshake,
            Err(error) => {
                // Handshake failure/timeout ⇒ tear the process tree down.
                kill.kill_now();
                let _ = child.wait().await;
                return Err(error);
            }
        },
    };

    let stopping = Arc::new(AtomicBool::new(false));
    let (exit_tx, exit_rx) = watch::channel(false);
    let running = RunningServer {
        pid,
        client: proto.client,
        kill: kill.clone(),
        handshake,
        stopping: Arc::clone(&stopping),
        exited: exit_rx,
    };
    // Atomic promotion to Running: fails iff a concurrent stop/remove
    // cancelled the start after the handshake select — the child must die
    // and the cancelled entry must not be resurrected.
    if !state.try_install_runtime(id, running) {
        kill.kill_now();
        let _ = tokio::time::timeout(KILL_CONFIRM_TIMEOUT, child.wait()).await;
        return Ok(StartOutcome::Cancelled);
    }

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

    Ok(StartOutcome::Running)
}

/// Graceful stop per spec: signal the tree, wait the grace period, then hard
/// kill. The exit waiter settles the Stopped status; this returns once the
/// child is confirmed dead. A start still in flight is cancelled first — the
/// start task kills anything it spawned and writes the final status before
/// its guard settles, so the re-inspection below sees the true end state.
pub async fn stop(state: &AppState, id: ServerId) -> AppResult<()> {
    if let Some(start) = state.start_handle(id) {
        start.cancel.cancel();
        let mut settled = start.settled.clone();
        // A closed channel means the guard dropped — settled either way.
        let settle = tokio::time::timeout(CANCEL_SETTLE_TIMEOUT, settled.wait_for(|done| *done));
        if matches!(settle.await, Err(_elapsed)) {
            return Err(AppError::Timeout(format!(
                "cancelling start of server {id}"
            )));
        }
    }

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

    let store_key = key.clone();
    tokio::task::spawn_blocking(move || crate::secrets::store_secret(id, &store_key, &value))
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

    let delete_key = key.clone();
    tokio::task::spawn_blocking(move || crate::secrets::delete_secret(id, &delete_key))
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
    mut rx: mpsc::Receiver<crate::mcp::protocol::NotificationEvent>,
) {
    use crate::mcp::protocol::NotificationEvent;
    while let Some(event) = rx.recv().await {
        state.publish(match event {
            NotificationEvent::Frame(payload) => AppEvent::Notification { server_id, payload },
            NotificationEvent::Gap(dropped) => AppEvent::NotificationGap { server_id, dropped },
        });
    }
}
