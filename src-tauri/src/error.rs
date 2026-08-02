use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::ser::{Serialize, SerializeStruct, Serializer};

pub type AppResult<T> = Result<T, AppError>;

/// The single error funnel: infrastructure failures arrive via `#[from]`,
/// domain failures via explicit variants.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("server not found: {0}")]
    ServerNotFound(String),

    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("conflict: {0}")]
    Conflict(String),

    /// Our own machinery failed (a panicked blocking task, a poisoned
    /// lock) — kept apart from `Io` so real I/O failures stay diagnosable.
    #[error("internal error: {0}")]
    Internal(String),

    #[error("MCP handshake failed: {0}")]
    Handshake(String),

    #[error("operation timed out: {0}")]
    Timeout(String),

    #[error("unauthorized")]
    Unauthorized,

    #[error("server RPC error {code}: {message}")]
    Rpc { code: i64, message: String },

    #[error("server connection closed")]
    ConnectionClosed,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("credential store error: {0}")]
    Keyring(#[from] keyring::Error),
}

impl AppError {
    /// Stable machine-readable code; the frontend matches on this,
    /// never on the human-readable message.
    pub fn code(&self) -> &'static str {
        match self {
            Self::ServerNotFound(_) => "server_not_found",
            Self::InvalidInput(_) => "invalid_input",
            Self::Conflict(_) => "conflict",
            Self::Internal(_) => "internal",
            Self::Handshake(_) => "handshake",
            Self::Timeout(_) => "timeout",
            Self::Unauthorized => "unauthorized",
            Self::Rpc { .. } => "rpc",
            Self::ConnectionClosed => "connection_closed",
            Self::Io(_) => "io",
            Self::Db(_) => "db",
            Self::Json(_) => "json",
            Self::Keyring(_) => "keyring",
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            Self::ServerNotFound(_) => StatusCode::NOT_FOUND,
            Self::InvalidInput(_) => StatusCode::BAD_REQUEST,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            // The child MCP server is the gateway's upstream.
            Self::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
            // Json included: malformed request bodies are rejected by axum
            // before reaching handlers, so a Json error surfacing here means
            // the *child's* output failed to (de)serialize — upstream fault,
            // not the caller's.
            Self::Handshake(_) | Self::Rpc { .. } | Self::ConnectionClosed | Self::Json(_) => {
                StatusCode::BAD_GATEWAY
            }
            Self::Io(_) | Self::Db(_) | Self::Keyring(_) | Self::Internal(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }
}

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("AppError", 2)?;
        s.serialize_field("code", self.code())?;
        s.serialize_field("message", &self.to_string())?;
        s.end()
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        (status, axum::Json(self)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_errors_map_to_caller_facing_statuses() {
        assert_eq!(
            AppError::InvalidInput("x".into()).status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            AppError::Conflict("x".into()).status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            AppError::Internal("x".into()).status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            AppError::ServerNotFound("x".into()).status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(AppError::Internal("x".into()).code(), "internal");
    }

    #[test]
    fn serializes_to_code_and_message() {
        let err = AppError::ServerNotFound("mock".into());
        let value = serde_json::to_value(&err).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "code": "server_not_found",
                "message": "server not found: mock",
            })
        );
    }
}
