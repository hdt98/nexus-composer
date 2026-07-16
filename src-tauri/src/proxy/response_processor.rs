//! 响应处理器模块
//!
//! 统一处理流式和非流式 API 响应

use super::{
    content_encoding::{decompress_body, get_content_encoding},
    error_mapper::{get_error_message, map_proxy_error_to_status},
    forwarder::ActiveConnectionGuard,
    handler_config::{StreamUsageEventFilter, UsageParserConfig},
    handler_context::{RequestAccountingGuard, RequestContext, StreamingTimeoutConfig},
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
// 响应头处理
// ============================================================================

/// RFC 2616 / RFC 7230 中定义的不应被代理继续转发的响应头。
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

/// 移除响应侧 hop-by-hop 头，以及 `Connection` 中点名的扩展头。
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

/// 移除在重建响应体后会失真的实体头。
pub(crate) fn strip_entity_headers_for_rebuilt_body(headers: &mut HeaderMap) {
    headers.remove(axum::http::header::CONTENT_ENCODING);
    headers.remove(axum::http::header::CONTENT_LENGTH);
    headers.remove(axum::http::header::TRANSFER_ENCODING);
}

/// 读取响应体并在需要时解压，确保 headers 与返回 body 一致。
///
/// `body_timeout`: 整包超时。当非零时用 `tokio::time::timeout` 包住 `.bytes()` 调用，
/// 防止上游发完响应头后卡住 body 导致请求永远挂住。
/// 传入 `Duration::ZERO` 表示不启用超时（故障转移关闭时）。
pub(crate) async fn read_decoded_body(
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
                    "响应体读取超时: {}s（上游发完响应头后 body 未到达）",
                    body_timeout.as_secs()
                ))
            })??
    };

    log::debug!(
        "[{tag}] 已接收上游响应体: status={}, bytes={}, headers={}",
        status.as_u16(),
        raw_bytes.len(),
        format_headers(&headers)
    );

    let mut body_bytes = raw_bytes.clone();
    let mut decoded = false;

    if let Some(encoding) = get_content_encoding(&headers) {
        log::debug!("[{tag}] 解压非流式响应: content-encoding={encoding}");
        match decompress_body(&encoding, &raw_bytes) {
            Ok(Some(decompressed)) => {
                body_bytes = Bytes::from(decompressed);
                decoded = true;
            }
            // 不支持的编码：原样透传且保留 content-encoding 头，
            // 让下游诊断/客户端知道这仍是压缩字节
            Ok(None) => {}
            Err(e) => {
                log::warn!("[{tag}] 解压失败 ({encoding}): {e}，使用原始数据");
            }
        }
    }

    if decoded {
        strip_entity_headers_for_rebuilt_body(&mut headers);
    }

    Ok((headers, status, body_bytes))
}

// ============================================================================
// 公共接口
// ============================================================================

/// 检测响应是否为 SSE 流式响应
#[inline]
pub fn is_sse_response(response: &ProxyResponse) -> bool {
    response.is_sse()
}

/// 处理流式响应
pub async fn handle_streaming(
    response: ProxyResponse,
    ctx: &RequestContext,
    state: &ProxyState,
    parser_config: &UsageParserConfig,
    connection_guard: Option<ActiveConnectionGuard>,
) -> Response {
    let status = response.status();
    log::debug!(
        "[{}] 已接收上游流式响应: status={}, headers={}",
        ctx.tag,
        status.as_u16(),
        format_headers(response.headers())
    );
    // 检查流式响应是否被压缩（SSE 通常不压缩，如果压缩则 SSE 解析会失败）
    if let Some(encoding) = get_content_encoding(response.headers()) {
        log::warn!(
            "[{}] 流式响应含 content-encoding={encoding}，SSE 解析可能失败。\
             上游在 accept-encoding 透传后压缩了 SSE 流。",
            ctx.tag
        );
    }

    let mut response_headers = response.headers().clone();
    strip_hop_by_hop_response_headers(&mut response_headers);

    let mut builder = axum::response::Response::builder().status(status);

    // 复制响应头
    for (key, value) in &response_headers {
        builder = builder.header(key, value);
    }

    // 创建字节流
    let stream = response.bytes_stream();

    // 创建使用量收集器；关闭 usage logging 时不要在流式热路径上解析每个 SSE event。
    let usage_collector = create_usage_collector(ctx, state, status.as_u16(), parser_config);

    // 获取流式超时配置
    let timeout_config = ctx.streaming_timeout_config();

    // 创建带日志和超时的透传流
    let logged_stream = create_logged_passthrough_stream(
        stream,
        ctx.tag,
        usage_collector,
        ctx.take_accounting_guard(),
        timeout_config,
        connection_guard,
    );

    let body = axum::body::Body::from_stream(logged_stream);
    match builder.body(body) {
        Ok(resp) => resp,
        Err(e) => {
            log::error!("[{}] 构建流式响应失败: {e}", ctx.tag);
            let error = ProxyError::Internal(format!("Failed to build streaming response: {e}"));
            log_proxy_error(state, ctx, true, &error);
            error.into_response()
        }
    }
}

