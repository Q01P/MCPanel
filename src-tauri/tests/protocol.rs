//! Protocol tests against the fixture (spec §4): handshake happy path and
//! timeout, JSON-RPC error surfacing, clean failure after stop, notification
//! fan-out, garbage-tolerant stdout routing.

#![cfg(unix)]

use std::time::Duration;

use serde_json::json;

use mcpanel_lib::error::AppError;
use mcpanel_lib::mcp::process::{ManagedChild, ProcessConfig, spawn};
use mcpanel_lib::mcp::protocol::{NotificationEvent, PROTOCOL_VERSION, ProtocolHandle, connect};
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
    let NotificationEvent::Frame(payload) = notification else {
        panic!("expected a frame, got {notification:?}");
    };
    assert_eq!(payload["method"], "notifications/message");
    assert_eq!(payload["params"]["data"], "hello from mock");

    managed.shutdown().await.expect("shutdown");
}

/// MCP `ping` expects an empty result; a server probing liveness must not
/// get `-32601` back. The fixture confirms the pong on stderr.
#[tokio::test]
async fn server_initiated_ping_gets_empty_result() {
    let mut managed = spawn(&ProcessConfig {
        command: env!("CARGO_BIN_EXE_mock-mcp-server").to_string(),
        args: vec!["--ping-client".into()],
        ..Default::default()
    })
    .expect("spawn fixture");
    let streams = attach(&mut managed.child).expect("attach streams");
    let stdin = managed.child.stdin.take().expect("piped stdin");
    let mut stderr = streams.stderr;
    let handle = connect(stdin, streams.stdout, Duration::from_millis(500));

    handle.client.handshake().await.expect("handshake");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let event = tokio::time::timeout_at(deadline, stderr.recv())
            .await
            .expect("pong confirmation within 2s")
            .expect("stderr open");
        if let StreamEvent::Line(line) = event {
            if line.contains("client answered ping") {
                break;
            }
        }
    }

    managed.shutdown().await.expect("shutdown");
}

/// A server negotiating a protocol revision we don't speak is disconnected
/// (spec behavior), not silently accepted.
#[tokio::test]
async fn handshake_rejects_unsupported_protocol_version() {
    let (mut managed, handle) = spawn_connected(&["--wrong-version"]);

    let err = handle.client.handshake().await.expect_err("must reject");
    assert!(
        matches!(err, AppError::Handshake(ref m) if m.contains("1999-01-01")),
        "expected Handshake mentioning the bad version, got {err:?}"
    );

    managed.kill.kill_now();
    managed.child.wait().await.expect("reap");
}

/// Flooding past the 256-slot advisory channel loses notifications — but
/// the loss is *accounted*: frames received + gap totals must equal what the
/// server sent.
#[tokio::test]
async fn notification_overflow_is_surfaced_as_gap() {
    let (mut managed, mut handle) = spawn_connected(&["--notify-flood"]);
    handle.client.handshake().await.expect("handshake");

    // Let the router chew through the whole flood while nobody drains.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut frames = 0u64;
    let mut gaps = 0u64;
    // Drain what's buffered (making room for the EOF gap flush)…
    loop {
        match tokio::time::timeout(Duration::from_millis(300), handle.notifications.recv()).await {
            Ok(Some(NotificationEvent::Frame(_))) => frames += 1,
            Ok(Some(NotificationEvent::Gap(n))) => gaps += n,
            Ok(None) | Err(_) => break,
        }
    }
    // …then EOF flushes whatever loss was still pending.
    managed.shutdown().await.expect("shutdown");
    while let Some(event) = handle.notifications.recv().await {
        match event {
            NotificationEvent::Frame(_) => frames += 1,
            NotificationEvent::Gap(n) => gaps += n,
        }
    }

    assert!(gaps > 0, "flood must overflow the advisory channel");
    assert_eq!(
        frames + gaps,
        400,
        "every notification is either delivered or accounted for"
    );
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
