use super::reasoning_boundary::{BoundaryOutput, Field, ReasoningBoundary};
use crate::provider::Provider;
use crate::proxy::sse::{
    append_utf8_safe, is_reportable_error, is_sse_comment_block, take_sse_block,
};
use bytes::Bytes;
use futures::{Stream, StreamExt};
use serde_json::{json, Value};
use std::{collections::BTreeMap, io};

const MAX_PENDING_BYTES: usize = 8 * 1024 * 1024;
const MAX_CHOICES: usize = 128;
const THINK_CLOSE_TAGS: [&str; 2] = ["</think>", "</thinking>"];

fn is_nexus(provider: &Provider) -> bool {
    provider
        .meta
        .as_ref()
        .and_then(|meta| meta.provider_type.as_deref())
        == Some("nexus")
}

pub(crate) fn enabled_for_attempt(provider: &Provider, body: &Value) -> bool {
    is_nexus(provider)
        && provider
            .meta
            .as_ref()
            .and_then(|meta| meta.local_proxy_request_overrides.as_ref())
            .and_then(|overrides| overrides.body.as_ref())
            .and_then(|body| body.pointer("/chat_template_kwargs/enable_thinking"))
            .or_else(|| body.pointer("/chat_template_kwargs/enable_thinking"))
            == Some(&Value::Bool(true))
}

#[derive(Default)]
struct ToolArgumentPrefix {
    buffered: String,
    resolved: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ToolIndex {
    Modern(u64),
    Legacy,
}

impl ToolArgumentPrefix {
    fn push(&mut self, delta: &str, terminal: bool) -> io::Result<(String, bool)> {
        if self.resolved {
            return Ok((delta.to_string(), false));
        }
        self.buffered.push_str(delta);
        let trimmed = self.buffered.trim_start();
        if trimmed.is_empty() {
            return Ok((std::mem::take(&mut self.buffered), false));
        }
        if let Some(tag) = THINK_CLOSE_TAGS
            .iter()
            .find(|tag| trimmed.starts_with(**tag))
        {
            let leading = self.buffered.len() - trimmed.len();
            let mut repaired = self.buffered[..leading].to_string();
            repaired.push_str(&self.buffered[leading + tag.len()..]);
            self.buffered.clear();
            self.resolved = true;
            return Ok((repaired, true));
        }
        if THINK_CLOSE_TAGS.iter().any(|tag| tag.starts_with(trimmed)) {
            if terminal && !trimmed.is_empty() {
                return Err(invalid_data(
                    "incomplete reasoning marker in tool arguments",
                ));
            }
            return Ok((String::new(), true));
        }
        self.resolved = true;
        Ok((std::mem::take(&mut self.buffered), false))
    }

    fn is_pending(&self) -> bool {
        !self.resolved && !self.buffered.trim().is_empty()
    }
}

#[derive(Default)]
struct Boundaries {
    channels: BTreeMap<u64, ReasoningBoundary>,
    tool_arguments: BTreeMap<(u64, ToolIndex), ToolArgumentPrefix>,
}

impl Boundaries {
    fn apply(&mut self, value: &mut Value, complete_body: bool) -> io::Result<bool> {
        let Some(choices) = value.get_mut("choices").and_then(Value::as_array_mut) else {
            return Ok(false);
        };
        let mut buffered = false;
        let require_indices = !complete_body && choices.len() > 1;
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
            let finish_reason = choice.get("finish_reason").is_some_and(|v| !v.is_null());
            let tool_marker_evidence = self.normalize_tool_arguments(
                choice,
                if complete_body { "message" } else { "delta" },
                index,
                !complete_body,
                complete_body || finish_reason,
            )?;
            let payload_key = if complete_body { "message" } else { "delta" };
            let tool_boundary = choice.get(payload_key).is_some_and(|payload| {
                payload
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .is_some_and(|calls| !calls.is_empty())
                    || payload.get("function_call").is_some_and(|v| !v.is_null())
            });
            let boundary = self
                .channels
                .entry(index)
                .or_insert_with(|| ReasoningBoundary::new(MAX_PENDING_BYTES));
            let (mut output, saw_nonempty) = take_fields(choice, payload_key, boundary)?;
            if tool_boundary {
                output.append(
                    boundary
                        .commit_tool_boundary(tool_marker_evidence)
                        .map_err(invalid_data)?,
                );
            }
            if complete_body || finish_reason {
                output.append(boundary.finish().map_err(invalid_data)?);
            }
            buffered |= saw_nonempty && output.is_empty();
            put_fields(choice, payload_key, output)?;
            if self
                .pending_bytes()
                .is_none_or(|pending| pending > MAX_PENDING_BYTES)
            {
                return Err(invalid_data("Chat SSE reasoning buffer limit exceeded"));
            }
        }
        Ok(buffered)
    }

