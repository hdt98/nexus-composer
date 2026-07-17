//! GLM Chat reasoning boundary framing shared by Codex and Anthropic adapters.

use super::codex_chat_common::extract_reasoning_field_text;
use bytes::Bytes;
use futures::{Stream, StreamExt};
use serde_json::{json, Map, Value};
use std::time::Duration;

use crate::{provider::Provider, proxy::sse::take_sse_block};

const BUFFER_LIMIT: usize = 8 * 1024 * 1024;
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);

pub(crate) fn enabled_for_attempt(provider: &Provider, body: &Value) -> bool {
    enabled_for_request(
        provider,
        body.pointer("/chat_template_kwargs/enable_thinking") == Some(&Value::Bool(true)),
    )
}

pub(crate) fn enabled_for_request(provider: &Provider, request_enabled: bool) -> bool {
    let Some(meta) = provider.meta.as_ref() else {
        return false;
    };
    if !matches!(meta.provider_type.as_deref(), None | Some("nexus")) {
        return false;
    }
    if let Some(enabled) = meta
        .local_proxy_request_overrides
        .as_ref()
        .and_then(|overrides| overrides.body.as_ref())
        .and_then(|body| body.pointer("/chat_template_kwargs/enable_thinking"))
    {
        return enabled == &Value::Bool(true);
    }
    meta.provider_type.as_deref() == Some("nexus") && request_enabled
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Channel {
    Reasoning,
    Content,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Parts {
    pub reasoning: String,
    pub content: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ProtocolError {
    #[error("malformed reasoning boundary")]
    Boundary,
    #[error("reasoning boundary buffer limit exceeded")]
    Overflow,
    #[error("malformed upstream stream framing")]
    Stream,
    #[error("malformed upstream tool arguments")]
    Tool,
}

impl ProtocolError {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Tool => "upstream_tool_protocol_error",
            Self::Boundary | Self::Overflow | Self::Stream => "stream_protocol_error",
        }
    }
}

const MARKERS: [(&str, bool); 4] = [
    ("<think>", true),
    ("</think>", false),
    ("<thinking>", true),
    ("</thinking>", false),
];

#[derive(Clone, Copy)]
struct Marker {
    start: usize,
    end: usize,
    opens: bool,
    long: bool,
}

#[derive(Clone, Copy)]
enum Code {
    Inline(usize),
    Fence(u8, usize),
}

fn run(bytes: &[u8], start: usize) -> usize {
    let Some(&symbol) = bytes.get(start) else {
        return 0;
    };
    if !matches!(symbol, b'`' | b'~') {
        return 0;
    }
    bytes[start..]
        .iter()
        .take_while(|byte| **byte == symbol)
        .count()
}

fn line_end(bytes: &[u8], start: usize) -> usize {
    bytes[start..]
        .iter()
        .position(|byte| matches!(byte, b'\n' | b'\r'))
        .map_or(bytes.len(), |end| start + end)
}

fn markers(text: &str) -> Result<Vec<Marker>, ProtocolError> {
    let tags = MARKERS.map(|(tag, _)| tag);
    let bytes = text.as_bytes();
    let (mut found, mut code, mut line_start, mut protected, mut i) =
        (Vec::new(), None, true, false, 0);
    while i < bytes.len() {
        if matches!(bytes[i], b'\n' | b'\r') {
            line_start = true;
            i += 1;
            continue;
        }
        if line_start && !matches!(code, Some(Code::Inline(_))) {
            let start = i;
            while bytes.get(i) == Some(&b' ') && i - start < 4 {
                i += 1;
            }
            if bytes.get(i) == Some(&b'\t') || i - start == 4 {
                let end = line_end(bytes, i);
                protected |= tags.iter().any(|tag| text[i..end].contains(tag));
                i = end;
                continue;
            }
            let width = run(bytes, i);
            let line_end = line_end(bytes, i);
            let suffix = &text[i + width..line_end];
            let closing_suffix = suffix.trim_matches([' ', '\t']).is_empty();
            match code {
                Some(Code::Fence(symbol, open))
                    if bytes.get(i) == Some(&symbol) && width >= open && closing_suffix =>
                {
                    code = None;
                    i += width;
                    line_start = false;
                    continue;
                }
                None if width >= 3 && (bytes[i] != b'`' || !suffix.as_bytes().contains(&b'`')) => {
                    code = Some(Code::Fence(bytes[i], width));
                    i += width;
                    line_start = false;
                    continue;
                }
                _ => line_start = false,
            }
        }
        if i == bytes.len() {
            break;
        }
        if bytes[i] == b'<' {
            if let Some((tag, opens)) = MARKERS.iter().find(|(tag, _)| text[i..].starts_with(*tag))
            {
                if code.is_some() || i > 0 && bytes[i - 1] == b'\\' {
                    protected = true;
                } else {
                    found.push(Marker {
                        start: i,
                        end: i + tag.len(),
                        opens: *opens,
                        long: tag.contains("thinking"),
                    });
                }
                i += tag.len();
                continue;
            }
        }
        if !matches!(code, Some(Code::Fence(_, _))) && bytes[i] == b'`' {
            let width = run(bytes, i);
            code = match code {
                Some(Code::Inline(open)) if open == width => None,
                None => Some(Code::Inline(width)),
                value => value,
            };
            i += width;
        } else {
            i += 1;
        }
    }
    if code.is_some() && protected {
        Err(ProtocolError::Boundary)
    } else {
        Ok(found)
    }
}

fn has_trailing_partial_marker(text: &str) -> bool {
    MARKERS.iter().any(|(tag, _)| {
        (4..tag.len()).any(|len| {
            text.strip_suffix(&tag[..len])
                .is_some_and(|prefix| !prefix.ends_with('\\'))
        })
    })
}

fn marked_parts(text: &str, truncated: bool) -> Result<Option<Parts>, ProtocolError> {
    let found = markers(text)?;
    if has_trailing_partial_marker(text) {
        return Err(ProtocolError::Boundary);
    }
    let parts = match found.as_slice() {
        [] => return Ok(None),
        [open] if truncated && open.opens && text[..open.start].trim().is_empty() => Parts {
            reasoning: text[open.end..].to_string(),
            content: String::new(),
        },
        [close] if !close.opens => Parts {
            reasoning: text[..close.start].to_string(),
            content: text[close.end..].to_string(),
        },
        [open, close]
            if open.opens
                && !close.opens
                && open.long == close.long
                && text[..open.start].trim().is_empty() =>
        {
            Parts {
                reasoning: text[open.end..close.start].to_string(),
                content: text[close.end..].to_string(),
            }
        }
        _ => return Err(ProtocolError::Boundary),
    };
    Ok(Some(parts))
}

#[derive(Debug)]
pub(crate) struct ReasoningFramer {
    combined: String,
    parts: Parts,
    limit: usize,
}

impl ReasoningFramer {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            combined: String::new(),
            parts: Parts::default(),
            limit,
        }
    }

    pub(crate) fn push(&mut self, channel: Channel, text: &str) -> Result<(), ProtocolError> {
        if self.combined.len().saturating_add(text.len()) > self.limit {
            return Err(ProtocolError::Overflow);
        }
        self.combined.push_str(text);
        match channel {
            Channel::Reasoning => self.parts.reasoning.push_str(text),
            Channel::Content => self.parts.content.push_str(text),
        }
        Ok(())
    }

    fn consume_tool_close(&mut self, arguments: &mut String) -> Result<(), ProtocolError> {
        let leading = arguments.len() - arguments.trim_start().len();
        let trimmed = &arguments[leading..];
        for (tag, opens) in MARKERS {
            if opens {
                continue;
            }
            for split in 0..tag.len() {
                if self.combined.ends_with(&tag[..split]) && trimmed.starts_with(&tag[split..]) {
                    self.push(Channel::Content, &tag[split..])?;
                    arguments.drain(leading..leading + tag.len() - split);
                    return Ok(());
                }
            }
            if !trimmed.is_empty() && tag.starts_with(trimmed) {
                return Err(ProtocolError::Boundary);
            }
        }
        Ok(())
    }

    pub(crate) fn finish(self, truncated: bool) -> Result<Parts, ProtocolError> {
        if let Some(parts) = marked_parts(&self.combined, truncated)? {
            return Ok(parts);
        }
        Ok(self.parts)
    }
}

