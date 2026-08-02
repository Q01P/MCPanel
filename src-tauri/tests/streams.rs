//! Stream pipeline tests against the real fixture: ANSI stripping, garbage
//! tolerance, and flood boundedness (spec §4).

#![cfg(unix)]

mod common;

use common::spawn_fixture;
use mcpanel_lib::mcp::stream::{CHANNEL_CAPACITY, StreamEvent};

async fn next_line(rx: &mut tokio::sync::mpsc::Receiver<StreamEvent>) -> String {
    loop {
        match rx.recv().await.expect("stream ended early") {
            StreamEvent::Line(line) => return line.to_string(),
            StreamEvent::Dropped(_) => continue,
        }
    }
}

#[tokio::test]
async fn stderr_ansi_is_stripped() {
    let (mut managed, mut streams) = spawn_fixture(&["--ansi"]);
    let line = next_line(&mut streams.stderr).await;
    assert!(!line.contains('\x1b'), "ANSI escape survived: {line:?}");
    assert_eq!(line, "error: all good actually underlined");

    drop(managed.child.stdin.take()); // EOF → fixture exits
    managed.child.wait().await.expect("reap");
}

#[tokio::test]
async fn stdout_garbage_bytes_are_stripped() {
    let (mut managed, mut streams) = spawn_fixture(&["--garbage"]);
    assert_eq!(next_line(&mut streams.stdout).await, "mock-mcp-server booting...");
    let garbage_line = next_line(&mut streams.stdout).await;
    assert!(
        garbage_line.chars().all(|c| !c.is_control()),
        "control bytes survived: {garbage_line:?}"
    );
    assert_eq!(garbage_line, " pre-JSON garbage bytes");

    drop(managed.child.stdin.take());
    managed.child.wait().await.expect("reap");
}

/// Flood-proofing: a server spamming stdout as fast as possible must not
/// grow memory beyond the bounded channel — excess lines are dropped and
/// accounted for in a gap marker.
#[tokio::test]
async fn flooding_server_is_bounded_not_buffered() {
    let (mut managed, mut streams) = spawn_fixture(&["--spam"]);

    // Let it flood without draining until the channel is genuinely full (a
    // fixed sleep flakes on starved runners), then one beat more so at least
    // one further send fails and is counted as dropped.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while streams.stdout.len() < streams.stdout.max_capacity() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "spam never filled the channel"
        );
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    managed.kill.kill_now();
    managed.child.wait().await.expect("reap");

    let mut lines = 0usize;
    let mut dropped = 0u64;
    while let Some(event) = streams.stdout.recv().await {
        match event {
            StreamEvent::Line(_) => lines += 1,
            StreamEvent::Dropped(n) => dropped += n,
        }
    }
    assert!(
        lines <= CHANNEL_CAPACITY + 1,
        "buffered {lines} lines — bound not enforced"
    );
    assert!(dropped > 0, "a 300ms flood should overflow the channel");
}

