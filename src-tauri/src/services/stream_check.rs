//! Provider reachability-check service.
//!
//! Probes only whether a provider's `base_url` is reachable and **does not send a real model request**:
//! - Any HTTP response, including 200, 4xx, or 5xx, means reachable because the port and gateway work.
//! - Only network-level DNS, connection-refused, TLS, timeout, and similar errors mean unreachable.
//! - Latency is time to response headers (TTFB), representing an actual round trip.
//!
//! ## Design tradeoff: reachable does not mean correctly configured
//!
//! This check intentionally avoids authentication and model validation so a third-party provider's
//! auth gate or model validation cannot be mistaken for unavailability. It therefore cannot confirm
//! whether credentials are valid or a model exists.
//!
//! ## Relationship to failover: important invariant
//!
//! A reachability check **never** changes the failover circuit breaker. A provider returning 401 or
//! 403 is reachable here but unusable for real traffic. Only success or failure of real traffic
//! forwarded by `proxy/forwarder.rs` drives the circuit breaker passively. Reachability answers
//! "can we connect?" while real traffic answers "does it work?".

use reqwest::header::HeaderValue;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Instant;

use crate::app_config::AppType;
use crate::error::AppError;
use crate::provider::Provider;
use crate::proxy::providers::{get_adapter, ClaudeAdapter, ProviderAdapter};

/// Health status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum HealthStatus {
    Operational,
    Degraded,
    Failed,
}

/// Reachability-check configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamCheckConfig {
    /// Timeout for one probe in seconds.
    pub timeout_secs: u64,
    /// Maximum retries for timeout-class failures.
    pub max_retries: u32,
    /// Degraded threshold in milliseconds: a reachable endpoint whose TTFB exceeds this is slow.
    pub degraded_threshold_ms: u64,
}

impl Default for StreamCheckConfig {
    fn default() -> Self {
        // The reachability probe sends a small base_url request and reads only headers without waiting
        // for model generation, so its timeout is much shorter than the old real-request check
        // (45s → 8s). Retain the old 6000 ms degraded threshold: probe TTFB is normally much lower,
        // marking only genuinely slow endpoints rather than normal latency slightly above one second.
        Self {
            timeout_secs: 8,
            max_retries: 1,
            degraded_threshold_ms: 6000,
        }
    }
}

/// Reachability-check result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamCheckResult {
    pub status: HealthStatus,
    pub success: bool,
    pub message: String,
    pub response_time_ms: Option<u64>,
    pub http_status: Option<u16>,
    /// Retained for compatibility with the `stream_check_logs` schema; always empty for reachability checks.
    pub model_used: String,
    pub tested_at: i64,
    pub retry_count: u32,
    /// Fine-grained error classification; reachability checks no longer subdivide errors and always use None.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_category: Option<String>,
}

/// Reachability-check service.
pub struct StreamCheckService;

impl StreamCheckService {
    /// Runs a reachability check, retrying only timeout-class failures.
    ///
    /// `base_url_override` supports providers such as Copilot whose endpoints are resolved dynamically
    /// by an OAuth manager. The command layer resolves and supplies it. Other providers pass `None`,
    /// and this service extracts the URL from `settings_config`.
    pub async fn check_with_retry(
        app_type: &AppType,
        provider: &Provider,
        config: &StreamCheckConfig,
        base_url_override: Option<String>,
    ) -> Result<StreamCheckResult, AppError> {
        let effective = Self::merge_provider_config(provider, config);

        let mut last_result: Option<StreamCheckResult> = None;
        for attempt in 0..=effective.max_retries {
            let start = Instant::now();
            let result = Self::check_once(
                app_type,
                provider,
                &effective,
                base_url_override.clone(),
                start,
            )
            .await?;

            if result.success {
                return Ok(StreamCheckResult {
                    retry_count: attempt,
                    ..result
                });
            }

            // Retry only timeout or abort-class network instability. Return immediately for refused
            // connections, DNS failures, and other deterministic errors.
            if Self::should_retry(&result.message) && attempt < effective.max_retries {
                last_result = Some(result);
                continue;
            }
            return Ok(StreamCheckResult {
                retry_count: attempt,
                ..result
            });
        }

        Ok(last_result.unwrap_or_else(|| StreamCheckResult {
            status: HealthStatus::Failed,
            success: false,
            message: "Check failed".to_string(),
            response_time_ms: None,
            http_status: None,
            model_used: String::new(),
            tested_at: chrono::Utc::now().timestamp(),
            retry_count: effective.max_retries,
            error_category: None,
        }))
    }

