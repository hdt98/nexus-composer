use serde::{Deserialize, Serialize};

/// Proxy-server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    /// Listen address.
    pub listen_address: String,
    /// Listen port.
    pub listen_port: u16,
    /// Maximum retries.
    pub max_retries: u8,
    /// Deprecated request timeout in seconds, retained for compatibility.
    pub request_timeout: u64,
    /// Whether logging is enabled.
    pub enable_logging: bool,
    /// Whether live configuration is under takeover.
    #[serde(default)]
    pub live_takeover_active: bool,
    /// Streaming time to first byte, 1-120 seconds; default 60.
    #[serde(default = "default_streaming_first_byte_timeout")]
    pub streaming_first_byte_timeout: u64,
    /// Streaming idle timeout between chunks, 60-600 seconds; zero disables it.
    #[serde(default = "default_streaming_idle_timeout")]
    pub streaming_idle_timeout: u64,
    /// Total non-streaming timeout, 60-1200 seconds; default 600.
    #[serde(default = "default_non_streaming_timeout")]
    pub non_streaming_timeout: u64,
}

fn default_streaming_first_byte_timeout() -> u64 {
    60
}

fn default_streaming_idle_timeout() -> u64 {
    120
}

fn default_non_streaming_timeout() -> u64 {
    600
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            listen_address: "127.0.0.1".to_string(),
            listen_port: 15721, // Less commonly occupied high port.
            max_retries: 3,
            request_timeout: 600,
            enable_logging: true,
            live_takeover_active: false,
            streaming_first_byte_timeout: 60,
            streaming_idle_timeout: 120,
            non_streaming_timeout: 600,
        }
    }
}

/// Proxy-server status.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProxyStatus {
    /// Whether the server is running.
    pub running: bool,
    /// Listen address.
    pub address: String,
    /// Listen port.
    pub port: u16,
    /// Active connections.
    pub active_connections: usize,
    /// Total requests.
    pub total_requests: u64,
    /// Successful requests.
    pub success_requests: u64,
    /// Failed requests.
    pub failed_requests: u64,
    /// Success rate from 0 to 100.
    pub success_rate: f32,
    /// Uptime in seconds.
    pub uptime_seconds: u64,
    /// Active provider name.
    pub current_provider: Option<String>,
    /// Active provider ID.
    pub current_provider_id: Option<String>,
    /// Last request time.
    pub last_request_at: Option<String>,
    /// Last error.
    pub last_error: Option<String>,
    /// Provider failover count.
    pub failover_count: u64,
    /// Active proxy targets.
    #[serde(default)]
    pub active_targets: Vec<ActiveTarget>,
}

/// Active proxy-target information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveTarget {
    pub app_type: String, // "Claude" | "Codex" | "Gemini"
    pub provider_name: String,
    pub provider_id: String,
}

/// Proxy-server information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyServerInfo {
    pub address: String,
    pub port: u16,
    pub started_at: String,
}

/// Per-application takeover state indicating whether live config points locally.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProxyTakeoverStatus {
    pub claude: bool,
    pub codex: bool,
    pub gemini: bool,
    pub opencode: bool,
    pub openclaw: bool,
}

/// Reserved API-format type; no transformation is currently needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ApiFormat {
    Claude,
    OpenAI,
    Gemini,
}

/// Provider health state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderHealth {
    pub provider_id: String,
    pub app_type: String,
    pub is_healthy: bool,
    pub consecutive_failures: u32,
    pub last_success_at: Option<String>,
    pub last_failure_at: Option<String>,
    pub last_error: Option<String>,
    pub updated_at: String,
}

/// Live-configuration backup record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveBackup {
    /// Application type: claude, codex, or gemini.
    pub app_type: String,
    /// Original JSON configuration.
    pub original_config: String,
    /// Backup time.
    pub backed_up_at: String,
}

/// Global proxy configuration mirrored across three rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalProxyConfig {
    /// Master proxy switch.
    pub proxy_enabled: bool,
    /// Listen address.
    pub listen_address: String,
    /// Listen port.
    pub listen_port: u16,
    /// Whether logging is enabled.
    pub enable_logging: bool,
}

/// Independent per-application proxy configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppProxyConfig {
    /// Application type: claude, codex, or gemini.
    pub app_type: String,
    /// Proxy enablement for this application.
    pub enabled: bool,
    /// Automatic failover for this application.
    pub auto_failover_enabled: bool,
    /// Maximum retries.
    pub max_retries: u32,
    /// Streaming first-byte timeout in seconds.
    pub streaming_first_byte_timeout: u32,
    /// Streaming idle timeout in seconds.
    pub streaming_idle_timeout: u32,
    /// Total non-streaming timeout in seconds.
    pub non_streaming_timeout: u32,
    /// Circuit failure threshold.
    pub circuit_failure_threshold: u32,
    /// Circuit recovery threshold.
    pub circuit_success_threshold: u32,
    /// Circuit recovery delay in seconds.
    pub circuit_timeout_seconds: u32,
    /// Error-rate threshold.
    pub circuit_error_rate_threshold: f64,
    /// Minimum requests before calculating error rate.
    pub circuit_min_requests: u32,
}

