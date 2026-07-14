use super::codex_chat_common::extract_reasoning_field_text;
use crate::{
    provider::{CodexChatReasoningConfig, Provider},
    proxy::sse::{append_utf8_safe, strip_sse_field, take_sse_block},
};
use bytes::Bytes;
use futures::{Stream, StreamExt};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

const TRUNCATED: &str = "upstream output ended at a partial reasoning delimiter";
const LATE_OUTPUT: &str = "upstream output contains data after finish_reason";
const TAGS: [(&str, bool); 4] = [
    ("</thinking>", false),
    ("</think>", false),
    ("<thinking>", true),
    ("<think>", true),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReasoningPolicy {
    ToolPlaceholder,
    GlmBoundary,
}

impl ReasoningPolicy {
    pub fn resolve(config: Option<&CodexChatReasoningConfig>) -> Self {
        let Some(config) = config else {
            return Self::ToolPlaceholder;
        };
        if config.supports_thinking == Some(false)
            || !config
                .output_format
                .as_deref()
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("reasoning_content"))
        {
            return Self::ToolPlaceholder;
        }
        if config.thinking_param.as_deref().is_some_and(|value| {
            value
                .trim()
                .eq_ignore_ascii_case("chat_template_kwargs.enable_thinking")
        }) {
            Self::GlmBoundary
        } else {
            Self::ToolPlaceholder
        }
    }

    pub fn from_provider(provider: &Provider) -> Self {
        if provider.settings_config["nexusCapabilities"]["reasoningBoundary"] == "think_close" {
            return Self::GlmBoundary;
        }
        Self::resolve(
            provider
                .meta
                .as_ref()
                .and_then(|meta| meta.codex_chat_reasoning.as_ref()),
        )
    }

    pub fn adapts_boundaries(self) -> bool {
        self == Self::GlmBoundary
    }
}

#[derive(Default)]
pub(crate) struct ReasoningFraming {
    reasoning: String,
    content: String,
    pending_markdown: Option<PendingMarkdown>,
    line_has_text: bool,
    opener_checked: usize,
    opener_decided: bool,
    explicit_open: bool,
    reasoning_emitted: bool,
    sent_output: bool,
    had_output: bool,
    tool_started: bool,
    finished: bool,
}

#[derive(Clone, Copy)]
struct PendingMarkdown {
    kind: PendingKind,
    checked: usize,
    observed: usize,
}

#[derive(Clone, Copy)]
enum PendingKind {
    Ticks(u8),
    Inline { width: usize, trailing: usize },
    Fence(u8, usize),
    Indented,
}

impl PendingMarkdown {
    fn shifted(mut self, bytes: usize) -> Self {
        self.checked -= bytes;
        self.observed -= bytes;
        self
    }
}

impl ReasoningFraming {
    pub(crate) fn adapt_fields(
        &mut self,
        reasoning: &mut Option<String>,
        content: &mut Option<String>,
        has_tool: bool,
        finish: bool,
    ) -> Result<(), &'static str> {
        if self.finished
            && (reasoning.as_deref().is_some_and(|text| !text.is_empty())
                || content.as_deref().is_some_and(|text| !text.is_empty())
                || has_tool)
        {
            return Err(LATE_OUTPUT);
        }
        let has_reasoning = reasoning.as_deref().is_some_and(|text| !text.is_empty());
        let has_content = content.as_deref().is_some_and(|text| !text.is_empty());
        self.had_output |= has_reasoning || has_content || has_tool;
        if self.tool_started && (has_reasoning || has_content) {
            return Err(LATE_OUTPUT);
        }
        if !self.content.is_empty() && has_reasoning {
            self.reasoning.push_str(&self.content);
            self.content.clear();
        }
        self.reasoning
            .push_str(reasoning.as_deref().unwrap_or_default());
        self.content
            .push_str(content.as_deref().unwrap_or_default());
        self.finished |= finish;
        let output = if has_tool || finish {
            let output = split_reasoning_with_state(
                &self.reasoning,
                &self.content,
                self.explicit_open,
                self.reasoning_emitted,
            )?;
            self.reasoning.clear();
            self.content.clear();
            self.pending_markdown = None;
            self.line_has_text = false;
            self.opener_checked = 0;
            self.opener_decided = false;
            self.explicit_open = false;
            self.reasoning_emitted = false;
            Some(output)
        } else {
            if !self.prepare_opener() {
                *reasoning = None;
                *content = None;
                return Ok(());
            }
            let (emit, pending, line_has_text) = safe_reasoning_prefix(
                &self.reasoning,
                self.pending_markdown.take(),
                self.line_has_text,
            );
            self.pending_markdown = pending.map(|pending| pending.shifted(emit));
            self.line_has_text = line_has_text;
            if emit > 0 {
                let reasoning = self.reasoning.drain(..emit).collect();
                self.reasoning_emitted = true;
                Some((reasoning, String::new()))
            } else {
                None
            }
        };
        self.tool_started |= has_tool;
        self.sent_output |= has_tool
            || output
                .as_ref()
                .is_some_and(|(reasoning, content)| !reasoning.is_empty() || !content.is_empty());
        *reasoning = None;
        *content = None;
        if let Some((reasoning_out, content_out)) = output {
            *reasoning = (!reasoning_out.is_empty()).then_some(reasoning_out);
            *content = (!content_out.is_empty()).then_some(content_out);
        }
        Ok(())
    }

    pub(crate) fn validate_done(&self) -> Result<(), &'static str> {
        (self.reasoning.is_empty()
            && self.content.is_empty()
            && self.pending_markdown.is_none()
            && !self.explicit_open)
            .then_some(())
            .ok_or(TRUNCATED)
    }

    fn prepare_opener(&mut self) -> bool {
        if self.opener_decided {
            return true;
        }
        while self.opener_checked < self.reasoning.len() {
            let character = self.reasoning[self.opener_checked..]
                .chars()
                .next()
                .unwrap();
            if !character.is_whitespace() {
                break;
            }
            self.opener_checked += character.len_utf8();
        }
        let candidate = &self.reasoning[self.opener_checked..];
        if candidate.is_empty() {
            return false;
        }
        if let Some(length) = ["<thinking>", "<think>"]
            .into_iter()
            .find(|opener| candidate.starts_with(opener))
            .map(str::len)
        {
            self.reasoning.drain(..self.opener_checked + length);
            self.explicit_open = true;
        } else if ["<thinking>", "<think>"]
            .into_iter()
            .any(|opener| opener.starts_with(candidate))
        {
            return false;
        }
        self.opener_checked = 0;
        self.opener_decided = true;
        true
    }
}

