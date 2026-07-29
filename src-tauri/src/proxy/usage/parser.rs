//! Response parser for extracting token usage from API responses.
//!
//! Supports multiple API formats:
//! - Claude API, streaming and non-streaming
//! - OpenRouter in OpenAI format
//! - Codex API, streaming and non-streaming
//! - Gemini API, streaming and non-streaming

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Session-log request_id prefix, matching the format in `session_usage.rs`.
pub const SESSION_REQUEST_ID_PREFIX: &str = "session:";

/// Token usage totals.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_creation_tokens: u32,
    /// Actual model name extracted from the response, when available.
    pub model: Option<String>,
    /// Message ID extracted from the response for cross-source deduplication.
    ///
    /// Claude API uses `msg_xxx`, matching `message.id` in session JSONL.
    #[serde(skip)]
    pub message_id: Option<String>,
}

impl TokenUsage {
    /// Generates the request_id shared with session logs for cross-source deduplication.
    /// Returns `session:{id}` when message_id exists; otherwise uses a random UUID.
    pub fn dedup_request_id(&self) -> String {
        self.message_id
            .as_ref()
            .filter(|mid| !mid.trim().is_empty())
            .map(|mid| format!("{SESSION_REQUEST_ID_PREFIX}{mid}"))
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
    }

    /// Whether any billable token dimension is nonzero.
    ///
    /// Filters all-zero usage before persistence. When an OpenAI-compatible upstream omits
    /// streaming usage, the converter synthesizes an all-zero terminal event. Without a
    /// message_id, `dedup_request_id` would use a random UUID and insert a meaningless row
    /// for every request, inflating the request count.
    pub fn has_billable_tokens(&self) -> bool {
        self.input_tokens > 0
            || self.output_tokens > 0
            || self.cache_read_tokens > 0
            || self.cache_creation_tokens > 0
    }
}

/// API type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ApiType {
    Claude,
    OpenRouter,
    Codex,
    Gemini,
}

