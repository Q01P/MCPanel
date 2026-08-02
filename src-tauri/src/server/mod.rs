//! Token-guarded local gateway on `127.0.0.1:6789` (loopback bind is what
//! rejects non-local clients): `GET /sse` streams state changes + log lines,
//! `POST /mcp/{server_id}` forwards JSON-RPC to a running server's stdin,
//! with a 30 s tower timeout on POSTs.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{Value, json};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::{Stream, StreamExt};
use tower::ServiceBuilder;
use tracing::{error, info};

use crate::error::{AppError, AppResult};
use crate::state::{AppState, ServerId};

/// Loopback bind is what rejects non-local clients.
pub const GATEWAY_ADDR: &str = "127.0.0.1:6789";

/// tower timeout on `POST /mcp/{server_id}` (spec §3).
pub const POST_TIMEOUT: Duration = Duration::from_secs(30);

const TOKEN_BYTES: usize = 32;

/// Bearer token generated in memory at startup; never persisted or logged.
#[derive(Clone)]
pub struct AuthToken(Arc<str>);

impl AuthToken {
    pub fn generate() -> Self {
        let mut bytes = [0u8; TOKEN_BYTES];
        SystemRandom::new()
            .fill(&mut bytes)
            .expect("system RNG failure");
        let mut hex = String::with_capacity(TOKEN_BYTES * 2);
        for b in bytes {
            use std::fmt::Write;
            let _ = write!(hex, "{b:02x}");
        }
        Self(hex.into())
    }

    /// Constant-time comparison so the token can't be recovered byte-by-byte
    /// through timing (XOR-accumulate; `black_box` keeps the compiler from
    /// short-circuiting the fold).
    pub fn matches(&self, candidate: &str) -> bool {
        let expected = self.0.as_bytes();
        let candidate = candidate.as_bytes();
        if expected.len() != candidate.len() {
            return false;
        }
        expected
            .iter()
            .zip(candidate)
            .fold(0u8, |acc, (a, b)| acc | std::hint::black_box(a ^ b))
            == 0
    }

    /// For handing to the UI (T10); do not log.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

#[derive(Clone)]
pub struct Gateway {
    pub token: AuthToken,
    pub app: AppState,
}

/// Accepts `Authorization: Bearer <token>` or, for `EventSource` clients that
/// cannot set headers, a `token` query parameter.
fn authorize(headers: &HeaderMap, query_token: Option<&str>, token: &AuthToken) -> AppResult<()> {
    let candidate = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .or(query_token);
    match candidate {
        Some(c) if token.matches(c) => Ok(()),
        _ => Err(AppError::Unauthorized),
    }
}

#[derive(serde::Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

/// `GET /sse`: a `ready` event, then every [`crate::state::AppEvent`]
/// pre-serialized to JSON. A lagged subscriber gets a `lagged` marker instead
/// of silently missing events.
async fn sse_events(
    State(gateway): State<Gateway>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
) -> AppResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    authorize(&headers, query.token.as_deref(), &gateway.token)?;

