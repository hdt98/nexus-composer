//! Request context.
//!
//! Manages request-lifecycle context and shared initialization.

use crate::app_config::AppType;
use crate::provider::Provider;
use crate::proxy::{
    extract_session_id,
    forwarder::RequestForwarder,
    server::ProxyState,
    types::{AppProxyConfig, CopilotOptimizerConfig, OptimizerConfig, RectifierConfig},
    ProxyError,
};
use axum::http::HeaderMap;
use std::time::Instant;

/// Streaming-timeout configuration.
#[derive(Debug, Clone, Copy)]
pub struct StreamingTimeoutConfig {
    /// Time to first byte in seconds; zero disables it.
    pub first_byte_timeout: u64,
    /// Idle timeout in seconds; zero disables it.
    pub idle_timeout: u64,
}

/// Request context.
///
/// Spans the request lifecycle with timing, per-application proxy settings,
/// failover providers, requested model, log label, and correlation session ID.
pub struct RequestContext {
    /// Request start time.
    pub start_time: Instant,
    /// Per-application proxy settings, including retries and timeouts.
    pub app_config: AppProxyConfig,
    /// Selected provider, first in the failover chain.
    pub provider: Provider,
    /// Complete provider list for failover.
    providers: Vec<Provider>,
    /// Current provider at request start, used to decide whether UI/tray must synchronize.
    ///
    /// Uses the device-level current provider from local settings. If proxy mode
    /// actually selects another provider, switching keeps the UI accurate.
    pub current_provider_id: String,
    /// Model name in the request.
    pub request_model: String,
    /// Model sent upstream after takeover/mapping, populated after forwarding succeeds.
    ///
    /// Usage attribution falls back from upstream response to outbound_model and
    /// then request_model. request_model alone is a pre-mapping client alias under takeover.
    pub outbound_model: Option<String>,
    /// Log label such as Claude, Codex, or Gemini.
    pub tag: &'static str,
    /// Application-type string such as claude, codex, or gemini.
    pub app_type_str: &'static str,
    /// Reserved application type; currently app_type_str is used.
    #[allow(dead_code)]
    pub app_type: AppType,
    /// Session ID extracted from the client or generated.
    pub session_id: String,
    /// Whether the client supplied the session ID. A generated UUID cannot be an
    /// upstream cache key because it changes on every request.
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
    /// * `state` - proxy-server state;
    /// * `body` - JSON request body;
    /// * `headers` - request headers used for session extraction;
    /// * `app_type` - application type;
    /// * `tag` - log label;
    /// * `app_type_str` - application-type string.
    ///
    /// # Errors
    /// Returns ProxyError when provider selection fails.
    pub async fn new(
        state: &ProxyState,
        body: &serde_json::Value,
        headers: &HeaderMap,
        app_type: AppType,
        tag: &'static str,
        app_type_str: &'static str,
    ) -> Result<Self, ProxyError> {
        let start_time = Instant::now();

        // Read per-application proxy settings.
        let app_config = state
            .db
            .get_proxy_config_for_app(app_type_str)
            .await
            .map_err(|e| ProxyError::DatabaseError(e.to_string()))?;

        // Read rectifier configuration.
        let rectifier_config = state.db.get_rectifier_config().unwrap_or_default();
        let optimizer_config = state.db.get_optimizer_config().unwrap_or_default();
        let copilot_optimizer_config = state.db.get_copilot_optimizer_config().unwrap_or_default();

        let current_provider_id =
            crate::settings::get_current_provider(&app_type).unwrap_or_default();

        // Extract model name from the request body.
        let request_model = body
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown")
            .to_string();

        // Extract session ID.
        let session_result = extract_session_id(headers, body, app_type_str);
        let session_id = session_result.session_id.clone();

        log::debug!(
            "[{}] Session ID: {} (from {:?}, client_provided: {})",
            tag,
            session_id,
            session_result.source,
            session_result.client_provided
        );

        // Select through the shared ProviderRouter so circuit state persists. Call
        // once and pass the result to the forwarder to avoid consuming two HalfOpen permits.
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
            session_client_provided: session_result.client_provided,
            rectifier_config,
            optimizer_config,
            copilot_optimizer_config,
        })
    }

    /// Extracts a Gemini model name from the URI.
    ///
    /// Gemini model names appear in URI forms such as:
    /// `/v1beta/models/gemini-pro:generateContent`
    pub fn with_model_from_uri(mut self, uri: &axum::http::Uri) -> Self {
        // Parse path rather than path_and_query so a GET query cannot become part
        // of request_model.
        let endpoint = uri.path();

        self.request_model =
            extract_gemini_model_from_path(endpoint).unwrap_or_else(|| "unknown".to_string());

        self
    }

    /// Creates a RequestForwarder.
    ///
    /// Uses the shared ProviderRouter so circuit state persists across requests.
    ///
    /// Timeouts apply only when failover is enabled, with zero disabling a timeout.
    /// When failover is disabled, all timeout values are passed as zero.
    pub fn create_forwarder(&self, state: &ProxyState) -> RequestForwarder {
        let (non_streaming_timeout, first_byte_timeout, idle_timeout) =
            if self.app_config.auto_failover_enabled {
                // Failover enabled: use configured values; zero disables timeout.
                (
                    self.app_config.non_streaming_timeout as u64,
                    self.app_config.streaming_first_byte_timeout as u64,
                    self.app_config.streaming_idle_timeout as u64,
                )
            } else {
                // Failover disabled: disable timeout configuration.
                log::debug!(
                    "[{}] Failover disabled, timeout configs are bypassed",
                    self.tag
                );
                (0, 0, 0)
            };

        // Without failover, keep at most one same-provider retry for transient
        // pre-stream transport failures. This does not enable provider switching
        // or response timeouts, and the forwarder gates the retry so model/parser
        // failures stay visible.
        let max_retries = if self.app_config.auto_failover_enabled {
            self.app_config.max_retries
        } else {
            self.app_config.max_retries.min(1)
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
    /// Reuses providers selected during context creation instead of selecting again.
    pub fn get_providers(&self) -> Vec<Provider> {
        self.providers.clone()
    }

    /// Returns request latency in milliseconds.
    #[inline]
    pub fn latency_ms(&self) -> u64 {
        self.start_time.elapsed().as_millis() as u64
    }

    /// Returns streaming-timeout configuration.
    ///
    /// Returns configured values when failover is enabled, with zero disabling a
    /// check. Returns zero for every check when failover is disabled.
    #[inline]
    pub fn streaming_timeout_config(&self) -> StreamingTimeoutConfig {
        if self.app_config.auto_failover_enabled {
            // Failover enabled: use configured values; zero disables a check.
            StreamingTimeoutConfig {
                first_byte_timeout: self.app_config.streaming_first_byte_timeout as u64,
                idle_timeout: self.app_config.streaming_idle_timeout as u64,
            }
        } else {
            // Failover disabled: disable streaming timeouts.
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
        // Defensively retain only the model ID even if input contains a query or action.
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
        // `/v1beta/models` (list endpoint) has no following segment, so return None.
        assert_eq!(extract_gemini_model_from_path("/v1beta/models"), None);
    }

    #[test]
    fn extract_model_get_with_query_only() {
        // GET /v1beta/models/<id>?key=... has no action verb, so colon splitting
        // alone would retain the query. Ensure the query is removed.
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