fn validate_tool_arguments(arguments: &str) -> Result<(), ProtocolError> {
    if !arguments.trim().is_empty() && serde_json::from_str::<Value>(arguments).is_err() {
        return Err(ProtocolError::Tool);
    }
    Ok(())
}

fn normalize_tool_arguments(
    function: &mut Value,
    framer: Option<&mut ReasoningFramer>,
) -> Result<(), ProtocolError> {
    let function = function.as_object_mut().ok_or(ProtocolError::Tool)?;
    if function
        .get("name")
        .and_then(Value::as_str)
        .is_none_or(|name| name.trim().is_empty())
    {
        return Err(ProtocolError::Tool);
    }
    let mut arguments = match function.get("arguments") {
        Some(Value::String(arguments)) => arguments.clone(),
        Some(Value::Null) | None => return Ok(()),
        Some(_) => return Err(ProtocolError::Tool),
    };
    if let Some(framer) = framer {
        framer.consume_tool_close(&mut arguments)?;
    }
    validate_tool_arguments(&arguments)?;
    function.insert("arguments".to_string(), Value::String(arguments));
    Ok(())
}

#[derive(Debug)]
enum ContentShape {
    Missing,
    Null,
    String,
    Value(Value),
}

fn take_content(
    payload: &mut Map<String, Value>,
) -> Result<(Option<String>, ContentShape), ProtocolError> {
    match payload.remove("content") {
        None => Ok((None, ContentShape::Missing)),
        Some(Value::Null) => Ok((None, ContentShape::Null)),
        Some(Value::String(text)) => Ok((Some(text), ContentShape::String)),
        Some(value @ Value::Array(_)) => {
            for part in value.as_array().expect("array was matched above") {
                let text = match part.get("type").and_then(Value::as_str) {
                    Some("text" | "output_text") => part.get("text").and_then(Value::as_str),
                    Some("refusal") => part.get("refusal").and_then(Value::as_str),
                    _ => None,
                };
                if text.is_some_and(|text| {
                    marked_parts(text, false)
                        .map(|parts| parts.is_some())
                        .unwrap_or(true)
                }) {
                    return Err(ProtocolError::Boundary);
                }
            }
            Ok((None, ContentShape::Value(value)))
        }
        Some(_) => Err(ProtocolError::Stream),
    }
}

fn restore_content(payload: &mut Map<String, Value>, shape: ContentShape, content: String) {
    match shape {
        ContentShape::Missing if content.is_empty() => {}
        ContentShape::Null if content.is_empty() => {
            payload.insert("content".to_string(), Value::Null);
        }
        ContentShape::Value(value) => {
            payload.insert("content".to_string(), value);
        }
        ContentShape::Missing | ContentShape::Null | ContentShape::String => {
            payload.insert("content".to_string(), Value::String(content));
        }
    }
}

pub(crate) fn normalize_first_choice(value: &mut Value) -> Result<(), ProtocolError> {
    normalize_first_choice_with_framer(value, None)
}

