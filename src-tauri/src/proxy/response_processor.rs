//! Response processing.
//!
//! Handles streaming and non-streaming API responses.

use super::{
    content_encoding::{decompress_body, get_content_encoding},
    forwarder::ActiveConnectionGuard,
    handler_config::{StreamUsageEventFilter, UsageParserConfig},
    handler_context::{RequestContext, StreamingTimeoutConfig},
    hyper_client::ProxyResponse,
    server::ProxyState,
    sse::{strip_sse_field, take_sse_block},
    usage::parser::TokenUsage,
    ProxyError,
};
use crate::database::PRICING_SOURCE_REQUEST;
use axum::http::{header::HeaderMap, HeaderName};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures::stream::{Stream, StreamExt};
use serde_json::Value;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::Mutex;

// ============================================================================
// Response-header processing.
// ============================================================================

/// Response headers that RFC 2616/RFC 7230 prohibit proxies from forwarding.
const HOP_BY_HOP_RESPONSE_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "trailers",
    "transfer-encoding",
    "upgrade",
];

/// Removes hop-by-hop response headers and extensions named by Connection.
pub(crate) fn strip_hop_by_hop_response_headers(headers: &mut HeaderMap) {
    let connection_listed_headers: Vec<HeaderName> = headers
        .get_all(axum::http::header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .filter_map(|name| HeaderName::from_bytes(name.as_bytes()).ok())
        .collect();

    for name in HOP_BY_HOP_RESPONSE_HEADERS {
        headers.remove(*name);
    }

    for name in connection_listed_headers {
        headers.remove(name);
    }
}

/// Removes entity headers invalidated by rebuilding a body.
pub(crate) fn strip_entity_headers_for_rebuilt_body(headers: &mut HeaderMap) {
    headers.remove(axum::http::header::CONTENT_ENCODING);
    headers.remove(axum::http::header::CONTENT_LENGTH);
    headers.remove(axum::http::header::TRANSFER_ENCODING);
}

/// Reads and optionally decompresses a response body.
///
/// `body_timeout` wraps `.bytes()` when nonzero so an upstream cannot send headers
/// and stall the body forever. `Duration::ZERO` disables it when failover is off.
pub(crate) async fn read_decoded_body_traced(
    response: ProxyResponse,
    tag: &str,
    body_timeout: Duration,
) -> Result<(HeaderMap, http::StatusCode, Bytes), ProxyError> {
    let mut headers = response.headers().clone();
    let status = response.status();
    let raw_bytes = if body_timeout.is_zero() {
        response.bytes().await?
    } else {
        tokio::time::timeout(body_timeout, response.bytes())
            .await
            .map_err(|_| {
                ProxyError::Timeout(format!(
                    "Response body timed out after {}s; upstream sent headers but no body",
                    body_timeout.as_secs()
                ))
            })??
    };

    log::debug!(
        "[{tag}] Received upstream response body: status={}, bytes={}, headers={}",
        status.as_u16(),
        raw_bytes.len(),
        format_headers(&headers)
    );

    let mut body_bytes = raw_bytes.clone();
    let mut decoded = false;

    if let Some(encoding) = get_content_encoding(&headers) {
        log::debug!("[{tag}] Decompressing non-streaming response: content-encoding={encoding}");
        match decompress_body(&encoding, &raw_bytes) {
            Ok(Some(decompressed)) => {
                body_bytes = Bytes::from(decompressed);
                decoded = true;
            }
            // Preserve an unsupported encoded body and its content-encoding header.
            Ok(None) => {}
            Err(e) => {
                log::warn!("[{tag}] Decompression failed ({encoding}); using original data: {e}");
            }
        }
    }

    if decoded {
        strip_entity_headers_for_rebuilt_body(&mut headers);
    }

    Ok((headers, status, body_bytes))
}

// ============================================================================
// Public interface.
// ============================================================================

/// Detects an SSE streaming response.
#[inline]
pub fn is_sse_response(response: &ProxyResponse) -> bool {
    response.is_sse()
}

/// Processes a streaming response.
pub async fn handle_streaming(
    response: ProxyResponse,
    ctx: &RequestContext,
    state: &ProxyState,
    parser_config: &UsageParserConfig,
    connection_guard: Option<ActiveConnectionGuard>,
) -> Response {
    let status = response.status();
    log::debug!(
        "[{}] Received upstream streaming response: status={}, headers={}",
        ctx.tag,
        status.as_u16(),
        format_headers(response.headers())
    );
    // SSE is normally uncompressed; warn because compression can break parsing.
    if let Some(encoding) = get_content_encoding(response.headers()) {
        log::warn!(
            "[{}] Streaming response has content-encoding={encoding}; SSE parsing may fail. \
             Upstream compressed SSE after accept-encoding passthrough.",
            ctx.tag
        );
    }

    let mut response_headers = response.headers().clone();
    strip_hop_by_hop_response_headers(&mut response_headers);

    let mut builder = axum::response::Response::builder().status(status);

    // Copy response headers.
    for (key, value) in &response_headers {
        builder = builder.header(key, value);
    }

    // Create the byte stream.
    let stream = response.bytes_stream();

    // Create a usage collector; when logging is off, avoid parsing every hot-path SSE event.
    let usage_collector = create_usage_collector(ctx, state, status.as_u16(), parser_config);

    // Load streaming timeouts.
    let timeout_config = ctx.streaming_timeout_config();

    // Create a passthrough stream with logging and timeouts.
    let logged_stream = create_logged_passthrough_stream(
        stream,
        ctx.tag,
        usage_collector,
        timeout_config,
        connection_guard,
    );

    let body = axum::body::Body::from_stream(logged_stream);
    match builder.body(body) {
        Ok(resp) => resp,
        Err(e) => {
            log::error!("[{}] Failed to build streaming response: {e}", ctx.tag);
            ProxyError::Internal(format!("Failed to build streaming response: {e}")).into_response()
        }
    }
}

/// Processes a non-streaming response.
pub async fn handle_non_streaming(
    response: ProxyResponse,
    ctx: &RequestContext,
    state: &ProxyState,
    parser_config: &UsageParserConfig,
    // Hold the guard through full-body reading and drop it on return.
    _connection_guard: Option<ActiveConnectionGuard>,
) -> Result<Response, ProxyError> {
    // Full-body timeout applies only when failover is enabled and configured nonzero.
    let body_timeout =
        if ctx.app_config.auto_failover_enabled && ctx.app_config.non_streaming_timeout > 0 {
            Duration::from_secs(ctx.app_config.non_streaming_timeout as u64)
        } else {
            Duration::ZERO
        };
    let (mut response_headers, status, body_bytes) = read_decoded_body_traced(
        response,
        ctx.tag,
        body_timeout,
    )
    .await?;
    strip_hop_by_hop_response_headers(&mut response_headers);

    log::debug!(
        "[{}] Upstream response body: {}",
        ctx.tag,
        String::from_utf8_lossy(&body_bytes)
    );

    // Parse and record usage. Skip whole-body JSON parsing when usage logging is off.
    if usage_logging_enabled(state) {
        if let Ok(json_value) = serde_json::from_slice::<Value>(&body_bytes) {
            // Parse usage.
            if let Some(usage) = (parser_config.response_parser)(&json_value) {
                // Attribution priority: parsed usage model, response model, mapped
                // outbound model, then requested model. Empty strings are missing.
                let model = usage
                    .model
                    .clone()
                    .filter(|m| !m.is_empty())
                    .or_else(|| {
                        json_value
                            .get("model")
                            .and_then(|m| m.as_str())
                            .filter(|m| !m.is_empty())
                            .map(str::to_string)
                    })
                    .or_else(|| ctx.outbound_model.clone())
                    .unwrap_or_else(|| ctx.request_model.clone());

                spawn_log_usage(
                    state,
                    ctx,
                    UsageLogRequest {
                        usage,
                        model: &model,
                        request_model: &ctx.request_model,
                        status_code: status.as_u16(),
                        is_streaming: false,
                    },
                );
            } else {
                let model = json_value
                    .get("model")
                    .and_then(|m| m.as_str())
                    .filter(|m| !m.is_empty())
                    .map(str::to_string)
                    .or_else(|| ctx.outbound_model.clone())
                    .unwrap_or_else(|| ctx.request_model.clone());
                spawn_log_usage(
                    state,
                    ctx,
                    UsageLogRequest {
                        usage: TokenUsage::default(),
                        model: &model,
                        request_model: &ctx.request_model,
                        status_code: status.as_u16(),
                        is_streaming: false,
                    },
                );
                log::debug!(
                    "[{}] Could not parse usage; recording request with unknown token usage",
                    parser_config.app_type_str
                );
            }
        } else {
            log::debug!(
                "[{}] <<< Response (non-JSON): {} bytes",
                ctx.tag,
                body_bytes.len()
            );
            spawn_log_usage(
                state,
                ctx,
                UsageLogRequest {
                    usage: TokenUsage::default(),
                    model: ctx.outbound_model.as_deref().unwrap_or(&ctx.request_model),
                    request_model: &ctx.request_model,
                    status_code: status.as_u16(),
                    is_streaming: false,
                },
            );
        }
    } else {
        log::debug!(
            "[{}] Usage logging disabled; skipping non-streaming usage parsing",
            ctx.tag
        );
    }

    // Build the response.
    let mut builder = axum::response::Response::builder().status(status);
    for (key, value) in response_headers.iter() {
        builder = builder.header(key, value);
    }

    let body = axum::body::Body::from(body_bytes);
    builder.body(body).map_err(|e| {
        log::error!("[{}] Failed to build response: {e}", ctx.tag);
        ProxyError::Internal(format!("Failed to build response: {e}"))
    })
}

/// Shared response-processing entry point.
///
/// Selects streaming or non-streaming handling from response type.
pub async fn process_response(
    response: ProxyResponse,
    ctx: &RequestContext,
    state: &ProxyState,
    parser_config: &UsageParserConfig,
    connection_guard: Option<ActiveConnectionGuard>,
) -> Result<Response, ProxyError> {
    if is_sse_response(&response) {
        Ok(handle_streaming(
            response,
            ctx,
            state,
            parser_config,
            connection_guard,
        )
        .await)
    } else {
        handle_non_streaming(
            response,
            ctx,
            state,
            parser_config,
            connection_guard,
        )
        .await
    }
}

// ============================================================================
// SSE usage collector.
// ============================================================================

type UsageCallbackWithTiming =
    Arc<dyn Fn(Vec<Value>, Option<u64>, Option<String>) + Send + Sync + 'static>;

/// SSE usage collector.
#[derive(Clone)]
pub struct SseUsageCollector {
    inner: Arc<SseUsageCollectorInner>,
}

struct SseUsageCollectorInner {
    events: Mutex<Vec<Value>>,
    first_meaningful_event_time: Mutex<Option<std::time::Instant>>,
    first_meaningful_event_set: AtomicBool,
    start_time: std::time::Instant,
    on_complete: UsageCallbackWithTiming,
    should_collect: Option<StreamUsageEventFilter>,
    finished: AtomicBool,
}

impl SseUsageCollector {
    /// Creates a collector. `should_collect` skips non-usage events on the hot path.
    pub fn new(
        start_time: std::time::Instant,
        should_collect: Option<StreamUsageEventFilter>,
        callback: impl Fn(Vec<Value>, Option<u64>, Option<String>) + Send + Sync + 'static,
    ) -> Self {
        let on_complete: UsageCallbackWithTiming = Arc::new(callback);
        Self {
            inner: Arc::new(SseUsageCollectorInner {
                events: Mutex::new(Vec::new()),
                first_meaningful_event_time: Mutex::new(None),
                first_meaningful_event_set: AtomicBool::new(false),
                start_time,
                on_complete,
                should_collect,
                finished: AtomicBool::new(false),
            }),
        }
    }

    pub fn should_collect(&self, data: &str) -> bool {
        self.inner
            .should_collect
            .map(|filter| filter(data))
            .unwrap_or(true)
    }

    /// Whether this raw event might contain the first user-visible model delta.
    /// Once TTFT is known, only usage-filter events continue to be parsed.
    fn should_inspect_timing(&self, data: &str) -> bool {
        !self
            .inner
            .first_meaningful_event_set
            .load(Ordering::Acquire)
            && is_potential_meaningful_stream_data(data)
    }

    async fn observe_timing(&self, event: &Value) {
        if self
            .inner
            .first_meaningful_event_set
            .load(Ordering::Acquire)
            || !is_meaningful_stream_event(event)
        {
            return;
        }
        let mut first_time = self.inner.first_meaningful_event_time.lock().await;
        if first_time.is_none() {
            *first_time = Some(std::time::Instant::now());
            self.inner
                .first_meaningful_event_set
                .store(true, Ordering::Release);
        }
    }

    /// Pushes an SSE event.
    pub async fn push(&self, event: Value) {
        let mut events = self.inner.events.lock().await;
        events.push(event);
    }

    /// Completes collection and invokes the callback with a stream-level error.
    pub async fn finish_with_error(&self, error_message: Option<String>) {
        if self.inner.finished.swap(true, Ordering::SeqCst) {
            return;
        }

        let events = {
            let mut guard = self.inner.events.lock().await;
            std::mem::take(&mut *guard)
        };

        let first_token_ms = {
            let first_time = self.inner.first_meaningful_event_time.lock().await;
            first_time.map(|t| (t - self.inner.start_time).as_millis() as u64)
        };

        (self.inner.on_complete)(events, first_token_ms, error_message);
    }
}

/// Returns a logical failure for Responses-style SSE streams.
///
/// HTTP remains 200 for SSE transport, but a terminal `response.failed` event is
/// a failed model/proxy response and must not be counted as a successful request
/// in usage statistics. If a completed event appears first, later residual failed
/// text is ignored, matching the non-streaming SSE conversion behavior.
pub(crate) fn responses_stream_failure_message(events: &[Value]) -> Option<String> {
    for event in events {
        match event.get("type").and_then(Value::as_str) {
            Some("response.completed") | Some("message_stop") => return None,
            Some("response.failed") => return Some(extract_response_failed_message(event)),
            Some("error") => return Some(extract_response_failed_message(event)),
            _ => {}
        }
    }
    None
}

pub(crate) fn logical_stream_outcome(
    transport_status: u16,
    events: &[Value],
) -> (u16, Option<String>) {
    match responses_stream_failure_message(events) {
        Some(message) => (502, Some(message)),
        None => (transport_status, None),
    }
}

pub(crate) fn logical_stream_outcome_with_terminal_error(
    transport_status: u16,
    events: &[Value],
    terminal_error_message: Option<String>,
) -> (u16, Option<String>) {
    if let Some(message) = terminal_error_message {
        let status = if message.starts_with("stream_abandoned_before_finalize") {
            499
        } else if transport_status >= 400 {
            transport_status
        } else {
            502
        };
        return (status, Some(message));
    }

    logical_stream_outcome(transport_status, events)
}

fn extract_response_failed_message(event: &Value) -> String {
    let error = event
        .pointer("/response/error")
        .or_else(|| event.get("error"));
    let message = error
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .or_else(|| {
            event
                .pointer("/response/status_details/error/message")
                .and_then(Value::as_str)
        })
        .unwrap_or("response.failed event received");
    let error_type = error
        .and_then(|e| e.get("type"))
        .and_then(Value::as_str)
        .or_else(|| {
            event
                .pointer("/response/error/type")
                .and_then(Value::as_str)
        });

    match error_type {
        Some(kind) if !kind.is_empty() && !message.contains(kind) => {
            format!("{kind}: {message}")
        }
        _ => message.to_string(),
    }
}

/// Cheap raw prefilter used before JSON parsing. False positives are harmless;
/// the structural predicate below is authoritative.
fn is_potential_meaningful_stream_data(data: &str) -> bool {
    [
        // Responses API payloads encode the delta kind in the `type` value
        // (for example `response.reasoning_summary_text.delta`) rather than in
        // a top-level `reasoning` or `output_text` field. Inspect this namespace
        // structurally so timing works for both native and synthesized Responses
        // streams; the predicate below still rejects metadata-only events.
        "\"response.",
        "\"choices\"",
        "\"candidates\"",
        "\"content_block",
        "\"output_text",
        "\"reasoning",
        "\"thinking",
        "\"partial_json",
        "\"tool_calls\"",
        "\"function_call",
        "\"functionCall\"",
    ]
    .iter()
    .any(|needle| data.contains(needle))
}

fn nonempty_string(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|text| !text.is_empty())
}

