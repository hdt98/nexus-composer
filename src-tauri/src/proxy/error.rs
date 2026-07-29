use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProxyError {
    #[error("Server is already running")]
    AlreadyRunning,

    #[error("Server is not running")]
    NotRunning,

    #[error("Failed to bind address: {0}")]
    BindFailed(String),

    #[error("Stop timed out")]
    StopTimeout,

    #[error("Failed to stop server: {0}")]
    StopFailed(String),

    #[error("Request forwarding failed: {0}")]
    ForwardFailed(String),

    #[error("No provider available")]
    NoAvailableProvider,

    #[error("All providers are circuit-open; no route available")]
    AllProvidersCircuitOpen,

    #[error("No providers configured")]
    NoProvidersConfigured,

    #[allow(dead_code)]
    #[error("Provider is unhealthy: {0}")]
    ProviderUnhealthy(String),

    #[error("Upstream error (status {status}): {body:?}")]
    UpstreamError { status: u16, body: Option<String> },

    #[error("Maximum retry count exceeded")]
    MaxRetriesExceeded,

    #[error("Database error: {0}")]
    DatabaseError(String),

    #[error("Configuration error: {0}")]
    ConfigError(String),

    #[allow(dead_code)]
    #[error("Transformation error: {0}")]
    TransformError(String),

    #[allow(dead_code)]
    #[error("Invalid request: {0}")]
    InvalidRequest(String),

    #[error("Timeout: {0}")]
    Timeout(String),

    /// Streaming response idle timeout.
    #[allow(dead_code)]
    #[error("Streaming response idle timeout: no data for {0} seconds")]
    StreamIdleTimeout(u64),

    /// Authentication error.
    #[error("Authentication failed: {0}")]
    AuthError(String),

    #[allow(dead_code)]
    #[error("Internal error: {0}")]
    Internal(String),
}

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response {
        let (status, body) = match &self {
            ProxyError::UpstreamError {
                status: upstream_status,
                body: upstream_body,
            } => {
                let http_status =
                    StatusCode::from_u16(*upstream_status).unwrap_or(StatusCode::BAD_GATEWAY);

                // Try to parse the upstream response body as JSON; otherwise wrap it as a string.
                let error_body = if let Some(body_str) = upstream_body {
                    if let Ok(json_body) = serde_json::from_str::<serde_json::Value>(body_str) {
                        // The upstream returned JSON, so pass it through directly.
                        json_body
                    } else {
                        // The upstream returned non-JSON content, so wrap it as an error message.
                        json!({
                            "error": {
                                "message": body_str,
                                "type": "upstream_error",
                            }
                        })
                    }
                } else {
                    json!({
                        "error": {
                            "message": format!("Upstream error (status {})", upstream_status),
                            "type": "upstream_error",
                        }
                    })
                };

                (http_status, error_body)
            }
            _ => {
                let (http_status, message) = match &self {
                    ProxyError::AlreadyRunning => (StatusCode::CONFLICT, self.to_string()),
                    ProxyError::NotRunning => (StatusCode::SERVICE_UNAVAILABLE, self.to_string()),
                    ProxyError::BindFailed(_) => {
                        (StatusCode::INTERNAL_SERVER_ERROR, self.to_string())
                    }
                    ProxyError::StopTimeout => {
                        (StatusCode::INTERNAL_SERVER_ERROR, self.to_string())
                    }
                    ProxyError::StopFailed(_) => {
                        (StatusCode::INTERNAL_SERVER_ERROR, self.to_string())
                    }
                    ProxyError::ForwardFailed(_) => (StatusCode::BAD_GATEWAY, self.to_string()),
                    ProxyError::NoAvailableProvider => {
                        (StatusCode::SERVICE_UNAVAILABLE, self.to_string())
                    }
                    ProxyError::AllProvidersCircuitOpen => {
                        (StatusCode::SERVICE_UNAVAILABLE, self.to_string())
                    }
                    ProxyError::NoProvidersConfigured => {
                        (StatusCode::SERVICE_UNAVAILABLE, self.to_string())
                    }
                    ProxyError::ProviderUnhealthy(_) => {
                        (StatusCode::SERVICE_UNAVAILABLE, self.to_string())
                    }
                    ProxyError::MaxRetriesExceeded => {
                        (StatusCode::SERVICE_UNAVAILABLE, self.to_string())
                    }
                    ProxyError::DatabaseError(_) => {
                        (StatusCode::INTERNAL_SERVER_ERROR, self.to_string())
                    }
                    ProxyError::ConfigError(_) => (StatusCode::BAD_REQUEST, self.to_string()),
                    ProxyError::TransformError(_) => {
                        (StatusCode::UNPROCESSABLE_ENTITY, self.to_string())
                    }
                    ProxyError::InvalidRequest(_) => (StatusCode::BAD_REQUEST, self.to_string()),
                    ProxyError::Timeout(_) => (StatusCode::GATEWAY_TIMEOUT, self.to_string()),
                    ProxyError::StreamIdleTimeout(_) => {
                        (StatusCode::GATEWAY_TIMEOUT, self.to_string())
                    }
                    ProxyError::AuthError(_) => (StatusCode::UNAUTHORIZED, self.to_string()),
                    ProxyError::Internal(_) => {
                        (StatusCode::INTERNAL_SERVER_ERROR, self.to_string())
                    }
                    ProxyError::UpstreamError { .. } => unreachable!(),
                };

                let error_body = json!({
                    "error": {
                        "message": message,
                        "type": "proxy_error",
                    }
                });

                (http_status, error_body)
            }
        };

        (status, Json(body)).into_response()
    }
}

/// Error classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCategory {
    /// Retryable errors such as network failures and 5xx responses.
    Retryable, // Network timeouts and 5xx errors.
    /// Non-retryable errors such as 4xx responses and authentication failures.
    NonRetryable, // Authentication failures, invalid parameters, and 4xx errors.
    #[allow(dead_code)]
    ClientAbort, // Client-initiated abort.
}

/// Determine whether an error is retryable.
#[allow(dead_code)]
pub fn categorize_error(error: &reqwest::Error) -> ErrorCategory {
    if error.is_timeout() || error.is_connect() {
        return ErrorCategory::Retryable;
    }

    if let Some(status) = error.status() {
        if status.is_server_error() {
            ErrorCategory::Retryable
        } else if status.is_client_error() {
            ErrorCategory::NonRetryable
        } else {
            ErrorCategory::Retryable
        }
    } else {
        ErrorCategory::Retryable
    }
}
