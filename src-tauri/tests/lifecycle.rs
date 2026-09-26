//! End-to-end lifecycle tests over AppState: the full state machine, crash
//! detection, handshake-failure teardown, and log/notification fan-out.

#![cfg(unix)]

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use common::{add_fixture, add_fixture_auto, alive, test_state, wait_for};
use mcpanel_lib::commands::lifecycle;
use mcpanel_lib::db::NewServer;
use mcpanel_lib::state::{AppEvent, LogStream, ServerId, ServerStatus};

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

    // The UI saw the whole state machine, in order — and the handshake
    // lands right after Running, so the row can show identity without a
    // list round trip.
    let mut seen = Vec::new();
    let mut handshake_after_running = false;
    while let Ok(event) = events.try_recv() {
        match event {
            AppEvent::StatusChanged { server_id, status } => {
                assert_eq!(server_id, id);
                seen.push(status);
            }
            AppEvent::Handshake {
                server_id,
                handshake,
            } => {
                assert_eq!(server_id, id);
                assert_eq!(seen.last(), Some(&ServerStatus::Running));
                assert_eq!(handshake.server_info["name"], "mock-mcp-server");
                assert_eq!(handshake.protocol_version, "2025-06-18");
                handshake_after_running = true;
            }
            _ => {}
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
    assert!(handshake_after_running, "Handshake event follows Running");

    // And the overview carries it while running.
    let listed = lifecycle::list(&state).await.expect("list");
    let overview = listed.iter().find(|o| o.record.id == id).expect("listed");
    assert_eq!(
        overview
            .handshake
            .as_ref()
            .map(|h| h.protocol_version.as_str()),
        Some("2025-06-18")
    );

    // Idempotent start while running is a no-op.
    lifecycle::start(&state, id).await.expect("noop start");
    assert_eq!(state.status(id), ServerStatus::Running);

    lifecycle::stop(&state, id).await.expect("stop");
    wait_for("Stopped status", || {
        state.status(id) == ServerStatus::Stopped
    })
    .await;
    assert!(state.runtime(id).is_none(), "registry entry cleared");
    wait_for("process death", || !alive(runtime.pid as i32)).await;
}

#[tokio::test]
async fn handshake_failure_tears_down_and_marks_errored() {
    let state = test_state();
    let id = add_fixture(&state, "mute", &["--no-handshake"]).await;

    let err = lifecycle::start_with_timeout(&state, id, Some(Duration::from_millis(300)))
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

    // The crash cleared the runtime — no stale handles to a dead process.
    assert!(state.runtime(id).is_none(), "no runtime after a crash");

    // And stop resets the Errored entry to Stopped instead of signalling
    // the dead child's (possibly recycled) process group.
    lifecycle::stop(&state, id)
        .await
        .expect("stop crashed server");
    assert_eq!(state.status(id), ServerStatus::Stopped);
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
            AppEvent::Log {
                server_id,
                stream,
                line,
            } if server_id == id => match stream {
                LogStream::Stdout if line.contains("booting") => saw_stdout_garbage = true,
                LogStream::Stderr if line.contains("error: all good actually") => {
                    assert!(!line.contains('\x1b'), "ANSI reached the UI: {line:?}");
                    saw_stderr = true;
                }
                _ => {}
            },
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
async fn add_rejects_invalid_and_conflicting_configs() {
    use mcpanel_lib::error::AppError;
    let state = test_state();
    let base = |name: &str, command: &str, cwd: Option<&str>| NewServer {
        name: name.into(),
        command: command.into(),
        args: vec![],
        env: BTreeMap::new(),
        cwd: cwd.map(Into::into),
        auto_start: false,
        request_timeout_s: None,
        restart_on_crash: false,
    };

    let blank_name = lifecycle::add(&state, base("   ", "true", None)).await;
    assert!(matches!(blank_name, Err(AppError::InvalidInput(_))));

    let blank_command = lifecycle::add(&state, base("ok", "", None)).await;
    assert!(matches!(blank_command, Err(AppError::InvalidInput(_))));

    let ghost_cwd = lifecycle::add(&state, base("ok", "true", Some("/no/such/dir"))).await;
    assert!(matches!(ghost_cwd, Err(AppError::InvalidInput(_))));

    lifecycle::add(&state, base("taken", "true", None))
        .await
        .expect("valid config");
    let duplicate = lifecycle::add(&state, base("taken", "true", None)).await;
    assert!(matches!(duplicate, Err(AppError::Conflict(_))));
}

/// The single most-reported MCP setup failure is a command that isn't on
/// the PATH a GUI app inherits. It must fail *before* spawn with a message
/// that names the command, never a bare "os error 2".
#[tokio::test]
async fn missing_command_is_reported_by_name_before_spawn() {
    let state = test_state();
    let id = lifecycle::add(
        &state,
        NewServer {
            name: "ghost".into(),
            command: "mcpanel-no-such-command-zz".into(),
            args: vec![],
            env: BTreeMap::new(),
            cwd: None,
            auto_start: false,
            request_timeout_s: None,
            restart_on_crash: false,
        },
    )
    .await
    .expect("add")
    .id;

    let err = lifecycle::start(&state, id)
        .await
        .expect_err("cannot start");
    assert_eq!(err.code(), "spawn");
    let message = err.to_string();
    assert!(
        message.contains("command not found: mcpanel-no-such-command-zz"),
        "{message}"
    );
    assert!(!message.contains("os error"), "{message}");
    if let ServerStatus::Errored { message } = state.status(id) {
        assert!(message.contains("command not found"), "{message}");
    } else {
        panic!("expected Errored");
    }
}

/// Restart is stop-then-start as one operation: a fresh process, Running
/// again, and the old one gone.
#[tokio::test]
async fn restart_replaces_the_process() {
    let state = test_state();
    let id = add_fixture(&state, "reloaded", &[]).await;
    lifecycle::start(&state, id).await.expect("start");
    let old_pid = state.runtime(id).expect("runtime").pid as i32;

    lifecycle::restart(&state, id).await.expect("restart");
    assert_eq!(state.status(id), ServerStatus::Running);
    let new_pid = state.runtime(id).expect("runtime").pid as i32;
    assert_ne!(old_pid, new_pid);
    wait_for("old process death", || !alive(old_pid)).await;
    assert!(alive(new_pid));

    // Restarting a stopped server simply starts it.
    lifecycle::stop(&state, id).await.expect("stop");
    lifecycle::restart(&state, id)
        .await
        .expect("restart from stopped");
    assert_eq!(state.status(id), ServerStatus::Running);
    lifecycle::stop(&state, id).await.expect("stop");
}

#[tokio::test]
async fn start_unknown_server_fails_without_ghost_entry() {
    let state = test_state();
    let err = lifecycle::start(&state, 4242)
        .await
        .expect_err("unknown id");
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
            request_timeout_s: None,
            restart_on_crash: false,
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
        if let AppEvent::Log {
            server_id,
            stream: LogStream::Stdout,
            line,
        } = event
            && server_id == id
            && let Ok(pid) = line.trim().parse::<i32>()
        {
            return pid;
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
        lifecycle::start_with_timeout(&task_state, id, Some(Duration::from_secs(10))).await
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
        lifecycle::start_with_timeout(&task_state, id, Some(Duration::from_secs(10))).await
    });
    wait_for("Initializing status", || {
        state.status(id) == ServerStatus::Initializing
    })
    .await;
    let grandchild = grandchild_pid_from_logs(&mut events, id).await;

    lifecycle::remove(&state, id).await.expect("remove");

    start_task
        .await
        .expect("join")
        .expect("cancelled start is Ok");
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
                lifecycle::start_with_timeout(&start_state, id, Some(Duration::from_secs(10))).await
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
    // Lagging must not end the drain early — that would silently skip every
    // pid check below (`try_recv` reports a lag as `Err`).
    let mut grandchildren = Vec::new();
    loop {
        match events.try_recv() {
            Ok(AppEvent::Log {
                stream: LogStream::Stdout,
                line,
                ..
            }) => {
                if let Ok(pid) = line.trim().parse::<i32>() {
                    grandchildren.push(pid);
                }
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }
    for pid in grandchildren {
        wait_for("grandchild death", || !alive(pid)).await;
    }
}

/// A server marked restart_on_crash comes back on its own after a crash,
/// and the row says so while the timer runs.
#[tokio::test]
async fn crash_with_restart_on_crash_respawns_with_backoff() {
    let state = test_state();
    let id = lifecycle::add(
        &state,
        NewServer {
            restart_on_crash: true,
            ..common::fixture_server("phoenix", &[], false)
        },
    )
    .await
    .expect("add")
    .id;
    lifecycle::start(&state, id).await.expect("start");
    let first_pid = state.runtime(id).expect("runtime").pid as i32;

    unsafe { libc::kill(first_pid, libc::SIGKILL) };
    wait_for("restart scheduled", || {
        matches!(state.status(id), ServerStatus::Errored { ref message } if message.contains("restarting in 1 s (attempt 1 of"))
    })
    .await;
    assert!(state.restart_pending(id));

    wait_for("respawned", || state.status(id) == ServerStatus::Running).await;
    let second_pid = state.runtime(id).expect("runtime").pid as i32;
    assert_ne!(first_pid, second_pid);
    assert!(!state.restart_pending(id));

    // Crashing again within the healthy window continues the streak with a
    // longer delay.
    unsafe { libc::kill(second_pid, libc::SIGKILL) };
    wait_for("second restart scheduled", || {
        matches!(state.status(id), ServerStatus::Errored { ref message } if message.contains("restarting in 2 s (attempt 2 of"))
    })
    .await;

    // A deliberate stop while the timer runs wins: Stopped now, and no
    // resurrection after the delay would have elapsed.
    lifecycle::stop(&state, id).await.expect("stop");
    assert_eq!(state.status(id), ServerStatus::Stopped);
    assert!(!state.restart_pending(id));
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(state.status(id), ServerStatus::Stopped);
    assert!(state.runtime(id).is_none());
}

/// Without the flag a crash stays Errored, exactly as before.
#[tokio::test]
async fn crash_without_the_flag_schedules_nothing() {
    let state = test_state();
    let id = add_fixture(&state, "mortal", &[]).await;
    lifecycle::start(&state, id).await.expect("start");
    let pid = state.runtime(id).expect("runtime").pid as i32;
    unsafe { libc::kill(pid, libc::SIGKILL) };
    wait_for("Errored", || {
        matches!(state.status(id), ServerStatus::Errored { .. })
    })
    .await;
    assert!(!state.restart_pending(id));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(matches!(state.status(id), ServerStatus::Errored { .. }));
    lifecycle::stop(&state, id).await.expect("clear");
}

#[test]
fn restart_delay_backs_off_exponentially_and_caps() {
    assert_eq!(lifecycle::restart_delay(1), Duration::from_secs(1));
    assert_eq!(lifecycle::restart_delay(2), Duration::from_secs(2));
    assert_eq!(lifecycle::restart_delay(3), Duration::from_secs(4));
    assert_eq!(lifecycle::restart_delay(5), Duration::from_secs(16));
    assert_eq!(lifecycle::restart_delay(6), Duration::from_secs(30));
    assert_eq!(lifecycle::restart_delay(60), Duration::from_secs(30));
}

/// The configured timeout bounds the handshake: a mute server fails in
/// about the configured time, not the 30 s default. And an out-of-range
/// timeout is rejected at add time.
#[tokio::test]
async fn configured_timeout_bounds_the_handshake_and_is_validated() {
    use mcpanel_lib::error::AppError;
    let state = test_state();
    let id = lifecycle::add(
        &state,
        NewServer {
            request_timeout_s: Some(1),
            ..common::fixture_server("slowpoke", &["--no-handshake"], false)
        },
    )
    .await
    .expect("add")
    .id;
    let started = std::time::Instant::now();
    let err = lifecycle::start(&state, id).await.expect_err("mute server");
    assert!(matches!(err, AppError::Timeout(_)));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "honoured the 1 s timeout"
    );
    lifecycle::stop(&state, id).await.expect("clear");

    let too_big = lifecycle::add(
        &state,
        NewServer {
            request_timeout_s: Some(lifecycle::MAX_SERVER_TIMEOUT_S + 1),
            ..common::fixture_server("greedy", &[], false)
        },
    )
    .await;
    assert!(matches!(too_big, Err(AppError::InvalidInput(_))));
    let zero = lifecycle::add(
        &state,
        NewServer {
            request_timeout_s: Some(0),
            ..common::fixture_server("zero", &[], false)
        },
    )
    .await;
    assert!(matches!(zero, Err(AppError::InvalidInput(_))));
}

/// A deliberate start after the streak gave up (or mid-streak) resets the
/// count: the user took over, so the next crash starts again from one.
#[tokio::test]
async fn manual_start_resets_the_restart_streak() {
    let state = test_state();
    let id = lifecycle::add(
        &state,
        NewServer {
            restart_on_crash: true,
            ..common::fixture_server("phoenix2", &[], false)
        },
    )
    .await
    .expect("add")
    .id;
    lifecycle::start(&state, id).await.expect("start");
    let pid = state.runtime(id).expect("runtime").pid as i32;
    unsafe { libc::kill(pid, libc::SIGKILL) };
    wait_for("attempt 1 scheduled", || {
        matches!(state.status(id), ServerStatus::Errored { ref message } if message.contains("attempt 1 of"))
    })
    .await;

    // Take over while the timer runs: the pending restart is cancelled and
    // the streak forgotten.
    lifecycle::start(&state, id).await.expect("manual start");
    assert_eq!(state.status(id), ServerStatus::Running);
    assert!(!state.restart_pending(id));
    let pid = state.runtime(id).expect("runtime").pid as i32;
    unsafe { libc::kill(pid, libc::SIGKILL) };
    wait_for("streak restarted from one", || {
        matches!(state.status(id), ServerStatus::Errored { ref message } if message.contains("attempt 1 of"))
    })
    .await;
    lifecycle::stop(&state, id).await.expect("stop");
    assert_eq!(state.status(id), ServerStatus::Stopped);
}
