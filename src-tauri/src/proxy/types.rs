use serde::{Deserialize, Serialize};

/// Proxy server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    /// Listen address.
    pub listen_address: String,
    /// Listen port.
    pub listen_port: u16,
    /// Maximum retry count.
    pub max_retries: u8,
    /// Request timeout in seconds. Deprecated but kept for compatibility.
    pub request_timeout: u64,
    /// Whether logging is enabled.
    pub enable_logging: bool,
    /// Whether Live config takeover is active.
    #[serde(default)]
    pub live_takeover_active: bool,
    /// Streaming first-byte timeout in seconds. Waits for the first chunk, range 1-120, default 60.
    #[serde(default = "default_streaming_first_byte_timeout")]
    pub streaming_first_byte_timeout: u64,
    /// Streaming idle timeout in seconds. Max gap between chunks, range 60-600, set 0 to disable.
    #[serde(default = "default_streaming_idle_timeout")]
    pub streaming_idle_timeout: u64,
    /// Non-streaming total timeout in seconds. Range 60-1200, default 600 (10 minutes).
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
            listen_port: 15721, // Use a higher port that is less likely to be occupied.
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

/// Proxy server status.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProxyStatus {
    /// Whether it is running.
    pub running: bool,
    /// Listen address.
    pub address: String,
    /// Listen port.
    pub port: u16,
    /// Active connection count.
    pub active_connections: usize,
    /// Total request count.
    pub total_requests: u64,
    /// Successful request count.
    pub success_requests: u64,
    /// Failed request count.
    pub failed_requests: u64,
    /// Success rate (0-100).
    pub success_rate: f32,
    /// Uptime in seconds.
    pub uptime_seconds: u64,
    /// Current provider name.
    pub current_provider: Option<String>,
    /// Current provider ID.
    pub current_provider_id: Option<String>,
    /// Last request time.
    pub last_request_at: Option<String>,
    /// Last error message.
    pub last_error: Option<String>,
    /// Provider failover count.
    pub failover_count: u64,
    /// Currently active proxy targets.
    #[serde(default)]
    pub active_targets: Vec<ActiveTarget>,
}

/// Active proxy target information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveTarget {
    pub app_type: String, // "Claude" | "Codex" | "Gemini"
    pub provider_name: String,
    pub provider_id: String,
}

/// Proxy server information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyServerInfo {
    pub address: String,
    pub port: u16,
    pub started_at: String,
}

/// Takeover status for each app. Indicates whether the app's Live config points to the local proxy.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProxyTakeoverStatus {
    pub claude: bool,
    pub codex: bool,
    pub gemini: bool,
    pub opencode: bool,
    pub openclaw: bool,
}

/// API format type. Reserved because format conversion is not currently needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ApiFormat {
    Claude,
    OpenAI,
    Gemini,
}

/// Provider health status.
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

/// Live configuration backup record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveBackup {
    /// Application type (claude/codex/gemini).
    pub app_type: String,
    /// Original configuration JSON.
    pub original_config: String,
    /// Backup timestamp.
    pub backed_up_at: String,
}

/// Global proxy configuration shared across mirrored settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalProxyConfig {
    /// Master proxy switch.
    pub proxy_enabled: bool,
    /// Listening address.
    pub listen_address: String,
    /// Listening port.
    pub listen_port: u16,
    /// Whether logging is enabled.
    pub enable_logging: bool,
}

/// Per-application proxy configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppProxyConfig {
    /// Application type (claude/codex/gemini).
    pub app_type: String,
    /// Whether proxy takeover is enabled for this application.
    pub enabled: bool,
    /// Whether automatic failover is enabled for this application.
    pub auto_failover_enabled: bool,
    /// Maximum retry count.
    pub max_retries: u32,
    /// Streaming first-byte timeout in seconds.
    pub streaming_first_byte_timeout: u32,
    /// Streaming idle timeout in seconds.
    pub streaming_idle_timeout: u32,
    /// Total non-streaming timeout in seconds.
    pub non_streaming_timeout: u32,
    /// Circuit-breaker failure threshold.
    pub circuit_failure_threshold: u32,
    /// Circuit-breaker recovery threshold.
    pub circuit_success_threshold: u32,
    /// Circuit-breaker recovery wait in seconds.
    pub circuit_timeout_seconds: u32,
    /// Error-rate threshold.
    pub circuit_error_rate_threshold: f64,
    /// Minimum request count used to calculate the error rate.
    pub circuit_min_requests: u32,
}