// Retain enough of the current line to classify a later Markdown delimiter.
// Ordinary prose streams with a one-character delay; indented code stays whole.
fn safe_reasoning_prefix(
    text: &str,
    pending: Option<PendingMarkdown>,
    line_has_text: bool,
) -> (usize, Option<PendingMarkdown>, bool) {
    let (bytes, limit) = (text.as_bytes(), text.len());
    if let Some(mut pending) = pending {
        let complete = match pending.kind {
            PendingKind::Ticks(symbol) => {
                text[pending.observed..].bytes().any(|byte| byte != symbol)
            }
            PendingKind::Inline {
                width,
                ref mut trailing,
            } => {
                let (end, tail) = inline_code_end(text, pending.observed, width, limit, *trailing);
                *trailing = tail;
                end.is_some()
            }
            PendingKind::Fence(symbol, width) => {
                text[pending.observed..].contains('\n')
                    && fence_end_from(text, pending.checked, symbol, width, limit).is_some()
            }
            PendingKind::Indented => text[pending.observed..].contains('\n'),
        };
        if !complete {
            pending.checked = match pending.kind {
                PendingKind::Fence(_, _) => text[pending.observed..]
                    .rfind('\n')
                    .map_or(pending.checked, |at| pending.observed + at + 1),
                _ => limit,
            };
            pending.observed = limit;
            return (0, Some(pending), line_has_text);
        }
    }
    let (mut i, mut line) = (0, (!line_has_text).then_some(0));
    while i < limit {
        if line == Some(i) && indented_line(bytes, i, limit) {
            let Some(end) = text[i..].find('\n') else {
                return (
                    i,
                    Some(PendingMarkdown {
                        kind: PendingKind::Indented,
                        checked: limit,
                        observed: limit,
                    }),
                    false,
                );
            };
            i += end + 1;
            line = Some(i);
            continue;
        }
        if bytes[i] == b'\\' {
            if i + 1 == limit {
                return (i, None, line.is_none());
            }
            let escaped = i + 1;
            i = escaped + text[escaped..].chars().next().unwrap().len_utf8();
            line = (bytes[escaped] == b'\n').then_some(i);
            continue;
        }
        if matches!(bytes[i], b'`' | b'~') {
            let symbol = bytes[i];
            let width = run_width(bytes, i, limit, symbol);
            if i + width == limit {
                let retain = line.unwrap_or(i);
                return (
                    retain,
                    Some(PendingMarkdown {
                        kind: PendingKind::Ticks(symbol),
                        checked: i,
                        observed: limit,
                    }),
                    line.is_none(),
                );
            }
            let fenced = line.is_some_and(|line| {
                width >= 3 && i - line <= 3 && text[line..i].bytes().all(|byte| byte == b' ')
            });
            if fenced {
                let start = i;
                let Some(end) = fence_end(text, i, symbol, width, limit) else {
                    return (
                        line.unwrap(),
                        Some(PendingMarkdown {
                            kind: PendingKind::Fence(symbol, width),
                            checked: last_line_start(text, i, limit),
                            observed: limit,
                        }),
                        false,
                    );
                };
                if end == limit && !text.ends_with('\n') {
                    return (
                        line.unwrap(),
                        Some(PendingMarkdown {
                            kind: PendingKind::Fence(symbol, width),
                            checked: last_line_start(text, i, limit),
                            observed: limit,
                        }),
                        false,
                    );
                }
                i = end;
                line = text[start..i].ends_with('\n').then_some(i);
                continue;
            }
            if symbol == b'`' {
                let (end, trailing) = inline_code_end(text, i + width, width, limit, 0);
                let Some(end) = end else {
                    let retain = line.unwrap_or(i);
                    return (
                        retain,
                        Some(PendingMarkdown {
                            kind: PendingKind::Inline { width, trailing },
                            checked: limit,
                            observed: limit,
                        }),
                        line.is_none(),
                    );
                };
                i = end;
                line = None;
                continue;
            }
            line = None;
            i += width;
            continue;
        }
        if bytes[i] == b'<' {
            let rest = &text[i..];
            if TAGS
                .iter()
                .any(|(tag, _)| rest.starts_with(tag) || tag.starts_with(rest))
            {
                return (i, None, line.is_none());
            }
        }
        let width = text[i..].chars().next().unwrap().len_utf8();
        if bytes[i] == b'\n' {
            line = Some(i + width);
        } else if bytes[i] != b' ' {
            line = None;
        }
        i += width;
    }
    (line.unwrap_or(limit), None, line.is_none())
}

fn inline_code_end(
    text: &str,
    mut at: usize,
    width: usize,
    limit: usize,
    mut trailing: usize,
) -> (Option<usize>, usize) {
    let bytes = text.as_bytes();
    while at < limit {
        if bytes[at] == b'`' {
            let run = run_width(bytes, at, limit, b'`');
            trailing += run;
            at += run;
            if at < limit {
                if trailing == width {
                    return (Some(at), 0);
                }
                trailing = 0;
            }
        } else {
            if trailing == width {
                return (Some(at), 0);
            }
            trailing = 0;
            at += text[at..].chars().next().unwrap().len_utf8();
        }
    }
    (None, trailing)
}

fn last_line_start(text: &str, floor: usize, limit: usize) -> usize {
    text[floor..limit]
        .rfind('\n')
        .map_or(floor, |at| floor + at + 1)
}

fn run_width(bytes: &[u8], at: usize, limit: usize, symbol: u8) -> usize {
    bytes[at..limit]
        .iter()
        .take_while(|byte| **byte == symbol)
        .count()
}

fn indented_line(bytes: &[u8], start: usize, limit: usize) -> bool {
    let spaces = run_width(bytes, start, limit, b' ');
    spaces >= 4 || (start + spaces < limit && bytes[start + spaces] == b'\t')
}

#[cfg(test)]
fn adapt_chat_chunk(chunk: &mut Value, framing: &mut ReasoningFraming) -> Result<(), &'static str> {
    let Some(choice) = chunk.pointer_mut("/choices/0") else {
        return Ok(());
    };
    adapt_choice(choice, framing, false)
}

type ChoiceFramings = BTreeMap<u64, ReasoningFraming>;

fn adapt_chat_choices(
    chunk: &mut Value,
    framings: &mut ChoiceFramings,
    flatten_messages: bool,
) -> Result<(), &'static str> {
    let Some(choices) = chunk.get_mut("choices").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for (position, choice) in choices.iter_mut().enumerate() {
        if !choice.is_object() {
            continue;
        }
        let index = choice["index"].as_u64().unwrap_or(position as u64);
        adapt_choice(choice, framings.entry(index).or_default(), flatten_messages)?;
    }
    Ok(())
}

fn adapt_choice(
    choice: &mut Value,
    framing: &mut ReasoningFraming,
    flatten_message: bool,
) -> Result<(), &'static str> {
    let finish = choice["finish_reason"].as_str().is_some();
    let delta_nonempty = choice["delta"]
        .as_object()
        .is_some_and(|delta| !delta.is_empty());
    let use_message = choice["message"].is_object() && !delta_nonempty;
    let field = if use_message { "message" } else { "delta" };
    if finish && !choice[field].is_object() {
        choice[field] = Value::Object(Default::default());
    }
    let Some(payload) = choice.get_mut(field) else {
        return Ok(());
    };
    let has_tool = payload["tool_calls"]
        .as_array()
        .is_some_and(|calls| !calls.is_empty())
        || payload["function_call"].is_object();
    if !use_message {
        return adapt_value_fields(payload, framing, has_tool, finish, true);
    }

    if flatten_message && (framing.sent_output || framing.finished || framing.tool_started) {
        return Err(LATE_OUTPUT);
    }
    let mut snapshot = ReasoningFraming::default();
    adapt_value_fields(payload, &mut snapshot, has_tool, finish, false)?;
    if flatten_message {
        let delta = payload.take();
        choice.as_object_mut().unwrap().remove("message");
        choice["delta"] = delta;
    }
    *framing = snapshot;
    Ok(())
}

fn adapt_value_fields(
    value: &mut Value,
    framing: &mut ReasoningFraming,
    has_tool: bool,
    finish: bool,
    streaming: bool,
) -> Result<(), &'static str> {
    let mut reasoning = extract_reasoning_field_text(value);
    let mut content = value["content"].as_str().map(str::to_string);
    let had_content = content.is_some();
    let has_opaque_content = value
        .get("content")
        .is_some_and(|content| !content.is_string() && value_is_meaningful(content));
    if !streaming
        && !finish
        && (reasoning.as_deref().is_some_and(|text| !text.is_empty())
            || content.as_deref().is_some_and(|text| !text.is_empty()))
    {
        return Err(TRUNCATED);
    }
    framing.adapt_fields(&mut reasoning, &mut content, has_tool, finish)?;
    framing.had_output |= has_opaque_content;
    framing.sent_output |= has_opaque_content;
    if let Some(object) = value.as_object_mut() {
        for field in ["reasoning_content", "reasoning", "reasoning_details"] {
            object.remove(field);
        }
        if streaming && had_content {
            object.remove("content");
        }
    }
    if let Some(reasoning) = reasoning {
        value["reasoning_content"] = reasoning.into();
    }
    match content {
        Some(content) => value["content"] = content.into(),
        None if !streaming && had_content => value["content"] = Value::Null,
        None => {}
    }
    Ok(())
}

