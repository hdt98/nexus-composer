use super::reasoning_boundary::{BoundaryOutput, Field, ReasoningBoundary};
use crate::provider::Provider;
use serde_json::{json, Value};
use std::{collections::BTreeMap, io};

// Provisional content cannot be emitted safely before its boundary is known, so
// callers need an idle timeout long enough for at most this bounded interval.
const MAX_PENDING_BYTES: usize = 8 * 1024 * 1024;
const MAX_CHOICES: usize = 128;
const CLOSE_TAGS: [&str; 2] = ["</think>", "</thinking>"];

pub(crate) fn enabled_for_attempt(provider: &Provider, body: &Value) -> bool {
    enabled_for_request(
        provider,
        body.pointer("/chat_template_kwargs/enable_thinking") == Some(&Value::Bool(true)),
    )
}

pub(crate) fn enabled_for_request(provider: &Provider, request_enabled: bool) -> bool {
    let Some(meta) = provider
        .meta
        .as_ref()
        .filter(|meta| meta.provider_type.as_deref() == Some("nexus"))
    else {
        return false;
    };
    meta.local_proxy_request_overrides
        .as_ref()
        .and_then(|overrides| overrides.body.as_ref())
        .and_then(|body| body.pointer("/chat_template_kwargs/enable_thinking"))
        .map_or(request_enabled, |value| value == &Value::Bool(true))
}

#[derive(Default)]
struct ToolArgumentPrefix {
    buffered: String,
    complete: String,
    resolved: bool,
    marker: bool,
}

impl ToolArgumentPrefix {
    fn push(&mut self, delta: &str, terminal: bool) -> io::Result<String> {
        if self.resolved {
            return Ok(delta.to_string());
        }
        self.buffered.push_str(delta);
        let trimmed = self.buffered.trim_start();
        if trimmed.is_empty() {
            if terminal {
                self.resolved = true;
                return Ok(std::mem::take(&mut self.buffered));
            }
            return Ok(String::new());
        }
        if let Some(tag) = CLOSE_TAGS.iter().find(|tag| trimmed.starts_with(**tag)) {
            let leading = self.buffered.len() - trimmed.len();
            let mut repaired = self.buffered[..leading].to_string();
            repaired.push_str(&self.buffered[leading + tag.len()..]);
            self.buffered.clear();
            self.resolved = true;
            self.marker = true;
            return Ok(repaired);
        }
        if CLOSE_TAGS.iter().any(|tag| tag.starts_with(trimmed)) {
            if terminal {
                return Err(invalid_data(
                    "incomplete reasoning marker in tool arguments",
                ));
            }
            return Ok(String::new());
        }
        self.resolved = true;
        Ok(std::mem::take(&mut self.buffered))
    }

    fn is_pending(&self) -> bool {
        !self.resolved
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ToolIndex {
    Modern(u64),
    Legacy,
}

#[derive(Default)]
pub(crate) struct ChatBoundaryNormalizer {
    channels: BTreeMap<u64, ReasoningBoundary>,
    tool_arguments: BTreeMap<(u64, ToolIndex), ToolArgumentPrefix>,
}

impl ChatBoundaryNormalizer {
    pub(crate) fn normalize_chunk(&mut self, value: &mut Value) -> io::Result<()> {
        self.apply(value, false)
    }

    pub(crate) fn finish(&self) -> io::Result<()> {
        if self
            .tool_arguments
            .values()
            .any(ToolArgumentPrefix::is_pending)
        {
            return Err(invalid_data(
                "incomplete reasoning marker in tool arguments",
            ));
        }
        if self.channels.is_empty() || self.channels.values().any(|boundary| !boundary.is_done()) {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "upstream Chat SSE ended before finish_reason",
            ));
        }
        Ok(())
    }

