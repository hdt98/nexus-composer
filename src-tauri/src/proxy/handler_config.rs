//! Handler configuration module.
//!
//! Defines configuration structures and usage parsers for API handlers.

use crate::app_config::AppType;
use crate::proxy::usage::parser::TokenUsage;
use serde_json::Value;

/// Usage parser type aliases.
pub type StreamUsageParser = fn(&[Value]) -> Option<TokenUsage>;
pub type ResponseUsageParser = fn(&Value) -> Option<TokenUsage>;

/// Model extractor type alias.
/// Arguments: (stream events, requested model name) -> final model name.
pub type StreamModelExtractor = fn(&[Value], &str) -> String;

/// Streaming usage event prefilter type alias.
///
/// Receives the raw SSE `data:` string. Returning false skips JSON parsing for
/// usage-independent events on the high-frequency token/chunk path.
pub type StreamUsageEventFilter = fn(&str) -> bool;

/// Usage parsing configuration for an API.
#[derive(Clone, Copy)]
pub struct UsageParserConfig {
    /// Streaming response parser.
    pub stream_parser: StreamUsageParser,
    /// Non-streaming response parser.
    pub response_parser: ResponseUsageParser,
    /// Model extractor for streaming responses.
    pub model_extractor: StreamModelExtractor,
    /// Streaming usage event prefilter.
    pub stream_event_filter: Option<StreamUsageEventFilter>,
    /// App type string used for logging.
    pub app_type_str: &'static str,
}

// ============================================================================
// Streaming usage event prefilters
// ============================================================================

pub fn claude_stream_usage_event_filter(data: &str) -> bool {
    data.contains("\"message_start\"") || data.contains("\"message_delta\"")
}

fn openai_stream_usage_event_filter(data: &str) -> bool {
    data.contains("\"usage\"")
}

pub fn codex_stream_usage_event_filter(data: &str) -> bool {
    data.contains("\"response.completed\"") || data.contains("\"usage\"")
}

fn gemini_stream_usage_event_filter(data: &str) -> bool {
    data.contains("\"usageMetadata\"")
}

// ============================================================================
// Model extractor implementations
// ============================================================================

/// Extracts the model from a Claude stream, preferring `usage.model`.
///
/// An empty model is missing because the conversion layer synthesizes `model: ""`
/// when upstream omits it. Falls back to the mapped outbound model or requested model.
fn claude_model_extractor(events: &[Value], fallback_model: &str) -> String {
    // Prefer the model from parsed usage.
    if let Some(usage) = TokenUsage::from_claude_stream_events(events) {
        if let Some(model) = usage.model.filter(|m| !m.is_empty()) {
            return model;
        }
    }
    fallback_model.to_string()
}

/// Extracts the model from an OpenAI Chat Completions stream, preferring `usage.model`.
fn openai_model_extractor(events: &[Value], fallback_model: &str) -> String {
    // Prefer the model from parsed usage.
    if let Some(usage) = TokenUsage::from_openai_stream_events(events) {
        if let Some(model) = usage.model.filter(|m| !m.is_empty()) {
            return model;
        }
    }
    // Fall back to extracting it directly from an event.
    events
        .iter()
        .find_map(|e| e.get("model")?.as_str().filter(|m| !m.is_empty()))
        .unwrap_or(fallback_model)
        .to_string()
}

/// Extracts the model from a Codex stream with automatic format detection.
fn codex_auto_model_extractor(events: &[Value], fallback_model: &str) -> String {
    // Prefer the model from parsed usage.
    if let Some(usage) = TokenUsage::from_codex_stream_events_auto(events) {
        if let Some(model) = usage.model.filter(|m| !m.is_empty()) {
            return model;
        }
    }
    // Fall back to the `response.completed` event.
    events
        .iter()
        .find_map(|e| {
            if e.get("type")?.as_str()? == "response.completed" {
                e.get("response")?
                    .get("model")?
                    .as_str()
                    .filter(|m| !m.is_empty())
            } else {
                None
            }
        })
        .or_else(|| {
            // Then fall back to an OpenAI-format event.
            events
                .iter()
                .find_map(|e| e.get("model")?.as_str().filter(|m| !m.is_empty()))
        })
        .unwrap_or(fallback_model)
        .to_string()
}

