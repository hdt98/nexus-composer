//! OpenAI Chat Completions SSE to OpenAI Responses SSE conversion.

use super::{
    codex_chat_common::{
        contains_think_open_tag, count_unprotected_think_close_tags, extract_reasoning_field_text,
        is_leading_think_close_marker_prefix, is_leading_think_open_marker_prefix,
        leading_think_tag_pair, normalize_glm_think_open_alias_for_literal,
        split_leading_think_block_repairing_unopened_close,
        strip_glm_think_open_alias_from_reasoning, strip_leading_think_close_tag,
        LiteralAwareThinkCloseScanner, ThinkCloseScanner,
    },
    transform_codex_chat::{
        chat_usage_to_responses_usage, custom_tool_input_from_chat_arguments,
        response_id_from_chat_id, response_status_from_finish_reason,
        response_tool_call_item_from_chat_name, response_tool_call_item_id_from_chat_name,
        CodexToolContext,
    },
};
use crate::proxy::json_canonical::canonicalize_tool_arguments_str;
use crate::proxy::sse::{strip_sse_field, take_sse_block};
use bytes::Bytes;
use futures::stream::{Stream, StreamExt};
use serde_json::{json, Value};
use std::collections::BTreeMap;

const VISIBLE_TEXT_GUARD_RELEASE_BYTES: usize = 768;
const VISIBLE_TEXT_GUARD_MAX_UNPROTECTED_CLOSE_TAGS: usize = 0;
const REASONING_GUARD_MIN_REPEATED_TOOL_CALLS: usize = 2;
const REASONING_GUARD_MIN_REPEATED_MARKER_FRAGMENTS: usize = 8;

#[derive(Debug, Default)]
struct TextItemState {
    output_index: Option<u32>,
    item_id: String,
    text: String,
    added: bool,
    done: bool,
}

#[derive(Debug, Default)]
struct VisibleTextGuardState {
    buffer: String,
    released: bool,
}

#[derive(Debug, Default)]
struct ReasoningItemState {
    output_index: Option<u32>,
    item_id: String,
    text: String,
    placeholder_prefix_buffer: String,
    marker_fragment_count: usize,
    added: bool,
    done: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum InlineThinkMode {
    #[default]
    Detecting,
    Reasoning,
    StructuredTextPrefix,
    StructuredDesync,
    Text,
}

#[derive(Debug, Default)]
struct InlineThinkState {
    mode: InlineThinkMode,
    buffer: String,
    close_scanner: ThinkCloseScanner,
    desync_scanner: LiteralAwareThinkCloseScanner,
    saw_content: bool,
}

#[derive(Debug, Default)]
struct ToolCallState {
    output_index: Option<u32>,
    item_id: String,
    call_id: String,
    name: String,
    arguments: String,
    argument_prefix_buffer: String,
    reasoning_content: String,
    added: bool,
    done: bool,
}

#[derive(Debug)]
struct ChatToResponsesState {
    response_started: bool,
    completed: bool,
    response_id: String,
    model: String,
    created_at: u64,
    next_output_index: u32,
    text: TextItemState,
    visible_text_guard: VisibleTextGuardState,
    reasoning: ReasoningItemState,
    inline_think: InlineThinkState,
    tools: BTreeMap<usize, ToolCallState>,
    output_items: Vec<(u32, Value)>,
    latest_usage: Option<Value>,
    finish_reason: Option<String>,
    tool_context: CodexToolContext,
    repair_unopened_think_blocks: bool,
}

impl Default for ChatToResponsesState {
    fn default() -> Self {
        Self {
            response_started: false,
            completed: false,
            response_id: "resp_nexus".to_string(),
            model: String::new(),
            created_at: 0,
            next_output_index: 0,
            text: TextItemState::default(),
            visible_text_guard: VisibleTextGuardState::default(),
            reasoning: ReasoningItemState::default(),
            inline_think: InlineThinkState::default(),
            tools: BTreeMap::new(),
            output_items: Vec::new(),
            latest_usage: None,
            finish_reason: None,
            tool_context: CodexToolContext::default(),
            repair_unopened_think_blocks: false,
        }
    }
}

impl ChatToResponsesState {
    fn with_tool_context(tool_context: CodexToolContext) -> Self {
        let repair_unopened_think_blocks = tool_context.repair_unopened_think_blocks();
        Self {
            tool_context,
            repair_unopened_think_blocks,
            ..Self::default()
        }
    }

    fn handle_chat_chunk(&mut self, chunk: &Value) -> Vec<Bytes> {
        if self.completed {
            return Vec::new();
        }

        let mut events = Vec::new();

        if let Some(id) = chunk.get("id").and_then(|v| v.as_str()) {
            self.response_id = response_id_from_chat_id(Some(id));
        }
        if let Some(model) = chunk.get("model").and_then(|v| v.as_str()) {
            if !model.is_empty() {
                self.model = model.to_string();
            }
        }
        if let Some(created) = chunk.get("created").and_then(|v| v.as_u64()) {
            self.created_at = created;
        }

        events.extend(self.ensure_response_started());

        if let Some(usage) = chunk.get("usage").filter(|v| !v.is_null()) {
            self.latest_usage = Some(chat_usage_to_responses_usage(Some(usage)));
        }

        let Some(choice) = chunk
            .get("choices")
            .and_then(|v| v.as_array())
            .and_then(|choices| choices.first())
        else {
            return events;
        };

        if let Some(delta) = choice.get("delta") {
            if let Some(reasoning) = chat_delta_reasoning_text(delta) {
                if self.repair_unopened_think_blocks
                    && !self.inline_think.saw_content
                    && matches!(self.inline_think.mode, InlineThinkMode::Detecting)
                {
                    // A structured reasoning field proves that subsequent content
                    // is answer text. Inspect only its leading boundary for one
                    // orphan close marker; never sanitize marker text later on.
                    self.inline_think.mode = InlineThinkMode::StructuredTextPrefix;
                    self.inline_think.buffer.clear();
                }
                events.extend(self.push_reasoning_delta(&reasoning));
                if self.completed {
                    return events;
                }
                self.append_reasoning_to_active_tools(&reasoning);
            }

            if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
                if !content.is_empty() {
                    events.extend(self.push_content_delta(content));
                    if self.completed {
                        return events;
                    }
                }
            }

            if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                events.extend(self.flush_inline_think_at_boundary());
                events.extend(self.flush_visible_text_guard());
                events.extend(self.flush_reasoning_placeholder_prefix_at_tool_boundary());
                if self.completed {
                    return events;
                }
                let reasoning_for_tool_call = self.current_reasoning_text();
                events.extend(self.finalize_reasoning());
                for tool_call in tool_calls {
                    events.extend(
                        self.push_tool_call_delta(tool_call, reasoning_for_tool_call.as_deref()),
                    );
                }
            }
        }

        if let Some(finish_reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
            self.finish_reason = Some(finish_reason.to_string());
        }