fn normalize_first_choice_with_framer(
    value: &mut Value,
    ordered_framer: Option<ReasoningFramer>,
) -> Result<(), ProtocolError> {
    let Some(choice) = value
        .get_mut("choices")
        .and_then(Value::as_array_mut)
        .and_then(|choices| choices.first_mut())
    else {
        return Ok(());
    };
    let truncated = choice.get("finish_reason").and_then(Value::as_str) == Some("length");
    let tool_terminal = matches!(
        choice.get("finish_reason").and_then(Value::as_str),
        Some("tool_calls" | "function_call")
    );
    let Some(message) = choice.get_mut("message").and_then(Value::as_object_mut) else {
        return Ok(());
    };
    let reasoning = extract_reasoning_field_text(&Value::Object(message.clone()));

    let ordered = ordered_framer.is_some();
    let mut framer = ordered_framer.unwrap_or_else(|| ReasoningFramer::new(BUFFER_LIMIT));
    message.remove("reasoning_content");
    message.remove("reasoning");
    message.remove("reasoning_details");
    if !ordered {
        if let Some(reasoning) = reasoning {
            framer.push(Channel::Reasoning, &reasoning)?;
        }
    }

    let (content, content_shape) = take_content(message)?;
    if !ordered {
        if let Some(content) = content {
            framer.push(Channel::Content, &content)?;
        }
    }

    let mut first_tool = true;
    if let Some(calls) = message.get_mut("tool_calls") {
        match calls {
            Value::Array(calls) => {
                for call in calls {
                    let function = call.get_mut("function").ok_or(ProtocolError::Tool)?;
                    let boundary = if first_tool { Some(&mut framer) } else { None };
                    normalize_tool_arguments(function, boundary)?;
                    first_tool = false;
                }
            }
            Value::Null => {}
            _ => return Err(ProtocolError::Stream),
        }
    }
    if let Some(function) = message.get_mut("function_call") {
        let boundary = if first_tool { Some(&mut framer) } else { None };
        normalize_tool_arguments(function, boundary)?;
        first_tool = false;
    }
    if tool_terminal && first_tool {
        return Err(ProtocolError::Tool);
    }
    let parts = framer.finish(truncated)?;

    if !parts.reasoning.is_empty() {
        message.insert(
            "reasoning_content".to_string(),
            Value::String(parts.reasoning),
        );
    }
    restore_content(message, content_shape, parts.content);
    Ok(())
}

struct StreamState {
    framer: Option<ReasoningFramer>,
    source: String,
    len: usize,
    limit: usize,
    seen: bool,
    finish_reason: Option<String>,
}

impl StreamState {
    fn new(limit: usize) -> Self {
        Self {
            framer: Some(ReasoningFramer::new(limit)),
            source: String::new(),
            len: 0,
            limit,
            seen: false,
            finish_reason: None,
        }
    }

    fn finished(&self) -> bool {
        self.framer.is_none()
    }

    fn record(&mut self, block: &str) -> Result<(), ProtocolError> {
        if self.finished() {
            return Ok(());
        }
        let added = block.len().saturating_add(2);
        if self.source.len().saturating_add(added) > self.limit.saturating_mul(2) {
            return Err(ProtocolError::Overflow);
        }
        self.source.push_str(block);
        self.source.push_str("\n\n");
        Ok(())
    }

    fn normalize(&mut self, value: &mut Value) -> Result<(), ProtocolError> {
        if !self.seen {
            return Err(ProtocolError::Stream);
        }
        if let Some(reason) = self.finish_reason.take() {
            let choice = value
                .pointer_mut("/choices/0")
                .and_then(Value::as_object_mut)
                .ok_or(ProtocolError::Stream)?;
            choice.insert("finish_reason".to_string(), Value::String(reason));
        }
        let framer = self.framer.take().ok_or(ProtocolError::Boundary)?;
        normalize_first_choice_with_framer(value, Some(framer))
    }
}

fn string_len(value: Option<&Value>) -> Result<usize, ProtocolError> {
    match value {
        Some(Value::String(value)) => Ok(value.len()),
        Some(Value::Null) | None => Ok(0),
        Some(_) => Err(ProtocolError::Stream),
    }
}

fn take_stream_tools(payload: &mut Map<String, Value>) -> Result<(bool, usize), ProtocolError> {
    let mut output = false;
    let mut added = 0usize;
    if let Some(calls) = payload.remove("tool_calls") {
        match calls {
            Value::Array(calls) => {
                for call in calls {
                    output = true;
                    let call = call.as_object().ok_or(ProtocolError::Stream)?;
                    added = added.saturating_add(string_len(call.get("id"))?);
                    let function = match call.get("function") {
                        Some(Value::Object(function)) => Some(function),
                        Some(Value::Null) | None => None,
                        Some(_) => return Err(ProtocolError::Stream),
                    };
                    added = added.saturating_add(string_len(
                        function.and_then(|function| function.get("name")),
                    )?);
                    let arguments = function.and_then(|function| function.get("arguments"));
                    added = added.saturating_add(match arguments {
                        Some(Value::String(arguments)) => arguments.len(),
                        Some(Value::Null) | None => 0,
                        Some(_) => return Err(ProtocolError::Tool),
                    });
                }
            }
            Value::Null => {}
            _ => return Err(ProtocolError::Stream),
        }
    }
    if let Some(function) = payload.remove("function_call") {
        match function {
            Value::Object(function) => {
                output = true;
                added = added.saturating_add(string_len(function.get("name"))?);
                added = added.saturating_add(match function.get("arguments") {
                    Some(Value::String(arguments)) => arguments.len(),
                    Some(Value::Null) | None => 0,
                    Some(_) => return Err(ProtocolError::Tool),
                });
            }
            Value::Null => {}
            _ => return Err(ProtocolError::Stream),
        }
    }
    Ok((output, added))
}

