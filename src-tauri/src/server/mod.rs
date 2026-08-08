//! Token-guarded local gateway on an ephemeral loopback port (bound at
//! startup, reported to the webview via `gateway_info`): `GET /sse` streams
//! state changes + log lines, `POST /mcp/{server_id}` forwards JSON-RPC to a
//! running server's stdin with a per-request timeout (`?timeout_s=`, capped).
//!
//! Three layers of defense: the loopback bind rejects non-local clients, the
//! bearer token rejects other local processes, and Host validation rejects
//! DNS-rebinding-shaped requests whose Host header isn't the bound address.
//! The token is accepted via query parameter on `/sse` only (EventSource
//! cannot set headers); nothing here logs request URIs.
//!
//! The webview is a *different origin* from the gateway (`tauri://localhost`
//! or `http://tauri.localhost` in production, the vite dev server in dev), so
//! every browser-side call — the `/sse` EventSource and the workbench POST —
//! is subject to CORS. The [`cors_layer`] grants exactly those origins;
//! without it the webview can never read a gateway response.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{Value, json};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::{Stream, StreamExt};
use tower::ServiceBuilder;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tracing::{error, info};

use crate::error::{AppError, AppResult};
use crate::state::{AppState, ServerId};

/// Cap on the caller-selectable `?timeout_s=` for `POST /mcp/{server_id}`.
pub const MAX_FORWARD_TIMEOUT: Duration = Duration::from_secs(300);

/// tower backstop on POSTs — strictly above [`MAX_FORWARD_TIMEOUT`] so it
/// never races the per-request protocol timeout; it only catches a handler
/// that wedged outright.
pub const GATEWAY_BACKSTOP_TIMEOUT: Duration =
    Duration::from_secs(MAX_FORWARD_TIMEOUT.as_secs() + 10);

/// The gateway's actual bound address (ephemeral port), resolved in `setup`
/// and managed as Tauri state for `gateway_info`.
#[derive(Clone, Copy, Debug)]
pub struct GatewayAddr(pub std::net::SocketAddr);

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

    /// For handing to the UI; do not log.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

#[derive(Clone)]
pub struct Gateway {
    pub token: AuthToken,
    pub app: AppState,
    /// The bound address as clients must name it in their Host header
    /// (`127.0.0.1:<port>`); requests arriving under any other Host are
    /// rejected — free defense-in-depth against DNS rebinding.
    pub host: String,
}

/// The only Hosts a legitimate client can arrive with: the bound address
/// exactly as `gateway_info` hands it out, or its `localhost` spelling.
fn host_allowed(headers: &HeaderMap, gateway: &Gateway) -> bool {
    let Some(host) = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    if host == gateway.host {
        return true;
    }
    let port = gateway.host.rsplit(':').next().unwrap_or_default();
    host == format!("localhost:{port}")
}

/// Host validation first, then the token: `Authorization: Bearer <token>`
/// or — where the route opts in (`/sse`, for `EventSource` clients that
/// cannot set headers) — a `token` query parameter.
fn authorize(headers: &HeaderMap, query_token: Option<&str>, gateway: &Gateway) -> AppResult<()> {
    if !host_allowed(headers, gateway) {
        return Err(AppError::Unauthorized);
    }
    let candidate = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .or(query_token);
    match candidate {
        Some(c) if gateway.token.matches(c) => Ok(()),
        _ => Err(AppError::Unauthorized),
    }
}