/// 处理非流式响应
pub async fn handle_non_streaming(
    response: ProxyResponse,
    ctx: &RequestContext,
    state: &ProxyState,
    parser_config: &UsageParserConfig,
    // guard 在函数 scope 内持有，整包响应读取完成后随函数返回一并 drop
    _connection_guard: Option<ActiveConnectionGuard>,
) -> Result<Response, ProxyError> {
    // 整包超时：仅在故障转移开启且配置值非零时生效
    let body_timeout =
        if ctx.app_config.auto_failover_enabled && ctx.app_config.non_streaming_timeout > 0 {
            Duration::from_secs(ctx.app_config.non_streaming_timeout as u64)
        } else {
            Duration::ZERO
        };
    let (mut response_headers, status, body_bytes) =
        match read_decoded_body(response, ctx.tag, body_timeout).await {
            Ok(response) => response,
            Err(error) => {
                log_proxy_error(state, ctx, false, &error);
                return Err(error);
            }
        };
    strip_hop_by_hop_response_headers(&mut response_headers);

    log::debug!(
        "[{}] 上游响应体内容: {}",
        ctx.tag,
        String::from_utf8_lossy(&body_bytes)
    );

    // Parse before constructing the downstream response, but defer the write
    // until construction succeeds so a request cannot get two terminal rows.
    let usage = if ctx.accounting().is_some() {
        match serde_json::from_slice::<Value>(&body_bytes) {
            Ok(json) => {
                let usage = (parser_config.response_parser)(&json).unwrap_or_default();
                let model = usage
                    .model
                    .clone()
                    .filter(|model| !model.is_empty())
                    .or_else(|| {
                        json.get("model")
                            .and_then(Value::as_str)
                            .filter(|model| !model.is_empty())
                            .map(str::to_string)
                    })
                    .or_else(|| ctx.outbound_model.clone())
                    .unwrap_or_else(|| ctx.request_model.clone());
                Some((usage, model))
            }
            Err(_) => {
                log::debug!(
                    "[{}] <<< Non-JSON response: {} bytes",
                    ctx.tag,
                    body_bytes.len()
                );
                Some((
                    TokenUsage::default(),
                    ctx.outbound_model
                        .clone()
                        .unwrap_or_else(|| ctx.request_model.clone()),
                ))
            }
        }
    } else {
        log::debug!("[{}] usage logging 已关闭，跳过非流式 usage 解析", ctx.tag);
        None
    };

    // 构建响应
    let mut builder = axum::response::Response::builder().status(status);
    for (key, value) in response_headers.iter() {
        builder = builder.header(key, value);
    }

    let body = axum::body::Body::from(body_bytes);
    let response = builder.body(body).map_err(|e| {
        log::error!("[{}] 构建响应失败: {e}", ctx.tag);
        let error = ProxyError::Internal(format!("Failed to build response: {e}"));
        log_proxy_error(state, ctx, false, &error);
        error
    })?;
    if let Some((usage, model)) = usage {
        spawn_log_usage(state, ctx, usage, &model, status.as_u16(), false);
    }
    Ok(response)
}

/// 通用响应处理入口
///
/// 根据响应类型自动选择流式或非流式处理
pub async fn process_response(
    response: ProxyResponse,
    ctx: &RequestContext,
    state: &ProxyState,
    parser_config: &UsageParserConfig,
    connection_guard: Option<ActiveConnectionGuard>,
) -> Result<Response, ProxyError> {
    if is_sse_response(&response) {
        Ok(handle_streaming(response, ctx, state, parser_config, connection_guard).await)
    } else {
        handle_non_streaming(response, ctx, state, parser_config, connection_guard).await
    }
}

// ============================================================================
// SSE 使用量收集器
// ============================================================================

type UsageCallbackWithTiming =
    Arc<dyn Fn(Vec<Value>, Option<u64>, u16, Option<String>) + Send + Sync + 'static>;

/// SSE 使用量收集器
#[derive(Clone)]
pub struct SseUsageCollector {
    inner: Arc<SseUsageCollectorInner>,
}

struct SseUsageCollectorInner {
    events: Mutex<Vec<Value>>,
    first_event_time: Mutex<Option<std::time::Instant>>,
    start_time: std::time::Instant,
    success_status: u16,
    on_complete: UsageCallbackWithTiming,
    should_collect: Option<StreamUsageEventFilter>,
    finished: AtomicBool,
    saw_completion: AtomicBool,
}

impl SseUsageCollector {
    /// 创建使用量收集器；`should_collect` 用来在 hot path 跳过与 usage 无关的事件。
    pub fn new(
        start_time: std::time::Instant,
        should_collect: Option<StreamUsageEventFilter>,
        success_status: u16,
        callback: impl Fn(Vec<Value>, Option<u64>, u16, Option<String>) + Send + Sync + 'static,
    ) -> Self {
        let on_complete: UsageCallbackWithTiming = Arc::new(callback);
        Self {
            inner: Arc::new(SseUsageCollectorInner {
                events: Mutex::new(Vec::new()),
                first_event_time: Mutex::new(None),
                start_time,
                success_status,
                on_complete,
                should_collect,
                finished: AtomicBool::new(false),
                saw_completion: AtomicBool::new(false),
            }),
        }
    }

    pub fn should_collect(&self, data: &str) -> bool {
        self.inner
            .should_collect
            .map(|filter| filter(data))
            .unwrap_or(true)
    }

    /// Mark the first substantive downstream data event, independently of the
    /// usage-event filter (usage commonly arrives only at stream completion).
    pub async fn observe_data(&self, data: &str) {
        if !is_substantive_sse_data(data) {
            return;
        }
        let mut first_time = self.inner.first_event_time.lock().await;
        if first_time.is_none() {
            *first_time = Some(std::time::Instant::now());
        }
    }

    /// 推送 SSE 事件
    pub async fn push(&self, event: Value) {
        let mut events = self.inner.events.lock().await;
        events.push(event);
    }

    /// 完成收集并触发回调
    pub async fn finish(&self) {
        self.finish_result(self.inner.success_status, None).await;
    }

    pub async fn finish_with_error(&self, status_code: u16, message: String) {
        self.finish_result(status_code, Some(message)).await;
    }

    pub fn mark_completion(&self) {
        self.inner.saw_completion.store(true, Ordering::Release);
    }