/// Normalize Chat Completions reasoning framing once before a protocol-specific
/// converter consumes the stream. This keeps Codex and Anthropic lifecycle code
/// independent from GLM boundary handling.
pub fn adapt_chat_sse_stream<E: std::error::Error + Send + 'static>(
    stream: impl Stream<Item = Result<Bytes, E>> + Send + 'static,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send {
    adapt_chat_sse_stream_with_errors(stream, false)
}

pub fn adapt_chat_passthrough_sse_stream<E: std::error::Error + Send + 'static>(
    stream: impl Stream<Item = Result<Bytes, E>> + Send + 'static,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send {
    adapt_chat_sse_stream_with_errors(stream, true)
}

fn adapt_chat_sse_stream_with_errors<E: std::error::Error + Send + 'static>(
    stream: impl Stream<Item = Result<Bytes, E>> + Send + 'static,
    passthrough: bool,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send {
    async_stream::stream! {
        let mut buffer = String::new();
        let mut utf8_remainder = Vec::new();
        let mut framings = ChoiceFramings::new();
        tokio::pin!(stream);

        while let Some(chunk) = stream.next().await {
            let bytes = match chunk {
                Ok(bytes) => bytes,
                Err(error) => {
                    yield Err(std::io::Error::other(error.to_string()));
                    return;
                }
            };
            append_utf8_safe(&mut buffer, &mut utf8_remainder, &bytes);
            while let Some(block) = take_sse_block(&mut buffer) {
                let event = block
                    .lines()
                    .find_map(|line| strip_sse_field(line, "event"))
                    .map(str::trim)
                    .map(str::to_string);
                let data = block
                    .lines()
                    .filter_map(|line| strip_sse_field(line, "data"))
                    .collect::<Vec<_>>()
                    .join("\n");
                if data.trim() == "[DONE]" {
                    match terminal_chunk(&mut framings) {
                        Ok(Some(chunk)) => yield Ok(chunk),
                        Ok(None) => {}
                        Err(error) => {
                            yield Err(std::io::Error::new(std::io::ErrorKind::InvalidData, error));
                            return;
                        }
                    }
                    yield Ok(Bytes::from("data: [DONE]\n\n"));
                    return;
                }

                if event.as_deref().is_some_and(|event| event.eq_ignore_ascii_case("error")) {
                    if passthrough {
                        yield Ok(Bytes::from(format!("{block}\n\n")));
                        return;
                    }
                    let error = serde_json::from_str(&data)
                        .unwrap_or_else(|_| Value::String(data.clone()));
                    yield Err(std::io::Error::other(upstream_error_message(&error)));
                    return;
                }
                let Ok(mut chunk) = serde_json::from_str::<Value>(&data) else {
                    yield Ok(Bytes::from(format!("{block}\n\n")));
                    continue;
                };
                if chunk.get("error").is_some_and(value_is_meaningful) {
                    if passthrough {
                        yield Ok(Bytes::from(format!("{block}\n\n")));
                        return;
                    }
                    yield Err(std::io::Error::other(upstream_error_message(&chunk)));
                    return;
                }
                if let Some(object) = chunk.as_object_mut() {
                    object.remove("error");
                }
                if let Err(error) = adapt_chat_choices(&mut chunk, &mut framings, !passthrough) {
                    yield Err(std::io::Error::new(std::io::ErrorKind::InvalidData, error));
                    return;
                }
                yield Ok(chat_sse(event.as_deref(), chunk));
            }
        }

        if !utf8_remainder.is_empty() || !buffer.trim().is_empty() {
            yield Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, TRUNCATED));
            return;
        }
        if framings.is_empty()
            || framings
                .values()
                .any(|framing| !framing.finished || framing.validate_done().is_err())
        {
            yield Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, TRUNCATED));
        }
    }
}

fn terminal_chunk(framings: &mut ChoiceFramings) -> Result<Option<Bytes>, &'static str> {
    if framings.is_empty() {
        return Err(TRUNCATED);
    }
    let mut choices = Vec::new();
    for (index, framing) in framings {
        if framing.finished {
            framing.validate_done()?;
            continue;
        }
        if !framing.had_output {
            return Err(TRUNCATED);
        }
        let (mut reasoning, mut content) = (None, None);
        framing.adapt_fields(&mut reasoning, &mut content, false, true)?;
        let mut delta = serde_json::Map::new();
        if let Some(reasoning) = reasoning {
            delta.insert("reasoning_content".into(), reasoning.into());
        }
        if let Some(content) = content {
            delta.insert("content".into(), content.into());
        }
        choices.push(serde_json::json!({
            "index": index,
            "delta": delta,
            "finish_reason": if framing.tool_started { "tool_calls" } else { "stop" }
        }));
    }
    Ok((!choices.is_empty()).then(|| chat_sse(None, serde_json::json!({"choices": choices}))))
}

fn value_is_meaningful(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(false) => false,
        Value::Number(value) => value.as_f64() != Some(0.0),
        Value::String(value) => !value.trim().is_empty(),
        Value::Array(values) => values.iter().any(value_is_meaningful),
        Value::Object(values) => values.values().any(value_is_meaningful),
        _ => true,
    }
}

fn upstream_error_message(chunk: &Value) -> String {
    let error = chunk.get("error").unwrap_or(chunk);
    error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| error.as_str())
        .filter(|message| !message.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("upstream SSE error: {error}"))
}

fn chat_sse(event: Option<&str>, chunk: Value) -> Bytes {
    let event = event.map_or_else(String::new, |event| format!("event: {event}\n"));
    Bytes::from(format!("{event}data: {chunk}\n\n"))
}

pub fn adapt_chat_completion(body: &mut Value) -> Result<(), &'static str> {
    let Some(choices) = body.get_mut("choices").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for choice in choices {
        let finish = choice["finish_reason"].as_str().is_some();
        let Some(message) = choice.get_mut("message") else {
            continue;
        };
        let has_tool = message["tool_calls"]
            .as_array()
            .is_some_and(|calls| !calls.is_empty())
            || message["function_call"].is_object();
        let mut framing = ReasoningFraming::default();
        adapt_value_fields(message, &mut framing, has_tool, finish, false)?;
    }
    Ok(())
}

#[cfg(test)]
fn split_reasoning(reasoning: &str, content: &str) -> Result<(String, String), &'static str> {
    split_reasoning_with_state(reasoning, content, false, false)
}