fn safe_reasoning_only_chunk(chunk: &Value) -> Result<bool, ProtocolError> {
    let Some(choice) = chunk
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
    else {
        return Ok(false);
    };
    let terminal = optional_string(choice.get("finish_reason"))?
        .as_deref()
        .is_some_and(|reason| !reason.trim().is_empty());
    if terminal {
        return Ok(false);
    }
    let payload = if choice.get("delta").is_some_and(Value::is_object) {
        choice.get("delta")
    } else if choice.get("message").is_some_and(Value::is_object) {
        choice.get("message")
    } else {
        None
    }
    .and_then(Value::as_object);
    let Some(payload) = payload else {
        return Ok(false);
    };
    let Some(reasoning) = extract_reasoning_field_text(&Value::Object(payload.clone())) else {
        return Ok(false);
    };
    if payload
        .get("content")
        .is_some_and(|value| !matches!(value, Value::Null) && value.as_str() != Some(""))
        || payload
            .get("refusal")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty())
        || payload
            .get("tool_calls")
            .is_some_and(|value| !matches!(value, Value::Null))
        || payload
            .get("function_call")
            .is_some_and(|value| !matches!(value, Value::Null))
        || has_trailing_partial_marker(&reasoning)
    {
        return Ok(false);
    }
    Ok(markers(&reasoning).is_ok_and(|found| found.is_empty()))
}

fn inspect_stream_chunk(chunk: &mut Value, state: &mut StreamState) -> Result<bool, ProtocolError> {
    let Some(choice) = chunk
        .get_mut("choices")
        .and_then(Value::as_array_mut)
        .and_then(|choices| choices.first_mut())
    else {
        return Ok(false);
    };
    let finish_reason = optional_string(choice.get("finish_reason"))?;
    let terminal = finish_reason
        .as_deref()
        .is_some_and(|reason| !reason.trim().is_empty());
    let key = if choice.get("delta").is_some_and(Value::is_object) {
        "delta"
    } else if choice.get("message").is_some_and(Value::is_object) {
        "message"
    } else {
        return Err(ProtocolError::Stream);
    };
    let payload = choice
        .get_mut(key)
        .and_then(Value::as_object_mut)
        .ok_or(ProtocolError::Stream)?;
    let reasoning = extract_reasoning_field_text(&Value::Object(payload.clone()));
    payload.remove("reasoning_content");
    payload.remove("reasoning");
    payload.remove("reasoning_details");
    let (content, content_shape) = take_content(payload)?;
    let (tool_output, tool_len) = take_stream_tools(payload)?;
    let output = reasoning.as_ref().is_some_and(|text| !text.is_empty())
        || content.as_ref().is_some_and(|text| !text.is_empty())
        || tool_output
        || payload
            .get("refusal")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty());
    if state.finished() {
        if terminal && !output {
            restore_content(payload, content_shape, String::new());
            return Ok(true);
        }
        return Err(ProtocolError::Boundary);
    }
    let added = reasoning
        .as_ref()
        .map_or(0, String::len)
        .saturating_add(content.as_ref().map_or(0, String::len))
        .saturating_add(tool_len);
    if state.len.saturating_add(added) > state.limit {
        return Err(ProtocolError::Overflow);
    }
    state.len += added;
    state.seen = true;
    if terminal {
        state.finish_reason = finish_reason;
    }
    let framer = state
        .framer
        .as_mut()
        .expect("unfinished state has a framer");
    if let Some(reasoning) = reasoning {
        framer.push(Channel::Reasoning, &reasoning)?;
    }
    if let Some(content) = content {
        framer.push(Channel::Content, &content)?;
    }
    restore_content(payload, content_shape, String::new());
    Ok(terminal)
}

pub(crate) fn error_event_message(error: &Value) -> Option<String> {
    error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| error.as_str())
        .filter(|message| !message.is_empty())
        .map(str::to_string)
}

fn sanitize_error_field(value: &mut Value) -> bool {
    let reportable = value.get("error").and_then(error_event_message).is_some();
    if !reportable {
        if let Some(object) = value.as_object_mut() {
            object.remove("error");
        }
    }
    reportable
}

enum SseBlock {
    Ignore,
    Done,
    Error,
    Data(Value, bool),
}

fn process_sse_block(block: &str, state: &mut StreamState) -> Result<SseBlock, ProtocolError> {
    let Some((event, data)) = crate::proxy::handlers::sse_block_parts(block, true) else {
        return Ok(SseBlock::Ignore);
    };
    if data.trim().is_empty() {
        return Ok(SseBlock::Ignore);
    }
    if data.trim() == "[DONE]" {
        state.record(block)?;
        return Ok(SseBlock::Done);
    }
    if event == "error" {
        return Ok(SseBlock::Error);
    }
    let mut chunk = serde_json::from_str(&data).map_err(|_| ProtocolError::Stream)?;
    if sanitize_error_field(&mut chunk) {
        return Ok(SseBlock::Error);
    }
    if safe_reasoning_only_chunk(&chunk)? {
        state.seen = true;
        return Ok(SseBlock::Data(chunk, false));
    }
    state.record(block)?;
    let terminal = inspect_stream_chunk(&mut chunk, state)?;
    Ok(SseBlock::Data(chunk, terminal))
}

fn optional_string(value: Option<&Value>) -> Result<Option<String>, ProtocolError> {
    match value {
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(ProtocolError::Stream),
    }
}