    pub async fn finish_at_eof(&self) {
        if self.inner.saw_completion.load(Ordering::Acquire) {
            self.finish().await;
        } else {
            let status = if (200..300).contains(&self.inner.success_status) {
                502
            } else {
                self.inner.success_status
            };
            self.finish_with_error(
                status,
                "Upstream SSE stream ended before a terminal event".to_string(),
            )
            .await;
        }
    }

    async fn finish_result(&self, status_code: u16, error_message: Option<String>) {
        if self.inner.finished.swap(true, Ordering::SeqCst) {
            return;
        }

        let events = {
            let mut guard = self.inner.events.lock().await;
            std::mem::take(&mut *guard)
        };

        let first_token_ms = {
            let first_time = self.inner.first_event_time.lock().await;
            first_time.map(|t| (t - self.inner.start_time).as_millis() as u64)
        };

        (self.inner.on_complete)(events, first_token_ms, status_code, error_message);
    }
}

struct SseUsageFinishGuard {
    collector: Option<SseUsageCollector>,
    request_guard: Option<RequestAccountingGuard>,
}

impl SseUsageFinishGuard {
    fn new(collector: SseUsageCollector, request_guard: Option<RequestAccountingGuard>) -> Self {
        Self {
            collector: Some(collector),
            request_guard,
        }
    }

    fn disarm(&mut self) {
        self.collector = None;
        self.request_guard = None;
    }
}

impl Drop for SseUsageFinishGuard {
    fn drop(&mut self) {
        if let Some(collector) = self.collector.take() {
            let request_guard = self.request_guard.take();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    collector
                        .finish_with_error(499, "Request cancelled before completion".to_string())
                        .await;
                    drop(request_guard);
                });
            } else {
                log::warn!("SSE 用量收尾保护触发时 Tokio runtime 不可用，跳过异步 finish");
            }
        }
    }
}

// ============================================================================
// 内部辅助函数
// ============================================================================

fn has_output_value(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
        Value::String(text) => !text.is_empty(),
        Value::Array(values) => values.iter().any(has_output_value),
        Value::Object(values) => values.iter().any(|(key, value)| {
            !matches!(
                key.as_str(),
                "id" | "type"
                    | "role"
                    | "index"
                    | "status"
                    | "object"
                    | "sequence"
                    | "sequence_number"
            ) && has_output_value(value)
        }),
    }
}

fn is_substantive_sse_data(data: &str) -> bool {
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return false;
    }
    let Ok(event) = serde_json::from_str::<Value>(data) else {
        return true;
    };
    if let Some(event_type) = event.get("type").and_then(Value::as_str) {
        if matches!(
            event_type,
            "message_start"
                | "message_delta"
                | "message_stop"
                | "ping"
                | "response.created"
                | "response.in_progress"
                | "response.completed"
                | "response.failed"
                | "response.incomplete"
                | "error"
        ) {
            return false;
        }
        if event_type == "content_block_start" {
            return event.get("content_block").is_some_and(has_output_value);
        }
        if event_type.ends_with("_delta") || event_type.contains(".delta") {
            return event.get("delta").is_some_and(has_output_value);
        }
        if event_type == "response.output_item.added" {
            return event.get("item").is_some_and(has_output_value);
        }
    }
    event
        .get("choices")
        .and_then(Value::as_array)
        .is_some_and(|choices| {
            choices.iter().any(|choice| {
                choice
                    .get("delta")
                    .or_else(|| choice.get("message"))
                    .is_some_and(has_output_value)
            })
        })
        || event
            .get("candidates")
            .and_then(Value::as_array)
            .is_some_and(|candidates| {
                candidates.iter().any(|candidate| {
                    candidate
                        .get("content")
                        .or_else(|| candidate.get("functionCall"))
                        .is_some_and(has_output_value)
                })
            })
}

enum SseSignal {
    None,
    Completion,
    Terminal,
    Failed(String),
}

fn sse_signal(data: &str) -> SseSignal {
    if data.trim() == "[DONE]" {
        return SseSignal::Terminal;
    }
    let Ok(event) = serde_json::from_str::<Value>(data) else {
        return SseSignal::None;
    };

    match event.get("type").and_then(Value::as_str) {
        Some("response.completed" | "message_stop") => return SseSignal::Terminal,
        Some(kind @ ("response.failed" | "response.incomplete" | "error")) => {
            let detail = event
                .pointer("/response/error/message")
                .or_else(|| event.pointer("/error/message"))
                .or_else(|| event.pointer("/response/incomplete_details/reason"))
                .or_else(|| event.get("message"))
                .and_then(Value::as_str)
                .filter(|message| !message.is_empty())
                .unwrap_or(kind);
            return SseSignal::Failed(format!("Upstream SSE {kind}: {detail}"));
        }
        _ => {}
    }

    if event
        .get("candidates")
        .and_then(Value::as_array)
        .is_some_and(|candidates| {
            candidates.iter().any(|candidate| {
                candidate
                    .get("finishReason")
                    .is_some_and(|reason| !reason.is_null())
            })
        })
    {
        return SseSignal::Terminal;
    }

    if event
        .get("choices")
        .and_then(Value::as_array)
        .is_some_and(|choices| {
            choices.iter().any(|choice| {
                choice
                    .get("finish_reason")
                    .is_some_and(|reason| !reason.is_null())
            })
        })
    {
        return SseSignal::Completion;
    }

    SseSignal::None
}

