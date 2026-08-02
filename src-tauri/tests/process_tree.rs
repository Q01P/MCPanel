//! Flagship orphan tests (spec §4): controlled shutdown kills the whole tree;
//! SIGKILLing the supervisor still kills the server (PDEATHSIG).

#![cfg(unix)]

mod common;

#[cfg(target_os = "linux")]
use std::process::Stdio;

use common::{alive, spawn_tree, wait_for};
#[cfg(target_os = "linux")]
use common::fixture_path;
#[cfg(target_os = "linux")]
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::test]
async fn controlled_shutdown_kills_the_whole_tree() {
    let (mut managed, grandchild) = spawn_tree().await;
    let server_pid = managed.pid as i32;
    assert!(alive(server_pid), "server should be running");
    assert!(alive(grandchild), "grandchild should be running");

    managed.shutdown().await.expect("shutdown");

    wait_for("server death after shutdown", || !alive(server_pid)).await;
    wait_for("grandchild death after shutdown (tree kill)", || {
        !alive(grandchild)
    })
    .await;
}

#[tokio::test]
async fn kill_now_kills_the_whole_tree() {
    let (mut managed, grandchild) = spawn_tree().await;
    let server_pid = managed.pid as i32;

    managed.kill.kill_now();
    managed.child.wait().await.expect("reap");

    wait_for("server death after kill_now", || !alive(server_pid)).await;
    wait_for("grandchild death after kill_now (tree kill)", || {
        !alive(grandchild)
    })
    .await;
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

    wait_for("server death after supervisor SIGKILL (PDEATHSIG)", || {
        !alive(server_pid)
    })
    .await;
}
