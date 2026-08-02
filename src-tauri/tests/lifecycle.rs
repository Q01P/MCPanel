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
    add_fixture_auto(state, name, args, false).await
}

async fn add_fixture_auto(
    state: &AppState,
    name: &str,
    args: &[&str],
    auto_start: bool,
) -> ServerId {
    lifecycle::add(
        state,
        NewServer {
            name: name.into(),
            command: env!("CARGO_BIN_EXE_mock-mcp-server").into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            env: BTreeMap::new(),
            cwd: None,
            auto_start,
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

/// The launch sweep starts exactly the servers marked `auto_start`; a
/// failing one goes Errored without derailing the others.
#[tokio::test]
async fn auto_start_sweep_starts_only_marked_servers() {
    let state = test_state();
    let auto = add_fixture_auto(&state, "auto", &[], true).await;
    let manual = add_fixture_auto(&state, "manual", &[], false).await;
    let broken = lifecycle::add(
        &state,
        NewServer {
            name: "auto-broken".into(),
            command: "/nonexistent/binary".into(),
            args: vec![],
            env: BTreeMap::new(),
            cwd: None,
            auto_start: true,
        },
    )
    .await
    .expect("insert broken server")
    .id;

    lifecycle::start_auto_servers(&state).await;

    assert_eq!(state.status(auto), ServerStatus::Running);
    assert_eq!(state.status(manual), ServerStatus::Stopped);
    assert!(
        matches!(state.status(broken), ServerStatus::Errored { .. }),
        "broken auto-start server is Errored, not fatal"
    );

    lifecycle::stop(&state, auto).await.expect("cleanup");
}

/// The `--spawn-child --no-handshake` fixture prints its grandchild's pid to
/// stdout (a `Log` event) and then never answers `initialize` — a start that
/// hangs in `Initializing` with a real process tree to kill.
async fn grandchild_pid_from_logs(
    events: &mut tokio::sync::broadcast::Receiver<AppEvent>,
    id: ServerId,
) -> i32 {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let event = tokio::time::timeout_at(deadline, events.recv())
            .await
            .expect("grandchild pid within deadline")
            .expect("event channel open");
        if let AppEvent::Log { server_id, stream: LogStream::Stdout, line } = event {
            if server_id == id {
                if let Ok(pid) = line.trim().parse::<i32>() {
                    return pid;
                }
            }
        }
    }
}

/// Stop during Initializing cancels the hung handshake, kills the process
/// tree, and settles Stopped — the stop must not be silently lost.
#[tokio::test]
async fn stop_cancels_start_during_initializing() {
    let state = test_state();
    let mut events = state.subscribe();
    let id = add_fixture(&state, "hung", &["--spawn-child", "--no-handshake"]).await;

    let task_state = state.clone();
    let start_task = tokio::spawn(async move {
        lifecycle::start_with_timeout(&task_state, id, Duration::from_secs(10)).await
    });
    wait_for("Initializing status", || {
        state.status(id) == ServerStatus::Initializing
    })
    .await;
    let grandchild = grandchild_pid_from_logs(&mut events, id).await;

    let stopped_at = tokio::time::Instant::now();
    lifecycle::stop(&state, id).await.expect("cancel via stop");
    assert!(
        stopped_at.elapsed() < Duration::from_secs(5),
        "stop must not ride out the handshake timeout"
    );

    start_task
        .await
        .expect("join")
        .expect("cancelled start returns Ok — the stop won");
    assert_eq!(state.status(id), ServerStatus::Stopped);
    assert!(state.runtime(id).is_none(), "no runtime after cancel");
    wait_for("grandchild death", || !alive(grandchild)).await;
}

/// Remove during an in-flight start cancels it before deleting the row — no
/// orphaned process, no ghost registry entry, no leftover config.
#[tokio::test]
async fn remove_while_starting_leaves_no_process_and_no_row() {
    let state = test_state();
    let mut events = state.subscribe();
    let id = add_fixture(&state, "doomed-early", &["--spawn-child", "--no-handshake"]).await;

    let task_state = state.clone();
    let start_task = tokio::spawn(async move {
        lifecycle::start_with_timeout(&task_state, id, Duration::from_secs(10)).await
    });
    wait_for("Initializing status", || {
        state.status(id) == ServerStatus::Initializing
    })
    .await;
    let grandchild = grandchild_pid_from_logs(&mut events, id).await;

    lifecycle::remove(&state, id).await.expect("remove");

    start_task.await.expect("join").expect("cancelled start is Ok");
    assert!(lifecycle::list(&state).await.expect("list").is_empty());
    assert_eq!(state.status(id), ServerStatus::Stopped);
    wait_for("grandchild death", || !alive(grandchild)).await;
}

/// start and remove racing from the first instruction: whatever the
/// interleaving, the end state is clean — no row, no registry entry, no
/// surviving process.
#[tokio::test]
async fn concurrent_start_and_remove_settle_clean() {
    let state = test_state();
    let mut events = state.subscribe();

    for round in 0..5 {
        let id = add_fixture(
            &state,
            &format!("racer-{round}"),
            &["--spawn-child", "--no-handshake"],
        )
        .await;

        let start_state = state.clone();
        let remove_state = state.clone();
        let (start_result, remove_result) = tokio::join!(
            tokio::spawn(async move {
                lifecycle::start_with_timeout(&start_state, id, Duration::from_secs(10)).await
            }),
            tokio::spawn(async move { lifecycle::remove(&remove_state, id).await }),
        );

        remove_result.expect("join").expect("remove succeeds");
        match start_result.expect("join") {
            // Cancelled mid-flight (Ok) or lost the row race entirely.
            Ok(()) => {}
            Err(mcpanel_lib::error::AppError::ServerNotFound(_)) => {}
            Err(other) => panic!("round {round}: unexpected start error: {other:?}"),
        }

        assert!(
            lifecycle::list(&state).await.expect("list").is_empty(),
            "round {round}: row must be gone"
        );
        assert_eq!(
            state.status(id),
            ServerStatus::Stopped,
            "round {round}: no ghost registry entry"
        );
    }

    // Any grandchild whose pid made it into the log stream must be dead.
    let mut grandchildren = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let AppEvent::Log { stream: LogStream::Stdout, line, .. } = event {
            if let Ok(pid) = line.trim().parse::<i32>() {
                grandchildren.push(pid);
            }
        }
    }
    for pid in grandchildren {
        wait_for("grandchild death", || !alive(pid)).await;
    }
}
