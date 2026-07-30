//! Codex Responses ↔ OpenAI Chat Completions conversion.
//!
//! This module is used when the Codex client talks to Nexus Composer through the
//! Responses API, while the selected upstream provider only exposes an
//! OpenAI-compatible Chat Completions endpoint.

use super::codex_chat_common::{
    append_reasoning_content, extract_reasoning_field_text, extract_reasoning_summary_text,
    response_function_call_item, response_function_call_item_with_namespace,
    split_leading_think_block,
};
use crate::provider::CodexChatReasoningConfig;
use crate::proxy::{
    error::ProxyError,
    json_canonical::{
        canonical_json_string, canonicalize_json_string_if_parseable, canonicalize_tool_arguments,
        short_sha256_hex,
    },
};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

const EXTRA_CHAT_PASSTHROUGH_FIELDS: &[&str] = &[
    "frequency_penalty",
    "logit_bias",
    "logprobs",
    "metadata",
    "n",
    "parallel_tool_calls",
    "presence_penalty",
    "response_format",
    "seed",
    "service_tier",
    "stop",
    "stream_options",
    "top_logprobs",
    "user",
];

const TOOL_SEARCH_PROXY_NAME: &str = "tool_search";
const CUSTOM_TOOL_INPUT_FIELD: &str = "input";
const CHAT_TOOL_NAME_MAX_LEN: usize = 64;
const CUSTOM_TOOL_INPUT_DESCRIPTION: &str = "Raw string input for the original custom tool. Preserve formatting exactly and follow the original tool definition embedded in the description.";
const CUSTOM_TOOL_PRESERVED_METADATA_HEADING: &str = "Original tool definition:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CodexToolKind {
    Function,
    Namespace,
    Custom,
    ToolSearch,
}

#[derive(Debug, Clone)]
pub(crate) struct CodexToolSpec {
    pub(crate) kind: CodexToolKind,
    pub(crate) name: String,
    pub(crate) namespace: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CodexToolContext {
    chat_tools: Vec<Value>,
    seen_chat_names: HashSet<String>,
    chat_name_to_spec: HashMap<String, CodexToolSpec>,
    namespace_name_to_chat_name: HashMap<(String, String), String>,
}

impl CodexToolContext {
    pub(crate) fn chat_tools(&self) -> &[Value] {
        &self.chat_tools
    }

    pub(crate) fn lookup_chat_name(&self, chat_name: &str) -> Option<&CodexToolSpec> {
        self.chat_name_to_spec.get(chat_name)
    }

    pub(crate) fn is_custom_tool_chat_name(&self, chat_name: &str) -> bool {
        self.lookup_chat_name(chat_name)
            .is_some_and(|spec| matches!(&spec.kind, CodexToolKind::Custom))
    }

    fn chat_name_for_response_function(&self, name: &str, namespace: Option<&str>) -> String {
        if let Some(namespace) = namespace.filter(|value| !value.is_empty()) {
            if let Some(chat_name) = self
                .namespace_name_to_chat_name
                .get(&(namespace.to_string(), name.to_string()))
            {
                return chat_name.clone();
            }
            return flatten_namespace_tool_name(namespace, name);
        }

        name.to_string()
    }

    fn add_chat_tool(&mut self, chat_name: String, spec: CodexToolSpec, chat_tool: Value) {
        if chat_name.trim().is_empty() || self.seen_chat_names.contains(&chat_name) {
            return;
        }
        self.seen_chat_names.insert(chat_name.clone());
        if let Some(namespace) = spec.namespace.as_ref() {
            self.namespace_name_to_chat_name
                .insert((namespace.clone(), spec.name.clone()), chat_name.clone());
        }
        self.chat_name_to_spec.insert(chat_name, spec);
        self.chat_tools.push(chat_tool);
    }

    fn add_function_tool(&mut self, tool: &Value, namespace: Option<&str>) {
        let Some(original_name) = responses_tool_name(tool) else {
            return;
        };
        let chat_name = namespace
            .map(|namespace| flatten_namespace_tool_name(namespace, &original_name))
            .unwrap_or_else(|| original_name.clone());

        let Some(chat_tool) = responses_function_tool_to_chat_tool(tool, &chat_name) else {
            return;
        };
        let spec = CodexToolSpec {
            kind: if namespace.is_some() {
                CodexToolKind::Namespace
            } else {
                CodexToolKind::Function
            },
            name: original_name,
            namespace: namespace.map(ToString::to_string),
        };
        self.add_chat_tool(chat_name, spec, chat_tool);
    }

    fn add_custom_tool(&mut self, tool: &Value) {
        let Some(name) = responses_tool_name(tool) else {
            return;
        };
        let description = json!(responses_custom_tool_description(tool));
        let chat_tool = json!({
            "type": "function",
            "function": {
                "name": name,
                "description": description,
                "parameters": {
                    "type": "object",
                    "properties": {
                        CUSTOM_TOOL_INPUT_FIELD: {
                            "type": "string",
                            "description": CUSTOM_TOOL_INPUT_DESCRIPTION
                        }
                    },
                    "required": [CUSTOM_TOOL_INPUT_FIELD]
                }
            }
        });
        let spec = CodexToolSpec {
            kind: CodexToolKind::Custom,
            name: name.clone(),
            namespace: None,
        };
        self.add_chat_tool(name, spec, chat_tool);
    }

    fn add_tool_search_tool(&mut self) {
        let chat_tool = json!({
            "type": "function",
            "function": {
                "name": TOOL_SEARCH_PROXY_NAME,
                "description": "Search and load Codex tools, plugins, connectors, and MCP namespaces for the current task.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Search query for tools or connectors to load."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of tool groups to return."
                        }
                    },
                    "required": ["query"]
                }
            }
        });
        let spec = CodexToolSpec {
            kind: CodexToolKind::ToolSearch,
            name: TOOL_SEARCH_PROXY_NAME.to_string(),
            namespace: None,
        };
        self.add_chat_tool(TOOL_SEARCH_PROXY_NAME.to_string(), spec, chat_tool);
    }

    fn add_namespace_tool(&mut self, namespace_tool: &Value) {
        let Some(namespace) = namespace_tool.get("name").and_then(|v| v.as_str()) else {
            return;
        };
        let Some(children) = namespace_tool
            .get("tools")
            .or_else(|| namespace_tool.get("children"))
            .and_then(|v| v.as_array())
        else {
            return;
        };

        for child in children {
            if child.get("type").and_then(|v| v.as_str()) == Some("function") {
                self.add_function_tool(child, Some(namespace));
            }
        }
    }

    fn add_response_tool(&mut self, tool: &Value) {
        match tool {
            Value::String(name) => {
                self.add_custom_tool(&json!({
                    "type": "custom",
                    "name": name
                }));
            }
            Value::Object(_) => match tool.get("type").and_then(|v| v.as_str()) {
                Some("function") => self.add_function_tool(tool, None),
                Some("custom") => self.add_custom_tool(tool),
                Some("tool_search") => self.add_tool_search_tool(),
                Some("namespace") => self.add_namespace_tool(tool),
                _ => {}
            },
            _ => {}
        }
    }
}

pub(crate) fn build_codex_tool_context_from_request(body: &Value) -> CodexToolContext {
    let mut context = CodexToolContext::default();

    if let Some(tools) = body.get("tools").and_then(|v| v.as_array()) {
        for tool in tools {
            context.add_response_tool(tool);
        }
    }

    if let Some(input) = body.get("input") {
        collect_tool_search_output_tools(input, &mut context);
    }

    context
}

/// Convert an OpenAI Responses request into an OpenAI Chat Completions request.
#[allow(dead_code)]
pub fn responses_to_chat_completions(body: Value) -> Result<Value, ProxyError> {
    responses_to_chat_completions_with_reasoning(body, None)
}

/// Convert an OpenAI Responses request into an OpenAI Chat Completions request,
/// using provider-declared Codex Chat reasoning capabilities when available.
pub fn responses_to_chat_completions_with_reasoning(
    body: Value,
    reasoning_config: Option<&CodexChatReasoningConfig>,
) -> Result<Value, ProxyError> {
    responses_to_chat_completions_with_reasoning_history(body, reasoning_config, false)
}

pub(crate) fn responses_to_chat_completions_with_reasoning_history(
    body: Value,
    reasoning_config: Option<&CodexChatReasoningConfig>,
    preserve_exact_history: bool,
) -> Result<Value, ProxyError> {
    let mut result = json!({});
    let tool_context = build_codex_tool_context_from_request(&body);

    if let Some(model) = body.get("model") {
        result["model"] = model.clone();
    }

    let mut messages = Vec::new();
    if let Some(instructions) = body.get("instructions") {
        let instructions = instruction_text(instructions);
        if !instructions.is_empty() {
            messages.push(json!({
                "role": "system",
                "content": instructions
            }));
        }
    }

    if let Some(input) = body.get("input") {
        append_responses_input_as_chat_messages(
            input,
            &mut messages,
            &tool_context,
            preserve_exact_history,
        )?;
    }
    let model = body.get("model").and_then(Value::as_str).unwrap_or("");
    let messages = if requires_single_leading_system_message(model) {
        collapse_system_messages_to_head(messages)
    } else {
        messages
    };
    result["messages"] = json!(messages);

    if let Some(max_tokens) = body.get("max_output_tokens") {
        if super::transform::is_openai_o_series(model) {
            result["max_completion_tokens"] = max_tokens.clone();
        } else {
            result["max_tokens"] = max_tokens.clone();
        }
    }
    if let Some(max_tokens) = body.get("max_tokens") {
        result["max_tokens"] = max_tokens.clone();
    }
    if let Some(max_tokens) = body.get("max_completion_tokens") {
        result["max_completion_tokens"] = max_tokens.clone();
    }

    for key in ["temperature", "top_p", "stream"] {
        if let Some(value) = body.get(key) {
            result[key] = value.clone();
        }
    }

    apply_reasoning_options(&mut result, &body, model, reasoning_config);

    let tools = tool_context.chat_tools();
    if !tools.is_empty() {
        result["tools"] = json!(tools);
    }

    if let Some(tool_choice) = body.get("tool_choice") {
        result["tool_choice"] = responses_tool_choice_to_chat(tool_choice, &tool_context);
    }

    for key in EXTRA_CHAT_PASSTHROUGH_FIELDS {
        if let Some(value) = body.get(*key) {
            result[*key] = value.clone();
        }
    }

    // Strict OpenAI-compatible upstreams (vLLM, enterprise gateways) reject
    // requests that carry tool_choice or parallel_tool_calls without a non-empty
    // tools array. Drop both fields when tools ended up absent or empty after
    // conversion to avoid 503/400 from such providers.
    let has_tools = result
        .get("tools")
        .is_some_and(|v| v.as_array().is_some_and(|a| !a.is_empty()));
    if !has_tools {
        if let Some(obj) = result.as_object_mut() {
            obj.remove("tool_choice");
            obj.remove("parallel_tool_calls");
        }
    }
    // OpenAI-compatible upstreams omit usage from streaming SSE by default.
    // We must explicitly request include_usage so the final usage chunk arrives.
    // Codex CLI uses the Responses protocol and does not send stream_options,
    // so without this injection third-party streaming requests such as
    // kimi/MiniMax lose all token, cost, and cache-hit accounting.
    // Share the same helper as the Claude -> openai_chat path to keep both
    // client directions aligned.
    super::transform::inject_openai_stream_include_usage(&mut result);

    Ok(result)
}

fn apply_reasoning_options(
    result: &mut Value,
    body: &Value,
    model: &str,
    config: Option<&CodexChatReasoningConfig>,
) {
    let Some(config) = config else {
        if super::transform::supports_reasoning_effort(model) {
            if let Some(effort) = body.pointer("/reasoning/effort") {
                result["reasoning_effort"] = effort.clone();
            }
        }
        return;
    };

    let supports_effort = config.supports_effort.unwrap_or(false);
    let supports_thinking = config.supports_thinking.unwrap_or(false) || supports_effort;
    let Some(reasoning_enabled) = reasoning_requested(body) else {
        return;
    };

    if supports_thinking {
        match config
            .thinking_param
            .as_deref()
            .unwrap_or("thinking")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "thinking" => {
                result["thinking"] = json!({
                    "type": if reasoning_enabled { "enabled" } else { "disabled" }
                });
            }
            "enable_thinking" => {
                result["enable_thinking"] = json!(reasoning_enabled);
            }
            "reasoning_split" => {
                result["reasoning_split"] = json!(reasoning_enabled);
            }
            _ => {}
        }
    }

    // Compute effort_param before the early return because the explicit-disable
    // reasoning.effort branch needs it.
    let effort_param = config
        .effort_param
        .as_deref()
        .unwrap_or("reasoning_effort")
        .trim()
        .to_ascii_lowercase();

    if !reasoning_enabled {
        // OpenRouter's native reasoning.effort supports explicit "none", which
        // means reasoning is fully disabled.
        // When upstream sends effort=none/off/disabled or reasoning=null,
        // reasoning_enabled is false. Returning immediately would drop that
        // disable intent, while some OpenRouter models enable reasoning by
        // default and cannot be turned off without the field. Forward
        // {"reasoning":{"effort":"none"}} exactly for this shape.
        // Top-level reasoning_effort providers do not allow none, so they still
        // use the thinking-disable path above without an effort field.
        // If reasoning is absent entirely, reasoning_requested already returned
        // None, so only explicit disable signals reach this branch.
        if effort_param == "reasoning.effort" {
            result["reasoning"] = json!({ "effort": "none" });
        }
        return;
    }

    if !supports_effort {
        return;
    }

    let Some(effort) = body.pointer("/reasoning/effort").and_then(|v| v.as_str()) else {
        return;
    };
    let Some(mapped) = map_reasoning_effort(effort, config.effort_value_mode.as_deref()) else {
        return;
    };

    match effort_param.as_str() {
        // OpenAI-style top-level field used by providers such as DeepSeek and
        // OpenAI o-series.
        "reasoning_effort" => {
            result["reasoning_effort"] = json!(mapped);
        }
        // OpenRouter's native normalized object: reasoning.effort is translated
        // by OpenRouter into the correct reasoning parameter for each backing
        // model family (OpenAI/Grok/Gemini/Anthropic), with broader coverage
        // than the top-level OpenAI alias.
        // This conversion builds from an empty object and does not retain the
        // original reasoning object, so reasoning and reasoning_effort cannot
        // coexist and trigger a 400 (see openclaw#24119).
        "reasoning.effort" => {
            result["reasoning"] = json!({ "effort": mapped });
        }
        _ => {}
    }
}