/// 创建使用量收集器
pub(crate) fn create_usage_collector(
    ctx: &RequestContext,
    state: &ProxyState,
    status_code: u16,
    parser_config: &UsageParserConfig,
) -> Option<SseUsageCollector> {
    let accounting = ctx.accounting()?;

    let state = state.clone();
    let provider_id = ctx.provider.id.clone();
    let request_model = ctx.request_model.clone();
    // 流式事件缺失模型名时的归因兜底：映射后的出站模型（路由接管真值）优先，
    // 其次才是客户端请求别名
    let fallback_model = ctx
        .outbound_model
        .clone()
        .unwrap_or_else(|| ctx.request_model.clone());
    // 用 ctx 的 app_type 而不是 parser_config 的：Claude Desktop 流式透传复用
    // CLAUDE_PARSER_CONFIG（app_type_str="claude"），按 parser_config 记账会把
    // claude-desktop 的行错记到 claude 名下，导致供应商计价覆盖解析不到。
    let app_type_str = ctx.app_type_str;
    let tag = ctx.tag;
    let start_time = ctx.start_time;
    let stream_parser = parser_config.stream_parser;
    let model_extractor = parser_config.model_extractor;
    let session_id = ctx.session_id.clone();
    let correlation_id = ctx.correlation_id();

    Some(SseUsageCollector::new(
        start_time,
        parser_config.stream_event_filter,
        status_code,
        move |events, first_token_ms, final_status, error_message| {
            if !accounting.claim() {
                return;
            }
            let usage = stream_parser(&events);
            if usage.is_none() {
                log::debug!("[{tag}] 流式响应缺少 usage 统计，跳过消费记录");
            }
            let usage = usage.unwrap_or_default();
            let model = model_extractor(&events, &fallback_model);
            let latency_ms = start_time.elapsed().as_millis() as u64;

            let state = state.clone();
            let provider_id = provider_id.clone();
            let session_id = session_id.clone();
            let request_model = request_model.clone();
            let outbound_model = fallback_model.clone();
            let correlation_id = correlation_id.clone();

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
                    final_status,
                    Some(session_id),
                    correlation_id,
                    error_message,
                )
                .await;
            });
        },
    ))
}

/// 异步记录使用量
pub(crate) fn spawn_log_usage(
    state: &ProxyState,
    ctx: &RequestContext,
    usage: TokenUsage,
    model: &str,
    status_code: u16,
    is_streaming: bool,
) {
    let Some(accounting) = ctx.accounting() else {
        return;
    };
    if !accounting.claim() {
        return;
    }

    let state = state.clone();
    let provider_id = ctx.provider.id.clone();
    let app_type_str = ctx.app_type_str.to_string();
    let model = model.to_string();
    let request_model = ctx.request_model.clone();
    // 「按请求计价」模式的锚点：映射后的出站模型，无映射时等于 request_model
    let outbound_model = ctx
        .outbound_model
        .clone()
        .unwrap_or_else(|| ctx.request_model.clone());
    let latency_ms = ctx.latency_ms();
    let session_id = ctx.session_id.clone();
    let correlation_id = ctx.correlation_id();

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
            Some(session_id),
            correlation_id,
            None,
        )
        .await;
    });
}

pub(crate) fn log_proxy_error(
    state: &ProxyState,
    ctx: &RequestContext,
    is_streaming: bool,
    error: &ProxyError,
) {
    use super::usage::logger::UsageLogger;

    let Some(accounting) = ctx.accounting() else {
        return;
    };
    if !accounting.claim() {
        return;
    }

    let logger = UsageLogger::new(&state.db);
    if let Err(error) = logger.log_error_with_context(
        uuid::Uuid::new_v4().to_string(),
        ctx.correlation_id(),
        ctx.provider.id.clone(),
        ctx.app_type_str.to_string(),
        ctx.request_model.clone(),
        map_proxy_error_to_status(error),
        get_error_message(error),
        ctx.latency_ms(),
        is_streaming,
        Some(ctx.session_id.clone()),
        None,
    ) {
        log::warn!("Failed to record proxied request error: {error}");
    }
}

/// 内部使用量记录函数
///
/// `outbound_model` 是「按请求计价」模式的锚点：实际发往上游的模型
/// （路由接管映射后的真值，无映射时等于 request_model）。该模式的语义是
/// 「按代理发出的请求计价、不信任上游回显」，接管场景下发出的请求模型是
/// 映射后的 Y 而非客户端别名 X，按 X 计价会用错定价表行。
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
    session_id: Option<String>,
    correlation_id: Option<String>,
    error_message: Option<String>,
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
        "[{app_type}] 记录请求日志: id={request_id}, provider={provider_id}, model={model}, streaming={is_streaming}, status={status_code}, latency_ms={latency_ms}, first_token_ms={first_token_ms:?}, session={}, input={}, output={}, cache_read={}, cache_creation={}",
        session_id.as_deref().unwrap_or("none"),
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_read_tokens,
        usage.cache_creation_tokens
    );

    if let Err(e) = logger.log_with_calculation(
        request_id,
        correlation_id,
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
        session_id,
        None, // provider_type
        is_streaming,
        error_message,
    ) {
        log::warn!("[USG-001] 记录使用量失败: {e}");
    }
}