    fn apply(&mut self, value: &mut Value, complete: bool) -> io::Result<()> {
        let Some(choices) = value.get_mut("choices").and_then(Value::as_array_mut) else {
            return Ok(());
        };
        let require_indices = !complete && choices.len() > 1;
        for (position, choice) in choices.iter_mut().enumerate() {
            let index = match choice.get("index").and_then(Value::as_u64) {
                Some(index) => index,
                None if require_indices => {
                    return Err(invalid_data("multi-choice Chat SSE omitted choice index"));
                }
                None => position as u64,
            };
            if !self.channels.contains_key(&index) && self.channels.len() >= MAX_CHOICES {
                return Err(invalid_data("Chat SSE exceeded choice limit"));
            }

            let terminal = complete || choice.get("finish_reason").is_some_and(|v| !v.is_null());
            let payload_key = if complete { "message" } else { "delta" };
            let has_tool = choice.get(payload_key).is_some_and(|payload| {
                payload
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .is_some_and(|calls| !calls.is_empty())
                    || payload
                        .get("function_call")
                        .is_some_and(|value| !value.is_null())
            });
            let tool_boundary =
                self.normalize_tool_arguments(choice, payload_key, index, !complete, terminal)?;
            if terminal {
                self.validate_tool_arguments(index)?;
            }
            let boundary = self
                .channels
                .entry(index)
                .or_insert_with(|| ReasoningBoundary::new(MAX_PENDING_BYTES));
            let mut output = take_fields(choice, payload_key, boundary)?;
            if let (true, Some(tool_marker)) = (has_tool, tool_boundary) {
                output.append(
                    boundary
                        .commit_tool_boundary(tool_marker)
                        .map_err(invalid_data)?,
                );
            }
            if terminal {
                output.append(boundary.finish().map_err(invalid_data)?);
            }
            put_fields(choice, payload_key, output)?;
            if self
                .pending_bytes()
                .is_none_or(|bytes| bytes > MAX_PENDING_BYTES)
            {
                return Err(invalid_data("Chat SSE reasoning buffer limit exceeded"));
            }
        }
        Ok(())
    }

    fn normalize_tool_arguments(
        &mut self,
        choice: &mut Value,
        payload_key: &str,
        choice_index: u64,
        streaming: bool,
        terminal: bool,
    ) -> io::Result<Option<bool>> {
        let Some(payload) = choice.get_mut(payload_key).and_then(Value::as_object_mut) else {
            return self.tool_boundary(choice_index, terminal);
        };
        if let Some(calls) = payload.get_mut("tool_calls").and_then(Value::as_array_mut) {
            for (position, call) in calls.iter_mut().enumerate() {
                let index = match call.get("index").and_then(Value::as_u64) {
                    Some(index) => index,
                    None if streaming => {
                        return Err(invalid_data("Chat SSE tool call omitted index"));
                    }
                    None => position as u64,
                };
                let key = (choice_index, ToolIndex::Modern(index));
                self.ensure_tool_state(key)?;
                if let Some(arguments) = call.pointer_mut("/function/arguments") {
                    self.repair_tool_argument(key, arguments, terminal)?;
                }
            }
        }
        if let Some(function) = payload
            .get_mut("function_call")
            .and_then(Value::as_object_mut)
        {
            let key = (choice_index, ToolIndex::Legacy);
            self.ensure_tool_state(key)?;
            if let Some(arguments) = function.get_mut("arguments") {
                self.repair_tool_argument(key, arguments, terminal)?;
            }
        }
        self.tool_boundary(choice_index, terminal)
    }

    fn ensure_tool_state(&mut self, key: (u64, ToolIndex)) -> io::Result<()> {
        if !self.tool_arguments.contains_key(&key) && self.tool_arguments.len() >= MAX_CHOICES {
            return Err(invalid_data("Chat SSE exceeded tool-call limit"));
        }
        self.tool_arguments.entry(key).or_default();
        Ok(())
    }

    fn repair_tool_argument(
        &mut self,
        key: (u64, ToolIndex),
        arguments: &mut Value,
        terminal: bool,
    ) -> io::Result<()> {
        let Some(delta) = arguments.as_str() else {
            return Ok(());
        };
        self.ensure_tool_state(key)?;
        let state = self.tool_arguments.entry(key).or_default();
        let repaired = state.push(delta, terminal)?;
        state.complete.push_str(&repaired);
        *arguments = Value::String(repaired);
        Ok(())
    }

    fn validate_tool_arguments(&self, choice_index: u64) -> io::Result<()> {
        for state in self
            .tool_arguments
            .iter()
            .filter(|((index, _), _)| *index == choice_index)
            .map(|(_, state)| state)
        {
            if !state.complete.trim().is_empty()
                && serde_json::from_str::<Value>(&state.complete).is_err()
            {
                return Err(tool_protocol_error("malformed tool arguments"));
            }
        }
        Ok(())
    }