fn split_reasoning_with_state(
    reasoning: &str,
    content: &str,
    mut explicit_open: bool,
    reasoning_emitted: bool,
) -> Result<(String, String), &'static str> {
    #[derive(Clone, Copy, PartialEq)]
    enum Phase {
        Reasoning,
        Prefix,
        Content,
    }
    fn append(phase: Phase, text: &str, out: &mut [String; 3]) {
        out[match phase {
            Phase::Reasoning => 0,
            Phase::Prefix => 1,
            Phase::Content => 2,
        }]
        .push_str(text);
    }

    let mut text = format!("{reasoning}{content}");
    let mut boundary = reasoning.len();
    if !explicit_open && !reasoning_emitted {
        let leading = text.len() - text.trim_start().len();
        let candidate = &text[leading..];
        if let Some(length) = ["<thinking>", "<think>"]
            .into_iter()
            .find(|opener| candidate.starts_with(opener))
            .map(str::len)
        {
            text.drain(..leading + length);
            boundary = boundary.saturating_sub(leading + length);
            explicit_open = true;
        }
    }
    let bytes = text.as_bytes();
    let mut out = [String::new(), String::new(), String::new()];
    let mut phase = if boundary == 0 && !explicit_open {
        Phase::Prefix
    } else {
        Phase::Reasoning
    };
    let (mut i, mut cursor, mut line) = (0, 0, 0);
    let mut crossed = boundary == 0;
    let mut inline_closers = inline_code_closers(&text, 0, boundary);
    inline_closers.extend(inline_code_closers(&text, boundary, bytes.len()));

    while i < bytes.len() {
        if !crossed && i == boundary {
            append(phase, &text[cursor..i], &mut out);
            (cursor, line, crossed) = (i, i, true);
            if phase == Phase::Reasoning && !explicit_open {
                phase = Phase::Prefix;
            }
        }
        let limit = if crossed { bytes.len() } else { boundary };
        if i == line && indented_line(bytes, i, limit) {
            i = text[i..limit].find('\n').map_or(limit, |end| i + end + 1);
            line = i;
            continue;
        }
        if bytes[i] == b'\\' {
            i += 1;
            if i < limit {
                let newline = bytes[i] == b'\n';
                i += text[i..].chars().next().unwrap().len_utf8();
                if newline {
                    line = i;
                }
            }
            continue;
        }
        if matches!(bytes[i], b'`' | b'~') {
            let symbol = bytes[i];
            let width = run_width(bytes, i, limit, symbol);
            let fenced =
                width >= 3 && i - line <= 3 && text[line..i].bytes().all(|byte| byte == b' ');
            if fenced {
                i = fence_end(&text, i, symbol, width, limit).unwrap_or(limit);
                line = i;
                continue;
            }
            if symbol == b'`' {
                if let Some(end) = inline_closers.get(&i).copied() {
                    if let Some(newline) = text[i..end].rfind('\n') {
                        line = i + newline + 1;
                    }
                    i = end;
                    continue;
                }
                let closing = run_width(bytes, boundary, bytes.len(), b'`');
                if !crossed && closing == width {
                    let end = boundary + closing;
                    append(phase, &text[cursor..end], &mut out);
                    (i, cursor, line, crossed) = (end, end, boundary, true);
                    if phase == Phase::Reasoning && !explicit_open {
                        phase = Phase::Prefix;
                    }
                    continue;
                }
            }
            i += width;
            continue;
        }
        let rest = &text[i..];
        if let Some((tag, opening)) = TAGS.iter().find(|(tag, _)| rest.starts_with(tag)) {
            append(phase, &text[cursor..i], &mut out);
            if *opening {
                match phase {
                    Phase::Reasoning => explicit_open = true,
                    Phase::Prefix if out[1].trim().is_empty() => {
                        out[1].clear();
                        phase = Phase::Reasoning;
                        explicit_open = true;
                    }
                    Phase::Prefix => {
                        let prefix = std::mem::take(&mut out[1]);
                        out[2].push_str(&prefix);
                        phase = Phase::Content;
                        explicit_open = false;
                    }
                    Phase::Content => explicit_open = false,
                }
            } else {
                if phase == Phase::Prefix {
                    let prefix = std::mem::take(&mut out[1]);
                    out[0].push_str(&prefix);
                }
                if phase != Phase::Content {
                    phase = Phase::Content;
                }
                explicit_open = false;
            }
            i += tag.len();
            cursor = i;
            if i >= boundary {
                crossed = true;
                line = boundary;
            }
            continue;
        }
        if rest.len() > 1 && TAGS.iter().any(|(tag, _)| tag.starts_with(rest)) {
            return Err(TRUNCATED);
        }
        if bytes[i] == b'\n' {
            line = i + 1;
        }
        i += text[i..].chars().next().unwrap().len_utf8();
    }
    append(phase, &text[cursor..], &mut out);
    let prefix = std::mem::take(&mut out[1]);
    out[2].push_str(&prefix);
    Ok((out[0].to_string(), out[2].to_string()))
}

fn inline_code_closers(text: &str, start: usize, limit: usize) -> HashMap<usize, usize> {
    let bytes = text.as_bytes();
    let mut runs = Vec::new();
    let mut i = start;
    while i < limit {
        if bytes[i] == b'`' {
            let width = run_width(bytes, i, limit, b'`');
            runs.push((i, width));
            i += width;
        } else {
            i += text[i..].chars().next().unwrap().len_utf8();
        }
    }

    let mut next = HashMap::new();
    let mut by_width = HashMap::new();
    for (at, width) in runs.into_iter().rev() {
        if let Some(close) = by_width.insert(width, at) {
            next.insert(at, close + width);
        }
    }
    next
}

fn fence_end(text: &str, opening: usize, symbol: u8, width: usize, limit: usize) -> Option<usize> {
    let line = text[opening..limit].find('\n').map(|at| opening + at + 1)?;
    fence_end_from(text, line, symbol, width, limit)
}