/// Rectifier configuration.
///
/// Stored in the `settings` table.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RectifierConfig {
    /// Master rectifier switch. Disabled until the user explicitly enables it.
    #[serde(default)]
    pub enabled: bool,
    /// Enables thinking-signature request repair. Disabled by default.
    ///
    /// Handles: `Invalid 'signature' in 'thinking' block`.
    #[serde(default)]
    pub request_thinking_signature: bool,
    /// Enables thinking-budget request repair. Disabled by default.
    ///
    /// Handles constraints involving `budget_tokens` and thinking.
    #[serde(default)]
    pub request_thinking_budget: bool,
    /// Enables unsupported-image fallback. Disabled by default.
    ///
    /// When upstream rejects image input, replaces image blocks with `[Unsupported Image]`
    /// so the conversation can continue. This controls both explicitly declared text-only
    /// handling and fallback after an observed upstream error.
    #[serde(default)]
    pub request_media_fallback: bool,
    /// Enables heuristic text-only model matching. Disabled by default.
    ///
    /// When model capabilities are undeclared, proactively strips images before sending based
    /// on the built-in model-name list. Governed by `request_media_fallback`; disabling only this
    /// option keeps explicit declarations and reactive upstream fallback, avoiding silent image
    /// removal when the built-in list misclassifies a multimodal model as text-only.
    #[serde(default)]
    pub request_media_heuristic: bool,
}

fn default_true() -> bool {
    true
}

fn default_log_level() -> String {
    "info".to_string()
}

/// Request optimizer configuration.
///
/// Stored in the `settings` table with key = `"optimizer_config"`.
/// Applies only to the Bedrock provider (`CLAUDE_CODE_USE_BEDROCK = "1"`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OptimizerConfig {
    /// Master switch, disabled until explicitly enabled by the user.
    #[serde(default)]
    pub enabled: bool,
    /// Thinking optimizer sub-switch, enabled by default when the master switch is on.
    #[serde(default = "default_true")]
    pub thinking_optimizer: bool,
    /// Cache-injection sub-switch, enabled by default when the master switch is on.
    #[serde(default = "default_true")]
    pub cache_injection: bool,
    /// Cache TTL: `"5m"` or `"1h"`; defaults to `"1h"`.
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
/// Stored in the `settings` table with key = `"copilot_optimizer_config"`.
/// Addresses abnormal Copilot proxy usage accounting (Issue #1813).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CopilotOptimizerConfig {
    /// Master switch. Disabled until the user explicitly enables request rewriting.
    #[serde(default)]
    pub enabled: bool,
    /// `x-initiator` request classification, enabled by default at P0 priority.
    #[serde(default = "default_true")]
    pub request_classification: bool,
    /// Merges tool-result messages. Disabled by default because it changes content.
    #[serde(default)]
    pub tool_result_merging: bool,
    /// Compact-request detection, enabled by default at P2 priority.
    #[serde(default = "default_true")]
    pub compact_detection: bool,
    /// Deterministic request IDs, enabled by default at P3 priority.
    #[serde(default = "default_true")]
    pub deterministic_request_id: bool,
    /// Subagent detection, enabled by default. Identifies Claude Code subagent requests and sets
    /// `x-initiator=agent` plus `x-interaction-type=conversation-subagent` to avoid subagent billing.
    #[serde(default = "default_true")]
    pub subagent_detection: bool,
    /// Downgrades warmup requests. Disabled by default because it changes model selection.
    #[serde(default)]
    pub warmup_downgrade: bool,
    /// Model used for warmup downgrade; defaults to `"gpt-5-mini"`.
    #[serde(default = "default_warmup_model")]
    pub warmup_model: String,
    /// Proactively strips thinking and redacted_thinking blocks from assistant messages.
    ///
    /// Copilot uses an OpenAI-compatible endpoint that rejects thinking blocks and would trigger
    /// a reactive rectifier retry after the first request already consumed premium quota.
    /// Proactive stripping avoids that waste.
    #[serde(default)]
    pub strip_thinking: bool,
}