    fn is_complete(&self) -> bool {
        !self.channels.is_empty()
            && self.channels.values().all(ReasoningBoundary::is_done)
            && self
                .tool_arguments
                .values()
                .all(|arguments| !arguments.is_pending())
    }

    fn finish(&mut self) -> io::Result<Option<Value>> {
        if self
            .tool_arguments
            .values()
            .any(ToolArgumentPrefix::is_pending)
        {
            return Err(invalid_data(
                "incomplete reasoning marker in tool arguments",
            ));
        }
        let mut choices = Vec::new();
        for (index, boundary) in &mut self.channels {
            let output = boundary.finish().map_err(invalid_data)?;
            if output.is_empty() {
                continue;
            }
            let delta = json!({
                "reasoning_content": output.reasoning,
                "content": output.content
            });
            choices.push(json!({"index": index, "delta": delta, "finish_reason": null}));
        }
        Ok((!choices.is_empty()).then(|| json!({"choices": choices})))
    }

    fn normalize_tool_arguments(
        &mut self,
        choice: &mut Value,
        payload_key: &str,
        choice_index: u64,
        streaming: bool,
        terminal: bool,
    ) -> io::Result<bool> {
        let Some(payload) = choice.get_mut(payload_key).and_then(Value::as_object_mut) else {
            if terminal && self.has_pending_tool_arguments(choice_index) {
                return Err(invalid_data(
                    "incomplete reasoning marker in tool arguments",
                ));
            }
            return Ok(false);
        };
        let mut marker_evidence = false;
        if let Some(calls) = payload.get_mut("tool_calls").and_then(Value::as_array_mut) {
            for (position, call) in calls.iter_mut().enumerate() {
                let call_index = match call.get("index").and_then(Value::as_u64) {
                    Some(index) => index,
                    None if streaming => {
                        return Err(invalid_data("Chat SSE tool call omitted index"));
                    }
                    None => position as u64,
                };
                if let Some(arguments) = call.pointer_mut("/function/arguments") {
                    marker_evidence |= self.repair_tool_argument(
                        (choice_index, ToolIndex::Modern(call_index)),
                        arguments,
                        terminal,
                    )?;
                }
            }
        }
        if let Some(arguments) = payload
            .get_mut("function_call")
            .and_then(|function| function.get_mut("arguments"))
        {
            marker_evidence |=
                self.repair_tool_argument((choice_index, ToolIndex::Legacy), arguments, terminal)?;
        }
        if terminal && self.has_pending_tool_arguments(choice_index) {
            return Err(invalid_data(
                "incomplete reasoning marker in tool arguments",
            ));
        }
        Ok(marker_evidence)
    }

    fn repair_tool_argument(
        &mut self,
        key: (u64, ToolIndex),
        arguments: &mut Value,
        terminal: bool,
    ) -> io::Result<bool> {
        let Some(delta) = arguments.as_str() else {
            return Ok(false);
        };
        if !self.tool_arguments.contains_key(&key) && self.tool_arguments.len() >= MAX_CHOICES {
            return Err(invalid_data("Chat SSE exceeded tool-call limit"));
        }
        let (repaired, marker_evidence) = self
            .tool_arguments
            .entry(key)
            .or_default()
            .push(delta, terminal)?;
        *arguments = Value::String(repaired);
        Ok(marker_evidence)
    }