fn merge_response_payload(
    mut chunk: Value,
    mut response: Value,
    emit_reasoning: bool,
) -> Result<Value, ProtocolError> {
    let mut message = response
        .pointer_mut("/choices/0/message")
        .and_then(Value::as_object_mut)
        .map(std::mem::take)
        .ok_or(ProtocolError::Stream)?;
    message.remove("role");
    let (content, _) = take_content(&mut message)?;
    if !emit_reasoning {
        message.remove("reasoning_content");
    }

    let choice = chunk
        .pointer_mut("/choices/0")
        .and_then(Value::as_object_mut)
        .ok_or(ProtocolError::Stream)?;
    let key = if choice.get("delta").is_some_and(Value::is_object) {
        "delta"
    } else if choice.get("message").is_some_and(Value::is_object) {
        "message"
    } else {
        return Err(ProtocolError::Stream);
    };
    let payload = choice
        .get_mut(key)
        .and_then(Value::as_object_mut)
        .ok_or(ProtocolError::Stream)?;
    let (_, content_shape) = take_content(payload)?;
    payload.extend(message);
    restore_content(payload, content_shape, content.unwrap_or_default());
    Ok(chunk)
}

fn done_response(response: Value, emit_reasoning: bool) -> Result<Value, ProtocolError> {
    merge_response_payload(
        json!({"choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": Value::Null
        }]}),
        response,
        emit_reasoning,
    )
}

fn normalize_source(state: &mut StreamState) -> Result<Value, ProtocolError> {
    let mut response =
        crate::proxy::handlers::chat_sse_to_response_value_for_normalization(&state.source)
            .map_err(|_| ProtocolError::Stream)?;
    state.normalize(&mut response)?;
    Ok(response)
}

pub(crate) fn normalize_sse_response(body: &str, value: &mut Value) -> Result<(), ProtocolError> {
    let mut buffer = format!("{body}\n\n");
    let mut state = StreamState::new(BUFFER_LIMIT);
    while let Some(block) = take_sse_block(&mut buffer) {
        let was_finished = state.finished();
        match process_sse_block(&block, &mut state)? {
            SseBlock::Done | SseBlock::Data(_, true) if !was_finished => {
                normalize_source(&mut state)?;
            }
            SseBlock::Error => return Err(ProtocolError::Stream),
            SseBlock::Ignore | SseBlock::Done | SseBlock::Data(..) => {}
        }
    }
    if !state.finished() {
        return Err(ProtocolError::Stream);
    }
    normalize_first_choice(value)
}

pub(crate) fn normalize_sse_stream<E: std::error::Error + Send + 'static>(
    stream: impl Stream<Item = Result<Bytes, E>> + Send + 'static,
    enabled: bool,
    emit_reasoning: bool,
) -> impl Stream<Item = Result<Bytes, E>> + Send {
    normalize_sse_stream_with_interval(stream, enabled, emit_reasoning, KEEPALIVE_INTERVAL)
}

fn normalize_sse_stream_with_interval<E: std::error::Error + Send + 'static>(
    stream: impl Stream<Item = Result<Bytes, E>> + Send + 'static,
    enabled: bool,
    emit_reasoning: bool,
    delay: Duration,
) -> impl Stream<Item = Result<Bytes, E>> + Send {
    async_stream::stream! {
        tokio::pin!(stream);
        if !enabled {
            while let Some(item) = stream.next().await {
                yield item;
            }
            return;
        }
        let mut buffer = String::new();
        let mut remainder = Vec::new();
        let mut state = StreamState::new(BUFFER_LIMIT);
        let mut clock = tokio::time::interval_at(tokio::time::Instant::now() + delay, delay);
        clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::task::yield_now().await;
            let item = tokio::select! { biased;
                _ = clock.tick() => {
                    yield Ok(Bytes::from_static(b"event: nexus.keepalive\ndata: {}\n\n"));
                    continue;
                },
                item = stream.next() => item,
            };
            let Some(item) = item else { break };
            let bytes = match item {
                Ok(bytes) => bytes,
                Err(error) => {
                    yield Err(error);
                    return;
                }
            };
            crate::proxy::sse::append_utf8_safe(&mut buffer, &mut remainder, &bytes);
            while let Some(block) = take_sse_block(&mut buffer) {
                let was_finished = state.finished();
                let parsed = match process_sse_block(&block, &mut state) {
                    Ok(value) => value,
                    Err(error) => {
                        yield Ok(error_event(error));
                        return;
                    }
                };
                match parsed {
                    SseBlock::Ignore => {}
                    SseBlock::Error => {
                        yield Ok(Bytes::from(format!("{block}\n\n")));
                        return;
                    }
                    SseBlock::Done => {
                        if !was_finished {
                            match normalize_source(&mut state)
                                .and_then(|response| done_response(response, emit_reasoning))
                            {
                                Ok(chunk) => yield Ok(Bytes::from(format!("data: {chunk}\n\n"))),
                                Err(error) => {
                                    yield Ok(error_event(error));
                                    return;
                                }
                            }
                        }
                        yield Ok(Bytes::from_static(b"data: [DONE]\n\n"));
                        return;
                    }
                    SseBlock::Data(chunk, true) if !was_finished => {
                        match normalize_source(&mut state).and_then(|response| {
                            merge_response_payload(chunk, response, emit_reasoning)
                        }) {
                            Ok(chunk) => yield Ok(Bytes::from(format!("data: {chunk}\n\n"))),
                            Err(error) => {
                                yield Ok(error_event(error));
                                return;
                            }
                        }
                    }
                    SseBlock::Data(chunk, _) => {
                        yield Ok(Bytes::from(format!("data: {chunk}\n\n")));
                    }
                }
            }
        }
        if !state.finished() {
            yield Ok(error_event(ProtocolError::Stream));
        }
    }
}