fn default_warmup_model() -> String {
    "gpt-5-mini".to_string()
}

impl Default for CopilotOptimizerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            request_classification: true,
            tool_result_merging: false,
            compact_detection: true,
            deterministic_request_id: true,
            subagent_detection: true,
            warmup_downgrade: false,
            warmup_model: "gpt-5-mini".to_string(),
            strip_thinking: false,
        }
    }
}

/// Log configuration.
///
/// Stored in the `log_config` field of the `settings` table as JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogConfig {
    /// Master switch for logging.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Log level: error, warn, info, debug, trace.
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
    /// Convert the configuration to `log::LevelFilter`.
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

    #[test]
    fn test_rectifier_config_defaults_to_explicit_opt_in() {
        let config = RectifierConfig::default();
        assert!(!config.enabled);
        assert!(!config.request_thinking_signature);
        assert!(!config.request_thinking_budget);
        assert!(!config.request_media_fallback);
        assert!(!config.request_media_heuristic);
    }

    #[test]
    fn test_rectifier_config_serde_default() {
        let json = "{}";
        let config: RectifierConfig = serde_json::from_str(json).unwrap();
        assert!(!config.enabled);
        assert!(!config.request_thinking_signature);
        assert!(!config.request_thinking_budget);
        assert!(!config.request_media_fallback);
        assert!(!config.request_media_heuristic);
    }

    #[test]
    fn test_rectifier_config_serde_explicit_true() {
        // Verify explicit true values deserialize correctly.
        let json =
            r#"{"enabled": true, "requestThinkingSignature": true, "requestThinkingBudget": true}"#;
        let config: RectifierConfig = serde_json::from_str(json).unwrap();
        assert!(config.enabled);
        assert!(config.request_thinking_signature);
        assert!(config.request_thinking_budget);
    }

    #[test]
    fn test_rectifier_config_serde_partial_fields() {
        let json = r#"{"enabled": true, "requestThinkingSignature": false}"#;
        let config: RectifierConfig = serde_json::from_str(json).unwrap();
        assert!(config.enabled);
        assert!(!config.request_thinking_signature);
        assert!(!config.request_thinking_budget);
    }

    #[test]
    fn test_rectifier_config_serde_media_explicit_false() {
        // Verify both media fields preserve explicit false values without default overrides.
        let json = r#"{"requestMediaFallback": false, "requestMediaHeuristic": false}"#;
        let config: RectifierConfig = serde_json::from_str(json).unwrap();
        assert!(!config.request_media_fallback);
        assert!(!config.request_media_heuristic);
        assert!(!config.enabled);
        assert!(!config.request_thinking_signature);
        assert!(!config.request_thinking_budget);
    }

    #[test]
    fn test_copilot_optimizer_defaults_do_not_rewrite_requests() {
        let config = CopilotOptimizerConfig::default();
        assert!(!config.enabled);
        assert!(config.request_classification);
        assert!(!config.tool_result_merging);
        assert!(config.compact_detection);
        assert!(config.deterministic_request_id);
        assert!(config.subagent_detection);
        assert!(!config.warmup_downgrade);
        assert!(!config.strip_thinking);
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

        // Invalid levels should fall back to info.
        let config = LogConfig {
            level: "invalid".to_string(),
            ..Default::default()
        };
        assert_eq!(config.to_level_filter(), log::LevelFilter::Info);

        // Disabled logging should return Off.
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
