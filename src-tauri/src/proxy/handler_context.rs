//! Request context module.
//!
//! Manages request lifecycle context and shared initialization.

use crate::app_config::AppType;
use crate::provider::Provider;
use crate::proxy::session::{extract_harness_identity, extract_request_id, HarnessIdentity};
use crate::proxy::{
    extract_session_id,
    forwarder::RequestForwarder,
    server::ProxyState,
    types::{AppProxyConfig, CopilotOptimizerConfig, OptimizerConfig, RectifierConfig},
    ProxyError,
};
use axum::http::HeaderMap;
use std::time::Instant;

/// Streaming timeout configuration.
#[derive(Debug, Clone, Copy)]
pub struct StreamingTimeoutConfig {
    /// First-byte timeout in seconds. Zero disables it.
    pub first_byte_timeout: u64,
    /// Idle timeout in seconds. Zero disables it.
    pub idle_timeout: u64,
}

/// Request context.
///
/// Spans the complete request lifecycle and contains:
/// - Timing information
/// - Per-app proxy configuration
/// - Selected providers for failover
/// - Requested model name
/// - Log tag
/// - Session ID for log correlation
pub struct RequestContext {
    /// Request start time.
    pub start_time: Instant,
    /// Per-app proxy configuration, including retries and timeouts.
    pub app_config: AppProxyConfig,
    /// First selected provider in the failover chain.
    pub provider: Provider,
    /// Complete provider list for failover.
    providers: Vec<Provider>,
    /// Current provider at request start, used to determine whether to sync the UI and tray.
    ///
    /// This is the device-level current provider from local settings. In proxy mode,
    /// using a different provider triggers a switch so the UI remains accurate.
    pub current_provider_id: String,
    /// Model name from the request.
    pub request_model: String,
    /// Actual model sent upstream after route takeover or model mapping; set after forwarding succeeds.
    ///
    /// Usage attribution falls back from the model echoed upstream, to `outbound_model`,
    /// then to `request_model`. Route takeover may leave `request_model` as the client's
    /// pre-mapping alias, so it cannot be the first fallback.
    pub outbound_model: Option<String>,
    /// Log tag such as "Claude", "Codex", or "Gemini".
    pub tag: &'static str,
    /// App type string such as "claude", "codex", or "gemini".
    pub app_type_str: &'static str,
    /// App type, reserved while current logic uses `app_type_str`.
    #[allow(dead_code)]
    pub app_type: AppType,
    /// Session ID extracted from the client request or generated locally.
    pub session_id: String,
    /// Correlation values held for the complete logical request.
    request_id: String,
    harness_identity: Option<HarnessIdentity>,
    /// Whether the client provided the session ID. A generated UUID cannot be an
    /// upstream cache key because it changes for every request.
    pub session_client_provided: bool,
    /// Rectifier configuration.
    pub rectifier_config: RectifierConfig,
    /// Optimizer configuration.
    pub optimizer_config: OptimizerConfig,
    /// Copilot optimizer configuration.
    pub copilot_optimizer_config: CopilotOptimizerConfig,
}

