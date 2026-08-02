//! Full-stack gateway test: a real fixture server driven end-to-end through
//! `POST /mcp/{server_id}` (spec §3 gateway).

#![cfg(unix)]

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{TEST_HOST, add_fixture, test_gateway, wait_for};
use serde_json::{Value, json};
use tower::ServiceExt;

use mcpanel_lib::commands::lifecycle;
use mcpanel_lib::server::{Gateway, router};
use mcpanel_lib::state::ServerStatus;

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .expect("read body");
    serde_json::from_slice(&bytes).expect("json body")
}

fn post(gateway: &Gateway, server_id: i64, payload: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("/mcp/{server_id}"))
        .header(header::HOST, TEST_HOST)
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", gateway.token.expose()),
        )
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload.to_string()))
        .unwrap()
}

#[tokio::test]
async fn forwards_jsonrpc_to_a_running_server() {
    let gateway = test_gateway();
    let id = add_fixture(&gateway.app, "gw", &[]).await;
    lifecycle::start(&gateway.app, id).await.expect("start");

    // Request: caller id echoed, result forwarded.
    let response = router(gateway.clone())
        .oneshot(post(&gateway, id, json!({"jsonrpc":"2.0","id":7,"method":"ping","params":{}})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await,
        json!({"jsonrpc":"2.0","id":7,"result":{}})
    );

    // RPC errors come back as JSON-RPC envelopes, not HTTP errors.
    let response = router(gateway.clone())
        .oneshot(post(&gateway, id, json!({"id":8,"method":"bogus/method"})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let error_body = body_json(response).await;
    assert_eq!(error_body["id"], 8);
    assert_eq!(error_body["error"]["code"], -32601);

    // Notifications (no id) are accepted fire-and-forget.
    let response = router(gateway.clone())
        .oneshot(post(&gateway, id, json!({"method":"notifications/whatever"})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, json!({"accepted": true}));

    lifecycle::stop(&gateway.app, id).await.expect("stop");

    // Stopped server → 404 on the same route.
    let response = router(gateway.clone())
        .oneshot(post(&gateway, id, json!({"id":9,"method":"ping"})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

/// A crashed server must 404 like a stopped one — not forward to the dead
/// child's stdin through a stale runtime.
#[tokio::test]
async fn crashed_server_returns_not_found() {
    let gateway = test_gateway();
    let id = add_fixture(&gateway.app, "gw-crash", &[]).await;
    lifecycle::start(&gateway.app, id).await.expect("start");

    let pid = gateway.app.runtime(id).expect("runtime").pid as i32;
    unsafe { libc::kill(pid, libc::SIGKILL) };
    wait_for("Errored status", || {
        matches!(gateway.app.status(id), ServerStatus::Errored { .. })
    })
    .await;

    let response = router(gateway.clone())
        .oneshot(post(&gateway, id, json!({"id":1,"method":"ping"})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