        events
    }

    fn push_content_delta(&mut self, delta: &str) -> Vec<Bytes> {
        self.inline_think.saw_content = true;
        match self.inline_think.mode {
            InlineThinkMode::Text => {
                let mut events = self.finalize_reasoning();
                events.extend(self.push_text_delta(delta));
                events
            }
            InlineThinkMode::StructuredTextPrefix => self.push_structured_text_prefix_delta(delta),
            InlineThinkMode::StructuredDesync => self.push_structured_desync_delta(delta),
            InlineThinkMode::Detecting => {
                self.inline_think.buffer.push_str(delta);
                match leading_think_prefix_decision(
                    &self.inline_think.buffer,
                    self.repair_unopened_think_blocks,
                ) {
                    ThinkPrefixDecision::NeedMore => Vec::new(),
                    ThinkPrefixDecision::TaggedReasoning(open_tag, close_tag) => {
                        let buffered = std::mem::take(&mut self.inline_think.buffer);
                        let initial = buffered
                            .trim_start()
                            .strip_prefix(open_tag)
                            .unwrap_or_default()
                            .to_string();
                        self.inline_think.mode = InlineThinkMode::Reasoning;
                        self.inline_think.close_scanner = ThinkCloseScanner::tagged(close_tag);
                        self.push_inline_reasoning_delta(&initial)
                    }
                    ThinkPrefixDecision::RawReasoning => {
                        if let Some((reasoning, answer)) =
                            split_leading_think_block_repairing_unopened_close(
                                &self.inline_think.buffer,
                            )
                        {
                            self.inline_think.mode = InlineThinkMode::Text;
                            self.inline_think.buffer.clear();
                            let mut events = Vec::new();
                            if !reasoning.is_empty() {
                                events.extend(self.push_reasoning_delta(&reasoning));
                                self.append_reasoning_to_active_tools(&reasoning);
                                events.extend(self.finalize_reasoning());
                            }
                            if !answer.is_empty() {
                                events.extend(self.push_text_delta(&answer));
                            }
                            events
                        } else if contains_think_open_tag(&self.inline_think.buffer) {
                            // An opener before the first close is ordinary answer
                            // content (commonly source code discussing delimiters),
                            // not an unopened reasoning block. Preserve it exactly.
                            self.inline_think.mode = InlineThinkMode::Text;
                            let text = std::mem::take(&mut self.inline_think.buffer);
                            let mut events = self.finalize_reasoning();
                            events.extend(self.push_text_delta(&text));
                            events
                        } else {
                            // An unopened GLM reasoning prefix is ambiguous until a
                            // close marker, tool-call boundary, or finish reason arrives.
                            Vec::new()
                        }
                    }
                    ThinkPrefixDecision::Text => {
                        self.inline_think.mode = InlineThinkMode::Text;
                        let text = std::mem::take(&mut self.inline_think.buffer);
                        let mut events = self.finalize_reasoning();
                        if !text.is_empty() {
                            events.extend(self.push_text_delta(&text));
                        }
                        events
                    }
                }
            }
            InlineThinkMode::Reasoning => self.push_inline_reasoning_delta(delta),
        }
    }

    fn push_inline_reasoning_delta(&mut self, delta: &str) -> Vec<Bytes> {
        let scan = self.inline_think.close_scanner.push(delta);
        let mut events = Vec::new();
        if !scan.reasoning.is_empty() {
            events.extend(self.push_reasoning_delta(&scan.reasoning));
            self.append_reasoning_to_active_tools(&scan.reasoning);
        }

        if let Some(answer) = scan.answer {
            self.inline_think.mode = InlineThinkMode::Text;
            events.extend(self.finalize_reasoning());
            if !answer.is_empty() {
                events.extend(self.push_text_delta(&answer));
            }
        }

        events
    }

    fn push_structured_text_prefix_delta(&mut self, delta: &str) -> Vec<Bytes> {
        self.inline_think.buffer.push_str(delta);
        let buffered = self.inline_think.buffer.as_str();
        let trimmed = buffered.trim_start();
        if trimmed.is_empty() {
            return Vec::new();
        }

        let text = if let Some(text) = strip_leading_think_close_tag(buffered) {
            self.inline_think.mode = InlineThinkMode::Text;
            self.inline_think.buffer.clear();
            text
        } else if is_leading_think_close_marker_prefix(trimmed) {
            return Vec::new();
        } else {
            if self.repair_unopened_think_blocks {
                let scanner =
                    LiteralAwareThinkCloseScanner::from_reasoning_prefix(&self.reasoning.text);
                if scanner.starts_inside_code() {
                    let initial = std::mem::take(&mut self.inline_think.buffer);
                    self.inline_think.mode = InlineThinkMode::StructuredDesync;
                    self.inline_think.desync_scanner = scanner;
                    return self.push_structured_desync_delta(&initial);
                }
            }
            self.inline_think.mode = InlineThinkMode::Text;
            std::mem::take(&mut self.inline_think.buffer)
        };

        let mut events = self.finalize_reasoning();
        if !text.is_empty() {
            events.extend(self.push_text_delta(&text));
        }
        events
    }

    fn push_structured_desync_delta(&mut self, delta: &str) -> Vec<Bytes> {
        self.inline_think.buffer.push_str(delta);
        let boundary = self.inline_think.desync_scanner.push(delta);
        if self.inline_think.desync_scanner.is_blocked() {
            return self.flush_structured_desync_as_text();
        }
        let Some(boundary) = boundary else {
            return Vec::new();
        };

        let buffered = std::mem::take(&mut self.inline_think.buffer);
        let reasoning_extension = &buffered[..boundary.close_start];
        let answer = buffered[boundary.close_start + boundary.close_tag.len()..]
            .trim_start_matches(['\r', '\n', '\t', ' ']);
        self.inline_think.mode = InlineThinkMode::Text;
        self.inline_think.desync_scanner = LiteralAwareThinkCloseScanner::default();

        let mut events = Vec::new();
        if !reasoning_extension.is_empty() {
            events.extend(self.push_reasoning_delta(reasoning_extension));
            self.append_reasoning_to_active_tools(reasoning_extension);
        }
        events.extend(self.finalize_reasoning());
        if !answer.is_empty() {
            events.extend(self.push_text_delta(answer));
        }
        events
    }

    fn flush_structured_desync_as_text(&mut self) -> Vec<Bytes> {
        let buffered = std::mem::take(&mut self.inline_think.buffer);
        if let Some(boundary) = self
            .inline_think
            .desync_scanner
            .fallback_last_close_boundary(&buffered)
        {
            let reasoning_extension = &buffered[..boundary.close_start];
            let answer = buffered[boundary.close_start + boundary.close_tag.len()..]
                .trim_start_matches(['\r', '\n', '\t', ' ']);
            self.inline_think.mode = InlineThinkMode::Text;
            self.inline_think.desync_scanner = LiteralAwareThinkCloseScanner::default();

            let mut events = Vec::new();
            if !reasoning_extension.is_empty() {
                events.extend(self.push_reasoning_delta(reasoning_extension));
                self.append_reasoning_to_active_tools(reasoning_extension);
            }
            events.extend(self.finalize_reasoning());
            if !answer.is_empty() {
                events.extend(self.push_text_delta(answer));
            }
            return events;
        }

        self.inline_think.mode = InlineThinkMode::Text;
        self.inline_think.desync_scanner = LiteralAwareThinkCloseScanner::default();
        let mut events = self.finalize_reasoning();
        if !buffered.is_empty() {
            events.extend(self.push_text_delta(&buffered));
        }
        events
    }

    fn flush_inline_think_at_boundary(&mut self) -> Vec<Bytes> {
        match self.inline_think.mode {
            InlineThinkMode::Text => Vec::new(),
            InlineThinkMode::StructuredTextPrefix => {
                self.inline_think.mode = InlineThinkMode::Text;
                let buffered = std::mem::take(&mut self.inline_think.buffer);
                let text = strip_leading_think_close_tag(&buffered).unwrap_or(buffered);
                let mut events = self.finalize_reasoning();
                if !text.is_empty() {
                    events.extend(self.push_text_delta(&text));
                }
                events
            }
            InlineThinkMode::StructuredDesync => self.flush_structured_desync_as_text(),
            InlineThinkMode::Detecting => {
                let buffered = std::mem::take(&mut self.inline_think.buffer);
                if buffered.is_empty() {
                    self.inline_think.mode = InlineThinkMode::Text;
                    return Vec::new();
                }

                if self.repair_unopened_think_blocks {
                    self.inline_think.mode = InlineThinkMode::Reasoning;
                    self.inline_think.close_scanner = ThinkCloseScanner::unopened();
                    let mut events = self.push_inline_reasoning_delta(&buffered);
                    if matches!(self.inline_think.mode, InlineThinkMode::Reasoning) {
                        events.extend(self.flush_reasoning_scanner_at_boundary());
                    }
                    events
                } else {
                    self.inline_think.mode = InlineThinkMode::Text;
                    let mut events = Vec::new();
                    events.extend(self.push_text_delta(&buffered));
                    events
                }
            }
            InlineThinkMode::Reasoning => self.flush_reasoning_scanner_at_boundary(),
        }
    }

    fn flush_inline_think_at_stream_end(&mut self) -> Vec<Bytes> {
        if self.repair_unopened_think_blocks
            && matches!(self.inline_think.mode, InlineThinkMode::Detecting)
            && self.finish_reason.as_deref() == Some("stop")
        {
            // A clean stop with no structured reasoning field and no delimiter
            // is normal answer text. Treating it as reasoning-only leaves Codex
            // with no assistant item and can trigger an unbounded retry loop.
            self.inline_think.mode = InlineThinkMode::Text;
            let text = std::mem::take(&mut self.inline_think.buffer);
            let mut events = self.finalize_reasoning();
            if !text.is_empty() {
                events.extend(self.push_text_delta(&text));
            }
            return events;
        }

        self.flush_inline_think_at_boundary()
    }

    fn flush_reasoning_scanner_at_boundary(&mut self) -> Vec<Bytes> {
        let pending = self.inline_think.close_scanner.finish();
        let mut events = Vec::new();
        if !pending.is_empty() {
            events.extend(self.push_reasoning_delta(&pending));
            self.append_reasoning_to_active_tools(&pending);
        }
        events.extend(self.finalize_reasoning());
        self.inline_think.mode = InlineThinkMode::Text;
        events
    }

    fn ensure_response_started(&mut self) -> Vec<Bytes> {
        if self.response_started {
            return Vec::new();
        }

        self.response_started = true;
        let response = self.base_response("in_progress", Vec::new());

        vec![
            sse_event(
                "response.created",
                json!({
                    "type": "response.created",
                    "response": response
                }),
            ),
            sse_event(
                "response.in_progress",
                json!({
                    "type": "response.in_progress",
                    "response": self.base_response("in_progress", Vec::new())
                }),
            ),
        ]
    }

    fn push_reasoning_delta(&mut self, delta: &str) -> Vec<Bytes> {
        let mut events = Vec::new();
        let delta = if self.repair_unopened_think_blocks {
            strip_glm_think_open_alias_from_reasoning(delta)
        } else {
            std::borrow::Cow::Borrowed(delta)
        };
        let mut delta = delta.as_ref().to_string();
        if delta.is_empty() {
            return events;
        }

        if self.repair_unopened_think_blocks {
            if is_suspicious_reasoning_marker_fragment(&delta) {
                self.reasoning.marker_fragment_count =
                    self.reasoning.marker_fragment_count.saturating_add(1);
                self.reasoning.placeholder_prefix_buffer.clear();
                if self.reasoning.marker_fragment_count
                    >= REASONING_GUARD_MIN_REPEATED_MARKER_FRAGMENTS
                {
                    return vec![self.failed_event(
                        "Upstream response repeated raw reasoning delimiter fragments inside reasoning output"
                            .to_string(),
                        Some("upstream_reasoning_marker_loop".to_string()),
                    )];
                }
                return events;
            }

            if self.tools.is_empty() && self.reasoning.text.is_empty() {
                if !self.reasoning.placeholder_prefix_buffer.is_empty() {
                    self.reasoning.placeholder_prefix_buffer.push_str(&delta);
                    let candidate = self.reasoning.placeholder_prefix_buffer.clone();
                    if is_tool_call_placeholder_pending_candidate(&candidate) {
                        return events;
                    }
                    self.reasoning.placeholder_prefix_buffer.clear();
                    delta = candidate;
                } else if is_tool_call_placeholder_pending_candidate(&delta) {
                    self.reasoning.placeholder_prefix_buffer = delta;
                    return events;
                }
            }

            let mut candidate = self.reasoning.text.clone();
            candidate.push_str(&delta);
            if let Some(message) = pathological_reasoning_text_message(&candidate) {
                self.reasoning.placeholder_prefix_buffer.clear();
                return vec![self.failed_event(
                    message.to_string(),
                    Some("upstream_reasoning_loop".to_string()),
                )];
            }
        }

        if !self.reasoning.added {
            let output_index = self.next_output_index();
            let item_id = format!("rs_{}", self.response_id);
            self.reasoning.output_index = Some(output_index);
            self.reasoning.item_id = item_id.clone();
            self.reasoning.added = true;

            events.push(sse_event(
                "response.output_item.added",
                json!({
                    "type": "response.output_item.added",
                    "output_index": output_index,
                    "item": {
                        "id": item_id,
                        "type": "reasoning",
                        "status": "in_progress",
                        "summary": []
                    }
                }),
            ));
            events.push(sse_event(
                "response.reasoning_summary_part.added",
                json!({
                    "type": "response.reasoning_summary_part.added",
                    "item_id": self.reasoning.item_id,
                    "output_index": output_index,
                    "summary_index": 0,
                    "part": {
                        "type": "summary_text",
                        "text": ""
                    }
                }),
            ));
        }

        self.reasoning.text.push_str(&delta);
        let output_index = self.reasoning.output_index.unwrap_or(0);
        events.push(sse_event(
            "response.reasoning_summary_text.delta",
            json!({
                "type": "response.reasoning_summary_text.delta",
                "item_id": self.reasoning.item_id,
                "output_index": output_index,
                "summary_index": 0,
                "delta": delta
            }),
        ));

        events
    }

    fn push_text_delta(&mut self, delta: &str) -> Vec<Bytes> {
        if self.repair_unopened_think_blocks && self.visible_text_guard.released {
            if let Some(repaired) = strip_recoverable_visible_boundary_artifact(delta) {
                if repaired.is_empty() {
                    return Vec::new();
                }
                return self.emit_text_delta(&repaired);
            }
            if let Some(message) = pathological_visible_text_message(delta) {
                return vec![self.failed_event(
                    message.to_string(),
                    Some("upstream_reasoning_marker_leak".to_string()),
                )];
            }
        }

        if self.repair_unopened_think_blocks && !self.visible_text_guard.released {
            self.visible_text_guard.buffer.push_str(delta);
            if let Some(repaired) =
                strip_recoverable_visible_boundary_artifact(&self.visible_text_guard.buffer)
            {
                self.visible_text_guard.buffer = repaired;
            }
            if let Some(message) =
                pathological_visible_text_message(&self.visible_text_guard.buffer)
            {
                self.visible_text_guard.buffer.clear();
                return vec![self.failed_event(
                    message.to_string(),
                    Some("upstream_reasoning_marker_leak".to_string()),
                )];
            }

            if self.visible_text_guard.buffer.len() < VISIBLE_TEXT_GUARD_RELEASE_BYTES {
                return Vec::new();
            }

            self.visible_text_guard.released = true;
            let buffered = std::mem::take(&mut self.visible_text_guard.buffer);
            return self.emit_text_delta(&buffered);
        }

        self.emit_text_delta(delta)
    }

    fn flush_visible_text_guard(&mut self) -> Vec<Bytes> {
        if !self.repair_unopened_think_blocks
            || self.visible_text_guard.released
            || self.visible_text_guard.buffer.is_empty()
        {
            return Vec::new();
        }

        if let Some(repaired) =
            strip_recoverable_visible_boundary_artifact(&self.visible_text_guard.buffer)
        {
            self.visible_text_guard.buffer = repaired;
        }

        if let Some(message) = pathological_visible_text_message(&self.visible_text_guard.buffer) {
            self.visible_text_guard.buffer.clear();
            return vec![self.failed_event(
                message.to_string(),
                Some("upstream_reasoning_marker_leak".to_string()),
            )];
        }

        self.visible_text_guard.released = true;
        let buffered = std::mem::take(&mut self.visible_text_guard.buffer);
        self.emit_text_delta(&buffered)
    }

    fn emit_text_delta(&mut self, delta: &str) -> Vec<Bytes> {
        let mut events = Vec::new();

        if !self.text.added {
            let output_index = self.next_output_index();
            let item_id = format!("{}_msg", self.response_id);
            self.text.output_index = Some(output_index);
            self.text.item_id = item_id.clone();
            self.text.added = true;

            events.push(sse_event(
                "response.output_item.added",
                json!({
                    "type": "response.output_item.added",
                    "output_index": output_index,
                    "item": {
                        "id": item_id,
                        "type": "message",
                        "status": "in_progress",
                        "role": "assistant",
                        "content": []
                    }
                }),
            ));
            events.push(sse_event(
                "response.content_part.added",
                json!({
                    "type": "response.content_part.added",
                    "item_id": self.text.item_id,
                    "output_index": output_index,
                    "content_index": 0,
                    "part": {
                        "type": "output_text",
                        "text": "",
                        "annotations": []
                    }
                }),
            ));
        }

        self.text.text.push_str(delta);
        let output_index = self.text.output_index.unwrap_or(0);
        events.push(sse_event(
            "response.output_text.delta",
            json!({
                "type": "response.output_text.delta",
                "item_id": self.text.item_id,
                "output_index": output_index,
                "content_index": 0,
                "delta": delta
            }),
        ));

        events
    }

    fn current_reasoning_text(&self) -> Option<String> {
        (!self.reasoning.text.trim().is_empty()).then(|| self.reasoning.text.trim().to_string())
    }

    fn flush_reasoning_placeholder_prefix_at_tool_boundary(&mut self) -> Vec<Bytes> {
        if !self.repair_unopened_think_blocks || self.reasoning.placeholder_prefix_buffer.is_empty()
        {
            return Vec::new();
        }

        self.reasoning.placeholder_prefix_buffer.clear();
        Vec::new()
    }

    fn push_tool_call_delta(&mut self, tool_call: &Value, reasoning: Option<&str>) -> Vec<Bytes> {
        let chat_index = tool_call.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let id_delta = tool_call
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let function = tool_call.get("function").unwrap_or(&Value::Null);
        let name_delta = function
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let args_delta = function
            .get("arguments")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let args_delta = if self.repair_unopened_think_blocks {
            normalize_glm_think_open_alias_for_literal(&args_delta).into_owned()
        } else {
            args_delta
        };

        let mut should_add = false;
        let mut output_index = None;
        let mut item_id = String::new();
        let mut pending_arguments = String::new();
        let mut emitted_args_delta = String::new();
        let current_name: String;

        {
            let state = self.tools.entry(chat_index).or_default();
            if let Some(id) = id_delta {
                state.call_id = id;
            }
            if let Some(ref name) = name_delta {
                if !name.is_empty() {
                    state.name.clone_from(name);
                }
            }
            let repaired_args_delta = if self.repair_unopened_think_blocks {
                Self::repair_leading_orphan_think_close_in_tool_arguments(state, &args_delta)
            } else {
                args_delta.clone()
            };
            if !repaired_args_delta.is_empty() {
                state.arguments.push_str(&repaired_args_delta);
                emitted_args_delta = repaired_args_delta;
            }
            if state.reasoning_content.is_empty() {
                if let Some(reasoning) = reasoning.map(str::trim).filter(|value| !value.is_empty())
                {
                    state.reasoning_content = reasoning.to_string();
                }
            }

            if !state.added && !state.call_id.is_empty() && !state.name.is_empty() {
                should_add = true;
                pending_arguments = state.arguments.clone();
            } else if state.added {
                output_index = state.output_index;
                item_id = state.item_id.clone();
            }
            current_name = state.name.clone();
        }

        let is_custom_tool = self.tool_context.is_custom_tool_chat_name(&current_name);
        let mut events = Vec::new();

        if should_add {
            let assigned = self.next_output_index();
            let Some(state) = self.tools.get_mut(&chat_index) else {
                return events;
            };
            state.added = true;
            if state.call_id.is_empty() {
                state.call_id = format!("call_{chat_index}");
            }
            state.output_index = Some(assigned);
            let is_custom_tool = self.tool_context.is_custom_tool_chat_name(&state.name);
            state.item_id = response_tool_call_item_id_from_chat_name(
                &state.call_id,
                &state.name,
                &self.tool_context,
            );
            item_id = state.item_id.clone();

            let item = response_tool_call_item_from_chat_name(
                &item_id,
                "in_progress",
                &state.call_id,
                &state.name,
                "",
                Some(&state.reasoning_content),
                &self.tool_context,
            );

            events.push(sse_event(
                "response.output_item.added",
                json!({
                    "type": "response.output_item.added",
                    "output_index": assigned,
                    "item": item
                }),
            ));

            if !pending_arguments.is_empty() && !is_custom_tool {
                events.push(sse_event(
                    "response.function_call_arguments.delta",
                    json!({
                        "type": "response.function_call_arguments.delta",
                        "item_id": state.item_id,
                        "output_index": assigned,
                        "delta": pending_arguments
                    }),
                ));
            }
        } else if !emitted_args_delta.is_empty() && !is_custom_tool {
            if let Some(output_index) = output_index {
                events.push(sse_event(
                    "response.function_call_arguments.delta",
                    json!({
                        "type": "response.function_call_arguments.delta",
                        "item_id": item_id,
                        "output_index": output_index,
                        "delta": emitted_args_delta
                    }),
                ));
            }
        }

        events
    }

    fn append_reasoning_to_active_tools(&mut self, delta: &str) {
        let delta = if self.repair_unopened_think_blocks {
            strip_glm_think_open_alias_from_reasoning(delta)
        } else {
            std::borrow::Cow::Borrowed(delta)
        };
        let delta = delta.as_ref();
        if delta.trim().is_empty() {
            return;
        }

        for state in self.tools.values_mut().filter(|state| !state.done) {
            if state.reasoning_content.is_empty() {
                state.reasoning_content = delta.trim_start().to_string();
            } else {
                state.reasoning_content.push_str(delta);
            }
        }
    }

    fn repair_leading_orphan_think_close_in_tool_arguments(
        state: &mut ToolCallState,
        delta: &str,
    ) -> String {
        if delta.is_empty() || !state.arguments.is_empty() {
            return delta.to_string();
        }

        let mut candidate = std::mem::take(&mut state.argument_prefix_buffer);
        candidate.push_str(delta);

        if candidate.trim_start().is_empty() {
            state.argument_prefix_buffer = candidate;
            return String::new();
        }

        if let Some(repaired) = strip_leading_think_close_tag(&candidate) {
            return repaired;
        }

        if is_leading_think_close_marker_prefix(candidate.trim_start()) {
            state.argument_prefix_buffer = candidate;
            return String::new();
        }

        candidate
    }

    fn has_substantive_output(&self) -> bool {
        !self.text.text.trim().is_empty()
            || !self.visible_text_guard.buffer.trim().is_empty()
            || !self.reasoning.text.trim().is_empty()
            || !self.inline_think.buffer.trim().is_empty()
            || self.inline_think.close_scanner.has_pending()
            || !self.output_items.is_empty()
            || self.tools.values().any(|state| {
                state.added
                    || !state.call_id.trim().is_empty()
                    || !state.name.trim().is_empty()
                    || !state.arguments.trim().is_empty()
                    || !state.argument_prefix_buffer.trim().is_empty()
                    || !state.reasoning_content.trim().is_empty()
            })
    }

    fn finalize(&mut self) -> Vec<Bytes> {
        if self.completed {
            return Vec::new();
        }

        let mut events = self.ensure_response_started();
        events.extend(self.flush_inline_think_at_stream_end());
        if self.completed {
            return events;
        }
        events.extend(self.finalize_reasoning());
        events.extend(self.flush_visible_text_guard());
        if self.completed {
            return events;
        }
        events.extend(self.finalize_text());
        events.extend(self.finalize_tools());

        let status = response_status_from_finish_reason(self.finish_reason.as_deref());
        let mut response = self.base_response(status, self.completed_output_items());
        if status == "incomplete" {
            response["incomplete_details"] = json!({ "reason": "max_output_tokens" });
        }

        events.push(sse_event(
            "response.completed",
            json!({
                "type": "response.completed",
                "response": response
            }),
        ));
        self.completed = true;
        events
    }

    fn finalize_reasoning(&mut self) -> Vec<Bytes> {
        if !self.reasoning.added || self.reasoning.done {
            return Vec::new();
        }

        let output_index = self.reasoning.output_index.unwrap_or(0);
        let item_id = self.reasoning.item_id.clone();
        let text = self.reasoning.text.clone();
        let item = json!({
            "id": item_id,
            "type": "reasoning",
            "summary": [{
                "type": "summary_text",
                "text": text
            }]
        });
        self.output_items.push((output_index, item.clone()));
        self.reasoning.done = true;

        vec![
            sse_event(
                "response.reasoning_summary_text.done",
                json!({
                    "type": "response.reasoning_summary_text.done",
                    "item_id": self.reasoning.item_id,
                    "output_index": output_index,
                    "summary_index": 0,
                    "text": self.reasoning.text
                }),
            ),
            sse_event(
                "response.reasoning_summary_part.done",
                json!({
                    "type": "response.reasoning_summary_part.done",
                    "item_id": self.reasoning.item_id,
                    "output_index": output_index,
                    "summary_index": 0,
                    "part": {
                        "type": "summary_text",
                        "text": self.reasoning.text
                    }
                }),
            ),
            sse_event(
                "response.output_item.done",
                json!({
                    "type": "response.output_item.done",
                    "output_index": output_index,
                    "item": item
                }),
            ),
        ]
    }

    fn finalize_text(&mut self) -> Vec<Bytes> {
        if !self.text.added || self.text.done {
            return Vec::new();
        }

        let output_index = self.text.output_index.unwrap_or(0);
        let item = json!({
            "id": self.text.item_id,
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": self.text.text,
                "annotations": []
            }]
        });
        self.output_items.push((output_index, item.clone()));
        self.text.done = true;

        vec![
            sse_event(
                "response.output_text.done",
                json!({
                    "type": "response.output_text.done",
                    "item_id": self.text.item_id,
                    "output_index": output_index,
                    "content_index": 0,
                    "text": self.text.text
                }),
            ),
            sse_event(
                "response.content_part.done",
                json!({
                    "type": "response.content_part.done",
                    "item_id": self.text.item_id,
                    "output_index": output_index,
                    "content_index": 0,
                    "part": {
                        "type": "output_text",
                        "text": self.text.text,
                        "annotations": []
                    }
                }),
            ),
            sse_event(
                "response.output_item.done",
                json!({
                    "type": "response.output_item.done",
                    "output_index": output_index,
                    "item": item
                }),
            ),
        ]
    }

    fn finalize_tools(&mut self) -> Vec<Bytes> {
        let mut events = Vec::new();
        let keys: Vec<usize> = self.tools.keys().copied().collect();

        for key in keys {
            let mut add_event: Option<Bytes> = None;
            if self.tools.get(&key).map(|state| state.done).unwrap_or(true) {
                continue;
            }

            // Skip tool calls with missing names (defensive: some models generate
            // tool call deltas without providing a valid function name)
            let has_bad_name = self
                .tools
                .get(&key)
                .map(|state| state.name.is_empty())
                .unwrap_or(true);
            if has_bad_name {
                if let Some(state) = self.tools.get_mut(&key) {
                    state.done = true;
                }
                log::warn!("[Codex] Skipping streaming tool call with missing name");
                continue;
            }

            if self
                .tools
                .get(&key)
                .map(|state| !state.added && !state.done)
                .unwrap_or(false)
            {
                let assigned = self.next_output_index();
                let Some(state) = self.tools.get_mut(&key) else {
                    continue;
                };
                state.added = true;
                if state.call_id.is_empty() {
                    state.call_id = format!("call_{key}");
                }
                state.output_index = Some(assigned);
                state.item_id = response_tool_call_item_id_from_chat_name(
                    &state.call_id,
                    &state.name,
                    &self.tool_context,
                );
                let item = response_tool_call_item_from_chat_name(
                    &state.item_id,
                    "in_progress",
                    &state.call_id,
                    &state.name,
                    "",
                    Some(&state.reasoning_content),
                    &self.tool_context,
                );
                add_event = Some(sse_event(
                    "response.output_item.added",
                    json!({
                        "type": "response.output_item.added",
                        "output_index": assigned,
                        "item": item
                    }),
                ));
            }

            if let Some(event) = add_event {
                events.push(event);
            }

            let Some(state) = self.tools.get_mut(&key) else {
                continue;
            };
            if self.repair_unopened_think_blocks && !state.argument_prefix_buffer.is_empty() {
                let pending = std::mem::take(&mut state.argument_prefix_buffer);
                state.arguments.push_str(&pending);
            }
            let output_index = state.output_index.unwrap_or(0);
            let arguments = canonicalize_tool_arguments_str(&state.arguments);
            let is_custom_tool = self.tool_context.is_custom_tool_chat_name(&state.name);
            let item = response_tool_call_item_from_chat_name(
                &state.item_id,
                "completed",
                &state.call_id,
                &state.name,
                &arguments,
                Some(&state.reasoning_content),
                &self.tool_context,
            );
            state.done = true;
            self.output_items.push((output_index, item.clone()));

            if is_custom_tool {
                let input = custom_tool_input_from_chat_arguments(&arguments);
                if !input.is_empty() {
                    events.push(sse_event(
                        "response.custom_tool_call_input.delta",
                        json!({
                            "type": "response.custom_tool_call_input.delta",
                            "item_id": state.item_id,
                            "output_index": output_index,
                            "delta": input.clone()
                        }),
                    ));
                }
                events.push(sse_event(
                    "response.custom_tool_call_input.done",
                    json!({
                        "type": "response.custom_tool_call_input.done",
                        "item_id": state.item_id,
                        "output_index": output_index,
                        "input": input
                    }),
                ));
            } else {
                events.push(sse_event(
                    "response.function_call_arguments.done",
                    json!({
                        "type": "response.function_call_arguments.done",
                        "item_id": state.item_id,
                        "output_index": output_index,
                        "arguments": arguments
                    }),
                ));
            }
            events.push(sse_event(
                "response.output_item.done",
                json!({
                    "type": "response.output_item.done",
                    "output_index": output_index,
                    "item": item
                }),
            ));
        }

        events
    }

    fn completed_output_items(&self) -> Vec<Value> {
        let mut output_items = self.output_items.clone();
        output_items.sort_by_key(|(output_index, _)| *output_index);
        output_items
            .into_iter()
            .map(|(_, item)| item)
            .collect::<Vec<_>>()
    }

    fn base_response(&self, status: &str, output: Vec<Value>) -> Value {
        json!({
            "id": self.response_id,
            "object": "response",
            "created_at": self.created_at,
            "status": status,
            "model": self.model,
            "output": output,
            "usage": self.latest_usage.clone().unwrap_or_else(|| {
                json!({
                    "input_tokens": 0,
                    "output_tokens": 0,
                    "total_tokens": 0,
                    "output_tokens_details": { "reasoning_tokens": 0 }
                })
            })
        })
    }

    fn next_output_index(&mut self) -> u32 {
        let index = self.next_output_index;
        self.next_output_index += 1;
        index
    }

    fn failed_event(&mut self, message: String, error_type: Option<String>) -> Bytes {
        self.completed = true;
        let mut error = json!({ "message": message });
        if let Some(error_type) = error_type.filter(|value| !value.is_empty()) {
            error["type"] = json!(error_type);
        }

        let mut response = self.base_response("failed", self.completed_output_items());
        response["error"] = error;

        sse_event(
            "response.failed",
            json!({
                "type": "response.failed",
                "response": response
            }),
        )
    }
}

