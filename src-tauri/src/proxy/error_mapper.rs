//! Mapping from proxy errors to HTTP status codes.
//!
//! Maps `ProxyError` to appropriate HTTP status codes for logging and manual
//! error responses.

use super::ProxyError;

/// Map `ProxyError` to an HTTP status code.
///
/// Mapping rules:
/// - Upstream errors: use the upstream status code directly
/// - Timeouts: `504 Gateway Timeout`
/// - Connection failures: `502 Bad Gateway`
/// - No provider available: `503 Service Unavailable`
/// - Retries exhausted: `503 Service Unavailable`
/// - Authentication errors: `401 Unauthorized`
/// - Configuration/request errors: `400 Bad Request`
/// - Transformation errors: `422 Unprocessable Entity`
/// - Other errors: `500 Internal Server Error`
pub fn map_proxy_error_to_status(error: &ProxyError) -> u16 {
    match error {
        // Server state errors: keep these aligned with `IntoResponse`.
        ProxyError::AlreadyRunning => 409,
        ProxyError::NotRunning => 503,

        // Upstream errors: use the actual upstream status code.
        ProxyError::UpstreamError { status, .. } => *status,

        // Timeout errors: `504 Gateway Timeout`.
        ProxyError::Timeout(_) | ProxyError::StreamIdleTimeout(_) => 504,

        // Forwarding and connection failures: `502 Bad Gateway`.
        ProxyError::ForwardFailed(_) => 502,

        // No provider available: `503 Service Unavailable`.
        ProxyError::NoAvailableProvider => 503,

        // All provider circuits are open: `503 Service Unavailable`.
        ProxyError::AllProvidersCircuitOpen => 503,

        // No providers configured: `503 Service Unavailable`.
        ProxyError::NoProvidersConfigured => 503,

        // Retries exhausted: `503 Service Unavailable`.
        ProxyError::MaxRetriesExceeded => 503,

        // Provider unhealthy: `503 Service Unavailable`.
        ProxyError::ProviderUnhealthy(_) => 503,

        // Configuration errors and invalid requests: `400 Bad Request`.
        ProxyError::ConfigError(_) | ProxyError::InvalidRequest(_) => 400,

        // Authentication errors: `401 Unauthorized`.
        ProxyError::AuthError(_) => 401,

        // Database errors: `500 Internal Server Error`.
        ProxyError::DatabaseError(_) => 500,

        // Transformation errors: `422 Unprocessable Entity`.
        ProxyError::TransformError(_) => 422,

        // Other unknown errors: `500 Internal Server Error`.
        _ => 500,
    }
}

/// Convert `ProxyError` to a user-friendly error message.
pub fn get_error_message(error: &ProxyError) -> String {
    match error {
        ProxyError::UpstreamError { status, body } => {
            if let Some(body) = body {
                format!("Upstream error ({status}): {body}")
            } else {
                format!("Upstream error ({status})")
            }
        }
        ProxyError::Timeout(msg) => format!("Request timed out: {msg}"),
        ProxyError::ForwardFailed(msg) => format!("Forwarding failed: {msg}"),
        ProxyError::NoAvailableProvider => "No provider available".to_string(),
        ProxyError::AllProvidersCircuitOpen => {
            "All providers are circuit-open; no route available".to_string()
        }
        ProxyError::NoProvidersConfigured => "No providers configured".to_string(),
        ProxyError::MaxRetriesExceeded => {
            "All providers failed and retries were exhausted".to_string()
        }
        ProxyError::ProviderUnhealthy(msg) => format!("Provider is unhealthy: {msg}"),
        ProxyError::DatabaseError(msg) => format!("Database error: {msg}"),
        ProxyError::TransformError(msg) => format!("Request/response transformation error: {msg}"),
        _ => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_upstream_error() {
        let error = ProxyError::UpstreamError {
            status: 401,
            body: Some("Unauthorized".to_string()),
        };
        assert_eq!(map_proxy_error_to_status(&error), 401);
    }

    #[test]
    fn test_map_timeout_error() {
        let error = ProxyError::Timeout("Request timeout".to_string());
        assert_eq!(map_proxy_error_to_status(&error), 504);
    }

    #[test]
    fn test_map_connection_error() {
        let error = ProxyError::ForwardFailed("Connection refused".to_string());
        assert_eq!(map_proxy_error_to_status(&error), 502);
    }

    #[test]
    fn test_map_no_provider_error() {
        let error = ProxyError::NoAvailableProvider;
        assert_eq!(map_proxy_error_to_status(&error), 503);
    }

    #[test]
    fn test_map_status_matches_proxy_error_response_semantics() {
        assert_eq!(
            map_proxy_error_to_status(&ProxyError::AuthError("bad token".to_string())),
            401
        );
        assert_eq!(
            map_proxy_error_to_status(&ProxyError::ConfigError("bad config".to_string())),
            400
        );
        assert_eq!(
            map_proxy_error_to_status(&ProxyError::InvalidRequest("bad request".to_string())),
            400
        );
        assert_eq!(
            map_proxy_error_to_status(&ProxyError::TransformError("bad transform".to_string())),
            422
        );
        assert_eq!(
            map_proxy_error_to_status(&ProxyError::StreamIdleTimeout(30)),
            504
        );
    }

    #[test]
    fn test_get_error_message() {
        let error = ProxyError::UpstreamError {
            status: 500,
            body: Some("Internal Server Error".to_string()),
        };
        let msg = get_error_message(&error);
        assert!(msg.contains("Upstream error"));
        assert!(msg.contains("500"));
        assert!(msg.contains("Internal Server Error"));
    }
}