    /// Merges provider-specific `meta.testConfig`, when enabled, with global configuration.
    fn merge_provider_config(provider: &Provider, global: &StreamCheckConfig) -> StreamCheckConfig {
        let tc = provider
            .meta
            .as_ref()
            .and_then(|m| m.test_config.as_ref())
            .filter(|tc| tc.enabled);

        match tc {
            Some(tc) => StreamCheckConfig {
                timeout_secs: tc.timeout_secs.unwrap_or(global.timeout_secs),
                max_retries: tc.max_retries.unwrap_or(global.max_retries),
                degraded_threshold_ms: tc
                    .degraded_threshold_ms
                    .unwrap_or(global.degraded_threshold_ms),
            },
            None => global.clone(),
        }
    }

    /// Runs one reachability probe.
    async fn check_once(
        app_type: &AppType,
        provider: &Provider,
        config: &StreamCheckConfig,
        base_url_override: Option<String>,
        start: Instant,
    ) -> Result<StreamCheckResult, AppError> {
        let base_url = match base_url_override {
            Some(b) => b,
            None => Self::resolve_base_url(app_type, provider)?,
        };

        let client = crate::proxy::http_client::get();
        let timeout = std::time::Duration::from_secs(config.timeout_secs);
        let ua = Self::custom_user_agent(provider);

        let result = Self::probe_reachability(&client, &base_url, timeout, ua).await;
        let response_time = start.elapsed().as_millis() as u64;
        Ok(Self::build_result(
            result,
            response_time,
            config.degraded_threshold_ms,
        ))
    }

    /// Resolves a provider `base_url`.
    ///
    /// A reachability probe needs only the base origin or user-configured base path. Any HTTP response
    /// proves the port is reachable, so unlike the old real-request check it does not resolve a specific
    /// API path such as `/v1/messages`, `/chat/completions`, or `:streamGenerateContent`.
    ///
    /// Official providers (`category == "official"`) intentionally leave base_url empty and use the
    /// client's default or OAuth endpoint. Nexus Composer has no reliable target to probe, so the
    /// renderer hides their reachability button in `ProviderCard.tsx`. Extraction may return an error
    /// here without falling back to an official endpoint.
    fn resolve_base_url(app_type: &AppType, provider: &Provider) -> Result<String, AppError> {
        match app_type {
            // Additive applications use settings_config structures different from Claude, Codex, and
            // Gemini. Extract base_url directly by each convention instead of using an adapter.
            AppType::OpenCode => {
                let npm = Self::extract_opencode_npm(provider);
                Self::resolve_opencode_base_url(provider, npm.as_deref())
            }
            AppType::OpenClaw => Self::extract_openclaw_base_url(provider),
            AppType::Hermes => Self::extract_hermes_base_url(provider),
            AppType::ClaudeDesktop => ClaudeAdapter::new()
                .extract_base_url(provider)
                .map_err(|e| AppError::Message(format!("Failed to extract base_url: {e}"))),
            _ => get_adapter(app_type)
                .extract_base_url(provider)
                .map_err(|e| AppError::Message(format!("Failed to extract base_url: {e}"))),
        }
    }

