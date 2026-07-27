//! Flagship orphan tests (spec §4): controlled shutdown kills the whole tree;
//! SIGKILLing the supervisor still kills the server (PDEATHSIG).

#![cfg(unix)]

use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};

use mcpanel_lib::mcp::process::{ProcessConfig, spawn};

fn fixture_path() -> String {
    env!("CARGO_BIN_EXE_mock-mcp-server").to_string()
}

/// Liveness check that dodges zombie false-positives: on Linux, a pid whose
/// /proc stat state is Z (zombie) or X (dead) counts as dead.
fn alive(pid: i32) -> bool {
    if unsafe { libc::kill(pid, 0) } != 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        // State is the first field after the parenthesized comm (which may
        // itself contain spaces or parens).
        let state = stat
            .rfind(')')
            .and_then(|i| stat[i + 1..].trim_start().chars().next());
        !matches!(state, Some('Z') | Some('X') | None)
    }
    #[cfg(not(target_os = "linux"))]
    true
}

async fn wait_dead(pid: i32) -> bool {
    for _ in 0..100 {
        if !alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// Spawn the fixture with `--spawn-child` through the supervisor and return
/// (managed child, grandchild pid read from the fixture's stdout).
async fn spawn_tree() -> (mcpanel_lib::mcp::process::ManagedChild, i32) {
    let mut managed = spawn(&ProcessConfig {
        command: fixture_path(),
        args: vec!["--spawn-child".into()],
        ..Default::default()
    })
    .expect("spawn fixture");

    let stdout = managed.child.stdout.take().expect("piped stdout");
    let mut lines = BufReader::new(stdout).lines();
    let grandchild: i32 = lines
        .next_line()
        .await
        .expect("read grandchild pid")
        .expect("fixture printed a pid line")
        .trim()
        .parse()
        .expect("pid parses");

    (managed, grandchild)
}

#[tokio::test]
async fn controlled_shutdown_kills_the_whole_tree() {
    let (mut managed, grandchild) = spawn_tree().await;
    let server_pid = managed.pid as i32;
    assert!(alive(server_pid), "server should be running");
    assert!(alive(grandchild), "grandchild should be running");

    managed.shutdown().await.expect("shutdown");

    assert!(wait_dead(server_pid).await, "server survived shutdown");
    assert!(
        wait_dead(grandchild).await,
        "grandchild survived shutdown — tree kill failed"
    );
}

#[tokio::test]
async fn kill_now_kills_the_whole_tree() {
    let (mut managed, grandchild) = spawn_tree().await;
    let server_pid = managed.pid as i32;

    managed.kill.kill_now();
    managed.child.wait().await.expect("reap");

    assert!(wait_dead(server_pid).await, "server survived kill_now");
    assert!(
        wait_dead(grandchild).await,
        "grandchild survived kill_now — tree kill failed"
    );
}

/// Crash half: SIGKILL the harness (the "supervisor") so no cleanup code runs;
/// the server must die anyway via PDEATHSIG. Linux-only by design.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn server_dies_when_supervisor_is_sigkilled() {
    let mut harness = tokio::process::Command::new(env!("CARGO_BIN_EXE_orphan-harness"))
        .arg(fixture_path())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn harness");

    let stdout = harness.stdout.take().expect("piped stdout");
    let mut lines = BufReader::new(stdout).lines();
    let server_pid: i32 = lines
        .next_line()
        .await
        .expect("read server pid")
        .expect("harness printed a pid line")
        .trim()
        .parse()
        .expect("pid parses");
    assert!(alive(server_pid), "server should be running");

    let harness_pid = harness.id().expect("harness pid") as i32;
    unsafe { libc::kill(harness_pid, libc::SIGKILL) };
    harness.wait().await.expect("reap harness");

    assert!(
        wait_dead(server_pid).await,
        "server survived supervisor SIGKILL — PDEATHSIG failed"
    );
}