/// Extracts the model from a Gemini stream, preferring `usage.model`.
fn gemini_model_extractor(events: &[Value], fallback_model: &str) -> String {
    // Prefer the model from parsed usage.
    if let Some(usage) = TokenUsage::from_gemini_stream_chunks(events) {
        if let Some(model) = usage.model.filter(|m| !m.is_empty()) {
            return model;
        }
    }
    fallback_model.to_string()
}

// ============================================================================
// Predefined configurations
// ============================================================================

/// Claude API parser configuration.
pub const CLAUDE_PARSER_CONFIG: UsageParserConfig = UsageParserConfig {
    stream_parser: TokenUsage::from_claude_stream_events,
    response_parser: TokenUsage::from_claude_response,
    model_extractor: claude_model_extractor,
    stream_event_filter: Some(claude_stream_usage_event_filter),
    app_type_str: "claude",
};

/// OpenAI Chat Completions parser configuration for Codex `/v1/chat/completions`.
pub const OPENAI_PARSER_CONFIG: UsageParserConfig = UsageParserConfig {
    stream_parser: TokenUsage::from_openai_stream_events,
    response_parser: TokenUsage::from_openai_response,
    model_extractor: openai_model_extractor,
    stream_event_filter: Some(openai_stream_usage_event_filter),
    app_type_str: "codex",
};

/// Codex parser configuration with automatic OpenAI or Codex format detection.
pub const CODEX_PARSER_CONFIG: UsageParserConfig = UsageParserConfig {
    stream_parser: TokenUsage::from_codex_stream_events_auto,
    response_parser: TokenUsage::from_codex_response_auto,
    model_extractor: codex_auto_model_extractor,
    stream_event_filter: Some(codex_stream_usage_event_filter),
    app_type_str: "codex",
};

/// Gemini API parser configuration.
pub const GEMINI_PARSER_CONFIG: UsageParserConfig = UsageParserConfig {
    stream_parser: TokenUsage::from_gemini_stream_chunks,
    response_parser: TokenUsage::from_gemini_response,
    model_extractor: gemini_model_extractor,
    stream_event_filter: Some(gemini_stream_usage_event_filter),
    app_type_str: "gemini",
};

// ============================================================================
// Handler configuration reserved for future consolidation
// ============================================================================

/// Base handler configuration.
///
/// Reserved for consolidating handler configuration.
#[allow(dead_code)]
#[derive(Clone)]
pub struct HandlerConfig {
    /// App type.
    pub app_type: AppType,
    /// Log tag.
    pub tag: &'static str,
    /// App type string.
    pub app_type_str: &'static str,
    /// Usage parser configuration.
    pub parser_config: &'static UsageParserConfig,
}

/// Claude handler configuration.
#[allow(dead_code)]
pub const CLAUDE_HANDLER_CONFIG: HandlerConfig = HandlerConfig {
    app_type: AppType::Claude,
    tag: "Claude",
    app_type_str: "claude",
    parser_config: &CLAUDE_PARSER_CONFIG,
};

/// Codex Chat Completions handler configuration.
#[allow(dead_code)]
pub const CODEX_CHAT_HANDLER_CONFIG: HandlerConfig = HandlerConfig {
    app_type: AppType::Codex,
    tag: "Codex",
    app_type_str: "codex",
    parser_config: &OPENAI_PARSER_CONFIG,
};

/// Codex Responses handler configuration.
#[allow(dead_code)]
pub const CODEX_RESPONSES_HANDLER_CONFIG: HandlerConfig = HandlerConfig {
    app_type: AppType::Codex,
    tag: "Codex",
    app_type_str: "codex",
    parser_config: &CODEX_PARSER_CONFIG,
};

/// Gemini handler configuration.
#[allow(dead_code)]
pub const GEMINI_HANDLER_CONFIG: HandlerConfig = HandlerConfig {
    app_type: AppType::Gemini,
    tag: "Gemini",
    app_type_str: "gemini",
    parser_config: &GEMINI_PARSER_CONFIG,
};