/// 创建带日志记录和超时控制的透传流
pub fn create_logged_passthrough_stream(
    stream: impl Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static,
    tag: &'static str,
    usage_collector: Option<SseUsageCollector>,
    request_guard: Option<RequestAccountingGuard>,
    timeout_config: StreamingTimeoutConfig,
    connection_guard: Option<ActiveConnectionGuard>,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send {
    // Construct the guard before the generator is first polled. A downstream
    // that disconnects without reading the body must still finalize its row.
    let finish_guard = usage_collector
        .clone()
        .map(|collector| SseUsageFinishGuard::new(collector, request_guard));
    async_stream::stream! {
        let _conn_guard = connection_guard;
        let mut buffer = String::new();
        let mut utf8_remainder: Vec<u8> = Vec::new();
        let mut collector = usage_collector;
        let mut finish_guard = finish_guard;
        let inspect_sse_events =
            collector.is_some() || log::log_enabled!(log::Level::Debug);
        let mut is_first_chunk = true;

        // 超时配置
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
            // 选择超时时间：首字节超时或静默期超时
            let timeout_duration = if is_first_chunk {
                first_byte_timeout
            } else {
                idle_timeout
            };

            let chunk_result = match timeout_duration {
                Some(duration) => {
                    match tokio::time::timeout(duration, stream.next()).await {
                        Ok(Some(chunk)) => Some(chunk),
                        Ok(None) => None, // 流结束
                        Err(_) => {
                            // 超时
                            let timeout_type = if is_first_chunk { "首字节" } else { "静默期" };
                            log::error!("[{tag}] 流式响应{}超时 ({}秒)", timeout_type, duration.as_secs());
                            if let Some(collector) = &collector {
                                collector
                                    .finish_with_error(
                                        504,
                                        "Upstream stream timed out".to_string(),
                                    )
                                    .await;
                            }
                            yield Err(std::io::Error::other(format!("流式响应{timeout_type}超时")));
                            break;
                        }
                    }
                }
                None => stream.next().await, // 无超时限制
            };

            match chunk_result {
                Some(Ok(bytes)) => {
                    if is_first_chunk {
                        log::debug!(
                            "[{tag}] 已接收上游流式首包: bytes={}",
                            bytes.len()
                        );
                    }
                    is_first_chunk = false;
                    if inspect_sse_events {
                        crate::proxy::sse::append_utf8_safe(&mut buffer, &mut utf8_remainder, &bytes);

                        // 尝试解析并记录完整的 SSE 事件
                        while let Some(event_text) = take_sse_block(&mut buffer) {
                            if !event_text.trim().is_empty() {
                                // 提取 data 部分；只有 usage collector 存在时才解析 JSON。
                                for line in event_text.lines() {
                                    if let Some(data) = strip_sse_field(line, "data") {
                                        if let Some(collector) = &collector {
                                            collector.observe_data(data).await;
                                        }
                                        if data.trim() != "[DONE]" {
                                            let collected = match &collector {
                                                Some(c) if c.should_collect(data) => {
                                                    match serde_json::from_str::<Value>(data) {
                                                        Ok(json_value) => {
                                                            c.push(json_value).await;
                                                            true
                                                        }
                                                        Err(_) => false,
                                                    }
                                                }
                                                _ => false,
                                            };
                                            if collected {
                                                log::debug!("[{tag}] <<< SSE 事件: {data}");
                                            } else {
                                                log::debug!("[{tag}] <<< SSE 数据: {data}");
                                            }
                                        } else {
                                            log::debug!("[{tag}] <<< SSE: [DONE]");
                                        }
                                        if let Some(collector) = &collector {
                                            match sse_signal(data) {
                                                SseSignal::None => {}
                                                SseSignal::Completion => {
                                                    collector.mark_completion();
                                                }
                                                SseSignal::Terminal => {
                                                    collector.mark_completion();
                                                    collector.finish().await;
                                                }
                                                SseSignal::Failed(message) => {
                                                    collector.finish_with_error(502, message).await;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    yield Ok(bytes);
                }
                Some(Err(e)) => {
                    log::error!("[{tag}] 流错误: {e}");
                    if let Some(collector) = &collector {
                        collector
                            .finish_with_error(502, "Upstream stream failed".to_string())
                            .await;
                    }
                    yield Err(std::io::Error::other(e.to_string()));
                    break;
                }
                None => {
                    // 流正常结束
                    break;
                }
            }
        }

        if let Some(c) = collector.take() {
            c.finish_at_eof().await;
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
    use crate::app_config::AppType;
    use crate::database::Database;
    use crate::error::AppError;
    use crate::provider::ProviderMeta;
    use crate::proxy::failover_switch::FailoverSwitchManager;
    use crate::proxy::handler_config::{
        CLAUDE_PARSER_CONFIG, CODEX_PARSER_CONFIG, OPENAI_PARSER_CONFIG,
    };
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

    async fn accounting_context(
        app_type: AppType,
        app_type_str: &'static str,
        request_id: &str,
    ) -> Result<(Arc<Database>, ProxyState, RequestContext), AppError> {
        let db = Arc::new(Database::memory()?);
        let provider_id = format!("{app_type_str}-usage-test");
        insert_provider(
            &db,
            &provider_id,
            app_type_str,
            ProviderMeta {
                provider_type: Some("nexus".to_string()),
                ..ProviderMeta::default()
            },
        )?;
        db.set_current_provider(app_type_str, &provider_id)?;
        let state = build_state(db.clone());
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", request_id.parse().unwrap());
        let ctx = RequestContext::new(
            &state,
            &serde_json::json!({"model": "GLM-5.2-FP8"}),
            &headers,
            app_type,
            "Test",
            app_type_str,
        )
        .await
        .map_err(|error| AppError::Message(error.to_string()))?;
        Ok((db, state, ctx))
    }

    async fn wait_for_usage_row(db: &Database) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let count = db
                    .conn
                    .lock()
                    .expect("lock usage database")
                    .query_row("SELECT COUNT(*) FROM proxy_request_logs", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .unwrap_or(0);
                if count > 0 {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("timed out waiting for usage row");
    }

    type StoredStreamOutcome = (
        i64,
        i64,
        i64,
        i64,
        Option<i64>,
        Option<i64>,
        Option<String>,
        Option<String>,
    );

    fn stored_stream_outcome(db: &Database) -> Result<StoredStreamOutcome, AppError> {
        let stored = db.conn.lock().expect("lock usage database").query_row(
            "SELECT COUNT(*), status_code, input_tokens, cache_read_tokens,
                    first_token_ms, duration_ms, error_message, correlation_id
             FROM proxy_request_logs",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )?;
        Ok(stored)
    }

    #[test]
    fn substantive_sse_detection_excludes_metadata_and_usage_trailers() {
        for data in [
            "[DONE]",
            r#"{"type":"message_start","message":{"usage":{"input_tokens":10}}}"#,
            r#"{"type":"message_delta","usage":{"output_tokens":2}}"#,
            r#"{"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2}}"#,
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":10}}}"#,
            r#"{"type":"response.output_item.added","sequence":1,"item":{"id":"item_1","type":"message","status":"in_progress","object":"response.output_item"}}"#,
            r#"{"candidates":[{"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10}}"#,
        ] {
            assert!(!is_substantive_sse_data(data), "metadata event: {data}");
        }
        for data in [
            r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"answer"}}"#,
            r#"{"choices":[{"delta":{"content":"answer"}}]}"#,
            r#"{"type":"response.output_text.delta","delta":"answer"}"#,
            r#"{"candidates":[{"content":{"parts":[{"text":"answer"}]}}]}"#,
        ] {
            assert!(is_substantive_sse_data(data), "output event: {data}");
        }
    }

    #[tokio::test]
    async fn all_stream_paths_record_one_zero_usage_row() -> Result<(), AppError> {
        for (app_type, app_type_str, parser) in [
            (AppType::Codex, "codex", &OPENAI_PARSER_CONFIG),
            (AppType::Claude, "claude", &CLAUDE_PARSER_CONFIG),
            (AppType::Codex, "codex", &CODEX_PARSER_CONFIG),
        ] {
            let request_id = format!("request-{app_type_str}");
            let (db, state, ctx) = accounting_context(app_type, app_type_str, &request_id).await?;
            create_usage_collector(&ctx, &state, 200, parser)
                .expect("usage logging enabled")
                .finish()
                .await;
            wait_for_usage_row(&db).await;

            let stored: (i64, i64, i64, Option<i64>, Option<String>) =
                db.conn.lock().expect("lock usage database").query_row(
                    "SELECT COUNT(*), SUM(input_tokens + output_tokens), status_code,
                            duration_ms, correlation_id FROM proxy_request_logs",
                    [],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    },
                )?;
            assert_eq!(stored.0, 1);
            assert_eq!(stored.1, 0);
            assert_eq!(stored.2, 200);
            assert!(stored.3.is_some());
            assert_eq!(stored.4.as_deref(), Some(request_id.as_str()));
        }
        Ok(())
    }

    #[tokio::test]
    async fn dropped_request_context_records_one_cancelled_row() -> Result<(), AppError> {
        let (db, _state, ctx) =
            accounting_context(AppType::Codex, "codex", "request-cancelled").await?;
        let accounting_clone = ctx.accounting().expect("usage accounting enabled");
        drop(ctx);
        wait_for_usage_row(&db).await;
        drop(accounting_clone);

        let stored: (i64, i64, Option<String>, Option<String>) =
            db.conn.lock().expect("lock usage database").query_row(
                "SELECT COUNT(*), status_code, error_message, correlation_id
                 FROM proxy_request_logs",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        assert_eq!(
            stored,
            (
                1,
                499,
                Some("Request cancelled before completion".to_string()),
                Some("request-cancelled".to_string()),
            )
        );
        Ok(())
    }

    #[tokio::test]
    async fn explicit_error_and_context_drop_record_once() -> Result<(), AppError> {
        let (db, state, ctx) =
            accounting_context(AppType::Codex, "codex", "request-timeout").await?;
        log_proxy_error(
            &state,
            &ctx,
            true,
            &ProxyError::Timeout("upstream timed out".to_string()),
        );
        drop(ctx);

        let stored: (i64, i64) = db.conn.lock().expect("lock usage database").query_row(
            "SELECT COUNT(*), status_code FROM proxy_request_logs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(stored, (1, 504));
        Ok(())
    }

    #[tokio::test]
    async fn non_stream_success_and_context_drop_record_once() -> Result<(), AppError> {
        let (db, state, ctx) =
            accounting_context(AppType::Codex, "codex", "request-success").await?;
        spawn_log_usage(
            &state,
            &ctx,
            TokenUsage {
                input_tokens: 12,
                output_tokens: 3,
                ..TokenUsage::default()
            },
            "GLM-5.2-FP8",
            200,
            false,
        );
        drop(ctx);
        wait_for_usage_row(&db).await;

        let stored: (i64, i64, i64, i64) = db.conn.lock().expect("lock usage database").query_row(
            "SELECT COUNT(*), status_code, input_tokens, output_tokens FROM proxy_request_logs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        assert_eq!(stored, (1, 200, 12, 3));
        Ok(())
    }

    async fn assert_openai_usage_trailer(
        terminator: &str,
        request_id: &str,
    ) -> Result<(), AppError> {
        let (db, state, ctx) = accounting_context(AppType::Codex, "codex", request_id).await?;
        let sse = [
            concat!(
                "data: {\"id\":\"chatcmpl-1\",\"model\":\"GLM-5.2-FP8\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: {\"id\":\"chatcmpl-1\",\"model\":\"GLM-5.2-FP8\",\"choices\":[],\"usage\":{\"prompt_tokens\":120,\"completion_tokens\":7,\"total_tokens\":127}}\n\n"
            ),
            terminator,
        ]
        .concat();
        let upstream = futures::stream::iter(vec![Ok(Bytes::from(sse))]);
        let mut downstream = Box::pin(create_logged_passthrough_stream(
            upstream,
            "Test",
            create_usage_collector(&ctx, &state, 200, &OPENAI_PARSER_CONFIG),
            ctx.take_accounting_guard(),
            StreamingTimeoutConfig {
                first_byte_timeout: 0,
                idle_timeout: 0,
            },
            None,
        ));
        while downstream.next().await.is_some() {}
        wait_for_usage_row(&db).await;

        let stored: (i64, i64, i64, i64) = db.conn.lock().expect("lock usage database").query_row(
            "SELECT COUNT(*), status_code, input_tokens, output_tokens FROM proxy_request_logs",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        assert_eq!(stored, (1, 200, 120, 7));
        Ok(())
    }

    #[tokio::test]
    async fn openai_stream_waits_for_usage_trailer_after_finish_reason() -> Result<(), AppError> {
        assert_openai_usage_trailer("data: [DONE]\n\n", "request-usage-trailer-done").await
    }

    #[tokio::test]
    async fn openai_stream_clean_eof_after_usage_trailer_is_success() -> Result<(), AppError> {
        assert_openai_usage_trailer("", "request-usage-trailer-eof").await
    }

    #[tokio::test]
    async fn responses_failures_preserve_partial_usage_and_error() -> Result<(), AppError> {
        for (name, sse, expected_input, expected_output) in [
            (
                "failed",
                "data: {\"type\":\"response.failed\",\"response\":{\"id\":\"resp_failed\",\"model\":\"GLM-5.2-FP8\",\"status\":\"failed\",\"usage\":{\"input_tokens\":80,\"output_tokens\":4},\"error\":{\"message\":\"generation failed\"}}}\n\n",
                80,
                4,
            ),
            (
                "incomplete",
                "data: {\"type\":\"response.incomplete\",\"response\":{\"id\":\"resp_incomplete\",\"model\":\"GLM-5.2-FP8\",\"status\":\"incomplete\",\"usage\":{\"input_tokens\":90,\"output_tokens\":5},\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n\n",
                90,
                5,
            ),
            (
                "error",
                concat!(
                    "data: {\"id\":\"chatcmpl-error\",\"model\":\"GLM-5.2-FP8\",\"choices\":[],\"usage\":{\"prompt_tokens\":70,\"completion_tokens\":3,\"total_tokens\":73}}\n\n",
                    "data: {\"type\":\"error\",\"error\":{\"message\":\"stream failed\"}}\n\n"
                ),
                70,
                3,
            ),
        ] {
            let request_id = format!("request-{name}");
            let (db, state, ctx) =
                accounting_context(AppType::Codex, "codex", &request_id).await?;
            let upstream = futures::stream::iter(vec![Ok(Bytes::from(sse))]);
            let mut downstream = Box::pin(create_logged_passthrough_stream(
                upstream,
                "Test",
                create_usage_collector(&ctx, &state, 200, &CODEX_PARSER_CONFIG),
                ctx.take_accounting_guard(),
                StreamingTimeoutConfig {
                    first_byte_timeout: 0,
                    idle_timeout: 0,
                },
                None,
            ));
            while downstream.next().await.is_some() {}
            wait_for_usage_row(&db).await;

            let stored: (i64, i64, i64, i64, Option<String>, Option<i64>) = db
                .conn
                .lock()
                .expect("lock usage database")
                .query_row(
                    "SELECT COUNT(*), status_code, input_tokens, output_tokens,
                            error_message, first_token_ms FROM proxy_request_logs",
                    [],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                        ))
                    },
                )?;
            assert_eq!(stored.0, 1, "{name}");
            assert_eq!(stored.1, 502, "{name}");
            assert_eq!((stored.2, stored.3), (expected_input, expected_output));
            assert!(stored.4.as_deref().is_some_and(|error| error.contains(name)));
            assert_eq!(stored.5, None, "terminal metadata is not TTFT");
        }
        Ok(())
    }

    #[tokio::test]
    async fn truncated_sse_eof_is_not_recorded_as_success() -> Result<(), AppError> {
        let (db, state, ctx) =
            accounting_context(AppType::Codex, "codex", "request-truncated").await?;
        let upstream = futures::stream::iter(vec![Ok(Bytes::from(
            "data: {\"id\":\"chatcmpl-truncated\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n",
        ))]);
        let mut downstream = Box::pin(create_logged_passthrough_stream(
            upstream,
            "Test",
            create_usage_collector(&ctx, &state, 200, &OPENAI_PARSER_CONFIG),
            ctx.take_accounting_guard(),
            StreamingTimeoutConfig {
                first_byte_timeout: 0,
                idle_timeout: 0,
            },
            None,
        ));
        while downstream.next().await.is_some() {}
        wait_for_usage_row(&db).await;

        let stored: (i64, i64, Option<String>, Option<i64>) =
            db.conn.lock().expect("lock usage database").query_row(
                "SELECT COUNT(*), status_code, error_message, first_token_ms
                 FROM proxy_request_logs",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        assert_eq!((stored.0, stored.1), (1, 502));
        assert!(stored
            .2
            .as_deref()
            .is_some_and(|error| error.contains("terminal event")));
        assert!(stored.3.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn aborted_stream_preserves_partial_usage_and_ttft() -> Result<(), AppError> {
        let (db, state, ctx) =
            accounting_context(AppType::Claude, "claude", "request-partial").await?;
        let collector = create_usage_collector(&ctx, &state, 200, &CLAUDE_PARSER_CONFIG)
            .expect("usage logging enabled");
        let upstream = futures::stream::iter(vec![Ok(Bytes::from(concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_partial\",\"model\":\"GLM-5.2-FP8\",\"usage\":{\"input_tokens\":120,\"cache_read_input_tokens\":20}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n"
        )))])
        .chain(futures::stream::pending());
        let mut downstream = Box::pin(create_logged_passthrough_stream(
            upstream,
            "Test",
            Some(collector),
            ctx.take_accounting_guard(),
            StreamingTimeoutConfig {
                first_byte_timeout: 0,
                idle_timeout: 0,
            },
            None,
        ));
        assert!(downstream.next().await.unwrap().is_ok());
        drop(downstream);
        wait_for_usage_row(&db).await;

        let stored = stored_stream_outcome(&db)?;
        assert_eq!((stored.0, stored.1, stored.2, stored.3), (1, 499, 120, 20));
        assert!(stored.4.is_some());
        assert!(stored.5.is_some());
        assert_eq!(
            stored.6.as_deref(),
            Some("Request cancelled before completion")
        );
        assert_eq!(stored.7.as_deref(), Some("request-partial"));
        Ok(())
    }

    #[tokio::test]
    async fn unread_stream_still_records_one_aborted_row() -> Result<(), AppError> {
        let (db, state, ctx) =
            accounting_context(AppType::Codex, "codex", "request-unread").await?;
        let stream = create_logged_passthrough_stream(
            futures::stream::pending(),
            "Test",
            create_usage_collector(&ctx, &state, 200, &CODEX_PARSER_CONFIG),
            ctx.take_accounting_guard(),
            StreamingTimeoutConfig {
                first_byte_timeout: 0,
                idle_timeout: 0,
            },
            None,
        );
        drop(stream);
        wait_for_usage_row(&db).await;

        let stored: (i64, i64, Option<String>, Option<String>) =
            db.conn.lock().expect("lock usage database").query_row(
                "SELECT COUNT(*), status_code, error_message, correlation_id
             FROM proxy_request_logs",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        assert_eq!(
            stored,
            (
                1,
                499,
                Some("Request cancelled before completion".to_string()),
                Some("request-unread".to_string()),
            )
        );
        Ok(())
    }

    #[tokio::test]
    async fn configured_stream_timeout_records_stable_error_and_partial_usage(
    ) -> Result<(), AppError> {
        let (db, state, ctx) =
            accounting_context(AppType::Claude, "claude", "request-stream-timeout").await?;
        let upstream = futures::stream::iter(vec![Ok(Bytes::from(concat!(
            "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_timeout\",\"model\":\"GLM-5.2-FP8\",\"usage\":{\"input_tokens\":90,\"cache_read_input_tokens\":10}}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n"
        )))])
        .chain(futures::stream::pending());
        let mut downstream = Box::pin(create_logged_passthrough_stream(
            upstream,
            "Test",
            create_usage_collector(&ctx, &state, 200, &CLAUDE_PARSER_CONFIG),
            ctx.take_accounting_guard(),
            StreamingTimeoutConfig {
                first_byte_timeout: 0,
                idle_timeout: 1,
            },
            None,
        ));
        assert!(downstream.next().await.unwrap().is_ok());
        assert!(downstream.next().await.unwrap().is_err());
        assert!(downstream.next().await.is_none());
        wait_for_usage_row(&db).await;

        let stored = stored_stream_outcome(&db)?;
        assert_eq!((stored.0, stored.1, stored.2, stored.3), (1, 504, 90, 10));
        assert!(stored.4.is_some());
        assert!(stored.5.is_some());
        assert_eq!(stored.6.as_deref(), Some("Upstream stream timed out"));
        assert_eq!(stored.7.as_deref(), Some("request-stream-timeout"));
        Ok(())
    }

    #[tokio::test]
    async fn upstream_stream_error_records_stable_error_and_partial_usage() -> Result<(), AppError>
    {
        let (db, state, ctx) =
            accounting_context(AppType::Claude, "claude", "request-stream-error").await?;
        let upstream = futures::stream::iter(vec![
            Ok(Bytes::from(concat!(
                "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_error\",\"model\":\"GLM-5.2-FP8\",\"usage\":{\"input_tokens\":80,\"cache_read_input_tokens\":8}}}\n\n",
                "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n"
            ))),
            Err(std::io::Error::other("private upstream transport detail")),
        ]);
        let mut downstream = Box::pin(create_logged_passthrough_stream(
            upstream,
            "Test",
            create_usage_collector(&ctx, &state, 200, &CLAUDE_PARSER_CONFIG),
            ctx.take_accounting_guard(),
            StreamingTimeoutConfig {
                first_byte_timeout: 0,
                idle_timeout: 0,
            },
            None,
        ));
        assert!(downstream.next().await.unwrap().is_ok());
        assert!(downstream.next().await.unwrap().is_err());
        assert!(downstream.next().await.is_none());
        wait_for_usage_row(&db).await;

        let stored = stored_stream_outcome(&db)?;
        assert_eq!((stored.0, stored.1, stored.2, stored.3), (1, 502, 80, 8));
        assert!(stored.4.is_some());
        assert!(stored.5.is_some());
        assert_eq!(stored.6.as_deref(), Some("Upstream stream failed"));
        assert!(!stored
            .6
            .as_deref()
            .is_some_and(|message| message.contains("private upstream")));
        assert_eq!(stored.7.as_deref(), Some("request-stream-error"));
        Ok(())
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

        // 路由接管场景：客户端请求 req-model（$2/M），代理实际发出 outbound-model
        // （$4/M），上游回显 resp-model。「按请求计价」必须锚定实际发出的模型。
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

        // model / request_model 列不受计价锚点影响
        assert_eq!(model, "resp-model");
        assert_eq!(request_model, "req-model");
        // 按 outbound-model（$4/M）计价，而不是 req-model（$2/M）或 resp-model（$1/M）
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

        // 全局计费配置只有 claude/codex/gemini 三行；claude-desktop 的
        // 全局默认必须继承 claude，而不是静默落回工厂默认（1 / response）
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
}