impl RequestContext {
    /// Creates a request context.
    ///
    /// # Arguments
    /// * `state` - Proxy server state
    /// * `body` - JSON request body
    /// * `headers` - Request headers used to extract the session ID
    /// * `app_type` - App type
    /// * `tag` - Log tag
    /// * `app_type_str` - App type string
    ///
    /// # Errors
    /// Returns `ProxyError` when provider selection fails.
    pub async fn new(
        state: &ProxyState,
        body: &serde_json::Value,
        headers: &HeaderMap,
        app_type: AppType,
        tag: &'static str,
        app_type_str: &'static str,
    ) -> Result<Self, ProxyError> {
        let start_time = Instant::now();

        // Load the per-app proxy configuration from the database.
        let app_config = state
            .db
            .get_proxy_config_for_app(app_type_str)
            .await
            .map_err(|e| ProxyError::DatabaseError(e.to_string()))?;

        // Load optimizer configurations from the database.
        let rectifier_config = state.db.get_rectifier_config().unwrap_or_default();
        let optimizer_config = state.db.get_optimizer_config().unwrap_or_default();
        let copilot_optimizer_config = state.db.get_copilot_optimizer_config().unwrap_or_default();

        let current_provider_id =
            crate::settings::get_current_provider(&app_type).unwrap_or_default();

        // Extract the model name from the request body.
        let request_model = body
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown")
            .to_string();

        // Extract correlation data.
        let session_result = extract_session_id(headers, body, app_type_str);
        let session_id = session_result.session_id.clone();
        let request_id = extract_request_id(headers);
        let harness_identity = extract_harness_identity(headers, body, app_type_str);

        log::debug!(
            "[{}] Session ID: {} (from {:?}, client_provided: {})",
            tag,
            session_id,
            session_result.source,
            session_result.client_provided
        );

        // Select providers through the shared router so circuit state persists across requests.
        // Select only once and pass the result to the forwarder to avoid consuming another
        // half-open permit.
        let providers = state
            .provider_router
            .select_providers(app_type_str)
            .await
            .map_err(|e| match e {
                crate::error::AppError::AllProvidersCircuitOpen => {
                    ProxyError::AllProvidersCircuitOpen
                }
                crate::error::AppError::NoProvidersConfigured => ProxyError::NoProvidersConfigured,
                _ => ProxyError::DatabaseError(e.to_string()),
            })?;

        let provider = providers
            .first()
            .cloned()
            .ok_or(ProxyError::NoAvailableProvider)?;

        log::debug!(
            "[{}] Provider: {}, model: {}, failover chain: {} providers, session: {}",
            tag,
            provider.name,
            request_model,
            providers.len(),
            session_id
        );

        Ok(Self {
            start_time,
            app_config,
            provider,
            providers,
            current_provider_id,
            request_model,
            outbound_model: None,
            tag,
            app_type_str,
            app_type,
            session_id,
            request_id,
            harness_identity,
            session_client_provided: session_result.client_provided,
            rectifier_config,
            optimizer_config,
            copilot_optimizer_config,
        })
    }

    /// Extracts the Gemini model name from the URI.
    ///
    /// Gemini API model names appear in URIs such as:
    /// `/v1beta/models/gemini-pro:generateContent`
    pub fn with_model_from_uri(mut self, uri: &axum::http::Uri) -> Self {
        // Parse only path segments so GET /v1beta/models/<id>?key=... does not append the
        // query string to `request_model`.
        let endpoint = uri.path();

        self.request_model =
            extract_gemini_model_from_path(endpoint).unwrap_or_else(|| "unknown".to_string());

        self
    }

    /// Creates a `RequestForwarder`.
    ///
    /// Uses the shared `ProviderRouter` so circuit state persists across requests.
    ///
    /// Configuration rules:
    /// - With failover enabled, configured timeouts apply; zero disables a timeout.
    /// - With failover disabled, all timeout values are zero.
    pub fn create_forwarder(&self, state: &ProxyState) -> RequestForwarder {
        let (non_streaming_timeout, first_byte_timeout, idle_timeout) =
            if self.app_config.auto_failover_enabled {
                // Use configured values when failover is enabled; zero disables a timeout.
                (
                    self.app_config.non_streaming_timeout as u64,
                    self.app_config.streaming_first_byte_timeout as u64,
                    self.app_config.streaming_idle_timeout as u64,
                )
            } else {
                // Disable configured timeouts when failover is disabled.
                log::debug!(
                    "[{}] Failover disabled, timeout configs are bypassed",
                    self.tag
                );
                (0, 0, 0)
            };

        // Force max_retries to zero when failover is disabled, allowing one provider attempt
        // without timeout or switching behavior.
        let max_retries = if self.app_config.auto_failover_enabled {
            self.app_config.max_retries
        } else {
            0
        };

        RequestForwarder::new(
            state.provider_router.clone(),
            non_streaming_timeout,
            state.status.clone(),
            state.current_providers.clone(),
            state.gemini_shadow.clone(),
            state.codex_chat_history.clone(),
            state.failover_manager.clone(),
            state.app_handle.clone(),
            self.current_provider_id.clone(),
            self.session_id.clone(),
            self.session_client_provided,
            self.request_id.clone(),
            self.harness_identity.clone(),
            first_byte_timeout,
            idle_timeout,
            self.rectifier_config.clone(),
            self.optimizer_config.clone(),
            self.copilot_optimizer_config.clone(),
            max_retries,
        )
    }

    /// Returns the provider list for failover.
    ///
    /// Reuses providers selected during context creation to avoid another
    /// `select_providers()` call.
    pub fn get_providers(&self) -> Vec<Provider> {
        self.providers.clone()
    }