fn has_meaningful_value(value: Option<&Value>) -> bool {
    value.is_some_and(|value| match value {
        Value::Null => false,
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
        Value::Bool(_) | Value::Number(_) => true,
    })
}

fn meaningful_tool_delta(value: &Value) -> bool {
    nonempty_string(value.get("id"))
        || nonempty_string(value.get("name"))
        || value.get("function").is_some_and(|function| {
            nonempty_string(function.get("name")) || has_meaningful_value(function.get("arguments"))
        })
}

/// True TTFT evidence: the first non-empty text/reasoning/tool payload that a
/// client can act on. Metadata, role-only chunks, keepalives, usage blocks and
/// terminal events deliberately do not count.
fn is_meaningful_stream_event(event: &Value) -> bool {
    let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");

    if let Some(delta) = event
        .get("delta")
        .filter(|_| event_type == "content_block_delta")
    {
        return nonempty_string(delta.get("text"))
            || nonempty_string(delta.get("thinking"))
            || nonempty_string(delta.get("partial_json"));
    }
    if event_type == "content_block_start"
        && event.get("content_block").is_some_and(|block| {
            block.get("type").and_then(Value::as_str) == Some("tool_use")
                && (nonempty_string(block.get("name")) || has_meaningful_value(block.get("input")))
        })
    {
        return true;
    }

    if (event_type.ends_with(".delta") || event_type.ends_with("_delta"))
        && (nonempty_string(event.get("delta"))
            || nonempty_string(event.get("text"))
            || nonempty_string(event.get("reasoning"))
            || nonempty_string(event.get("reasoning_content")))
    {
        return true;
    }
    if event_type == "response.output_item.added"
        && event.get("item").is_some_and(|item| {
            matches!(
                item.get("type").and_then(Value::as_str),
                Some("function_call" | "tool_call")
            ) && meaningful_tool_delta(item)
        })
    {
        return true;
    }

    if event
        .get("choices")
        .and_then(Value::as_array)
        .is_some_and(|choices| {
            choices.iter().any(|choice| {
                choice.get("delta").is_some_and(|delta| {
                    nonempty_string(delta.get("content"))
                        || nonempty_string(delta.get("reasoning_content"))
                        || nonempty_string(delta.get("reasoning"))
                        || nonempty_string(delta.get("thinking"))
                        || has_meaningful_value(delta.get("reasoning_details"))
                        || delta
                            .get("tool_calls")
                            .and_then(Value::as_array)
                            .is_some_and(|calls| calls.iter().any(meaningful_tool_delta))
                        || delta
                            .get("function_call")
                            .is_some_and(meaningful_tool_delta)
                })
            })
        })
    {
        return true;
    }

    event
        .get("candidates")
        .and_then(Value::as_array)
        .is_some_and(|candidates| {
            candidates.iter().any(|candidate| {
                candidate
                    .pointer("/content/parts")
                    .and_then(Value::as_array)
                    .is_some_and(|parts| {
                        parts.iter().any(|part| {
                            nonempty_string(part.get("text"))
                                || part.get("functionCall").is_some_and(|call| {
                                    nonempty_string(call.get("name"))
                                        || has_meaningful_value(call.get("args"))
                                })
                        })
                    })
            })
        })
}