impl TokenUsage {
    /// Parses a non-streaming Claude API response.
    pub fn from_claude_response(body: &Value) -> Option<Self> {
        let usage = body.get("usage")?;
        // Extract the model name from the response.
        let model = body
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let message_id = body
            .get("id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Some(Self {
            input_tokens: usage.get("input_tokens")?.as_u64()? as u32,
            output_tokens: usage.get("output_tokens")?.as_u64()? as u32,
            cache_read_tokens: usage
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            cache_creation_tokens: usage
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            model,
            message_id,
        })
    }

    /// Parses Claude API streaming events.
    #[allow(dead_code)]
    pub fn from_claude_stream_events(events: &[Value]) -> Option<Self> {
        let mut usage = Self::default();
        let mut model: Option<String> = None;
        let mut message_id: Option<String> = None;
        let mut input_from_delta = false;

        for event in events {
            if let Some(event_type) = event.get("type").and_then(|v| v.as_str()) {
                match event_type {
                    "message_start" => {
                        if let Some(message) = event.get("message") {
                            if model.is_none() {
                                if let Some(m) = message.get("model").and_then(|v| v.as_str()) {
                                    model = Some(m.to_string());
                                }
                            }
                            if message_id.is_none() {
                                if let Some(id) = message.get("id").and_then(|v| v.as_str()) {
                                    message_id = Some(id.to_string());
                                }
                            }
                        }
                        if let Some(msg_usage) = event.get("message").and_then(|m| m.get("usage")) {
                            // Read input_tokens from message_start for the native Claude API.
                            if let Some(input) =
                                msg_usage.get("input_tokens").and_then(|v| v.as_u64())
                            {
                                usage.input_tokens = input as u32;
                            }
                            usage.cache_read_tokens = msg_usage
                                .get("cache_read_input_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0)
                                as u32;
                            usage.cache_creation_tokens = msg_usage
                                .get("cache_creation_input_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0)
                                as u32;
                        }
                    }
                    "message_delta" => {
                        if let Some(delta_usage) = event.get("usage") {
                            // Read output_tokens from message_delta.
                            if let Some(output) =
                                delta_usage.get("output_tokens").and_then(|v| v.as_u64())
                            {
                                usage.output_tokens = output as u32;
                            }

                            let delta_input = delta_usage
                                .get("input_tokens")
                                .and_then(|v| v.as_u64())
                                .map(|v| v as u32);
                            let delta_cache_read = delta_usage
                                .get("cache_read_input_tokens")
                                .and_then(|v| v.as_u64())
                                .map(|v| v as u32);
                            let delta_cache_creation = delta_usage
                                .get("cache_creation_input_tokens")
                                .and_then(|v| v.as_u64())
                                .map(|v| v as u32);

                            // Some Anthropic-compatible SSE providers report the full context
                            // in message_start, then corrected fresh input in message_delta.
                            // Prefer a smaller positive delta input and its cache counts to avoid
                            // double counting. If the delta omits cache fields, retain the start
                            // values as a best-effort fallback.
                            if let Some(input) = delta_input {
                                let should_use_delta_input = input > 0
                                    && (usage.input_tokens == 0
                                        || input < usage.input_tokens
                                        || (input_from_delta && input <= usage.input_tokens));

                                if should_use_delta_input {
                                    usage.input_tokens = input;
                                    input_from_delta = true;
                                    if let Some(cache_read) = delta_cache_read {
                                        usage.cache_read_tokens = cache_read;
                                    }
                                    if let Some(cache_creation) = delta_cache_creation {
                                        usage.cache_creation_tokens = cache_creation;
                                    }
                                }
                            }
                            // Read cache hits (cache_read_input_tokens) from message_delta.
                            if usage.cache_read_tokens == 0 {
                                if let Some(cache_read) = delta_cache_read {
                                    usage.cache_read_tokens = cache_read;
                                }
                            }
                            // Read cache creation (cache_creation_input_tokens) from message_delta.
                            // Zhipu currently omits cache_creation_input_tokens.
                            if usage.cache_creation_tokens == 0 {
                                if let Some(cache_creation) = delta_cache_creation {
                                    usage.cache_creation_tokens = cache_creation;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // Use has_billable_tokens rather than only input/output. A fully cached stream with
        // no output (input == 0, output == 0, cache_read > 0) has real cache-read cost and
        // must be retained. This is especially common after the Gemini-to-Anthropic path
        // computes fresh input as promptTokenCount - cachedContentTokenCount; the old gate
        // incorrectly discarded it as missing usage.
        if usage.has_billable_tokens() {
            usage.model = model;
            usage.message_id = message_id;
            Some(usage)
        } else {
            None
        }
    }

    /// Parses an OpenRouter response in OpenAI format.
    #[allow(dead_code)]
    pub fn from_openrouter_response(body: &Value) -> Option<Self> {
        let usage = body.get("usage")?;
        Some(Self {
            input_tokens: usage.get("prompt_tokens")?.as_u64()? as u32,
            output_tokens: usage.get("completion_tokens")?.as_u64()? as u32,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            model: None,
            message_id: None,
        })
    }

    /// Parses a non-streaming Codex API response.
    pub fn from_codex_response(body: &Value) -> Option<Self> {
        let usage = body.get("usage");
        if usage.is_none() {
            log::debug!(
                "[Codex] Response has no usage field; body keys: {:?}",
                body.as_object().map(|o| o.keys().collect::<Vec<_>>())
            );
            return None;
        }
        let usage = usage?;

        let input_tokens = usage.get("input_tokens").and_then(|v| v.as_u64());
        let output_tokens = usage.get("output_tokens").and_then(|v| v.as_u64());

        if input_tokens.is_none() || output_tokens.is_none() {
            log::debug!("[Codex] usage is missing input_tokens or output_tokens: {usage:?}");
            return None;
        }

        // Extract the model name from the response.
        let model = body
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let cached_tokens = usage
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_u64())
            .or_else(|| {
                usage
                    .get("input_tokens_details")
                    .and_then(|d| d.get("cached_tokens"))
                    .and_then(|v| v.as_u64())
            })
            .unwrap_or(0) as u32;

        Some(Self {
            input_tokens: input_tokens? as u32,
            output_tokens: output_tokens? as u32,
            cache_read_tokens: cached_tokens,
            cache_creation_tokens: usage
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            model,
            message_id: None,
        })
    }

    /// Parses a Codex API response and adjusts input_tokens.
    ///
    /// Codex input_tokens must exclude cached_tokens to obtain billable tokens:
    /// adjusted_input = max(input_tokens - cached_tokens, 0).
    #[allow(dead_code)]
    pub fn from_codex_response_adjusted(body: &Value) -> Option<Self> {
        let usage = body.get("usage")?;
        let input_tokens = usage.get("input_tokens")?.as_u64()? as u32;
        let output_tokens = usage.get("output_tokens")?.as_u64()? as u32;

        // Read cached_tokens from cache_read_input_tokens or input_tokens_details.
        let cached_tokens = usage
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_u64())
            .or_else(|| {
                usage
                    .get("input_tokens_details")
                    .and_then(|d| d.get("cached_tokens"))
                    .and_then(|v| v.as_u64())
            })
            .unwrap_or(0) as u32;

        // Adjust input_tokens by subtracting cached_tokens.
        let adjusted_input = input_tokens.saturating_sub(cached_tokens);

        // Extract the model name from the response.
        let model = body
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Some(Self {
            input_tokens: adjusted_input,
            output_tokens,
            cache_read_tokens: cached_tokens,
            cache_creation_tokens: usage
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            model,
            message_id: None,
        })
    }

    /// Parses Codex API streaming events.
    #[allow(dead_code)]
    pub fn from_codex_stream_events(events: &[Value]) -> Option<Self> {
        log::debug!("[Codex] Parsing {} streaming events", events.len());
        for event in events {
            if let Some(event_type) = event.get("type").and_then(|v| v.as_str()) {
                log::debug!("[Codex] Event type: {event_type}");
                if event_type == "response.completed" {
                    if let Some(response) = event.get("response") {
                        log::debug!("[Codex] Found response.completed event; parsing usage");
                        return Self::from_codex_response_adjusted(response);
                    }
                }
            }
        }
        log::debug!("[Codex] No response.completed event found");
        None
    }

    /// Parses a Codex response by automatically detecting OpenAI or Codex format.
    ///
    /// Codex supports two API formats:
    /// - `/v1/responses`: input_tokens/output_tokens
    /// - `/v1/chat/completions`: prompt_tokens/completion_tokens in OpenAI format
    ///
    /// Records raw input_tokens; cost calculation subtracts cached_tokens later.
    pub fn from_codex_response_auto(body: &Value) -> Option<Self> {
        let usage = body.get("usage")?;

        // Detect the format: OpenAI uses prompt_tokens, while Codex uses input_tokens.
        if usage.get("prompt_tokens").is_some() {
            log::debug!("[Codex] Detected OpenAI format (prompt_tokens)");
            Self::from_openai_response(body)
        } else if usage.get("input_tokens").is_some() {
            log::debug!("[Codex] Detected Codex format (input_tokens)");
            // Use the unadjusted parser to record raw input_tokens.
            Self::from_codex_response(body)
        } else {
            log::debug!("[Codex] Unrecognized response format; usage: {usage:?}");
            None
        }
    }

    /// Parses Codex streaming events by automatically detecting OpenAI or Codex format.
    pub fn from_codex_stream_events_auto(events: &[Value]) -> Option<Self> {
        log::debug!("[Codex] Auto-parsing {} streaming events", events.len());

        // Try Codex Responses API first; any terminal event may carry usage.
        for event in events {
            if let Some(event_type) = event.get("type").and_then(|v| v.as_str()) {
                if matches!(
                    event_type,
                    "response.completed" | "response.incomplete" | "response.failed"
                ) {
                    if let Some(response) = event.get("response") {
                        log::debug!("[Codex] Found {event_type} event");
                        return Self::from_codex_response_auto(response);
                    }
                }
            }
        }

        // Fall back to OpenAI Chat Completions, whose last chunk contains usage.
        log::debug!("[Codex] Trying OpenAI streaming format");
        Self::from_openai_stream_events(events)
    }

    /// Parses an OpenAI Chat Completions response (prompt_tokens, completion_tokens).
    pub fn from_openai_response(body: &Value) -> Option<Self> {
        let usage = body.get("usage")?;

        // OpenAI uses prompt_tokens and completion_tokens.
        let prompt_tokens = usage.get("prompt_tokens").and_then(|v| v.as_u64())?;
        let completion_tokens = usage.get("completion_tokens").and_then(|v| v.as_u64())?;

        // Read cached_tokens from prompt_tokens_details when present.
        let cached_tokens = usage
            .get("prompt_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;

        // Extract the model name from the response.
        let model = body
            .get("model")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Some(Self {
            input_tokens: prompt_tokens as u32,
            output_tokens: completion_tokens as u32,
            cache_read_tokens: cached_tokens,
            cache_creation_tokens: 0,
            model,
            message_id: None,
        })
    }

    /// Parses OpenAI Chat Completions streaming events.
    pub fn from_openai_stream_events(events: &[Value]) -> Option<Self> {
        log::debug!("[Codex] Parsing {} OpenAI streaming events", events.len());
        // OpenAI streaming responses include usage in the last chunk.
        for event in events.iter().rev() {
            if let Some(usage) = event.get("usage") {
                if !usage.is_null() {
                    log::debug!("[Codex] Found usage: {usage:?}");
                    return Self::from_openai_response(event);
                }
            }
        }
        log::debug!("[Codex] No usage found");
        None
    }

    /// Parses a non-streaming Gemini API response.
    pub fn from_gemini_response(body: &Value) -> Option<Self> {
        let usage = body.get("usageMetadata")?;
        // Extract the actual model name from modelVersion.
        let model = body
            .get("modelVersion")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let prompt_tokens = usage.get("promptTokenCount")?.as_u64()? as u32;
        let total_tokens = usage.get("totalTokenCount")?.as_u64()? as u32;

        // Output tokens equal total tokens minus input tokens.
        // This includes candidatesTokenCount and thoughtsTokenCount.
        let output_tokens = total_tokens.saturating_sub(prompt_tokens);

        Some(Self {
            input_tokens: prompt_tokens,
            output_tokens,
            cache_read_tokens: usage
                .get("cachedContentTokenCount")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            cache_creation_tokens: 0,
            model,
            message_id: None,
        })
    }

    /// Parses Gemini API streaming chunks.
    #[allow(dead_code)]
    pub fn from_gemini_stream_chunks(chunks: &[Value]) -> Option<Self> {
        let mut total_input = 0u32;
        let mut total_tokens = 0u32;
        let mut total_cache_read = 0u32;
        let mut model: Option<String> = None;

        for chunk in chunks {
            if let Some(usage) = chunk.get("usageMetadata") {
                // Input tokens, usually unchanged across chunks.
                total_input = usage
                    .get("promptTokenCount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;

                // Total tokens include input, output, and thinking.
                total_tokens = usage
                    .get("totalTokenCount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;

                // Cache-read tokens.
                total_cache_read = usage
                    .get("cachedContentTokenCount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
            }

            // Extract the actual model name from modelVersion.
            if model.is_none() {
                if let Some(model_version) = chunk.get("modelVersion").and_then(|v| v.as_str()) {
                    model = Some(model_version.to_string());
                }
            }
        }

        // Output tokens equal total tokens minus input tokens.
        let total_output = total_tokens.saturating_sub(total_input);

        if total_input > 0 || total_output > 0 {
            Some(Self {
                input_tokens: total_input,
                output_tokens: total_output,
                cache_read_tokens: total_cache_read,
                cache_creation_tokens: 0,
                model,
                message_id: None,
            })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_claude_response_parsing() {
        let response = json!({
            "model": "claude-sonnet-4-20250514",
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "cache_read_input_tokens": 20,
                "cache_creation_input_tokens": 10
            }
        });

        let usage = TokenUsage::from_claude_response(&response).unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_read_tokens, 20);
        assert_eq!(usage.cache_creation_tokens, 10);
        assert_eq!(usage.model, Some("claude-sonnet-4-20250514".to_string()));
    }

    #[test]
    fn test_dedup_request_id_rejects_empty_message_ids() {
        for message_id in ["", "  "] {
            let usage = TokenUsage {
                message_id: Some(message_id.to_string()),
                ..Default::default()
            };
            let first = usage.dedup_request_id();
            let second = usage.dedup_request_id();

            assert_ne!(first, "session:");
            assert_ne!(first, second);
            assert!(uuid::Uuid::parse_str(&first).is_ok());
            assert!(uuid::Uuid::parse_str(&second).is_ok());
        }

        let padded = TokenUsage {
            message_id: Some(" msg_1 ".to_string()),
            ..Default::default()
        };
        assert_eq!(padded.dedup_request_id(), "session: msg_1 ");
    }

    #[test]
    fn test_has_billable_tokens_gates_empty_usage() {
        // All-zero usage, such as a synthesized terminal event when upstream omits usage,
        // is not billable. This gate prevents extra empty Codex streaming records.
        assert!(!TokenUsage::default().has_billable_tokens());
        // Cache-read-only usage still contains real billable tokens.
        let only_cache = TokenUsage {
            cache_read_tokens: 100,
            ..Default::default()
        };
        assert!(only_cache.has_billable_tokens());
        let normal = TokenUsage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        };
        assert!(normal.has_billable_tokens());
    }

    #[test]
    fn test_claude_stream_cache_only_request_is_recorded() {
        // P2 regression: a fully cached stream with no output (input == 0, output == 0,
        // cache_read > 0) is billable and must survive the old input/output-only gate.
        let events = vec![
            json!({
                "type": "message_start",
                "message": {
                    "id": "msg_cacheonly",
                    "model": "claude-opus-4-8",
                    "usage": {
                        "input_tokens": 0,
                        "cache_read_input_tokens": 50000,
                        "cache_creation_input_tokens": 0
                    }
                }
            }),
            json!({
                "type": "message_delta",
                "usage": { "output_tokens": 0 }
            }),
        ];
        let usage = TokenUsage::from_claude_stream_events(&events)
            .expect("cache-only streaming usage must survive the input/output gate");
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.cache_read_tokens, 50000);
        assert_eq!(usage.message_id, Some("msg_cacheonly".to_string()));
    }

    #[test]
    fn test_codex_response_auto_returns_some_for_synthetic_all_zero() {
        // P3 regression: when an upstream non-streaming Chat response omits usage, the
        // converter synthesizes all-zero usage. from_codex_response_auto still returns Some
        // because the fields exist, proving handlers need the has_billable_tokens gate.
        let synthetic = json!({
            "usage": { "input_tokens": 0, "output_tokens": 0, "total_tokens": 0 }
        });
        let usage = TokenUsage::from_codex_response_auto(&synthetic)
            .expect("from_codex_response_auto returns Some when all-zero usage fields exist");
        assert!(
            !usage.has_billable_tokens(),
            "has_billable_tokens must mark all-zero usage as non-billable for handlers to skip"
        );
    }

    #[test]
    fn test_claude_response_parsing_no_model() {
        let response = json!({
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "cache_read_input_tokens": 20,
                "cache_creation_input_tokens": 10
            }
        });

        let usage = TokenUsage::from_claude_response(&response).unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_read_tokens, 20);
        assert_eq!(usage.cache_creation_tokens, 10);
        assert_eq!(usage.model, None);
    }

    #[test]
    fn test_claude_stream_parsing() {
        let events = vec![
            json!({
                "type": "message_start",
                "message": {
                    "model": "claude-sonnet-4-20250514",
                    "usage": {
                        "input_tokens": 100,
                        "cache_read_input_tokens": 20,
                        "cache_creation_input_tokens": 10
                    }
                }
            }),
            json!({
                "type": "message_delta",
                "usage": {
                    "output_tokens": 50
                }
            }),
        ];

        let usage = TokenUsage::from_claude_stream_events(&events).unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_read_tokens, 20);
        assert_eq!(usage.cache_creation_tokens, 10);
        assert_eq!(usage.model, Some("claude-sonnet-4-20250514".to_string()));
    }

    #[test]
    fn test_claude_stream_parsing_no_model() {
        let events = vec![
            json!({
                "type": "message_start",
                "message": {
                    "usage": {
                        "input_tokens": 100,
                        "cache_read_input_tokens": 20,
                        "cache_creation_input_tokens": 10
                    }
                }
            }),
            json!({
                "type": "message_delta",
                "usage": {
                    "output_tokens": 50
                }
            }),
        ];

        let usage = TokenUsage::from_claude_stream_events(&events).unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_read_tokens, 20);
        assert_eq!(usage.cache_creation_tokens, 10);
        assert_eq!(usage.model, None);
    }

    #[test]
    fn test_openrouter_response_parsing() {
        let response = json!({
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 50
            }
        });

        let usage = TokenUsage::from_openrouter_response(&response).unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_read_tokens, 0);
        assert_eq!(usage.cache_creation_tokens, 0);
    }

    #[test]
    fn test_gemini_response_parsing() {
        let response = json!({
            "modelVersion": "gemini-3-pro-high",
            "usageMetadata": {
                "promptTokenCount": 8383,
                "candidatesTokenCount": 50,
                "thoughtsTokenCount": 114,
                "totalTokenCount": 8547,
                "cachedContentTokenCount": 20
            }
        });

        let usage = TokenUsage::from_gemini_response(&response).unwrap();
        assert_eq!(usage.input_tokens, 8383);
        // output_tokens = totalTokenCount - promptTokenCount = 8547 - 8383 = 164
        assert_eq!(usage.output_tokens, 164);
        assert_eq!(usage.cache_read_tokens, 20);
        assert_eq!(usage.cache_creation_tokens, 0);
        assert_eq!(usage.model, Some("gemini-3-pro-high".to_string()));
    }

    #[test]
    fn test_gemini_response_parsing_no_model() {
        // Parse a response without modelVersion.
        let response = json!({
            "usageMetadata": {
                "promptTokenCount": 100,
                "totalTokenCount": 150,
                "cachedContentTokenCount": 20
            }
        });

        let usage = TokenUsage::from_gemini_response(&response).unwrap();
        assert_eq!(usage.input_tokens, 100);
        // output_tokens = totalTokenCount - promptTokenCount = 150 - 100 = 50
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_read_tokens, 20);
        assert_eq!(usage.cache_creation_tokens, 0);
        assert_eq!(usage.model, None);
    }

    #[test]
    fn test_gemini_response_with_thoughts() {
        // Parse a real user-reported response containing thoughtsTokenCount.
        let response = json!({
            "candidates": [
                {
                    "content": {
                        "parts": [
                            {
                                "text": "",
                                "thoughtSignature": "EvcECvQE..."
                            }
                        ],
                        "role": "model"
                    },
                    "finishReason": "STOP"
                }
            ],
            "modelVersion": "gemini-3-pro-high",
            "responseId": "yupTafqLDu-PjMcPhrOx4QQ",
            "usageMetadata": {
                "candidatesTokenCount": 50,
                "promptTokenCount": 8383,
                "thoughtsTokenCount": 114,
                "totalTokenCount": 8547
            }
        });

        let usage = TokenUsage::from_gemini_response(&response).unwrap();
        assert_eq!(usage.input_tokens, 8383);
        // output_tokens = totalTokenCount - promptTokenCount
        // = 8547 - 8383 = 164 (candidatesTokenCount 50 + thoughtsTokenCount 114)
        assert_eq!(usage.output_tokens, 164);
        assert_eq!(usage.cache_read_tokens, 0);
        assert_eq!(usage.cache_creation_tokens, 0);
        assert_eq!(usage.model, Some("gemini-3-pro-high".to_string()));
    }

    #[test]
    fn test_codex_response_parsing_cached_tokens_in_details() {
        let response = json!({
            "usage": {
                "input_tokens": 1000,
                "output_tokens": 500,
                "input_tokens_details": {
                    "cached_tokens": 300
                }
            }
        });

        let usage = TokenUsage::from_codex_response(&response).unwrap();
        // Unadjusted mode preserves input_tokens while recording cache hits.
        assert_eq!(usage.input_tokens, 1000);
        assert_eq!(usage.output_tokens, 500);
        assert_eq!(usage.cache_read_tokens, 300);
    }

    #[test]
    fn test_codex_response_adjusted() {
        let response = json!({
            "usage": {
                "input_tokens": 1000,
                "output_tokens": 500,
                "input_tokens_details": {
                    "cached_tokens": 300
                }
            }
        });

        let usage = TokenUsage::from_codex_response_adjusted(&response).unwrap();
        // Adjust input_tokens: 1000 - 300 = 700.
        assert_eq!(usage.input_tokens, 700);
        assert_eq!(usage.output_tokens, 500);
        assert_eq!(usage.cache_read_tokens, 300);
    }

    #[test]
    fn test_codex_response_adjusted_no_cache() {
        let response = json!({
            "usage": {
                "input_tokens": 1000,
                "output_tokens": 500
            }
        });

        let usage = TokenUsage::from_codex_response_adjusted(&response).unwrap();
        // Without cached_tokens, input_tokens remains unchanged.
        assert_eq!(usage.input_tokens, 1000);
        assert_eq!(usage.output_tokens, 500);
        assert_eq!(usage.cache_read_tokens, 0);
    }

    #[test]
    fn test_codex_response_adjusted_cache_read_input_tokens() {
        let response = json!({
            "usage": {
                "input_tokens": 1000,
                "output_tokens": 500,
                "cache_read_input_tokens": 200
            }
        });

        let usage = TokenUsage::from_codex_response_adjusted(&response).unwrap();
        assert_eq!(usage.input_tokens, 800);
        assert_eq!(usage.output_tokens, 500);
        assert_eq!(usage.cache_read_tokens, 200);
    }

    #[test]
    fn test_codex_response_adjusted_saturating_sub() {
        // Cover the cached_tokens > input_tokens boundary.
        let response = json!({
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "input_tokens_details": {
                    "cached_tokens": 200
                }
            }
        });

        let usage = TokenUsage::from_codex_response_adjusted(&response).unwrap();
        // saturating_sub prevents underflow.
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.cache_read_tokens, 200);
    }

    #[test]
    fn test_openrouter_stream_parsing() {
        // Parse a converted OpenRouter stream where message_delta contains input_tokens.
        let events = vec![
            json!({
                "type": "message_start",
                "message": {
                    "model": "claude-sonnet-4-20250514",
                    "usage": {
                        "input_tokens": 0,
                        "output_tokens": 0
                    }
                }
            }),
            json!({
                "type": "message_delta",
                "delta": {
                    "stop_reason": "end_turn"
                },
                "usage": {
                    "input_tokens": 150,
                    "output_tokens": 75
                }
            }),
        ];

        let usage = TokenUsage::from_claude_stream_events(&events).unwrap();
        assert_eq!(usage.input_tokens, 150);
        assert_eq!(usage.output_tokens, 75);
        assert_eq!(usage.model, Some("claude-sonnet-4-20250514".to_string()));
    }

    #[test]
    fn test_claude_stream_prefers_smaller_delta_input_and_cache_pair() {
        // Some Anthropic-compatible providers report a cache-inclusive context in
        // message_start and corrected fresh input in message_delta; prefer delta usage.
        let events = vec![
            json!({
                "type": "message_start",
                "message": {
                    "model": "qwen-max",
                    "usage": {
                        "input_tokens": 200_000,
                        "cache_read_input_tokens": 180_000,
                        "cache_creation_input_tokens": 2_000
                    }
                }
            }),
            json!({
                "type": "message_delta",
                "usage": {
                    "input_tokens": 80_000,
                    "output_tokens": 1_000,
                    "cache_read_input_tokens": 120_000,
                    "cache_creation_input_tokens": 500
                }
            }),
        ];

        let usage = TokenUsage::from_claude_stream_events(&events).unwrap();
        assert_eq!(usage.input_tokens, 80_000);
        assert_eq!(usage.output_tokens, 1_000);
        assert_eq!(usage.cache_read_tokens, 120_000);
        assert_eq!(usage.cache_creation_tokens, 500);
        assert_eq!(usage.model, Some("qwen-max".to_string()));
    }

    #[test]
    fn test_claude_stream_updates_cache_pair_from_later_delta_input() {
        // Some providers send multiple message_delta events with input. Once delta input is
        // accepted, later equal or smaller deltas should update cache counts from that block.
        let events = vec![
            json!({
                "type": "message_start",
                "message": {
                    "model": "qwen-max",
                    "usage": {
                        "input_tokens": 200_000,
                        "cache_read_input_tokens": 180_000,
                        "cache_creation_input_tokens": 2_000
                    }
                }
            }),
            json!({
                "type": "message_delta",
                "usage": {
                    "input_tokens": 80_000,
                    "output_tokens": 100,
                    "cache_read_input_tokens": 110_000,
                    "cache_creation_input_tokens": 300
                }
            }),
            json!({
                "type": "message_delta",
                "usage": {
                    "input_tokens": 80_000,
                    "output_tokens": 1_000,
                    "cache_read_input_tokens": 120_000,
                    "cache_creation_input_tokens": 500
                }
            }),
        ];

        let usage = TokenUsage::from_claude_stream_events(&events).unwrap();
        assert_eq!(usage.input_tokens, 80_000);
        assert_eq!(usage.output_tokens, 1_000);
        assert_eq!(usage.cache_read_tokens, 120_000);
        assert_eq!(usage.cache_creation_tokens, 500);
        assert_eq!(usage.model, Some("qwen-max".to_string()));
    }

    #[test]
    fn test_claude_stream_keeps_start_when_delta_input_is_larger() {
        // Under normal Anthropic semantics, message_start input_tokens is authoritative.
        // A larger delta input must not overwrite the start input or cache values.
        let events = vec![
            json!({
                "type": "message_start",
                "message": {
                    "usage": {
                        "input_tokens": 100,
                        "cache_read_input_tokens": 20
                    }
                }
            }),
            json!({
                "type": "message_delta",
                "usage": {
                    "input_tokens": 150,
                    "output_tokens": 75,
                    "cache_read_input_tokens": 30
                }
            }),
        ];

        let usage = TokenUsage::from_claude_stream_events(&events).unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 75);
        assert_eq!(usage.cache_read_tokens, 20);
    }

    #[test]
    fn test_native_claude_stream_parsing() {
        // Parse a native Claude API stream, where message_start contains input_tokens.
        let events = vec![
            json!({
                "type": "message_start",
                "message": {
                    "model": "claude-sonnet-4-20250514",
                    "usage": {
                        "input_tokens": 200,
                        "cache_read_input_tokens": 50
                    }
                }
            }),
            json!({
                "type": "message_delta",
                "usage": {
                    "output_tokens": 100
                }
            }),
        ];

        let usage = TokenUsage::from_claude_stream_events(&events).unwrap();
        assert_eq!(usage.input_tokens, 200);
        assert_eq!(usage.output_tokens, 100);
        assert_eq!(usage.cache_read_tokens, 50);
        assert_eq!(usage.model, Some("claude-sonnet-4-20250514".to_string()));
    }

    // ============================================================================
    // Codex auto-detection tests
    // ============================================================================

    #[test]
    fn test_codex_response_auto_openai_format() {
        // OpenAI format (prompt_tokens/completion_tokens).
        let response = json!({
            "model": "gpt-4o",
            "usage": {
                "prompt_tokens": 1000,
                "completion_tokens": 500,
                "prompt_tokens_details": {
                    "cached_tokens": 200
                }
            }
        });

        let usage = TokenUsage::from_codex_response_auto(&response).unwrap();
        assert_eq!(usage.input_tokens, 1000);
        assert_eq!(usage.output_tokens, 500);
        assert_eq!(usage.cache_read_tokens, 200);
        assert_eq!(usage.model, Some("gpt-4o".to_string()));
    }

    #[test]
    fn test_codex_response_auto_codex_format() {
        // Codex format (input_tokens/output_tokens).
        let response = json!({
            "model": "o3",
            "usage": {
                "input_tokens": 1000,
                "output_tokens": 500,
                "input_tokens_details": {
                    "cached_tokens": 300
                }
            }
        });

        let usage = TokenUsage::from_codex_response_auto(&response).unwrap();
        // Record raw input_tokens without adjustment.
        assert_eq!(usage.input_tokens, 1000);
        assert_eq!(usage.output_tokens, 500);
        assert_eq!(usage.cache_read_tokens, 300);
        assert_eq!(usage.model, Some("o3".to_string()));
    }

    #[test]
    fn test_codex_stream_events_auto_codex_format() {
        // Codex Responses API streaming format (response.completed event).
        let events = vec![
            json!({
                "type": "response.created",
                "response": {
                    "id": "resp_123"
                }
            }),
            json!({
                "type": "response.completed",
                "response": {
                    "model": "o3",
                    "usage": {
                        "input_tokens": 1000,
                        "output_tokens": 500,
                        "input_tokens_details": {
                            "cached_tokens": 200
                        }
                    }
                }
            }),
        ];

        let usage = TokenUsage::from_codex_stream_events_auto(&events).unwrap();
        // Record raw input_tokens without adjustment.
        assert_eq!(usage.input_tokens, 1000);
        assert_eq!(usage.output_tokens, 500);
        assert_eq!(usage.cache_read_tokens, 200);
        assert_eq!(usage.model, Some("o3".to_string()));
    }

    #[test]
    fn test_codex_stream_events_auto_preserves_terminal_failure_usage() {
        for event_type in ["response.incomplete", "response.failed"] {
            let events = vec![json!({
                "type": event_type,
                "response": {
                    "model": "glm-5.2",
                    "usage": {
                        "input_tokens": 1200,
                        "output_tokens": 400,
                        "input_tokens_details": {"cached_tokens": 300}
                    }
                }
            })];

            let usage = TokenUsage::from_codex_stream_events_auto(&events)
                .unwrap_or_else(|| panic!("{event_type} usage should be retained"));
            assert_eq!(usage.input_tokens, 1200);
            assert_eq!(usage.output_tokens, 400);
            assert_eq!(usage.cache_read_tokens, 300);
            assert_eq!(usage.model.as_deref(), Some("glm-5.2"));
        }
    }

    #[test]
    fn test_codex_stream_events_auto_openai_format() {
        // OpenAI Chat Completions streaming format, with usage in the final chunk.
        let events = vec![
            json!({
                "id": "chatcmpl-123",
                "model": "gpt-4o",
                "choices": [{"delta": {"content": "Hello"}}]
            }),
            json!({
                "id": "chatcmpl-123",
                "model": "gpt-4o",
                "choices": [{"delta": {}}],
                "usage": {
                    "prompt_tokens": 100,
                    "completion_tokens": 50
                }
            }),
        ];

        let usage = TokenUsage::from_codex_stream_events_auto(&events).unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.model, Some("gpt-4o".to_string()));
    }
}