fn chat_delta_reasoning_text(delta: &Value) -> Option<String> {
    extract_reasoning_field_text(delta)
}

enum ThinkPrefixDecision {
    NeedMore,
    TaggedReasoning(&'static str, &'static str),
    RawReasoning,
    Text,
}

fn leading_think_prefix_decision(
    buffer: &str,
    repair_unopened_think_blocks: bool,
) -> ThinkPrefixDecision {
    let trimmed = buffer.trim_start();
    if trimmed.is_empty() {
        return ThinkPrefixDecision::NeedMore;
    }

    if let Some((open_tag, close_tag)) = leading_think_tag_pair(trimmed) {
        return ThinkPrefixDecision::TaggedReasoning(open_tag, close_tag);
    }

    if is_leading_think_open_marker_prefix(trimmed) {
        return ThinkPrefixDecision::NeedMore;
    }

    if repair_unopened_think_blocks {
        return ThinkPrefixDecision::RawReasoning;
    }

    ThinkPrefixDecision::Text
}

fn pathological_visible_text_message(text: &str) -> Option<&'static str> {
    let close_count = count_unprotected_think_close_tags(text);
    if close_count > VISIBLE_TEXT_GUARD_MAX_UNPROTECTED_CLOSE_TAGS {
        return Some("Upstream response leaked a raw reasoning delimiter into visible output");
    }

    None
}