struct SseUsageFinishGuard {
    collector: Option<SseUsageCollector>,
}

impl SseUsageFinishGuard {
    fn new(collector: SseUsageCollector) -> Self {
        Self {
            collector: Some(collector),
        }
    }

    fn disarm(&mut self) {
        self.collector = None;
    }
}

impl Drop for SseUsageFinishGuard {
    fn drop(&mut self) {
        if let Some(collector) = self.collector.take() {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    collector
                        .finish_with_error(Some(
                            "stream_abandoned_before_finalize: streaming response dropped before completion"
                                .to_string(),
                        ))
                        .await;
                });
            } else {
                log::warn!("Tokio runtime unavailable during SSE usage finalization guard; skipping async finish");
            }
        }
    }
}

// ============================================================================
// Internal helpers.
// ============================================================================

/// Creates a usage collector.
fn create_usage_collector(
    ctx: &RequestContext,
    state: &ProxyState,
    status_code: u16,
    parser_config: &UsageParserConfig,
) -> Option<SseUsageCollector> {
    let logging_enabled = state
        .config
        .try_read()
        .map(|c| c.enable_logging)
        .unwrap_or(true);
    if !logging_enabled {
        return None;
    }

    let state = state.clone();
    let provider_id = ctx.provider.id.clone();
    let request_model = ctx.request_model.clone();
    // If streaming events omit a model, prefer the mapped outbound model and then
    // the client-requested alias.
    let fallback_model = ctx
        .outbound_model
        .clone()
        .unwrap_or_else(|| ctx.request_model.clone());
    // Use ctx app_type, not parser_config. Claude Desktop passthrough reuses
    // CLAUDE_PARSER_CONFIG, whose app_type_str is claude; using it would misattribute
    // claude-desktop rows and lose provider pricing overrides.
    let app_type_str = ctx.app_type_str;
    let tag = ctx.tag;
    let start_time = ctx.start_time;
    let stream_parser = parser_config.stream_parser;
    let model_extractor = parser_config.model_extractor;
    let session_id = ctx.session_id.clone();

    Some(SseUsageCollector::new(
        start_time,
        parser_config.stream_event_filter,
        move |events, first_token_ms, terminal_error_message| {
            let (logical_status_code, stream_error_message) =
                logical_stream_outcome_with_terminal_error(
                    status_code,
                    &events,
                    terminal_error_message,
                );
            if let Some(usage) = stream_parser(&events) {
                let model = model_extractor(&events, &fallback_model);
                let latency_ms = start_time.elapsed().as_millis() as u64;

                let state = state.clone();
                let provider_id = provider_id.clone();
                let session_id = session_id.clone();
                let request_model = request_model.clone();
                let outbound_model = fallback_model.clone();

                tokio::spawn(async move {
                    log_usage_internal(
                        &state,
                        &provider_id,
                        app_type_str,
                        &model,
                        &request_model,
                        &outbound_model,
                        usage,
                        latency_ms,
                        first_token_ms,
                        true, // is_streaming
                        logical_status_code,
                        stream_error_message,
                        Some(session_id),
                    )
                    .await;
                });
            } else {
                let model = model_extractor(&events, &fallback_model);
                let latency_ms = start_time.elapsed().as_millis() as u64;
                let state = state.clone();
                let provider_id = provider_id.clone();
                let session_id = session_id.clone();
                let request_model = request_model.clone();
                let outbound_model = fallback_model.clone();

                tokio::spawn(async move {
                    log_usage_internal(
                        &state,
                        &provider_id,
                        app_type_str,
                        &model,
                        &request_model,
                        &outbound_model,
                        TokenUsage::default(),
                        latency_ms,
                        first_token_ms,
                        true, // is_streaming
                        logical_status_code,
                        stream_error_message,
                        Some(session_id),
                    )
                    .await;
                });
                log::debug!(
                    "[{tag}] Streaming response has no usage; recording request with unknown token usage"
                );
            }
        },
    ))
}