    fn has_pending_tool_arguments(&self, choice_index: u64) -> bool {
        self.tool_arguments
            .iter()
            .any(|((index, _), state)| *index == choice_index && state.is_pending())
    }

    fn pending_bytes(&self) -> Option<usize> {
        self.channels
            .values()
            .map(ReasoningBoundary::pending_bytes)
            .chain(
                self.tool_arguments
                    .values()
                    .map(|arguments| arguments.buffered.len()),
            )
            .try_fold(0usize, usize::checked_add)
    }
}

fn take_fields(
    choice: &mut Value,
    payload_key: &str,
    boundary: &mut ReasoningBoundary,
) -> io::Result<(BoundaryOutput, bool)> {
    let mut output = BoundaryOutput::default();
    let mut saw_nonempty = false;
    let Some(payload) = choice.get_mut(payload_key).and_then(Value::as_object_mut) else {
        return Ok((output, saw_nonempty));
    };
    for (key, field) in [
        ("reasoning_content", Field::Reasoning),
        ("content", Field::Content),
    ] {
        let Some(text) = payload.get(key).and_then(Value::as_str).map(str::to_owned) else {
            continue;
        };
        payload.remove(key);
        if text.is_empty() {
            continue;
        }
        saw_nonempty = true;
        output.append(boundary.push(field, &text).map_err(invalid_data)?);
    }
    Ok((output, saw_nonempty))
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
    Boundaries::default().apply(value, true).map(drop)
}

pub(crate) fn normalize_chat_sse(
    stream: impl Stream<Item = io::Result<Bytes>> + Send + 'static,
    enabled: bool,
) -> impl Stream<Item = io::Result<Bytes>> + Send {
    async_stream::stream! {
        tokio::pin!(stream);
        if !enabled {
            while let Some(item) = stream.next().await {
                yield item;
            }
            return;
        }

        let mut buffer = String::new();
        let mut utf8_remainder = Vec::new();
        let mut boundaries = Boundaries::default();
        while let Some(item) = stream.next().await {
            let bytes = match item {
                Ok(bytes) => bytes,
                Err(error) => {
                    yield Err(error);
                    return;
                }
            };
            append_utf8_safe(&mut buffer, &mut utf8_remainder, &bytes);
            let mut terminal: Option<(Option<Bytes>, Bytes, Vec<Bytes>)> = None;
            while let Some(block) = take_sse_block(&mut buffer) {
                if block.len() > MAX_PENDING_BYTES {
                    yield Err(invalid_data("upstream Chat SSE record exceeded buffer limit"));
                    return;
                }
                if let Some((_, _, comments)) = terminal.as_mut() {
                    if block.trim().is_empty() {
                        continue;
                    }
                    if is_sse_comment_block(&block) {
                        comments.push(Bytes::from(format!("{block}\n\n")));
                        continue;
                    }
                    yield Err(invalid_data("upstream Chat SSE contained data after [DONE]"));
                    return;
                }
                let (event, data) = split_block(&block);
                let Some(data) = data else {
                    yield Ok(Bytes::from(format!("{block}\n\n")));
                    continue;
                };
                if event == "error" {
                    yield Err(io::Error::other(data));
                    return;
                }
                if data.trim() == "[DONE]" {
                    let flush = match boundaries.finish() {
                        Ok(Some(flush)) => Some(encode_block("", &flush.to_string())),
                        Ok(None) => None,
                        Err(error) => {
                            yield Err(error);
                            return;
                        }
                    };
                    terminal = Some((flush, encode_block(event, "[DONE]"), Vec::new()));
                    continue;
                }
                let mut value: Value = match serde_json::from_str(&data) {
                    Ok(value) => value,
                    Err(error) => {
                        yield Err(invalid_data(error));
                        return;
                    }
                };
                if value.get("error").is_some_and(is_reportable_error) {
                    yield Err(io::Error::other(value.to_string()));
                    return;
                }
                let buffered = match boundaries.apply(&mut value, false) {
                    Ok(buffered) => buffered,
                    Err(error) => {
                        yield Err(error);
                        return;
                    }
                };
                yield Ok(encode_block(event, &value.to_string()));
                if buffered {
                    yield Ok(Bytes::from_static(b": nexus-boundary-buffered\n\n"));
                }
            }
            if buffer.len().saturating_add(utf8_remainder.len()) > MAX_PENDING_BYTES {
                yield Err(invalid_data("upstream Chat SSE record exceeded buffer limit"));
                return;
            }
            if let Some((flush, done, mut comments)) = terminal {
                if !utf8_remainder.is_empty() {
                    yield Err(invalid_data("upstream Chat SSE contained invalid data after [DONE]"));
                    return;
                }
                if !buffer.trim().is_empty() {
                    if is_sse_comment_block(&buffer) {
                        comments.push(Bytes::from(format!("{buffer}\n\n")));
                    } else {
                        yield Err(invalid_data("upstream Chat SSE contained data after [DONE]"));
                        return;
                    }
                }
                if let Some(flush) = flush {
                    yield Ok(flush);
                }
                for comment in comments {
                    yield Ok(comment);
                }
                yield Ok(done);
                return;
            }
        }

        if !buffer.is_empty() || !utf8_remainder.is_empty() || !boundaries.is_complete() {
            yield Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "upstream Chat SSE ended before a complete terminal event",
            ));
        }
    }
}