    fn tool_boundary(&mut self, choice_index: u64, terminal: bool) -> io::Result<Option<bool>> {
        if terminal {
            for state in self
                .tool_arguments
                .iter_mut()
                .filter(|((index, _), _)| *index == choice_index)
                .map(|(_, state)| state)
            {
                if state.buffered.trim().is_empty() && state.complete.trim().is_empty() {
                    state.buffered.clear();
                    state.resolved = true;
                }
            }
        }
        let states = self
            .tool_arguments
            .iter()
            .filter(|((index, _), _)| *index == choice_index)
            .map(|(_, state)| state);
        let pending = states.clone().any(ToolArgumentPrefix::is_pending);
        if terminal && pending {
            return Err(invalid_data(
                "incomplete reasoning marker in tool arguments",
            ));
        }
        Ok((!pending).then(|| states.into_iter().any(|state| state.marker)))
    }

    fn pending_bytes(&self) -> Option<usize> {
        self.channels
            .values()
            .map(ReasoningBoundary::pending_bytes)
            .chain(self.tool_arguments.values().map(|arguments| {
                arguments
                    .buffered
                    .len()
                    .saturating_add(arguments.complete.len())
            }))
            .try_fold(0usize, usize::checked_add)
    }
}

fn take_fields(
    choice: &mut Value,
    payload_key: &str,
    boundary: &mut ReasoningBoundary,
) -> io::Result<BoundaryOutput> {
    let mut output = BoundaryOutput::default();
    let Some(payload) = choice.get_mut(payload_key).and_then(Value::as_object_mut) else {
        return Ok(output);
    };
    for (key, field) in [
        ("reasoning_content", Field::Reasoning),
        ("content", Field::Content),
    ] {
        let Some(text) = payload.get(key).and_then(Value::as_str).map(str::to_owned) else {
            continue;
        };
        payload.remove(key);
        if !text.is_empty() {
            output.append(boundary.push(field, &text).map_err(invalid_data)?);
        }
    }
    Ok(output)
}

fn put_fields(choice: &mut Value, payload_key: &str, output: BoundaryOutput) -> io::Result<()> {
    if output.is_empty() {
        return Ok(());
    }
    let payload = choice
        .as_object_mut()
        .ok_or_else(|| invalid_data("choice is not an object"))?
        .entry(payload_key)
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| invalid_data("choice payload is not an object"))?;
    for (key, text) in [
        ("reasoning_content", output.reasoning),
        ("content", output.content),
    ] {
        if !text.is_empty() {
            payload.insert(key.into(), Value::String(text));
        }
    }
    Ok(())
}