fn reasoning_requested(body: &Value) -> Option<bool> {
    if let Some(effort) = body.pointer("/reasoning/effort").and_then(|v| v.as_str()) {
        return Some(!matches!(
            effort.trim().to_ascii_lowercase().as_str(),
            "none" | "off" | "disabled"
        ));
    }

    body.get("reasoning").map(|value| !value.is_null())
}

fn map_reasoning_effort(effort: &str, mode: Option<&str>) -> Option<&'static str> {
    let effort = effort.trim().to_ascii_lowercase();
    if matches!(effort.as_str(), "none" | "off" | "disabled") {
        return None;
    }

    match mode.unwrap_or("passthrough") {
        "deepseek" => match effort.as_str() {
            "max" | "xhigh" => Some("max"),
            _ => Some("high"),
        },
        "low_high" => match effort.as_str() {
            "minimal" | "low" => Some("low"),
            _ => Some("high"),
        },
        // OpenRouter allows xhigh|high|medium|low|minimal and does not support
        // max. max is a Codex or provider-specific extension, so sending it to
        // OpenRouter causes `400 reasoning_effort: Invalid option`
        // (see openclaw#77350). Clamp it to xhigh, pass through other valid
        // values, and drop unknown values to avoid upstream rejection.
        "openrouter" => match effort.as_str() {
            "max" | "xhigh" => Some("xhigh"),
            "high" => Some("high"),
            "medium" => Some("medium"),
            "low" => Some("low"),
            "minimal" => Some("minimal"),
            _ => None,
        },
        _ => match effort.as_str() {
            "minimal" => Some("minimal"),
            "low" => Some("low"),
            "medium" => Some("medium"),
            "high" => Some("high"),
            "xhigh" => Some("xhigh"),
            "max" => Some("max"),
            _ => None,
        },
    }
}

fn requires_single_leading_system_message(model: &str) -> bool {
    model.to_ascii_lowercase().contains("minimax")
}

/// MiniMax only allows `role=system` on the first message entry, otherwise it
/// returns `invalid params, chat content has invalid message role: system (2013)`.
/// Merge all system messages to the front so mid-history system entries such as
/// Codex `developer` instructions do not trigger this constraint.
/// Runs only for MiniMax because mid-history developer/system placement is time-sensitive.
fn collapse_system_messages_to_head(messages: Vec<Value>) -> Vec<Value> {
    let mut system_chunks: Vec<String> = Vec::new();
    let mut rest: Vec<Value> = Vec::with_capacity(messages.len());

    for msg in messages {
        if msg.get("role").and_then(|v| v.as_str()) == Some("system") {
            if let Some(text) = msg.get("content").and_then(|v| v.as_str()) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    system_chunks.push(text.to_string());
                }
                continue;
            }
        }
        rest.push(msg);
    }

    let mut out: Vec<Value> = Vec::with_capacity(rest.len() + 1);
    if !system_chunks.is_empty() {
        out.push(json!({
            "role": "system",
            "content": system_chunks.join("\n\n")
        }));
    }
    out.extend(rest);
    out
}

fn instruction_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(|v| v.as_str())
                    .or_else(|| part.as_str())
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
        other => other.as_str().unwrap_or_default().to_string(),
    }
}

fn append_responses_input_as_chat_messages(
    input: &Value,
    messages: &mut Vec<Value>,
    tool_context: &CodexToolContext,
    preserve_exact_history: bool,
) -> Result<(), ProxyError> {
    let mut pending_tool_calls = Vec::new();
    let mut pending_reasoning: Option<String> = None;
    let mut last_assistant_index: Option<usize> = None;

    match input {
        Value::String(text) => {
            messages.push(json!({
                "role": "user",
                "content": text
            }));
        }
        Value::Array(items) => {
            for item in items {
                append_responses_item_as_chat_message(
                    item,
                    messages,
                    &mut pending_tool_calls,
                    &mut pending_reasoning,
                    &mut last_assistant_index,
                    tool_context,
                    preserve_exact_history,
                )?;
            }
        }
        Value::Object(_) => {
            append_responses_item_as_chat_message(
                input,
                messages,
                &mut pending_tool_calls,
                &mut pending_reasoning,
                &mut last_assistant_index,
                tool_context,
                preserve_exact_history,
            )?;
        }
        _ => {}
    }

    flush_pending_tool_calls(
        messages,
        &mut pending_tool_calls,
        &mut pending_reasoning,
        &mut last_assistant_index,
    );
    // Codex records mid-turn narration as a standalone assistant message right
    // before the tool calls it announced. Left as-is, the chat history shows
    // a bare content-only assistant turn, which teaches chat models that
    // narration alone ends a turn and causes agent loops to stall. Reattach
    // the narration to the assistant message that carries those tool calls so
    // the history matches what actually happened. Standalone assistant
    // messages (final answers, narration before a user turn) are untouched.
    merge_narration_into_tool_call_messages(messages);
    // Codex emits duplicate or delayed tool outputs whose call sits in an
    // earlier assistant block. Strict chat gateways (Kimi K3) resolve tool
    // names positionally from the immediately preceding assistant block and
    // reject such results with HTTP 400. Make every result self-describing.
    attach_tool_names_to_tool_result_messages(messages);
    if preserve_exact_history {
        if let Some(reasoning) = pending_reasoning
            .take()
            .filter(|reasoning| !reasoning.trim().is_empty())
        {
            messages.push(json!({
                "role": "assistant",
                "content": null,
                "reasoning_content": reasoning
            }));
        }
    } else {
        messages.iter_mut().for_each(|message| {
            if let Some(Value::String(reasoning)) = message.get_mut("reasoning_content") {
                *reasoning = reasoning.trim().to_owned();
            }
        });
        backfill_tool_call_reasoning_placeholders(messages);
    }
    Ok(())
}

fn append_responses_item_as_chat_message(
    item: &Value,
    messages: &mut Vec<Value>,
    pending_tool_calls: &mut Vec<Value>,
    pending_reasoning: &mut Option<String>,
    last_assistant_index: &mut Option<usize>,
    tool_context: &CodexToolContext,
    preserve_exact_history: bool,
) -> Result<(), ProxyError> {
    let item_type = item.get("type").and_then(|v| v.as_str());
    match item_type {
        Some("function_call") => {
            append_unique_pending_reasoning(
                pending_reasoning,
                reasoning_history_fragment(
                    responses_item_reasoning_text(item),
                    preserve_exact_history,
                ),
            );
            pending_tool_calls.push(responses_function_call_to_chat_tool_call(
                item,
                tool_context,
            ));
        }
        Some("custom_tool_call") => {
            append_unique_pending_reasoning(
                pending_reasoning,
                reasoning_history_fragment(
                    responses_item_reasoning_text(item),
                    preserve_exact_history,
                ),
            );
            pending_tool_calls.push(responses_custom_tool_call_to_chat_tool_call(item));
        }
        Some("tool_search_call") => {
            append_unique_pending_reasoning(
                pending_reasoning,
                reasoning_history_fragment(
                    responses_item_reasoning_text(item),
                    preserve_exact_history,
                ),
            );
            pending_tool_calls.push(responses_tool_search_call_to_chat_tool_call(item));
        }
        Some("function_call_output") => {
            flush_pending_tool_calls(
                messages,
                pending_tool_calls,
                pending_reasoning,
                last_assistant_index,
            );
            let call_id = item.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
            let output = match item.get("output") {
                Some(Value::String(s)) => canonicalize_json_string_if_parseable(s),
                Some(v) => canonical_json_string(v),
                None => String::new(),
            };
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": output
            }));
        }
        Some("custom_tool_call_output") | Some("tool_search_output") => {
            flush_pending_tool_calls(
                messages,
                pending_tool_calls,
                pending_reasoning,
                last_assistant_index,
            );
            let call_id = item.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
            let output = canonical_json_string(item);
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call_id,
                "content": output
            }));
        }
        Some("reasoning") => {
            let reasoning = reasoning_history_fragment(
                responses_reasoning_item_text(item),
                preserve_exact_history,
            );
            let attached_to_previous = pending_tool_calls.is_empty()
                && attach_reasoning_to_last_assistant(messages, *last_assistant_index, &reasoning);
            if !attached_to_previous {
                append_pending_reasoning(pending_reasoning, reasoning);
            }
        }
        Some("input_text" | "input_image" | "input_file" | "input_audio") => {
            flush_pending_tool_calls(
                messages,
                pending_tool_calls,
                pending_reasoning,
                last_assistant_index,
            );
            let role = item
                .get("role")
                .and_then(|v| v.as_str())
                .map(responses_role_to_chat_role)
                .unwrap_or("user");
            let message = json!({
                "role": role,
                "content": responses_content_to_chat_content(
                    &Value::Array(vec![item.clone()]),
                    false,
                )
            });
            if role == "assistant" {
                let mut message = message;
                attach_pending_reasoning_to_assistant(&mut message, pending_reasoning);
                update_last_assistant_index(messages, &message, last_assistant_index);
                messages.push(message);
                return Ok(());
            } else if pending_reasoning.is_some() {
                pending_reasoning.take();
            }
            update_last_assistant_index(messages, &message, last_assistant_index);
            messages.push(message);
        }
        Some("message") | None => {
            flush_pending_tool_calls(
                messages,
                pending_tool_calls,
                pending_reasoning,
                last_assistant_index,
            );
            if item.get("role").is_some() || item.get("content").is_some() {
                let message = responses_message_item_to_chat_message(
                    item,
                    pending_reasoning,
                    preserve_exact_history,
                );
                update_last_assistant_index(messages, &message, last_assistant_index);
                messages.push(message);
            }
        }
        _ => {
            flush_pending_tool_calls(
                messages,
                pending_tool_calls,
                pending_reasoning,
                last_assistant_index,
            );
            if item.get("role").is_some() || item.get("content").is_some() {
                let message = responses_message_item_to_chat_message(
                    item,
                    pending_reasoning,
                    preserve_exact_history,
                );
                update_last_assistant_index(messages, &message, last_assistant_index);
                messages.push(message);
            }
        }
    }

    Ok(())
}

/// Stamp the chat tool name onto every tool result message whose matching
/// assistant tool_call is present anywhere in the converted history. The
/// first assistant block wins when a call id repeats, mirroring how upstream
/// gateways resolve positional matches. Unresolvable results keep their
/// previously accepted shape instead of being rewritten.
fn attach_tool_names_to_tool_result_messages(messages: &mut [Value]) {
    let mut names_by_call_id: HashMap<String, String> = HashMap::new();
    for message in messages.iter() {
        let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) else {
            continue;
        };
        for tool_call in tool_calls {
            if let (Some(id), Some(name)) = (
                tool_call.get("id").and_then(Value::as_str),
                tool_call.pointer("/function/name").and_then(Value::as_str),
            ) {
                names_by_call_id
                    .entry(id.to_string())
                    .or_insert_with(|| name.to_string());
            }
        }
    }
    if names_by_call_id.is_empty() {
        return;
    }
    for message in messages.iter_mut() {
        if message.get("role").and_then(Value::as_str) != Some("tool") {
            continue;
        }
        if message.get("name").is_some() {
            continue;
        }
        if let Some(name) = message
            .get("tool_call_id")
            .and_then(Value::as_str)
            .and_then(|call_id| names_by_call_id.get(call_id))
        {
            message["name"] = json!(name);
        }
    }
}

/// Fold an assistant narration message directly followed by an assistant
/// tool_calls message into that tool_calls message. Both describe the same
/// model turn split by Codex history, so the merged form preserves every
/// session-visible word while matching the canonical chat shape.
fn merge_narration_into_tool_call_messages(messages: &mut Vec<Value>) {
    let mut index = 0;
    while index + 1 < messages.len() {
        let narration_text = {
            let message = &messages[index];
            let is_narration = message.get("role").and_then(Value::as_str) == Some("assistant")
                && message.get("tool_calls").is_none();
            if !is_narration {
                index += 1;
                continue;
            }
            match message.get("content") {
                Some(Value::String(text)) if !text.trim().is_empty() => Some(text.clone()),
                _ => None,
            }
        };
        let Some(narration_text) = narration_text else {
            index += 1;
            continue;
        };
        let next_has_tool_calls = {
            let next = &messages[index + 1];
            next.get("role").and_then(Value::as_str) == Some("assistant")
                && next
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .is_some_and(|calls| !calls.is_empty())
        };
        if !next_has_tool_calls {
            index += 1;
            continue;
        }
        if let Some(obj) = messages[index + 1].as_object_mut() {
            obj.insert("content".to_string(), Value::String(narration_text));
        }
        messages.remove(index);
        index += 1;
    }
}

