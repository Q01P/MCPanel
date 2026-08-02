//! End-to-end lifecycle tests over AppState: the full state machine, crash
//! detection, handshake-failure teardown, and log/notification fan-out.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::time::Duration;

use mcpanel_lib::commands::lifecycle;
use mcpanel_lib::db::{self, NewServer};
use mcpanel_lib::state::{AppEvent, AppState, LogStream, ServerId, ServerStatus};

fn test_state() -> AppState {
    AppState::new(db::open_in_memory().expect("in-memory db"))
}

async fn add_fixture(state: &AppState, name: &str, args: &[&str]) -> ServerId {
    lifecycle::add(
        state,
        NewServer {
            name: name.into(),
            command: env!("CARGO_BIN_EXE_mock-mcp-server").into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            env: BTreeMap::new(),
            cwd: None,
            auto_start: false,
        },
    )
    .await
    .expect("insert server")
    .id
}

fn alive(pid: i32) -> bool {
    if unsafe { libc::kill(pid, 0) } != 0 {
        return false;
    }
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let state = stat
        .rfind(')')
        .and_then(|i| stat[i + 1..].trim_start().chars().next());
    !matches!(state, Some('Z') | Some('X') | None)
}

async fn wait_for<F: Fn() -> bool>(what: &str, cond: F) {
    for _ in 0..100 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn full_lifecycle_walks_the_state_machine() {
    let state = test_state();
    let mut events = state.subscribe();
    let id = add_fixture(&state, "happy", &[]).await;

    lifecycle::start(&state, id).await.expect("start");
    assert_eq!(state.status(id), ServerStatus::Running);

    let runtime = state.runtime(id).expect("runtime installed");
    assert_eq!(runtime.handshake.server_info["name"], "mock-mcp-server");
    assert!(alive(runtime.pid as i32), "server process running");

    // The UI saw the whole state machine, in order.
    let mut seen = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let AppEvent::StatusChanged { server_id, status } = event {
            assert_eq!(server_id, id);
            seen.push(status);
        }
    }
    assert_eq!(
        seen,
        vec![
            ServerStatus::Starting,
            ServerStatus::Initializing,
            ServerStatus::Running,
        ]
    );

    // Idempotent start while running is a no-op.
    lifecycle::start(&state, id).await.expect("noop start");
    assert_eq!(state.status(id), ServerStatus::Running);

    lifecycle::stop(&state, id).await.expect("stop");
    wait_for("Stopped status", || state.status(id) == ServerStatus::Stopped).await;
    assert!(state.runtime(id).is_none(), "registry entry cleared");
    wait_for("process death", || !alive(runtime.pid as i32)).await;
}

#[tokio::test]
async fn handshake_failure_tears_down_and_marks_errored() {
    let state = test_state();
    let id = add_fixture(&state, "mute", &["--no-handshake"]).await;

    let err = lifecycle::start_with_timeout(&state, id, Duration::from_millis(300))
        .await
        .expect_err("handshake must time out");
    assert!(matches!(err, mcpanel_lib::error::AppError::Timeout(_)));
    assert!(matches!(state.status(id), ServerStatus::Errored { .. }));
    assert!(state.runtime(id).is_none(), "no runtime for a failed start");

    // Stop clears the Errored entry back to Stopped, and a restart is allowed
    // (it fails the same way, but the guard lets it through).
    lifecycle::stop(&state, id).await.expect("clear errored");
    assert_eq!(state.status(id), ServerStatus::Stopped);
}

#[tokio::test]
async fn crash_is_reported_as_errored() {
    let state = test_state();
    let id = add_fixture(&state, "crashy", &[]).await;
    lifecycle::start(&state, id).await.expect("start");

    let pid = state.runtime(id).expect("runtime").pid as i32;
    unsafe { libc::kill(pid, libc::SIGKILL) };

    wait_for("Errored status", || {
        matches!(state.status(id), ServerStatus::Errored { .. })
    })
    .await;
    if let ServerStatus::Errored { message } = state.status(id) {
        assert!(message.contains("unexpectedly"), "got: {message}");
    }
}

#[tokio::test]
async fn logs_and_notifications_fan_out_to_app_events() {
    let state = test_state();
    let mut events = state.subscribe();
    let id = add_fixture(&state, "noisy", &["--garbage", "--ansi", "--notify"]).await;
    lifecycle::start(&state, id).await.expect("start");

    let mut saw_stdout_garbage = false;
    let mut saw_stderr = false;
    let mut saw_notification = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while !(saw_stdout_garbage && saw_stderr && saw_notification) {
        let event = tokio::time::timeout_at(deadline, events.recv())
            .await
            .expect("events within deadline")
            .expect("event channel open");
        match event {
            AppEvent::Log { server_id, stream, line } if server_id == id => {
                match stream {
                    LogStream::Stdout if line.contains("booting") => saw_stdout_garbage = true,
                    LogStream::Stderr if line.contains("error: all good actually") => {
                        assert!(!line.contains('\x1b'), "ANSI reached the UI: {line:?}");
                        saw_stderr = true;
                    }
                    _ => {}
                }
            }
            AppEvent::Notification { server_id, payload } if server_id == id => {
                assert_eq!(payload["method"], "notifications/message");
                saw_notification = true;
            }
            _ => {}
        }
    }

    lifecycle::stop(&state, id).await.expect("stop");
}

#[tokio::test]
async fn start_unknown_server_fails_without_ghost_entry() {
    let state = test_state();
    let err = lifecycle::start(&state, 4242).await.expect_err("unknown id");
    assert!(matches!(
        err,
        mcpanel_lib::error::AppError::ServerNotFound(_)
    ));
    assert_eq!(state.status(4242), ServerStatus::Stopped);
}

#[tokio::test]
async fn remove_stops_then_deletes() {
    let state = test_state();
    let id = add_fixture(&state, "doomed", &[]).await;
    lifecycle::start(&state, id).await.expect("start");
    let pid = state.runtime(id).expect("runtime").pid as i32;

    lifecycle::remove(&state, id).await.expect("remove");
    wait_for("process death", || !alive(pid)).await;
    assert!(lifecycle::list(&state).await.expect("list").is_empty());
}