/// CORS for the webview origins — and only those. CORS is a browser gate,
/// not the auth layer (that stays token + Host), but there is no reason to
/// let any other page's script read gateway responses. Preflights are
/// answered by the layer itself before `authorize` runs; that is correct —
/// a preflight carries no credentials by design and grants nothing beyond
/// permission to send the real, fully-authenticated request.
fn cors_layer() -> CorsLayer {
    let mut origins = vec![
        // Linux/macOS production webview.
        HeaderValue::from_static("tauri://localhost"),
        // Windows production webview.
        HeaderValue::from_static("http://tauri.localhost"),
    ];
    if cfg!(debug_assertions) {
        // The vite dev server (`npm run tauri dev`).
        origins.push(HeaderValue::from_static("http://localhost:1420"));
    }
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
        .max_age(Duration::from_secs(3600))
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
    authorize(&headers, query.token.as_deref(), &gateway)?;

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

/// `POST /mcp/{server_id}` options. No `token` here — the POST route
/// requires the Authorization header, so the token can never end up in a
/// URL that devtools, HAR exports, or a future trace layer might capture.
#[derive(serde::Deserialize)]
struct ForwardQuery {
    /// Per-request timeout in seconds, clamped to `1..=MAX_FORWARD_TIMEOUT`.
    /// Slow tools (LLM-backed ones routinely blow 30 s) are the workbench's
    /// subject matter, not a failure.
    timeout_s: Option<u64>,
}

/// Clamp the caller's `?timeout_s=` to `1..=MAX_FORWARD_TIMEOUT` (spec §3);
/// absent means the protocol default. Always strictly below the tower
/// backstop, so the two can never race.
fn forward_timeout(timeout_s: Option<u64>) -> Duration {
    timeout_s
        .map(|s| Duration::from_secs(s.clamp(1, MAX_FORWARD_TIMEOUT.as_secs())))
        .unwrap_or(crate::mcp::protocol::DEFAULT_REQUEST_TIMEOUT)
}

/// `POST /mcp/{server_id}`: forward JSON-RPC to a *running* server. RPC-level
/// errors come back as JSON-RPC error envelopes (the workbench wants to
/// inspect them); transport-level failures surface as HTTP errors.
async fn mcp_forward(
    State(gateway): State<Gateway>,
    Path(server_id): Path<ServerId>,
    headers: HeaderMap,
    Query(query): Query<ForwardQuery>,
    body: bytes::Bytes,
) -> AppResult<Json<Value>> {
    authorize(&headers, None, &gateway)?;

    // Parsed here, not via the `Json` extractor: extractors run before the
    // handler, so a malformed body would otherwise earn a 4xx parse hint
    // without ever presenting a token.
    let request: ForwardRequest = serde_json::from_slice(&body)
        .map_err(|error| AppError::InvalidInput(format!("invalid JSON-RPC body: {error}")))?;

    let runtime = gateway
        .app
        .runtime(server_id)
        .ok_or_else(|| AppError::ServerNotFound(server_id.to_string()))?;

    let Some(caller_id) = request.id else {
        runtime
            .client
            .notify(&request.method, request.params)
            .await?;
        return Ok(Json(json!({ "accepted": true })));
    };

    let timeout = forward_timeout(query.timeout_s);
    match runtime
        .client
        .request_with_timeout(&request.method, request.params, timeout)
        .await
    {
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

// Auth is a per-handler call (`authorize` first statement), not middleware —
// the two routes accept the token from different places. Any route added
// here MUST call `authorize` and be added to the
// `every_route_rejects_unauthenticated_requests` test below.
pub fn router(gateway: Gateway) -> Router {
    let forward = post(mcp_forward).layer(
        ServiceBuilder::new()
            .layer(axum::error_handling::HandleErrorLayer::new(
                |_: tower::BoxError| async { AppError::Timeout("gateway request".to_string()) },
            ))
            .layer(tower::timeout::TimeoutLayer::new(GATEWAY_BACKSTOP_TIMEOUT)),
    );

    Router::new()
        .route("/sse", get(sse_events))
        .route("/mcp/{server_id}", forward)
        .layer(cors_layer())
        .with_state(gateway)
}

/// Bind the gateway on an ephemeral loopback port. Called synchronously from
/// `setup` so a bind failure aborts the launch loudly, instead of the app
/// running with the webview pointed at a dead port. An ephemeral port also
/// means a second MCPanel instance gets its own gateway, and no local page
/// can fingerprint the app by probing a well-known port.
pub fn bind() -> AppResult<(std::net::TcpListener, std::net::SocketAddr)> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    Ok((listener, addr))
}

pub async fn serve(gateway: Gateway, listener: std::net::TcpListener) {
    let listener = match tokio::net::TcpListener::from_std(listener) {
        Ok(listener) => listener,
        Err(error) => {
            error!(target: "app::server", %error, "failed to adopt gateway listener");
            return;
        }
    };
    info!(target: "app::server", "gateway listening on {}", gateway.host);
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

    const TEST_HOST: &str = "127.0.0.1:4321";

    fn test_gateway() -> Gateway {
        Gateway {
            token: AuthToken::generate(),
            app: AppState::new(db::open_in_memory().unwrap()),
            host: TEST_HOST.into(),
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
        let missing = Request::builder()
            .uri("/sse")
            .header(header::HOST, TEST_HOST)
            .body(Body::empty())
            .unwrap();
        assert_eq!(status_of(&gateway, missing).await, StatusCode::UNAUTHORIZED);

        let wrong = Request::builder()
            .uri("/sse?token=deadbeef")
            .header(header::HOST, TEST_HOST)
            .body(Body::empty())
            .unwrap();
        assert_eq!(status_of(&gateway, wrong).await, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn sse_accepts_query_token_and_bearer_header() {
        let gateway = test_gateway();
        let via_query = Request::builder()
            .uri(format!("/sse?token={}", gateway.token.expose()))
            .header(header::HOST, TEST_HOST)
            .body(Body::empty())
            .unwrap();
        assert_eq!(status_of(&gateway, via_query).await, StatusCode::OK);

        let via_header = Request::builder()
            .uri("/sse")
            .header(header::HOST, TEST_HOST)
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
    async fn foreign_or_missing_host_is_rejected() {
        let gateway = test_gateway();

        // A rebinding-style request: valid token, attacker-controlled Host.
        let rebound = Request::builder()
            .uri(format!("/sse?token={}", gateway.token.expose()))
            .header(header::HOST, "evil.example:4321")
            .body(Body::empty())
            .unwrap();
        assert_eq!(status_of(&gateway, rebound).await, StatusCode::UNAUTHORIZED);

        let hostless = Request::builder()
            .uri(format!("/sse?token={}", gateway.token.expose()))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            status_of(&gateway, hostless).await,
            StatusCode::UNAUTHORIZED
        );

        // The localhost spelling of the bound port is legitimate.
        let localhost = Request::builder()
            .uri(format!("/sse?token={}", gateway.token.expose()))
            .header(header::HOST, "localhost:4321")
            .body(Body::empty())
            .unwrap();
        assert_eq!(status_of(&gateway, localhost).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn mcp_post_rejects_query_token() {
        // The query-parameter fallback is an /sse-only concession to
        // EventSource; POSTs can set headers, so a token in the URL is
        // rejected rather than normalized.
        let gateway = test_gateway();
        let via_query = Request::builder()
            .method("POST")
            .uri(format!("/mcp/1?token={}", gateway.token.expose()))
            .header(header::HOST, TEST_HOST)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"method":"ping","id":1}"#))
            .unwrap();
        assert_eq!(
            status_of(&gateway, via_query).await,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn mcp_post_requires_token_and_running_server() {
        let gateway = test_gateway();
        let unauthorized = Request::builder()
            .method("POST")
            .uri("/mcp/1")
            .header(header::HOST, TEST_HOST)
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
            .header(header::HOST, TEST_HOST)
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", gateway.token.expose()),
            )
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(r#"{"method":"ping","id":1}"#))
            .unwrap();
        assert_eq!(
            status_of(&gateway, not_running).await,
            StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn forward_timeout_clamps_to_gateway_bounds() {
        use crate::mcp::protocol::DEFAULT_REQUEST_TIMEOUT;
        assert_eq!(forward_timeout(None), DEFAULT_REQUEST_TIMEOUT);
        assert_eq!(forward_timeout(Some(0)), Duration::from_secs(1));
        assert_eq!(forward_timeout(Some(1)), Duration::from_secs(1));
        assert_eq!(forward_timeout(Some(42)), Duration::from_secs(42));
        assert_eq!(forward_timeout(Some(300)), MAX_FORWARD_TIMEOUT);
        assert_eq!(forward_timeout(Some(u64::MAX)), MAX_FORWARD_TIMEOUT);
        // The clamp ceiling must stay strictly below the tower backstop.
        assert!(MAX_FORWARD_TIMEOUT < GATEWAY_BACKSTOP_TIMEOUT);
    }

    /// The structural guard `router()`'s comment points at: every registered
    /// route, hit without credentials, must 401. Extend this list whenever a
    /// route is added.
    #[tokio::test]
    async fn every_route_rejects_unauthenticated_requests() {
        let gateway = test_gateway();
        let routes = [("GET", "/sse"), ("POST", "/mcp/1")];
        for (method, uri) in routes {
            let request = Request::builder()
                .method(method)
                .uri(uri)
                .header(header::HOST, TEST_HOST)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"method":"ping","id":1}"#))
                .unwrap();
            assert_eq!(
                status_of(&gateway, request).await,
                StatusCode::UNAUTHORIZED,
                "{method} {uri} must reject an unauthenticated request"
            );
        }
    }

    /// SSE end-to-end over the response body: `ready` first, then published
    /// events as `event: app` frames, and a `lagged` marker when the
    /// subscriber falls behind the broadcast channel.
    #[tokio::test]
    async fn sse_delivers_ready_events_and_lagged_markers() {
        use crate::state::{AppEvent, ServerStatus};
        use tokio_stream::StreamExt;

        let gateway = test_gateway();
        let request = Request::builder()
            .uri(format!("/sse?token={}", gateway.token.expose()))
            .header(header::HOST, TEST_HOST)
            .body(Body::empty())
            .unwrap();
        let response = router(gateway.clone()).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let mut body = response.into_body().into_data_stream();
        let mut seen = String::new();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        async fn read_until(
            body: &mut axum::body::BodyDataStream,
            seen: &mut String,
            needle: &str,
            deadline: tokio::time::Instant,
        ) {
            while !seen.contains(needle) {
                let chunk = tokio::time::timeout_at(deadline, StreamExt::next(body))
                    .await
                    .expect("chunk within deadline")
                    .expect("stream open")
                    .expect("chunk ok");
                seen.push_str(&String::from_utf8_lossy(&chunk));
            }
        }

        // 1. The ready event opens every subscription.
        read_until(&mut body, &mut seen, "event: ready", deadline).await;
        assert!(
            !seen.contains("event: app"),
            "no app events before ready: {seen}"
        );

        // 2. A published AppEvent arrives serialized under `event: app`.
        gateway.app.publish(AppEvent::StatusChanged {
            server_id: 7,
            status: ServerStatus::Running,
        });
        read_until(&mut body, &mut seen, r#""type":"status_changed""#, deadline).await;
        assert!(seen.contains("event: app"), "app frame present: {seen}");
        assert!(seen.contains(r#""server_id":7"#), "payload intact: {seen}");

        // 3. Overflow the broadcast channel while the body sits unpolled —
        //    the loss must surface as a lagged marker, not silence.
        for i in 0..2000u64 {
            gateway.app.publish(AppEvent::LogGap {
                server_id: 7,
                stream: crate::state::LogStream::Stdout,
                dropped: i,
            });
        }
        read_until(&mut body, &mut seen, r#""type":"lagged""#, deadline).await;
    }

    /// The webview can only read a gateway response if CORS grants its
    /// origin — gateway tests speak raw HTTP and would never notice a
    /// missing grant, so the headers are asserted explicitly.
    #[tokio::test]
    async fn cors_grants_the_webview_origins() {
        let gateway = test_gateway();
        let mut origins = vec!["tauri://localhost", "http://tauri.localhost"];
        if cfg!(debug_assertions) {
            origins.push("http://localhost:1420");
        }
        for origin in origins {
            let request = Request::builder()
                .uri(format!("/sse?token={}", gateway.token.expose()))
                .header(header::HOST, TEST_HOST)
                .header(header::ORIGIN, origin)
                .body(Body::empty())
                .unwrap();
            let response = router(gateway.clone()).oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
                origin,
                "{origin} must be granted"
            );
        }
    }

    /// The workbench POST carries an Authorization header, which forces a
    /// preflight — if OPTIONS 405s, the real request is never sent.
    #[tokio::test]
    async fn cors_preflight_succeeds_for_the_workbench_post() {
        let gateway = test_gateway();
        let preflight = Request::builder()
            .method("OPTIONS")
            .uri("/mcp/1")
            .header(header::HOST, TEST_HOST)
            .header(header::ORIGIN, "tauri://localhost")
            .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
            .header(
                header::ACCESS_CONTROL_REQUEST_HEADERS,
                "authorization,content-type",
            )
            .body(Body::empty())
            .unwrap();
        let response = router(gateway.clone()).oneshot(preflight).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let headers = response.headers();
        assert_eq!(
            headers[header::ACCESS_CONTROL_ALLOW_ORIGIN],
            "tauri://localhost"
        );
        let allowed_headers = headers[header::ACCESS_CONTROL_ALLOW_HEADERS]
            .to_str()
            .unwrap()
            .to_ascii_lowercase();
        assert!(
            allowed_headers.contains("authorization"),
            "{allowed_headers}"
        );
        assert!(
            allowed_headers.contains("content-type"),
            "{allowed_headers}"
        );
        assert!(
            headers[header::ACCESS_CONTROL_ALLOW_METHODS]
                .to_str()
                .unwrap()
                .contains("POST")
        );
    }

    #[tokio::test]
    async fn cors_grants_nothing_to_foreign_origins() {
        let gateway = test_gateway();
        let request = Request::builder()
            .uri(format!("/sse?token={}", gateway.token.expose()))
            .header(header::HOST, TEST_HOST)
            .header(header::ORIGIN, "http://evil.example")
            .body(Body::empty())
            .unwrap();
        let response = router(gateway.clone()).oneshot(request).await.unwrap();
        // The request is served — CORS is a browser gate, and auth already
        // passed — but no allow-origin grant is issued for the response.
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none()
        );
    }

    /// Auth strictly precedes body parsing: an unauthenticated caller gets
    /// 401 and nothing else — never a parse error that hints at the schema.
    #[tokio::test]
    async fn malformed_body_is_rejected_only_after_auth() {
        let gateway = test_gateway();
        let unauthenticated = Request::builder()
            .method("POST")
            .uri("/mcp/1")
            .header(header::HOST, TEST_HOST)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("not json"))
            .unwrap();
        assert_eq!(
            status_of(&gateway, unauthenticated).await,
            StatusCode::UNAUTHORIZED
        );

        let authenticated = Request::builder()
            .method("POST")
            .uri("/mcp/1")
            .header(header::HOST, TEST_HOST)
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", gateway.token.expose()),
            )
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("not json"))
            .unwrap();
        assert_eq!(
            status_of(&gateway, authenticated).await,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn bind_yields_a_usable_ephemeral_port() {
        let (listener, addr) = bind().expect("bind");
        assert!(addr.ip().is_loopback());
        assert_ne!(addr.port(), 0, "a real port was allocated");
        // A second instance binds its own distinct port instead of dying.
        let (second, second_addr) = bind().expect("second bind");
        assert_ne!(addr.port(), second_addr.port());
        drop((listener, second));
    }
}