fn flush_pending_tool_calls(
    messages: &mut Vec<Value>,
    pending_tool_calls: &mut Vec<Value>,
    pending_reasoning: &mut Option<String>,
    last_assistant_index: &mut Option<usize>,
) {
    if pending_tool_calls.is_empty() {
        return;
    }

    let mut message = json!({
        "role": "assistant",
        "content": null,
        "tool_calls": std::mem::take(pending_tool_calls)
    });
    attach_pending_reasoning_to_assistant(&mut message, pending_reasoning);
    *last_assistant_index = Some(messages.len());
    messages.push(message);
}

fn responses_message_item_to_chat_message(
    item: &Value,
    pending_reasoning: &mut Option<String>,
    preserve_exact_history: bool,
) -> Value {
    let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("user");
    let chat_role = responses_role_to_chat_role(role);
    let content = item
        .get("content")
        .map(|value| {
            responses_content_to_chat_content(
                value,
                item.get("type").and_then(Value::as_str) == Some("agent_message"),
            )
        })
        .unwrap_or(Value::Null);

    let mut message = json!({
        "role": chat_role,
        "content": content
    });

    if chat_role == "assistant" {
        append_pending_reasoning(
            pending_reasoning,
            reasoning_history_fragment(
                responses_message_reasoning_text(item),
                preserve_exact_history,
            ),
        );
        attach_pending_reasoning_to_assistant(&mut message, pending_reasoning);
    } else if pending_reasoning.is_some() {
        pending_reasoning.take();
    }

    message
}

fn reasoning_history_fragment(
    reasoning: Option<String>,
    preserve_exact_history: bool,
) -> Option<String> {
    reasoning.map(|reasoning| {
        if preserve_exact_history {
            reasoning
        } else {
            reasoning.trim().to_owned()
        }
    })
}

fn responses_role_to_chat_role(role: &str) -> &'static str {
    match role {
        "system" | "developer" => "system",
        "assistant" => "assistant",
        "tool" => "tool",
        "user" | "latest_reminder" => "user",
        _ => "user",
    }
}

fn update_last_assistant_index(
    messages: &[Value],
    message: &Value,
    last_assistant_index: &mut Option<usize>,
) {
    match message.get("role").and_then(|v| v.as_str()) {
        Some("assistant") => {
            *last_assistant_index = Some(messages.len());
        }
        Some("tool") => {}
        _ => {
            *last_assistant_index = None;
        }
    }
}

fn append_pending_reasoning(pending_reasoning: &mut Option<String>, reasoning: Option<String>) {
    let Some(reasoning) = reasoning.filter(|text| !text.trim().is_empty()) else {
        return;
    };

    match pending_reasoning {
        Some(existing) if !existing.is_empty() => {
            existing.push_str("\n\n");
            existing.push_str(&reasoning);
        }
        _ => {
            *pending_reasoning = Some(reasoning);
        }
    }
}

fn append_unique_pending_reasoning(
    pending_reasoning: &mut Option<String>,
    reasoning: Option<String>,
) {
    if reasoning
        .as_ref()
        .is_some_and(|reasoning| pending_reasoning.as_ref() == Some(reasoning))
    {
        return;
    }
    append_pending_reasoning(pending_reasoning, reasoning);
}

fn attach_pending_reasoning_to_assistant(
    message: &mut Value,
    pending_reasoning: &mut Option<String>,
) {
    let Some(reasoning) = pending_reasoning.take() else {
        return;
    };
    if let Some(obj) = message.as_object_mut() {
        append_reasoning_content(obj, &reasoning);
    }
}

/// After all input items are processed, backfill placeholders for assistant
/// tool-call messages that still lack `reasoning_content`.
/// This must run as the final pipeline fallback because real reasoning may be
/// attached later from a trailing `reasoning` item via
/// `attach_reasoning_to_last_assistant`. Injecting placeholders too early would
/// let `append_reasoning_content` append onto them and contaminate real
/// reasoning text.
fn backfill_tool_call_reasoning_placeholders(messages: &mut [Value]) {
    for message in messages.iter_mut() {
        let is_assistant_tool_call = message.get("role").and_then(|value| value.as_str())
            == Some("assistant")
            && message
                .get("tool_calls")
                .and_then(|value| value.as_array())
                .is_some_and(|calls| !calls.is_empty());
        if is_assistant_tool_call {
            ensure_tool_call_reasoning_content(message);
        }
    }
}

/// Thinking models such as kimi/Moonshot and DeepSeek require every assistant
/// message with `tool_calls` to include non-empty `reasoning_content`.
/// When cross-turn history restoration misses, for example after proxy restart,
/// ambiguous call_id recovery, or an upstream turn with no reasoning output,
/// inject a placeholder here to avoid
/// `reasoning_content is missing in assistant tool call message`.
/// Keep this symmetric with the placeholder behavior in
/// `transform::anthropic_to_openai_with_reasoning_content`.
fn ensure_tool_call_reasoning_content(message: &mut Value) {
    let Some(obj) = message.as_object_mut() else {
        return;
    };
    let has_reasoning = obj
        .get("reasoning_content")
        .and_then(|value| value.as_str())
        .is_some_and(|text| !text.trim().is_empty());
    if !has_reasoning {
        obj.insert(
            "reasoning_content".to_string(),
            Value::String("tool call".to_string()),
        );
    }
}

fn attach_reasoning_to_last_assistant(
    messages: &mut [Value],
    last_assistant_index: Option<usize>,
    reasoning: &Option<String>,
) -> bool {
    let Some(reasoning) = reasoning.as_deref().filter(|text| !text.trim().is_empty()) else {
        return true;
    };
    let Some(index) = last_assistant_index else {
        return false;
    };
    let Some(message) = messages.get_mut(index) else {
        return false;
    };
    if message.get("role").and_then(|v| v.as_str()) != Some("assistant") {
        return false;
    }

    if let Some(obj) = message.as_object_mut() {
        append_reasoning_content(obj, reasoning);
        return true;
    }

    false
}

fn responses_message_reasoning_text(item: &Value) -> Option<String> {
    responses_item_reasoning_text(item)
}

fn responses_item_reasoning_text(item: &Value) -> Option<String> {
    extract_reasoning_field_text(item)
}

fn responses_reasoning_item_text(item: &Value) -> Option<String> {
    extract_reasoning_summary_text(item)
}

fn responses_content_to_chat_content(content: &Value, allow_agent_payload: bool) -> Value {
    if content.is_null() || content.is_string() {
        return content.clone();
    }

    let Some(parts) = content.as_array() else {
        return content.clone();
    };

    let mut chat_parts: Vec<Value> = Vec::new();
    let mut has_non_text_part = false;

    for part in parts {
        let part_type = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match part_type {
            "input_text" | "output_text" | "text" => {
                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        chat_parts.push(json!({
                            "type": "text",
                            "text": text
                        }));
                    }
                }
            }
            "encrypted_content" if allow_agent_payload => {
                if let Some(text) = part.get("encrypted_content").and_then(Value::as_str) {
                    if !text.is_empty() {
                        chat_parts.push(json!({ "type": "text", "text": text }));
                    }
                }
            }
            "refusal" => {
                if let Some(text) = part.get("refusal").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        chat_parts.push(json!({
                            "type": "text",
                            "text": text
                        }));
                    }
                }
            }
            "input_image" => {
                if let Some(image_url) = part.get("image_url") {
                    let image_url = if image_url.is_object() {
                        image_url.clone()
                    } else {
                        json!({ "url": image_url.as_str().unwrap_or_default() })
                    };
                    chat_parts.push(json!({
                        "type": "image_url",
                        "image_url": image_url
                    }));
                    has_non_text_part = true;
                }
            }
            "input_file" => {
                if let Some(file) = responses_input_file_to_chat_file(part) {
                    chat_parts.push(json!({
                        "type": "file",
                        "file": file
                    }));
                    has_non_text_part = true;
                }
            }
            "input_audio" => {
                if let Some(input_audio) = part.get("input_audio") {
                    chat_parts.push(json!({
                        "type": "input_audio",
                        "input_audio": input_audio.clone()
                    }));
                    has_non_text_part = true;
                }
            }
            _ => {}
        }
    }

    if !has_non_text_part {
        return Value::String(
            chat_parts
                .iter()
                .filter_map(|part| part.get("text").and_then(|v| v.as_str()))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }

    Value::Array(chat_parts)
}

fn responses_input_file_to_chat_file(part: &Value) -> Option<Value> {
    let mut file = serde_json::Map::new();
    let has_supported_file_ref = part.get("file_id").is_some() || part.get("file_data").is_some();
    if !has_supported_file_ref {
        return None;
    }

    for key in ["file_id", "file_data", "filename"] {
        if let Some(value) = part.get(key) {
            file.insert(key.to_string(), value.clone());
        }
    }
    Some(Value::Object(file))
}

fn collect_tool_search_output_tools(value: &Value, context: &mut CodexToolContext) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_tool_search_output_tools(item, context);
            }
        }
        Value::Object(obj) => {
            if obj.get("type").and_then(|v| v.as_str()) == Some("tool_search_output") {
                if let Some(tools) = obj.get("tools").and_then(|v| v.as_array()) {
                    for tool in tools {
                        context.add_response_tool(tool);
                    }
                }
            }
            for value in obj.values() {
                collect_tool_search_output_tools(value, context);
            }
        }
        _ => {}
    }
}

fn flatten_namespace_tool_name(namespace: &str, name: &str) -> String {
    let full_name = format!("{namespace}__{name}");
    if full_name.len() <= CHAT_TOOL_NAME_MAX_LEN {
        return full_name;
    }

    let hash = short_sha256_hex(full_name.as_bytes());
    let suffix = format!("__{hash}");
    let prefix_len = CHAT_TOOL_NAME_MAX_LEN.saturating_sub(suffix.len());
    let mut prefix = String::new();
    for ch in full_name.chars() {
        if prefix.len() + ch.len_utf8() > prefix_len {
            break;
        }
        prefix.push(ch);
    }
    format!("{prefix}{suffix}")
}