    let events = BroadcastStream::new(gateway.app.subscribe()).map(|item| {
        let payload = match item {
            Ok(event) => serde_json::to_string(&event)
                .unwrap_or_else(|_| r#"{"type":"serialize_error"}"#.into()),
            Err(BroadcastStreamRecvError::Lagged(missed)) => {
                format!(r#"{{"type":"lagged","missed":{missed}}}"#)
            }
        };
        Ok(Event::default().event("app").data(payload))
    });
    let ready = tokio_stream::once(Ok(Event::default().event("ready").data("{}")));

    Ok(Sse::new(ready.chain(events)).keep_alive(KeepAlive::default()))
}

/// The JSON-RPC workbench payload. The gateway re-correlates: the child sees
/// our own monotonic ids, and the caller's `id` is echoed back in the
/// envelope.
#[derive(serde::Deserialize)]
struct ForwardRequest {
    method: String,
    #[serde(default)]
    params: Value,
    id: Option<Value>,
}

/// `POST /mcp/{server_id}`: forward JSON-RPC to a *running* server. RPC-level
/// errors come back as JSON-RPC error envelopes (the workbench wants to
/// inspect them); transport-level failures surface as HTTP errors.
async fn mcp_forward(
    State(gateway): State<Gateway>,
    Path(server_id): Path<ServerId>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    Json(request): Json<ForwardRequest>,
) -> AppResult<Json<Value>> {
    authorize(&headers, query.token.as_deref(), &gateway.token)?;

    let runtime = gateway
        .app
        .runtime(server_id)
        .ok_or_else(|| AppError::ServerNotFound(server_id.to_string()))?;

    let Some(caller_id) = request.id else {
        runtime.client.notify(&request.method, request.params).await?;
        return Ok(Json(json!({ "accepted": true })));
    };

    match runtime.client.request(&request.method, request.params).await {
        Ok(result) => Ok(Json(json!({
            "jsonrpc": "2.0",
            "id": caller_id,
            "result": result,
        }))),
        Err(AppError::Rpc { code, message }) => Ok(Json(json!({
            "jsonrpc": "2.0",
            "id": caller_id,
            "error": { "code": code, "message": message },
        }))),
        Err(other) => Err(other),
    }
}

pub fn router(gateway: Gateway) -> Router {
    let forward = post(mcp_forward).layer(
        ServiceBuilder::new()
            .layer(axum::error_handling::HandleErrorLayer::new(
                |_: tower::BoxError| async {
                    AppError::Timeout("gateway request".to_string())
                },
            ))
            .layer(tower::timeout::TimeoutLayer::new(POST_TIMEOUT)),
    );

    Router::new()
        .route("/sse", get(sse_events))
        .route("/mcp/{server_id}", forward)
        .with_state(gateway)
}

pub async fn serve(gateway: Gateway) {
    let listener = match tokio::net::TcpListener::bind(GATEWAY_ADDR).await {
        Ok(listener) => listener,
        Err(error) => {
            error!(target: "app::server", %error, "failed to bind gateway on {GATEWAY_ADDR}");
            return;
        }
    };
    info!(target: "app::server", "gateway listening on {GATEWAY_ADDR}");
    if let Err(error) = axum::serve(listener, router(gateway)).await {
        error!(target: "app::server", %error, "gateway server error");
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt;

    use super::*;
    use crate::db;

    fn test_gateway() -> Gateway {
        Gateway {
            token: AuthToken::generate(),
            app: AppState::new(db::open_in_memory().unwrap()),
        }
    }

    #[test]
    fn tokens_are_unique_and_self_matching() {
        let a = AuthToken::generate();
        let b = AuthToken::generate();
        assert_eq!(a.expose().len(), TOKEN_BYTES * 2);
        assert!(a.matches(a.expose()));
        assert!(!a.matches(b.expose()));
    }

    async fn status_of(gateway: &Gateway, request: Request<Body>) -> StatusCode {
        router(gateway.clone())
            .oneshot(request)
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn sse_rejects_missing_and_wrong_tokens() {
        let gateway = test_gateway();
        let missing = Request::builder().uri("/sse").body(Body::empty()).unwrap();
        assert_eq!(status_of(&gateway, missing).await, StatusCode::UNAUTHORIZED);

        let wrong = Request::builder()
            .uri("/sse?token=deadbeef")
            .body(Body::empty())
            .unwrap();
        assert_eq!(status_of(&gateway, wrong).await, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn sse_accepts_query_token_and_bearer_header() {
        let gateway = test_gateway();
        let via_query = Request::builder()
            .uri(format!("/sse?token={}", gateway.token.expose()))
            .body(Body::empty())
            .unwrap();
        assert_eq!(status_of(&gateway, via_query).await, StatusCode::OK);

        let via_header = Request::builder()
            .uri("/sse")
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", gateway.token.expose()),
            )
            .body(Body::empty())
            .unwrap();
        let response = router(gateway.clone()).oneshot(via_header).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
    }

    #[tokio::test]
    async fn mcp_post_requires_token_and_running_server() {
        let gateway = test_gateway();
        let unauthorized = Request::builder()
            .method("POST")
            .uri("/mcp/1")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"method":"ping","id":1}"#))
            .unwrap();
        assert_eq!(
            status_of(&gateway, unauthorized).await,
            StatusCode::UNAUTHORIZED
        );

        let not_running = Request::builder()
            .method("POST")
            .uri("/mcp/1")
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", gateway.token.expose()),
            )
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"method":"ping","id":1}"#))
            .unwrap();
        assert_eq!(status_of(&gateway, not_running).await, StatusCode::NOT_FOUND);
    }
}