fn error_event(error: ProtocolError) -> Bytes {
    Bytes::from(format!(
        "event: error\ndata: {}\n\n",
        json!({"error": {"message": error.to_string(), "type": error.code(), "code": error.code()}})
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{stream, StreamExt};

    fn frame(parts: &[(Channel, &str)], truncated: bool) -> Result<Parts, ProtocolError> {
        let mut framer = ReasoningFramer::new(4096);
        for (channel, text) in parts {
            framer.push(*channel, text)?;
        }
        framer.finish(truncated)
    }

    fn expected(reasoning: &str, content: &str) -> Parts {
        Parts {
            reasoning: reasoning.to_string(),
            content: content.to_string(),
        }
    }

    #[test]
    fn boundaries_are_chunk_field_and_markdown_safe() {
        for (open, close) in [("<think>", "</think>"), ("<thinking>", "</thinking>")] {
            let text = format!("{open}Lập luận…{close}\nĐáp án");
            assert_eq!(
                frame(&[(Channel::Content, &text)], false),
                Ok(expected("Lập luận…", "\nĐáp án"))
            );
        }

        assert_eq!(
            frame(&[(Channel::Content, "Xong…</think>\nĐáp án")], false),
            Ok(expected("Xong…", "\nĐáp án"))
        );

        for marker in ["</think>", "</thinking>"] {
            for split in marker
                .char_indices()
                .map(|(index, _)| index)
                .chain([marker.len()])
            {
                assert_eq!(
                    frame(
                        &[
                            (Channel::Content, &format!("plan{}", &marker[..split])),
                            (Channel::Reasoning, &format!("{}answer", &marker[split..])),
                        ],
                        false,
                    ),
                    Ok(expected("plan", "answer")),
                    "marker={marker} split={split}"
                );
            }
        }

        assert_eq!(
            frame(
                &[
                    (Channel::Reasoning, "Before `No raw <think> marker"),
                    (Channel::Content, "` and continued. "),
                    (Channel::Content, "Checkpoint.</think>Final."),
                ],
                false,
            ),
            Ok(expected(
                "Before `No raw <think> marker` and continued. Checkpoint.",
                "Final.",
            ))
        );

        for literal in [
            "Use `</think>` literally.",
            "Use \\</thinking> literally.",
            "```text\n</think>\n```",
            "```text\r\n</think>\r\n```",
            "```text\r</think>\r```",
            "~~~text\n</thinking>\n~~~",
            "~~~text\r\n</thinking>\r\n~~~",
            "```\n``` not-a-close\n</think>\n```",
            "~~~\n~~~ not-a-close\n</thinking>\n~~~",
            "    </think>\nanswer",
        ] {
            assert_eq!(
                frame(&[(Channel::Content, literal)], false),
                Ok(expected("", literal))
            );
        }

        assert_eq!(
            frame(
                &[(Channel::Content, "```bad`info```\nprivate</think>answer",)],
                false,
            ),
            Ok(expected("```bad`info```\nprivate", "answer"))
        );
    }

    #[test]
    fn malformed_boundaries_and_buffer_overflow_fail_closed() {
        for malformed in [
            "<think>unfinished",
            "<think>plan</thinking>answer",
            "`ambiguous </think>",
            "plan</think>answer</think>late",
            "plan</think>answer</thi",
        ] {
            assert_eq!(
                frame(&[(Channel::Content, malformed)], false),
                Err(ProtocolError::Boundary)
            );
        }
        assert_eq!(
            ReasoningFramer::new(3).push(Channel::Content, "abcd"),
            Err(ProtocolError::Overflow)
        );
        assert_eq!(
            frame(&[(Channel::Content, "<think>checkpoint")], true),
            Ok(expected("checkpoint", ""))
        );
    }

    fn normalize_source_value(source: &str) -> Result<Value, ProtocolError> {
        let mut value =
            crate::proxy::handlers::chat_sse_to_response_value_for_normalization(source)
                .map_err(|_| ProtocolError::Stream)?;
        normalize_sse_response(source, &mut value)?;
        Ok(value)
    }

    #[test]
    fn accumulated_tool_arguments_are_repaired_and_validated() {
        let repaired = normalize_source_value(concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"private\",\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"read\",\"arguments\":\"</thi\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"nk>{\\\"path\\\":\\\"a\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n"
        ))
        .unwrap();
        let message = &repaired["choices"][0]["message"];
        assert_eq!(message["reasoning_content"], "private");
        assert_eq!(message["content"], "");
        assert_eq!(
            message["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"a\"}"
        );

        let malformed = normalize_source_value(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"name\":\"read\",\"arguments\":\"{\\\"broken\\\":\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
        );
        assert_eq!(malformed, Err(ProtocolError::Tool));
        assert_eq!(ProtocolError::Tool.code(), "upstream_tool_protocol_error");

        let literal = "{\"literal\":\"</think>\"}";
        assert_eq!(validate_tool_arguments(literal), Ok(()));

        let repeated = "tool call\n".repeat(32);
        let source = format!(
            "data: {}\n\n",
            json!({"choices": [{"delta": {
                "reasoning_content": repeated,
                "tool_calls": [{"index": 0, "function": {"name": "read", "arguments": "{}"}}]
            }, "finish_reason": "tool_calls"}]})
        );
        let valid = normalize_source_value(&source).unwrap();
        assert_eq!(
            valid["choices"][0]["message"]["reasoning_content"],
            repeated
        );

        let fragments = normalize_source_value(concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"function\":{\"name\":\"read\",\"arguments\":\"{\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"\",\"function\":{\"name\":\"\",\"arguments\":\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n"
        ))
        .unwrap();
        let tool = &fragments["choices"][0]["message"]["tool_calls"][0];
        assert_eq!(tool["id"], "call-1");
        assert_eq!(tool["function"]["name"], "read");
        assert_eq!(tool["function"]["arguments"], "{}");

        let mut bounded = StreamState::new(3);
        let mut chunk = json!({"choices": [{"delta": {"content": "abcd"}}]});
        assert!(matches!(
            inspect_stream_chunk(&mut chunk, &mut bounded),
            Err(ProtocolError::Overflow)
        ));

        assert_eq!(
            normalize_source_value(
                "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n"
            ),
            Err(ProtocolError::Tool)
        );

        let missing_name = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n";
        let mut synthesized = json!({"choices": [{
            "message": {"tool_calls": [{"function": {
                "name": "unknown_tool",
                "arguments": "{}"
            }}]},
            "finish_reason": "tool_calls"
        }]});
        assert_eq!(
            normalize_sse_response(missing_name, &mut synthesized),
            Err(ProtocolError::Tool)
        );
    }

    #[test]
    fn trailing_reasoning_close_before_tool_call_is_not_visible() {
        let prose = "Focus on the logical structure.";
        let source = format!(
            "data: {}\n\ndata: {}\n\n",
            json!({"choices": [{"delta": {"content": format!("{prose}</think>")}}]}),
            json!({"choices": [{"delta": {"tool_calls": [{
                "index": 0,
                "id": "call-1",
                "function": {"name": "spawn_agent", "arguments": "{}"}
            }]}, "finish_reason": "tool_calls"}]})
        );

        let normalized = normalize_source_value(&source).unwrap();
        let message = &normalized["choices"][0]["message"];
        assert_eq!(message["reasoning_content"], prose);
        assert_eq!(message["content"], "");
        assert_eq!(message["tool_calls"][0]["function"]["name"], "spawn_agent");
        assert!(!normalized.to_string().contains("</think>"));
    }

    #[test]
    fn nonstream_normalizes_first_choice_and_legacy_tool_boundaries() {
        let mut response = json!({"choices": [{"message": {
            "reasoning_content": "private",
            "content": " continued</think>answer",
            "function_call": {"name": "read", "arguments": "{}"}
        }, "finish_reason": "function_call"}]});
        normalize_first_choice(&mut response).unwrap();
        let message = &response["choices"][0]["message"];
        assert_eq!(message["reasoning_content"], "private continued");
        assert_eq!(message["content"], "answer");
        assert_eq!(message["function_call"]["arguments"], "{}");

        let mut malformed = json!({"choices": [{"message": {
            "content": " leaked tail",
            "tool_calls": [{"function": {"arguments": "{\"broken\":"}}]
        }, "finish_reason": "tool_calls"}]});
        assert_eq!(
            normalize_first_choice(&mut malformed),
            Err(ProtocolError::Tool)
        );

        let content = json!([
            {"type": "text", "text": "Use `</think>` literally."},
            {"type": "refusal", "refusal": "No."}
        ]);
        let mut array = json!({"choices": [{"message": {
            "reasoning_content": "private",
            "content": content.clone()
        }, "finish_reason": "stop"}]});
        normalize_first_choice(&mut array).unwrap();
        assert_eq!(array["choices"][0]["message"]["content"], content);

        let mut malformed_call = json!({"choices": [{"message": {
            "content": null,
            "tool_calls": [{}]
        }, "finish_reason": "tool_calls"}]});
        assert_eq!(
            normalize_first_choice(&mut malformed_call),
            Err(ProtocolError::Tool)
        );

        let mut missing_call = json!({"choices": [{
            "message": {"content": null},
            "finish_reason": "tool_calls"
        }]});
        assert_eq!(
            normalize_first_choice(&mut missing_call),
            Err(ProtocolError::Tool)
        );
    }

    async fn normalized_stream(input: &str, enabled: bool) -> String {
        normalized_chunks(vec![Bytes::copy_from_slice(input.as_bytes())], enabled).await
    }

    async fn normalized_chunks(chunks: Vec<Bytes>, enabled: bool) -> String {
        let source = stream::iter(chunks.into_iter().map(Ok::<_, std::io::Error>));
        let bytes = normalize_sse_stream(source, enabled, true)
            .map(|item| item.unwrap())
            .collect::<Vec<_>>()
            .await
            .concat();
        String::from_utf8(bytes).unwrap()
    }

    #[tokio::test]
    async fn disabled_stream_is_byte_for_byte_passthrough() {
        let input = "first\0chunk\nsecond";
        assert_eq!(normalized_stream(input, false).await, input);
    }

    #[tokio::test]
    async fn stream_accepts_bom_and_empty_error_placeholders() {
        for prefix in ["", "\u{feff}"] {
            let input = format!(
                "{prefix}data: {{\"error\":{{}},\"choices\":[{{\"delta\":{{\"content\":\"plan</think>answer\"}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n"
            );
            let output = normalized_stream(&input, true).await;
            assert!(output.contains(r#""reasoning_content":"plan""#), "{output}");
            assert!(output.contains(r#""content":"answer""#), "{output}");
            assert!(!output.contains("event: error"), "{output}");
        }
    }

    #[tokio::test]
    async fn stream_preserves_split_bytes_fields_and_markers() {
        let input = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"plan</thi\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"nk>answer\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let split = input.find("reasoning_content").unwrap() + 5;
        let output = normalized_chunks(
            vec![
                Bytes::copy_from_slice(&input.as_bytes()[..split]),
                Bytes::copy_from_slice(&input.as_bytes()[split..]),
            ],
            true,
        )
        .await;
        assert!(output.contains(r#""reasoning_content":"plan""#), "{output}");
        assert!(output.contains(r#""content":"answer""#), "{output}");
        assert!(!output.contains("</think>"), "{output}");

        let legacy = normalized_stream(
            "data: {\"choices\":[{\"delta\":{\"function_call\":{\"name\":\"read\",\"arguments\":\"{}\"}},\"finish_reason\":\"function_call\"}]}\n\n",
            true,
        )
        .await;
        assert!(legacy.contains(r#""function_call""#), "{legacy}");
        assert!(legacy.contains(r#""name":"read""#), "{legacy}");
        assert!(legacy.contains(r#""arguments":"{}""#), "{legacy}");
        assert!(!legacy.contains("tool_calls"), "{legacy}");
    }

    #[test]
    fn pure_reasoning_deltas_do_not_consume_boundary_buffer() {
        let mut state = StreamState::new(3);
        let block = format!(
            "data: {}\n\n",
            json!({"choices": [{"delta": {"reasoning_content": "abcd"}}]})
        );

        let parsed = process_sse_block(&block, &mut state).unwrap();

        assert!(matches!(parsed, SseBlock::Data(_, false)));
        assert_eq!(state.len, 0);
        assert!(state.source.is_empty());
    }

    #[tokio::test]
    async fn stream_forwards_safe_reasoning_deltas_before_terminal_chunk() {
        let output = normalized_stream(
            concat!(
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"abcd\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n"
            ),
            true,
        )
        .await;

        assert!(output.contains(r#""reasoning_content":"abcd""#), "{output}");
        assert!(output.contains(r#""finish_reason":"stop""#), "{output}");
        assert!(!output.contains("event: error"), "{output}");
    }

    #[tokio::test]
    async fn stream_preserves_terminal_shape_and_mixed_tool_forms() {
        let output = normalized_stream(
            "data: {\"id\":\"terminal\",\"model\":\"m\",\"vendor\":\"keep\",\"choices\":[{\"index\":0,\"message\":{\"role\":\"assistant\",\"content\":null,\"vendor\":\"keep\",\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"function\":{\"name\":\"modern\",\"arguments\":\"{}\"}}],\"function_call\":{\"name\":\"legacy\",\"arguments\":\"{}\"}},\"finish_reason\":\"tool_calls\"}]}\n\n",
            true,
        )
        .await;
        assert!(output.contains(r#""id":"terminal""#), "{output}");
        assert!(output.contains(r#""model":"m""#), "{output}");
        assert!(output.contains(r#""vendor":"keep""#), "{output}");
        assert!(output.contains(r#""message""#), "{output}");
        assert!(!output.contains(r#""delta""#), "{output}");
        assert!(output.contains(r#""name":"modern""#), "{output}");
        assert!(output.contains(r#""name":"legacy""#), "{output}");
        assert!(output.contains(r#""content":null"#), "{output}");
    }

    #[tokio::test]
    async fn stream_accepts_both_valid_terminal_conventions() {
        let without_done = normalized_stream(
            "data: {\"choices\":[{\"delta\":{\"content\":\"plan</think>answer\"},\"finish_reason\":\"stop\"}]}\n\n",
            true,
        )
        .await;
        assert!(
            without_done.contains(r#""content":"answer""#),
            "{without_done}"
        );
        assert!(!without_done.contains("event: error"), "{without_done}");

        let done_only = normalized_stream(
            "data: {\"choices\":[{\"delta\":{\"content\":\"plan</think>answer\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n",
            true,
        )
        .await;
        assert!(done_only.contains(r#""content":"answer""#), "{done_only}");
        assert!(done_only.contains("data: [DONE]"), "{done_only}");
        assert!(!done_only.contains("event: error"), "{done_only}");

        let empty = normalized_stream("data: [DONE]\n\n", true).await;
        assert!(empty.contains("stream_protocol_error"), "{empty}");
    }

    #[tokio::test]
    async fn keepalive_is_not_starved_by_a_continuously_ready_stream() {
        assert_eq!(KEEPALIVE_INTERVAL, Duration::from_secs(30));
        let source = stream::poll_fn(|_| {
            std::task::Poll::Ready(Some(Ok::<_, std::io::Error>(Bytes::from_static(
                b": upstream keepalive\n\n",
            ))))
        });
        let normalized =
            normalize_sse_stream_with_interval(source, true, true, Duration::from_millis(1));
        tokio::pin!(normalized);
        let item = tokio::time::timeout(Duration::from_secs(1), normalized.next())
            .await
            .expect("keepalive was starved")
            .unwrap()
            .unwrap();
        assert_eq!(
            item,
            Bytes::from_static(b"event: nexus.keepalive\ndata: {}\n\n")
        );
    }

    #[test]
    fn activation_requires_nexus_or_an_explicit_override() {
        fn provider(provider_type: Option<&str>, override_enabled: Option<bool>) -> Provider {
            let mut provider = Provider::with_id("p".into(), "test".into(), json!({}), None);
            provider.meta = Some(crate::provider::ProviderMeta {
                provider_type: provider_type.map(str::to_string),
                local_proxy_request_overrides: override_enabled.map(|enabled| {
                    crate::provider::LocalProxyRequestOverrides {
                        body: Some(json!({
                            "chat_template_kwargs": {"enable_thinking": enabled}
                        })),
                        ..Default::default()
                    }
                }),
                ..Default::default()
            });
            provider
        }

        let enabled = json!({"chat_template_kwargs": {"enable_thinking": true}});
        assert!(enabled_for_attempt(
            &provider(Some("nexus"), None),
            &enabled
        ));
        assert!(!enabled_for_attempt(&provider(None, None), &enabled));
        assert!(enabled_for_attempt(
            &provider(Some("nexus"), Some(true)),
            &json!({})
        ));
        assert!(enabled_for_attempt(&provider(None, Some(true)), &json!({})));
        assert!(!enabled_for_attempt(
            &provider(Some("nexus"), Some(false)),
            &enabled
        ));
        assert!(!enabled_for_attempt(&provider(None, Some(false)), &enabled));
        assert!(!enabled_for_attempt(
            &provider(Some("github_copilot"), Some(true)),
            &json!({})
        ));
    }
}