fn split_block(block: &str) -> (&str, Option<String>) {
    let lines = || {
        block
            .lines()
            .map(|line| line.trim_start_matches('\u{feff}').trim_start())
    };
    let event = lines()
        .find_map(|line| crate::proxy::sse::strip_sse_field(line, "event"))
        .map(str::trim)
        .unwrap_or("");
    let data = lines()
        .filter_map(|line| crate::proxy::sse::strip_sse_field(line, "data"))
        .collect::<Vec<_>>();
    (event, (!data.is_empty()).then(|| data.join("\n")))
}

fn encode_block(event: &str, data: &str) -> Bytes {
    Bytes::from(if event.is_empty() {
        format!("data: {data}\n\n")
    } else {
        format!("event: {event}\ndata: {data}\n\n")
    })
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;

    fn provider(provider_type: Option<&str>) -> Provider {
        let mut provider = Provider::with_id("p".into(), "arbitrary".into(), json!({}), None);
        provider.meta = Some(crate::provider::ProviderMeta {
            provider_type: provider_type.map(str::to_string),
            ..Default::default()
        });
        provider
    }

    fn event(value: Value) -> io::Result<Bytes> {
        Ok(Bytes::from(format!("data: {value}\n\n")))
    }

    async fn collect(
        input: Vec<io::Result<Bytes>>,
        enabled: bool,
    ) -> (Vec<Bytes>, Vec<(io::ErrorKind, String)>) {
        let output = normalize_chat_sse(stream::iter(input), enabled);
        futures::pin_mut!(output);
        let (mut bytes, mut errors) = (Vec::new(), Vec::new());
        while let Some(item) = output.next().await {
            match item {
                Ok(item) => bytes.push(item),
                Err(error) => errors.push((error.kind(), error.to_string())),
            }
        }
        (bytes, errors)
    }

    fn values(bytes: &[Bytes]) -> Vec<Value> {
        bytes
            .iter()
            .filter_map(|bytes| {
                let data = std::str::from_utf8(bytes)
                    .ok()?
                    .lines()
                    .filter_map(|line| crate::proxy::sse::strip_sse_field(line, "data"))
                    .collect::<Vec<_>>();
                (!data.is_empty() && data.join("\n") != "[DONE]")
                    .then(|| serde_json::from_str(&data.join("\n")).unwrap())
            })
            .collect()
    }

    #[test]
    fn activation_is_exact_and_nexus_only() {
        let mut nexus = provider(Some("nexus"));
        for (body, enabled) in [
            (json!({}), false),
            (
                json!({"chat_template_kwargs": {"enable_thinking": true}}),
                true,
            ),
            (
                json!({"chat_template_kwargs": {"enable_thinking": false}}),
                false,
            ),
            (
                json!({"chat_template_kwargs": {"enable_thinking": "true"}}),
                false,
            ),
        ] {
            assert_eq!(enabled_for_attempt(&nexus, &body), enabled);
        }

        let mut generic = provider(None);
        generic.settings_config =
            json!({"nexusCapabilities": {"reasoningBoundary": "think_close"}});
        let enabled_body = json!({"chat_template_kwargs": {"enable_thinking": true}});
        assert!(!enabled_for_attempt(&generic, &enabled_body));
        assert!(!enabled_for_attempt(&nexus, &json!({})));
        nexus.meta.as_mut().unwrap().local_proxy_request_overrides =
            Some(crate::provider::LocalProxyRequestOverrides {
                body: Some(enabled_body.clone()),
                ..Default::default()
            });
        assert!(enabled_for_attempt(&nexus, &json!({})));
        assert!(!enabled_for_attempt(&generic, &enabled_body));
        nexus
            .meta
            .as_mut()
            .unwrap()
            .local_proxy_request_overrides
            .as_mut()
            .unwrap()
            .body = Some(json!({"chat_template_kwargs": {"enable_thinking": false}}));
        assert!(!enabled_for_attempt(&nexus, &enabled_body));
    }

    #[tokio::test]
    async fn disabled_path_and_successful_stream_matrix() {
        let (bytes, errors) = collect(
            vec![
                Ok(Bytes::from_static(b"not even sse")),
                Err(io::Error::new(io::ErrorKind::TimedOut, "same error")),
            ],
            false,
        )
        .await;
        assert_eq!(bytes, [Bytes::from_static(b"not even sse")]);
        assert_eq!(errors, [(io::ErrorKind::TimedOut, "same error".into())]);

        let (bytes, errors) = collect(
            vec![
                Ok(Bytes::from_static(b": ping\n\n")),
                event(json!({"choices": [
                    {"index": 0, "delta": {"content": "r0</thi"}},
                    {"index": 1, "delta": {
                        "reasoning_content": "r1",
                        "content": "a1",
                        "tool_calls": [{"index": 0, "function": {"name": "run", "arguments": "{}"}}]
                    }},
                    {"index": 2, "delta": {"content": "a2"}}
                ]})),
                event(json!({"choices": [{
                    "index": 0,
                    "delta": {"content": "nk>a0"},
                    "finish_reason": "stop"
                }]})),
                Ok(Bytes::from_static(b"data: [DONE]\n\n")),
            ],
            true,
        )
        .await;
        assert!(errors.is_empty());
        assert_eq!(bytes[0], Bytes::from_static(b": ping\n\n"));
        let parsed = values(&bytes);
        assert_eq!(parsed[0]["choices"][1]["delta"]["reasoning_content"], "r1");
        assert_eq!(parsed[0]["choices"][1]["delta"]["content"], "a1");
        assert_eq!(parsed[1]["choices"][0]["delta"]["reasoning_content"], "r0");
        assert_eq!(parsed[1]["choices"][0]["delta"]["content"], "a0");
        assert_eq!(parsed[2]["choices"][0]["delta"]["content"], "a2");
        assert_eq!(
            bytes
                .iter()
                .filter(|chunk| chunk.as_ref() == b"data: [DONE]\n\n")
                .count(),
            1
        );
        assert!(bytes
            .iter()
            .any(|chunk| chunk.as_ref() == b": nexus-boundary-buffered\n\n"));
    }

    #[tokio::test]
    async fn stream_failures_never_flush_held_content() {
        let (bytes, errors) = collect(
            vec![event(json!({"choices": [{"index": 0, "delta": {
                "content": "secret<think>again<think>"
            }}]}))],
            true,
        )
        .await;
        assert_eq!(errors[0].0, io::ErrorKind::InvalidData);
        assert!(!String::from_utf8_lossy(&bytes.concat()).contains("secret"));

        let (bytes, errors) = collect(
            vec![event(json!({"choices": [{
                "index": 0, "delta": {"content": "secret"}
            }]}))],
            true,
        )
        .await;
        assert_eq!(errors[0].0, io::ErrorKind::UnexpectedEof);
        assert!(!String::from_utf8_lossy(&bytes.concat()).contains("secret"));

        for (tail, kind, message) in [
            (
                Ok(Bytes::from_static(
                    b"event: error\ndata: {\"error\":{\"message\":\"boom\"}}\n\n",
                )),
                io::ErrorKind::Other,
                "boom",
            ),
            (
                Err(io::Error::new(io::ErrorKind::ConnectionReset, "reset")),
                io::ErrorKind::ConnectionReset,
                "reset",
            ),
        ] {
            let (bytes, errors) = collect(
                vec![
                    event(json!({"choices": [{"index": 0, "delta": {
                        "content": "secret"
                    }}]})),
                    tail,
                ],
                true,
            )
            .await;
            assert_eq!(errors.len(), 1);
            assert_eq!(errors[0].0, kind);
            assert!(errors[0].1.contains(message));
            assert!(!String::from_utf8_lossy(&bytes.concat()).contains("secret"));
        }
    }

    #[tokio::test]
    async fn tool_argument_prefix_repair_is_split_safe_and_literal_safe() {
        let (bytes, errors) = collect(
            vec![
                event(json!({"choices": [{"index": 0, "delta": {
                    "content": "private reasoning",
                    "tool_calls": [
                    {"index": 0, "function": {"arguments": "</thi"}},
                    {"index": 1, "function": {"arguments": "{\"literal\":\""}},
                    {"index": 2, "function": {"arguments": "  "}}
                ]}}]})),
                event(json!({"choices": [{"index": 0, "delta": {"tool_calls": [
                    {"index": 0, "function": {"arguments": "nk>{\"path\":\"a\"}"}},
                    {"index": 1, "function": {"arguments": "</think>\"}"}},
                    {"index": 2, "function": {"arguments": "</thinking>{\"path\":\"b\"}"}}
                ]}, "finish_reason": "tool_calls"}]})),
                Ok(Bytes::from_static(b"data: [DONE]\n\n")),
            ],
            true,
        )
        .await;
        assert!(errors.is_empty());

        let mut arguments = BTreeMap::<u64, String>::new();
        for value in values(&bytes) {
            let Some(calls) = value
                .pointer("/choices/0/delta/tool_calls")
                .and_then(Value::as_array)
            else {
                continue;
            };
            for call in calls {
                let index = call.get("index").and_then(Value::as_u64).unwrap();
                if let Some(delta) = call.pointer("/function/arguments").and_then(Value::as_str) {
                    arguments.entry(index).or_default().push_str(delta);
                }
            }
        }
        assert_eq!(arguments[&0], r#"{"path":"a"}"#);
        assert_eq!(arguments[&1], r#"{"literal":"</think>"}"#);
        assert_eq!(arguments[&2], r#"  {"path":"b"}"#);
        let merged = String::from_utf8(bytes.concat()).unwrap();
        assert!(merged.contains("\"reasoning_content\":\"private reasoning\""));
        assert!(!merged.contains("\"content\":\"private reasoning\""));
    }

    #[tokio::test]
    async fn ordinary_streaming_tool_calls_preserve_visible_content() {
        for (label, payload) in [
            (
                "modern",
                json!({
                    "content": "I will inspect the file now.",
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "inspect", "arguments": "{}"}
                    }]
                }),
            ),
            (
                "legacy",
                json!({
                    "content": "I will inspect the file now.",
                    "function_call": {"name": "inspect", "arguments": "{}"}
                }),
            ),
        ] {
            let (bytes, errors) = collect(
                vec![
                    event(json!({"choices": [{
                        "index": 0,
                        "delta": payload,
                        "finish_reason": "tool_calls"
                    }]})),
                    Ok(Bytes::from_static(b"data: [DONE]\n\n")),
                ],
                true,
            )
            .await;
            assert!(errors.is_empty(), "{label}: {errors:?}");
            let output = values(&bytes);
            assert_eq!(
                output[0].pointer("/choices/0/delta/content"),
                Some(&json!("I will inspect the file now.")),
                "{label}: {output:#?}"
            );
            assert!(
                output[0]
                    .pointer("/choices/0/delta/reasoning_content")
                    .is_none(),
                "{label}: {output:#?}"
            );
        }
    }

    #[tokio::test]
    async fn tool_argument_prefix_failures_are_bounded_and_unambiguous() {
        let (_, errors) = collect(
            vec![event(json!({"choices": [{"index": 0, "delta": {
                "tool_calls": [{"function": {"arguments": "{}"}}]
            }}]}))],
            true,
        )
        .await;
        assert_eq!(errors[0].0, io::ErrorKind::InvalidData);
        assert!(errors[0].1.contains("tool call omitted index"));

        let (_, errors) = collect(
            vec![
                event(json!({"choices": [{"index": 0, "delta": {
                    "tool_calls": [{"index": 0, "function": {"arguments": "</thi"}}]
                }}]})),
                Ok(Bytes::from_static(b"data: [DONE]\n\n")),
            ],
            true,
        )
        .await;
        assert_eq!(errors[0].0, io::ErrorKind::InvalidData);
        assert!(errors[0]
            .1
            .contains("incomplete reasoning marker in tool arguments"));

        let calls = (0..=MAX_CHOICES)
            .map(|index| json!({"index": index, "function": {"arguments": "{}"}}))
            .collect::<Vec<_>>();
        let (_, errors) = collect(
            vec![event(json!({"choices": [{"index": 0, "delta": {
                "tool_calls": calls
            }}]}))],
            true,
        )
        .await;
        assert_eq!(errors[0].0, io::ErrorKind::InvalidData);
        assert!(errors[0].1.contains("tool-call limit"));
    }

    #[tokio::test]
    async fn done_validates_same_chunk_trailing_evidence() {
        let (bytes, errors) = collect(
            vec![Ok(Bytes::from_static(
                b"data: [DONE]\n\ndata: {\"choices\":[]}\n\n",
            ))],
            true,
        )
        .await;
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, io::ErrorKind::InvalidData);
        assert!(bytes.is_empty());

        let (bytes, errors) = collect(
            vec![Ok(Bytes::from_static(
                b"data: [DONE]\n\n: complete keepalive\n\n: trailing keepalive",
            ))],
            true,
        )
        .await;
        assert!(errors.is_empty());
        let merged = String::from_utf8(bytes.concat()).unwrap();
        assert!(merged.contains(": complete keepalive"));
        assert!(merged.contains(": trailing keepalive"));
        assert!(merged.ends_with("data: [DONE]\n\n"));
        assert_eq!(merged.matches("data: [DONE]\n\n").count(), 1);
    }

    #[tokio::test]
    async fn done_completes_without_transport_eof() {
        let input = stream::iter(vec![Ok(Bytes::from_static(b"data: [DONE]\n\n"))])
            .chain(stream::pending::<io::Result<Bytes>>());
        let output = normalize_chat_sse(input, true);
        futures::pin_mut!(output);

        let item = tokio::time::timeout(std::time::Duration::from_secs(1), output.next())
            .await
            .expect("[DONE] must not wait for transport EOF")
            .expect("normalized stream ended before [DONE]")
            .unwrap();
        assert_eq!(item, Bytes::from_static(b"data: [DONE]\n\n"));
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), output.next())
                .await
                .expect("normalized stream did not terminate after [DONE]")
                .is_none()
        );
    }

    #[tokio::test]
    async fn oversized_pending_sse_record_fails_without_transport_eof() {
        let input = stream::iter(vec![Ok(Bytes::from(vec![b'x'; MAX_PENDING_BYTES + 1]))])
            .chain(stream::pending::<io::Result<Bytes>>());
        let output = normalize_chat_sse(input, true);
        futures::pin_mut!(output);

        let error = tokio::time::timeout(std::time::Duration::from_secs(1), output.next())
            .await
            .expect("oversized SSE record must fail before another upstream poll")
            .expect("normalized stream ended without an error")
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn top_level_error_placeholders_are_distinguished() {
        let (bytes, errors) = collect(
            vec![event(json!({
                "error": {},
                "choices": [{"index": 0, "delta": {
                    "content": "ok"
                }, "finish_reason": "stop"}]
            }))],
            true,
        )
        .await;
        assert!(errors.is_empty());
        assert!(String::from_utf8(bytes.concat()).unwrap().contains("ok"));
        let (bytes, errors) =
            collect(vec![event(json!({"error": {"message": "boom"}}))], true).await;
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, io::ErrorKind::Other);
        assert!(errors[0].1.contains("boom"));
        assert!(bytes.is_empty());
    }

    #[test]
    fn nonstream_is_multi_choice_and_fail_closed() {
        let mut value = json!({"choices": [
            {"index": 0, "message": {"content": "r0</think>a0"}, "finish_reason": "stop"},
            {"index": 1, "message": {
                "content": "private reasoning",
                "tool_calls": [{"function": {"arguments": "</think>{}"}}]
            }, "finish_reason": "tool_calls"}
        ]});
        normalize_chat_json(&mut value).unwrap();
        assert_eq!(value["choices"][0]["message"]["reasoning_content"], "r0");
        assert_eq!(value["choices"][0]["message"]["content"], "a0");
        assert_eq!(
            value["choices"][1]["message"]["reasoning_content"],
            "private reasoning"
        );
        assert!(value["choices"][1]["message"].get("content").is_none());
        assert_eq!(
            value.pointer("/choices/1/message/tool_calls/0/function/arguments"),
            Some(&json!("{}"))
        );

        let mut malformed = json!({"choices": [{
            "message": {"content": "<think>x<think>"}
        }]});
        assert_eq!(
            normalize_chat_json(&mut malformed).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        let mut legacy = json!({"choices": [{"message": {"function_call": {
            "arguments": "</think>{\"path\":\"legacy.md\"}"
        }}}]});
        normalize_chat_json(&mut legacy).unwrap();
        assert_eq!(
            legacy.pointer("/choices/0/message/function_call/arguments"),
            Some(&json!(r#"{"path":"legacy.md"}"#))
        );
    }

    #[test]
    fn ordinary_nonstream_tool_calls_preserve_visible_content() {
        for (label, message) in [
            (
                "modern",
                json!({
                    "content": "I will inspect the file now.",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {"name": "inspect", "arguments": "{}"}
                    }]
                }),
            ),
            (
                "legacy",
                json!({
                    "content": "I will inspect the file now.",
                    "function_call": {"name": "inspect", "arguments": "{}"}
                }),
            ),
        ] {
            let mut value = json!({"choices": [{
                "index": 0,
                "message": message,
                "finish_reason": "tool_calls"
            }]});
            normalize_chat_json(&mut value).unwrap();
            assert_eq!(
                value.pointer("/choices/0/message/content"),
                Some(&json!("I will inspect the file now.")),
                "{label}: {value:#?}"
            );
            assert!(
                value
                    .pointer("/choices/0/message/reasoning_content")
                    .is_none(),
                "{label}: {value:#?}"
            );
        }
    }

    #[test]
    fn streaming_choice_state_is_bounded_and_unambiguous() {
        let mut too_many = json!({
            "choices": (0..=128)
                .map(|index| json!({"index": index, "delta": {}}))
                .collect::<Vec<_>>()
        });
        assert!(Boundaries::default().apply(&mut too_many, false).is_err());

        let mut missing_indices = json!({"choices": [{"delta": {}}, {"delta": {}}]});
        assert!(Boundaries::default()
            .apply(&mut missing_indices, false)
            .is_err());

        let held = "x".repeat(MAX_PENDING_BYTES / 2 + 1);
        let mut oversized = json!({"choices": [
            {"index": 0, "delta": {"content": held}},
            {"index": 1, "delta": {"content": held}}
        ]});
        assert!(Boundaries::default().apply(&mut oversized, false).is_err());
    }
}