    /// ID sent upstream as `x-request-id`, when the selected provider declares
    /// correlation support.
    pub(crate) fn correlation_id(&self) -> Option<String> {
        self.provider
            .uses_server_request_correlation()
            .then(|| self.request_id.clone())
    }

    /// Canonical request ID extracted from the harness request headers, or a
    /// generated fallback when the client did not provide one.
    pub(crate) fn request_id(&self) -> &str {
        &self.request_id
    }

    /// Returns request latency in milliseconds.
    #[inline]
    pub fn latency_ms(&self) -> u64 {
        self.start_time.elapsed().as_millis() as u64
    }

    /// Returns the streaming timeout configuration.
    ///
    /// Configuration rules:
    /// - With failover enabled, returns configured values; zero disables timeout checks.
    /// - With failover disabled, returns zero to disable timeout checks.
    #[inline]
    pub fn streaming_timeout_config(&self) -> StreamingTimeoutConfig {
        if self.app_config.auto_failover_enabled {
            // Use configured values when failover is enabled; zero disables a timeout.
            StreamingTimeoutConfig {
                first_byte_timeout: self.app_config.streaming_first_byte_timeout as u64,
                idle_timeout: self.app_config.streaming_idle_timeout as u64,
            }
        } else {
            // Disable streaming timeout checks when failover is disabled.
            StreamingTimeoutConfig {
                first_byte_timeout: 0,
                idle_timeout: 0,
            }
        }
    }
}

/// Pull the Gemini model name out of an API path.
///
/// Accepts forms like `/v1beta/models/gemini-pro:generateContent`,
/// `/v1/models/gemini-1.5-flash`, `gemini/v1beta/models/<model>:streamGenerateContent`.
/// Returns `None` when no `models/<name>` segment is present.
pub(crate) fn extract_gemini_model_from_path(endpoint: &str) -> Option<String> {
    let segments: Vec<&str> = endpoint.split('/').collect();
    segments
        .iter()
        .position(|s| *s == "models")
        .and_then(|i| segments.get(i + 1).copied())
        // Defensively retain only the model ID if the caller includes `?` or `:action`.
        .map(|s| s.split('?').next().unwrap_or(s))
        .map(|s| s.split(':').next().unwrap_or(s))
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::extract_gemini_model_from_path;

    #[test]
    fn extract_model_with_action() {
        assert_eq!(
            extract_gemini_model_from_path("/v1beta/models/gemini-pro:generateContent").as_deref(),
            Some("gemini-pro"),
        );
    }

    #[test]
    fn extract_model_with_dotted_version() {
        assert_eq!(
            extract_gemini_model_from_path("/v1beta/models/gemini-1.5-flash:streamGenerateContent")
                .as_deref(),
            Some("gemini-1.5-flash"),
        );
    }

    #[test]
    fn extract_model_without_action() {
        assert_eq!(
            extract_gemini_model_from_path("/v1/models/gemini-1.5-pro").as_deref(),
            Some("gemini-1.5-pro"),
        );
    }

    #[test]
    fn extract_model_with_proxy_prefix() {
        assert_eq!(
            extract_gemini_model_from_path("/gemini/v1beta/models/gemini-2.0-flash:countTokens")
                .as_deref(),
            Some("gemini-2.0-flash"),
        );
    }

    #[test]
    fn extract_model_with_query_string() {
        assert_eq!(
            extract_gemini_model_from_path("/v1beta/models/gemini-pro:generateContent?key=abc")
                .as_deref(),
            Some("gemini-pro"),
        );
    }

    #[test]
    fn extract_model_missing_segment() {
        assert_eq!(extract_gemini_model_from_path("/v1beta/operations"), None);
    }

    #[test]
    fn extract_model_trailing_models_segment() {
        // `/v1beta/models` (list endpoint) has no following segment → None.
        assert_eq!(extract_gemini_model_from_path("/v1beta/models"), None);
    }

    #[test]
    fn extract_model_get_with_query_only() {
        // GET /v1beta/models/<id>?key=... has no action verb, so splitting only on ':'
        // would include the query in the model name. The query must be stripped.
        assert_eq!(
            extract_gemini_model_from_path("/v1beta/models/gemini-pro?key=abc").as_deref(),
            Some("gemini-pro"),
        );
    }

    #[test]
    fn extract_model_get_with_proxy_prefix_and_query() {
        assert_eq!(
            extract_gemini_model_from_path("/gemini/v1beta/models/gemini-2.0-flash?key=abc")
                .as_deref(),
            Some("gemini-2.0-flash"),
        );
    }
}