fn responses_tool_name(tool: &Value) -> Option<String> {
    tool.get("function")
        .and_then(|function| function.get("name"))
        .or_else(|| tool.get("name"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn responses_custom_tool_description(tool: &Value) -> String {
    let mut description = String::new();
    description.push_str(CUSTOM_TOOL_PRESERVED_METADATA_HEADING);
    description.push_str("\n```json\n");
    description.push_str(&serialize_tool_definition_for_description(tool));
    description.push_str("\n```");
    description
}

fn serialize_tool_definition_for_description(tool: &Value) -> String {
    // Keep the embedded definition compact to reduce tool-description token
    // overhead for chat-only upstreams, while remaining stable across map
    // storage order.
    canonical_json_string(tool)
}

fn responses_function_tool_to_chat_tool(tool: &Value, chat_name: &str) -> Option<Value> {
    if tool.get("type").and_then(|v| v.as_str()) != Some("function") {
        return None;
    }

    if let Some(function) = tool.get("function") {
        let mut chat_tool = json!({
            "type": "function",
            "function": function.clone()
        });
        if let Some(obj) = chat_tool
            .get_mut("function")
            .and_then(|value| value.as_object_mut())
        {
            obj.insert("name".to_string(), json!(chat_name));
            if let Some(strict) = tool.get("strict").cloned() {
                obj.entry("strict".to_string()).or_insert(strict);
            }
        }
        return Some(chat_tool);
    }

    let mut function = json!({
        "name": chat_name,
        "description": tool.get("description").cloned().unwrap_or(Value::Null),
        "parameters": tool.get("parameters").cloned().unwrap_or_else(|| json!({}))
    });
    if let Some(strict) = tool.get("strict") {
        function["strict"] = strict.clone();
    }

    Some(json!({
        "type": "function",
        "function": function
    }))
}

fn responses_function_call_to_chat_tool_call(
    item: &Value,
    tool_context: &CodexToolContext,
) -> Value {
    let call_id = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let namespace = item.get("namespace").and_then(|v| v.as_str());
    let chat_name = tool_context.chat_name_for_response_function(name, namespace);
    let arguments = canonicalize_tool_arguments(item.get("arguments"));

    json!({
        "id": call_id,
        "type": "function",
        "function": {
            "name": chat_name,
            "arguments": arguments
        }
    })
}

fn responses_custom_tool_call_to_chat_tool_call(item: &Value) -> Value {
    let call_id = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let input = item.get("input").cloned().unwrap_or_else(|| json!(""));

    json!({
        "id": call_id,
        "type": "function",
        "function": {
            "name": name,
            "arguments": canonical_json_string(&json!({ CUSTOM_TOOL_INPUT_FIELD: input }))
        }
    })
}

fn responses_tool_search_call_to_chat_tool_call(item: &Value) -> Value {
    let call_id = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let arguments = item
        .get("arguments")
        .map(canonical_json_string)
        .unwrap_or_else(|| "{}".to_string());

    json!({
        "id": call_id,
        "type": "function",
        "function": {
            "name": TOOL_SEARCH_PROXY_NAME,
            "arguments": arguments
        }
    })
}

fn responses_tool_choice_to_chat(tool_choice: &Value, tool_context: &CodexToolContext) -> Value {
    match tool_choice {
        Value::Object(obj) if obj.get("type").and_then(|v| v.as_str()) == Some("function") => {
            let name = obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let namespace = obj.get("namespace").and_then(|v| v.as_str());
            let chat_name = tool_context.chat_name_for_response_function(name, namespace);
            json!({
                "type": "function",
                "function": {
                    "name": chat_name
                }
            })
        }
        Value::Object(obj) if obj.get("type").and_then(|v| v.as_str()) == Some("tool_search") => {
            json!({
                "type": "function",
                "function": {
                    "name": TOOL_SEARCH_PROXY_NAME
                }
            })
        }
        Value::Object(obj) if obj.get("type").and_then(|v| v.as_str()) == Some("custom") => {
            let name = obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
            json!({
                "type": "function",
                "function": {
                    "name": name
                }
            })
        }
        _ => tool_choice.clone(),
    }
}

/// Convert a non-streaming Chat Completions response into a Responses response.
#[allow(dead_code)]
pub fn chat_completion_to_response(body: Value) -> Result<Value, ProxyError> {
    chat_completion_to_response_with_context(body, &CodexToolContext::default())
}

/// Convert a non-streaming Chat Completions response into a Responses response,
/// restoring Codex-specific tool names using the original Responses request.
pub(crate) fn chat_completion_to_response_with_context(
    body: Value,
    tool_context: &CodexToolContext,
) -> Result<Value, ProxyError> {
    let choices = body
        .get("choices")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ProxyError::TransformError("No choices in chat response".to_string()))?;
    let choice = choices
        .first()
        .ok_or_else(|| ProxyError::TransformError("Empty choices in chat response".to_string()))?;
    let message = choice
        .get("message")
        .ok_or_else(|| ProxyError::TransformError("No message in chat choice".to_string()))?;

    let response_id = response_id_from_chat_id(body.get("id").and_then(|v| v.as_str()));
    let model = body.get("model").and_then(|v| v.as_str()).unwrap_or("");
    let created_at = body.get("created").and_then(|v| v.as_u64()).unwrap_or(0);
    let finish_reason = choice.get("finish_reason").and_then(|v| v.as_str());

    let reasoning = chat_reasoning_text(message);
    let mut output = Vec::new();
    if let Some(message_item) = chat_message_to_response_output_item(message, &response_id) {
        output.push(message_item);
    }
    output.extend(chat_tool_calls_to_response_output_items(
        message,
        reasoning.as_deref(),
        tool_context,
    ));

    let mut response = json!({
        "id": response_id,
        "object": "response",
        "created_at": created_at,
        "status": response_status_from_finish_reason(finish_reason),
        "model": model,
        "output": output,
        "usage": chat_usage_to_responses_usage(body.get("usage"))
    });

    if finish_reason == Some("length") {
        response["incomplete_details"] = json!({ "reason": "max_output_tokens" });
    }

    Ok(response)
}

fn chat_reasoning_text(message: &Value) -> Option<String> {
    if let Some(reasoning) = extract_reasoning_field_text(message) {
        return Some(reasoning);
    }

    if let Some(content) = message.get("content").and_then(|v| v.as_str()) {
        if let Some((reasoning, _answer)) = split_leading_think_block(content) {
            if !reasoning.is_empty() {
                return Some(reasoning);
            }
        }
    }

    None
}

fn chat_message_to_response_output_item(message: &Value, response_id: &str) -> Option<Value> {
    let mut content = Vec::new();

    if let Some(text) = message.get("content").and_then(|v| v.as_str()) {
        let text = split_leading_think_block(text)
            .map(|(_reasoning, answer)| answer)
            .unwrap_or_else(|| text.to_string());
        if !text.is_empty() {
            content.push(json!({
                "type": "output_text",
                "text": text,
                "annotations": []
            }));
        }
    } else if let Some(parts) = message.get("content").and_then(|v| v.as_array()) {
        for part in parts {
            let part_type = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match part_type {
                "text" | "output_text" => {
                    if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                        if !text.is_empty() {
                            content.push(json!({
                                "type": "output_text",
                                "text": text,
                                "annotations": []
                            }));
                        }
                    }
                }
                "refusal" => {
                    if let Some(text) = part.get("refusal").and_then(|v| v.as_str()) {
                        if !text.is_empty() {
                            content.push(json!({
                                "type": "refusal",
                                "refusal": text
                            }));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    if let Some(refusal) = message.get("refusal").and_then(|v| v.as_str()) {
        if !refusal.is_empty() {
            content.push(json!({
                "type": "refusal",
                "refusal": refusal
            }));
        }
    }

    if content.is_empty() {
        return None;
    }

    Some(json!({
        "id": response_message_item_id(response_id),
        "type": "message",
        "status": "completed",
        "role": "assistant",
        "content": content
    }))
}

fn chat_tool_calls_to_response_output_items(
    message: &Value,
    reasoning: Option<&str>,
    tool_context: &CodexToolContext,
) -> Vec<Value> {
    let mut output = Vec::new();

    if let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
        for (index, tool_call) in tool_calls.iter().enumerate() {
            // Skip tool calls with missing function names (defensive: some models
            // may generate tool calls without providing a valid name)
            let function = tool_call.get("function").unwrap_or(&Value::Null);
            let name = function.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if name.is_empty() {
                log::warn!("[Codex] Skipping tool call with missing name");
                continue;
            }
            output.push(chat_tool_call_to_response_item(
                tool_call,
                index,
                reasoning,
                tool_context,
            ));
        }
    } else if let Some(function_call) = message.get("function_call") {
        if let Some(item) =
            chat_legacy_function_call_to_response_item(function_call, reasoning, tool_context)
        {
            output.push(item);
        }
    }

    output
}

fn chat_tool_call_to_response_item(
    tool_call: &Value,
    index: usize,
    reasoning: Option<&str>,
    tool_context: &CodexToolContext,
) -> Value {
    let call_id = tool_call
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("call_{index}"));
    let function = tool_call.get("function").unwrap_or(&Value::Null);
    let name = function.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let arguments = canonicalize_tool_arguments(function.get("arguments"));

    let item_id = response_tool_call_item_id_from_chat_name(&call_id, name, tool_context);
    response_tool_call_item_from_chat_name(
        &item_id,
        "completed",
        &call_id,
        name,
        &arguments,
        reasoning,
        tool_context,
    )
}

fn chat_legacy_function_call_to_response_item(
    function_call: &Value,
    reasoning: Option<&str>,
    tool_context: &CodexToolContext,
) -> Option<Value> {
    let call_id = function_call
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .unwrap_or("call_0");
    let name = function_call
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // Skip legacy function calls with missing names (defensive: some models
    // may generate function_call without providing a valid name)
    if name.is_empty() {
        log::warn!("[Codex] Skipping legacy function_call with missing name");
        return None;
    }

    let arguments = canonicalize_tool_arguments(function_call.get("arguments"));

    let item_id = response_tool_call_item_id_from_chat_name(call_id, name, tool_context);
    Some(response_tool_call_item_from_chat_name(
        &item_id,
        "completed",
        call_id,
        name,
        &arguments,
        reasoning,
        tool_context,
    ))
}

pub(crate) fn response_tool_call_item_id_from_chat_name(
    call_id: &str,
    chat_name: &str,
    tool_context: &CodexToolContext,
) -> String {
    let suffix = responses_item_id_suffix(call_id);
    if tool_context.is_custom_tool_chat_name(chat_name) {
        format!("ctc_{suffix}")
    } else {
        format!("fc_{suffix}")
    }
}

fn responses_item_id_suffix(value: &str) -> String {
    let mut suffix = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
            suffix.push(character);
        } else {
            use std::fmt::Write as _;
            write!(&mut suffix, "_{:x}", character as u32)
                .expect("writing to a String cannot fail");
        }
    }
    suffix
}

pub(crate) fn response_tool_call_item_from_chat_name(
    item_id: &str,
    status: &str,
    call_id: &str,
    chat_name: &str,
    arguments: &str,
    reasoning: Option<&str>,
    tool_context: &CodexToolContext,
) -> Value {
    match tool_context.lookup_chat_name(chat_name) {
        Some(spec) if spec.kind == CodexToolKind::ToolSearch => {
            response_tool_search_call_item(call_id, status, arguments, reasoning)
        }
        Some(spec) if spec.kind == CodexToolKind::Custom => response_custom_tool_call_item(
            item_id, status, call_id, &spec.name, arguments, reasoning,
        ),
        Some(spec) => response_function_call_item_with_namespace(
            item_id,
            status,
            call_id,
            &spec.name,
            spec.namespace.as_deref(),
            arguments,
            reasoning,
        ),
        None => {
            response_function_call_item(item_id, status, call_id, chat_name, arguments, reasoning)
        }
    }
}

fn response_tool_search_call_item(
    call_id: &str,
    status: &str,
    arguments: &str,
    reasoning: Option<&str>,
) -> Value {
    let parsed_arguments = parse_tool_arguments_object(arguments);
    let mut item = json!({
        "type": "tool_search_call",
        "call_id": call_id,
        "status": status,
        "execution": "client",
        "arguments": parsed_arguments
    });
    super::codex_chat_common::attach_optional_reasoning_content_field(&mut item, reasoning);
    item
}

fn response_custom_tool_call_item(
    item_id: &str,
    status: &str,
    call_id: &str,
    name: &str,
    arguments: &str,
    reasoning: Option<&str>,
) -> Value {
    let input = custom_tool_input_from_chat_arguments(arguments);
    let mut item = json!({
        "id": item_id,
        "type": "custom_tool_call",
        "status": status,
        "call_id": call_id,
        "name": name,
        "input": input
    });
    super::codex_chat_common::attach_optional_reasoning_content_field(&mut item, reasoning);
    item
}

fn parse_tool_arguments_object(arguments: &str) -> Value {
    if arguments.trim().is_empty() {
        return json!({});
    }
    serde_json::from_str::<Value>(arguments)
        .ok()
        .filter(|value| value.is_object())
        .unwrap_or_else(|| json!({ "query": arguments }))
}

pub(crate) fn custom_tool_input_from_chat_arguments(arguments: &str) -> String {
    if arguments.trim().is_empty() {
        return String::new();
    }
    match serde_json::from_str::<Value>(arguments) {
        Ok(Value::Object(obj)) => obj
            .get(CUSTOM_TOOL_INPUT_FIELD)
            .and_then(|value| value.as_str())
            .unwrap_or(arguments)
            .to_string(),
        _ => arguments.to_string(),
    }
}

pub(crate) fn chat_usage_to_responses_usage(usage: Option<&Value>) -> Value {
    let Some(usage) = usage.filter(|value| value.is_object() && !value.is_null()) else {
        return json!({
            "input_tokens": 0,
            "output_tokens": 0,
            "total_tokens": 0,
            "output_tokens_details": { "reasoning_tokens": 0 }
        });
    };

    let input_tokens = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let total_tokens = usage
        .get("total_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(input_tokens + output_tokens);

    let mut result = json!({
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "total_tokens": total_tokens
    });

    if let Some(cached) = usage
        .pointer("/prompt_tokens_details/cached_tokens")
        .or_else(|| usage.pointer("/input_tokens_details/cached_tokens"))
        .and_then(|v| v.as_u64())
    {
        result["input_tokens_details"] = json!({ "cached_tokens": cached });
    }

    if let Some(details) = usage
        .get("completion_tokens_details")
        .filter(|v| v.is_object())
    {
        let mut details = details.clone();
        if details.get("reasoning_tokens").is_none() {
            details["reasoning_tokens"] = json!(0);
        }
        result["output_tokens_details"] = details;
    } else {
        result["output_tokens_details"] = json!({ "reasoning_tokens": 0 });
    }

    if let Some(cache_read) = usage.get("cache_read_input_tokens") {
        result["cache_read_input_tokens"] = cache_read.clone();
    }
    if let Some(cache_creation) = usage.get("cache_creation_input_tokens") {
        result["cache_creation_input_tokens"] = cache_creation.clone();
    }

    result
}

pub(crate) fn response_id_from_chat_id(id: Option<&str>) -> String {
    let id = id.unwrap_or("nexus");
    if id.starts_with("resp_") {
        id.to_string()
    } else {
        format!("resp_{id}")
    }
}

pub(crate) fn response_message_item_id(response_id: &str) -> String {
    let suffix = response_id.strip_prefix("resp_").unwrap_or(response_id);
    format!("msg_{suffix}")
}

pub(crate) fn response_status_from_finish_reason(finish_reason: Option<&str>) -> &'static str {
    match finish_reason {
        Some("length") => "incomplete",
        _ => "completed",
    }
}

/// Normalize upstream Chat Completions error bodies into an OpenAI Responses
/// API-style error object.
///
/// Supports three input shapes:
/// 1. Standard OpenAI form `{"error": {"message": "...", "type": "...", "code": ...}}`
/// 2. Non-standard forms such as MiniMax
///    `{"base_resp": {"status_code": 2013, "status_msg": "..."}}`
/// 3. Minimal top-level `message`, `detail`, or bare string errors
///
/// Always returns `{"error": {"message", "type", "code", "param"}}` to
/// match the OpenAI Responses API error shape expected by the Codex client.
pub fn chat_error_to_response_error(body: Option<&Value>) -> Value {
    let Some(value) = body else {
        return json!({
            "error": {
                "message": "Upstream returned an empty error response",
                "type": "upstream_error",
                "code": serde_json::Value::Null,
                "param": serde_json::Value::Null,
            }
        });
    };

    if let Some(text) = value.as_str() {
        return json!({
            "error": {
                "message": text,
                "type": "upstream_error",
                "code": serde_json::Value::Null,
                "param": serde_json::Value::Null,
            }
        });
    }

    let source = value.get("error").unwrap_or(value);

    let message = source
        .get("message")
        .or_else(|| source.get("detail"))
        .or_else(|| source.get("status_msg"))
        .or_else(|| source.pointer("/base_resp/status_msg"))
        .and_then(|v| v.as_str())
        .map(ToString::to_string)
        .or_else(|| source.as_str().map(ToString::to_string))
        .unwrap_or_else(|| {
            // If no text field is extractable, serialize the full JSON so the
            // user can inspect the upstream payload.
            serde_json::to_string(source).unwrap_or_else(|_| "Upstream error".to_string())
        });

    let error_type = source
        .get("type")
        .and_then(|v| v.as_str())
        .map(ToString::to_string)
        .unwrap_or_else(|| "upstream_error".to_string());

    let code = source
        .get("code")
        .cloned()
        .or_else(|| source.pointer("/base_resp/status_code").cloned())
        .unwrap_or(serde_json::Value::Null);

    let param = source
        .get("param")
        .cloned()
        .unwrap_or(serde_json::Value::Null);

    json!({
        "error": {
            "message": message,
            "type": error_type,
            "code": code,
            "param": param,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_tool_requests_preserve_harness_instructions_and_user_text() {
        let result = responses_to_chat_completions(json!({
            "model": "kimi-k3",
            "instructions": "Keep working until the task is complete.",
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": "Inspect the repository."}]
            }],
            "tools": [{
                "type": "custom",
                "name": "exec",
                "description": "Run a command"
            }]
        }))
        .unwrap();

        let messages = result["messages"].as_array().unwrap();
        assert_eq!(
            messages[0]["content"],
            "Keep working until the task is complete."
        );
        assert_eq!(messages[1]["content"], "Inspect the repository.");
        let serialized = serde_json::to_string(&result).unwrap();
        assert!(!serialized.contains("Agent-loop control"));
        assert!(!serialized.contains("MUST NOT send narration"));
        assert!(!serialized.contains("executed nothing and ended the turn"));
    }

    #[test]
    fn text_only_history_is_not_rewritten_with_loop_control() {
        let input = || {
            json!({
                "model": "kimi-k3",
                "instructions": "Keep working until the task is complete.",
                "input": [{
                    "type": "function_call", "call_id": "call_1",
                    "name": "exec", "arguments": "{}"
                }, {
                    "type": "function_call_output", "call_id": "call_1", "output": "ok"
                }, {
                    "type": "message", "role": "assistant",
                    "content": [{"type": "output_text", "text": "The earlier check found a gap."}]
                }, {
                    "type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "Will the other sessions hit it too?"}]
                }],
                "tools": [{
                    "type": "function", "name": "exec",
                    "description": "Run a command",
                    "parameters": {"type": "object", "properties": {}}
                }]
            })
        };

        for preserve in [true, false] {
            let result =
                responses_to_chat_completions_with_reasoning_history(input(), None, preserve)
                    .unwrap();
            let last = result["messages"].as_array().unwrap().last().unwrap();

            assert_eq!(last["role"], "user");
            assert_eq!(last["content"], "Will the other sessions hit it too?");
            let serialized = serde_json::to_string(&result).unwrap();
            assert!(!serialized.contains("MUST now reply with a tool call"));
            assert!(!serialized.contains("Agent-loop control"));
        }
    }

    #[test]
    fn nexus_history_is_exact_while_baseline_history_still_backfills() {
        let tool = "  tool call\n\n[redacted thinking]\nReal.  \n";
        let input = json!({
            "model": "glm-5.2",
            "input": [{
                "type": "function_call", "call_id": "call_1",
                "name": "lookup", "arguments": "{}", "reasoning_content": tool
            }, {
                "type": "function_call_output", "call_id": "call_1", "output": "ok"
            }]
        });
        let config = CodexChatReasoningConfig {
            supports_thinking: Some(true),
            thinking_param: Some("thinking".into()),
            output_format: Some("reasoning_content".into()),
            ..Default::default()
        };
        let preserved = responses_to_chat_completions_with_reasoning_history(
            input.clone(),
            Some(&config),
            true,
        )
        .unwrap();
        assert_eq!(preserved["messages"][0]["reasoning_content"], tool);
        let filled = responses_to_chat_completions(input).unwrap();
        assert_eq!(filled["messages"][0]["reasoning_content"], tool.trim());

        let missing = responses_to_chat_completions(json!({
            "input": [{
                "type": "function_call", "call_id": "call_1",
                "name": "lookup", "arguments": "{}"
            }]
        }))
        .unwrap();
        assert_eq!(missing["messages"][0]["reasoning_content"], "tool call");
    }

    #[test]
    fn responses_request_with_stream_injects_include_usage() {
        let input = json!({
            "model": "kimi-k2.6",
            "input": [{"role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
            "stream": true
        });

        let result = responses_to_chat_completions(input).unwrap();

        assert_eq!(result["stream"], true);
        assert_eq!(result["stream_options"]["include_usage"], true);
    }

    #[test]
    fn responses_request_without_stream_omits_stream_options() {
        let input = json!({
            "model": "kimi-k2.6",
            "input": [{"role": "user", "content": [{"type": "input_text", "text": "hi"}]}]
        });

        let result = responses_to_chat_completions(input).unwrap();

        assert!(result.get("stream_options").is_none());
    }

    #[test]
    fn responses_request_merges_include_usage_into_existing_stream_options() {
        let input = json!({
            "model": "kimi-k2.6",
            "input": [{"role": "user", "content": [{"type": "input_text", "text": "hi"}]}],
            "stream": true,
            "stream_options": {"continuous_usage_stats": true}
        });

        let result = responses_to_chat_completions(input).unwrap();

        // Inject include_usage while preserving the client's existing
        // stream_options fields.
        assert_eq!(result["stream_options"]["include_usage"], true);
        assert_eq!(result["stream_options"]["continuous_usage_stats"], true);
    }

    #[test]
    fn responses_request_maps_input_file_content_parts() {
        let input = json!({
            "model": "gpt-5.4",
            "input": [{
                "role": "user",
                "content": [
                    {"type": "input_text", "text": "Summarize this."},
                    {
                        "type": "input_file",
                        "file_id": "file_123",
                        "file_url": "https://example.com/spec.pdf",
                        "filename": "spec.pdf"
                    },
                    {
                        "type": "input_audio",
                        "input_audio": {
                            "data": "UklGRg==",
                            "format": "wav"
                        }
                    }
                ]
            }]
        });

        let result = responses_to_chat_completions(input).unwrap();
        let content = result["messages"][0]["content"].as_array().unwrap();

        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "file");
        assert_eq!(content[1]["file"]["file_id"], "file_123");
        assert!(content[1]["file"].get("file_url").is_none());
        assert_eq!(content[1]["file"]["filename"], "spec.pdf");
        assert_eq!(content[2]["type"], "input_audio");
        assert_eq!(content[2]["input_audio"]["format"], "wav");
    }

    #[test]
    fn responses_request_does_not_emit_chat_file_for_url_only_input_file() {
        let input = json!({
            "model": "gpt-5.4",
            "input": [{
                "role": "user",
                "content": [
                    {"type": "input_text", "text": "Summarize this URL file."},
                    {
                        "type": "input_file",
                        "file_url": "https://example.com/spec.pdf"
                    }
                ]
            }]
        });

        let result = responses_to_chat_completions(input).unwrap();

        assert_eq!(result["messages"][0]["content"], "Summarize this URL file.");
    }

    #[test]
    fn responses_request_maps_top_level_input_file_item() {
        let input = json!({
            "model": "gpt-5.4",
            "input": [
                {
                    "type": "input_file",
                    "file_id": "file_top",
                    "filename": "top.pdf"
                }
            ]
        });

        let result = responses_to_chat_completions(input).unwrap();
        let content = result["messages"][0]["content"].as_array().unwrap();

        assert_eq!(result["messages"][0]["role"], "user");
        assert_eq!(content[0]["type"], "file");
        assert_eq!(content[0]["file"]["file_id"], "file_top");
        assert_eq!(content[0]["file"]["filename"], "top.pdf");
    }

    #[test]
    fn mid_turn_assistant_narration_merges_into_following_tool_call_message() {
        let input = json!({
            "model": "kimi-k3",
            "input": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "I will check the config now."}]
                },
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "exec",
                    "arguments": "{}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "ok"
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "All done."}]
                }
            ],
            "tools": [{
                "type": "function",
                "name": "exec",
                "parameters": {"type": "object"}
            }]
        });

        let result = responses_to_chat_completions(input).unwrap();
        let messages = result["messages"].as_array().unwrap();
        let roles: Vec<&str> = messages
            .iter()
            .filter_map(|message| message.get("role").and_then(|role| role.as_str()))
            .collect();

        assert_eq!(
            roles,
            vec!["assistant", "tool", "assistant"],
            "narration must merge into the adjacent assistant tool_call message, got {roles:?}"
        );
        assert_eq!(messages[0]["content"], "I will check the config now.");
        assert_eq!(messages[0]["tool_calls"][0]["id"], "call_1");
        assert_eq!(messages[2]["content"], "All done.");
        assert!(messages[2].get("tool_calls").is_none());
    }

    #[test]
    fn top_level_user_content_part_clears_pending_reasoning() {
        let input = json!({
            "model": "gpt-5.4",
            "input": [
                {
                    "type": "reasoning",
                    "summary": [{"text": "stale reasoning"}]
                },
                {
                    "type": "input_text",
                    "text": "Please run the tool."
                },
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "lookup",
                    "arguments": "{}"
                }
            ],
            "tools": [{
                "type": "function",
                "name": "lookup",
                "parameters": {"type": "object"}
            }]
        });

        let result = responses_to_chat_completions(input).unwrap();
        let messages = result["messages"].as_array().unwrap();

        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "Please run the tool.");
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["reasoning_content"], "tool call");
    }

    #[test]
    fn responses_request_to_chat_maps_messages_tools_and_limits() {
        let input = json!({
            "model": "gpt-5.4",
            "instructions": "You are concise.",
            "input": [
                {
                    "role": "user",
                    "content": [
                        {"type": "input_text", "text": "Weather?"},
                        {"type": "input_image", "image_url": "data:image/png;base64,abc"},
                        {"type": "input_text", "text": "Use Celsius."}
                    ]
                },
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "get_weather",
                    "arguments": "{\"city\":\"Tokyo\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "Sunny"
                }
            ],
            "tools": [{
                "type": "function",
                "name": "get_weather",
                "description": "Get weather",
                "parameters": {"type": "object"},
                "strict": true
            }],
            "tool_choice": {"type": "function", "name": "get_weather"},
            "max_output_tokens": 100,
            "reasoning": {"effort": "high"},
            "stream": true
        });

        let result = responses_to_chat_completions(input).unwrap();

        assert_eq!(result["model"], "gpt-5.4");
        assert_eq!(result["messages"][0]["role"], "system");
        assert_eq!(result["messages"][1]["role"], "user");
        assert_eq!(result["messages"][1]["content"][0]["type"], "text");
        assert_eq!(result["messages"][1]["content"][1]["type"], "image_url");
        assert_eq!(result["messages"][1]["content"][2]["type"], "text");
        assert_eq!(result["messages"][1]["content"][2]["text"], "Use Celsius.");
        assert_eq!(result["messages"][2]["tool_calls"][0]["id"], "call_1");
        assert_eq!(result["messages"][3]["role"], "tool");
        assert_eq!(result["tools"][0]["function"]["name"], "get_weather");
        assert_eq!(result["tools"][0]["function"]["strict"], true);
        assert_eq!(result["tool_choice"]["function"]["name"], "get_weather");
        assert_eq!(result["max_tokens"], 100);
        assert_eq!(result["reasoning_effort"], "high");
    }

    #[test]
    fn responses_request_to_chat_exposes_tool_search_and_loaded_namespace_tools() {
        let input = json!({
            "model": "gpt-5.4",
            "tools": [{"type": "tool_search"}],
            "input": [
                {
                    "type": "tool_search_call",
                    "call_id": "call_tool_search_1",
                    "status": "completed",
                    "execution": "client",
                    "arguments": {"query": "Gmail search emails", "limit": 5}
                },
                {
                    "type": "tool_search_output",
                    "call_id": "call_tool_search_1",
                    "status": "completed",
                    "execution": "client",
                    "tools": [{
                        "type": "namespace",
                        "name": "mcp__codex_apps__gmail",
                        "description": "Find and reference emails from your inbox.",
                        "tools": [{
                            "type": "function",
                            "name": "_search_emails",
                            "description": "Search Gmail for emails matching a query.",
                            "strict": false,
                            "parameters": {
                                "type": "object",
                                "properties": {
                                    "query": {"type": "string"},
                                    "max_results": {"type": "integer"}
                                },
                                "required": ["query"]
                            }
                        }]
                    }]
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": "Search unread inbox mail."
                }
            ]
        });

        let result = responses_to_chat_completions(input).unwrap();
        let tools = result["tools"].as_array().unwrap();
        let tool_names = tools
            .iter()
            .filter_map(|tool| tool.pointer("/function/name").and_then(|v| v.as_str()))
            .collect::<Vec<_>>();

        assert!(tool_names.contains(&"tool_search"));
        assert!(tool_names.contains(&"mcp__codex_apps__gmail___search_emails"));
        assert_eq!(
            result["messages"][0]["tool_calls"][0]["function"]["name"],
            "tool_search"
        );
        assert_eq!(result["messages"][1]["role"], "tool");
        assert_eq!(result["messages"][1]["tool_call_id"], "call_tool_search_1");
        assert!(result["messages"][1]["content"]
            .as_str()
            .unwrap()
            .contains("mcp__codex_apps__gmail"));
    }

    #[test]
    fn responses_request_to_chat_maps_custom_tool_and_choice() {
        let input = json!({
            "model": "gpt-5.4",
            "tools": [{
                "type": "custom",
                "name": "apply_patch",
                "description": "Apply a patch to files."
            }],
            "tool_choice": {"type": "custom", "name": "apply_patch"},
            "input": [{
                "type": "custom_tool_call",
                "id": "ctc_1",
                "call_id": "call_patch",
                "name": "apply_patch",
                "input": "*** Begin Patch\n*** End Patch"
            }]
        });

        let result = responses_to_chat_completions(input).unwrap();

        assert_eq!(result["tools"][0]["function"]["name"], "apply_patch");
        assert_eq!(
            result["tools"][0]["function"]["parameters"]["required"][0],
            "input"
        );
        assert_eq!(result["tool_choice"]["function"]["name"], "apply_patch");
        assert_eq!(
            result["messages"][0]["tool_calls"][0]["function"]["arguments"],
            r#"{"input":"*** Begin Patch\n*** End Patch"}"#
        );
    }

    #[test]
    fn responses_request_to_chat_preserves_custom_tool_metadata_in_description() {
        let input = json!({
            "model": "gpt-5.4",
            "tools": [{
                "type": "custom",
                "name": "apply_patch",
                "description": "Use the `apply_patch` tool to edit files. This is a FREEFORM tool, so do not wrap the patch in JSON.",
                "format": {
                    "type": "grammar",
                    "syntax": "lark",
                    "definition": "start: begin_patch hunk+ end_patch"
                }
            }]
        });

        let result = responses_to_chat_completions(input).unwrap();
        let description = result["tools"][0]["function"]["description"]
            .as_str()
            .unwrap();

        assert!(description.starts_with("Original tool definition:"));
        assert!(!description.contains("Original Codex tool definition"));
        assert!(description.contains("\"type\":\"custom\""));
        assert!(description.contains("\"format\":"));
        assert!(description.contains("\"syntax\":\"lark\""));
    }

    #[test]
    fn responses_request_to_chat_uses_provider_reasoning_effort_for_deepseek_model() {
        let input = json!({
            "model": "deepseek-v4-pro",
            "input": "hello",
            "reasoning": {"effort": "xhigh"}
        });
        let config = CodexChatReasoningConfig {
            supports_thinking: Some(true),
            supports_effort: Some(true),
            thinking_param: Some("thinking".to_string()),
            effort_param: Some("reasoning_effort".to_string()),
            effort_value_mode: Some("deepseek".to_string()),
            output_format: Some("reasoning_content".to_string()),
        };

        let result = responses_to_chat_completions_with_reasoning(input, Some(&config)).unwrap();

        assert_eq!(result["thinking"]["type"], "enabled");
        assert_eq!(result["reasoning_effort"], "max");
    }

    #[test]
    fn responses_request_to_chat_maps_openrouter_to_native_reasoning_object() {
        // OpenRouter shape: native reasoning:{effort} plus "openrouter" value
        // mapping, aligned with infer_aggregator_platform_config.
        let config = CodexChatReasoningConfig {
            supports_thinking: Some(false),
            supports_effort: Some(true),
            thinking_param: Some("none".to_string()),
            effort_param: Some("reasoning.effort".to_string()),
            effort_value_mode: Some("openrouter".to_string()),
            output_format: Some("auto".to_string()),
        };

        // max is not valid in the OpenRouter enum (see openclaw#77350), so it
        // must be clamped to xhigh and written to the native reasoning object
        // rather than the top-level reasoning_effort alias.
        let input = json!({
            "model": "deepseek/deepseek-chat-v3.1",
            "input": "hello",
            "reasoning": {"effort": "max"}
        });
        let result = responses_to_chat_completions_with_reasoning(input, Some(&config)).unwrap();

        assert_eq!(result["reasoning"]["effort"], "xhigh");
        assert!(result.get("reasoning_effort").is_none());
        // With thinking_param=none, do not emit any thinking field even if
        // supports_effort makes supports_thinking true. OpenRouter does not
        // accept thinking:{type}.
        assert!(result.get("thinking").is_none());

        // Pass through valid effort values unchanged.
        let input_high = json!({
            "model": "deepseek/deepseek-chat-v3.1",
            "input": "hello",
            "reasoning": {"effort": "high"}
        });
        let result_high =
            responses_to_chat_completions_with_reasoning(input_high, Some(&config)).unwrap();
        assert_eq!(result_high["reasoning"]["effort"], "high");
        assert!(result_high.get("reasoning_effort").is_none());
    }

    #[test]
    fn responses_request_to_chat_passes_explicit_none_through_for_openrouter() {
        // OpenRouter's native reasoning object supports explicit disable, so
        // effort=none must be forwarded as {"reasoning":{"effort":"none"}}
        // instead of being dropped. Otherwise models that default reasoning on
        // cannot be disabled, changing both behavior and cost.
        let config = CodexChatReasoningConfig {
            supports_thinking: Some(false),
            supports_effort: Some(true),
            thinking_param: Some("none".to_string()),
            effort_param: Some("reasoning.effort".to_string()),
            effort_value_mode: Some("openrouter".to_string()),
            output_format: Some("auto".to_string()),
        };

        let input = json!({
            "model": "openai/gpt-5",
            "input": "hello",
            "reasoning": {"effort": "none"}
        });
        let result = responses_to_chat_completions_with_reasoning(input, Some(&config)).unwrap();

        assert_eq!(result["reasoning"]["effort"], "none");
        // none is not a valid OpenAI top-level reasoning_effort enum value, so
        // do not emit the alias or any thinking field.
        assert!(result.get("reasoning_effort").is_none());
        assert!(result.get("thinking").is_none());
    }

    #[test]
    fn responses_request_to_chat_drops_explicit_none_for_top_level_effort_provider() {
        // Contrast case: top-level reasoning_effort providers in the
        // DeepSeek/OpenAI style do not allow none, so explicit none must not be
        // forwarded as reasoning_effort:"none" because upstream rejects it.
        // It should only drive the thinking-disable path. This locks the
        // boundary that none passthrough is limited to reasoning.effort.
        let config = CodexChatReasoningConfig {
            supports_thinking: Some(true),
            supports_effort: Some(true),
            thinking_param: Some("thinking".to_string()),
            effort_param: Some("reasoning_effort".to_string()),
            effort_value_mode: Some("deepseek".to_string()),
            output_format: Some("reasoning_content".to_string()),
        };

        let input = json!({
            "model": "deepseek-v4-pro",
            "input": "hello",
            "reasoning": {"effort": "none"}
        });
        let result = responses_to_chat_completions_with_reasoning(input, Some(&config)).unwrap();

        // Still emit the thinking disable signal, but do not emit
        // reasoning_effort or the native reasoning object.
        assert_eq!(result["thinking"]["type"], "disabled");
        assert!(result.get("reasoning_effort").is_none());
        assert!(result.get("reasoning").is_none());
    }

    #[test]
    fn responses_request_to_chat_maps_thinking_only_provider_without_effort() {
        let input = json!({
            "model": "kimi-k2.6",
            "input": "hello",
            "reasoning": {"effort": "high"}
        });
        let config = CodexChatReasoningConfig {
            supports_thinking: Some(true),
            supports_effort: Some(false),
            thinking_param: Some("thinking".to_string()),
            effort_param: Some("none".to_string()),
            effort_value_mode: None,
            output_format: Some("reasoning_content".to_string()),
        };

        let result = responses_to_chat_completions_with_reasoning(input, Some(&config)).unwrap();

        assert_eq!(result["thinking"]["type"], "enabled");
        assert!(result.get("reasoning_effort").is_none());
    }

    #[test]
    fn responses_request_to_chat_maps_enable_thinking_provider() {
        let input = json!({
            "model": "qwen3-max",
            "input": "hello",
            "reasoning": {"effort": "medium"}
        });
        let config = CodexChatReasoningConfig {
            supports_thinking: Some(true),
            supports_effort: Some(false),
            thinking_param: Some("enable_thinking".to_string()),
            effort_param: Some("none".to_string()),
            effort_value_mode: None,
            output_format: Some("reasoning_content".to_string()),
        };

        let result = responses_to_chat_completions_with_reasoning(input, Some(&config)).unwrap();

        assert_eq!(result["enable_thinking"], true);
        assert!(result.get("reasoning_effort").is_none());
    }

    #[test]
    fn chat_response_to_responses_omits_unreplayable_reasoning_details() {
        let input = json!({
            "id": "chatcmpl_minimax",
            "object": "chat.completion",
            "created": 123,
            "model": "MiniMax-M2.7",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "reasoning_details": [
                        {"type": "reasoning_text", "text": "Need to inspect the code."}
                    ],
                    "content": "Done"
                },
                "finish_reason": "stop"
            }]
        });

        let result = chat_completion_to_response(input).unwrap();

        assert_eq!(result["output"].as_array().unwrap().len(), 1);
        assert_eq!(result["output"][0]["type"], "message");
        assert_eq!(result["output"][0]["id"], "msg_chatcmpl_minimax");
        assert_eq!(result["output"][0]["content"][0]["text"], "Done");
    }

    #[test]
    fn responses_request_to_chat_normalizes_codex_internal_roles() {
        let input = json!({
            "model": "gpt-5.4",
            "input": [
                {
                    "type": "message",
                    "role": "developer",
                    "content": [
                        {"type": "input_text", "text": "Follow project instructions."}
                    ]
                },
                {
                    "type": "message",
                    "role": "latest_reminder",
                    "content": "Keep the reply brief."
                },
                {
                    "type": "message",
                    "role": "unknown_codex_role",
                    "content": "Fallback content."
                }
            ]
        });

        let result = responses_to_chat_completions(input).unwrap();
        let messages = result["messages"].as_array().unwrap();

        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "Follow project instructions.");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "Keep the reply brief.");
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"], "Fallback content.");
    }

    #[test]
    fn responses_request_to_chat_forwards_only_agent_message_payload() {
        let payload = format!("BEGIN\n{}\nEND", "0123456789abcdef".repeat(8192));
        let input = json!({
            "model": "GLM-5.2-FP8",
            "input": [
                {"type": "reasoning", "encrypted_content": "opaque-reasoning", "summary": []},
                {
                    "type": "message",
                    "role": "user",
                    "content": [
                        {"type": "input_text", "text": "Visible history"},
                        {"type": "encrypted_content", "encrypted_content": "opaque-message"}
                    ]
                },
                {
                    "type": "agent_message",
                    "role": "user",
                    "content": [
                        {"type": "input_text", "text": "Message Type: NEW_TASK"},
                        {"type": "encrypted_content", "encrypted_content": payload}
                    ]
                }
            ]
        });

        let result = responses_to_chat_completions(input).unwrap();
        assert_eq!(
            result["messages"],
            json!([
                {"role": "user", "content": "Visible history"},
                {"role": "user", "content": format!("Message Type: NEW_TASK\n{payload}")}
            ])
        );
        let serialized = serde_json::to_string(&result).unwrap();
        assert!(!serialized.contains("opaque-reasoning"));
        assert!(!serialized.contains("opaque-message"));
        assert!(!serialized.contains("encrypted_content"));
    }

    #[test]
    fn responses_request_to_chat_merges_mid_stream_system_into_head() {
        let input = json!({
            "model": "MiniMax-M2.7",
            "instructions": "You are Codex.",
            "input": [
                {"type": "message", "role": "developer", "content": [{"type": "input_text", "text": "Permissions block"}]},
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "AGENTS.md"}]},
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "xin chào"}]},
                {"type": "message", "role": "developer", "content": [{"type": "input_text", "text": "Collaboration Mode: Default"}]},
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "xin chào"}]},
                {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "xin chào"}]}
            ]
        });

        let result = responses_to_chat_completions(input).unwrap();
        let messages = result["messages"].as_array().unwrap();

        for (idx, msg) in messages.iter().enumerate() {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap();
            if idx == 0 {
                assert_eq!(role, "system", "first message must be system");
            } else {
                assert_ne!(
                    role, "system",
                    "no system role allowed past index 0 (got at {idx})"
                );
            }
        }

        let head_content = messages[0]["content"].as_str().unwrap();
        assert!(head_content.contains("You are Codex."));
        assert!(head_content.contains("Permissions block"));
        assert!(head_content.contains("Collaboration Mode: Default"));
    }

    #[test]
    fn responses_request_to_chat_preserves_mid_stream_system_order_for_kimi() {
        let input = json!({
            "model": "kimi-k3",
            "instructions": "You are Codex.",
            "input": [
                {"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "First task state."}]},
                {"type": "message", "role": "developer",
                    "content": [{"type": "input_text", "text": "New collaboration mode."}]},
                {"type": "message", "role": "user",
                    "content": [{"type": "input_text", "text": "Continue."}]}
            ]
        });

        let result = responses_to_chat_completions(input).unwrap();
        let messages = result["messages"].as_array().unwrap();

        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "You are Codex.");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "First task state.");
        assert_eq!(messages[2]["role"], "system");
        assert_eq!(messages[2]["content"], "New collaboration mode.");
        assert_eq!(messages[3]["role"], "user");
        assert_eq!(messages[3]["content"], "Continue.");
    }

    #[test]
    fn collapse_system_messages_preserves_non_system_order() {
        let input = vec![
            json!({"role": "system", "content": "S1"}),
            json!({"role": "user", "content": "U1"}),
            json!({"role": "assistant", "content": "A1"}),
            json!({"role": "system", "content": "S2"}),
            json!({"role": "user", "content": "U2"}),
        ];
        let out = collapse_system_messages_to_head(input);

        assert_eq!(out.len(), 4);
        assert_eq!(out[0]["role"], "system");
        assert_eq!(out[0]["content"], "S1\n\nS2");
        assert_eq!(out[1]["content"], "U1");
        assert_eq!(out[2]["content"], "A1");
        assert_eq!(out[3]["content"], "U2");
    }

    #[test]
    fn responses_request_to_chat_passes_reasoning_content_back_to_assistant_message() {
        let input = json!({
            "model": "gpt-5.4",
            "input": [
                {
                    "type": "reasoning",
                    "summary": [
                        {"type": "summary_text", "text": "Need to inspect the repo."}
                    ]
                },
                {
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {"type": "output_text", "text": "I will check the files."}
                    ]
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": "Continue"
                }
            ]
        });

        let result = responses_to_chat_completions(input).unwrap();
        let messages = result["messages"].as_array().unwrap();

        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["content"], "I will check the files.");
        assert_eq!(
            messages[0]["reasoning_content"],
            "Need to inspect the repo."
        );
        assert_eq!(messages[1]["role"], "user");
        assert!(messages[1].get("reasoning_content").is_none());
    }

    #[test]
    fn responses_request_to_chat_attaches_trailing_reasoning_to_previous_assistant() {
        let input = json!({
            "model": "gpt-5.4",
            "input": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": "I checked the files."
                },
                {
                    "type": "reasoning",
                    "summary": [
                        {"type": "summary_text", "text": "The answer came from README."}
                    ]
                },
                {
                    "type": "message",
                    "role": "user",
                    "content": "Continue"
                }
            ]
        });

        let result = responses_to_chat_completions(input).unwrap();
        let messages = result["messages"].as_array().unwrap();

        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["content"], "I checked the files.");
        assert_eq!(
            messages[0]["reasoning_content"],
            "The answer came from README."
        );
        assert_eq!(messages[1]["role"], "user");
        assert!(messages[1].get("reasoning_content").is_none());
    }

    #[test]
    fn responses_request_to_chat_keeps_embedded_assistant_reasoning() {
        let input = json!({
            "model": "gpt-5.4",
            "input": [
                {
                    "type": "message",
                    "role": "assistant",
                    "reasoning_content": "I need to preserve thinking history.",
                    "content": "Done."
                }
            ]
        });

        let result = responses_to_chat_completions(input).unwrap();
        let messages = result["messages"].as_array().unwrap();

        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["content"], "Done.");
        assert_eq!(
            messages[0]["reasoning_content"],
            "I need to preserve thinking history."
        );
    }

    #[test]
    fn responses_request_to_chat_attaches_reasoning_to_tool_call_message() {
        let input = json!({
            "model": "gpt-5.4",
            "input": [
                {
                    "type": "reasoning",
                    "summary": "Need to read a file."
                },
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "read_file",
                    "arguments": "{\"path\":\"README.md\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "Readme content"
                }
            ]
        });

        let result = responses_to_chat_completions(input).unwrap();
        let messages = result["messages"].as_array().unwrap();

        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["reasoning_content"], "Need to read a file.");
        assert_eq!(messages[0]["tool_calls"][0]["id"], "call_1");
        assert_eq!(messages[1]["role"], "tool");
    }

    #[test]
    fn responses_request_to_chat_recovers_reasoning_from_function_call_item() {
        let input = json!({
            "model": "gpt-5.4",
            "input": [
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "read_file",
                    "arguments": "{\"path\":\"README.md\"}",
                    "reasoning_content": "Need to read a file."
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "Readme content"
                }
            ]
        });

        let result = responses_to_chat_completions(input).unwrap();
        let messages = result["messages"].as_array().unwrap();

        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["tool_calls"][0]["id"], "call_1");
        assert_eq!(messages[0]["reasoning_content"], "Need to read a file.");
        assert_eq!(messages[1]["role"], "tool");
    }

    #[test]
    fn responses_request_to_chat_injects_placeholder_reasoning_for_bare_tool_call() {
        // When history restoration misses and an assistant tool-call message
        // has no usable reasoning, inject a placeholder or kimi/Moonshot
        // thinking models reject the entire request.
        let input = json!({
            "model": "kimi-k2-thinking",
            "input": [
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "read_file",
                    "arguments": "{\"path\":\"README.md\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "Readme content"
                }
            ]
        });

        let result = responses_to_chat_completions(input).unwrap();
        let messages = result["messages"].as_array().unwrap();

        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["tool_calls"][0]["id"], "call_1");
        assert_eq!(messages[0]["reasoning_content"], "tool call");
        assert_eq!(messages[1]["role"], "tool");
    }

    #[test]
    fn responses_request_to_chat_attaches_trailing_reasoning_to_tool_call_message() {
        let input = json!({
            "model": "gpt-5.4",
            "input": [
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "read_file",
                    "arguments": "{\"path\":\"README.md\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "Readme content"
                },
                {
                    "type": "reasoning",
                    "summary": "Need to read a file."
                }
            ]
        });

        let result = responses_to_chat_completions(input).unwrap();
        let messages = result["messages"].as_array().unwrap();

        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["tool_calls"][0]["id"], "call_1");
        assert_eq!(messages[0]["reasoning_content"], "Need to read a file.");
        assert_eq!(messages[1]["role"], "tool");
    }

    #[test]
    fn responses_request_to_chat_keeps_multiple_tool_calls_adjacent_to_outputs() {
        let input = json!({
            "model": "gpt-5.4",
            "input": [
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "read_file",
                    "arguments": "{\"path\":\"README.md\"}"
                },
                {
                    "type": "function_call",
                    "call_id": "call_2",
                    "name": "list_files",
                    "arguments": "{\"path\":\"src\"}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "Readme content"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_2",
                    "output": ["main.rs", "lib.rs"]
                },
                {
                    "role": "user",
                    "content": "Continue"
                }
            ]
        });

        let result = responses_to_chat_completions(input).unwrap();
        let messages = result["messages"].as_array().unwrap();

        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0]["role"], "assistant");
        assert_eq!(messages[0]["tool_calls"][0]["id"], "call_1");
        assert_eq!(messages[0]["tool_calls"][1]["id"], "call_2");
        assert_eq!(messages[1]["role"], "tool");
        assert_eq!(messages[1]["tool_call_id"], "call_1");
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "call_2");
        assert_eq!(messages[2]["content"], "[\"main.rs\",\"lib.rs\"]");
        assert_eq!(messages[3]["role"], "user");
    }

    #[test]
    fn tool_results_carry_tool_names_after_later_assistant_tool_blocks() {
        // Regression: Codex Desktop re-emits outputs of an earlier call after
        // a later assistant tool block. Strict chat gateways resolve tool
        // names positionally and reject such history with HTTP 400 unless each
        // tool message carries the tool name explicitly.
        let input = json!({
            "model": "kimi-k3",
            "input": [
                {
                    "type": "custom_tool_call",
                    "call_id": "exec:14",
                    "name": "exec",
                    "input": "ls"
                },
                {
                    "type": "custom_tool_call_output",
                    "call_id": "exec:14",
                    "output": [{"type": "input_text", "text": "Script running with cell ID 13"}]
                },
                {
                    "type": "function_call",
                    "call_id": "wait:15",
                    "name": "wait",
                    "arguments": "{}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "wait:15",
                    "output": "Script running with cell ID 13"
                },
                {
                    "type": "custom_tool_call_output",
                    "call_id": "exec:14",
                    "output": [{"type": "input_text", "text": "idle-poll 1"}]
                },
                {
                    "type": "custom_tool_call_output",
                    "call_id": "exec:14",
                    "output": [{"type": "input_text", "text": "idle-poll 2"}]
                },
                {
                    "role": "user",
                    "content": "Continue."
                }
            ]
        });

        let result = responses_to_chat_completions(input).unwrap();
        let messages = result["messages"].as_array().unwrap();

        let tool_results: Vec<&Value> = messages
            .iter()
            .filter(|message| message["role"] == "tool")
            .collect();
        assert_eq!(tool_results.len(), 4);
        assert!(tool_results.iter().all(|message| {
            matches!(message.get("name"), Some(Value::String(name)) if !name.is_empty())
        }));
        let named = |call_id: &str| {
            tool_results
                .iter()
                .filter(|message| message["tool_call_id"] == call_id)
                .map(|message| message["name"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(named("exec:14"), vec!["exec", "exec", "exec"]);
        assert_eq!(named("wait:15"), vec!["wait"]);
        // The dangling outputs keep their order and content; only the name is added.
        let first = tool_results[0]["content"].as_str().unwrap();
        assert!(first.contains("custom_tool_call_output"));
        assert!(first.contains("cell ID 13"));
    }

    #[test]
    fn tool_results_use_flattened_namespace_chat_names() {
        let input = json!({
            "model": "kimi-k3",
            "input": [
                {
                    "type": "function_call",
                    "call_id": "call_ns",
                    "name": "exec",
                    "namespace": "node_repl",
                    "arguments": "{}"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_ns",
                    "output": "ok"
                }
            ]
        });

        let result = responses_to_chat_completions(input).unwrap();
        let messages = result["messages"].as_array().unwrap();

        let chat_name = messages[0]["tool_calls"][0]["function"]["name"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(chat_name, "node_repl__exec");
        assert_eq!(messages[1]["role"], "tool");
        assert_eq!(messages[1]["name"], chat_name);
    }

    #[test]
    fn tool_results_without_matching_call_keep_prevailing_shape() {
        let input = json!({
            "model": "kimi-k3",
            "input": [
                {
                    "type": "function_call_output",
                    "call_id": "orphan",
                    "output": "ok"
                }
            ]
        });

        let result = responses_to_chat_completions(input).unwrap();
        let messages = result["messages"].as_array().unwrap();

        assert_eq!(messages[0]["role"], "tool");
        assert_eq!(messages[0]["tool_call_id"], "orphan");
        assert!(messages[0].get("name").is_none());
    }

    #[test]
    fn responses_request_to_chat_canonicalizes_json_string_tool_payloads() {
        let input = json!({
            "model": "gpt-5.4",
            "input": [
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "lookup",
                    "arguments": "{ \"b\": 2, \"a\": 1 }"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "{ \"z\": true, \"a\": [2, 1] }"
                }
            ]
        });

        let result = responses_to_chat_completions(input).unwrap();
        let messages = result["messages"].as_array().unwrap();

        assert_eq!(
            messages[0]["tool_calls"][0]["function"]["arguments"],
            r#"{"a":1,"b":2}"#
        );
        assert_eq!(messages[1]["content"], r#"{"a":[2,1],"z":true}"#);
    }

    #[test]
    fn responses_request_to_chat_preserves_plain_text_tool_output() {
        let input = json!({
            "model": "gpt-5.4",
            "input": [
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "read_file",
                    "arguments": "not json"
                },
                {
                    "type": "function_call_output",
                    "call_id": "call_1",
                    "output": "plain text result"
                }
            ]
        });

        let result = responses_to_chat_completions(input).unwrap();
        let messages = result["messages"].as_array().unwrap();

        assert_eq!(
            messages[0]["tool_calls"][0]["function"]["arguments"],
            "not json"
        );
        assert_eq!(messages[1]["content"], "plain text result");
    }

    #[test]
    fn chat_response_to_responses_maps_text_tool_calls_and_usage() {
        let input = json!({
            "id": "chatcmpl_1",
            "object": "chat.completion",
            "created": 123,
            "model": "gpt-5.4",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "reasoning_content": "I should check the weather before answering.",
                    "content": "Let me check.",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"city\":\"Tokyo\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15,
                "prompt_tokens_details": {"cached_tokens": 3}
            }
        });

        let result = chat_completion_to_response(input).unwrap();

        assert_eq!(result["id"], "resp_chatcmpl_1");
        assert_eq!(result["status"], "completed");
        assert_eq!(result["output"][0]["type"], "message");
        assert_eq!(result["output"][0]["id"], "msg_chatcmpl_1");
        assert_eq!(result["output"][0]["content"][0]["text"], "Let me check.");
        assert_eq!(result["output"][1]["type"], "function_call");
        assert_eq!(result["output"][1]["call_id"], "call_1");
        assert_eq!(
            result["output"][1]["reasoning_content"],
            "I should check the weather before answering."
        );
        assert_eq!(result["output"].as_array().unwrap().len(), 2);
        assert_eq!(result["usage"]["input_tokens"], 10);
        assert_eq!(result["usage"]["output_tokens"], 5);
        assert_eq!(result["usage"]["input_tokens_details"]["cached_tokens"], 3);
    }

    #[test]
    fn chat_response_to_responses_restores_loaded_namespace_tool_call() {
        let request = json!({
            "model": "gpt-5.4",
            "tools": [{"type": "tool_search"}],
            "input": [{
                "type": "tool_search_output",
                "call_id": "call_tool_search_1",
                "status": "completed",
                "execution": "client",
                "tools": [{
                    "type": "namespace",
                    "name": "mcp__codex_apps__gmail",
                    "description": "Find and reference emails from your inbox.",
                    "tools": [{
                        "type": "function",
                        "name": "_search_emails",
                        "description": "Search Gmail for emails matching a query.",
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "query": {"type": "string"},
                                "label_ids": {"type": "array", "items": {"type": "string"}},
                                "max_results": {"type": "integer"}
                            }
                        }
                    }]
                }]
            }]
        });
        let context = build_codex_tool_context_from_request(&request);
        let chat = json!({
            "id": "chatcmpl_gmail",
            "object": "chat.completion",
            "created": 123,
            "model": "gpt-5.4",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_gmail",
                        "type": "function",
                        "function": {
                            "name": "mcp__codex_apps__gmail___search_emails",
                            "arguments": "{\"query\":\"-in:spam -in:trash\",\"label_ids\":[\"UNREAD\"],\"max_results\":5}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });

        let result = chat_completion_to_response_with_context(chat, &context).unwrap();

        assert_eq!(result["output"][0]["type"], "function_call");
        assert_eq!(result["output"][0]["call_id"], "call_gmail");
        assert_eq!(result["output"][0]["namespace"], "mcp__codex_apps__gmail");
        assert_eq!(result["output"][0]["name"], "_search_emails");
        assert_eq!(
            result["output"][0]["arguments"],
            r#"{"label_ids":["UNREAD"],"max_results":5,"query":"-in:spam -in:trash"}"#
        );
    }

    #[test]
    fn chat_response_to_responses_restores_tool_search_call() {
        let request = json!({
            "model": "gpt-5.4",
            "tools": [{"type": "tool_search"}],
            "input": "Find tools."
        });
        let context = build_codex_tool_context_from_request(&request);
        let chat = json!({
            "id": "chatcmpl_tool_search",
            "object": "chat.completion",
            "created": 123,
            "model": "gpt-5.4",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_tool_search_1",
                        "type": "function",
                        "function": {
                            "name": "tool_search",
                            "arguments": "{\"query\":\"Gmail search emails\",\"limit\":10}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });

        let result = chat_completion_to_response_with_context(chat, &context).unwrap();

        assert_eq!(result["output"][0]["type"], "tool_search_call");
        assert_eq!(result["output"][0]["call_id"], "call_tool_search_1");
        assert_eq!(result["output"][0]["execution"], "client");
        assert_eq!(
            result["output"][0]["arguments"]["query"],
            "Gmail search emails"
        );
        assert_eq!(result["output"][0]["arguments"]["limit"], 10);
    }

    #[test]
    fn chat_response_to_responses_restores_custom_tool_call() {
        let request = json!({
            "model": "gpt-5.4",
            "tools": [{"type": "custom", "name": "apply_patch"}],
            "input": "Patch it."
        });
        let context = build_codex_tool_context_from_request(&request);
        let chat = json!({
            "id": "chatcmpl_custom",
            "object": "chat.completion",
            "created": 123,
            "model": "gpt-5.4",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "exec:0",
                        "type": "function",
                        "function": {
                            "name": "apply_patch",
                            "arguments": "{\"input\":\"*** Begin Patch\\n*** End Patch\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });

        let result = chat_completion_to_response_with_context(chat, &context).unwrap();

        assert_eq!(result["output"][0]["type"], "custom_tool_call");
        assert_eq!(result["output"][0]["id"], "ctc_exec_3a0");
        assert_eq!(result["output"][0]["call_id"], "exec:0");
        assert_eq!(result["output"][0]["name"], "apply_patch");
        assert_eq!(
            result["output"][0]["input"],
            "*** Begin Patch\n*** End Patch"
        );
    }

    #[test]
    fn chat_response_to_responses_canonicalizes_json_string_tool_arguments() {
        let input = json!({
            "id": "chatcmpl_args",
            "object": "chat.completion",
            "created": 123,
            "model": "gpt-5.4",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "lookup",
                            "arguments": "{ \"b\": 2, \"a\": 1 }"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });

        let result = chat_completion_to_response(input).unwrap();

        assert_eq!(result["output"][0]["type"], "function_call");
        assert_eq!(result["output"][0]["arguments"], r#"{"a":1,"b":2}"#);
    }

    #[test]
    fn chat_response_to_responses_splits_inline_think_content() {
        let input = json!({
            "id": "chatcmpl_think",
            "object": "chat.completion",
            "created": 123,
            "model": "MiniMax-M2.7",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "<think>\nI should answer with pong.\n</think>\n\npong"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 20,
                "total_tokens": 30,
                "completion_tokens_details": {"reasoning_tokens": 18}
            }
        });

        let result = chat_completion_to_response(input).unwrap();

        assert_eq!(result["output"].as_array().unwrap().len(), 1);
        assert_eq!(result["output"][0]["type"], "message");
        assert_eq!(result["output"][0]["content"][0]["text"], "pong");
        assert_eq!(
            result["usage"]["output_tokens_details"]["reasoning_tokens"],
            18
        );
    }

    #[test]
    fn chat_response_length_maps_to_incomplete_response() {
        let input = json!({
            "id": "chatcmpl_2",
            "model": "gpt-5.4",
            "choices": [{
                "message": {"role": "assistant", "content": "partial"},
                "finish_reason": "length"
            }]
        });

        let result = chat_completion_to_response(input).unwrap();

        assert_eq!(result["status"], "incomplete");
        assert_eq!(result["incomplete_details"]["reason"], "max_output_tokens");
    }

    #[test]
    fn chat_error_to_response_error_normalizes_standard_openai_shape() {
        let input = json!({
            "error": {
                "message": "Invalid API key",
                "type": "invalid_request_error",
                "code": "invalid_api_key",
                "param": "api_key"
            }
        });

        let result = chat_error_to_response_error(Some(&input));

        assert_eq!(result["error"]["message"], "Invalid API key");
        assert_eq!(result["error"]["type"], "invalid_request_error");
        assert_eq!(result["error"]["code"], "invalid_api_key");
        assert_eq!(result["error"]["param"], "api_key");
    }

    #[test]
    fn chat_error_to_response_error_normalizes_minimax_base_resp() {
        // MiniMax places the error under base_resp, and code is numeric rather
        // than a string.
        let input = json!({
            "base_resp": {
                "status_code": 2013,
                "status_msg": "invalid params, chat content has invalid message role: system"
            }
        });

        let result = chat_error_to_response_error(Some(&input));

        assert_eq!(
            result["error"]["message"],
            "invalid params, chat content has invalid message role: system"
        );
        assert_eq!(result["error"]["code"], 2013);
        // type is not provided explicitly, so it should fall back to
        // upstream_error.
        assert_eq!(result["error"]["type"], "upstream_error");
    }

    #[test]
    fn chat_error_to_response_error_handles_plain_text_body() {
        let input = json!("Upstream timeout");

        let result = chat_error_to_response_error(Some(&input));

        assert_eq!(result["error"]["message"], "Upstream timeout");
        assert_eq!(result["error"]["type"], "upstream_error");
        assert!(result["error"]["code"].is_null());
        assert!(result["error"]["param"].is_null());
    }

    #[test]
    fn chat_error_to_response_error_handles_missing_body() {
        let result = chat_error_to_response_error(None);

        assert_eq!(
            result["error"]["message"],
            "Upstream returned an empty error response"
        );
        assert_eq!(result["error"]["type"], "upstream_error");
    }

    #[test]
    fn chat_error_to_response_error_falls_back_to_detail_field() {
        // Some gateways place the error in the top-level detail field, which is
        // common in OpenAI-compatible layers.
        let input = json!({
            "detail": "rate limit exceeded"
        });

        let result = chat_error_to_response_error(Some(&input));

        assert_eq!(result["error"]["message"], "rate limit exceeded");
        assert_eq!(result["error"]["type"], "upstream_error");
    }
    // Regression tests for tool_choice without tools guard
    // https://github.com/farion1231/cc-switch/issues/3557

    #[test]
    fn responses_request_to_chat_drops_tool_choice_when_no_tools() {
        // When tools is absent from the request, tool_choice must be dropped
        // to avoid 503/400 from strict OpenAI-compatible upstreams.
        let input = json!({
            "model": "qwen3-7-max",
            "tool_choice": "auto",
            "input": "hi"
        });

        let result = responses_to_chat_completions(input).unwrap();

        assert!(
            result.get("tool_choice").is_none(),
            "tool_choice should be dropped when tools is absent"
        );
        assert!(result.get("tools").is_none(), "tools should be absent");
        assert_eq!(result["model"], "qwen3-7-max");
    }

    #[test]
    fn responses_request_to_chat_drops_tool_choice_when_tools_empty_array() {
        // When tools is an empty array, tool_choice must be dropped.
        let input = json!({
            "model": "gpt-5.4",
            "tools": [],
            "tool_choice": "auto",
            "input": "hi"
        });

        let result = responses_to_chat_completions(input).unwrap();

        assert!(
            result.get("tool_choice").is_none(),
            "tool_choice should be dropped when tools is empty"
        );
        assert!(
            result.get("tools").is_none(),
            "tools should be absent when input tools was empty"
        );
    }

    #[test]
    fn responses_request_to_chat_drops_parallel_tool_calls_when_no_tools() {
        // parallel_tool_calls must also be dropped when tools is absent,
        // as it is part of EXTRA_CHAT_PASSTHROUGH_FIELDS.
        let input = json!({
            "model": "gpt-5.4",
            "tool_choice": "auto",
            "parallel_tool_calls": true,
            "input": "hi"
        });

        let result = responses_to_chat_completions(input).unwrap();

        assert!(
            result.get("tool_choice").is_none(),
            "tool_choice should be dropped"
        );
        assert!(
            result.get("parallel_tool_calls").is_none(),
            "parallel_tool_calls should be dropped"
        );
        assert!(result.get("tools").is_none(), "tools should be absent");
    }

    #[test]
    fn responses_request_to_chat_drops_tool_choice_when_all_tools_filtered() {
        // When all tools are filtered out (e.g., missing name), tool_choice must be dropped.
        let input = json!({
            "model": "gpt-5.4",
            "tools": [
                {"type": "function"}
            ],
            "tool_choice": "auto",
            "input": "hi"
        });

        let result = responses_to_chat_completions(input).unwrap();

        assert!(
            result.get("tool_choice").is_none(),
            "tool_choice should be dropped when all tools filtered"
        );
        assert!(
            result.get("tools").is_none(),
            "tools should be absent when all filtered"
        );
    }

    #[test]
    fn responses_request_to_chat_keeps_tool_choice_when_tools_present() {
        // When tools is present and non-empty, tool_choice must be preserved.
        let input = json!({
            "model": "gpt-5.4",
            "tools": [{
                "type": "function",
                "name": "get_weather",
                "description": "Get weather",
                "parameters": {"type": "object"}
            }],
            "tool_choice": "auto",
            "parallel_tool_calls": true,
            "input": "hi"
        });

        let result = responses_to_chat_completions(input).unwrap();

        assert!(
            result.get("tool_choice").is_some(),
            "tool_choice should be kept when tools present"
        );
        assert_eq!(result["tool_choice"], "auto");
        assert!(
            result.get("parallel_tool_calls").is_some(),
            "parallel_tool_calls should be kept"
        );
        assert_eq!(result["parallel_tool_calls"], true);
        assert!(
            result
                .get("tools")
                .is_some_and(|v| v.as_array().is_some_and(|a| !a.is_empty())),
            "tools should be present"
        );
        assert_eq!(result["tools"][0]["function"]["name"], "get_weather");
    }

    #[test]
    fn responses_request_to_chat_keeps_tool_choice_function_when_tools_present() {
        // When tools is present, function-type tool_choice must be preserved and mapped.
        let input = json!({
            "model": "gpt-5.4",
            "tools": [{
                "type": "function",
                "name": "get_weather",
                "description": "Get weather",
                "parameters": {"type": "object"}
            }],
            "tool_choice": {"type": "function", "name": "get_weather"},
            "input": "hi"
        });

        let result = responses_to_chat_completions(input).unwrap();

        assert!(
            result.get("tool_choice").is_some(),
            "tool_choice should be kept"
        );
        assert_eq!(result["tool_choice"]["type"], "function");
        assert_eq!(result["tool_choice"]["function"]["name"], "get_weather");
    }

    #[test]
    fn responses_request_to_chat_no_tool_choice_no_tools_stays_clean() {
        // When neither tool_choice nor tools are present, the output should be clean.
        let input = json!({
            "model": "gpt-5.4",
            "input": "hi"
        });

        let result = responses_to_chat_completions(input).unwrap();

        assert!(
            result.get("tool_choice").is_none(),
            "tool_choice should be absent"
        );
        assert!(result.get("tools").is_none(), "tools should be absent");
        assert!(
            result.get("parallel_tool_calls").is_none(),
            "parallel_tool_calls should be absent"
        );
    }

    #[test]
    fn responses_request_to_chat_tool_choice_none_dropped_when_no_tools() {
        // Even tool_choice: "none" should be dropped when tools is absent,
        // because strict upstreams reject the combination regardless of value.
        let input = json!({
            "model": "gpt-5.4",
            "tool_choice": "none",
            "input": "hi"
        });

        let result = responses_to_chat_completions(input).unwrap();

        assert!(
            result.get("tool_choice").is_none(),
            "tool_choice 'none' should be dropped when no tools"
        );
    }

    #[test]
    fn responses_request_to_chat_tool_search_output_provides_tools_keeps_tool_choice() {
        // When tool_search_output in input provides tools, tool_choice should be kept.
        let input = json!({
            "model": "gpt-5.4",
            "tool_choice": "auto",
            "input": [{
                "type": "tool_search_output",
                "call_id": "call_ts_1",
                "status": "completed",
                "execution": "client",
                "tools": [{
                    "type": "function",
                    "name": "search_docs",
                    "description": "Search documentation.",
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "query": {"type": "string"}
                        }
                    }
                }]
            }]
        });

        let result = responses_to_chat_completions(input).unwrap();

        assert!(
            result.get("tool_choice").is_some(),
            "tool_choice should be kept when tool_search_output provides tools"
        );
        assert_eq!(result["tool_choice"], "auto");
        assert!(
            result
                .get("tools")
                .is_some_and(|v| v.as_array().is_some_and(|a| !a.is_empty())),
            "tools should be present from tool_search_output"
        );
        assert_eq!(result["tools"][0]["function"]["name"], "search_docs");
    }
}