fn fence_end_from(
    text: &str,
    mut line: usize,
    symbol: u8,
    width: usize,
    limit: usize,
) -> Option<usize> {
    let bytes = text.as_bytes();
    while line < limit {
        let end = text[line..limit].find('\n').map_or(limit, |at| line + at);
        let indent = bytes[line..end]
            .iter()
            .take_while(|byte| **byte == b' ')
            .count();
        let run = run_width(bytes, line + indent, end, symbol);
        if indent <= 3 && run >= width && text[line + indent + run..end].trim().is_empty() {
            return Some((end + 1).min(limit));
        }
        line = (end + 1).min(limit);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use serde_json::json;

    const DONE: &str = "data: [DONE]\n\n";
    const FINISHED: &str =
        "data: {\"choices\":[{\"delta\":{\"content\":\"answer\"},\"finish_reason\":\"stop\"}]}\n\n";

    fn adapted_text(input: &str) -> (String, String) {
        let mut framing = ReasoningFraming::default();
        let mut text = (String::new(), String::new());
        for data in input
            .split("\n\n")
            .filter_map(|block| block.strip_prefix("data: "))
        {
            if data.trim() == "[DONE]" {
                let (reasoning, content) = fields(&mut framing, None, None, false, true).unwrap();
                text.0.push_str(reasoning.as_deref().unwrap_or_default());
                text.1.push_str(content.as_deref().unwrap_or_default());
                continue;
            }
            let mut chunk: Value = serde_json::from_str(data).unwrap();
            adapt_chat_chunk(&mut chunk, &mut framing).unwrap();
            let delta = &chunk["choices"][0]["delta"];
            text.0
                .push_str(delta["reasoning_content"].as_str().unwrap_or_default());
            text.1
                .push_str(delta["content"].as_str().unwrap_or_default());
        }
        text
    }

    async fn adapted_stream(chunks: Vec<&str>) -> Result<String, std::io::Error> {
        let chunks = chunks
            .into_iter()
            .map(|chunk| Ok::<_, std::io::Error>(Bytes::copy_from_slice(chunk.as_bytes())))
            .collect::<Vec<_>>();
        let stream = stream::iter(chunks);
        let bytes = adapt_chat_sse_stream(stream)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        Ok(String::from_utf8(bytes.concat()).unwrap())
    }

    async fn adapted_passthrough_stream(chunks: Vec<&str>) -> Result<String, std::io::Error> {
        let chunks = chunks
            .into_iter()
            .map(|chunk| Ok::<_, std::io::Error>(Bytes::copy_from_slice(chunk.as_bytes())))
            .collect::<Vec<_>>();
        let stream = stream::iter(chunks);
        let bytes = adapt_chat_passthrough_sse_stream(stream)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        Ok(String::from_utf8(bytes.concat()).unwrap())
    }

    fn json_events(output: &str) -> Vec<Value> {
        output
            .split("\n\n")
            .filter_map(|block| block.strip_prefix("data: "))
            .filter(|data| data.trim() != "[DONE]")
            .map(|data| serde_json::from_str(data).unwrap())
            .collect()
    }

    fn fields(
        framing: &mut ReasoningFraming,
        reasoning: Option<&str>,
        content: Option<&str>,
        has_tool: bool,
        finish: bool,
    ) -> Result<(Option<String>, Option<String>), &'static str> {
        let (mut reasoning, mut content) =
            (reasoning.map(str::to_string), content.map(str::to_string));
        framing.adapt_fields(&mut reasoning, &mut content, has_tool, finish)?;
        Ok((reasoning, content))
    }

    async fn protocol_outputs(input: &'static [u8]) -> (String, String) {
        let source = || stream::iter([Ok::<_, std::io::Error>(Bytes::from_static(input))]);
        let codex = super::super::streaming_codex_chat::create_responses_sse_stream_from_chat(
            adapt_chat_sse_stream(source()),
        );
        let claude =
            super::super::streaming::create_anthropic_sse_stream(adapt_chat_sse_stream(source()));
        let collect = |chunks: Vec<Result<Bytes, std::io::Error>>| {
            String::from_utf8(
                chunks
                    .into_iter()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap()
                    .concat(),
            )
            .unwrap()
        };
        (
            collect(codex.collect::<Vec<_>>().await),
            collect(claude.collect::<Vec<_>>().await),
        )
    }

    #[tokio::test]
    async fn upstream_error_stops_before_following_done() {
        let error = adapted_stream(vec!["data: {\"error\":{\"message\":\"boom\"}}\n\n", DONE])
            .await
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Other);
        assert!(error.to_string().contains("boom"));

        let error = adapted_stream(vec![
            "event: error\ndata: {\"message\":\"quota\"}\n\n",
            DONE,
        ])
        .await
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Other);
        assert!(error.to_string().contains("quota"));

        let (codex, claude) = protocol_outputs(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"},\"finish_reason\":\"stop\"}]}\n\nevent: error\ndata: {\"message\":\"quota\"}\n\ndata: [DONE]\n\n",
        )
        .await;
        assert!(codex.contains("event: response.failed"));
        assert!(!codex.contains("event: response.completed"));
        assert!(claude.contains("event: error"));
        assert!(!claude.contains("event: message_stop"));
    }

    #[tokio::test]
    async fn chat_passthrough_preserves_structured_error_and_stops() {
        for error in [
            "event: error\ndata: {\"message\":\"quota\"}\n\n",
            "data: {\"error\":{\"message\":\"quota\",\"type\":\"rate_limit\"}}\n\n",
        ] {
            let output = adapted_passthrough_stream(vec![error, DONE]).await.unwrap();
            assert_eq!(output, error);
        }
    }

    #[tokio::test]
    async fn adapts_every_choice_and_message_snapshot() {
        let output = adapted_stream(vec![
            concat!(
                "data: {\"choices\":[",
                "{\"index\":0,\"delta\":{\"reasoning_content\":\"<think>r0\",\"content\":\"</think>a0\"},\"finish_reason\":\"stop\"},",
                "{\"index\":1,\"delta\":{\"reasoning_content\":\"<thinking>r1\",\"content\":\"</thinking>a1\"},\"finish_reason\":\"stop\"}",
                "]}\n\n"
            ),
            DONE,
        ])
        .await
        .unwrap();
        let event = &json_events(&output)[0];
        for (index, reasoning, content) in [(0, "r0", "a0"), (1, "r1", "a1")] {
            let choice = &event["choices"][index];
            assert_eq!(choice["delta"]["reasoning_content"], reasoning);
            assert_eq!(choice["delta"]["content"], content);
        }

        let output = adapted_stream(vec![
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"message\":{\"role\":\"assistant\",\"reasoning_content\":\"<think>plan\",\"content\":\"</think>answer\"},\"finish_reason\":\"stop\"}]}\n\n",
            DONE,
        ])
        .await
        .unwrap();
        let choice = &json_events(&output)[0]["choices"][0];
        assert_eq!(choice["delta"]["reasoning_content"], "plan");
        assert_eq!(choice["delta"]["content"], "answer");
        assert!(!output.contains("<think>"));
        assert!(!output.contains("</think>"));

        let passthrough = adapted_passthrough_stream(vec![
            "data: {\"choices\":[{\"index\":0,\"delta\":{},\"message\":{\"role\":\"assistant\",\"reasoning_content\":\"<think>plan\",\"content\":\"</think>answer\"},\"finish_reason\":\"stop\"}]}\n\n",
            DONE,
        ])
        .await
        .unwrap();
        let choice = &json_events(&passthrough)[0]["choices"][0];
        assert_eq!(choice["message"]["reasoning_content"], "plan");
        assert_eq!(choice["message"]["content"], "answer");

        let (codex, claude) = protocol_outputs(
            b"data: {\"choices\":[{\"index\":0,\"delta\":{},\"message\":{\"role\":\"assistant\",\"reasoning_content\":\"<think>plan\",\"content\":\"</think>answer\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        )
        .await;
        assert!(codex.contains("plan"));
        assert!(codex.contains("answer"));
        assert!(codex.contains("event: response.completed"));
        assert!(!codex.contains("event: response.failed"));
        assert!(claude.contains("plan"));
        assert!(claude.contains("answer"));
        assert!(claude.contains("event: message_stop"));
        assert!(!claude.contains("event: error"));

        let output = adapted_stream(vec![
            "data: {\"choices\":[{\"delta\":{\"content\":\"draft\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"message\":{\"role\":\"assistant\",\"content\":\"final answer\"},\"finish_reason\":\"stop\"}]}\n\n",
            DONE,
        ])
        .await
        .unwrap();
        assert_eq!(
            json_events(&output)[1]["choices"][0]["delta"]["content"],
            "final answer"
        );
    }

    #[tokio::test]
    async fn transformed_snapshot_rejects_prior_emitted_output() {
        let error = adapted_stream(vec![
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"plan\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"message\":{\"role\":\"assistant\",\"reasoning_content\":\"plan revised\",\"content\":\"answer\"},\"finish_reason\":\"stop\"}]}\n\n",
            DONE,
        ])
        .await
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), LATE_OUTPUT);
    }

    #[tokio::test]
    async fn preserves_opaque_content_shape() {
        let output = adapted_stream(vec![
            "data: {\"choices\":[{\"delta\":{\"content\":[{\"type\":\"text\",\"text\":\"answer\"}]},\"finish_reason\":\"stop\"}]}\n\n",
            DONE,
        ])
        .await
        .unwrap();
        assert_eq!(
            json_events(&output)[0]["choices"][0]["delta"]["content"],
            serde_json::json!([{"type": "text", "text": "answer"}])
        );
    }

    #[test]
    fn nonstream_adapts_every_choice() {
        let mut body = serde_json::json!({"choices": [
            {"message": {"reasoning_content": "r0", "content": "</think>a0"}, "finish_reason": "stop"},
            {"message": {"reasoning_content": "r1", "content": "</thinking>a1"}, "finish_reason": "stop"}
        ]});
        adapt_chat_completion(&mut body).unwrap();
        assert_eq!(body["choices"][0]["message"]["reasoning_content"], "r0");
        assert_eq!(body["choices"][0]["message"]["content"], "a0");
        assert_eq!(body["choices"][1]["message"]["reasoning_content"], "r1");
        assert_eq!(body["choices"][1]["message"]["content"], "a1");
    }

    #[tokio::test]
    async fn empty_error_placeholders_do_not_truncate_stream() {
        for placeholder in ["null", "{}", "{\"message\":\"\"}", "false", "0"] {
            let chunk = format!(
                "data: {{\"error\":{placeholder},\"choices\":[{{\"delta\":{{\"content\":\"answer\"}},\"finish_reason\":\"stop\"}}]}}\n\n"
            );
            let output = adapted_stream(vec![&chunk, DONE]).await.unwrap();
            assert!(output.contains("\"content\":\"answer\""));
            assert!(!output.contains("\"error\""));
        }

        let output = adapted_stream(vec![
            "event: chunk\ndata: {\"choices\":[{\"delta\":{\"content\":\"answer\"},\"finish_reason\":\"stop\"}]}\n\n",
            DONE,
        ])
        .await
        .unwrap();
        assert!(output.contains("event: chunk\n"));
    }

    #[tokio::test]
    async fn duplicate_done_emits_one_done() {
        let output = adapted_stream(vec![FINISHED, DONE, DONE]).await.unwrap();
        assert_eq!(output.matches("data: [DONE]").count(), 1);
        assert_eq!(output.matches("\"finish_reason\":\"stop\"").count(), 1);
    }

    #[tokio::test]
    async fn explicit_finish_then_usage_and_done_preserves_one_terminal() {
        let output = adapted_stream(vec![
            FINISHED,
            "data: {\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":7}}\n\n",
            DONE,
        ])
        .await
        .unwrap();
        let events = json_events(&output);
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[1]["usage"],
            json!({"prompt_tokens": 11, "completion_tokens": 7})
        );
        assert_eq!(output.matches("\"finish_reason\":\"stop\"").count(), 1);
        assert_eq!(output.matches("data: [DONE]").count(), 1);
    }

    #[tokio::test]
    async fn explicit_finish_accepts_natural_eof() {
        let output = adapted_stream(vec![FINISHED]).await.unwrap();
        assert_eq!(output.matches("\"finish_reason\":\"stop\"").count(), 1);
        assert!(!output.contains("[DONE]"));
    }

    #[tokio::test]
    async fn complete_opener_is_implicitly_closed_at_done() {
        let output = adapted_stream(vec![
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"<think>plan\"}}]}\n\n",
            DONE,
        ])
        .await
        .unwrap();
        assert!(output.contains("\"reasoning_content\":\"plan\""));
        assert!(!output.contains("<think>"));
        assert!(output.contains("data: [DONE]"));
    }

    #[tokio::test]
    async fn split_parallel_tool_calls_are_preserved_and_finish_as_tools() {
        let first = json!({"choices": [{"delta": {"tool_calls": [
            {"index": 0, "id": "call_0", "type": "function", "function": {"name": "read", "arguments": "{\"path\":"}},
            {"index": 1, "id": "call_1", "type": "function", "function": {"name": "search", "arguments": "{\"query\":"}}
        ]}}]});
        let second = json!({"choices": [{"delta": {"tool_calls": [
            {"index": 0, "function": {"arguments": "\"a\"}"}},
            {"index": 1, "function": {"arguments": "\"b\"}"}}
        ]}}]});
        let first_sse = format!("data: {first}\n\n");
        let second_sse = format!("data: {second}\n\n");
        let output = adapted_stream(vec![&first_sse, &second_sse, DONE])
            .await
            .unwrap();
        let events = json_events(&output);
        assert_eq!(
            events[0]["choices"][0]["delta"]["tool_calls"],
            first["choices"][0]["delta"]["tool_calls"]
        );
        assert_eq!(
            events[1]["choices"][0]["delta"]["tool_calls"],
            second["choices"][0]["delta"]["tool_calls"]
        );
        assert_eq!(events[2]["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(
            output.matches("\"finish_reason\":\"tool_calls\"").count(),
            1
        );
    }

    #[tokio::test]
    async fn standalone_stream_adapter_owns_done_and_eof_lifecycle() {
        let without_finish =
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"plan</think>answer\"}}]}\n\n";
        let output = adapted_stream(vec![without_finish, DONE]).await.unwrap();
        assert!(output.contains("\"reasoning_content\":\"plan\""));
        assert!(output.contains("\"content\":\"answer\""));
        assert!(output.contains("\"finish_reason\":\"stop\""));
        assert!(!output.contains("</think>"));
        assert_eq!(
            adapted_stream(vec![without_finish])
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::UnexpectedEof
        );
        assert_eq!(
            adapted_stream(vec!["data: {\"choices\":[{\"delta\":{}}]}\n\n", DONE])
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidData
        );
        let finished = adapted_stream(vec![FINISHED]).await.unwrap();
        assert!(finished.contains("\"finish_reason\":\"stop\""));

        let tool = adapted_stream(vec![
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read\"}}]}}]}\n\ndata: [DONE]\n\n",
        ])
        .await
        .unwrap();
        assert!(tool.contains("\"finish_reason\":\"tool_calls\""));
    }

    #[tokio::test]
    async fn standalone_stream_adapter_is_network_chunk_invariant() {
        let input = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"plan</thi\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"nk>answer\"}}]}\n\n",
            "data: [DONE]\n\n"
        );
        for cut in 1..input.len() {
            let output = adapted_stream(vec![&input[..cut], &input[cut..]])
                .await
                .unwrap();
            assert!(
                output.contains("\"reasoning_content\":\"plan\""),
                "cut={cut}"
            );
            assert!(output.contains("\"content\":\"answer\""), "cut={cut}");
            assert!(!output.contains("</think>"), "cut={cut}");
        }
    }

    #[tokio::test]
    async fn standalone_adapter_composes_with_both_harness_protocols() {
        let (codex, claude) = protocol_outputs(
            b"data: {\"id\":\"glm\",\"choices\":[{\"delta\":{\"reasoning_content\":\"plan</think>answer\"}}]}\n\ndata: [DONE]\n\n",
        )
        .await;
        for output in [&codex, &claude] {
            assert!(output.contains("plan"));
            assert!(output.contains("answer"));
            assert!(!output.contains("</think>"));
        }
        assert!(codex.contains("event: response.reasoning_summary_text.delta"));
        assert!(codex.contains("event: response.output_text.delta"));
        assert!(codex.contains("event: response.completed"));
        assert!(!codex.contains("event: response.failed"));
        let thinking = claude.find("\"type\":\"thinking\"").unwrap();
        let text = claude.find("\"type\":\"text\"").unwrap();
        assert!(thinking < text);
        assert!(claude.contains("event: message_stop"));
        assert!(!claude.contains("event: error"));

        let (codex, claude) = protocol_outputs(concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"plan \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"continued \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"analysis\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"</think>answer\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        ).as_bytes()).await;
        for output in [&codex, &claude] {
            assert!(output.contains("plan "));
            assert!(output.contains("continued "));
            assert!(output.contains("analysis"));
            assert!(output.contains("answer"));
            assert!(!output.contains("</think>"));
        }
        assert!(codex.contains("event: response.completed"));
        assert!(claude.contains("event: message_stop"));

        let (codex, claude) = protocol_outputs(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"<think>unfinished\"}}]}\n\ndata: [DONE]\n\n",
        )
        .await;
        assert!(codex.contains("unfinished"));
        assert!(codex.contains("event: response.completed"));
        assert!(!codex.contains("<think>"));
        assert!(claude.contains("unfinished"));
        assert!(claude.contains("event: message_stop"));
        assert!(!claude.contains("<think>"));
    }

    #[test]
    fn parses_boundaries_and_preserves_markdown_literals() {
        for (reasoning, content) in [
            ("", "<think>plan</think>answer"),
            ("", "<thinking>plan</thinking>answer"),
            ("plan</thi", "nk>answer"),
            ("plan</think", "ing>answer"),
        ] {
            assert_eq!(
                split_reasoning(reasoning, content),
                Ok(("plan".into(), "answer".into()))
            );
        }
        let mut body = json!({"choices": [{"finish_reason": "stop", "message": {
            "content": "plan</think>answer"
        }}]});
        adapt_chat_completion(&mut body).unwrap();
        assert_eq!(body["choices"][0]["message"]["reasoning_content"], "plan");
        assert_eq!(body["choices"][0]["message"]["content"], "answer");
        for literal in [
            "Use `</think>` safely",
            "Use ``</think>`` safely",
            "```text\n</think>\n```",
            "\\\n~~~\n</think>\n~~~\n",
            r"Use \</think> safely",
            "    </think>\nnext",
            " \t</think>",
            "\t</thinking>\nnext",
        ] {
            assert_eq!(
                split_reasoning("structured", literal),
                Ok(("structured".into(), literal.into()))
            );
        }
    }

    #[test]
    fn clean_stream_and_nonstream_preserve_whitespace() {
        let input = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"  plan \\n\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"\\n answer \"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        assert_eq!(
            adapted_text(input),
            ("  plan \n".into(), "\n answer ".into())
        );

        let mut body = json!({"choices": [{"finish_reason": "stop", "message": {
            "reasoning_content": "  plan \n", "content": "\n answer "
        }}]});
        adapt_chat_completion(&mut body).unwrap();
        assert_eq!(
            body["choices"][0]["message"]["reasoning_content"],
            "  plan \n"
        );
        assert_eq!(body["choices"][0]["message"]["content"], "\n answer ");

        let framed = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"  plan \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"</think>\\n answer \"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let expected = ("  plan ".into(), "\n answer ".into());
        assert_eq!(adapted_text(framed), expected);
        assert_eq!(
            split_reasoning("  plan ", "</think>\n answer "),
            Ok(expected)
        );
    }

    #[test]
    fn normalizes_redundant_sentinels_and_rejects_partial_tags() {
        assert_eq!(
            split_reasoning("", "<thinking>mismatched</think>answer"),
            Ok(("mismatched".into(), "answer".into()))
        );
        assert_eq!(
            split_reasoning("", "answer<think>late</think>"),
            Ok(("".into(), "answerlate".into()))
        );
        for (reasoning, content, expected) in [
            ("", "<think>missing close", ("missing close", "")),
            ("", "<think>nested<think>x</think>", ("nestedx", "")),
            ("", "a</think>b</think>c", ("a", "bc")),
            ("", "a</think></thinking>b", ("a", "b")),
            ("plan<thi", "nk>answer", ("plananswer", "")),
        ] {
            assert_eq!(
                split_reasoning(reasoning, content),
                Ok((expected.0.into(), expected.1.into()))
            );
        }
        assert_eq!(split_reasoning("", "answer</thi"), Err(TRUNCATED));
    }

    #[test]
    fn markdown_state_does_not_cross_field_boundary() {
        for reasoning in ["prefix \\", "prefix `unclosed", "```text\nunclosed"] {
            assert_eq!(
                split_reasoning(reasoning, "</think>answer"),
                Ok((reasoning.trim().into(), "answer".into())),
                "reasoning={reasoning:?}"
            );
        }
        assert_eq!(
            split_reasoning("prefix `unclosed", "</think>` answer"),
            Ok(("prefix `unclosed".into(), "` answer".into()))
        );
        assert_eq!(
            split_reasoning("prefix `unclosed", "``</think>answer"),
            Ok(("prefix `unclosed``".into(), "answer".into()))
        );
        assert_eq!(
            split_reasoning("", "prefix `unmatched </think>answer"),
            Ok(("prefix `unmatched ".into(), "answer".into()))
        );
    }

    #[test]
    fn policy_is_capability_gated_and_survives_persistence() {
        let config = |thinking_param: &str| CodexChatReasoningConfig {
            thinking_param: Some(thinking_param.into()),
            output_format: Some("reasoning_content".into()),
            ..Default::default()
        };
        for (thinking, expected) in [
            (
                "chat_template_kwargs.enable_thinking",
                ReasoningPolicy::GlmBoundary,
            ),
            ("thinking", ReasoningPolicy::ToolPlaceholder),
            ("unused", ReasoningPolicy::ToolPlaceholder),
        ] {
            assert_eq!(ReasoningPolicy::resolve(Some(&config(thinking))), expected);
        }
        assert_eq!(
            ReasoningPolicy::resolve(None),
            ReasoningPolicy::ToolPlaceholder
        );
        let mut disabled = config("chat_template_kwargs.enable_thinking");
        disabled.supports_thinking = Some(false);
        assert_eq!(
            ReasoningPolicy::resolve(Some(&disabled)),
            ReasoningPolicy::ToolPlaceholder
        );

        let declared = Provider::with_id(
            "declared".into(),
            "Declared".into(),
            json!({"nexusCapabilities": {"reasoningBoundary": "think_close"}}),
            None,
        );
        let declared: Provider =
            serde_json::from_value(serde_json::to_value(declared).unwrap()).unwrap();
        assert_eq!(
            ReasoningPolicy::from_provider(&declared),
            ReasoningPolicy::GlmBoundary
        );

        let mut inferred = Provider::with_id("glm".into(), "GLM".into(), json!({}), None);
        inferred.meta = Some(crate::provider::ProviderMeta {
            codex_chat_reasoning: Some(config("chat_template_kwargs.enable_thinking")),
            ..Default::default()
        });
        let inferred: Provider =
            serde_json::from_value(serde_json::to_value(inferred).unwrap()).unwrap();
        assert_eq!(
            ReasoningPolicy::from_provider(&inferred),
            ReasoningPolicy::GlmBoundary
        );
    }

    #[test]
    fn stream_semantics_are_delta_chunk_invariant() {
        let canonical = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"<think>plan\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"</think>answer\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        assert_eq!(adapted_text(canonical), ("plan".into(), "answer".into()));

        let head = "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"structured\"}}]}\n\n";
        let finish = "data: [DONE]\n\n";
        let split = format!(
            "{head}data: {{\"choices\":[{{\"delta\":{{\"content\":\"prefix\"}}}}]}}\n\ndata: {{\"choices\":[{{\"delta\":{{\"content\":\"</think>answer\"}},\"finish_reason\":\"stop\"}}]}}\n\n{finish}"
        );
        let combined = format!(
            "{head}data: {{\"choices\":[{{\"delta\":{{\"content\":\"prefix</think>answer\"}},\"finish_reason\":\"stop\"}}]}}\n\n{finish}"
        );
        assert_eq!(adapted_text(&split), adapted_text(&combined));
        assert_eq!(
            adapted_text(&split),
            ("structuredprefix".into(), "answer".into())
        );

        let head = "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"Before `No raw <think> marker\"}}]}\n\n";
        let tail = "data: {\"choices\":[{\"delta\":{\"content\":\"Let me write checkpoint V now.</think>Good, I have a clear picture now.\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        let split = format!("{head}data: {{\"choices\":[{{\"delta\":{{\"content\":\"` and continued reasoning. \"}}}}]}}\n\n{tail}");
        let combined = format!("{head}data: {{\"choices\":[{{\"delta\":{{\"content\":\"` and continued reasoning. Let me write checkpoint V now.</think>Good, I have a clear picture now.\"}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n");
        assert_eq!(adapted_text(&split), adapted_text(&combined));
        assert_eq!(
            adapted_text(&split),
            (
                "Before `No raw <think> marker` and continued reasoning. Let me write checkpoint V now."
                    .into(),
                "Good, I have a clear picture now.".into(),
            )
        );
    }

    #[test]
    fn streaming_persists_open_and_prior_reasoning_state() {
        let mut framing = ReasoningFraming::default();
        assert_eq!(
            fields(&mut framing, Some("<think>"), None, false, false),
            Ok((None, None))
        );
        assert_eq!(
            fields(&mut framing, Some("plan"), None, false, false),
            Ok((Some("plan".into()), None))
        );
        assert_eq!(
            fields(&mut framing, None, None, false, true),
            Ok((None, None))
        );

        let mut framing = ReasoningFraming::default();
        assert_eq!(
            fields(&mut framing, Some("plain reasoning"), None, false, false),
            Ok((Some("plain reasoning".into()), None))
        );
        assert_eq!(
            fields(&mut framing, Some("<think>x</think>"), None, false, true),
            Ok((Some("x".into()), None))
        );
    }

    #[test]
    fn streaming_accepts_leading_whitespace_before_explicit_open() {
        for leading in ["\n", "\t", "    "] {
            assert_eq!(
                split_reasoning("", &format!("{leading}<think>plan</think>answer")),
                Ok(("plan".into(), "answer".into()))
            );
            let mut framing = ReasoningFraming::default();
            assert_eq!(
                fields(&mut framing, Some(leading), None, false, false),
                Ok((None, None))
            );
            assert_eq!(
                fields(
                    &mut framing,
                    Some("<think>plan</think>answer"),
                    None,
                    false,
                    false,
                ),
                Ok((Some("plan".into()), None))
            );
            assert_eq!(
                fields(&mut framing, None, None, false, true),
                Ok((None, Some("answer".into())))
            );
        }
    }

    #[test]
    fn streaming_retains_markdown_line_context() {
        let split = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"    literal \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"</think>\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let combined = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"    literal </think>\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        assert_eq!(adapted_text(split), adapted_text(combined));
        assert_eq!(
            adapted_text(split),
            ("    literal </think>".into(), String::new())
        );
    }

    #[test]
    fn streaming_advances_past_complete_markdown() {
        for input in [
            "Before `x` reasoning continues",
            r"Use \</think> literally and continue",
            "```text\n</think>\n```\ncontinue",
        ] {
            let mut framing = ReasoningFraming::default();
            let output = fields(&mut framing, Some(input), None, false, false)
                .unwrap()
                .0
                .expect("complete Markdown must not stall the stream");
            assert!(output.len() + 1 >= input.len(), "input={input:?}");
        }
    }

    #[test]
    fn pending_markdown_scan_resumes_from_new_bytes() {
        let mut framing = ReasoningFraming::default();
        fields(&mut framing, Some("``"), None, false, false).unwrap();
        let mut checked = framing.pending_markdown.unwrap().checked;
        for _ in 0..256 {
            fields(&mut framing, Some("x"), None, false, false).unwrap();
            let next = framing.pending_markdown.unwrap().checked;
            assert!(next > checked);
            checked = next;
        }
        fields(&mut framing, Some("`"), None, false, false).unwrap();
        let output = fields(&mut framing, Some("`\n"), None, false, false)
            .unwrap()
            .0;
        assert!(framing.pending_markdown.is_none());
        assert!(output.as_deref().is_some_and(|text| text.ends_with("``\n")));
    }

    #[test]
    fn streaming_reclassifies_split_fence_openers() {
        for (chunks, expected) in [
            (["``", "`\ncode\n```\ncontinue"], "```\ncode\n```\ncontinue"),
            (["`", "``\ncode\n```\ncontinue"], "```\ncode\n```\ncontinue"),
            (
                ["~~", "~\n</think>\n~~~\ncontinue"],
                "~~~\n</think>\n~~~\ncontinue",
            ),
        ] {
            let mut framing = ReasoningFraming::default();
            let mut streamed = String::new();
            for chunk in chunks {
                let (reasoning, _) = fields(&mut framing, Some(chunk), None, false, false).unwrap();
                streamed.push_str(reasoning.as_deref().unwrap_or_default());
            }
            assert!(framing.pending_markdown.is_none());
            assert_eq!(streamed, expected);
            let (reasoning, _) = fields(&mut framing, None, None, false, true).unwrap();
            streamed.push_str(reasoning.as_deref().unwrap_or_default());
            assert_eq!(streamed, expected);
        }
    }

    #[test]
    fn pending_fence_only_observes_new_bytes() {
        let mut framing = ReasoningFraming::default();
        fields(&mut framing, Some("```\n"), None, false, false).unwrap();
        for expected in 5..261 {
            fields(&mut framing, Some("x"), None, false, false).unwrap();
            let pending = framing.pending_markdown.unwrap();
            assert_eq!(pending.observed, expected);
        }
    }

    #[test]
    fn streaming_defers_fence_close_at_temporary_eof() {
        let mut framing = ReasoningFraming::default();
        let output = fields(&mut framing, Some("```\n</think>\n```"), None, false, false).unwrap();
        assert_eq!(output, (None, None));
        assert!(framing.pending_markdown.is_some());

        let (reasoning, content) =
            fields(&mut framing, Some("x\n</think>answer"), None, false, true).unwrap();
        assert_eq!(
            reasoning.as_deref(),
            Some("```\n</think>\n```x\n</think>answer")
        );
        assert!(content.is_none());
    }

    #[test]
    fn ordinary_trailing_whitespace_does_not_accumulate() {
        let mut framing = ReasoningFraming::default();
        assert_eq!(
            fields(&mut framing, Some("x"), None, false, false),
            Ok((Some("x".into()), None))
        );
        for _ in 0..256 {
            assert_eq!(
                fields(&mut framing, Some(" "), None, false, false),
                Ok((Some(" ".into()), None))
            );
            assert!(framing.reasoning.is_empty());
        }

        let mut framing = ReasoningFraming::default();
        let mut expected = String::new();
        for _ in 0..256 {
            let output = fields(&mut framing, Some("\u{a0}"), None, false, false).unwrap();
            expected.push('\u{a0}');
            assert_eq!(output, (None, None));
            assert_eq!(framing.opener_checked, expected.len());
        }
        let (reasoning, _) = fields(&mut framing, Some("x"), None, false, false).unwrap();
        expected.push('x');
        assert_eq!(reasoning.as_deref(), Some(expected.as_str()));
        assert!(framing.reasoning.is_empty());
    }

    #[test]
    fn done_without_finish_accepts_only_resolved_state() {
        let mut framing = ReasoningFraming::default();
        fields(&mut framing, Some("plan"), None, false, false).unwrap();
        assert_eq!(framing.validate_done(), Ok(()));

        let mut framing = ReasoningFraming::default();
        let (reasoning, _) =
            fields(&mut framing, Some("plan</think>answer"), None, false, false).unwrap();
        assert_eq!(reasoning.as_deref(), Some("plan"));
        assert_eq!(
            fields(&mut framing, None, None, false, true),
            Ok((None, Some("answer".into())))
        );

        let mut framing = ReasoningFraming::default();
        fields(&mut framing, None, Some("answer"), false, false).unwrap();
        assert_eq!(framing.validate_done(), Err(TRUNCATED));
    }

    #[test]
    fn framing_streams_reasoning_and_buffers_ambiguous_content() {
        let mut framing = ReasoningFraming::default();
        let (reasoning, _) = fields(&mut framing, Some("plan"), None, false, false).unwrap();
        let mut streamed = reasoning.unwrap();
        assert_eq!(streamed, "plan");

        assert_eq!(
            fields(&mut framing, None, Some("answer"), false, false),
            Ok((None, None))
        );
        let (reasoning, content) = fields(&mut framing, None, None, false, true).unwrap();
        streamed.push_str(reasoning.as_deref().unwrap_or_default());
        assert_eq!(streamed, "plan");
        assert_eq!(content.as_deref(), Some("answer"));

        for delta in [
            r#"{"content":"late"}"#,
            r#"{"reasoning_content":"late"}"#,
            r#"{"tool_calls":[{"index":0}]}"#,
            r#"{"function_call":{"name":"late"}}"#,
        ] {
            let mut framing = ReasoningFraming::default();
            let mut finish = json!({"choices": [{"delta": {}, "finish_reason": "stop"}]});
            adapt_chat_chunk(&mut finish, &mut framing).unwrap();
            let mut late =
                json!({"choices": [{"delta": serde_json::from_str::<Value>(delta).unwrap()}]});
            assert_eq!(adapt_chat_chunk(&mut late, &mut framing), Err(LATE_OUTPUT));
        }
        let mut usage = json!({"usage": {"completion_tokens": 1}});
        assert_eq!(adapt_chat_chunk(&mut usage, &mut framing), Ok(()));
    }

    #[test]
    fn preserves_interleaved_reasoning_and_rejects_output_after_tools() {
        let mut framing = ReasoningFraming::default();
        fields(&mut framing, None, Some("answer"), false, false).unwrap();
        let (reasoning, content) =
            fields(&mut framing, Some("late reasoning"), None, false, true).unwrap();
        assert_eq!(
            (reasoning.as_deref(), content.as_deref()),
            (Some("answerlate reasoning"), None)
        );

        let interleaved = concat!(
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"plan \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"continued \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"analysis\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"</think>answer\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        assert_eq!(
            adapted_text(interleaved),
            ("plan continued analysis".into(), "answer".into())
        );

        let mut framing = ReasoningFraming::default();
        fields(&mut framing, Some("plan"), None, true, false).unwrap();
        assert_eq!(
            fields(&mut framing, Some("late reasoning"), None, false, false),
            Err(LATE_OUTPUT)
        );
    }
}