pub(crate) fn normalize_chat_json(value: &mut Value) -> io::Result<()> {
    ChatBoundaryNormalizer::default().apply(value, true)
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

#[derive(Debug, thiserror::Error)]
#[error("upstream tool protocol error: {0}")]
struct ToolProtocolError(&'static str);

fn tool_protocol_error(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, ToolProtocolError(message))
}

pub(crate) fn is_tool_protocol_error(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|source| source.is::<ToolProtocolError>())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(provider_type: Option<&str>, override_body: Option<Value>) -> Provider {
        let mut provider = Provider::with_id("p".into(), "test".into(), json!({}), None);
        provider.meta = Some(crate::provider::ProviderMeta {
            provider_type: provider_type.map(str::to_string),
            local_proxy_request_overrides: override_body.map(|body| {
                crate::provider::LocalProxyRequestOverrides {
                    body: Some(body),
                    ..Default::default()
                }
            }),
            ..Default::default()
        });
        provider
    }

    fn collect_fields(value: &Value, reasoning: &mut String, content: &mut String) {
        for choice in value["choices"].as_array().into_iter().flatten() {
            let payload = choice.get("delta").or_else(|| choice.get("message"));
            if let Some(text) = payload
                .and_then(|payload| payload.get("reasoning_content"))
                .and_then(Value::as_str)
            {
                reasoning.push_str(text);
            }
            if let Some(text) = payload
                .and_then(|payload| payload.get("content"))
                .and_then(Value::as_str)
            {
                content.push_str(text);
            }
        }
    }

    #[test]
    fn activation_is_exact_nexus_only_and_override_authoritative() {
        let enabled = json!({"chat_template_kwargs": {"enable_thinking": true}});
        let disabled = json!({"chat_template_kwargs": {"enable_thinking": false}});
        for (label, provider, body, expected) in [
            (
                "request",
                provider(Some("nexus"), None),
                enabled.clone(),
                true,
            ),
            ("wrong type", provider(None, None), enabled.clone(), false),
            (
                "wrong value",
                provider(Some("nexus"), None),
                disabled.clone(),
                false,
            ),
            (
                "override",
                provider(Some("nexus"), Some(enabled.clone())),
                json!({}),
                true,
            ),
            (
                "override wins",
                provider(Some("nexus"), Some(disabled)),
                enabled,
                false,
            ),
        ] {
            assert_eq!(enabled_for_attempt(&provider, &body), expected, "{label}");
        }
    }

    #[test]
    fn stream_normalization_is_choice_and_field_safe() {
        let mut normalizer = ChatBoundaryNormalizer::default();
        let mut reasoning = String::new();
        let mut content = String::new();
        let mut chunks = [
            json!({"choices": [
                {"index": 0, "delta": {"content": "r0</thi"}},
                {"index": 1, "delta": {"reasoning_content": "r1", "content": "a1"}}
            ]}),
            json!({"choices": [
                {"index": 0, "delta": {"content": "nk>a0"}, "finish_reason": "stop"},
                {"index": 1, "delta": {}, "finish_reason": "stop"}
            ]}),
        ];
        for chunk in &mut chunks {
            normalizer.normalize_chunk(chunk).unwrap();
            collect_fields(chunk, &mut reasoning, &mut content);
        }
        normalizer.finish().unwrap();
        assert_eq!(reasoning, "r1r0");
        assert_eq!(content, "a0a1");
    }

    #[test]
    fn split_tool_prefix_repairs_reasoning_without_touching_literals() {
        let mut normalizer = ChatBoundaryNormalizer::default();
        let mut first = json!({"choices": [{"index": 0, "delta": {
            "content": "private reasoning",
            "tool_calls": [
                {"index": 0, "function": {"arguments": "</thi"}},
                {"index": 1, "function": {"arguments": "{\"literal\":\""}}
            ]
        }}]});
        normalizer.normalize_chunk(&mut first).unwrap();
        assert!(first.pointer("/choices/0/delta/content").is_none());
        assert!(first
            .pointer("/choices/0/delta/reasoning_content")
            .is_none());

        let mut last = json!({"choices": [{"index": 0, "delta": {"tool_calls": [
            {"index": 0, "function": {"arguments": "nk>{\"path\":\"a\"}"}},
            {"index": 1, "function": {"arguments": "</think>\"}"}}
        ]}, "finish_reason": "tool_calls"}]});
        normalizer.normalize_chunk(&mut last).unwrap();
        normalizer.finish().unwrap();
        assert_eq!(
            last.pointer("/choices/0/delta/reasoning_content"),
            Some(&json!("private reasoning"))
        );
        assert_eq!(
            last.pointer("/choices/0/delta/tool_calls/0/function/arguments"),
            Some(&json!(r#"{"path":"a"}"#))
        );
        assert_eq!(
            last.pointer("/choices/0/delta/tool_calls/1/function/arguments"),
            Some(&json!(r#"</think>"}"#))
        );

        let mut literal = ChatBoundaryNormalizer::default();
        let mut first = json!({"choices": [{"index": 0, "delta": {
            "content": "Visible.",
            "tool_calls": [{"index": 0, "function": {"arguments": "</thi"}}]
        }}]});
        literal.normalize_chunk(&mut first).unwrap();
        assert!(first.pointer("/choices/0/delta/content").is_none());
        let mut last = json!({"choices": [{"index": 0, "delta": {
            "tool_calls": [{"index": 0, "function": {"arguments": "s>{}"}}]
        }, "finish_reason": "tool_calls"}]});
        let error = literal.normalize_chunk(&mut last).unwrap_err();
        assert!(is_tool_protocol_error(&error));
        assert!(last.pointer("/choices/0/delta/content").is_none());

        let mut spaced = ChatBoundaryNormalizer::default();
        let mut first = json!({"choices": [{"index": 0, "delta": {
            "content": "private",
            "tool_calls": [{"index": 0, "function": {"arguments": "  "}}]
        }}]});
        spaced.normalize_chunk(&mut first).unwrap();
        assert!(first.pointer("/choices/0/delta/content").is_none());
        let mut last = json!({"choices": [{"index": 0, "delta": {
            "tool_calls": [{"index": 0, "function": {"arguments": "</think>{}"}}]
        }, "finish_reason": "tool_calls"}]});
        spaced.normalize_chunk(&mut last).unwrap();
        spaced.finish().unwrap();
        assert_eq!(
            last.pointer("/choices/0/delta/reasoning_content"),
            Some(&json!("private"))
        );
        assert_eq!(
            last.pointer("/choices/0/delta/tool_calls/0/function/arguments"),
            Some(&json!("  {}"))
        );
    }

    #[test]
    fn ordinary_tool_preamble_stays_visible_for_modern_and_legacy_calls() {
        for payload in [
            json!({"content": "Visible.", "tool_calls": [{"index": 0, "function": {"arguments": "{}"}}]}),
            json!({"content": "Visible.", "function_call": {"arguments": "{}"}}),
        ] {
            let mut normalizer = ChatBoundaryNormalizer::default();
            let mut chunk = json!({"choices": [{
                "index": 0, "delta": payload, "finish_reason": "tool_calls"
            }]});
            normalizer.normalize_chunk(&mut chunk).unwrap();
            assert_eq!(
                chunk.pointer("/choices/0/delta/content"),
                Some(&json!("Visible."))
            );
            assert!(chunk
                .pointer("/choices/0/delta/reasoning_content")
                .is_none());
        }
    }

    #[test]
    fn terminal_whitespace_only_tool_arguments_resolve_as_empty() {
        for payload in [
            json!({"tool_calls": [{"index": 0, "function": {"arguments": "  "}}]}),
            json!({"function_call": {"arguments": "\n"}}),
        ] {
            let mut normalizer = ChatBoundaryNormalizer::default();
            let mut terminal = json!({"choices": [{
                "index": 0, "delta": payload, "finish_reason": "tool_calls"
            }]});
            normalizer.normalize_chunk(&mut terminal).unwrap();
            normalizer.finish().unwrap();
        }

        let mut split = ChatBoundaryNormalizer::default();
        let mut first = json!({"choices": [{"index": 0, "delta": {
            "content": "Visible preamble.",
            "tool_calls": [{"index": 0, "function": {"arguments": "  "}}]
        }}]});
        split.normalize_chunk(&mut first).unwrap();
        assert!(first.pointer("/choices/0/delta/content").is_none());
        let mut terminal =
            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]});
        split.normalize_chunk(&mut terminal).unwrap();
        split.finish().unwrap();
        assert_eq!(
            terminal.pointer("/choices/0/delta/content"),
            Some(&json!("Visible preamble."))
        );
    }

    #[test]
    fn metadata_only_tool_chunk_waits_for_argument_boundary() {
        let mut normalizer = ChatBoundaryNormalizer::default();
        let mut first = json!({"choices": [{"index": 0, "delta": {
            "content": "private reasoning",
            "tool_calls": [{"index": 0, "id": "call_1", "function": {"name": "Write"}}]
        }}]});
        normalizer.normalize_chunk(&mut first).unwrap();
        assert!(first.pointer("/choices/0/delta/content").is_none());

        let mut terminal = json!({"choices": [{"index": 0, "delta": {
            "tool_calls": [{"index": 0, "function": {"arguments": "</think>{}"}}]
        }, "finish_reason": "tool_calls"}]});
        normalizer.normalize_chunk(&mut terminal).unwrap();
        normalizer.finish().unwrap();
        assert_eq!(
            terminal.pointer("/choices/0/delta/reasoning_content"),
            Some(&json!("private reasoning"))
        );
        assert_eq!(
            terminal.pointer("/choices/0/delta/tool_calls/0/function/arguments"),
            Some(&json!("{}"))
        );

        let mut empty = ChatBoundaryNormalizer::default();
        let mut first = json!({"choices": [{"index": 0, "delta": {
            "content": "Visible preamble.",
            "tool_calls": [{"index": 0, "id": "call_2", "function": {"name": "Refresh"}}]
        }}]});
        empty.normalize_chunk(&mut first).unwrap();
        assert!(first.pointer("/choices/0/delta/content").is_none());
        let mut terminal =
            json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]});
        empty.normalize_chunk(&mut terminal).unwrap();
        empty.finish().unwrap();
        assert_eq!(
            terminal.pointer("/choices/0/delta/content"),
            Some(&json!("Visible preamble."))
        );
    }

    #[test]
    fn captured_glm47_tool_tail_fails_closed_without_harming_literal_prose() {
        let mut normalizer = ChatBoundaryNormalizer::default();
        normalizer
            .normalize_chunk(&mut json!({"choices": [{"index": 0, "delta": {
                "tool_calls": [{"index": 0, "function": {
                    "arguments": r#"{"content":"before \"</tool_call>"#
                }}]
            }}]}))
            .unwrap();

        let error = normalizer
            .normalize_chunk(&mut json!({"choices": [{"index": 0, "delta": {
                "content": r#"\" after</arg_value>"#
            }, "finish_reason": "tool_calls"}]}))
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("tool protocol"));
        assert!(!error.to_string().contains("reasoning boundary"));

        let mut discussion = ChatBoundaryNormalizer::default();
        let mut chunk = json!({"choices": [{"index": 0, "delta": {
            "content": "A literal `</tool_call>` belongs in this parser test."
        }, "finish_reason": "stop"}]});
        discussion.normalize_chunk(&mut chunk).unwrap();
        discussion.finish().unwrap();
        assert_eq!(
            chunk.pointer("/choices/0/delta/content"),
            Some(&json!(
                "A literal `</tool_call>` belongs in this parser test."
            ))
        );

        let mut valid = ChatBoundaryNormalizer::default();
        valid
            .normalize_chunk(&mut json!({"choices": [{"index": 0, "delta": {
                "tool_calls": [{"index": 0, "function": {"arguments": "{}"}}]
            }}]}))
            .unwrap();
        let mut tail = json!({"choices": [{"index": 0, "delta": {
            "content": "Legitimate text after a valid tool call."
        }, "finish_reason": "stop"}]});
        valid.normalize_chunk(&mut tail).unwrap();
        valid.finish().unwrap();
        assert_eq!(
            tail.pointer("/choices/0/delta/content"),
            Some(&json!("Legitimate text after a valid tool call."))
        );
    }

    #[test]
    fn terminal_and_cardinality_errors_fail_closed() {
        assert_eq!(
            ChatBoundaryNormalizer::default()
                .finish()
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );

        let mut unfinished = ChatBoundaryNormalizer::default();
        unfinished
            .normalize_chunk(&mut json!({"choices": [{"index": 0, "delta": {"content": "held"}}]}))
            .unwrap();
        assert_eq!(
            unfinished.finish().unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );

        let mut partial_tool = ChatBoundaryNormalizer::default();
        assert!(partial_tool
            .normalize_chunk(&mut json!({"choices": [{"index": 0, "delta": {
                "tool_calls": [{"index": 0, "function": {"arguments": "</thi"}}]
            }, "finish_reason": "tool_calls"}]}))
            .is_err());

        let mut too_many = ChatBoundaryNormalizer::default();
        let mut value = json!({"choices": (0..=MAX_CHOICES)
            .map(|index| json!({"index": index, "delta": {}}))
            .collect::<Vec<_>>()});
        assert!(too_many.normalize_chunk(&mut value).is_err());
    }

    #[test]
    fn provisional_content_is_held_and_post_finish_content_is_rejected() {
        let mut normalizer = ChatBoundaryNormalizer::default();
        let mut held = json!({"choices": [{"index": 0, "delta": {"content": "private"}}]});
        normalizer.normalize_chunk(&mut held).unwrap();
        assert!(held.pointer("/choices/0/delta/content").is_none());

        let mut terminal = json!({"choices": [{"index": 0, "delta": {
            "content": "</think>answer"
        }, "finish_reason": "stop"}]});
        normalizer.normalize_chunk(&mut terminal).unwrap();
        assert_eq!(
            terminal.pointer("/choices/0/delta/reasoning_content"),
            Some(&json!("private"))
        );
        assert_eq!(
            terminal.pointer("/choices/0/delta/content"),
            Some(&json!("answer"))
        );

        normalizer
            .normalize_chunk(&mut json!({"choices": [], "usage": {"completion_tokens": 2}}))
            .unwrap();
        assert!(normalizer
            .normalize_chunk(&mut json!({"choices": [{"index": 0, "delta": {
                "content": "late"
            }}]}))
            .is_err());
    }

    #[test]
    fn nonstream_repairs_both_boundary_and_tool_prefix() {
        let mut value = json!({"choices": [
            {"message": {"content": "r0 </think> a0"}},
            {"message": {
                "content": "private",
                "tool_calls": [{"function": {"arguments": "</thinking>{}"}}]
            }, "finish_reason": "tool_calls"}
        ]});
        normalize_chat_json(&mut value).unwrap();
        assert_eq!(
            value.pointer("/choices/0/message/reasoning_content"),
            Some(&json!("r0 "))
        );
        assert_eq!(
            value.pointer("/choices/0/message/content"),
            Some(&json!(" a0"))
        );
        assert_eq!(
            value.pointer("/choices/1/message/reasoning_content"),
            Some(&json!("private"))
        );
        assert_eq!(
            value.pointer("/choices/1/message/tool_calls/0/function/arguments"),
            Some(&json!("{}"))
        );
    }
}
