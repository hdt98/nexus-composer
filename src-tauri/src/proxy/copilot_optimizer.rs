//! Copilot request optimizer.
//!
//! Corrects abnormal GitHub Copilot proxy consumption (issue #1813).
//!
//! Copilot uses the `x-initiator` header to distinguish user initiation from agent
//! continuation:
//! - `user`: one premium interaction that consumes quota
//! - `agent`: continuation of the previous interaction with no additional charge
//!
//! Reference implementation: https://github.com/caozhiyuan/copilot-api

use std::collections::HashSet;

use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Request classification result.
#[derive(Debug, Clone)]
pub struct CopilotClassification {
    /// `user` or `agent`, mapped to the x-initiator header.
    pub initiator: &'static str,
    /// Whether this is a warmup/probe request that can use a smaller model.
    pub is_warmup: bool,
    /// Whether this is a context-compaction request.
    pub is_compact: bool,
    /// Whether Claude Code's Agent tool generated a subagent request.
    /// Subagent requests use x-interaction-type=conversation-subagent and do not count
    /// as premium interactions.
    pub is_subagent: bool,
}

/// Classifies an Anthropic request body to select Copilot headers.
///
/// Classification checks only the last message, matching caozhiyuan/copilot-api:
/// 1. No messages -> `user`, the safe first-request default.
/// 2. Last role is user:
///    - non-tool_result content -> `user`
///    - only tool_result content -> `agent`
///    - compact pattern -> `agent`
/// 3. Last role is not user -> `user`, the safe default.
///
/// Warmup detection follows the reference: `anthropic-beta`, no tools, and not compact.
///
/// `compact_detection` skips compact detection when false, honoring
/// `CopilotOptimizerConfig.compact_detection`.
///
/// `subagent_detection` scans user content for `__SUBAGENT_MARKER__` when enabled and
/// marks subagent requests as non-billable.
pub fn classify_request(
    body: &Value,
    has_anthropic_beta: bool,
    compact_detection: bool,
    subagent_detection: bool,
) -> CopilotClassification {
    let is_compact = compact_detection && is_compact_request(body);
    let is_subagent = subagent_detection && detect_subagent(body);

    let messages = match body.get("messages").and_then(|m| m.as_array()) {
        Some(msgs) if !msgs.is_empty() => msgs,
        _ => {
            return CopilotClassification {
                initiator: "user",
                is_warmup: is_warmup_request(body, has_anthropic_beta, false),
                is_compact: false,
                is_subagent,
            }
        }
    };

    let last_msg = &messages[messages.len() - 1];
    let role = last_msg.get("role").and_then(|r| r.as_str()).unwrap_or("");

    // Only role=user messages need finer classification.
    if role != "user" {
        return CopilotClassification {
            initiator: if is_subagent { "agent" } else { "user" },
            is_warmup: false,
            is_compact,
            is_subagent,
        };
    }

    // Match copilot-api's merge-then-classify result: any tool_result in content means
    // an agent tool continuation. This covers mixed [tool_result, text] messages from
    // skills, edit hooks, and plan follow-ups. Handling it during classification avoids
    // dependence on whether merging is enabled or when it runs.
    let is_user_initiated = match last_msg.get("content") {
        Some(Value::Array(blocks)) => {
            // tool_result means agent continuation; otherwise the user initiated it.
            !blocks
                .iter()
                .any(|block| block.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
        }
        Some(Value::String(_)) => true,
        _ => false,
    };

    // A subagent request is always agent-initiated, even with user text in its first message.
    let initiator = if is_subagent || !is_user_initiated || is_compact {
        "agent"
    } else {
        "user"
    };

    CopilotClassification {
        initiator,
        is_warmup: initiator == "user" && is_warmup_request(body, has_anthropic_beta, is_compact),
        is_compact,
        is_subagent,
    }
}

/// Detects a warmup/probe request suitable for a smaller model.
///
/// Matches the reference when all conditions hold:
/// 1. `anthropic-beta` header marks a Claude Code warmup probe.
/// 2. No tools are defined.
/// 3. The request is not compact.
fn is_warmup_request(body: &Value, has_anthropic_beta: bool, is_compact: bool) -> bool {
    if !has_anthropic_beta || is_compact {
        return false;
    }
    // No tool definitions.
    body.get("tools")
        .and_then(|tools| tools.as_array())
        .is_none_or(|tools| tools.is_empty())
}

/// Detects a Claude Code context-compaction request.
///
/// Matches only machine-generated Claude Code features, not generic phrases a user
/// could type, to avoid marking real user requests as agent continuations.
///
/// Strong signals:
/// 1. A dedicated compact system prompt that users cannot set manually.
/// 2. `CRITICAL: Respond with TEXT ONLY. Do NOT call any tools.`
/// 3. Both `Pending Tasks:` and `Current Work:` structural markers.
fn is_compact_request(body: &Value) -> bool {
    // Signal 1: the system prompt begins with Claude Code's dedicated compact prefix.
    // Users cannot directly control this prompt, making it the strongest signal.
    let system_text = extract_system_text(body);
    if system_text
        .starts_with("You are a helpful AI assistant tasked with summarizing conversations")
    {
        return true;
    }

    // Signals 2 and 3: inspect machine-generated features in the last user message.
    let messages = match body.get("messages").and_then(|m| m.as_array()) {
        Some(msgs) => msgs,
        None => return false,
    };

    if let Some(last_msg) = messages.last() {
        if last_msg.get("role").and_then(|r| r.as_str()) != Some("user") {
            return false;
        }

        let text = extract_text_from_message(last_msg);

        // Signal 2: exact case-sensitive Claude Code compact instruction.
        if text.contains("CRITICAL: Respond with TEXT ONLY. Do NOT call any tools.") {
            return true;
        }

        // Signal 3: both Claude Code compact structural markers must appear.
        if text.contains("Pending Tasks:") && text.contains("Current Work:") {
            return true;
        }
    }

    false
}

/// Merges tool_result and text blocks in user messages.
///
/// Matches the reference `mergeToolResultForClaude` implementation.
///
/// **Intra-message merge** (core): absorb text blocks into tool_result blocks within a
/// user message, leaving only tool_result blocks so Copilot does not count it as a
/// user-initiated interaction.
///
/// Claude Code sends mixed tool_result + text user messages for skill calls, edit
/// hooks, and plan reminders. The text block otherwise makes Copilot bill a premium
/// request.
///
/// **Cross-message merge** (supplemental): combine consecutive tool_result-only user
/// messages.
pub fn merge_tool_results(mut body: Value) -> Value {
    let messages = match body.get_mut("messages").and_then(|m| m.as_array_mut()) {
        Some(msgs) if !msgs.is_empty() => msgs,
        _ => return body,
    };

    // Phase 1: absorb text blocks into tool_result blocks within each message.
    for msg in messages.iter_mut() {
        if msg.get("role").and_then(|r| r.as_str()) != Some("user") {
            continue;
        }
        let content = match msg.get("content").and_then(|c| c.as_array()) {
            Some(blocks) => blocks,
            None => continue,
        };

        // Separate tool_result and text blocks.
        let mut tool_results: Vec<Value> = Vec::new();
        let mut text_blocks: Vec<Value> = Vec::new();
        let mut valid = true;

        for block in content {
            match block.get("type").and_then(|t| t.as_str()) {
                Some("tool_result") => tool_results.push(block.clone()),
                Some("text") => text_blocks.push(block.clone()),
                _ => {
                    // Skip a message containing any other block type.
                    valid = false;
                    break;
                }
            }
        }

        // A merge requires both tool_result and text blocks.
        if !valid || tool_results.is_empty() || text_blocks.is_empty() {
            continue;
        }

        // Merge using the reference strategy.
        let merged = merge_blocks_into_tool_results(tool_results, text_blocks);
        msg["content"] = Value::Array(merged);
    }

    // Phase 2: combine consecutive tool_result-only user messages.
    let messages = match body.get("messages").and_then(|m| m.as_array()) {
        Some(messages) => messages.clone(),
        None => return body,
    };
    if messages.len() <= 1 {
        return body;
    }

    let mut merged_msgs: Vec<Value> = Vec::with_capacity(messages.len());
    let mut i = 0;

    while i < messages.len() {
        if is_tool_result_only_message(&messages[i]) {
            let mut combined_content: Vec<Value> = Vec::new();
            while i < messages.len() && is_tool_result_only_message(&messages[i]) {
                if let Some(content) = messages[i].get("content").and_then(|c| c.as_array()) {
                    combined_content.extend(content.iter().cloned());
                }
                i += 1;
            }
            if !combined_content.is_empty() {
                merged_msgs.push(serde_json::json!({
                    "role": "user",
                    "content": combined_content
                }));
            }
        } else {
            merged_msgs.push(messages[i].clone());
            i += 1;
        }
    }

    body["messages"] = Value::Array(merged_msgs);
    body
}

/// Generates a deterministic request ID from the last user message.
///
/// Nexus Composer adds behavior beyond copilot-api's random UUID:
/// - hash sessionId + lastUserContent, excluding tool_result and cache_control
/// - identical content produces the same ID, which may help Copilot deduplicate
/// - fall back to a random UUID when no user content exists
/// - use UUID v4 format
pub fn deterministic_request_id(body: &Value, session_id: &str) -> String {
    let last_user_content = find_last_user_content(body);

    match last_user_content {
        Some(content) => {
            let mut hasher = Sha256::new();
            hasher.update(session_id.as_bytes());
            hasher.update(content.as_bytes());
            let result = hasher.finalize();

            let mut bytes = [0u8; 16];
            bytes.copy_from_slice(&result[..16]);
            // UUID v4 version and variant bits, matching the reference.
            bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
            bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant 1

            Uuid::from_bytes(bytes).to_string()
        }
        None => Uuid::new_v4().to_string(),
    }
}

/// Generates a stable interaction ID from a session ID.
///
/// Matches copilot-api session.ts:
/// - all requests in a primary conversation share one interaction ID
/// - hash only the session ID, unlike request IDs
/// - Copilot groups requests into one premium-billing interaction with this ID
/// - return None for an empty session instead of fragmenting interactions with random IDs
pub fn deterministic_interaction_id(session_id: &str) -> Option<String> {
    if session_id.is_empty() {
        return None;
    }

    let mut hasher = Sha256::new();
    hasher.update(b"interaction:");
    hasher.update(session_id.as_bytes());
    let result = hasher.finalize();

    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&result[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // variant 1

    Some(Uuid::from_bytes(bytes).to_string())
}

/// Detects a Claude Code subagent request generated by the Agent tool.
///
/// The Agent tool injects a `__SUBAGENT_MARKER__` JSON marker into a
/// `<system-reminder>` in the subagent's first user message:
/// ```json
/// {"__SUBAGENT_MARKER__": {"session_id": "...", "agent_id": "...", "agent_type": "..."}}
/// ```
///
/// Scan strategy aligned with copilot-api subagent-marker.ts:
/// 1. Inspect all user messages because context compaction may reorder them.
/// 2. Search message text for `__SUBAGENT_MARKER__`.
/// 3. Treat any match as a subagent request.
fn detect_subagent(body: &Value) -> bool {
    // Signal 1: explicit __SUBAGENT_MARKER__, injected by Claude Code 2.x+.
    if extract_system_text(body).contains("__SUBAGENT_MARKER__") {
        return true;
    }

    if let Some(messages) = body.get("messages").and_then(|m| m.as_array()) {
        for msg in messages {
            if msg.get("role").and_then(|r| r.as_str()) != Some("user") {
                continue;
            }
            let text = extract_text_from_message(msg);
            if text.contains("__SUBAGENT_MARKER__") {
                return true;
            }
        }
    }

    // Signal 2 fallback: metadata.user_id contains the subagent marker. Claude Code's
    // Agent tool uses `parentSessionId_agent_agentId`; detect `_agent_`.
    if let Some(user_id) = body.pointer("/metadata/user_id").and_then(|v| v.as_str()) {
        // `_agent_` is the Agent tool's internal marker.
        if user_id.contains("_agent_") {
            return true;
        }
    }

    // Signal 3 fallback would inspect typical subagent system-prompt framing. Agent tool
    // task descriptions differ from primary CLI prompts, but users may type the same
    // words, so this weak signal remains disabled as a reserved extension point.

    false
}

/// Converts orphaned tool_results without a matching tool_use into text blocks.
///
/// Context compaction or truncation may remove an assistant tool_use while leaving the
/// later user tool_result, causing upstream mismatch errors and retries.
///
/// Matches copilot-api's `sanitizeOrphanToolResults`.
pub fn sanitize_orphan_tool_results(mut body: Value) -> Value {
    let messages = match body.get_mut("messages").and_then(|m| m.as_array_mut()) {
        Some(msgs) if msgs.len() >= 2 => msgs,
        _ => return body,
    };

    // Anthropic requires tool_result immediately after the assistant turn containing
    // its tool_use. Check only messages[i-1], matching sanitizeOrphanToolResults.
    for i in 1..messages.len() {
        if messages[i].get("role").and_then(|r| r.as_str()) != Some("user") {
            continue;
        }

        // Collect tool_use IDs from the immediately preceding assistant message.
        let prev_tool_use_ids: HashSet<String> =
            if messages[i - 1].get("role").and_then(|r| r.as_str()) == Some("assistant") {
                messages[i - 1]
                    .get("content")
                    .and_then(|c| c.as_array())
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
                            .filter_map(|b| b.get("id").and_then(|i| i.as_str()).map(String::from))
                            .collect()
                    })
                    .unwrap_or_default()
            } else {
                // If the previous message is not assistant, every tool_result here is orphaned.
                HashSet::new()
            };

        let content = match messages[i]
            .get_mut("content")
            .and_then(|c| c.as_array_mut())
        {
            Some(blocks) => blocks,
            None => continue,
        };

        for block in content.iter_mut() {
            if block.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                continue;
            }
            let tool_use_id = block
                .get("tool_use_id")
                .and_then(|id| id.as_str())
                .unwrap_or("");
            // An empty ID or one absent from the adjacent assistant tool_uses is orphaned.
            if tool_use_id.is_empty() || !prev_tool_use_ids.contains(tool_use_id) {
                let content_text = match block.get("content") {
                    Some(Value::String(text)) => text.clone(),
                    Some(Value::Array(blocks)) => blocks
                        .iter()
                        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    _ => String::new(),
                };
                *block = serde_json::json!({
                    "type": "text",
                    "text": format!("[Tool result for {}]: {}", tool_use_id, content_text)
                });
            }
        }
    }

    body
}