pub const MAX_PROXY_TIMEOUT_SECONDS: u32 = 3600;

impl GlobalProxyConfig {
    pub fn validate(&self) -> Result<(), String> {
        fn require_range(field: &str, value: u32, min: u32, max: u32) -> Result<(), String> {
            if (min..=max).contains(&value) {
                Ok(())
            } else {
                Err(format!("{field} must be between {min} and {max}"))
            }
        }

        require_range("listenPort", self.listen_port as u32, 1024, u16::MAX as u32)?;

        if self.listen_address.trim().is_empty() {
            return Err("listenAddress cannot be empty".to_string());
        }

        Ok(())
    }
}

impl AppProxyConfig {
    pub fn validate(&self) -> Result<(), String> {
        fn require_range(field: &str, value: u32, min: u32, max: u32) -> Result<(), String> {
            if (min..=max).contains(&value) {
                Ok(())
            } else {
                Err(format!("{field} must be between {min} and {max}"))
            }
        }

        require_range("maxRetries", self.max_retries, 0, 10)?;
        require_range(
            "streamingFirstByteTimeout",
            self.streaming_first_byte_timeout,
            0,
            MAX_PROXY_TIMEOUT_SECONDS,
        )?;
        require_range(
            "streamingIdleTimeout",
            self.streaming_idle_timeout,
            0,
            MAX_PROXY_TIMEOUT_SECONDS,
        )?;
        require_range(
            "nonStreamingTimeout",
            self.non_streaming_timeout,
            0,
            MAX_PROXY_TIMEOUT_SECONDS,
        )?;
        require_range(
            "circuitFailureThreshold",
            self.circuit_failure_threshold,
            1,
            20,
        )?;
        require_range(
            "circuitSuccessThreshold",
            self.circuit_success_threshold,
            1,
            10,
        )?;
        require_range(
            "circuitTimeoutSeconds",
            self.circuit_timeout_seconds,
            0,
            300,
        )?;
        require_range("circuitMinRequests", self.circuit_min_requests, 5, 100)?;

        if !self.circuit_error_rate_threshold.is_finite()
            || !(0.0..=1.0).contains(&self.circuit_error_rate_threshold)
        {
            return Err("circuitErrorRateThreshold must be between 0 and 1".to_string());
        }

        Ok(())
    }
}

/// Rectifier configuration.
///
/// Stored in the settings table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RectifierConfig {
    /// Master rectifier switch, enabled by default.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Thinking-signature rectification, enabled by default.
    ///
    /// Handles invalid signatures in thinking blocks.
    #[serde(default = "default_true")]
    pub request_thinking_signature: bool,
    /// Thinking-budget rectification, enabled by default.
    ///
    /// Handles budget_tokens and thinking constraints.
    #[serde(default = "default_true")]
    pub request_thinking_budget: bool,
    /// Unsupported-image fallback, enabled by default.
    ///
    /// Replaces rejected image blocks with `[Unsupported Image]` so conversation can
    /// continue. Governs explicit text-only declarations and post-error fallback.
    #[serde(default = "default_true")]
    pub request_media_fallback: bool,
    /// Heuristic text-only model-name matching, enabled by default.
    ///
    /// Before sending, strips images predicted unsupported from the built-in model
    /// list when capabilities are undeclared. request_media_fallback governs it;
    /// disabling the heuristic retains only explicit and post-error paths.
    #[serde(default = "default_true")]
    pub request_media_heuristic: bool,
}

fn default_true() -> bool {
    true
}

fn default_log_level() -> String {
    "info".to_string()
}

impl Default for RectifierConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            request_thinking_signature: true,
            request_thinking_budget: true,
            request_media_fallback: true,
            request_media_heuristic: true,
        }
    }
}

/// Request-optimizer configuration.
///
/// Stored in settings under `optimizer_config`; applies only to Bedrock providers
/// with CLAUDE_CODE_USE_BEDROCK=1.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizerConfig {
    /// Master switch, disabled by default.
    #[serde(default)]
    pub enabled: bool,
    /// Thinking optimization switch, on when the master switch is enabled.
    #[serde(default = "default_true")]
    pub thinking_optimizer: bool,
    /// Cache injection switch, on when the master switch is enabled.
    #[serde(default = "default_true")]
    pub cache_injection: bool,
    /// Cache TTL, `5m` or `1h`; default `1h`.
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl: String,
}

fn default_cache_ttl() -> String {
    "1h".to_string()
}