    /// Lightweight reachability probe: GET `base_url`; any HTTP response means reachable.
    ///
    /// - `send()` returns when headers arrive, naturally measuring TTFB without reading the body.
    /// - reqwest returns `Ok` for every HTTP status and `Err` only for network failures, exactly
    ///   matching the rule that any response is reachable and only connection failure is not.
    async fn probe_reachability(
        client: &Client,
        base_url: &str,
        timeout: std::time::Duration,
        custom_ua: Option<HeaderValue>,
    ) -> Result<u16, AppError> {
        let url = base_url.trim();
        if url.is_empty() {
            return Err(AppError::Message("base_url is empty".to_string()));
        }

        let mut req = client
            .get(url)
            .timeout(timeout)
            .header("accept", "*/*")
            .header("accept-encoding", "identity");
        // Reuse the provider's custom user agent because some gateways allowlist it, matching the
        // forwarding path.
        if let Some(ua) = custom_ua {
            req = req.header("user-agent", ua);
        }

        match req.send().await {
            Ok(resp) => Ok(resp.status().as_u16()),
            Err(e) => Err(Self::map_request_error(e)),
        }
    }

    /// Wraps a raw probe result in `StreamCheckResult`.
    fn build_result(
        result: Result<u16, AppError>,
        response_time: u64,
        degraded_threshold_ms: u64,
    ) -> StreamCheckResult {
        let tested_at = chrono::Utc::now().timestamp();
        match result {
            Ok(status) => StreamCheckResult {
                status: Self::determine_status(response_time, degraded_threshold_ms),
                success: true,
                message: "Reachable".to_string(),
                response_time_ms: Some(response_time),
                http_status: Some(status),
                model_used: String::new(),
                tested_at,
                retry_count: 0,
                error_category: None,
            },
            Err(e) => StreamCheckResult {
                status: HealthStatus::Failed,
                success: false,
                message: e.to_string(),
                response_time_ms: Some(response_time),
                http_status: None,
                model_used: String::new(),
                tested_at,
                retry_count: 0,
                error_category: None,
            },
        }
    }

    fn determine_status(latency_ms: u64, threshold: u64) -> HealthStatus {
        if latency_ms <= threshold {
            HealthStatus::Operational
        } else {
            HealthStatus::Degraded
        }
    }

    fn should_retry(msg: &str) -> bool {
        let lower = msg.to_lowercase();
        lower.contains("timeout") || lower.contains("abort") || lower.contains("timed out")
    }

    fn map_request_error(e: reqwest::Error) -> AppError {
        if e.is_timeout() {
            AppError::Message("Request timeout".to_string())
        } else if e.is_connect() {
            AppError::Message(format!("Connection failed: {e}"))
        } else {
            AppError::Message(e.to_string())
        }
    }

    /// Provider-level custom user agent from `meta.customUserAgent`, following the forwarding path's
    /// policy: trim it, treat an empty value as unset, and silently ignore invalid values by returning None.
    fn custom_user_agent(provider: &Provider) -> Option<HeaderValue> {
        provider
            .meta
            .as_ref()
            .and_then(|meta| meta.custom_user_agent_header().ok().flatten())
    }

    // ===== Per-application base_url extraction for distinct settings_config structures =====