/// Proactively strips thinking and redacted_thinking blocks from assistant messages.
///
/// Copilot's `/chat/completions`, `/v1/responses`, and `/v1/chat/completions` endpoints
/// are OpenAI-compatible and reject Anthropic thinking blocks with
/// invalid_request_error. Reactive cleanup by thinking_rectifier would retry only after
/// the failed request consumed premium quota, so strip these blocks before sending.
///
/// Unlike `thinking_rectifier::rectify_anthropic_request`:
/// - This removes only thinking and redacted_thinking blocks, leaving signatures and
///   the top-level thinking field for aggressive error-path rectification.
/// - It consumes and returns a body like `merge_tool_results` and
///   `sanitize_orphan_tool_results`, fitting the forwarder pipeline.
pub fn strip_thinking_blocks(mut body: Value) -> Value {
    let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return body;
    };

    for msg in messages.iter_mut() {
        if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            continue;
        }
        let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut()) else {
            continue;
        };
        content.retain(|block| {
            !matches!(
                block.get("type").and_then(|t| t.as_str()),
                Some("thinking") | Some("redacted_thinking")
            )
        });
    }

    body
}

// ─── Internal helpers ─────────────────────────────

/// Extracts text from a string- or array-valued `system` field.
fn extract_system_text(body: &Value) -> String {
    match body.get("system") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// Finds non-tool_result text in the last user message.
///
/// Matches `findLastUserContent`: scan backward, exclude tool_result blocks, and remove
/// cache_control fields.
fn find_last_user_content(body: &Value) -> Option<String> {
    let messages = body.get("messages").and_then(|m| m.as_array())?;

    for msg in messages.iter().rev() {
        if msg.get("role").and_then(|r| r.as_str()) != Some("user") {
            continue;
        }
        let content = msg.get("content")?;

        if let Some(s) = content.as_str() {
            return Some(s.to_string());
        }

        if let Some(blocks) = content.as_array() {
            // Exclude tool_result, preserve other blocks, and remove cache_control.
            let filtered: Vec<Value> = blocks
                .iter()
                .filter(|b| b.get("type").and_then(|t| t.as_str()) != Some("tool_result"))
                .map(|b| {
                    let mut b = b.clone();
                    if let Some(obj) = b.as_object_mut() {
                        obj.remove("cache_control");
                    }
                    b
                })
                .collect();

            if !filtered.is_empty() {
                return Some(serde_json::to_string(&filtered).unwrap_or_default());
            }
        }
    }

    None
}

/// Merges text blocks into tool_result blocks.
///
/// Reference merge strategies:
/// - Equal counts: append each text to its corresponding tool_result content.
/// - Unequal counts: append all text to the last tool_result content.
fn merge_blocks_into_tool_results(
    mut tool_results: Vec<Value>,
    text_blocks: Vec<Value>,
) -> Vec<Value> {
    if tool_results.len() == text_blocks.len() {
        // Merge one-to-one.
        for (tr, tb) in tool_results.iter_mut().zip(text_blocks.iter()) {
            append_text_to_tool_result(tr, tb);
        }
    } else {
        // Append all text to the final tool_result.
        if let Some(last_tr) = tool_results.last_mut() {
            for tb in &text_blocks {
                append_text_to_tool_result(last_tr, tb);
            }
        }
    }
    tool_results
}

/// Appends a text block to tool_result content.
fn append_text_to_tool_result(tool_result: &mut Value, text_block: &Value) {
    let text = text_block
        .get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("");
    if text.trim().is_empty() {
        return;
    }

    // tool_result content may be a string or an array.
    match tool_result.get_mut("content") {
        Some(Value::String(existing)) => {
            existing.push('\n');
            existing.push_str(text);
        }
        Some(Value::Array(arr)) => {
            arr.push(serde_json::json!({"type": "text", "text": text}));
        }
        _ => {
            // Set content directly when missing or null.
            tool_result["content"] = Value::String(text.to_string());
        }
    }
}

/// Extracts text from a message.
fn extract_text_from_message(msg: &Value) -> String {
    match msg.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| {
                if block.get("type").and_then(|t| t.as_str()) == Some("text") {
                    block.get("text").and_then(|t| t.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

/// Returns whether a user message contains only tool_result blocks.
fn is_tool_result_only_message(msg: &Value) -> bool {
    if msg.get("role").and_then(|r| r.as_str()) != Some("user") {
        return false;
    }
    match msg.get("content").and_then(|c| c.as_array()) {
        Some(blocks) if !blocks.is_empty() => blocks
            .iter()
            .all(|block| block.get("type").and_then(|t| t.as_str()) == Some("tool_result")),
        _ => false,
    }
}

// ─── Tests ─────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // === classify_request tests ===

    #[test]
    fn test_classify_user_text_message() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": "Hello, please help me write some code"}
            ]
        });
        let result = classify_request(&body, false, true, false);
        assert_eq!(result.initiator, "user");
        assert!(!result.is_compact);
    }

    #[test]
    fn test_classify_user_text_array_message() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "Please explain this code"}
                ]}
            ]
        });
        let result = classify_request(&body, false, true, false);
        assert_eq!(result.initiator, "user");
    }

    #[test]
    fn test_classify_tool_result_only() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "tools": [{"name": "Read", "description": "Read a file", "input_schema": {}}],
            "messages": [
                {"role": "user", "content": "Read the file"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "I'll read that file."},
                    {"type": "tool_use", "id": "toolu_123", "name": "Read", "input": {"path": "/tmp/test.rs"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_123", "content": "file contents here"}
                ]}
            ]
        });
        let result = classify_request(&body, true, true, false);
        assert_eq!(result.initiator, "agent");
        assert!(!result.is_warmup);
    }

    #[test]
    fn test_classify_tool_result_with_text_block() {
        // A tool_result + text block is common in skill/edit-hook/plan follow-ups.
        // Any tool_result marks a tool continuation, so classify it as agent.
        // This matches the copilot-api merge-then-classify behavior.
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_123", "content": "file contents"},
                    {"type": "text", "text": "Now please refactor this code"}
                ]}
            ]
        });
        let result = classify_request(&body, false, true, false);
        assert_eq!(result.initiator, "agent");
    }

    #[test]
    fn test_classify_empty_messages() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": []
        });
        let result = classify_request(&body, false, true, false);
        assert_eq!(result.initiator, "user");
    }

    #[test]
    fn test_classify_no_messages() {
        let body = json!({"model": "claude-sonnet-4-20250514"});
        let result = classify_request(&body, false, true, false);
        assert_eq!(result.initiator, "user");
    }

    #[test]
    fn test_classify_compact_request_system_prompt() {
        // Detect compact requests through a strong system-prompt signal.
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "system": "You are a helpful AI assistant tasked with summarizing conversations. Please create a summary.",
            "messages": [
                {"role": "user", "content": "Here is the conversation history to summarize..."}
            ]
        });
        let result = classify_request(&body, false, true, false);
        assert_eq!(result.initiator, "agent");
        assert!(result.is_compact);
    }

    #[test]
    fn test_classify_compact_request_critical_marker() {
        // Detect compact requests through the machine-generated CRITICAL instruction.
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "CRITICAL: Respond with TEXT ONLY. Do NOT call any tools. Summarize the conversation."}
                ]}
            ]
        });
        let result = classify_request(&body, false, true, false);
        assert_eq!(result.initiator, "agent");
        assert!(result.is_compact);
    }

    #[test]
    fn test_classify_compact_disabled_by_config() {
        // compact_detection=false suppresses compact classification even on a match.
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "system": "You are a helpful AI assistant tasked with summarizing conversations.",
            "messages": [
                {"role": "user", "content": "Summarize"}
            ]
        });
        let result = classify_request(&body, false, false, false); // compact_detection=false
        assert_eq!(result.initiator, "user"); // Not classified as agent.
        assert!(!result.is_compact);
    }

    #[test]
    fn test_no_false_positive_on_user_summarize_request() {
        // P1 regression: user-entered "summarize the conversation" is not compact.
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": "Please summarize the conversation so far into a concise summary."}
            ]
        });
        let result = classify_request(&body, false, true, false);
        // Without a strong system-prompt signal or CRITICAL instruction, this is user input.
        assert_eq!(result.initiator, "user");
        assert!(!result.is_compact);
    }

    // === warmup tests aligned with the reference implementation ===

    #[test]
    fn test_warmup_with_anthropic_beta_no_tools() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });
        // has_anthropic_beta=true with no tools indicates a warmup request.
        let result = classify_request(&body, true, true, false);
        assert!(result.is_warmup);
    }

    #[test]
    fn test_not_warmup_without_anthropic_beta() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });
        // has_anthropic_beta=false means this is not a warmup request.
        let result = classify_request(&body, false, true, false);
        assert!(!result.is_warmup);
    }

    #[test]
    fn test_not_warmup_with_tools() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "tools": [{"name": "Read", "description": "Read a file", "input_schema": {}}],
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });
        // Tools prevent warmup classification even with anthropic-beta.
        let result = classify_request(&body, true, true, false);
        assert!(!result.is_warmup);
    }

    #[test]
    fn test_not_warmup_when_agent() {
        // A tool_result means agent, so this is not a warmup request.
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_123", "content": "ok"}
                ]}
            ]
        });
        let result = classify_request(&body, true, true, false);
        assert_eq!(result.initiator, "agent");
        assert!(!result.is_warmup);
    }

    // === merge_tool_results tests ===

    #[test]
    fn test_merge_intra_message_tool_result_text() {
        // Core case: absorb text into tool_result within one message.
        let body = json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "file contents"},
                    {"type": "text", "text": "skill output here"}
                ]}
            ]
        });
        let result = merge_tool_results(body);
        let content = result["messages"][0]["content"].as_array().unwrap();
        // Only one tool_result block remains after absorbing the text.
        assert_eq!(content.len(), 1);
        assert_eq!(content[0]["type"], "tool_result");
        // tool_result content includes both the original content and absorbed text.
        let tr_content = content[0]["content"].as_str().unwrap();
        assert!(tr_content.contains("file contents"));
        assert!(tr_content.contains("skill output here"));
    }

    #[test]
    fn test_merge_intra_message_equal_count() {
        // Equal counts merge one-to-one.
        let body = json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "result1"},
                    {"type": "text", "text": "text1"},
                    {"type": "tool_result", "tool_use_id": "t2", "content": "result2"},
                    {"type": "text", "text": "text2"}
                ]}
            ]
        });
        let result = merge_tool_results(body);
        let content = result["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert!(content[0]["content"].as_str().unwrap().contains("text1"));
        assert!(content[1]["content"].as_str().unwrap().contains("text2"));
    }

    #[test]
    fn test_merge_intra_message_empty_text_ignored() {
        // Ignore an empty text block.
        let body = json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "result"},
                    {"type": "text", "text": ""}
                ]}
            ]
        });
        let result = merge_tool_results(body);
        let content = result["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        // Empty text does not change the original content.
        assert_eq!(content[0]["content"], "result");
    }

    #[test]
    fn test_merge_intra_skips_other_block_types() {
        // Skip a message containing a block other than tool_result or text.
        let body = json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "result"},
                    {"type": "image", "source": {"data": "..."}},
                    {"type": "text", "text": "caption"}
                ]}
            ]
        });
        let result = merge_tool_results(body);
        let content = result["messages"][0]["content"].as_array().unwrap();
        // No merge occurs; preserve all three original blocks.
        assert_eq!(content.len(), 3);
    }

    #[test]
    fn test_merge_cross_message_consecutive() {
        // Merge consecutive tool_result-only user messages across messages.
        let body = json!({
            "messages": [
                {"role": "user", "content": "Read files"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "t1", "name": "Read", "input": {}},
                    {"type": "tool_use", "id": "t2", "name": "Read", "input": {}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "file1"}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t2", "content": "file2"}
                ]}
            ]
        });
        let result = merge_tool_results(body);
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);
        let merged_content = messages[2]["content"].as_array().unwrap();
        assert_eq!(merged_content.len(), 2);
    }

    #[test]
    fn test_merge_does_not_affect_normal_messages() {
        let body = json!({
            "messages": [
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hi!"},
                {"role": "user", "content": "How are you?"}
            ]
        });
        let result = merge_tool_results(body.clone());
        assert_eq!(result["messages"], body["messages"]);
    }

    // === deterministic_request_id tests ===

    #[test]
    fn test_deterministic_id_stable() {
        let body = json!({
            "model": "claude-sonnet-4-20250514",
            "messages": [{"role": "user", "content": "Hello"}]
        });
        let id1 = deterministic_request_id(&body, "session1");
        let id2 = deterministic_request_id(&body, "session1");
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_deterministic_id_varies_by_content() {
        let body1 = json!({
            "messages": [{"role": "user", "content": "Hello"}]
        });
        let body2 = json!({
            "messages": [{"role": "user", "content": "Goodbye"}]
        });
        let id1 = deterministic_request_id(&body1, "session1");
        let id2 = deterministic_request_id(&body2, "session1");
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_deterministic_id_varies_by_session() {
        let body = json!({
            "messages": [{"role": "user", "content": "Hello"}]
        });
        let id1 = deterministic_request_id(&body, "session1");
        let id2 = deterministic_request_id(&body, "session2");
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_deterministic_id_ignores_tool_result() {
        // Different tool_result content with the same user text produces the same ID.
        let body1 = json!({
            "messages": [
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hi"},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "version_A"}
                ]},
                {"role": "user", "content": "do something"}
            ]
        });
        let body2 = json!({
            "messages": [
                {"role": "user", "content": "Hello"},
                {"role": "assistant", "content": "Hi"},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "version_B"}
                ]},
                {"role": "user", "content": "do something"}
            ]
        });
        let id1 = deterministic_request_id(&body1, "s");
        let id2 = deterministic_request_id(&body2, "s");
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_deterministic_id_fallback_when_no_user_content() {
        // With no user message, fall back to a fresh random UUID.
        let body = json!({
            "messages": [
                {"role": "assistant", "content": "Hi"}
            ]
        });
        let id1 = deterministic_request_id(&body, "s");
        let id2 = deterministic_request_id(&body, "s");
        // A random UUID should differ on each call.
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_deterministic_id_is_valid_uuid() {
        let body = json!({
            "messages": [{"role": "user", "content": "test"}]
        });
        let id = deterministic_request_id(&body, "session");
        assert!(Uuid::parse_str(&id).is_ok());
    }

    // === deterministic_interaction_id tests ===

    #[test]
    fn test_interaction_id_stable_for_same_session() {
        let id1 = deterministic_interaction_id("session_abc");
        let id2 = deterministic_interaction_id("session_abc");
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_interaction_id_differs_across_sessions() {
        let id1 = deterministic_interaction_id("session_abc");
        let id2 = deterministic_interaction_id("session_def");
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_interaction_id_differs_from_request_id() {
        let body = json!({
            "messages": [{"role": "user", "content": "Hello"}]
        });
        let interaction = deterministic_interaction_id("session_abc").unwrap();
        let request = deterministic_request_id(&body, "session_abc");
        assert_ne!(interaction, request);
    }

    #[test]
    fn test_interaction_id_empty_session_is_none() {
        // Without a session, omit the interaction ID to avoid fragmentation.
        assert!(deterministic_interaction_id("").is_none());
    }

    #[test]
    fn test_interaction_id_is_valid_uuid() {
        let id = deterministic_interaction_id("test_session").unwrap();
        assert!(Uuid::parse_str(&id).is_ok());
    }

    // === extended compact-detection tests ===

    #[test]
    fn test_compact_detection_system_prompt() {
        let body = json!({
            "system": "You are a helpful AI assistant tasked with summarizing conversations. Please provide a concise summary.",
            "messages": [
                {"role": "user", "content": "Here is the conversation to summarize..."}
            ]
        });
        assert!(is_compact_request(&body));
    }

    #[test]
    fn test_compact_detection_critical_keyword() {
        let body = json!({
            "messages": [
                {"role": "user", "content": "CRITICAL: Respond with TEXT ONLY. Do NOT call any tools. Summarize this conversation."}
            ]
        });
        assert!(is_compact_request(&body));
    }

    #[test]
    fn test_compact_detection_structural_markers() {
        // Structural markers specific to Claude Code compact requests.
        let body = json!({
            "messages": [
                {"role": "user", "content": "Summary of conversation:\n\nPending Tasks:\n- Fix bug\n\nCurrent Work:\n- Implementing feature"}
            ]
        });
        assert!(is_compact_request(&body));
    }

    #[test]
    fn test_compact_no_false_positive_on_generic_summary() {
        // A generic phrase must not trigger compact detection.
        let body = json!({
            "messages": [
                {"role": "user", "content": "Your task is to create a detailed summary of the conversation so far."}
            ]
        });
        assert!(!is_compact_request(&body));
    }

    #[test]
    fn test_compact_detection_negative() {
        let body = json!({
            "messages": [
                {"role": "user", "content": "What is the weather today?"}
            ]
        });
        assert!(!is_compact_request(&body));
    }

    #[test]
    fn test_compact_detection_system_array() {
        let body = json!({
            "system": [
                {"type": "text", "text": "You are a helpful AI assistant tasked with summarizing conversations."}
            ],
            "messages": [
                {"role": "user", "content": "Summarize"}
            ]
        });
        assert!(is_compact_request(&body));
    }

    // === detect_subagent tests ===

    #[test]
    fn test_detect_subagent_with_marker_in_user_message() {
        let body = json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "<system-reminder>\n{\"__SUBAGENT_MARKER__\":{\"session_id\":\"abc123\",\"agent_id\":\"explore-1\",\"agent_type\":\"Explore\"}}\n</system-reminder>\nPlease search the codebase for auth handlers"}
                ]}
            ]
        });
        assert!(detect_subagent(&body));
    }

    #[test]
    fn test_detect_subagent_with_marker_in_system() {
        let body = json!({
            "system": "You are an agent. {\"__SUBAGENT_MARKER__\":{\"session_id\":\"abc\",\"agent_id\":\"plan-1\",\"agent_type\":\"Plan\"}}",
            "messages": [
                {"role": "user", "content": "Design the implementation plan"}
            ]
        });
        assert!(detect_subagent(&body));
    }

    #[test]
    fn test_detect_subagent_no_marker() {
        let body = json!({
            "messages": [
                {"role": "user", "content": "Hello, please help me write code"}
            ]
        });
        assert!(!detect_subagent(&body));
    }

    #[test]
    fn test_detect_subagent_via_metadata_user_id() {
        // Fallback signal: metadata.user_id contains the "_agent_" marker.
        let body = json!({
            "metadata": {
                "user_id": "session_abc123_agent_explore-1"
            },
            "messages": [
                {"role": "user", "content": "Search for files"}
            ]
        });
        assert!(detect_subagent(&body));
    }

    #[test]
    fn test_detect_subagent_normal_user_id_not_matched() {
        // A normal session ID must not be classified as a subagent.
        let body = json!({
            "metadata": {
                "user_id": "session_abc123"
            },
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });
        assert!(!detect_subagent(&body));
    }

    #[test]
    fn test_classify_subagent_sets_agent_initiator() {
        let body = json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "<system-reminder>\n{\"__SUBAGENT_MARKER__\":{\"session_id\":\"abc\",\"agent_id\":\"explore-1\",\"agent_type\":\"Explore\"}}\n</system-reminder>\nSearch for files"}
                ]}
            ]
        });
        let result = classify_request(&body, false, true, true);
        assert_eq!(result.initiator, "agent");
        assert!(result.is_subagent);
    }

    #[test]
    fn test_classify_subagent_disabled_flag() {
        let body = json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "<system-reminder>\n{\"__SUBAGENT_MARKER__\":{\"session_id\":\"abc\",\"agent_id\":\"explore-1\",\"agent_type\":\"Explore\"}}\n</system-reminder>\nSearch for files"}
                ]}
            ]
        });
        // subagent_detection=false disables subagent detection.
        let result = classify_request(&body, false, true, false);
        assert_eq!(result.initiator, "user");
        assert!(!result.is_subagent);
    }

    // === sanitize_orphan_tool_results tests ===

    #[test]
    fn test_sanitize_orphan_tool_results_converts_orphans() {
        let body = json!({
            "messages": [
                {"role": "user", "content": "Help me"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "tool_1", "name": "read_file", "input": {}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "tool_1", "content": "file contents"},
                    {"type": "tool_result", "tool_use_id": "tool_orphan", "content": "orphan data"}
                ]}
            ]
        });
        let result = sanitize_orphan_tool_results(body);
        let msgs = result["messages"].as_array().unwrap();
        let last_content = msgs[2]["content"].as_array().unwrap();
        // Preserve tool_1 as a tool_result.
        assert_eq!(last_content[0]["type"], "tool_result");
        // Convert tool_orphan to text.
        assert_eq!(last_content[1]["type"], "text");
        assert!(last_content[1]["text"]
            .as_str()
            .unwrap()
            .contains("tool_orphan"));
    }

    #[test]
    fn test_sanitize_orphan_tool_results_no_orphans() {
        let body = json!({
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "tool_1", "name": "read_file", "input": {}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "tool_1", "content": "ok"}
                ]}
            ]
        });
        let result = sanitize_orphan_tool_results(body.clone());
        // With no orphaned tool_result, the message remains unchanged.
        assert_eq!(result["messages"][1]["content"][0]["type"], "tool_result");
    }

    #[test]
    fn test_sanitize_orphan_non_adjacent_assistant_tool_use_is_orphan() {
        // The tool_use is in an earlier assistant message, but another assistant message
        // immediately precedes the tool_result, making it orphaned under Anthropic semantics.
        let body = json!({
            "messages": [
                {"role": "user", "content": "step 1"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "old_tool", "name": "search", "input": {}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "old_tool", "content": "found it"}
                ]},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "OK, now let me think..."}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "old_tool", "content": "stale ref"}
                ]}
            ]
        });
        let result = sanitize_orphan_tool_results(body);
        let msgs = result["messages"].as_array().unwrap();
        // messages[2]: the preceding assistant has old_tool, so preserve it.
        assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
        // messages[4]: the preceding assistant has no tool_use, so convert it to text.
        assert_eq!(msgs[4]["content"][0]["type"], "text");
    }

    #[test]
    fn test_sanitize_orphan_prev_not_assistant() {
        // A preceding user message makes all adjacent tool_results orphaned.
        let body = json!({
            "messages": [
                {"role": "user", "content": "first"},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "t1", "content": "data"}
                ]}
            ]
        });
        let result = sanitize_orphan_tool_results(body);
        assert_eq!(result["messages"][1]["content"][0]["type"], "text");
    }

    /// Key case: an orphaned tool_result whose adjacent tool_use was lost during compaction.
    /// Classification must still treat it as an agent continuation before sanitization
    /// converts it to text.
    ///
    /// This verifies that classify_request identifies the orphaned tool_result as agent
    /// from the original, unsanitized body.
    #[test]
    fn test_orphan_tool_result_classified_as_agent_before_sanitize() {
        // The final user message contains only tool_results, but the immediately preceding
        // assistant message lacks the matching tool_use because compaction removed it.
        let body = json!({
            "messages": [
                {"role": "assistant", "content": "I'll help you with that."},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "orphan_tool_1", "content": "file contents here"},
                    {"type": "tool_result", "tool_use_id": "orphan_tool_2", "content": "another result"}
                ]}
            ]
        });
        // Classify the original body as agent because it contains only tool_results.
        let classification = classify_request(&body, false, false, false);
        assert_eq!(classification.initiator, "agent");

        // After sanitization, tool_result becomes text and reclassification yields user.
        let sanitized = sanitize_orphan_tool_results(body);
        let classification_after = classify_request(&sanitized, false, false, false);
        assert_eq!(
            classification_after.initiator, "user",
            "After sanitization, orphaned tool_result becomes text and classifies as user; \
             classification must run before sanitization"
        );
    }

    /// Mixed orphaned tool_result + text case:
    /// The classifier identifies any message containing tool_result as agent, regardless
    /// of text blocks or merge order. Classification occurs before sanitization can convert
    /// the orphaned tool_result to text.
    #[test]
    fn test_orphan_tool_result_with_text_classified_as_agent() {
        let body = json!({
            "messages": [
                {"role": "assistant", "content": "Processing..."},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "orphan_1", "content": "result data"},
                    {"type": "text", "text": "Here's the output from the tool"}
                ]}
            ]
        });
        // Any tool_result classifies as agent, regardless of accompanying text blocks.
        let classification = classify_request(&body, false, false, false);
        assert_eq!(classification.initiator, "agent");

        // Sanitization converts the orphaned tool_result to text, which would classify as user.
        // The intended order classifies first, so this does not affect behavior.
        let sanitized = sanitize_orphan_tool_results(body);
        let classification_after = classify_request(&sanitized, false, false, false);
        assert_eq!(classification_after.initiator, "user");
    }

    #[test]
    fn test_sanitize_orphan_empty_tool_use_id_is_orphan() {
        // An empty or missing tool_use_id cannot match a tool_use and is therefore orphaned.
        let body = json!({
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "tool_1", "name": "read", "input": {}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "", "content": "empty id"},
                    {"type": "tool_result", "content": "missing id field"}
                ]}
            ]
        });
        let result = sanitize_orphan_tool_results(body);
        let content = result["messages"][1]["content"].as_array().unwrap();
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "text");
    }

    // === strip_thinking_blocks tests ===

    #[test]
    fn test_strip_thinking_removes_assistant_thinking_blocks() {
        let body = serde_json::json!({
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "hi"}]},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "let me ponder", "signature": "sig"},
                    {"type": "redacted_thinking", "data": "opaque"},
                    {"type": "text", "text": "hello"},
                    {"type": "tool_use", "id": "t1", "name": "read", "input": {}}
                ]}
            ]
        });
        let result = strip_thinking_blocks(body);
        let content = result["messages"][1]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "tool_use");
    }

    #[test]
    fn test_strip_thinking_leaves_user_messages_untouched() {
        // Only assistant messages are processed; preserve rare user thinking blocks.
        let body = serde_json::json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "thinking", "thinking": "x"},
                    {"type": "text", "text": "hi"}
                ]}
            ]
        });
        let result = strip_thinking_blocks(body);
        let content = result["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
    }

    #[test]
    fn test_strip_thinking_handles_missing_messages() {
        let body = serde_json::json!({ "model": "claude-3-5-sonnet" });
        let result = strip_thinking_blocks(body.clone());
        assert_eq!(result, body);
    }

    #[test]
    fn test_strip_thinking_leaves_empty_content_array() {
        // Stripping a thinking-only assistant message leaves empty content for upstream handling.
        let body = serde_json::json!({
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "solo"}
                ]}
            ]
        });
        let result = strip_thinking_blocks(body);
        let content = result["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 0);
    }

    #[test]
    fn test_strip_thinking_preserves_signature_on_non_thinking_blocks() {
        // Preserve signatures for thinking_rectifier to handle on the error path.
        let body = serde_json::json!({
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "t1", "name": "x", "input": {}, "signature": "s"}
                ]}
            ]
        });
        let result = strip_thinking_blocks(body);
        let block = &result["messages"][0]["content"][0];
        assert_eq!(block["signature"], "s");
    }

    #[test]
    fn test_strip_thinking_multiple_assistant_turns() {
        let body = serde_json::json!({
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "q1"}]},
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "a"},
                    {"type": "text", "text": "r1"}
                ]},
                {"role": "user", "content": [{"type": "text", "text": "q2"}]},
                {"role": "assistant", "content": [
                    {"type": "redacted_thinking", "data": "x"},
                    {"type": "text", "text": "r2"}
                ]}
            ]
        });
        let result = strip_thinking_blocks(body);
        let a1 = result["messages"][1]["content"].as_array().unwrap();
        let a2 = result["messages"][3]["content"].as_array().unwrap();
        assert_eq!(a1.len(), 1);
        assert_eq!(a1[0]["text"], "r1");
        assert_eq!(a2.len(), 1);
        assert_eq!(a2[0]["text"], "r2");
    }

    #[test]
    fn test_strip_thinking_ignores_string_content() {
        // Historical requests or minimal clients may use string assistant.content.
        // Do not panic or alter its structure.
        let body = serde_json::json!({
            "messages": [
                {"role": "assistant", "content": "plain text response"}
            ]
        });
        let result = strip_thinking_blocks(body.clone());
        assert_eq!(result, body);
    }

    #[test]
    fn test_strip_thinking_preserves_block_order() {
        let body = serde_json::json!({
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "thinking", "thinking": "pre"},
                    {"type": "text", "text": "A"},
                    {"type": "tool_use", "id": "t1", "name": "x", "input": {}},
                    {"type": "redacted_thinking", "data": "mid"},
                    {"type": "text", "text": "B"}
                ]}
            ]
        });
        let result = strip_thinking_blocks(body);
        let content = result["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 3);
        assert_eq!(content[0]["text"], "A");
        assert_eq!(content[1]["type"], "tool_use");
        assert_eq!(content[2]["text"], "B");
    }
}