fn strip_recoverable_visible_boundary_artifact(text: &str) -> Option<String> {
    if count_unprotected_think_close_tags(text) != 1 {
        return None;
    }

    let trimmed_end = text.trim_end_matches(['\r', '\n', '\t', ' ']);
    for close_tag in ["</thinking>", "</think>"] {
        if let Some(prefix) = trimmed_end.strip_suffix(close_tag) {
            return Some(prefix.to_string());
        }
    }

    None
}

fn pathological_reasoning_text_message(text: &str) -> Option<&'static str> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let repeated_tool_calls = tokens
        .chunks_exact(2)
        .filter(|pair| *pair == ["tool", "call"])
        .count();
    let is_only_tool_call_loop = !tokens.is_empty()
        && tokens.len() == repeated_tool_calls * 2
        && repeated_tool_calls >= REASONING_GUARD_MIN_REPEATED_TOOL_CALLS;

    if is_only_tool_call_loop {
        return Some("Upstream response repeated a tool-call placeholder inside reasoning output");
    }

    None
}

fn is_suspicious_reasoning_marker_fragment(text: &str) -> bool {
    let trimmed = text.trim();
    if !trimmed.contains("</think") && !trimmed.contains("</thinking") {
        return false;
    }

    // A long sentence can legitimately discuss marker syntax during a diagnostic
    // task. The pathological GLM/Codex cases arrive as short parser-sentinel
    // fragments such as `</thinking`, `</thinking"` or `tool call</thinking`.
    trimmed.chars().count() <= 96 && trimmed.split_whitespace().count() <= 4
}

fn is_tool_call_placeholder_pending_candidate(text: &str) -> bool {
    let normalized = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();

    normalized == "tool call"
        || (!normalized.is_empty()
            && normalized != "tool call"
            && "tool call".starts_with(&normalized))
}

/// Create a stream that converts Chat Completions SSE chunks into Responses SSE events.
#[allow(dead_code)]
pub fn create_responses_sse_stream_from_chat<E: std::error::Error + Send + 'static>(
    stream: impl Stream<Item = Result<Bytes, E>> + Send + 'static,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send {
    create_responses_sse_stream_from_chat_with_context(stream, CodexToolContext::default())
}

/// Create a stream that converts Chat Completions SSE chunks into Responses SSE
/// events while restoring Codex tool namespace/custom/tool_search metadata.
pub fn create_responses_sse_stream_from_chat_with_context<E: std::error::Error + Send + 'static>(
    stream: impl Stream<Item = Result<Bytes, E>> + Send + 'static,
    tool_context: CodexToolContext,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send {
    async_stream::stream! {
        let mut buffer = String::new();
        let mut utf8_remainder: Vec<u8> = Vec::new();
        let mut state = ChatToResponsesState::with_tool_context(tool_context);
        let mut stream_failed = false;

        tokio::pin!(stream);

        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    crate::proxy::sse::append_utf8_safe(&mut buffer, &mut utf8_remainder, &bytes);

                    while let Some(block) = take_sse_block(&mut buffer) {
                        if block.trim().is_empty() {
                            continue;
                        }

                        let mut event_name: Option<String> = None;
                        let mut data_parts: Vec<String> = Vec::new();
                        for line in block.lines() {
                            if let Some(event) = strip_sse_field(line, "event") {
                                event_name = Some(event.trim().to_string());
                            }
                            if let Some(data) = strip_sse_field(line, "data") {
                                data_parts.push(data.to_string());
                            }
                        }

                        if data_parts.is_empty() {
                            continue;
                        }

                        let data = data_parts.join("\n");
                        if data.trim() == "[DONE]" {
                            for event in state.finalize() {
                                yield Ok(event);
                            }
                            continue;
                        }

                        let chunk: Value = match serde_json::from_str(&data) {
                            Ok(value) => value,
                            Err(_) => continue,
                        };

                        if event_name.as_deref() == Some("error") || chunk.get("error").is_some() {
                            let (message, error_type) = extract_chat_sse_error(&chunk);
                            yield Ok(state.failed_event(message, error_type));
                            stream_failed = true;
                            break;
                        }

                        for event in state.handle_chat_chunk(&chunk) {
                            yield Ok(event);
                        }
                    }

                    if stream_failed {
                        break;
                    }
                }
                Err(e) => {
                    yield Ok(state.failed_event(
                        format!("Stream error: {e}"),
                        Some("stream_error".to_string()),
                    ));
                    stream_failed = true;
                    break;
                }
            }
        }

        if !stream_failed {
            if state.completed || state.finish_reason.is_some() {
                for event in state.finalize() {
                    yield Ok(event);
                }
            } else if state.has_substantive_output() {
                state.finish_reason = Some("length".to_string());
                for event in state.finalize() {
                    yield Ok(event);
                }
            } else {
                yield Ok(state.failed_event(
                    "Upstream Chat Completions stream ended before sending finish_reason".to_string(),
                    Some("stream_truncated".to_string()),
                ));
            }
        }
    }
}