impl Default for OptimizerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            thinking_optimizer: true,
            cache_injection: true,
            cache_ttl: "1h".to_string(),
        }
    }
}

/// Copilot optimizer configuration.
///
/// Stored in settings under `copilot_optimizer_config`; addresses Copilot proxy
/// usage anomalies from issue #1813.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotOptimizerConfig {
    /// Master switch, enabled by default and important for Copilot users.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// x-initiator request classification, enabled by default at P0.
    #[serde(default = "default_true")]
    pub request_classification: bool,
    /// Tool-result message merging, enabled by default at P1.
    #[serde(default = "default_true")]
    pub tool_result_merging: bool,
    /// Compact-request detection, enabled by default at P2.
    #[serde(default = "default_true")]
    pub compact_detection: bool,
    /// Deterministic request IDs, enabled by default at P3.
    #[serde(default = "default_true")]
    pub deterministic_request_id: bool,
    /// Subagent detection, enabled by default. Marks Claude Code subagents with
    /// x-initiator=agent and x-interaction-type=conversation-subagent to avoid billing.
    #[serde(default = "default_true")]
    pub subagent_detection: bool,
    /// Warmup downgrade, enabled by default to prevent probes consuming premium quota.
    #[serde(default = "default_true")]
    pub warmup_downgrade: bool,
    /// Warmup downgrade model; default `gpt-5-mini`.
    #[serde(default = "default_warmup_model")]
    pub warmup_model: String,
    /// Proactively strips thinking/redacted_thinking from assistant messages.
    ///
    /// Copilot's OpenAI-compatible endpoint rejects thinking blocks; reactive retry
    /// wastes premium quota on the first request, so strip them before sending.
    #[serde(default = "default_true")]
    pub strip_thinking: bool,
}

fn default_warmup_model() -> String {
    "gpt-5-mini".to_string()
}

impl Default for CopilotOptimizerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            request_classification: true,
            tool_result_merging: true,
            compact_detection: true,
            deterministic_request_id: true,
            subagent_detection: true,
            warmup_downgrade: true,
            warmup_model: "gpt-5-mini".to_string(),
            strip_thinking: true,
        }
    }
}

/// Logging configuration.
///
/// Stored as JSON in the settings table under log_config.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogConfig {
    /// Master logging switch.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Log level: error, warn, info, debug, or trace.
    #[serde(default = "default_log_level")]
    pub level: String,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            level: "info".to_string(),
        }
    }
}