/// Records usage asynchronously.
struct UsageLogRequest<'a> {
    usage: TokenUsage,
    model: &'a str,
    request_model: &'a str,
    status_code: u16,
    is_streaming: bool,
}

fn spawn_log_usage(state: &ProxyState, ctx: &RequestContext, request: UsageLogRequest<'_>) {
    let UsageLogRequest {
        usage,
        model,
        request_model,
        status_code,
        is_streaming,
    } = request;
    // Check enable_logging before spawning the log task
    if let Ok(config) = state.config.try_read() {
        if !config.enable_logging {
            return;
        }
    }

    let state = state.clone();
    let provider_id = ctx.provider.id.clone();
    let app_type_str = ctx.app_type_str.to_string();
    let model = model.to_string();
    let request_model = request_model.to_string();
    // Request-pricing anchor: mapped outbound model, or request_model without mapping.
    let outbound_model = ctx
        .outbound_model
        .clone()
        .unwrap_or_else(|| ctx.request_model.clone());
    let latency_ms = ctx.latency_ms();
    let session_id = ctx.session_id.clone();

    tokio::spawn(async move {
        log_usage_internal(
            &state,
            &provider_id,
            &app_type_str,
            &model,
            &request_model,
            &outbound_model,
            usage,
            latency_ms,
            None,
            is_streaming,
            status_code,
            None,
            Some(session_id),
        )
        .await;
    });
}

pub(crate) fn usage_logging_enabled(state: &ProxyState) -> bool {
    state
        .config
        .try_read()
        .map(|config| config.enable_logging)
        .unwrap_or(true)
}

/// Internal usage-recording function.
///
/// `outbound_model` anchors request-pricing mode to the model actually sent upstream
/// after takeover mapping, or request_model when unmapped. This mode prices the
/// proxy's request rather than trusting upstream echo; pricing a client alias would
/// select the wrong row after mapping.
#[allow(clippy::too_many_arguments)]
async fn log_usage_internal(
    state: &ProxyState,
    provider_id: &str,
    app_type: &str,
    model: &str,
    request_model: &str,
    outbound_model: &str,
    usage: TokenUsage,
    latency_ms: u64,
    first_token_ms: Option<u64>,
    is_streaming: bool,
    status_code: u16,
    error_message: Option<String>,
    session_id: Option<String>,
) {
    use super::usage::logger::UsageLogger;

    let logger = UsageLogger::new(&state.db);
    let (multiplier, pricing_model_source) =
        logger.resolve_pricing_config(provider_id, app_type).await;
    let pricing_model = if pricing_model_source == PRICING_SOURCE_REQUEST {
        outbound_model
    } else {
        model
    };

    let request_id = usage.dedup_request_id();

    log::debug!(
        "[{app_type}] Recording request log: id={request_id}, provider={provider_id}, model={model}, streaming={is_streaming}, status={status_code}, latency_ms={latency_ms}, first_token_ms={first_token_ms:?}, session={}, input={}, output={}, cache_read={}, cache_creation={}",
        session_id.as_deref().unwrap_or("none"),
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_read_tokens,
        usage.cache_creation_tokens
    );

    if let Err(e) = logger.log_with_calculation_and_error(
        request_id,
        provider_id.to_string(),
        app_type.to_string(),
        model.to_string(),
        request_model.to_string(),
        pricing_model.to_string(),
        usage,
        multiplier,
        latency_ms,
        first_token_ms,
        status_code,
        error_message,
        session_id,
        None, // provider_type
        is_streaming,
    ) {
        log::warn!("[USG-001] Failed to record usage: {e}");
    }
}