fn extract_chat_sse_error(value: &Value) -> (String, Option<String>) {
    let error = value.get("error").unwrap_or(value);
    let message = error
        .as_str()
        .map(ToString::to_string)
        .or_else(|| {
            error
                .get("message")
                .or_else(|| error.get("detail"))
                .and_then(|v| v.as_str())
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| error.to_string());
    let error_type = error
        .get("type")
        .or_else(|| error.get("code"))
        .and_then(|v| v.as_str())
        .map(ToString::to_string);

    (message, error_type)
}

fn sse_event(event: &str, data: Value) -> Bytes {
    Bytes::from(format!(
        "event: {event}\ndata: {}\n\n",
        serde_json::to_string(&data).unwrap_or_default()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::providers::codex_chat_common::glm_think_open_alias_char;
    use futures::{stream, StreamExt};

    async fn collect(chunks: Vec<&str>) -> String {
        collect_with_context(chunks, CodexToolContext::default()).await
    }

    async fn collect_with_context(chunks: Vec<&str>, tool_context: CodexToolContext) -> String {
        let chunks: Vec<Result<Bytes, std::io::Error>> = chunks
            .into_iter()
            .map(|chunk| Ok(Bytes::copy_from_slice(chunk.as_bytes())))
            .collect();
        let upstream = stream::iter(chunks);
        let converted = create_responses_sse_stream_from_chat_with_context(upstream, tool_context);
        let bytes: Vec<Bytes> = converted.map(|item| item.unwrap()).collect().await;
        String::from_utf8(bytes.concat()).unwrap()
    }

    async fn collect_owned_with_context(
        chunks: Vec<String>,
        tool_context: CodexToolContext,
    ) -> String {
        let chunks: Vec<Result<Bytes, std::io::Error>> = chunks
            .into_iter()
            .map(|chunk| Ok(Bytes::from(chunk)))
            .collect();
        let upstream = stream::iter(chunks);
        let converted = create_responses_sse_stream_from_chat_with_context(upstream, tool_context);
        let bytes: Vec<Bytes> = converted.map(|item| item.unwrap()).collect().await;
        String::from_utf8(bytes.concat()).unwrap()
    }

    fn output_text_payloads(output: &str) -> Vec<String> {
        output
            .split("\n\n")
            .filter_map(|block| {
                let data = block.lines().find_map(|line| line.strip_prefix("data: "))?;
                let event: Value = serde_json::from_str(data).ok()?;
                match event.get("type").and_then(Value::as_str) {
                    Some("response.output_text.delta") => event
                        .get("delta")
                        .and_then(Value::as_str)
                        .map(ToString::to_string),
                    Some("response.output_text.done") => event
                        .get("text")
                        .and_then(Value::as_str)
                        .map(ToString::to_string),
                    _ => None,
                }
            })
            .collect()
    }

    fn glm_repair_context() -> CodexToolContext {
        let mut context = CodexToolContext::default();
        context.set_repair_unopened_think_blocks(true);
        context
    }

    #[tokio::test]
    async fn converts_text_chat_sse_to_responses_sse() {
        let output = collect(vec![
            "data: {\"id\":\"chatcmpl_1\",\"created\":123,\"model\":\"gpt-5.4\",\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
            "data: {\"id\":\"chatcmpl_1\",\"created\":123,\"model\":\"gpt-5.4\",\"choices\":[{\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":2,\"total_tokens\":6}}\n\n",
            "data: [DONE]\n\n",
        ])
        .await;

        assert!(output.contains("event: response.created"));
        assert!(output.contains("event: response.output_text.delta"));
        assert!(output.contains("\"text\":\"Hello\""));
        assert!(output.contains("event: response.completed"));
        assert!(output.contains("\"input_tokens\":4"));
    }

    #[tokio::test]
    async fn converts_reasoning_content_chat_sse_to_responses_reasoning_events() {
        let output = collect(vec![
            "data: {\"id\":\"chatcmpl_reason\",\"created\":123,\"model\":\"deepseek-reasoner\",\"choices\":[{\"delta\":{\"reasoning_content\":\"Need context. \"}}]}\n\n",
            "data: {\"id\":\"chatcmpl_reason\",\"created\":123,\"model\":\"deepseek-reasoner\",\"choices\":[{\"delta\":{\"reasoning\":\"Now answer. \"}}]}\n\n",
            "data: {\"id\":\"chatcmpl_reason\",\"created\":123,\"model\":\"deepseek-reasoner\",\"choices\":[{\"delta\":{\"content\":\"Done\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":6,\"total_tokens\":10,\"completion_tokens_details\":{\"reasoning_tokens\":3}}}\n\n",
            "data: [DONE]\n\n",
        ])
        .await;

        assert!(output.contains("event: response.reasoning_summary_part.added"));
        assert!(output.contains("event: response.reasoning_summary_text.delta"));
        assert!(output.contains("event: response.reasoning_summary_text.done"));
        assert!(output.contains("Need context. Now answer. "));
        assert!(output.contains("\"type\":\"reasoning\""));
        assert!(output.contains("\"text\":\"Done\""));
        assert!(output.contains("\"reasoning_tokens\":3"));

        let reasoning_pos = output.find("\"type\":\"reasoning\"").unwrap();
        let message_pos = output.find("\"type\":\"message\"").unwrap();
        assert!(reasoning_pos < message_pos);
    }

    #[tokio::test]
    async fn converts_inline_think_chat_sse_to_reasoning_without_leaking_tags() {
        let output = collect(vec![
            "data: {\"id\":\"chatcmpl_minimax\",\"created\":123,\"model\":\"MiniMax-M2.7\",\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"<think>\\nNeed\"}}]}\n\n",
            "data: {\"id\":\"chatcmpl_minimax\",\"created\":123,\"model\":\"MiniMax-M2.7\",\"choices\":[{\"delta\":{\"content\":\" context.</think>\\n\\npong\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: {\"id\":\"chatcmpl_minimax\",\"created\":123,\"model\":\"MiniMax-M2.7\",\"choices\":[],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":6,\"total_tokens\":10,\"completion_tokens_details\":{\"reasoning_tokens\":3}}}\n\n",
        ])
        .await;

        assert!(output.contains("event: response.reasoning_summary_text.delta"));
        assert!(output.contains("Need context."));
        assert!(output.contains("\"text\":\"pong\""));
        assert!(output.contains("\"reasoning_tokens\":3"));
        assert!(!output.contains("<think>"));
        assert!(!output.contains("</think>"));
        assert!(output.contains("event: response.completed"));
    }

    #[tokio::test]
    async fn converts_inline_thinking_chat_sse_to_reasoning_without_leaking_tags() {
        let output = collect(vec![
            "data: {\"id\":\"chatcmpl_glm\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"<thin\"}}]}\n\n",
            "data: {\"id\":\"chatcmpl_glm\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"content\":\"king>\\nNeed\"}}]}\n\n",
            "data: {\"id\":\"chatcmpl_glm\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"content\":\" context.</thinking>\\n\\npong\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        ])
        .await;

        assert!(output.contains("event: response.reasoning_summary_text.delta"));
        assert!(output.contains("Need context."));
        assert!(output.contains("\"text\":\"pong\""));
        assert!(!output.contains("<thinking>"));
        assert!(!output.contains("</thinking>"));
    }

    #[tokio::test]
    async fn converts_unopened_inline_think_chat_sse_to_reasoning_without_leaking_tags() {
        let output = collect_with_context(vec![
            "data: {\"id\":\"chatcmpl_glm_unopened\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"Need to inspect\"}}]}\n\n",
            "data: {\"id\":\"chatcmpl_glm_unopened\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"content\":\" the repo first.</think>The fix is ready.\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        ], glm_repair_context())
        .await;

        assert!(output.contains("event: response.reasoning_summary_text.delta"));
        assert!(output.contains("Need to inspect the repo first."));
        assert!(output.contains("\"text\":\"The fix is ready.\""));
        assert!(!output.contains("</think>"));
    }

    #[tokio::test]
    async fn converts_split_unopened_think_close_before_later_answer_chunks() {
        let output = collect_with_context(vec![
            "data: {\"id\":\"chatcmpl_glm_split_unopened\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"Need to inspect\"}}]}\n\n",
            "data: {\"id\":\"chatcmpl_glm_split_unopened\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"content\":\" the repo.</thi\"}}]}\n\n",
            "data: {\"id\":\"chatcmpl_glm_split_unopened\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"content\":\"nk>The fix\"}}]}\n\n",
            "data: {\"id\":\"chatcmpl_glm_split_unopened\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"content\":\" is ready.\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        ], glm_repair_context())
        .await;

        assert!(output.contains("event: response.reasoning_summary_text.delta"));
        assert!(output.contains("Need to inspect the repo."));
        assert!(output.contains("\"text\":\"The fix is ready.\""));
        assert!(!output.contains("</think>"));
    }

    #[tokio::test]
    async fn strips_orphan_leading_thinking_close_marker_from_streamed_text() {
        let output = collect_with_context(vec![
            "data: {\"id\":\"chatcmpl_glm_close\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"</think\"}}]}\n\n",
            "data: {\"id\":\"chatcmpl_glm_close\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"content\":\"ing>\\n\\npong\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        ], glm_repair_context())
        .await;

        assert!(output.contains("\"text\":\"pong\""));
        assert!(!output.contains("</thinking>"));
    }

    #[tokio::test]
    async fn keeps_long_raw_reasoning_out_of_visible_text_until_a_late_close() {
        let long_prefix = "a".repeat(65_536);
        let first = format!(
            "data: {{\"id\":\"chatcmpl_glm_late_close\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{{\"delta\":{{\"role\":\"assistant\",\"content\":\"{long_prefix}\"}}}}]}}\n\n"
        );

        let output = collect_with_context(vec![
            first.as_str(),
            "data: {\"id\":\"chatcmpl_glm_late_close\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"content\":\"</think> visible answer\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        ], glm_repair_context())
        .await;

        assert!(output.contains("event: response.reasoning_summary_text.delta"));
        assert!(output.contains("visible answer"));
        assert!(!output.contains("</think>"));
    }

    #[tokio::test]
    async fn recognizes_a_split_close_after_long_raw_reasoning() {
        let long_prefix = "a".repeat(65_536);
        let first = format!(
            "data: {{\"id\":\"chatcmpl_glm_split_late_close\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{{\"delta\":{{\"role\":\"assistant\",\"content\":\"{long_prefix}\"}}}}]}}\n\n"
        );

        let output = collect_with_context(vec![
            first.as_str(),
            "data: {\"id\":\"chatcmpl_glm_split_late_close\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"content\":\"</thi\"}}]}\n\n",
            "data: {\"id\":\"chatcmpl_glm_split_late_close\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"content\":\"nk> visible answer\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        ], glm_repair_context())
        .await;

        assert!(output.contains("visible answer"));
        assert!(!output.contains("</think>"));
        assert!(!output.contains("</thi"));
    }

    #[tokio::test]
    async fn preserves_an_incomplete_close_prefix_as_text_on_clean_stop() {
        let long_prefix = "a".repeat(65_536);
        let first = format!(
            "data: {{\"id\":\"chatcmpl_glm_incomplete_late_close\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{{\"delta\":{{\"role\":\"assistant\",\"content\":\"{long_prefix}\"}}}}]}}\n\n"
        );

        let output = collect_with_context(vec![
            first.as_str(),
            "data: {\"id\":\"chatcmpl_glm_incomplete_late_close\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"content\":\" </thi\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        ], glm_repair_context())
        .await;

        assert!(output.contains("</thi"));
        assert!(output.contains("event: response.output_text.done"));
        assert!(!output.contains("event: response.reasoning_summary_text.done"));
        assert!(output.contains("event: response.completed"));
    }

    #[test]
    fn raw_repair_buffers_ambiguous_content_before_boundary_evidence() {
        let mut state = ChatToResponsesState::with_tool_context(glm_repair_context());
        let events = state.handle_chat_chunk(&json!({
            "id": "chatcmpl_glm_immediate",
            "created": 123,
            "model": "glm-5.2",
            "choices": [{"delta": {"content": "Need to inspect now."}}]
        }));
        let output = String::from_utf8(events.into_iter().flatten().collect()).unwrap();
        assert!(!output.contains("event: response.reasoning_summary_text.delta"));
        assert!(!output.contains("event: response.output_text.delta"));
        assert_eq!(state.inline_think.buffer, "Need to inspect now.");
    }

    #[tokio::test]
    async fn raw_repair_preserves_literal_code_tag_pairs_as_answer_text() {
        let output = collect_with_context(
            vec![
                "data: {\"id\":\"chatcmpl_literal_code\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"content\":\"Here is the implementation:\\n```js\\nconst open = text.indexOf(\\\"\"}}]}\n\n",
                "data: {\"id\":\"chatcmpl_literal_code\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"content\":\"<think>\\\");\\nconst close = text.indexOf(\\\"</think>\\\");\\n```\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n",
            ],
            glm_repair_context(),
        )
        .await;

        assert!(output.contains("event: response.output_text.delta"));
        assert!(!output.contains("event: response.reasoning_summary_text.delta"));
        assert!(output.contains("Here is the implementation:"));
        assert!(output.contains(r#"text.indexOf(\"<think>\")"#));
        assert!(output.contains(r#"text.indexOf(\"</think>\")"#));
    }

    #[tokio::test]
    async fn raw_repair_preserves_marker_free_content_as_text_on_clean_stop() {
        let output = collect_with_context(
            vec![
                "data: {\"id\":\"chatcmpl_plain_stop\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"content\":\"The implementation is ready.\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n",
            ],
            glm_repair_context(),
        )
        .await;

        assert!(output.contains("event: response.output_text.delta"));
        assert!(output.contains("The implementation is ready."));
        assert!(!output.contains("event: response.reasoning_summary_text.delta"));
    }

    #[tokio::test]
    async fn structured_reasoning_streams_normal_answer_text_without_delay() {
        let output = collect_with_context(
            vec![
                "data: {\"id\":\"chatcmpl_structured\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"reasoning_content\":\"Reason first.\"}}]}\n\n",
                "data: {\"id\":\"chatcmpl_structured\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"content\":\"Answer now.\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n",
            ],
            glm_repair_context(),
        )
        .await;

        assert!(output.contains("Reason first."));
        assert!(output.contains("\"text\":\"Answer now.\""));
    }

    #[tokio::test]
    async fn structured_reasoning_preserves_balanced_answer_code_markers() {
        let content = "Use `</think>` literally.\n```text\n<thinking>example</thinking>\n```";
        let chunk = format!(
            "data: {{\"id\":\"chatcmpl_structured_code\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{{\"delta\":{{\"content\":{}}},\"finish_reason\":\"stop\"}}]}}\n\n",
            serde_json::to_string(content).unwrap()
        );
        let output = collect_with_context(
            vec![
                "data: {\"id\":\"chatcmpl_structured_code\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"reasoning_content\":\"Reasoning is complete.\"}}]}\n\n",
                chunk.as_str(),
                "data: [DONE]\n\n",
            ],
            glm_repair_context(),
        )
        .await;

        assert!(output_text_payloads(&output)
            .iter()
            .any(|text| text == content));
    }

    #[tokio::test]
    async fn content_only_diagnostic_code_literal_close_marker_remains_text() {
        let content = "The regression test `structured_reasoning_fails_late_single_visible_close_before_tool_call` confirms that a literal `</thinking>` before a tool call is quarantined. The `<thinking>` string here is diagnostic text, not a model delimiter.";
        let chunk = format!(
            "data: {{\"id\":\"chatcmpl_content_code_literal\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{{\"delta\":{{\"role\":\"assistant\",\"content\":{}}},\"finish_reason\":\"stop\"}}]}}\n\n",
            serde_json::to_string(content).unwrap()
        );
        let output = collect_with_context(
            vec![chunk.as_str(), "data: [DONE]\n\n"],
            glm_repair_context(),
        )
        .await;

        assert!(!output.contains("event: response.failed"));
        assert!(!output.contains("event: response.reasoning_summary_text.delta"));
        assert!(output_text_payloads(&output)
            .iter()
            .any(|text| text == content));
    }

    #[tokio::test]
    async fn structured_reasoning_fails_later_visible_close_after_stripping_leading_orphan_close() {
        let output = collect_with_context(
            vec![
                "data: {\"id\":\"chatcmpl_structured_close\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"reasoning_content\":\"Reason first.\"}}]}\n\n",
                "data: {\"id\":\"chatcmpl_structured_close\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"content\":\"</thi\"}}]}\n\n",
                "data: {\"id\":\"chatcmpl_structured_close\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"content\":\"nk>Answer with literal </think> text.\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n",
            ],
            glm_repair_context(),
        )
        .await;

        assert!(output.contains("event: response.failed"));
        assert!(output.contains("upstream_reasoning_marker_leak"));
        assert!(!output.contains("Answer with literal </think> text."));
        assert!(!output.contains("\"delta\":\"</think>"));
    }

    #[tokio::test]
    async fn structured_reasoning_strips_late_single_visible_close_before_tool_call() {
        let prefix = "The event stream now shows command executions and file changes. ".repeat(20);
        let answer = format!("{prefix}Let me check the command execution details.");
        let content = format!("{answer}</think>");
        let content_chunk = format!(
            "data: {}\n\n",
            json!({
                "id": "chatcmpl_late_visible_close",
                "created": 123,
                "model": "glm-5.2",
                "choices": [{"delta": {"content": content}}],
            })
        );
        let output = collect_owned_with_context(
            vec![
                "data: {\"id\":\"chatcmpl_late_visible_close\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"reasoning_content\":\"Inspect the event stream.\"}}]}\n\n".to_string(),
                content_chunk,
                "data: {\"id\":\"chatcmpl_late_visible_close\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_bad\",\"type\":\"function\",\"function\":{\"name\":\"exec_command\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n".to_string(),
                "data: [DONE]\n\n".to_string(),
            ],
            glm_repair_context(),
        )
        .await;

        assert!(!output.contains("event: response.failed"));
        assert!(!output.contains("</think>"));
        assert!(output_text_payloads(&output)
            .iter()
            .any(|text| text == &answer));
        assert!(output.contains("call_bad"));
    }

    #[tokio::test]
    async fn structured_reasoning_strips_repeated_leading_orphan_close_markers_before_answer() {
        let output = collect_with_context(
            vec![
                "data: {\"id\":\"chatcmpl_structured_repeated_close\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"reasoning_content\":\"The test finished; prepare the summary.\"}}]}\n\n",
                "data: {\"id\":\"chatcmpl_structured_repeated_close\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"content\":\"</think></think>The test is making much better progress now.\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n",
            ],
            glm_repair_context(),
        )
        .await;

        assert_eq!(
            output
                .matches("\"delta\":\"The test is making much better progress now.\"")
                .count(),
            1
        );
        assert!(output.contains("\"text\":\"The test is making much better progress now.\""));
        assert!(!output.contains("event: response.failed"));
        assert!(!output.contains("</think>"));
    }

    #[tokio::test]
    async fn suppresses_single_tool_call_placeholder_reasoning() {
        let output = collect_with_context(
            vec![
                "data: {\"id\":\"chatcmpl_reasoning_loop\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"reasoning_content\":\"tool\"}}]}\n\n",
                "data: {\"id\":\"chatcmpl_reasoning_loop\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"reasoning_content\":\" call\"}}]}\n\n",
                "data: {\"id\":\"chatcmpl_reasoning_loop\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call_good\",\"index\":0,\"type\":\"function\",\"function\":{\"name\":\"exec_command\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                "data: [DONE]\n\n",
            ],
            glm_repair_context(),
        )
        .await;

        assert!(!output.contains("event: response.failed"));
        assert!(!output.contains("\"delta\":\"tool\""));
        assert!(!output.contains("\"text\":\"tool call\""));
        assert!(output.contains("response.function_call_arguments.delta"));
        assert!(output.contains("call_good"));
    }

    #[tokio::test]
    async fn suppresses_tool_placeholder_prefix_at_tool_boundary_without_leaking_delta() {
        let output = collect_with_context(
            vec![
                "data: {\"id\":\"chatcmpl_reasoning_prefix\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"reasoning_content\":\"tool\"}}]}\n\n",
                "data: {\"id\":\"chatcmpl_reasoning_prefix\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call_good\",\"index\":0,\"type\":\"function\",\"function\":{\"name\":\"exec_command\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                "data: [DONE]\n\n",
            ],
            glm_repair_context(),
        )
        .await;

        assert!(!output.contains("event: response.failed"));
        assert!(!output.contains("\"delta\":\"tool\""));
        assert!(!output.contains("\"text\":\"tool\""));
        assert!(output.contains("response.function_call_arguments.delta"));
        assert!(output.contains("call_good"));
    }

    #[tokio::test]
    async fn suppresses_single_reasoning_marker_fragment_without_visible_delta() {
        let output = collect_with_context(
            vec![
                "data: {\"id\":\"chatcmpl_reasoning_marker_once\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"reasoning_content\":\"</thinking\"}}]}\n\n",
                "data: {\"id\":\"chatcmpl_reasoning_marker_once\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"reasoning_content\":\"Need to inspect the trace.\"}}]}\n\n",
                "data: {\"id\":\"chatcmpl_reasoning_marker_once\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"content\":\"Done.\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n",
            ],
            glm_repair_context(),
        )
        .await;

        assert!(!output.contains("event: response.failed"));
        assert!(!output.contains("</thinking"));
        assert!(output.contains("Need to inspect the trace."));
        assert!(output.contains("\"text\":\"Done.\""));
    }

    #[tokio::test]
    async fn fails_repeated_reasoning_marker_fragments_without_leaking_them() {
        let mut chunks = Vec::new();
        for _ in 0..REASONING_GUARD_MIN_REPEATED_MARKER_FRAGMENTS {
            chunks.push(
                "data: {\"id\":\"chatcmpl_reasoning_marker_loop\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"reasoning_content\":\"</thinking\"}}]}\n\n",
            );
        }
        chunks.push(
            "data: {\"id\":\"chatcmpl_reasoning_marker_loop\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call_bad\",\"index\":0,\"type\":\"function\",\"function\":{\"name\":\"exec_command\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
        );
        chunks.push("data: [DONE]\n\n");

        let output = collect_with_context(chunks, glm_repair_context()).await;

        assert!(output.contains("event: response.failed"));
        assert!(output.contains("upstream_reasoning_marker_loop"));
        assert!(output.contains(
            "Upstream response repeated raw reasoning delimiter fragments inside reasoning output"
        ));
        assert!(!output.contains("</thinking"));
        assert!(!output.contains("call_bad"));
    }

    #[tokio::test]
    async fn allows_sentence_starting_with_tool_prefix_in_reasoning() {
        let output = collect_with_context(
            vec![
                "data: {\"id\":\"chatcmpl_reasoning_tool_sentence\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"reasoning_content\":\"tool\"}}]}\n\n",
                "data: {\"id\":\"chatcmpl_reasoning_tool_sentence\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"reasoning_content\":\" use is appropriate here.\"}}]}\n\n",
                "data: {\"id\":\"chatcmpl_reasoning_tool_sentence\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call_good\",\"index\":0,\"type\":\"function\",\"function\":{\"name\":\"exec_command\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                "data: [DONE]\n\n",
            ],
            glm_repair_context(),
        )
        .await;

        assert!(!output.contains("event: response.failed"));
        assert!(output.contains("\"delta\":\"tool use is appropriate here.\""));
        assert!(output.contains("call_good"));
    }

    #[tokio::test]
    async fn allows_sentence_that_mentions_tool_call_in_reasoning() {
        let output = collect_with_context(
            vec![
                "data: {\"id\":\"chatcmpl_reasoning_sentence\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"reasoning_content\":\"I will make a tool call to inspect the file.\"}}]}\n\n",
                "data: {\"id\":\"chatcmpl_reasoning_sentence\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call_good\",\"index\":0,\"type\":\"function\",\"function\":{\"name\":\"exec_command\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                "data: [DONE]\n\n",
            ],
            glm_repair_context(),
        )
        .await;

        assert!(!output.contains("event: response.failed"));
        assert!(output.contains("response.function_call_arguments.delta"));
        assert!(output.contains("call_good"));
    }

    #[tokio::test]
    async fn allows_split_tool_call_sentence_before_tool_call() {
        let output = collect_with_context(
            vec![
                "data: {\"id\":\"chatcmpl_reasoning_split_sentence\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"reasoning_content\":\"tool\"}}]}\n\n",
                "data: {\"id\":\"chatcmpl_reasoning_split_sentence\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"reasoning_content\":\" call\"}}]}\n\n",
                "data: {\"id\":\"chatcmpl_reasoning_split_sentence\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"reasoning_content\":\", let me proceed with the tool calls.\"}}]}\n\n",
                "data: {\"id\":\"chatcmpl_reasoning_split_sentence\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call_good\",\"index\":0,\"type\":\"function\",\"function\":{\"name\":\"exec_command\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                "data: [DONE]\n\n",
            ],
            glm_repair_context(),
        )
        .await;

        assert!(!output.contains("event: response.failed"));
        assert!(output.contains("\"delta\":\"tool call, let me proceed with the tool calls.\""));
        assert!(output.contains("response.function_call_arguments.delta"));
        assert!(output.contains("call_good"));
    }

    #[tokio::test]
    async fn structured_reasoning_fails_repeated_visible_orphan_close_markers() {
        let output = collect_with_context(
            vec![
                "data: {\"id\":\"chatcmpl_structured_pathological\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"reasoning_content\":\" ming strategy a\"}}]}\n\n",
                "data: {\"id\":\"chatcmpl_structured_pathological\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"content\":\" andassertTrue's grab</think>.\\n\\n\"}}]}\n\n",
                "data: {\"id\":\"chatcmpl_structured_pathological\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"content\":\"</think></think> Args: application-license\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n",
            ],
            glm_repair_context(),
        )
        .await;

        assert!(output.contains("event: response.failed"));
        assert!(output.contains("upstream_reasoning_marker_leak"));
        assert!(!output.contains("event: response.output_text.delta"));
        assert!(!output.contains("andassertTrue"));
    }

    #[tokio::test]
    async fn structured_desync_repairs_dangling_code_literal_until_last_close() {
        let reasoning = "Interesting findings:\n\
1. **1250 downstream bodies contain `<thinking>` or `</thinking>` or `` markers** — this is a significant finding!\n\
2. **1 downstream body contains U+FFFD**\n\
3. **44 upstream bodies contain `<thinking>` or `</thinking>` markers in server-side request logs**";
        let content = "` markers** — the upstream is sending raw thinking markers.\n\
\n\
Wait, let me re-examine this. The downstream response body contains `<thinking>` markers in 1250 traces.\n\
\n\
Actually, the 1250 count includes `</think>` markers. Let me check which specific markers are appearing.\n\
The upstream model may be outputting `<think>...</think>` or `<thinking>...</thinking>` tags.\n\
\n\
Let me look at some actual examples:\n\
1. How many have `<thinking>` specifically\n\
2. How many have `</thinking>` specifically\n\
3. How many have `</think>` specifically\n\
4. Sample some actual downstream bodies with these markers</think>Significant finding.";

        let output = collect_owned_with_context(
            vec![
                format!(
                    "data: {}\n\n",
                    json!({
                        "id": "chatcmpl_structured_last_close",
                        "created": 123,
                        "model": "glm-5.2",
                        "choices": [{"delta": {"reasoning_content": reasoning}}],
                    })
                ),
                format!(
                    "data: {}\n\n",
                    json!({
                        "id": "chatcmpl_structured_last_close",
                        "created": 123,
                        "model": "glm-5.2",
                        "choices": [{"delta": {"content": content}, "finish_reason": "stop"}],
                    })
                ),
                "data: [DONE]\n\n".to_string(),
            ],
            glm_repair_context(),
        )
        .await;

        let visible_text = output_text_payloads(&output).join("");
        assert!(!output.contains("event: response.failed"));
        assert!(output.contains("event: response.completed"));
        assert!(visible_text.contains("Significant finding."));
        assert!(!visible_text.contains("Wait, let me re-examine"));
        assert!(!visible_text.contains("</think>"));
    }

    #[tokio::test]
    async fn repairs_structured_desync_after_markdown_literal_close() {
        let output = collect_with_context(
            vec![
                "data: {\"id\":\"chatcmpl_desync\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"reasoning_content\":\"Inspect `markers: ['\"}}]}\n\n",
                "data: {\"id\":\"chatcmpl_desync\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"content\":\"', '</thinking>', '<think>', '<thinking>']` then continue.\"}}]}\n\n",
                "data: {\"id\":\"chatcmpl_desync\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"content\":\"</thi\"}}]}\n\n",
                "data: {\"id\":\"chatcmpl_desync\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"content\":\"nk>Final answer.\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n",
            ],
            glm_repair_context(),
        )
        .await;

        let visible = output_text_payloads(&output);
        assert!(visible.iter().any(|text| text == "Final answer."));
        assert!(visible.iter().all(|text| !text.contains("</think>")));
        assert!(visible.iter().all(|text| !text.contains("</thinking>")));
        assert!(output.contains("then continue."));
    }

    #[tokio::test]
    async fn repairs_structured_desync_with_thinking_close_variant() {
        let output = collect_with_context(
            vec![
                "data: {\"id\":\"chatcmpl_desync_thinking\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"reasoning_content\":\"Inspect ```text\\nmarkers: \"}}]}\n\n",
                "data: {\"id\":\"chatcmpl_desync_thinking\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"content\":\"literal\\n``` then continue.</thin\"}}]}\n\n",
                "data: {\"id\":\"chatcmpl_desync_thinking\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"content\":\"king>Answer.\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n",
            ],
            glm_repair_context(),
        )
        .await;

        let visible = output_text_payloads(&output);
        assert!(visible.iter().any(|text| text == "Answer."));
        assert!(visible.iter().all(|text| !text.contains("</thinking>")));
    }

    #[tokio::test]
    async fn structured_desync_without_late_close_falls_back_to_original_text() {
        let content = "']` is the complete literal-code answer.";
        let chunk = format!(
            "data: {{\"id\":\"chatcmpl_desync_fallback\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{{\"delta\":{{\"content\":{}}},\"finish_reason\":\"stop\"}}]}}\n\n",
            serde_json::to_string(content).unwrap()
        );
        let output = collect_with_context(
            vec![
                "data: {\"id\":\"chatcmpl_desync_fallback\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"reasoning_content\":\"Inspect `markers: ['\"}}]}\n\n",
                chunk.as_str(),
                "data: [DONE]\n\n",
            ],
            glm_repair_context(),
        )
        .await;

        assert!(output_text_payloads(&output)
            .iter()
            .any(|text| text == content));
    }

    #[tokio::test]
    async fn structured_desync_with_unprotected_opener_falls_back_unchanged() {
        let content = "']` then show <think>literal</think> text.";
        let chunk = format!(
            "data: {{\"id\":\"chatcmpl_desync_open\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{{\"delta\":{{\"content\":{}}},\"finish_reason\":\"stop\"}}]}}\n\n",
            serde_json::to_string(content).unwrap()
        );
        let output = collect_with_context(
            vec![
                "data: {\"id\":\"chatcmpl_desync_open\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"reasoning_content\":\"Inspect `markers: ['\"}}]}\n\n",
                chunk.as_str(),
                "data: [DONE]\n\n",
            ],
            glm_repair_context(),
        )
        .await;

        assert!(output_text_payloads(&output)
            .iter()
            .any(|text| text == content));
    }

    #[tokio::test]
    async fn structured_desync_without_close_falls_back_at_tool_boundary() {
        let content = "']` is literal pre-tool text.";
        let chunk = format!(
            "data: {{\"id\":\"chatcmpl_desync_tool\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{{\"delta\":{{\"content\":{}}}}}]}}\n\n",
            serde_json::to_string(content).unwrap()
        );
        let output = collect_with_context(
            vec![
                "data: {\"id\":\"chatcmpl_desync_tool\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"reasoning_content\":\"Inspect `markers: ['\"}}]}\n\n",
                chunk.as_str(),
                "data: {\"id\":\"chatcmpl_desync_tool\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"inspect\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                "data: [DONE]\n\n",
            ],
            glm_repair_context(),
        )
        .await;

        assert!(output_text_payloads(&output)
            .iter()
            .any(|text| text == content));
        assert!(output.contains("\"type\":\"function_call\""));
        assert!(output.contains("\"name\":\"inspect\""));
    }

    #[tokio::test]
    async fn preserves_unopened_close_marker_without_repair_context() {
        let output = collect(vec![
            "data: {\"id\":\"chatcmpl_generic_close\",\"created\":123,\"model\":\"generic\",\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"literal </think> marker\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        ])
        .await;

        assert!(output.contains("literal </think> marker"));
    }

    #[test]
    fn unopened_think_repair_is_opt_in_for_prefix_detection() {
        assert!(matches!(
            leading_think_prefix_decision("Need to inspect", false),
            ThinkPrefixDecision::Text
        ));
        assert!(matches!(
            leading_think_prefix_decision("Need to inspect</think>Done", false),
            ThinkPrefixDecision::Text
        ));
        assert!(matches!(
            leading_think_prefix_decision("Need to inspect</think>Done", true),
            ThinkPrefixDecision::RawReasoning
        ));
    }

    #[tokio::test]
    async fn converts_tool_call_chat_sse_to_responses_sse() {
        let output = collect(vec![
            "data: {\"id\":\"chatcmpl_2\",\"model\":\"gpt-5.4\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\"}}]}}]}\n\n",
            "data: {\"id\":\"chatcmpl_2\",\"model\":\"gpt-5.4\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"city\\\":\\\"Tokyo\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        ])
        .await;

        assert!(output.contains("event: response.function_call_arguments.delta"));
        assert!(output.contains("event: response.function_call_arguments.done"));
        assert!(output.contains("\"type\":\"function_call\""));
        assert!(output.contains("\"call_id\":\"call_1\""));
    }

    #[tokio::test]
    async fn restores_custom_tool_input_stream_events() {
        let request = json!({
            "model": "gpt-5.4",
            "tools": [{ "type": "custom", "name": "exec" }]
        });
        let context =
            super::super::transform_codex_chat::build_codex_tool_context_from_request(&request);
        let output = collect_with_context(
            vec![
                "data: {\"id\":\"chatcmpl_custom\",\"model\":\"gpt-5.4\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_custom\",\"type\":\"function\",\"function\":{\"name\":\"exec\"}}]}}]}\n\n",
                "data: {\"id\":\"chatcmpl_custom\",\"model\":\"gpt-5.4\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"input\\\":\"}}]}}]}\n\n",
                "data: {\"id\":\"chatcmpl_custom\",\"model\":\"gpt-5.4\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"ls -la\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                "data: [DONE]\n\n",
            ],
            context,
        )
        .await;

        assert!(output.contains("event: response.custom_tool_call_input.delta"));
        assert!(output.contains("event: response.custom_tool_call_input.done"));
        assert!(!output.contains("event: response.function_call_arguments.delta"));
        assert!(!output.contains("event: response.function_call_arguments.done"));
        assert!(output.contains("\"id\":\"ctc_call_custom\""));
        assert!(output.contains("\"type\":\"custom_tool_call\""));
        assert!(output.contains("\"name\":\"exec\""));
        assert!(output.contains("\"input\":\"ls -la\""));
    }

    #[tokio::test]
    async fn canonicalizes_streamed_tool_call_arguments_on_done_events() {
        let output = collect(vec![
            "data: {\"id\":\"chatcmpl_args\",\"model\":\"gpt-5.4\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"lookup\"}}]}}]}\n\n",
            "data: {\"id\":\"chatcmpl_args\",\"model\":\"gpt-5.4\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{ \\\"b\\\": 2,\"}}]}}]}\n\n",
            "data: {\"id\":\"chatcmpl_args\",\"model\":\"gpt-5.4\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\" \\\"a\\\": 1 }\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        ])
        .await;

        assert!(output.contains(r#""arguments":"{\"a\":1,\"b\":2}""#));
    }

    #[tokio::test]
    async fn repairs_leading_orphan_think_close_in_streamed_tool_arguments() {
        let output = collect_with_context(
            vec![
                "data: {\"id\":\"chatcmpl_tool_close\",\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"read_file\"}}]}}]}\n\n",
                "data: {\"id\":\"chatcmpl_tool_close\",\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"</think>{\\\"path\\\":\\\"README.md\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                "data: [DONE]\n\n",
            ],
            glm_repair_context(),
        )
        .await;

        assert!(!output.contains("</think>"));
        assert!(output.contains(r#""delta":"{\"path\":\"README.md\"}""#));
        assert!(output.contains(r#""arguments":"{\"path\":\"README.md\"}""#));
    }

    #[tokio::test]
    async fn repairs_split_leading_orphan_think_close_in_streamed_tool_arguments() {
        let output = collect_with_context(
            vec![
                "data: {\"id\":\"chatcmpl_tool_split_close\",\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"read_file\"}}]}}]}\n\n",
                "data: {\"id\":\"chatcmpl_tool_split_close\",\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"</thi\"}}]}}]}\n\n",
                "data: {\"id\":\"chatcmpl_tool_split_close\",\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"nk>{\\\"path\\\":\\\"README.md\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                "data: [DONE]\n\n",
            ],
            glm_repair_context(),
        )
        .await;

        assert!(!output.contains("</think>"));
        assert!(output.contains(r#""delta":"{\"path\":\"README.md\"}""#));
        assert!(output.contains(r#""arguments":"{\"path\":\"README.md\"}""#));
    }

    #[tokio::test]
    async fn repairs_glm_alias_in_streamed_tool_arguments_and_reasoning() {
        let alias = glm_think_open_alias_char();
        let reasoning_chunk = format!(
            "data: {{\"id\":\"chatcmpl_glm_alias\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{{\"delta\":{{\"reasoning_content\":\"Need literal {alias} syntax.\"}}}}]}}\n\n"
        );
        let tool_args_chunk = format!(
            "data: {{\"id\":\"chatcmpl_glm_alias\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"function\":{{\"arguments\":\"{{\\\"text\\\":\\\"{alias}\\\"}}\"}}}}]}},\"finish_reason\":\"tool_calls\"}}]}}\n\n"
        );
        let output = collect_with_context(
            vec![
                reasoning_chunk.as_str(),
                "data: {\"id\":\"chatcmpl_glm_alias\",\"created\":123,\"model\":\"glm-5.2\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"edit_file\"}}]}}]}\n\n",
                tool_args_chunk.as_str(),
                "data: [DONE]\n\n",
            ],
            glm_repair_context(),
        )
        .await;

        assert!(!output.contains(alias));
        assert!(output.contains("Need literal  syntax."));
        assert!(output.contains(r#""delta":"{\"text\":\"<think>\"}""#));
        assert!(output.contains(r#""arguments":"{\"text\":\"<think>\"}""#));
    }

    #[tokio::test]
    async fn preserves_reasoning_content_on_streamed_tool_call_items() {
        let output = collect(vec![
            "data: {\"id\":\"chatcmpl_tool_reasoning\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"delta\":{\"reasoning_content\":\"Need file.\"}}]}\n\n",
            "data: {\"id\":\"chatcmpl_tool_reasoning\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"read_file\"}}]}}]}\n\n",
            "data: {\"id\":\"chatcmpl_tool_reasoning\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        ])
        .await;

        assert!(output.contains("event: response.output_item.done"));
        assert!(output.contains("\"type\":\"function_call\""));
        assert!(output.contains("\"reasoning_content\":\"Need file.\""));
    }

    #[tokio::test]
    async fn preserves_late_reasoning_content_on_streamed_tool_call_items() {
        let output = collect(vec![
            "data: {\"id\":\"chatcmpl_tool_late_reasoning\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"read_file\"}}]}}]}\n\n",
            "data: {\"id\":\"chatcmpl_tool_late_reasoning\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}}]}}]}\n\n",
            "data: {\"id\":\"chatcmpl_tool_late_reasoning\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"delta\":{\"reasoning_content\":\"Need file.\"}}]}\n\n",
            "data: {\"id\":\"chatcmpl_tool_late_reasoning\",\"model\":\"deepseek-v4-flash\",\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        ])
        .await;

        assert!(output.contains("event: response.output_item.done"));
        assert!(output.contains("\"type\":\"function_call\""));
        assert!(output.contains("\"reasoning_content\":\"Need file.\""));
    }

    #[tokio::test]
    async fn restores_namespace_on_streamed_tool_call_items() {
        let request = json!({
            "model": "gpt-5.4",
            "input": [{
                "type": "tool_search_output",
                "call_id": "call_tool_search_1",
                "tools": [{
                    "type": "namespace",
                    "name": "mcp__codex_apps__gmail",
                    "tools": [{
                        "type": "function",
                        "name": "_search_emails",
                        "description": "Search Gmail.",
                        "parameters": {"type": "object"}
                    }]
                }]
            }]
        });
        let context =
            super::super::transform_codex_chat::build_codex_tool_context_from_request(&request);
        let output = collect_with_context(
            vec![
                "data: {\"id\":\"chatcmpl_gmail\",\"model\":\"gpt-5.4\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_gmail\",\"type\":\"function\",\"function\":{\"name\":\"mcp__codex_apps__gmail___search_emails\"}}]}}]}\n\n",
                "data: {\"id\":\"chatcmpl_gmail\",\"model\":\"gpt-5.4\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"query\\\":\\\"in:inbox\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                "data: [DONE]\n\n",
            ],
            context,
        )
        .await;

        assert!(output.contains("\"type\":\"function_call\""));
        assert!(output.contains("\"namespace\":\"mcp__codex_apps__gmail\""));
        assert!(output.contains("\"name\":\"_search_emails\""));
        assert!(output.contains(r#""arguments":"{\"query\":\"in:inbox\"}""#));
    }

    #[tokio::test]
    async fn restores_tool_search_on_streamed_tool_call_items() {
        let request = json!({
            "model": "gpt-5.4",
            "tools": [{"type": "tool_search"}],
            "input": "Search for Gmail tools."
        });
        let context =
            super::super::transform_codex_chat::build_codex_tool_context_from_request(&request);
        let output = collect_with_context(
            vec![
                "data: {\"id\":\"chatcmpl_tool_search\",\"model\":\"gpt-5.4\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_tool_search_1\",\"type\":\"function\",\"function\":{\"name\":\"tool_search\"}}]}}]}\n\n",
                "data: {\"id\":\"chatcmpl_tool_search\",\"model\":\"gpt-5.4\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"query\\\":\\\"Gmail search emails\\\",\\\"limit\\\":10}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
                "data: [DONE]\n\n",
            ],
            context,
        )
        .await;

        assert!(output.contains("\"type\":\"tool_search_call\""));
        assert!(output.contains("\"execution\":\"client\""));
        assert!(output.contains("\"call_id\":\"call_tool_search_1\""));
        assert!(output.contains("\"query\":\"Gmail search emails\""));
    }

    #[tokio::test]
    async fn stream_error_emits_failed_without_completed() {
        let upstream = stream::iter(vec![Err::<Bytes, std::io::Error>(std::io::Error::other(
            "boom",
        ))]);
        let converted = create_responses_sse_stream_from_chat(upstream);
        let bytes: Vec<Bytes> = converted.map(|item| item.unwrap()).collect().await;
        let output = String::from_utf8(bytes.concat()).unwrap();

        assert!(output.contains("event: response.failed"));
        assert!(!output.contains("event: response.completed"));
    }

    #[tokio::test]
    async fn stream_end_with_output_without_finish_reason_emits_incomplete_without_failed() {
        let output = collect(vec![
            "data: {\"id\":\"chatcmpl_truncated\",\"model\":\"gpt-5.4\",\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
        ])
        .await;

        assert!(output.contains("event: response.completed"));
        assert!(output.contains("\"status\":\"incomplete\""));
        assert!(output.contains("\"incomplete_details\":{\"reason\":\"max_output_tokens\"}"));
        assert!(!output.contains("event: response.failed"));
    }

    #[tokio::test]
    async fn stream_end_without_output_or_finish_reason_emits_failed_without_completed() {
        let output = collect(vec![
            "data: {\"id\":\"chatcmpl_truncated\",\"model\":\"gpt-5.4\",\"choices\":[{\"delta\":{}}]}\n\n",
        ])
        .await;

        assert!(output.contains("event: response.failed"));
        assert!(output.contains("stream_truncated"));
        assert!(!output.contains("event: response.completed"));
    }

    #[tokio::test]
    async fn chat_sse_error_event_emits_failed_without_completed() {
        let output = collect(vec![
            "event: error\ndata: {\"error\":{\"message\":\"bad request\",\"type\":\"invalid_request_error\"}}\n\n",
            "data: [DONE]\n\n",
        ])
        .await;

        assert!(output.contains("event: response.failed"));
        assert!(output.contains("bad request"));
        assert!(output.contains("invalid_request_error"));
        assert!(!output.contains("event: response.completed"));
    }

    #[tokio::test]
    async fn chat_sse_data_only_error_emits_failed_without_completed() {
        let output = collect(vec![
            "data: {\"error\":{\"message\":\"quota exceeded\",\"code\":\"rate_limit_exceeded\"}}\n\n",
            "data: [DONE]\n\n",
        ])
        .await;

        assert!(output.contains("event: response.failed"));
        assert!(output.contains("quota exceeded"));
        assert!(output.contains("rate_limit_exceeded"));
        assert!(!output.contains("event: response.completed"));
    }
}