impl LogConfig {
    /// Converts configuration to log::LevelFilter.
    pub fn to_level_filter(&self) -> log::LevelFilter {
        if !self.enabled {
            return log::LevelFilter::Off;
        }
        match self.level.to_lowercase().as_str() {
            "error" => log::LevelFilter::Error,
            "warn" => log::LevelFilter::Warn,
            "info" => log::LevelFilter::Info,
            "debug" => log::LevelFilter::Debug,
            "trace" => log::LevelFilter::Trace,
            _ => log::LevelFilter::Info,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_proxy_config() -> AppProxyConfig {
        AppProxyConfig {
            app_type: "codex".to_string(),
            enabled: true,
            auto_failover_enabled: true,
            max_retries: 1,
            streaming_first_byte_timeout: 600,
            streaming_idle_timeout: 600,
            non_streaming_timeout: 1200,
            circuit_failure_threshold: 4,
            circuit_success_threshold: 2,
            circuit_timeout_seconds: 60,
            circuit_error_rate_threshold: 0.6,
            circuit_min_requests: 10,
        }
    }

    fn global_proxy_config() -> GlobalProxyConfig {
        GlobalProxyConfig {
            proxy_enabled: true,
            listen_address: "127.0.0.1".to_string(),
            listen_port: 15721,
            enable_logging: true,
        }
    }

    #[test]
    fn global_proxy_config_validates_listen_port() {
        assert_eq!(global_proxy_config().validate(), Ok(()));

        let too_low_port = GlobalProxyConfig {
            listen_port: 1023,
            ..global_proxy_config()
        };
        assert_eq!(
            too_low_port.validate(),
            Err("listenPort must be between 1024 and 65535".to_string())
        );
    }

    #[test]
    fn app_proxy_config_allows_long_prefill_timeouts_and_zero_to_disable() {
        let config = app_proxy_config();
        assert_eq!(config.validate(), Ok(()));

        let disabled = AppProxyConfig {
            streaming_first_byte_timeout: 0,
            streaming_idle_timeout: 0,
            non_streaming_timeout: 0,
            ..config
        };
        assert_eq!(disabled.validate(), Ok(()));
    }

    #[test]
    fn app_proxy_config_rejects_timeout_above_operational_limit() {
        let config = AppProxyConfig {
            streaming_first_byte_timeout: MAX_PROXY_TIMEOUT_SECONDS + 1,
            ..app_proxy_config()
        };
        assert_eq!(
            config.validate(),
            Err("streamingFirstByteTimeout must be between 0 and 3600".to_string())
        );
    }

    #[test]
    fn test_rectifier_config_default_enabled() {
        // RectifierConfig::default() enables every feature.
        let config = RectifierConfig::default();
        assert!(
            config.enabled,
            "rectifier master switch should default to true"
        );
        assert!(
            config.request_thinking_signature,
            "thinking-signature rectifier should default to true"
        );
        assert!(
            config.request_thinking_budget,
            "thinking-budget rectifier should default to true"
        );
        assert!(
            config.request_media_fallback,
            "media fallback should default to true"
        );
        assert!(
            config.request_media_heuristic,
            "heuristic text-only detection should default to true"
        );
    }

    #[test]
    fn test_rectifier_config_serde_default() {
        // Missing fields deserialize to true.
        let json = "{}";
        let config: RectifierConfig = serde_json::from_str(json).unwrap();
        assert!(config.enabled);
        assert!(config.request_thinking_signature);
        assert!(config.request_thinking_budget);
        assert!(
            config.request_media_fallback,
            "missing requestMediaFallback should use true"
        );
        assert!(
            config.request_media_heuristic,
            "missing requestMediaHeuristic should use true"
        );
    }

    #[test]
    fn test_rectifier_config_serde_explicit_true() {
        // Explicit true values deserialize correctly.
        let json =
            r#"{"enabled": true, "requestThinkingSignature": true, "requestThinkingBudget": true}"#;
        let config: RectifierConfig = serde_json::from_str(json).unwrap();
        assert!(config.enabled);
        assert!(config.request_thinking_signature);
        assert!(config.request_thinking_budget);
    }

    #[test]
    fn test_rectifier_config_serde_partial_fields() {
        // Missing fields remain true when only some are supplied.
        let json = r#"{"enabled": true, "requestThinkingSignature": false}"#;
        let config: RectifierConfig = serde_json::from_str(json).unwrap();
        assert!(config.enabled);
        assert!(!config.request_thinking_signature);
        assert!(config.request_thinking_budget);
    }

    #[test]
    fn test_rectifier_config_serde_media_explicit_false() {
        // Explicit false media settings must override defaults.
        let json = r#"{"requestMediaFallback": false, "requestMediaHeuristic": false}"#;
        let config: RectifierConfig = serde_json::from_str(json).unwrap();
        assert!(!config.request_media_fallback);
        assert!(!config.request_media_heuristic);
        // Remaining fields retain true defaults.
        assert!(config.enabled);
        assert!(config.request_thinking_signature);
        assert!(config.request_thinking_budget);
    }

    #[test]
    fn test_log_config_default() {
        let config = LogConfig::default();
        assert!(config.enabled);
        assert_eq!(config.level, "info");
    }

    #[test]
    fn test_log_config_serde_default() {
        let json = "{}";
        let config: LogConfig = serde_json::from_str(json).unwrap();
        assert!(config.enabled);
        assert_eq!(config.level, "info");
    }

    #[test]
    fn test_log_config_to_level_filter() {
        let config = LogConfig {
            level: "error".to_string(),
            ..Default::default()
        };
        assert_eq!(config.to_level_filter(), log::LevelFilter::Error);

        let config = LogConfig {
            level: "warn".to_string(),
            ..Default::default()
        };
        assert_eq!(config.to_level_filter(), log::LevelFilter::Warn);

        let config = LogConfig {
            level: "info".to_string(),
            ..Default::default()
        };
        assert_eq!(config.to_level_filter(), log::LevelFilter::Info);

        let config = LogConfig {
            level: "debug".to_string(),
            ..Default::default()
        };
        assert_eq!(config.to_level_filter(), log::LevelFilter::Debug);

        let config = LogConfig {
            level: "trace".to_string(),
            ..Default::default()
        };
        assert_eq!(config.to_level_filter(), log::LevelFilter::Trace);

        // Invalid levels fall back to info.
        let config = LogConfig {
            level: "invalid".to_string(),
            ..Default::default()
        };
        assert_eq!(config.to_level_filter(), log::LevelFilter::Info);

        // Disabled logging returns Off.
        let config = LogConfig {
            enabled: false,
            level: "debug".to_string(),
        };
        assert_eq!(config.to_level_filter(), log::LevelFilter::Off);
    }

    #[test]
    fn test_log_config_serde_roundtrip() {
        let config = LogConfig {
            enabled: true,
            level: "debug".to_string(),
        };
        let json = serde_json::to_string(&config).unwrap();
        let parsed: LogConfig = serde_json::from_str(&json).unwrap();
        assert!(parsed.enabled);
        assert_eq!(parsed.level, "debug");
    }
}