/// Creates a passthrough stream with logging and timeout control.
pub fn create_logged_passthrough_stream(
    stream: impl Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static,
    tag: &'static str,
    usage_collector: Option<SseUsageCollector>,
    timeout_config: StreamingTimeoutConfig,
    connection_guard: Option<ActiveConnectionGuard>,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send {
    async_stream::stream! {
        let _conn_guard = connection_guard;
        let mut buffer = String::new();
        let mut utf8_remainder: Vec<u8> = Vec::new();
        let mut collector = usage_collector;
        let mut finish_guard = collector.clone().map(SseUsageFinishGuard::new);
        let inspect_sse_events =
            collector.is_some() || log::log_enabled!(log::Level::Debug);
        let mut is_first_chunk = true;
        let mut terminal_stream_error: Option<String> = None;

        // Timeout configuration.
        let first_byte_timeout = if timeout_config.first_byte_timeout > 0 {
            Some(Duration::from_secs(timeout_config.first_byte_timeout))
        } else {
            None
        };
        let idle_timeout = if timeout_config.idle_timeout > 0 {
            Some(Duration::from_secs(timeout_config.idle_timeout))
        } else {
            None
        };

        tokio::pin!(stream);

        loop {
            // Select first-byte or idle timeout.
            let timeout_duration = if is_first_chunk {
                first_byte_timeout
            } else {
                idle_timeout
            };

            let chunk_result = match timeout_duration {
                Some(duration) => {
                    match tokio::time::timeout(duration, stream.next()).await {
                        Ok(Some(chunk)) => Some(chunk),
                        Ok(None) => None, // End of stream.
                        Err(_) => {
                            // Timeout.
                            let timeout_type = if is_first_chunk { "first byte" } else { "idle period" };
                            let error_message = format!(
                                "stream_timeout: Streaming response {timeout_type} timed out after {}s",
                                duration.as_secs()
                            );
                            log::error!("[{tag}] {error_message}");
                            terminal_stream_error = Some(error_message.clone());
                            yield Err(std::io::Error::other(error_message));
                            break;
                        }
                    }
                }
                None => stream.next().await, // No timeout.
            };

            match chunk_result {
                Some(Ok(bytes)) => {
                    if is_first_chunk {
                        log::debug!(
                            "[{tag}] Received first upstream streaming chunk: bytes={}",
                            bytes.len()
                        );
                    }
                    is_first_chunk = false;
                    if inspect_sse_events {
                        crate::proxy::sse::append_utf8_safe(&mut buffer, &mut utf8_remainder, &bytes);

                        // Parse and record complete SSE events.
                        while let Some(event_text) = take_sse_block(&mut buffer) {
                            if !event_text.trim().is_empty() {
                                // Extract data; parse JSON only when a usage collector exists.
                                for line in event_text.lines() {
                                    if let Some(data) = strip_sse_field(line, "data") {
                                        if data.trim() != "[DONE]" {
                                            let collected = match &collector {
                                                Some(c) => {
                                                    let collect_usage = c.should_collect(data);
                                                    let inspect_timing = c.should_inspect_timing(data);
                                                    if collect_usage || inspect_timing {
                                                        match serde_json::from_str::<Value>(data) {
                                                            Ok(json_value) => {
                                                                if inspect_timing {
                                                                    c.observe_timing(&json_value).await;
                                                                }
                                                                if collect_usage {
                                                                    c.push(json_value).await;
                                                                }
                                                                collect_usage
                                                            }
                                                            Err(_) => false,
                                                        }
                                                    } else {
                                                        false
                                                    }
                                                }
                                                None => false,
                                            };
                                            if collected {
                                                log::debug!("[{tag}] <<< SSE event: {data}");
                                            } else {
                                                log::debug!("[{tag}] <<< SSE data: {data}");
                                            }
                                        } else {
                                            log::debug!("[{tag}] <<< SSE: [DONE]");
                                        }
                                    }
                                }
                            }
                        }
                    }

                    yield Ok(bytes);
                }
                Some(Err(e)) => {
                    let error_message = format!("stream_error: {e}");
                    log::error!("[{tag}] {error_message}");
                    terminal_stream_error = Some(error_message.clone());
                    yield Err(std::io::Error::other(error_message));
                    break;
                }
                None => {
                    // Normal end of stream.
                    break;
                }
            }
        }

        if let Some(c) = collector.take() {
            c.finish_with_error(terminal_stream_error).await;
        }
        if let Some(guard) = &mut finish_guard {
            guard.disarm();
        }
    }
}

fn format_headers(headers: &HeaderMap) -> String {
    headers
        .iter()
        .map(|(key, value)| {
            let value_str = value.to_str().unwrap_or("<non-utf8>");
            format!("{key}={value_str}")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;
    use crate::error::AppError;
    use crate::provider::ProviderMeta;
    use crate::proxy::failover_switch::FailoverSwitchManager;
    use crate::proxy::provider_router::ProviderRouter;
    use crate::proxy::providers::{
        codex_chat_history::CodexChatHistoryStore, gemini_shadow::GeminiShadowStore,
    };
    use crate::proxy::types::{ProxyConfig, ProxyStatus};
    use rust_decimal::Decimal;
    use std::collections::HashMap;
    use std::str::FromStr;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    #[test]
    fn test_strip_sse_field_accepts_optional_space() {
        assert_eq!(
            super::strip_sse_field("data: {\"ok\":true}", "data"),
            Some("{\"ok\":true}")
        );
        assert_eq!(
            super::strip_sse_field("data:{\"ok\":true}", "data"),
            Some("{\"ok\":true}")
        );
        assert_eq!(
            super::strip_sse_field("event: message_start", "event"),
            Some("message_start")
        );
        assert_eq!(
            super::strip_sse_field("event:message_start", "event"),
            Some("message_start")
        );
        assert_eq!(super::strip_sse_field("id:1", "data"), None);
    }

    #[test]
    fn test_logical_stream_outcome_marks_response_failed_as_failed() {
        let failed = serde_json::json!({
            "type": "response.failed",
            "response": {
                "error": {
                    "type": "upstream_reasoning_loop",
                    "message": "Upstream response repeated a tool-call placeholder inside reasoning output"
                }
            }
        });
        let (status, message) = super::logical_stream_outcome(200, &[failed]);
        assert_eq!(status, 502);
        assert_eq!(
            message.as_deref(),
            Some("upstream_reasoning_loop: Upstream response repeated a tool-call placeholder inside reasoning output")
        );

        let completed = serde_json::json!({
            "type": "response.completed",
            "response": {"status": "completed"}
        });
        let residual_failed = serde_json::json!({
            "type": "response.failed",
            "response": {"error": {"message": "ignored"}}
        });
        let (status, message) = super::logical_stream_outcome(200, &[completed, residual_failed]);
        assert_eq!(status, 200);
        assert!(message.is_none());
    }

    #[test]
    fn test_logical_stream_outcome_marks_anthropic_error_as_failed() {
        let failed = serde_json::json!({
            "type": "error",
            "error": {
                "type": "upstream_visible_output_corruption",
                "message": "Upstream response leaked a raw reasoning delimiter into assistant-visible output"
            }
        });
        let (status, message) = super::logical_stream_outcome(200, &[failed]);
        assert_eq!(status, 502);
        assert_eq!(
            message.as_deref(),
            Some("upstream_visible_output_corruption: Upstream response leaked a raw reasoning delimiter into assistant-visible output")
        );

        let completed = serde_json::json!({"type": "message_stop"});
        let residual_failed = serde_json::json!({
            "type": "error",
            "error": {"message": "ignored"}
        });
        let (status, message) = super::logical_stream_outcome(200, &[completed, residual_failed]);
        assert_eq!(status, 200);
        assert!(message.is_none());
    }

    #[test]
    fn test_logical_stream_outcome_marks_terminal_stream_errors() {
        let (status, message) = super::logical_stream_outcome_with_terminal_error(
            200,
            &[],
            Some(
                "stream_abandoned_before_finalize: streaming response dropped before completion"
                    .to_string(),
            ),
        );
        assert_eq!(status, 499);
        assert_eq!(
            message.as_deref(),
            Some("stream_abandoned_before_finalize: streaming response dropped before completion")
        );

        let (status, message) = super::logical_stream_outcome_with_terminal_error(
            200,
            &[],
            Some("stream_timeout: Streaming response idle period timed out after 30s".to_string()),
        );
        assert_eq!(status, 502);
        assert_eq!(
            message.as_deref(),
            Some("stream_timeout: Streaming response idle period timed out after 30s")
        );

        let (status, message) = super::logical_stream_outcome_with_terminal_error(
            504,
            &[],
            Some("stream_error: upstream reset".to_string()),
        );
        assert_eq!(status, 504);
        assert_eq!(message.as_deref(), Some("stream_error: upstream reset"));
    }

    #[test]
    fn test_ttft_requires_meaningful_text_reasoning_or_tool_payload() {
        let metadata = serde_json::json!({
            "type": "message_start",
            "message": {"id": "m1", "usage": {"input_tokens": 10}}
        });
        let usage_only = serde_json::json!({
            "choices": [],
            "usage": {"prompt_tokens": 10, "completion_tokens": 2}
        });
        let role_only = serde_json::json!({
            "choices": [{"delta": {"role": "assistant"}}]
        });
        assert!(!is_meaningful_stream_event(&metadata));
        assert!(!is_meaningful_stream_event(&usage_only));
        assert!(!is_meaningful_stream_event(&role_only));

        let claude_text = serde_json::json!({
            "type": "content_block_delta",
            "delta": {"type": "text_delta", "text": "hello"}
        });
        let openai_reasoning = serde_json::json!({
            "choices": [{"delta": {"reasoning_content": "think"}}]
        });
        let responses_tool = serde_json::json!({
            "type": "response.function_call_arguments.delta",
            "delta": "{\"path\":"
        });
        let gemini_tool = serde_json::json!({
            "candidates": [{"content": {"parts": [{
                "functionCall": {"name": "read_file", "args": {"path": "a"}}
            }]}}]
        });
        assert!(is_meaningful_stream_event(&claude_text));
        assert!(is_meaningful_stream_event(&openai_reasoning));
        assert!(is_meaningful_stream_event(&responses_tool));
        assert!(is_meaningful_stream_event(&gemini_tool));
    }

    #[test]
    fn test_strip_hop_by_hop_response_headers_removes_standard_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::CONNECTION,
            axum::http::HeaderValue::from_static("keep-alive"),
        );
        headers.insert(
            axum::http::header::HeaderName::from_static("keep-alive"),
            axum::http::HeaderValue::from_static("timeout=5"),
        );
        headers.insert(
            axum::http::header::TRANSFER_ENCODING,
            axum::http::HeaderValue::from_static("chunked"),
        );
        headers.insert(
            axum::http::header::HeaderName::from_static("proxy-connection"),
            axum::http::HeaderValue::from_static("keep-alive"),
        );
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        );
        headers.insert(
            axum::http::header::CONTENT_LENGTH,
            axum::http::HeaderValue::from_static("12"),
        );

        strip_hop_by_hop_response_headers(&mut headers);

        assert!(!headers.contains_key(axum::http::header::CONNECTION));
        assert!(!headers.contains_key("keep-alive"));
        assert!(!headers.contains_key(axum::http::header::TRANSFER_ENCODING));
        assert!(!headers.contains_key("proxy-connection"));
        assert_eq!(
            headers.get(axum::http::header::CONTENT_TYPE),
            Some(&axum::http::HeaderValue::from_static("application/json"))
        );
        assert_eq!(
            headers.get(axum::http::header::CONTENT_LENGTH),
            Some(&axum::http::HeaderValue::from_static("12"))
        );
    }

    #[test]
    fn test_strip_hop_by_hop_response_headers_removes_connection_listed_extensions() {
        let mut headers = HeaderMap::new();
        headers.append(
            axum::http::header::CONNECTION,
            axum::http::HeaderValue::from_static("x-trace-hop, x-debug-hop"),
        );
        headers.append(
            axum::http::header::CONNECTION,
            axum::http::HeaderValue::from_static("upgrade"),
        );
        headers.insert(
            axum::http::header::HeaderName::from_static("x-trace-hop"),
            axum::http::HeaderValue::from_static("trace"),
        );
        headers.insert(
            axum::http::header::HeaderName::from_static("x-debug-hop"),
            axum::http::HeaderValue::from_static("debug"),
        );
        headers.insert(
            axum::http::header::UPGRADE,
            axum::http::HeaderValue::from_static("websocket"),
        );
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("text/event-stream"),
        );

        strip_hop_by_hop_response_headers(&mut headers);

        assert!(!headers.contains_key(axum::http::header::CONNECTION));
        assert!(!headers.contains_key("x-trace-hop"));
        assert!(!headers.contains_key("x-debug-hop"));
        assert!(!headers.contains_key(axum::http::header::UPGRADE));
        assert_eq!(
            headers.get(axum::http::header::CONTENT_TYPE),
            Some(&axum::http::HeaderValue::from_static("text/event-stream"))
        );
    }

    fn build_state(db: Arc<Database>) -> ProxyState {
        ProxyState {
            db: db.clone(),
            config: Arc::new(RwLock::new(ProxyConfig::default())),
            status: Arc::new(RwLock::new(ProxyStatus::default())),
            start_time: Arc::new(RwLock::new(None)),
            current_providers: Arc::new(RwLock::new(HashMap::new())),
            provider_router: Arc::new(ProviderRouter::new(db.clone())),
            gemini_shadow: Arc::new(GeminiShadowStore::default()),
            codex_chat_history: Arc::new(CodexChatHistoryStore::default()),
            app_handle: None,
            failover_manager: Arc::new(FailoverSwitchManager::new(db)),
        }
    }

    fn seed_pricing(db: &Database) -> Result<(), AppError> {
        let conn = crate::database::lock_conn!(db.conn);
        conn.execute(
            "INSERT OR REPLACE INTO model_pricing (model_id, display_name, input_cost_per_million, output_cost_per_million)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["resp-model", "Resp Model", "1.0", "0"],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        conn.execute(
            "INSERT OR REPLACE INTO model_pricing (model_id, display_name, input_cost_per_million, output_cost_per_million)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["req-model", "Req Model", "2.0", "0"],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    fn insert_provider(
        db: &Database,
        id: &str,
        app_type: &str,
        meta: ProviderMeta,
    ) -> Result<(), AppError> {
        let meta_json =
            serde_json::to_string(&meta).map_err(|e| AppError::Database(e.to_string()))?;
        let conn = crate::database::lock_conn!(db.conn);
        conn.execute(
            "INSERT INTO providers (id, app_type, name, settings_config, meta)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, app_type, "Test Provider", "{}", meta_json],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    #[tokio::test]
    async fn test_log_usage_uses_provider_override_config() -> Result<(), AppError> {
        let db = Arc::new(Database::memory()?);
        let app_type = "claude";

        db.set_default_cost_multiplier(app_type, "1.5").await?;
        db.set_pricing_model_source(app_type, "response").await?;
        seed_pricing(&db)?;

        let meta = ProviderMeta {
            cost_multiplier: Some("2".to_string()),
            pricing_model_source: Some("request".to_string()),
            ..ProviderMeta::default()
        };
        insert_provider(&db, "provider-1", app_type, meta)?;

        let state = build_state(db.clone());
        let usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            model: None,
            message_id: None,
        };

        log_usage_internal(
            &state,
            "provider-1",
            app_type,
            "resp-model",
            "req-model",
            "req-model",
            usage,
            10,
            None,
            false,
            200,
            None,
            None,
        )
        .await;

        let conn = crate::database::lock_conn!(db.conn);
        let (model, request_model, total_cost, cost_multiplier): (String, String, String, String) =
            conn.query_row(
                "SELECT model, request_model, total_cost_usd, cost_multiplier
                 FROM proxy_request_logs WHERE provider_id = ?1",
                ["provider-1"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        assert_eq!(model, "resp-model");
        assert_eq!(request_model, "req-model");
        assert_eq!(
            Decimal::from_str(&cost_multiplier).unwrap(),
            Decimal::from_str("2").unwrap()
        );
        assert_eq!(
            Decimal::from_str(&total_cost).unwrap(),
            Decimal::from_str("4").unwrap()
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_request_pricing_mode_anchors_to_outbound_model() -> Result<(), AppError> {
        let db = Arc::new(Database::memory()?);
        let app_type = "claude";

        db.set_pricing_model_source(app_type, "request").await?;
        seed_pricing(&db)?;
        {
            let conn = crate::database::lock_conn!(db.conn);
            conn.execute(
                "INSERT OR REPLACE INTO model_pricing (model_id, display_name, input_cost_per_million, output_cost_per_million)
                 VALUES ('outbound-model', 'Outbound Model', '4.0', '0')",
                [],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        }

        insert_provider(&db, "provider-3", app_type, ProviderMeta::default())?;

        let state = build_state(db.clone());
        let usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            model: None,
            message_id: None,
        };

        // Routing takeover: client requests req-model at $2/M, proxy sends
        // outbound-model at $4/M, and upstream echoes resp-model. Request pricing
        // must anchor to the model actually sent.
        log_usage_internal(
            &state,
            "provider-3",
            app_type,
            "resp-model",
            "req-model",
            "outbound-model",
            usage,
            10,
            None,
            false,
            200,
            None,
            None,
        )
        .await;

        let conn = crate::database::lock_conn!(db.conn);
        let (model, request_model, total_cost): (String, String, String) = conn
            .query_row(
                "SELECT model, request_model, total_cost_usd
                 FROM proxy_request_logs WHERE provider_id = ?1",
                ["provider-3"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        // model and request_model columns are independent of the pricing anchor.
        assert_eq!(model, "resp-model");
        assert_eq!(request_model, "req-model");
        // Price outbound-model at $4/M, not req-model at $2/M or resp-model at $1/M.
        assert_eq!(
            Decimal::from_str(&total_cost).unwrap(),
            Decimal::from_str("4").unwrap()
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_claude_desktop_inherits_claude_global_defaults() -> Result<(), AppError> {
        use crate::proxy::usage::logger::UsageLogger;

        let db = Arc::new(Database::memory()?);

        // Global pricing has only Claude, Codex, and Gemini rows. Claude Desktop
        // inherits Claude instead of silently using factory defaults (1/response).
        db.set_default_cost_multiplier("claude", "1.5").await?;
        db.set_pricing_model_source("claude", "request").await?;

        let logger = UsageLogger::new(&db);
        let (multiplier, source) = logger
            .resolve_pricing_config("nonexistent-provider", "claude-desktop")
            .await;

        assert_eq!(multiplier, Decimal::from_str("1.5").unwrap());
        assert_eq!(source, "request");
        Ok(())
    }

    #[tokio::test]
    async fn test_log_usage_falls_back_to_global_defaults() -> Result<(), AppError> {
        let db = Arc::new(Database::memory()?);
        let app_type = "claude";

        db.set_default_cost_multiplier(app_type, "1.5").await?;
        db.set_pricing_model_source(app_type, "response").await?;
        seed_pricing(&db)?;

        let meta = ProviderMeta::default();
        insert_provider(&db, "provider-2", app_type, meta)?;

        let state = build_state(db.clone());
        let usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            model: None,
            message_id: None,
        };

        log_usage_internal(
            &state,
            "provider-2",
            app_type,
            "resp-model",
            "req-model",
            "req-model",
            usage,
            10,
            None,
            false,
            200,
            None,
            None,
        )
        .await;

        let conn = crate::database::lock_conn!(db.conn);
        let (total_cost, cost_multiplier): (String, String) = conn
            .query_row(
                "SELECT total_cost_usd, cost_multiplier
                 FROM proxy_request_logs WHERE provider_id = ?1",
                ["provider-2"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;

        assert_eq!(
            Decimal::from_str(&cost_multiplier).unwrap(),
            Decimal::from_str("1.5").unwrap()
        );
        assert_eq!(
            Decimal::from_str(&total_cost).unwrap(),
            Decimal::from_str("1.5").unwrap()
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_generic_stream_without_usage_persists_unknown_success_row() -> Result<(), AppError>
    {
        use crate::proxy::usage::logger::UsageLogger;

        let db = Arc::new(Database::memory()?);
        let callback_db = db.clone();
        let collector = SseUsageCollector::new(
            std::time::Instant::now(),
            None,
            move |events, first_token_ms, _terminal_error_message| {
                assert!(
                    !events.is_empty(),
                    "the generic SSE event should be collected"
                );
                assert!(
                    first_token_ms.is_some(),
                    "the text delta should establish TTFT"
                );
                let usage = TokenUsage::from_openai_stream_events(&events).unwrap_or_default();
                UsageLogger::new(&callback_db)
                    .log_with_calculation(
                        "generic-stream-empty-usage".to_string(),
                        "provider-1".to_string(),
                        "codex".to_string(),
                        "test-model".to_string(),
                        "test-model".to_string(),
                        "test-model".to_string(),
                        usage,
                        Decimal::from(1),
                        10,
                        first_token_ms,
                        200,
                        None,
                        None,
                        true,
                    )
                    .expect("logging an empty successful stream should persist timing");
            },
        );

        let upstream = futures::stream::iter([Ok::<_, std::io::Error>(Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n\
              data: [DONE]\n\n",
        ))]);
        let output: Vec<_> = create_logged_passthrough_stream(
            upstream,
            "generic-stream-test",
            Some(collector),
            StreamingTimeoutConfig {
                first_byte_timeout: 0,
                idle_timeout: 0,
            },
            None,
            None,
        )
        .collect()
        .await;
        assert_eq!(output.len(), 1);
        assert!(output[0].is_ok());

        let conn = crate::database::lock_conn!(db.conn);
        let (count, known, status, first_token_ms): (i64, i64, i64, Option<i64>) = conn.query_row(
            "SELECT COUNT(*), token_usage_known, status_code, first_token_ms
             FROM proxy_request_logs WHERE request_id = 'generic-stream-empty-usage'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        assert_eq!(count, 1);
        assert_eq!(known, 0);
        assert_eq!(status, 200);
        assert!(first_token_ms.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn test_codex_responses_reasoning_first_stream_persists_ttft() -> Result<(), AppError> {
        use crate::proxy::handler_config::codex_stream_usage_event_filter;
        use crate::proxy::usage::logger::UsageLogger;

        let db = Arc::new(Database::memory()?);
        let callback_db = db.clone();
        let collector = SseUsageCollector::new(
            std::time::Instant::now(),
            Some(codex_stream_usage_event_filter),
            move |events, first_token_ms, _terminal_error_message| {
                let usage = TokenUsage::from_codex_stream_events_auto(&events)
                    .expect("response.completed usage should be collected");
                UsageLogger::new(&callback_db)
                    .log_with_calculation(
                        "codex-responses-reasoning-first".to_string(),
                        "provider-1".to_string(),
                        "codex".to_string(),
                        "glm-5.2".to_string(),
                        "glm-5.2".to_string(),
                        "glm-5.2".to_string(),
                        usage,
                        Decimal::from(1),
                        10,
                        first_token_ms,
                        200,
                        None,
                        None,
                        true,
                    )
                    .expect("reasoning-first TTFT should reach the usage logger");
            },
        );

        let upstream = futures::stream::iter([Ok::<_, std::io::Error>(Bytes::from_static(
            b"event: response.created\n\
              data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}\n\n\
              event: response.reasoning_summary_text.delta\n\
              data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"reasoning\"}\n\n\
              event: response.output_text.delta\n\
              data: {\"type\":\"response.output_text.delta\",\"delta\":\"answer\"}\n\n\
              event: response.completed\n\
              data: {\"type\":\"response.completed\",\"response\":{\"model\":\"glm-5.2\",\"usage\":{\"input_tokens\":24,\"output_tokens\":49}}}\n\n",
        ))]);
        let output: Vec<_> = create_logged_passthrough_stream(
            upstream,
            "codex-reasoning-first-ttft-test",
            Some(collector),
            StreamingTimeoutConfig {
                first_byte_timeout: 0,
                idle_timeout: 0,
            },
            None,
            None,
        )
        .collect()
        .await;
        assert_eq!(output.len(), 1);
        assert!(output[0].is_ok());

        let conn = crate::database::lock_conn!(db.conn);
        let (input_tokens, output_tokens, first_token_ms): (i64, i64, Option<i64>) = conn
            .query_row(
                "SELECT input_tokens, output_tokens, first_token_ms
                 FROM proxy_request_logs WHERE request_id = 'codex-responses-reasoning-first'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
        assert_eq!(input_tokens, 24);
        assert_eq!(output_tokens, 49);
        assert!(first_token_ms.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn test_codex_responses_text_first_stream_establishes_ttft() {
        use crate::proxy::handler_config::codex_stream_usage_event_filter;

        let observed = Arc::new(std::sync::Mutex::new(None));
        let callback_observed = observed.clone();
        let collector = SseUsageCollector::new(
            std::time::Instant::now(),
            Some(codex_stream_usage_event_filter),
            move |_events, first_token_ms, _terminal_error_message| {
                *callback_observed.lock().expect("callback lock") = Some(first_token_ms);
            },
        );
        let upstream = futures::stream::iter([Ok::<_, std::io::Error>(Bytes::from_static(
            b"event: response.created\n\
              data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}\n\n\
              event: response.output_text.delta\n\
              data: {\"type\":\"response.output_text.delta\",\"delta\":\"answer\"}\n\n\
              event: response.completed\n\
              data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
        ))]);
        let _: Vec<_> = create_logged_passthrough_stream(
            upstream,
            "codex-text-first-ttft-test",
            Some(collector),
            StreamingTimeoutConfig {
                first_byte_timeout: 0,
                idle_timeout: 0,
            },
            None,
            None,
        )
        .collect()
        .await;

        assert!(
            matches!(*observed.lock().expect("result lock"), Some(Some(_))),
            "a non-empty output_text delta should establish TTFT"
        );
    }

    #[tokio::test]
    async fn test_codex_responses_metadata_only_stream_does_not_establish_ttft() {
        use crate::proxy::handler_config::codex_stream_usage_event_filter;

        let observed = Arc::new(std::sync::Mutex::new(None));
        let callback_observed = observed.clone();
        let collector = SseUsageCollector::new(
            std::time::Instant::now(),
            Some(codex_stream_usage_event_filter),
            move |_events, first_token_ms, _terminal_error_message| {
                *callback_observed.lock().expect("callback lock") = Some(first_token_ms);
            },
        );
        let upstream = futures::stream::iter([Ok::<_, std::io::Error>(Bytes::from_static(
            b"event: response.created\n\
              data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_1\"}}\n\n\
              event: response.output_item.added\n\
              data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[]}}\n\n\
              event: response.content_part.added\n\
              data: {\"type\":\"response.content_part.added\",\"part\":{\"type\":\"output_text\",\"text\":\"\"}}\n\n\
              event: response.completed\n\
              data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\n",
        ))]);
        let _: Vec<_> = create_logged_passthrough_stream(
            upstream,
            "codex-metadata-only-ttft-test",
            Some(collector),
            StreamingTimeoutConfig {
                first_byte_timeout: 0,
                idle_timeout: 0,
            },
            None,
            None,
        )
        .collect()
        .await;

        assert_eq!(
            *observed.lock().expect("result lock"),
            Some(None),
            "Responses metadata must not count as first-token latency"
        );
    }

    #[tokio::test]
    async fn test_usage_only_stream_does_not_establish_ttft() {
        let observed = Arc::new(std::sync::Mutex::new(None));
        let callback_observed = observed.clone();
        let collector = SseUsageCollector::new(
            std::time::Instant::now(),
            None,
            move |_events, first_token_ms, _terminal_error_message| {
                *callback_observed.lock().expect("callback lock") = Some(first_token_ms);
            },
        );
        let upstream = futures::stream::iter([Ok::<_, std::io::Error>(Bytes::from_static(
            b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2}}\n\n\
              data: [DONE]\n\n",
        ))]);
        let _: Vec<_> = create_logged_passthrough_stream(
            upstream,
            "ttft-usage-only-test",
            Some(collector),
            StreamingTimeoutConfig {
                first_byte_timeout: 0,
                idle_timeout: 0,
            },
            None,
            None,
        )
        .collect()
        .await;

        assert_eq!(
            *observed.lock().expect("result lock"),
            Some(None),
            "metadata and usage events must not count as first-token latency"
        );
    }
}
