//! Protocol tests against the fixture (spec §4): handshake happy path and
//! timeout, JSON-RPC error surfacing, clean failure after stop, notification
//! fan-out, garbage-tolerant stdout routing.

#![cfg(unix)]

use std::time::Duration;

use serde_json::json;

use mcpanel_lib::error::AppError;
use mcpanel_lib::mcp::process::{ManagedChild, ProcessConfig, spawn};
use mcpanel_lib::mcp::protocol::{PROTOCOL_VERSION, ProtocolHandle, connect};
use mcpanel_lib::mcp::stream::{StreamEvent, attach};

fn spawn_connected(args: &[&str]) -> (ManagedChild, ProtocolHandle) {
    let mut managed = spawn(&ProcessConfig {
        command: env!("CARGO_BIN_EXE_mock-mcp-server").to_string(),
        args: args.iter().map(|a| a.to_string()).collect(),
        ..Default::default()
    })
    .expect("spawn fixture");
    let streams = attach(&mut managed.child).expect("attach streams");
    let stdin = managed.child.stdin.take().expect("piped stdin");
    let handle = connect(stdin, streams.stdout, Duration::from_millis(500));
    (managed, handle)
}

#[tokio::test]
async fn handshake_happy_path_then_requests() {
    let (mut managed, handle) = spawn_connected(&[]);

    let hs = handle.client.handshake().await.expect("handshake");
    assert_eq!(hs.protocol_version, PROTOCOL_VERSION);
    assert_eq!(hs.server_info["name"], "mock-mcp-server");
    assert!(hs.capabilities.get("tools").is_some(), "capabilities captured");

    let pong = handle.client.request("ping", json!({})).await.expect("ping");
    assert_eq!(pong, json!({}));

    let tools = handle
        .client
        .request("tools/list", json!({}))
        .await
        .expect("tools/list");
    assert_eq!(tools["tools"][0]["name"], "echo");

    managed.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn handshake_timeout_with_unresponsive_server() {
    let (mut managed, handle) = spawn_connected(&["--no-handshake"]);

    let err = handle.client.handshake().await.expect_err("must time out");
    assert!(
        matches!(err, AppError::Timeout(ref m) if m == "initialize"),
        "expected Timeout(initialize), got {err:?}"
    );

    // Caller contract on handshake failure: tear the tree down (T7 marks
    // the server Errored).
    managed.kill.kill_now();
    managed.child.wait().await.expect("reap");
}

#[tokio::test]
async fn jsonrpc_errors_surface_as_rpc_variant() {
    let (mut managed, handle) = spawn_connected(&[]);
    handle.client.handshake().await.expect("handshake");

    let err = handle
        .client
        .request("bogus/method", json!({}))
        .await
        .expect_err("unknown method must error");
    assert!(
        matches!(err, AppError::Rpc { code: -32601, .. }),
        "expected Rpc(-32601), got {err:?}"
    );

    managed.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn requests_fail_cleanly_after_stop() {
    let (mut managed, handle) = spawn_connected(&[]);
    handle.client.handshake().await.expect("handshake");

    managed.shutdown().await.expect("shutdown");

    // The router needs a moment to drain stdout EOF and fail the client.
    let mut last = None;
    for _ in 0..40 {
        match handle.client.request("ping", json!({})).await {
            Err(AppError::ConnectionClosed) => return,
            other => last = Some(other),
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("request never failed with ConnectionClosed; last: {last:?}");
}

#[tokio::test]
async fn server_notifications_fan_out() {
    let (mut managed, mut handle) = spawn_connected(&["--notify"]);
    handle.client.handshake().await.expect("handshake");

    let notification = tokio::time::timeout(Duration::from_secs(2), handle.notifications.recv())
        .await
        .expect("notification within 2s")
        .expect("channel open");
    assert_eq!(notification["method"], "notifications/message");
    assert_eq!(notification["params"]["data"], "hello from mock");

    managed.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn garbage_stdout_falls_back_to_logs_while_protocol_works() {
    let (mut managed, mut handle) = spawn_connected(&["--garbage"]);

    // Handshake succeeds despite the junk printed before serving…
    handle.client.handshake().await.expect("handshake");
    managed.shutdown().await.expect("shutdown");

    // …and the junk landed in the log fallback instead of being lost.
    let mut log_lines = Vec::new();
    while let Some(event) = handle.stdout_logs.recv().await {
        if let StreamEvent::Line(line) = event {
            log_lines.push(line.to_string());
        }
    }
    assert!(
        log_lines.iter().any(|l| l == "mock-mcp-server booting..."),
        "boot line missing from log fallback: {log_lines:?}"
    );
}
