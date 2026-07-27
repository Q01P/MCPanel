use std::convert::Infallible;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::get;
use ring::rand::{SecureRandom, SystemRandom};
use tokio_stream::{Stream, StreamExt};
use tracing::{error, info};

use crate::error::{AppError, AppResult};

/// Loopback bind is what rejects non-local clients.
pub const GATEWAY_ADDR: &str = "127.0.0.1:6789";

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

    /// For handing to the UI (T7); do not log.
    pub fn expose(&self) -> &str {
        &self.0
    }
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
struct SseQuery {
    token: Option<String>,
}

/// Stub: one `ready` event, then keep-alives. Real state-change and log-line
/// fan-out lands in T8.
async fn sse_stub(
    State(token): State<AuthToken>,
    headers: HeaderMap,
    Query(query): Query<SseQuery>,
) -> AppResult<Sse<impl Stream<Item = Result<Event, Infallible>>>> {
    authorize(&headers, query.token.as_deref(), &token)?;
    let stream =
        tokio_stream::once(Ok(Event::default().event("ready").data("{}"))).chain(tokio_stream::pending());
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

pub fn router(token: AuthToken) -> Router {
    Router::new().route("/sse", get(sse_stub)).with_state(token)
}

pub async fn serve(token: AuthToken) {
    let listener = match tokio::net::TcpListener::bind(GATEWAY_ADDR).await {
        Ok(listener) => listener,
        Err(error) => {
            error!(target: "app::server", %error, "failed to bind gateway on {GATEWAY_ADDR}");
            return;
        }
    };
    info!(target: "app::server", "gateway listening on {GATEWAY_ADDR}");
    if let Err(error) = axum::serve(listener, router(token)).await {
        error!(target: "app::server", %error, "gateway server error");
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use tower::ServiceExt;

    use super::*;

    #[test]
    fn tokens_are_unique_and_self_matching() {
        let a = AuthToken::generate();
        let b = AuthToken::generate();
        assert_eq!(a.expose().len(), TOKEN_BYTES * 2);
        assert!(a.matches(a.expose()));
        assert!(!a.matches(b.expose()));
    }

    async fn sse_status(request: Request<Body>, token: &AuthToken) -> StatusCode {
        router(token.clone()).oneshot(request).await.unwrap().status()
    }

    #[tokio::test]
    async fn sse_rejects_missing_and_wrong_tokens() {
        let token = AuthToken::generate();
        let missing = Request::builder().uri("/sse").body(Body::empty()).unwrap();
        assert_eq!(sse_status(missing, &token).await, StatusCode::UNAUTHORIZED);

        let wrong = Request::builder()
            .uri("/sse?token=deadbeef")
            .body(Body::empty())
            .unwrap();
        assert_eq!(sse_status(wrong, &token).await, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn sse_accepts_query_token_and_bearer_header() {
        let token = AuthToken::generate();
        let via_query = Request::builder()
            .uri(format!("/sse?token={}", token.expose()))
            .body(Body::empty())
            .unwrap();
        assert_eq!(sse_status(via_query, &token).await, StatusCode::OK);

        let via_header = Request::builder()
            .uri("/sse")
            .header(header::AUTHORIZATION, format!("Bearer {}", token.expose()))
            .body(Body::empty())
            .unwrap();
        let response = router(token.clone()).oneshot(via_header).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
    }
}