    /// OpenClaw: `{ baseUrl, apiKey, api, ... }` (camelCase).
    fn extract_openclaw_base_url(provider: &Provider) -> Result<String, AppError> {
        provider
            .settings_config
            .get("baseUrl")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                AppError::localized(
                    "openclaw_base_url_missing",
                    "The OpenClaw provider is missing baseUrl",
                    "OpenClaw provider is missing `baseUrl`",
                )
            })
    }

    /// Hermes: `{ base_url, api_key, api_mode }` (snake_case).
    fn extract_hermes_base_url(provider: &Provider) -> Result<String, AppError> {
        provider
            .settings_config
            .get("base_url")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                AppError::localized(
                    "hermes_base_url_missing",
                    "The Hermes provider is missing base_url",
                    "Hermes provider is missing `base_url`",
                )
            })
    }

    /// OpenCode: `{ npm, options: { baseURL, apiKey }, ... }`
    ///
    /// When the user does not explicitly set `options.baseURL`, falls back by `npm` AI SDK package
    /// to the package's built-in default endpoint. `@ai-sdk/openai-compatible` has no default and
    /// requires an explicit value.
    fn resolve_opencode_base_url(
        provider: &Provider,
        npm: Option<&str>,
    ) -> Result<String, AppError> {
        if let Some(explicit) = Self::extract_opencode_base_url(provider) {
            return Ok(explicit);
        }

        let fallback = match npm {
            Some("@ai-sdk/openai") => Some("https://api.openai.com/v1"),
            Some("@ai-sdk/anthropic") => Some("https://api.anthropic.com"),
            Some("@ai-sdk/google") => Some("https://generativelanguage.googleapis.com"),
            _ => None,
        };

        fallback.map(|s| s.to_string()).ok_or_else(|| {
            AppError::localized(
                "opencode_base_url_missing",
                "The OpenCode provider is missing options.baseURL and the current SDK package has no default endpoint",
                "OpenCode provider is missing `options.baseURL` and the SDK package has no default endpoint",
            )
        })
    }

    fn extract_opencode_base_url(provider: &Provider) -> Option<String> {
        provider
            .settings_config
            .get("options")
            .and_then(|v| v.get("baseURL"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn extract_opencode_npm(provider: &Provider) -> Option<String> {
        provider
            .settings_config
            .get("npm")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_provider(settings_config: serde_json::Value) -> Provider {
        Provider::with_id(
            "test".to_string(),
            "Test".to_string(),
            settings_config,
            None,
        )
    }

    #[test]
    fn test_default_config_uses_reachability_friendly_values() {
        let config = StreamCheckConfig::default();
        assert_eq!(config.timeout_secs, 8);
        assert_eq!(config.max_retries, 1);
        // Retain the previous degraded threshold so normal latency slightly above one second is not marked slow.
        assert_eq!(config.degraded_threshold_ms, 6000);
    }

    #[test]
    fn test_determine_status() {
        assert_eq!(
            StreamCheckService::determine_status(1000, 1500),
            HealthStatus::Operational
        );
        assert_eq!(
            StreamCheckService::determine_status(1500, 1500),
            HealthStatus::Operational
        );
        assert_eq!(
            StreamCheckService::determine_status(1501, 1500),
            HealthStatus::Degraded
        );
    }

    #[test]
    fn test_should_retry_only_on_timeout_like_errors() {
        assert!(StreamCheckService::should_retry("Request timeout"));
        assert!(StreamCheckService::should_retry("request timed out"));
        assert!(StreamCheckService::should_retry("connection abort"));
        // Do not retry refused connections or DNS failures.
        assert!(!StreamCheckService::should_retry(
            "Connection failed: dns error"
        ));
        assert!(!StreamCheckService::should_retry("Reachable"));
    }

    #[test]
    fn test_build_result_any_http_status_is_reachable() {
        // Every HTTP status means reachable (success=true).
        for status in [200u16, 401, 403, 404, 429, 500, 503] {
            let r = StreamCheckService::build_result(Ok(status), 100, 1500);
            assert!(r.success, "status {status} should be reachable");
            assert_eq!(r.status, HealthStatus::Operational);
            assert_eq!(r.http_status, Some(status));
            assert!(r.model_used.is_empty());
            assert!(r.error_category.is_none());
        }
    }

    #[test]
    fn test_build_result_network_error_is_unreachable() {
        let r = StreamCheckService::build_result(
            Err(AppError::Message("Connection failed: refused".to_string())),
            5,
            1500,
        );
        assert!(!r.success);
        assert_eq!(r.status, HealthStatus::Failed);
        assert!(r.http_status.is_none());
    }

    #[test]
    fn test_build_result_slow_response_is_degraded() {
        let r = StreamCheckService::build_result(Ok(200), 3000, 1500);
        assert!(r.success);
        assert_eq!(r.status, HealthStatus::Degraded);
    }

    #[test]
    fn test_merge_provider_config_override_and_default() {
        use crate::provider::{ProviderMeta, ProviderTestConfig};

        let global = StreamCheckConfig::default();

        // Without testConfig, use global configuration.
        let p = make_provider(serde_json::json!({}));
        let merged = StreamCheckService::merge_provider_config(&p, &global);
        assert_eq!(merged.timeout_secs, global.timeout_secs);

        // Enabled testConfig overrides selected fields.
        let mut p2 = make_provider(serde_json::json!({}));
        p2.meta = Some(ProviderMeta {
            test_config: Some(ProviderTestConfig {
                enabled: true,
                timeout_secs: Some(20),
                degraded_threshold_ms: Some(3000),
                max_retries: None,
            }),
            ..Default::default()
        });
        let merged2 = StreamCheckService::merge_provider_config(&p2, &global);
        assert_eq!(merged2.timeout_secs, 20);
        assert_eq!(merged2.degraded_threshold_ms, 3000);
        assert_eq!(merged2.max_retries, global.max_retries); // Not overridden; use global.

        // Ignore a present but disabled testConfig and use global configuration.
        let mut p3 = make_provider(serde_json::json!({}));
        p3.meta = Some(ProviderMeta {
            test_config: Some(ProviderTestConfig {
                enabled: false,
                timeout_secs: Some(99),
                degraded_threshold_ms: None,
                max_retries: None,
            }),
            ..Default::default()
        });
        let merged3 = StreamCheckService::merge_provider_config(&p3, &global);
        assert_eq!(merged3.timeout_secs, global.timeout_secs);
    }

    #[test]
    fn test_resolve_opencode_base_url_explicit_wins() {
        let p = make_provider(serde_json::json!({
            "npm": "@ai-sdk/openai",
            "options": { "baseURL": "https://proxy.local/v1", "apiKey": "k" },
            "models": {},
        }));
        let resolved =
            StreamCheckService::resolve_opencode_base_url(&p, Some("@ai-sdk/openai")).unwrap();
        assert_eq!(resolved, "https://proxy.local/v1");
    }

    #[test]
    fn test_resolve_opencode_base_url_falls_back_for_known_npm() {
        let p = make_provider(serde_json::json!({
            "npm": "@ai-sdk/anthropic",
            "options": { "apiKey": "k" },
            "models": {},
        }));
        let resolved =
            StreamCheckService::resolve_opencode_base_url(&p, Some("@ai-sdk/anthropic")).unwrap();
        assert_eq!(resolved, "https://api.anthropic.com");
    }

    #[test]
    fn test_resolve_opencode_base_url_errors_for_openai_compatible_without_url() {
        let p = make_provider(serde_json::json!({
            "npm": "@ai-sdk/openai-compatible",
            "options": { "apiKey": "k" },
            "models": {},
        }));
        let result =
            StreamCheckService::resolve_opencode_base_url(&p, Some("@ai-sdk/openai-compatible"));
        assert!(result.is_err());
    }

    #[test]
    fn test_extract_openclaw_base_url_missing_errors() {
        let p = make_provider(serde_json::json!({ "apiKey": "k", "api": "openai-completions" }));
        assert!(StreamCheckService::extract_openclaw_base_url(&p).is_err());

        let p2 = make_provider(serde_json::json!({ "baseUrl": "https://api.deepseek.com/v1" }));
        assert_eq!(
            StreamCheckService::extract_openclaw_base_url(&p2).unwrap(),
            "https://api.deepseek.com/v1"
        );
    }

    #[test]
    fn test_resolve_base_url_uses_explicit_url_or_errors_when_missing() {
        // Use an explicit base_url directly.
        let p = make_provider(
            serde_json::json!({ "env": { "ANTHROPIC_BASE_URL": "https://relay.example/v1" } }),
        );
        assert_eq!(
            StreamCheckService::resolve_base_url(&AppType::Claude, &p).unwrap(),
            "https://relay.example/v1"
        );

        // Missing base_url, whether intentionally empty for an official provider or omitted by the
        // user, is an error. The renderer hides checks for official providers, so they do not reach
        // this path. Do not fall back to an official endpoint because that could incorrectly mark a
        // third-party provider with a missing URL as reachable.
        let empty = make_provider(serde_json::json!({ "env": {} }));
        assert!(StreamCheckService::resolve_base_url(&AppType::Claude, &empty).is_err());
    }
}
