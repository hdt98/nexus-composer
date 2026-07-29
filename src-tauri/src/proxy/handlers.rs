//! Request handlers.
//!
//! Handles HTTP requests for the supported API endpoints.
//!
//! Refactored structure:
//! - Common behavior lives in `handler_context` and `response_processor`.
//! - Each handler retains only endpoint-specific business logic.
//! - Claude protocol transformation remains here as a fallback for the legacy
//!   OpenRouter interface.

use super::{
    content_encoding::{decompress_body, get_content_encoding, is_supported_content_encoding},
    error_mapper::{get_error_message, map_proxy_error_to_status},
    forwarder::{is_claude_count_tokens_path, ActiveConnectionGuard, RequestForwarder},
    handler_config::{
        claude_stream_usage_event_filter, codex_stream_usage_event_filter, UsageParserConfig,
        CLAUDE_PARSER_CONFIG, CODEX_PARSER_CONFIG, GEMINI_PARSER_CONFIG, OPENAI_PARSER_CONFIG,
    },
    handler_context::RequestContext,
    providers::{
        chat_reasoning::{
            enabled_for_attempt, enabled_for_request, error_event_message, normalize_first_choice,
            normalize_sse_response, normalize_sse_stream, ProtocolError,
        },
        codex_chat_common::extract_reasoning_field_text,
        codex_chat_history::record_responses_sse_stream,
        get_adapter, get_claude_api_format,
        streaming::{create_anthropic_sse_stream, create_anthropic_sse_stream_with_mode},
        streaming_codex_chat::{
            create_responses_sse_stream_from_chat_with_mode, is_clearly_unfinished_progress,
            recover_progress_only_chat_stream, BoxChatStream, ChatContinuation,
            MAX_PROGRESS_CONTINUATIONS,
        },
        streaming_gemini::create_anthropic_sse_stream_from_gemini,
        streaming_responses::create_anthropic_sse_stream_from_responses,
        transform, transform_codex_chat, transform_gemini, transform_responses,
    },
    response_processor::{
        create_logged_passthrough_stream, handle_non_streaming, process_response,
        read_decoded_body, strip_entity_headers_for_rebuilt_body,
        strip_hop_by_hop_response_headers, usage_logging_enabled, SseUsageCollector,
    },
    server::ProxyState,
    sse::{strip_sse_field, take_sse_block},
    types::*,
    usage::parser::TokenUsage,
    ProxyError,
};
use crate::app_config::AppType;
use crate::database::PRICING_SOURCE_REQUEST;
use crate::provider::Provider;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use bytes::Bytes;
use http::Extensions;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;

// ============================================================================
// Health and status endpoints.
// ============================================================================

/// Health check.
pub async fn health_check() -> (StatusCode, Json<Value>) {
    (
        StatusCode::OK,
        Json(json!({
            "status": "healthy",
            "timestamp": chrono::Utc::now().to_rfc3339(),
        })),
    )
}

/// Returns service status.
pub async fn get_status(State(state): State<ProxyState>) -> Result<Json<ProxyStatus>, ProxyError> {
    let status = state.status.read().await.clone();
    Ok(Json(status))
}

/// GET /v1/models — Codex model list (reachability check)
///
/// Codex CLI probes this endpoint at startup and deserializes the response as a
/// catalog with a top-level `models` field.  Return the nexus-composer–managed model
/// catalog file directly so the format always matches what the current version
/// of Codex expects.
///
/// Only serves the catalog when the live config.toml still references the
/// nexus-composer–owned `model_catalog_json`, using the same path ownership rules as
/// Codex live-setting import.
pub async fn handle_models() -> Result<Json<Value>, ProxyError> {
    let generated_path = crate::codex_config::get_codex_model_catalog_path();
    let active_catalog_path = match crate::codex_config::read_codex_config_text() {
        Ok(config_text) => {
            crate::codex_config::resolve_nexus_catalog_path(&config_text, &generated_path)
        }
        Err(_) => None,
    };

    let catalog = if let Some(catalog_path) =
        active_catalog_path.as_ref().filter(|path| path.exists())
    {
        let text = std::fs::read_to_string(catalog_path).unwrap_or_default();
        serde_json::from_str(&text).unwrap_or(json!({"models": []}))
    } else {
        if active_catalog_path.is_none() {
            log::debug!(
                "[models] stale guard: catalog not served (model_catalog_json not set to nexus-composer catalog)"
            );
        }
        json!({"models": []})
    };
    Ok(Json(catalog))
}

// ============================================================================
// Claude API handler with protocol transformation.
// ============================================================================

/// Handles Claude API `/v1/messages` requests.
///
/// Claude has endpoint-specific transformation behavior:
/// - It previously adapted Anthropic to OpenAI Chat Completions for OpenRouter's
///   compatibility endpoint.
/// - OpenRouter now has a Claude Code-compatible endpoint, so this transformation is
///   disabled by default but retained as a fallback.
pub async fn handle_messages(
    State(state): State<ProxyState>,
    request: axum::extract::Request,
) -> Result<axum::response::Response, ProxyError> {
    handle_messages_for_app(state, request, AppType::Claude, "Claude", "claude", None).await
}

pub async fn handle_claude_desktop_messages(
    State(state): State<ProxyState>,
    request: axum::extract::Request,
) -> Result<axum::response::Response, ProxyError> {
    validate_claude_desktop_gateway_auth(&state, request.headers())?;
    handle_messages_for_app(
        state,
        request,
        AppType::ClaudeDesktop,
        "Claude Desktop",
        "claude-desktop",
        Some("/claude-desktop"),
    )
    .await
}

pub async fn handle_claude_desktop_models(
    State(state): State<ProxyState>,
    headers: axum::http::HeaderMap,
) -> Result<Json<Value>, ProxyError> {
    validate_claude_desktop_gateway_auth(&state, &headers)?;
    let providers = state
        .provider_router
        .select_providers("claude-desktop")
        .await
        .map_err(|e| ProxyError::DatabaseError(e.to_string()))?;
    let provider = providers.first().ok_or(ProxyError::NoAvailableProvider)?;
    let response = crate::claude_desktop_config::model_list_response(provider)
        .map_err(|e| ProxyError::ConfigError(e.to_string()))?;
    Ok(Json(response))
}

async fn handle_messages_for_app(
    state: ProxyState,
    request: axum::extract::Request,
    app_type: AppType,
    tag: &'static str,
    app_type_str: &'static str,
    strip_prefix: Option<&'static str>,
) -> Result<axum::response::Response, ProxyError> {
    let (parts, body) = request.into_parts();
    let method = parts.method.clone();
    let uri = parts.uri;
    let headers = parts.headers;
    let extensions = parts.extensions;
    let body_bytes = body
        .collect()
        .await
        .map_err(|e| ProxyError::Internal(format!("Failed to read request body: {e}")))?
        .to_bytes();
    let body: Value = serde_json::from_slice(&body_bytes)
        .map_err(|e| ProxyError::Internal(format!("Failed to parse request body: {e}")))?;

    let mut ctx =
        RequestContext::new(&state, &body, &headers, app_type.clone(), tag, app_type_str).await?;

    let raw_endpoint = uri
        .path_and_query()
        .map(|path_and_query| path_and_query.as_str())
        .unwrap_or(uri.path());
    let endpoint = strip_prefix
        .and_then(|prefix| raw_endpoint.strip_prefix(prefix))
        .unwrap_or(raw_endpoint);
    let record_usage =
        !is_claude_count_tokens_path(endpoint.split_once('?').map_or(endpoint, |(path, _)| path));

    let is_stream = body
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);

    // Forward the request.
    let forwarder = ctx.create_forwarder(&state);
    let mut result = match forwarder
        .forward_with_retry(
            &app_type,
            method,
            endpoint,
            body.clone(),
            headers,
            extensions,
            ctx.get_providers(),
        )
        .await
    {
        Ok(result) => result,
        Err(mut err) => {
            if let Some(provider) = err.provider.take() {
                ctx.provider = provider;
            }
            if record_usage {
                log_forward_error(&state, &ctx, is_stream, &err.error);
            }
            return Err(err.error);
        }
    };

    let connection_guard = result.connection_guard.take();
    ctx.outbound_model = result.outbound_model.take();
    ctx.provider = result.provider;
    let api_format = result
        .claude_api_format
        .as_deref()
        .unwrap_or_else(|| get_claude_api_format(&ctx.provider))
        .to_string();
    let response = result.response;

    // Check whether a relay such as OpenRouter requires protocol transformation.
    let adapter = get_adapter(&app_type);
    let needs_transform = record_usage && adapter.needs_transform(&ctx.provider);

    // Claude-specific protocol transformation.
    if needs_transform {
        return handle_claude_transform(
            response,
            &ctx,
            &state,
            &body,
            is_stream,
            &api_format,
            connection_guard,
        )
        .await;
    }

    if !record_usage {
        return handle_count_tokens_response(
            response,
            &ctx,
            &state,
            &CLAUDE_PARSER_CONFIG,
            connection_guard,
            &api_format,
        )
        .await;
    }

    // Common response processing in passthrough mode.
    process_response(
        response,
        &ctx,
        &state,
        &CLAUDE_PARSER_CONFIG,
        connection_guard,
    )
    .await
}

async fn handle_count_tokens_response(
    response: super::hyper_client::ProxyResponse,
    ctx: &RequestContext,
    state: &ProxyState,
    parser_config: &UsageParserConfig,
    connection_guard: Option<ActiveConnectionGuard>,
    api_format: &str,
) -> Result<axum::response::Response, ProxyError> {
    if api_format != "openai_chat" || !response.status().is_success() {
        return handle_non_streaming(response, ctx, state, parser_config, connection_guard, false)
            .await;
    }

    let body_timeout =
        if ctx.app_config.auto_failover_enabled && ctx.app_config.non_streaming_timeout > 0 {
            std::time::Duration::from_secs(ctx.app_config.non_streaming_timeout as u64)
        } else {
            std::time::Duration::ZERO
        };
    let (mut headers, status, body) = read_decoded_body(response, ctx.tag, body_timeout).await?;
    let body = translate_tokenize_count_response(&body)?;

    strip_hop_by_hop_response_headers(&mut headers);
    strip_entity_headers_for_rebuilt_body(&mut headers);
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    let mut builder = axum::response::Response::builder().status(status);
    for (name, value) in &headers {
        builder = builder.header(name, value);
    }
    builder
        .body(axum::body::Body::from(body))
        .map_err(|error| ProxyError::Internal(format!("Failed to build response: {error}")))
}

fn translate_tokenize_count_response(body: &[u8]) -> Result<Vec<u8>, ProxyError> {
    let value: Value = serde_json::from_slice(body).map_err(|error| {
        ProxyError::ForwardFailed(format!("invalid /v1/tokenize response: {error}"))
    })?;
    let count = value.get("count").and_then(Value::as_u64).ok_or_else(|| {
        ProxyError::ForwardFailed(
            "invalid /v1/tokenize response: count must be a non-negative integer".to_string(),
        )
    })?;
    serde_json::to_vec(&json!({"input_tokens": count}))
        .map_err(|error| ProxyError::Internal(error.to_string()))
}

fn validate_claude_desktop_gateway_auth(
    state: &ProxyState,
    headers: &axum::http::HeaderMap,
) -> Result<(), ProxyError> {
    let expected = crate::claude_desktop_config::get_or_create_gateway_token(state.db.as_ref())
        .map_err(|e| ProxyError::AuthError(e.to_string()))?;
    let Some(value) = headers.get(axum::http::header::AUTHORIZATION) else {
        return Err(ProxyError::AuthError(
            "Claude Desktop gateway is missing the Authorization header".to_string(),
        ));
    };
    let value = value
        .to_str()
        .map_err(|_| ProxyError::AuthError("Invalid Authorization header format".to_string()))?;
    let token = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
        .unwrap_or("")
        .trim();
    if token != expected {
        return Err(ProxyError::AuthError(
            "Invalid Claude Desktop gateway token".to_string(),
        ));
    }
    Ok(())
}

fn chat_protocol_error(error: ProtocolError, anthropic: bool) -> ProxyError {
    let detail = json!({"type": error.code(), "code": error.code(),
        "message": error.to_string(), "param": null});
    let body = if anthropic {
        json!({"type": "error", "error": detail})
    } else {
        json!({"error": detail})
    };
    ProxyError::UpstreamError {
        status: StatusCode::BAD_GATEWAY.as_u16(),
        body: Some(body.to_string()),
    }
}

/// Claude-specific protocol transformation.
///
/// Supports both OpenAI Chat Completions and Responses API formats.
fn should_emit_anthropic_reasoning(body: &Value, normalize_chat: bool) -> bool {
    !normalize_chat
        || matches!(
            body.pointer("/thinking/type").and_then(Value::as_str),
            Some("enabled" | "adaptive")
        ) && body.pointer("/thinking/display").and_then(Value::as_str) != Some("omitted")
}

async fn handle_claude_transform(
    response: super::hyper_client::ProxyResponse,
    ctx: &RequestContext,
    state: &ProxyState,
    original_body: &Value,
    is_stream: bool,
    api_format: &str,
    connection_guard: Option<ActiveConnectionGuard>,
) -> Result<axum::response::Response, ProxyError> {
    let normalize = enabled_for_attempt(&ctx.provider, original_body);
    let normalize_chat = normalize && api_format == "openai_chat";
    let emit_reasoning = should_emit_anthropic_reasoning(original_body, normalize_chat);
    let status = response.status();
    let is_codex_oauth = ctx
        .provider
        .meta
        .as_ref()
        .and_then(|meta| meta.provider_type.as_deref())
        == Some("codex_oauth");
    // Codex OAuth upgrades openai_responses replies to SSE even for stream:false.
    // should_use_claude_transform_streaming normally routes that combination through
    // the streaming transformer. Although this avoids a JSON parse 422, it gives a
    // non-streaming client text/event-stream and violates Anthropic semantics. Override
    // this exact combination by aggregating upstream SSE into Anthropic JSON. Other SSE
    // upstreams and non-Codex OAuth cases retain the existing streaming fallback.
    let aggregate_codex_oauth_responses_sse =
        !is_stream && is_codex_oauth && api_format == "openai_responses";
    let use_streaming = if aggregate_codex_oauth_responses_sse {
        false
    } else {
        should_use_claude_transform_streaming(
            is_stream,
            response.is_sse(),
            api_format,
            is_codex_oauth,
        )
    };
    let tool_schema_hints = transform_gemini::extract_anthropic_tool_schema_hints(original_body);
    let tool_schema_hints = (!tool_schema_hints.is_empty()).then_some(tool_schema_hints);

    if use_streaming {
        // Select the streaming transformer for api_format.
        let stream = normalize_sse_stream(response.bytes_stream(), normalize_chat, emit_reasoning);
        let sse_stream: Box<
            dyn futures::Stream<Item = Result<Bytes, std::io::Error>> + Send + Unpin,
        > = if api_format == "openai_responses" {
            Box::new(Box::pin(create_anthropic_sse_stream_from_responses(stream)))
        } else if api_format == "gemini_native" {
            Box::new(Box::pin(create_anthropic_sse_stream_from_gemini(
                stream,
                Some(state.gemini_shadow.clone()),
                Some(ctx.provider.id.clone()),
                Some(ctx.session_id.clone()),
                tool_schema_hints.clone(),
            )))
        } else if normalize_chat {
            Box::new(Box::pin(create_anthropic_sse_stream_with_mode(
                stream, true,
            )))
        } else {
            Box::new(Box::pin(create_anthropic_sse_stream(stream)))
        };

        // Create a usage collector. Do not parse transformed SSE when usage logging is
        // disabled.
        let usage_collector = if usage_logging_enabled(state) {
            let state = state.clone();
            let provider_id = ctx.provider.id.clone();
            let request_model = ctx.request_model.clone();
            // If upstream or the transformer omits the model, prefer the mapped outbound
            // model, which is the takeover truth, then the client request alias. Treat an
            // empty string as missing because transformers synthesize "" when upstream
            // does not echo a model.
            let fallback_model = ctx
                .outbound_model
                .clone()
                .unwrap_or_else(|| ctx.request_model.clone());
            let status_code = status.as_u16();
            let start_time = ctx.start_time;
            let session_id = ctx.session_id.clone();
            let correlation_id = ctx.correlation_id();
            // Use ctx.app_type because the Claude Desktop gateway also takes this path.
            // Hard-coding "claude" would attribute claude-desktop rows to Claude.
            let app_type_str = ctx.app_type_str;

            Some(SseUsageCollector::new(
                start_time,
                Some(claude_stream_usage_event_filter),
                status_code,
                move |events, first_token_ms, outcome| {
                    let usage = TokenUsage::from_claude_stream_events(&events).unwrap_or_default();
                    let model = usage
                        .model
                        .clone()
                        .filter(|m| !m.is_empty())
                        .unwrap_or_else(|| fallback_model.clone());
                    let latency_ms = start_time.elapsed().as_millis() as u64;
                    let state = state.clone();
                    let provider_id = provider_id.clone();
                    let session_id = session_id.clone();
                    let request_model = request_model.clone();
                    let outbound_model = fallback_model.clone();
                    let correlation_id = correlation_id.clone();

                    tokio::spawn(async move {
                        log_usage(
                            &state,
                            correlation_id,
                            &provider_id,
                            app_type_str,
                            &model,
                            &request_model,
                            &outbound_model,
                            usage,
                            latency_ms,
                            first_token_ms,
                            true,
                            outcome.status_code,
                            Some(session_id),
                            outcome.error_message,
                        )
                        .await;
                    });
                },
            ))
        } else {
            None
        };

        // Read streaming timeout configuration.
        let timeout_config = ctx.streaming_timeout_config();

        let logged_stream = create_logged_passthrough_stream(
            sse_stream,
            "Claude/OpenRouter",
            usage_collector,
            timeout_config,
            connection_guard,
        );

        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "Content-Type",
            axum::http::HeaderValue::from_static("text/event-stream"),
        );
        headers.insert(
            "Cache-Control",
            axum::http::HeaderValue::from_static("no-cache"),
        );

        let body = axum::body::Body::from_stream(logged_stream);
        return Ok((headers, body).into_response());
    }

    // Non-streaming response transformation: OpenAI/Responses to Anthropic.
    let body_timeout =
        if ctx.app_config.auto_failover_enabled && ctx.app_config.non_streaming_timeout > 0 {
            std::time::Duration::from_secs(ctx.app_config.non_streaming_timeout as u64)
        } else {
            std::time::Duration::ZERO
        };
    let (mut response_headers, _status, body_bytes) =
        read_decoded_body(response, ctx.tag, body_timeout).await?;

    let body_str = String::from_utf8_lossy(&body_bytes);

    let upstream_response: Value = if aggregate_codex_oauth_responses_sse {
        responses_sse_to_response_value(&body_str)?
    } else {
        match serde_json::from_slice(&body_bytes) {
            Ok(value) if normalize && api_format == "openai_chat" => {
                normalize_chat_value(value, true)?
            }
            Ok(value) => value,
            // Fallback sniffing (#2234): some gateways force SSE for stream:false while
            // labeling the body application/json, defeating is_sse() header detection.
            // Aggregate that SSE into one JSON value and use the existing non-streaming
            // transformer so the client still receives Anthropic JSON. gemini_native has
            // no aggregator yet and falls through to the diagnostic error.
            Err(_) if body_looks_like_sse(&body_str) && api_format != "gemini_native" => {
                log::warn!(
                    "[Claude] Upstream returned unlabeled SSE for a non-streaming request (api_format={api_format}); using SSE aggregation fallback"
                );
                // If aggregation also fails, keep the complete body in server logs and
                // attach matching diagnostics (content type and body summary) to the
                // client error. Otherwise the sniffing branch would lose the richer
                // diagnostics already present on the regular parse-error path (C7).
                let value = if api_format == "openai_chat" {
                    normalize_chat_fallback(&body_str, &response_headers, normalize, true)
                } else {
                    responses_sse_to_response_value(&body_str).map_err(|error| {
                        aggregate_fallback_error(error, &response_headers, &body_str)
                    })
                }
                .map_err(|error| {
                    log::error!("[Claude] SSE fallback failed: {error}, body: {body_str}");
                    error
                })?;
                value
            }
            Err(e) => {
                log::error!("[Claude] Failed to parse upstream response: {e}, body: {body_str}");
                return Err(upstream_body_parse_error(
                    "Failed to parse upstream response",
                    &e,
                    &response_headers,
                    &body_str,
                ));
            }
        }
    };
    // Select the non-streaming transformer for api_format.
    let anthropic_response = if api_format == "openai_responses" {
        transform_responses::responses_to_anthropic(upstream_response)
    } else if api_format == "gemini_native" {
        transform_gemini::gemini_to_anthropic_with_shadow_and_hints(
            upstream_response,
            Some(state.gemini_shadow.as_ref()),
            Some(&ctx.provider.id),
            Some(&ctx.session_id),
            tool_schema_hints.as_ref(),
        )
    } else {
        transform::openai_to_anthropic_with_thinking(upstream_response, emit_reasoning)
    }
    .map_err(|e| {
        log::error!("[Claude] Failed to transform response: {e}");
        e
    })?;

    // Record usage. Skip all-zero usage, matching the Codex streaming collector. An SSE
    // aggregation fallback has no usage when upstream omits stream_options.include_usage,
    // and persisting it would create a meaningless empty row.
    if let Some(usage) =
        TokenUsage::from_claude_response(&anthropic_response).filter(|u| u.has_billable_tokens())
    {
        // If the transformed response omits or synthesizes an empty model, fall back to
        // the mapped outbound model (takeover truth), then the client request alias.
        let model = anthropic_response
            .get("model")
            .and_then(|m| m.as_str())
            .filter(|m| !m.is_empty())
            .map(str::to_string)
            .or_else(|| ctx.outbound_model.clone())
            .unwrap_or_else(|| ctx.request_model.clone());
        let latency_ms = ctx.latency_ms();

        let request_model = ctx.request_model.clone();
        let outbound_model = ctx
            .outbound_model
            .clone()
            .unwrap_or_else(|| ctx.request_model.clone());
        let app_type_str = ctx.app_type_str;
        tokio::spawn({
            let state = state.clone();
            let provider_id = ctx.provider.id.clone();
            let session_id = ctx.session_id.clone();
            let correlation_id = ctx.correlation_id();
            async move {
                log_usage(
                    &state,
                    correlation_id,
                    &provider_id,
                    app_type_str,
                    &model,
                    &request_model,
                    &outbound_model,
                    usage,
                    latency_ms,
                    None,
                    false,
                    status.as_u16(),
                    Some(session_id),
                    None,
                )
                .await;
            }
        });
    }

    // Build the response.
    let mut builder = axum::response::Response::builder().status(status);
    strip_entity_headers_for_rebuilt_body(&mut response_headers);
    strip_hop_by_hop_response_headers(&mut response_headers);
    // Builder::header appends, so remove upstream Content-Type to avoid duplicates.
    response_headers.remove(axum::http::header::CONTENT_TYPE);

    for (key, value) in response_headers.iter() {
        builder = builder.header(key, value);
    }

    builder = builder.header(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );

    let response_body = serde_json::to_vec(&anthropic_response).map_err(|e| {
        log::error!("[Claude] Failed to serialize response: {e}");
        ProxyError::TransformError(format!("Failed to serialize response: {e}"))
    })?;

    let body = axum::body::Body::from(response_body);
    builder.body(body).map_err(|e| {
        log::error!("[Claude] Failed to build response: {e}");
        ProxyError::Internal(format!("Failed to build response: {e}"))
    })
}

fn endpoint_with_query(uri: &axum::http::Uri, endpoint: &str) -> String {
    match uri.query() {
        Some(query) => format!("{endpoint}?{query}"),
        None => endpoint.to_string(),
    }
}

/// Codex clients, especially an authenticated Desktop, may zstd-compress request
/// bodies, causing a direct `serde_json::from_slice` call to fail. Decompress before
/// parsing and remove stale entity headers (content-encoding, content-length, and
/// transfer-encoding). The forwarding layer rebuilds correct headers from the decoded
/// JSON bytes.
fn decode_codex_request_body(
    headers: &mut axum::http::HeaderMap,
    body_bytes: Bytes,
) -> Result<Bytes, ProxyError> {
    let Some(encoding) = get_content_encoding(headers) else {
        return Ok(body_bytes);
    };

    if !is_supported_content_encoding(&encoding) {
        return Err(ProxyError::InvalidRequest(format!(
            "Unsupported request content-encoding: {encoding}"
        )));
    }

    log::debug!("[Codex] Decompressing request body: content-encoding={encoding}");
    let decompressed = match decompress_body(&encoding, &body_bytes) {
        Ok(Some(decompressed)) => decompressed,
        // is_supported_content_encoding already validates the encoding, so None should
        // not occur. Fail defensively rather than forwarding compressed bytes as JSON.
        Ok(None) => {
            return Err(ProxyError::InvalidRequest(format!(
                "Unsupported request content-encoding: {encoding}"
            )));
        }
        Err(e) => {
            log::warn!("[Codex] Failed to decompress request body ({encoding}): {e}");
            return Err(ProxyError::InvalidRequest(format!(
                "Failed to decompress request body ({encoding}): {e}"
            )));
        }
    };

    headers.remove(axum::http::header::CONTENT_ENCODING);
    headers.remove(axum::http::header::CONTENT_LENGTH);
    headers.remove(axum::http::header::TRANSFER_ENCODING);

    Ok(Bytes::from(decompressed))
}

// ============================================================================
// Codex API handlers.
// ============================================================================

/// Handles Codex CLI OpenAI Chat Completions requests at `/v1/chat/completions`.
pub async fn handle_chat_completions(
    State(state): State<ProxyState>,
    request: axum::extract::Request,
) -> Result<axum::response::Response, ProxyError> {
    let (parts, req_body) = request.into_parts();
    let method = parts.method.clone();
    let uri = parts.uri;
    let mut headers = parts.headers;
    let extensions = parts.extensions;
    let body_bytes = req_body
        .collect()
        .await
        .map_err(|e| ProxyError::Internal(format!("Failed to read request body: {e}")))?
        .to_bytes();
    let body_bytes = decode_codex_request_body(&mut headers, body_bytes)?;
    let body: Value = serde_json::from_slice(&body_bytes)
        .map_err(|e| ProxyError::Internal(format!("Failed to parse request body: {e}")))?;

    let mut ctx =
        RequestContext::new(&state, &body, &headers, AppType::Codex, "Codex", "codex").await?;
    let endpoint = endpoint_with_query(&uri, "/chat/completions");

    let is_stream = body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let forwarder = ctx.create_forwarder(&state);
    let mut result = match forwarder
        .forward_with_retry(
            &AppType::Codex,
            method,
            &endpoint,
            body,
            headers,
            extensions,
            ctx.get_providers(),
        )
        .await
    {
        Ok(result) => result,
        Err(mut err) => {
            if let Some(provider) = err.provider.take() {
                ctx.provider = provider;
            }
            log_forward_error(&state, &ctx, is_stream, &err.error);
            return build_codex_proxy_error_response(&ctx, &endpoint, &err.error);
        }
    };

    let connection_guard = result.connection_guard.take();
    ctx.outbound_model = result.outbound_model.take();
    ctx.provider = result.provider;
    let response = result.response;

    process_response(
        response,
        &ctx,
        &state,
        &OPENAI_PARSER_CONFIG,
        connection_guard,
    )
    .await
}

/// Handles passthrough Codex CLI OpenAI Responses requests at `/v1/responses`.
pub async fn handle_responses(
    State(state): State<ProxyState>,
    request: axum::extract::Request,
) -> Result<axum::response::Response, ProxyError> {
    let (parts, req_body) = request.into_parts();
    let method = parts.method.clone();
    let uri = parts.uri;
    let mut headers = parts.headers;
    let extensions = parts.extensions;
    let body_bytes = req_body
        .collect()
        .await
        .map_err(|e| ProxyError::Internal(format!("Failed to read request body: {e}")))?
        .to_bytes();
    let body_bytes = decode_codex_request_body(&mut headers, body_bytes)?;
    let body: Value = serde_json::from_slice(&body_bytes)
        .map_err(|e| ProxyError::Internal(format!("Failed to parse request body: {e}")))?;

    let mut ctx =
        RequestContext::new(&state, &body, &headers, AppType::Codex, "Codex", "codex").await?;
    let endpoint = endpoint_with_query(&uri, "/responses");

    let is_stream = body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let codex_tool_context = transform_codex_chat::build_codex_tool_context_from_request(&body);
    let request_enables_reasoning =
        body.pointer("/chat_template_kwargs/enable_thinking") == Some(&Value::Bool(true));

    let forwarder = ctx.create_forwarder(&state);
    let continuation_method = method.clone();
    let continuation_endpoint = endpoint.clone();
    let continuation_body = body.clone();
    let continuation_headers = headers.clone();
    let continuation_extensions = extensions.clone();
    let mut result = match forwarder
        .forward_with_retry(
            &AppType::Codex,
            method,
            &endpoint,
            body,
            headers,
            extensions,
            ctx.get_providers(),
        )
        .await
    {
        Ok(result) => result,
        Err(mut err) => {
            if let Some(provider) = err.provider.take() {
                ctx.provider = provider;
            }
            log_forward_error(&state, &ctx, is_stream, &err.error);
            return build_codex_proxy_error_response(&ctx, &endpoint, &err.error);
        }
    };

    let connection_guard = result.connection_guard.take();
    ctx.outbound_model = result.outbound_model.take();
    ctx.provider = result.provider;
    let normalize = enabled_for_request(&ctx.provider, request_enables_reasoning);
    let response = result.response;
    let continuation =
        (!codex_tool_context.chat_tools().is_empty()).then(|| CodexChatContinuationContext {
            forwarder,
            method: continuation_method,
            endpoint: continuation_endpoint,
            body: continuation_body,
            headers: continuation_headers,
            extensions: continuation_extensions,
            provider: ctx.provider.clone(),
            normalize,
        });

    if super::providers::should_convert_codex_responses_to_chat(&ctx.provider, &endpoint) {
        return handle_codex_chat_to_responses_transform(
            response,
            &ctx,
            &state,
            CodexChatTransformOptions {
                is_stream,
                connection_guard,
                tool_context: codex_tool_context,
                normalize,
                continuation,
            },
        )
        .await;
    }

    process_response(
        response,
        &ctx,
        &state,
        &CODEX_PARSER_CONFIG,
        connection_guard,
    )
    .await
}

/// Handles passthrough Codex CLI Responses Compact requests at `/v1/responses/compact`.
pub async fn handle_responses_compact(
    State(state): State<ProxyState>,
    request: axum::extract::Request,
) -> Result<axum::response::Response, ProxyError> {
    let (parts, req_body) = request.into_parts();
    let method = parts.method.clone();
    let uri = parts.uri;
    let mut headers = parts.headers;
    let extensions = parts.extensions;
    let body_bytes = req_body
        .collect()
        .await
        .map_err(|e| ProxyError::Internal(format!("Failed to read request body: {e}")))?
        .to_bytes();
    let body_bytes = decode_codex_request_body(&mut headers, body_bytes)?;
    let body: Value = serde_json::from_slice(&body_bytes)
        .map_err(|e| ProxyError::Internal(format!("Failed to parse request body: {e}")))?;

    let mut ctx =
        RequestContext::new(&state, &body, &headers, AppType::Codex, "Codex", "codex").await?;
    let endpoint = endpoint_with_query(&uri, "/responses/compact");

    let is_stream = body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let codex_tool_context = transform_codex_chat::build_codex_tool_context_from_request(&body);
    let request_enables_reasoning =
        body.pointer("/chat_template_kwargs/enable_thinking") == Some(&Value::Bool(true));

    let forwarder = ctx.create_forwarder(&state);
    let mut result = match forwarder
        .forward_with_retry(
            &AppType::Codex,
            method,
            &endpoint,
            body,
            headers,
            extensions,
            ctx.get_providers(),
        )
        .await
    {
        Ok(result) => result,
        Err(mut err) => {
            if let Some(provider) = err.provider.take() {
                ctx.provider = provider;
            }
            log_forward_error(&state, &ctx, is_stream, &err.error);
            return build_codex_proxy_error_response(&ctx, &endpoint, &err.error);
        }
    };

    let connection_guard = result.connection_guard.take();
    ctx.outbound_model = result.outbound_model.take();
    ctx.provider = result.provider;
    let normalize = enabled_for_request(&ctx.provider, request_enables_reasoning);
    let response = result.response;

    if super::providers::should_convert_codex_responses_to_chat(&ctx.provider, &endpoint) {
        return handle_codex_chat_to_responses_transform(
            response,
            &ctx,
            &state,
            CodexChatTransformOptions {
                is_stream,
                connection_guard,
                tool_context: codex_tool_context,
                normalize,
                continuation: None,
            },
        )
        .await;
    }

    process_response(
        response,
        &ctx,
        &state,
        &CODEX_PARSER_CONFIG,
        connection_guard,
    )
    .await
}

struct CodexChatContinuationContext {
    forwarder: RequestForwarder,
    method: http::Method,
    endpoint: String,
    body: Value,
    headers: axum::http::HeaderMap,
    extensions: Extensions,
    provider: Provider,
    normalize: bool,
}

impl CodexChatContinuationContext {
    async fn request(
        &self,
        progress: &str,
    ) -> Result<super::hyper_client::ProxyResponse, ProxyError> {
        let body =
            transform_codex_chat::append_progress_continuation_request(&self.body, progress)?;
        self.forwarder
            .forward_codex_chat_continuation(
                &self.method,
                &self.provider,
                &self.endpoint,
                &body,
                &self.headers,
                &self.extensions,
            )
            .await
    }

    async fn stream(&self, progress: String) -> Result<BoxChatStream, String> {
        let response = self
            .request(&progress)
            .await
            .map_err(|error| error.to_string())?;
        let stream = normalize_sse_stream(response.bytes_stream(), self.normalize, true);
        Ok(Box::pin(stream))
    }
}

fn progress_only_chat_stop(chat_response: &Value, tools_available: bool) -> Option<String> {
    if !tools_available {
        return None;
    }
    let choice = chat_response
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())?;
    if choice.get("finish_reason").and_then(Value::as_str) != Some("stop") {
        return None;
    }
    let message = choice.get("message")?;
    if message
        .get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|calls| !calls.is_empty())
        || message.get("function_call").is_some()
    {
        return None;
    }
    let text = message.get("content").and_then(Value::as_str)?.trim();
    is_clearly_unfinished_progress(text).then(|| text.to_string())
}

struct CodexChatTransformOptions {
    is_stream: bool,
    connection_guard: Option<ActiveConnectionGuard>,
    tool_context: transform_codex_chat::CodexToolContext,
    normalize: bool,
    continuation: Option<CodexChatContinuationContext>,
}

async fn handle_codex_chat_to_responses_transform(
    response: super::hyper_client::ProxyResponse,
    ctx: &RequestContext,
    state: &ProxyState,
    options: CodexChatTransformOptions,
) -> Result<axum::response::Response, ProxyError> {
    let CodexChatTransformOptions {
        is_stream,
        connection_guard,
        tool_context,
        normalize,
        continuation,
    } = options;
    let status = response.status();

    if !status.is_success() {
        // Upstream Chat error bodies differ from Responses, for example MiniMax
        // base_resp or custom detail fields. Direct passthrough prevents Codex from
        // recognizing the error code. Normalize to the Responses shape
        // `{"error": {message, type, code, param}}` while preserving HTTP status.
        return handle_codex_chat_error_response(response, ctx, status).await;
    }

    if is_stream || response.is_sse() {
        let tools_available = !tool_context.chat_tools().is_empty();
        let initial: BoxChatStream = Box::pin(normalize_sse_stream(
            response.bytes_stream(),
            normalize,
            true,
        ));
        let continuation: Option<ChatContinuation> = continuation.map(|context| {
            let context = Arc::new(context);
            Box::new(move |progress| {
                let context = Arc::clone(&context);
                Box::pin(async move { context.stream(progress).await })
                    as super::providers::streaming_codex_chat::ChatContinuationFuture
            }) as ChatContinuation
        });
        let stream = recover_progress_only_chat_stream(initial, tools_available, continuation);
        let sse_stream =
            create_responses_sse_stream_from_chat_with_mode(stream, tool_context, normalize);
        let sse_stream = record_responses_sse_stream(sse_stream, state.codex_chat_history.clone());

        let usage_collector = if usage_logging_enabled(state) {
            let state = state.clone();
            let provider_id = ctx.provider.id.clone();
            let request_model = ctx.request_model.clone();
            // For takeover or model overrides, attribute to the outbound truth before
            // the client request alias.
            let fallback_model = ctx
                .outbound_model
                .clone()
                .unwrap_or_else(|| ctx.request_model.clone());
            let app_type_str = ctx.app_type_str;
            let start_time = ctx.start_time;
            let session_id = ctx.session_id.clone();
            let correlation_id = ctx.correlation_id();

            Some(SseUsageCollector::new(
                start_time,
                Some(codex_stream_usage_event_filter),
                status.as_u16(),
                move |events, first_token_ms, outcome| {
                    let usage =
                        TokenUsage::from_codex_stream_events_auto(&events).unwrap_or_default();
                    let model = usage
                        .model
                        .clone()
                        .filter(|m| !m.is_empty())
                        .unwrap_or_else(|| fallback_model.clone());
                    let latency_ms = start_time.elapsed().as_millis() as u64;

                    let state = state.clone();
                    let provider_id = provider_id.clone();
                    let request_model = request_model.clone();
                    let outbound_model = fallback_model.clone();
                    let session_id = session_id.clone();
                    let correlation_id = correlation_id.clone();

                    tokio::spawn(async move {
                        log_usage(
                            &state,
                            correlation_id,
                            &provider_id,
                            app_type_str,
                            &model,
                            &request_model,
                            &outbound_model,
                            usage,
                            latency_ms,
                            first_token_ms,
                            true,
                            outcome.status_code,
                            Some(session_id),
                            outcome.error_message,
                        )
                        .await;
                    });
                },
            ))
        } else {
            None
        };

        let logged_stream = create_logged_passthrough_stream(
            sse_stream,
            ctx.tag,
            usage_collector,
            ctx.streaming_timeout_config(),
            connection_guard,
        );

        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "Content-Type",
            axum::http::HeaderValue::from_static("text/event-stream"),
        );
        headers.insert(
            "Cache-Control",
            axum::http::HeaderValue::from_static("no-cache"),
        );

        let body = axum::body::Body::from_stream(logged_stream);
        return Ok((headers, body).into_response());
    }

    let _connection_guard = connection_guard;
    let body_timeout =
        if ctx.app_config.auto_failover_enabled && ctx.app_config.non_streaming_timeout > 0 {
            std::time::Duration::from_secs(ctx.app_config.non_streaming_timeout as u64)
        } else {
            std::time::Duration::ZERO
        };
    let (mut response_headers, status, body_bytes) =
        read_decoded_body(response, ctx.tag, body_timeout).await?;
    let body_str = String::from_utf8_lossy(&body_bytes);
    let chat_response: Value = match serde_json::from_slice(&body_bytes) {
        Ok(value) if normalize => normalize_chat_value(value, false)?,
        Ok(value) => value,
        // Fallback sniffing symmetric with Claude's handle_claude_transform (#2234):
        // aggregate an unlabeled SSE body returned for stream:false.
        Err(_) if body_looks_like_sse(&body_str) => {
            log::warn!("[Codex] Upstream returned unlabeled SSE for a non-streaming request; using Chat SSE aggregation fallback");
            // If aggregation also fails, keep the complete body in server logs and
            // attach diagnostics to the client error (C7).
            let value = normalize_chat_fallback(&body_str, &response_headers, normalize, false)
                .map_err(|error| {
                    log::error!(
                        "[Codex] SSE fallback aggregation failed: {error}, body: {body_str}"
                    );
                    error
                })?;
            value
        }
        Err(e) => {
            log::error!("[Codex] Failed to parse upstream Chat response: {e}, body: {body_str}");
            return Err(upstream_body_parse_error(
                "Failed to parse upstream chat response",
                &e,
                &response_headers,
                &body_str,
            ));
        }
    };
    let tools_available = !tool_context.chat_tools().is_empty();
    let initial_progress = progress_only_chat_stop(&chat_response, tools_available);
    let mut responses_response = transform_codex_chat::chat_completion_to_response_with_context(
        chat_response,
        &tool_context,
    )
    .map_err(|e| {
        log::error!("[Codex] Failed to transform Chat response to Responses: {e}");
        e
    })?;
    if let (Some(context), Some(mut progress)) = (continuation.as_ref(), initial_progress) {
        for attempt in 0..MAX_PROGRESS_CONTINUATIONS {
            log::warn!(
                "[Codex] Non-streaming Chat upstream stopped after an unfinished progress update; \
                 continuation attempt {}",
                attempt + 1
            );
            let next_response = context.request(&progress).await?;
            let next_headers = next_response.headers().clone();
            let next_bytes = next_response.bytes().await?;
            let next_body = String::from_utf8_lossy(&next_bytes);
            let next_chat: Value = match serde_json::from_slice(&next_bytes) {
                Ok(value) if normalize => normalize_chat_value(value, false)?,
                Ok(value) => value,
                Err(_) if body_looks_like_sse(&next_body) => {
                    normalize_chat_fallback(&next_body, &next_headers, normalize, false)?
                }
                Err(error) => {
                    return Err(upstream_body_parse_error(
                        "Failed to parse upstream chat continuation",
                        &error,
                        &next_headers,
                        &next_body,
                    ))
                }
            };
            let next_progress = progress_only_chat_stop(&next_chat, tools_available);
            let next_responses = transform_codex_chat::chat_completion_to_response_with_context(
                next_chat,
                &tool_context,
            )?;
            transform_codex_chat::merge_continuation_response(
                &mut responses_response,
                next_responses,
            );
            let Some(next_progress) = next_progress else {
                break;
            };
            if attempt + 1 == MAX_PROGRESS_CONTINUATIONS {
                return Err(ProxyError::TransformError(
                    "Chat upstream repeatedly stopped after unfinished progress updates"
                        .to_string(),
                ));
            }
            progress = next_progress;
        }
    }
    state
        .codex_chat_history
        .record_response(&responses_response)
        .await;

    // When a non-streaming upstream Chat response omits usage,
    // chat_usage_to_responses_usage synthesizes all-zero usage
    // (transform_codex_chat.rs:1581), and from_codex_response returns Some whenever
    // input/output fields exist, even when zero. Gate with has_billable_tokens to avoid
    // empty rows inflating request counts, matching the streaming branch and Claude
    // transform handler.
    if let Some(usage) = TokenUsage::from_codex_response_auto(&responses_response)
        .filter(TokenUsage::has_billable_tokens)
    {
        let model = responses_response
            .get("model")
            .and_then(|m| m.as_str())
            .filter(|m| !m.is_empty())
            .map(str::to_string)
            .or_else(|| ctx.outbound_model.clone())
            .unwrap_or_else(|| ctx.request_model.clone());
        let request_model = ctx.request_model.clone();
        let outbound_model = ctx
            .outbound_model
            .clone()
            .unwrap_or_else(|| ctx.request_model.clone());
        let app_type_str = ctx.app_type_str;
        tokio::spawn({
            let state = state.clone();
            let provider_id = ctx.provider.id.clone();
            let session_id = ctx.session_id.clone();
            let latency_ms = ctx.latency_ms();
            let correlation_id = ctx.correlation_id();
            async move {
                log_usage(
                    &state,
                    correlation_id,
                    &provider_id,
                    app_type_str,
                    &model,
                    &request_model,
                    &outbound_model,
                    usage,
                    latency_ms,
                    None,
                    false,
                    status.as_u16(),
                    Some(session_id),
                    None,
                )
                .await;
            }
        });
    }

    strip_entity_headers_for_rebuilt_body(&mut response_headers);
    strip_hop_by_hop_response_headers(&mut response_headers);
    // Builder::header appends, so remove upstream Content-Type to avoid duplicates.
    response_headers.remove(axum::http::header::CONTENT_TYPE);

    let mut builder = axum::response::Response::builder().status(status);
    for (key, value) in response_headers.iter() {
        builder = builder.header(key, value);
    }
    builder = builder.header(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );

    let response_body = serde_json::to_vec(&responses_response).map_err(|e| {
        log::error!("[Codex] Failed to serialize Responses response: {e}");
        ProxyError::TransformError(format!("Failed to serialize responses response: {e}"))
    })?;

    builder
        .body(axum::body::Body::from(response_body))
        .map_err(|e| {
            log::error!("[Codex] Failed to build Responses response: {e}");
            ProxyError::Internal(format!("Failed to build response: {e}"))
        })
}

/// Transforms an upstream Chat Completions error into the Responses API error shape.
///
/// The success branch already rewrites responses to Responses format. Leaving a Chat
/// error body such as MiniMax's `{"base_resp": {"status_code": 2013}}` would prevent
/// Codex from matching fields. Read the upstream body, normalize it to
/// `{"error": {message, type, code, param}}`, and preserve the HTTP status.
async fn handle_codex_chat_error_response(
    response: super::hyper_client::ProxyResponse,
    ctx: &RequestContext,
    status: axum::http::StatusCode,
) -> Result<axum::response::Response, ProxyError> {
    let body_timeout =
        if ctx.app_config.auto_failover_enabled && ctx.app_config.non_streaming_timeout > 0 {
            std::time::Duration::from_secs(ctx.app_config.non_streaming_timeout as u64)
        } else {
            std::time::Duration::ZERO
        };
    let (mut response_headers, _status, body_bytes) =
        read_decoded_body(response, ctx.tag, body_timeout).await?;

    // Preserve non-JSON upstream errors such as Cloudflare HTML or plain-text
    // Unauthorized as Value::String. Dropping them to None would hide diagnostics.
    let parsed_value: Value = match serde_json::from_slice::<Value>(&body_bytes) {
        Ok(value) => value,
        Err(_) => {
            const MAX_RAW_ERROR_BYTES: usize = 1024;
            let lossy = String::from_utf8_lossy(&body_bytes);
            let truncated = if lossy.len() > MAX_RAW_ERROR_BYTES {
                let mut end = MAX_RAW_ERROR_BYTES;
                while end > 0 && !lossy.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{}…(truncated)", &lossy[..end])
            } else {
                lossy.into_owned()
            };
            log::warn!(
                "[Codex] Chat error response is not valid JSON; preserving as text: {truncated}"
            );
            Value::String(truncated)
        }
    };

    let responses_error = transform_codex_chat::chat_error_to_response_error(Some(&parsed_value));

    strip_entity_headers_for_rebuilt_body(&mut response_headers);
    strip_hop_by_hop_response_headers(&mut response_headers);
    // Builder::header appends, so remove upstream Content-Type to avoid duplicates.
    response_headers.remove(axum::http::header::CONTENT_TYPE);

    let mut builder = axum::response::Response::builder().status(status);
    for (key, value) in response_headers.iter() {
        builder = builder.header(key, value);
    }
    builder = builder.header(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );

    let body = serde_json::to_vec(&responses_error).map_err(|e| {
        log::error!("[Codex] Failed to serialize Responses error body: {e}");
        ProxyError::TransformError(format!("Failed to serialize responses error: {e}"))
    })?;

    builder.body(axum::body::Body::from(body)).map_err(|e| {
        log::error!("[Codex] Failed to build Responses error response: {e}");
        ProxyError::Internal(format!("Failed to build response: {e}"))
    })
}

/// Builds an enriched Codex error response for a forwarding-layer failure.
///
/// Unlike `handle_codex_chat_error_response`, which copies a real upstream error and
/// its headers, this path has no upstream response and emits only an `application/json`
/// body. `map_proxy_error_to_status` supplies a status aligned with
/// `ProxyError::into_response`.
///
/// `endpoint_with_query` may include a query such as `?beta=true`, which is echoed in
/// the error body. Current Codex endpoints carry no credentials in the query, so this
/// is safe. Redact before reuse with credential-bearing endpoints such as Gemini
/// `?key=`.
fn build_codex_proxy_error_response(
    ctx: &RequestContext,
    endpoint: &str,
    error: &ProxyError,
) -> Result<axum::response::Response, ProxyError> {
    let status = axum::http::StatusCode::from_u16(map_proxy_error_to_status(error))
        .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    let body = codex_proxy_error_json(&ctx.provider.name, &ctx.request_model, endpoint, error);
    let body = serde_json::to_vec(&body).map_err(|e| {
        log::error!("[Codex] Failed to serialize proxy error body: {e}");
        ProxyError::Internal(format!("Failed to serialize proxy error: {e}"))
    })?;

    axum::response::Response::builder()
        .status(status)
        .header(
            axum::http::header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        )
        .body(axum::body::Body::from(body))
        .map_err(|e| {
            log::error!("[Codex] Failed to build proxy error response: {e}");
            ProxyError::Internal(format!("Failed to build proxy error response: {e}"))
        })
}

fn codex_proxy_error_json(
    provider_name: &str,
    request_model: &str,
    endpoint: &str,
    error: &ProxyError,
) -> Value {
    let (mut body, upstream_status) = match error {
        ProxyError::UpstreamError { status, body } => {
            let parsed_body = body
                .as_deref()
                .map(|body| serde_json::from_str::<Value>(body).unwrap_or_else(|_| json!(body)));
            (
                transform_codex_chat::chat_error_to_response_error(parsed_body.as_ref()),
                Some(*status),
            )
        }
        _ => (
            json!({
                "error": {
                    "message": get_error_message(error),
                    "type": "proxy_error",
                    "code": codex_proxy_error_code(error),
                    "param": Value::Null,
                }
            }),
            None,
        ),
    };

    let Some(error_obj) = body
        .get_mut("error")
        .and_then(|value| value.as_object_mut())
    else {
        return body;
    };

    let message = if upstream_status == Some(413) {
        // A 413 comes from the upstream gateway, typically nginx client_max_body_size,
        // not the local proxy, whose DefaultBodyLimit is 200 MB. Upstream often returns
        // an unhelpful nginx HTML page. Replace it with actionable upstream guidance to
        // avoid incorrectly blaming Nexus Composer or the local proxy.
        format!(
            concat!(
                "Upstream provider rejected the request with HTTP 413 (Payload Too Large). ",
                "The request body exceeds the upstream gateway's size limit; this is the ",
                "provider's server-side limit, not a Nexus Composer limit. ",
                "Provider: {provider}; model: {model}; endpoint: {endpoint}. ",
                "To recover, shrink the request: run /compact, remove large pasted logs or ",
                "inline images, or ask the provider to raise its request body limit ",
                "(e.g. nginx client_max_body_size)."
            ),
            provider = provider_name,
            model = request_model,
            endpoint = endpoint,
        )
    } else {
        let cause = error_obj
            .get("message")
            .and_then(|value| value.as_str())
            .map(ToString::to_string)
            .filter(|message| !message.trim().is_empty())
            .unwrap_or_else(|| get_error_message(error));
        let status_fragment = upstream_status
            .map(|status| format!("; upstream_status: HTTP {status}"))
            .unwrap_or_default();
        format!(
            "Nexus Composer local proxy failed while handling Codex endpoint {endpoint}. Provider: {provider_name}; model: {request_model}{status_fragment}; cause: {cause}"
        )
    };

    error_obj.insert(
        "message".to_string(),
        Value::String(compact_error_message(&message, 1800)),
    );

    if error_obj
        .get("type")
        .and_then(|value| value.as_str())
        .map(|value| value.trim().is_empty())
        .unwrap_or(true)
    {
        error_obj.insert("type".to_string(), Value::String("proxy_error".to_string()));
    }

    if error_obj.get("code").map(Value::is_null).unwrap_or(true) {
        error_obj.insert(
            "code".to_string(),
            Value::String(codex_proxy_error_code(error).to_string()),
        );
    }

    if !error_obj.contains_key("param") {
        error_obj.insert("param".to_string(), Value::Null);
    }

    error_obj.insert(
        "provider".to_string(),
        Value::String(provider_name.to_string()),
    );
    error_obj.insert(
        "model".to_string(),
        Value::String(request_model.to_string()),
    );
    // Codex local routing only. Do not reuse for endpoints whose query may contain
    // credentials.
    error_obj.insert("endpoint".to_string(), Value::String(endpoint.to_string()));
    if let Some(status) = upstream_status {
        error_obj.insert(
            "upstream_status".to_string(),
            Value::Number(serde_json::Number::from(status)),
        );
    }

    body
}

fn codex_proxy_error_code(error: &ProxyError) -> &'static str {
    match error {
        ProxyError::ForwardFailed(_) => "cc_switch_forward_failed",
        ProxyError::Timeout(_) | ProxyError::StreamIdleTimeout(_) => "cc_switch_timeout",
        ProxyError::NoAvailableProvider => "cc_switch_no_available_provider",
        ProxyError::AllProvidersCircuitOpen => "cc_switch_all_providers_circuit_open",
        ProxyError::NoProvidersConfigured => "cc_switch_no_providers_configured",
        ProxyError::MaxRetriesExceeded => "cc_switch_max_retries_exceeded",
        ProxyError::ProviderUnhealthy(_) => "cc_switch_provider_unhealthy",
        ProxyError::ConfigError(_) => "cc_switch_config_error",
        ProxyError::TransformError(_) => "cc_switch_transform_error",
        ProxyError::InvalidRequest(_) => "cc_switch_invalid_request",
        ProxyError::AuthError(_) => "cc_switch_auth_error",
        ProxyError::UpstreamError { .. } => "cc_switch_upstream_error",
        ProxyError::DatabaseError(_) => "cc_switch_database_error",
        ProxyError::Internal(_) => "cc_switch_internal_error",
        ProxyError::AlreadyRunning
        | ProxyError::NotRunning
        | ProxyError::BindFailed(_)
        | ProxyError::StopTimeout
        | ProxyError::StopFailed(_) => "cc_switch_proxy_error",
    }
}

fn compact_error_message(message: &str, max_chars: usize) -> String {
    let normalized = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }

    let truncated = normalized
        .chars()
        .take(max_chars)
        .collect::<String>()
        .trim_end()
        .to_string();
    format!("{truncated}…(truncated)")
}

// ============================================================================
// Gemini API handler.
// ============================================================================

/// Handles passthrough Gemini API requests, including query parameters.
pub async fn handle_gemini(
    State(state): State<ProxyState>,
    uri: axum::http::Uri,
    request: axum::extract::Request,
) -> Result<axum::response::Response, ProxyError> {
    let (parts, req_body) = request.into_parts();
    let method = parts.method.clone();
    let headers = parts.headers;
    let extensions = parts.extensions;
    let body_bytes = req_body
        .collect()
        .await
        .map_err(|e| ProxyError::Internal(format!("Failed to read request body: {e}")))?
        .to_bytes();
    // Read-only GET endpoints such as /v1beta/models and /v1beta/models/<model> have no
    // body. Do not force JSON parsing or an empty body will be rejected.
    let body: Value = if body_bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body_bytes)
            .map_err(|e| ProxyError::Internal(format!("Failed to parse request body: {e}")))?
    };

    // Gemini carries the model name in the URI.
    let mut ctx = RequestContext::new(&state, &body, &headers, AppType::Gemini, "Gemini", "gemini")
        .await?
        .with_model_from_uri(&uri);

    // Extract the full path and query string.
    let endpoint = uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or(uri.path());

    let is_stream = body
        .get("stream")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let forwarder = ctx.create_forwarder(&state);
    let mut result = match forwarder
        .forward_with_retry(
            &AppType::Gemini,
            method,
            endpoint,
            body,
            headers,
            extensions,
            ctx.get_providers(),
        )
        .await
    {
        Ok(result) => result,
        Err(mut err) => {
            if let Some(provider) = err.provider.take() {
                ctx.provider = provider;
            }
            log_forward_error(&state, &ctx, is_stream, &err.error);
            return Err(err.error);
        }
    };

    let connection_guard = result.connection_guard.take();
    ctx.outbound_model = result.outbound_model.take();
    ctx.provider = result.provider;
    let response = result.response;

    process_response(
        response,
        &ctx,
        &state,
        &GEMINI_PARSER_CONFIG,
        connection_guard,
    )
    .await
}

fn should_use_claude_transform_streaming(
    requested_streaming: bool,
    upstream_is_sse: bool,
    api_format: &str,
    is_codex_oauth: bool,
) -> bool {
    requested_streaming || upstream_is_sse || (is_codex_oauth && api_format == "openai_responses")
}

/// Aggregates an OpenAI Responses SSE stream into one complete Responses JSON object
/// for downstream Anthropic non-streaming transformation. Used only when Codex OAuth
/// forces `stream:false` to SSE.
///
/// Reuses `proxy::sse` helpers: `take_sse_block` supports both `\n\n` and
/// `\r\n\r\n`; `strip_sse_field` accepts fields with or without a following space.
fn responses_sse_to_response_value(body: &str) -> Result<Value, ProxyError> {
    let mut buffer = body.trim_start_matches('\u{feff}').to_string();
    let mut completed_response: Option<Value> = None;
    let mut output_items = Vec::new();

    // strict=false handles a residual tail block: ignore truncated JSON instead of
    // invalidating an already complete aggregate. The Codex OAuth path also uses this.
    let mut process_block = |block: &str, strict: bool| -> Result<(), ProxyError> {
        // Once completed is available, skip a residual tail block entirely. Processing
        // a complete response.failed or unrelated event after completion would turn a
        // successful Codex OAuth aggregate into 422 (C8).
        if !strict && completed_response.is_some() {
            return Ok(());
        }
        let mut event_name = "";
        let mut data_lines: Vec<&str> = Vec::new();

        for line in block.lines() {
            let line = line.trim_start();
            if let Some(evt) = strip_sse_field(line, "event") {
                event_name = evt.trim();
            } else if let Some(d) = strip_sse_field(line, "data") {
                data_lines.push(d);
            }
        }

        if data_lines.is_empty() {
            return Ok(());
        }

        let data_str = data_lines.join("\n");
        if data_str.trim() == "[DONE]" {
            return Ok(());
        }

        let data: Value = match serde_json::from_str(&data_str) {
            Ok(v) => v,
            Err(_) if !strict => return Ok(()),
            Err(e) => {
                return Err(ProxyError::TransformError(format!(
                    "Failed to parse upstream SSE event: {e}"
                )))
            }
        };

        match event_name {
            "response.output_item.done" => {
                if let Some(item) = data.get("item") {
                    output_items.push(item.clone());
                }
            }
            "response.completed" => {
                completed_response = Some(data.get("response").cloned().unwrap_or(data));
            }
            "response.failed" => {
                let message = data
                    .pointer("/response/error/message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("response.failed event received");
                return Err(ProxyError::TransformError(message.to_string()));
            }
            _ => {}
        }
        Ok(())
    };

    while let Some(block) = take_sse_block(&mut buffer) {
        process_block(&block, true)?;
    }
    // A mislabeled or nonconforming SSE stream may omit the blank line after its final
    // event. Process the residual buffer as the last block or response.completed would
    // be lost. The closure owns the post-completion skip check (C8).
    process_block(&buffer, false)?;

    let mut response = completed_response.ok_or_else(|| {
        ProxyError::TransformError("No response.completed event in upstream SSE".to_string())
    })?;

    if !output_items.is_empty() {
        if let Some(obj) = response.as_object_mut() {
            obj.insert("output".to_string(), Value::Array(output_items));
        } else {
            return Err(ProxyError::TransformError(
                "response.completed payload is not an object".to_string(),
            ));
        }
    }

    Ok(response)
}

/// Detects whether a response body looks like SSE text (#2234 fallback sniffing).
///
/// Called only after JSON parsing fails. Valid JSON cannot begin with these prefixes,
/// so false positives are avoided. Covers all four SSE field lines plus `:` because
/// gateways such as OpenRouter may emit a `: PROCESSING` comment first.
fn body_looks_like_sse(body: &str) -> bool {
    let trimmed = body.trim_start_matches('\u{feff}').trim_start();
    ["data:", "event:", "id:", "retry:", ":"]
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
}

/// Builds an upstream parse error with content-type, content-encoding, and a body-prefix
/// summary. The client can distinguish mislabeled SSE (`data:`), an HTML interception
/// page (`<`), or undecompressed binary data (replacement characters) without needing
/// server logs.
fn upstream_body_parse_error(
    prefix: &str,
    err: &serde_json::Error,
    headers: &axum::http::HeaderMap,
    body: &str,
) -> ProxyError {
    ProxyError::TransformError(format!(
        "{prefix}: {err} {}",
        body_diagnostics_suffix(headers, body)
    ))
}

/// Adds the same content-type, content-encoding, and body diagnostics when SSE
/// aggregation fallback fails. Clients on the #2234 sniffing path receive root-cause
/// evidence instead of a bare message such as "No chat completion choices in upstream
/// SSE" without headers or body context.
fn aggregate_fallback_error(
    err: ProxyError,
    headers: &axum::http::HeaderMap,
    body: &str,
) -> ProxyError {
    let base = match &err {
        ProxyError::TransformError(m) => m.clone(),
        other => other.to_string(),
    };
    ProxyError::TransformError(format!("{base} {}", body_diagnostics_suffix(headers, body)))
}

fn normalize_chat_fallback(
    body: &str,
    headers: &axum::http::HeaderMap,
    normalize: bool,
    anthropic: bool,
) -> Result<Value, ProxyError> {
    let value = chat_sse_to_response_value(body)
        .map_err(|error| aggregate_fallback_error(error, headers, body))?;
    if normalize {
        let mut value = value;
        normalize_sse_response(body, &mut value)
            .map_err(|error| chat_protocol_error(error, anthropic))?;
        Ok(value)
    } else {
        Ok(value)
    }
}

fn normalize_chat_value(mut value: Value, anthropic: bool) -> Result<Value, ProxyError> {
    normalize_first_choice(&mut value).map_err(|error| chat_protocol_error(error, anthropic))?;
    Ok(value)
}

/// Diagnostic suffix containing content-type, content-encoding, and the first 120 body characters.
fn body_diagnostics_suffix(headers: &axum::http::HeaderMap, body: &str) -> String {
    let header_str = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("<none>")
    };
    format!(
        "(content-type: {}; content-encoding: {}; body[..120]: '{}')",
        header_str("content-type"),
        header_str("content-encoding"),
        body_snippet(body, 120),
    )
}

/// Returns a one-line summary of the first `max_chars` body characters: discard `\r`,
/// render `\n` literally, replace other control characters with the replacement
/// character, and append an ellipsis when truncated.
fn body_snippet(body: &str, max_chars: usize) -> String {
    let mut snippet = String::new();
    for c in body.chars().take(max_chars) {
        match c {
            '\n' => snippet.push_str("\\n"),
            '\r' => {}
            c if c.is_control() => snippet.push('\u{FFFD}'),
            c => snippet.push(c),
        }
    }
    if body.chars().nth(max_chars).is_some() {
        snippet.push('…');
    }
    snippet
}

/// Parses the event name and data payload from one SSE block, joining multiline data
/// with `\n`. Allow leading whitespace before field names to match body_looks_like_sse's
/// trimming tolerance; otherwise sniffing would accept an indented `  data:` line that
/// parsing silently drops (C4). Returns None when no data line exists.
pub(crate) fn sse_block_parts(block: &str, strip_bom: bool) -> Option<(String, String)> {
    let mut event_name = String::new();
    let mut data_lines: Vec<&str> = Vec::new();
    for line in block.lines() {
        let line = line.trim_start();
        let line = if strip_bom {
            line.strip_prefix('\u{feff}').unwrap_or(line).trim_start()
        } else {
            line
        };
        if let Some(evt) = strip_sse_field(line, "event") {
            event_name = evt.trim().to_string();
        } else if let Some(d) = strip_sse_field(line, "data") {
            data_lines.push(d);
        }
    }
    (!data_lines.is_empty()).then(|| (event_name, data_lines.join("\n")))
}

/// Aggregates Chat Completions SSE into one chat.completion JSON value (#2234 fallback).
///
/// Used by non-streaming branches when upstream returns SSE for stream:false without a
/// text/event-stream Content-Type, defeating is_sse. Feed the aggregate to existing
/// non-streaming transformers (`openai_to_anthropic` for Claude and
/// `chat_completion_to_response_with_context` for Codex), preserving JSON semantics.
/// Incremental merge behavior matches providers/streaming.rs: locate tool calls by
/// delta.index, replace id/name when present, append argument fragments, combine all
/// reasoning aliases through codex_chat_common, and lock the first non-null
/// finish_reason. Kimi-k2.6 may send another finish_reason after tool_use.
fn chat_sse_to_response_value(body: &str) -> Result<Value, ProxyError> {
    chat_sse_to_response_value_with_mode(body, false)
}

pub(crate) fn chat_sse_to_response_value_for_normalization(
    body: &str,
) -> Result<Value, ProxyError> {
    chat_sse_to_response_value_with_mode(body, true)
}

fn chat_sse_to_response_value_with_mode(body: &str, normalize: bool) -> Result<Value, ProxyError> {
    // Strip the BOM. Sniffing accepts it, but strip_sse_field matches at line start and
    // would otherwise silently lose the first data line.
    let mut buffer = body.trim_start_matches('\u{feff}').to_string();

    let mut id = Value::Null;
    let mut created = Value::Null;
    let mut model = Value::Null;
    let mut content = String::new();
    let mut reasoning_content = String::new();
    // Aggregate tool_calls by index in a BTreeMap. An upstream-controlled u64 index
    // must not densify an array; the old `while len() <= index { push }` could OOM on
    // index=4e9 (C1). BTreeMap avoids unbounded allocation and preserves index order.
    let mut tool_calls: std::collections::BTreeMap<usize, Value> =
        std::collections::BTreeMap::new();
    let mut legacy_calls = std::collections::BTreeMap::new();
    let mut finish_reason = Value::Null;
    let mut usage = Value::Null;
    let mut saw_choice = false;
    let mut saw_done = false;

    // strict=false ignores truncated JSON in a residual tail, symmetric with
    // responses_sse_to_response_value (C2). A severed tail must not turn an already
    // complete aggregate into 422.
    let mut process_event =
        |event_name: &str, data_str: &str, strict: bool| -> Result<(), ProxyError> {
            let trimmed = data_str.trim();
            if trimmed == "[DONE]" {
                saw_done = true;
                return Ok(());
            }
            if trimmed.is_empty() {
                return Ok(());
            }
            let chunk: Value = match serde_json::from_str(data_str) {
                Ok(v) => v,
                Err(_) if !strict => return Ok(()),
                Err(e) => {
                    return Err(ProxyError::TransformError(format!(
                        "Failed to parse upstream SSE chunk: {e}"
                    )))
                }
            };

            // `event: error` marks failure by event name and its data may be the error
            // object without an error key. Fail even after a complete choice or gateway
            // quota/rate-limit errors could be disguised as success (C18).
            if event_name.eq_ignore_ascii_case("error") {
                let message = chunk
                    .get("error")
                    .and_then(error_event_message)
                    .or_else(|| error_event_message(&chunk))
                    .unwrap_or_else(|| "upstream error event in SSE stream".to_string());
                return Err(ProxyError::TransformError(message));
            }
            // A gateway may send an error in an ordinary data chunk. Fail only when the
            // error has a reportable message. Empty objects/messages, null, or false are
            // placeholders emitted on every chunk by some compatible gateways and must
            // not invalidate a successful stream (C12).
            if let Some(message) = chunk
                .get("error")
                .filter(|e| !e.is_null())
                .and_then(error_event_message)
            {
                return Err(ProxyError::TransformError(message));
            }

            // The first meaningful value locks each envelope field. Azure content-filter
            // prelude chunks carry ""/0 placeholders, which must not freeze fields.
            for (slot, key) in [
                (&mut id, "id"),
                (&mut created, "created"),
                (&mut model, "model"),
            ] {
                if slot.is_null() {
                    if let Some(v) = chunk.get(key).filter(|v| envelope_value_meaningful(v)) {
                        *slot = v.clone();
                    }
                }
            }
            // OpenAI emits non-null usage only on the final chunk.
            if let Some(u) = chunk.get("usage").filter(|u| !u.is_null()) {
                usage = u.clone();
            }

            // Fallback mode selects index 0; strict normalization preserves first-choice behavior.
            let Some(choice) = chunk
                .get("choices")
                .and_then(|c| c.as_array())
                .and_then(|arr| {
                    if normalize {
                        arr.first()
                    } else {
                        arr.iter()
                            .find(|ch| ch.get("index").and_then(|i| i.as_u64()).unwrap_or(0) == 0)
                    }
                })
            else {
                return Ok(());
            };

            // Only a choice payload proves that a response was observed. Treating a
            // metadata/usage-only stream plus [DONE] as evidence would bypass completion
            // guards and fabricate an empty success.
            saw_choice = true;

            // Lock the first non-null finish_reason, matching streaming.rs first-wins.
            // A trailing "stop" must not override an earlier "tool_calls".
            if finish_reason.is_null() {
                if let Some(fr) = choice.get("finish_reason").filter(|v| !v.is_null()) {
                    finish_reason = fr.clone();
                }
            }
            // Normal streaming uses delta, but fake-streaming relays may wrap a complete
            // chat.completion as one message event, sometimes with an empty delta. When
            // delta is empty and message exists, use the message snapshot and replace
            // prior deltas to avoid double counting. Otherwise content could be silently
            // dropped while finish_reason bypasses completion checks (C3).
            let (payload, is_full_message) = if normalize {
                let payload = choice
                    .get("delta")
                    .filter(|value| value.is_object())
                    .or_else(|| choice.get("message").filter(|value| value.is_object()));
                let Some(payload) = payload else {
                    return Ok(());
                };
                (payload, false)
            } else {
                let delta_nonempty = choice
                    .get("delta")
                    .and_then(|d| d.as_object())
                    .is_some_and(|o| !o.is_empty());
                if delta_nonempty {
                    (choice.get("delta").unwrap(), false)
                } else if let Some(message) = choice.get("message") {
                    (message, true)
                } else if let Some(delta) = choice.get("delta") {
                    // An empty delta without a message is a normal finish_reason-only chunk.
                    (delta, false)
                } else {
                    return Ok(());
                }
            };
            if is_full_message {
                content.clear();
                reasoning_content.clear();
                tool_calls.clear();
            }
            match payload.get("content") {
                Some(Value::String(text)) => content.push_str(text),
                Some(Value::Array(parts)) if !normalize => {
                    for part in parts {
                        if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                            content.push_str(text);
                        } else if let Some(refusal) = part.get("refusal").and_then(|r| r.as_str()) {
                            content.push_str(refusal);
                        }
                    }
                }
                _ => {}
            }
            // Official OpenAI refusal strings appear as delta.refusal or message.refusal.
            // Both downstream transformers treat them as visible content, so omitting
            // them would turn a refusal into an empty false success (C15).
            if !normalize {
                if let Some(refusal) = payload.get("refusal").and_then(|r| r.as_str()) {
                    content.push_str(refusal);
                }
            }
            // Reuse codex_chat_common to exhaustively extract reasoning_content,
            // reasoning string/object, then reasoning_details. A third implementation
            // could miss reasoning_details-only providers such as MiMo/OpenRouter.
            if let Some(text) = extract_reasoning_field_text(payload) {
                reasoning_content.push_str(&text);
            }
            let modern_tools = payload.get("tool_calls").and_then(|t| t.as_array());
            if let Some(deltas) = modern_tools {
                for (pos, tc) in deltas.iter().enumerate() {
                    merge_tool_call_delta(&mut tool_calls, tc, pos);
                }
            }
            let legacy = (normalize || modern_tools.is_none())
                .then(|| payload.get("function_call").filter(|v| !v.is_null()))
                .flatten();
            if let Some(fc) = legacy {
                // Convert a legacy function_call, deprecated in 2023 but still emitted
                // by relays, into one tool_call. Otherwise function_call maps to tool_use
                // with no tool block and stalls the agent loop (C17).
                let synthetic = json!({
                    "index": 0,
                    "id": fc.get("id").and_then(|v| v.as_str()).unwrap_or(""),
                    "type": "function",
                    "function": fc,
                });
                if normalize {
                    merge_tool_call_delta(&mut legacy_calls, &synthetic, 0);
                } else {
                    merge_tool_call_delta(&mut tool_calls, &synthetic, 0);
                }
            }
            Ok(())
        };

    while let Some(block) = take_sse_block(&mut buffer) {
        if let Some((event, data)) = sse_block_parts(&block, false) {
            process_event(&event, &data, true)?;
        }
    }
    // A partial or nonconforming stream may omit the final blank separator. Process the
    // residual buffer as the last block with strict=false to tolerate a severed tail (C2).
    if let Some((event, data)) = sse_block_parts(&buffer, false) {
        process_event(&event, &data, false)?;
    }

    if !saw_choice {
        return Err(ProxyError::TransformError(
            "No chat completion choices in upstream SSE".to_string(),
        ));
    }
    // A close-delimited response can be truncated undetectably at the byte layer. If
    // both finish_reason and [DONE] are absent, treat it as truncated rather than
    // silently wrapping partial content as an apparent success.
    if finish_reason.is_null() && !saw_done {
        return Err(ProxyError::TransformError(
            "Upstream SSE stream appears truncated (no finish_reason or [DONE] marker)".to_string(),
        ));
    }

    // Finalize tool_calls by discarding empty shells from index gaps or missing fields.
    // Backfill missing id/name from the original index, matching streaming.rs
    // tool_call_{idx}/unknown_tool. An empty ID breaks Claude's tool_use_id-to-tool_result
    // round trip.
    let tool_calls: Vec<Value> = tool_calls
        .into_iter()
        .filter_map(|(index, mut tool)| {
            if normalize {
                tool["index"] = json!(index);
                if tool["id"].as_str().is_none_or(str::is_empty) {
                    tool.as_object_mut()?.remove("id");
                }
            } else {
                let function = &tool["function"];
                let empty = [&tool["id"], &function["name"], &function["arguments"]]
                    .into_iter()
                    .all(|value| value.as_str().is_none_or(str::is_empty));
                if empty {
                    return None;
                }
                if tool["id"].as_str().is_none_or(str::is_empty) {
                    tool["id"] = json!(format!("tool_call_{index}"));
                }
                if tool["function"]["name"].as_str().is_none_or(str::is_empty) {
                    tool["function"]["name"] = json!("unknown_tool");
                }
            }
            Some(tool)
        })
        .collect();

    let mut message = serde_json::Map::new();
    message.insert("role".to_string(), json!("assistant"));
    message.insert("content".to_string(), json!(content));
    if !reasoning_content.is_empty() {
        message.insert("reasoning_content".to_string(), json!(reasoning_content));
    }
    if !tool_calls.is_empty() {
        message.insert("tool_calls".to_string(), Value::Array(tool_calls));
    }
    if normalize {
        if let Some(function) = legacy_calls
            .remove(&0)
            .and_then(|call| call.get("function").cloned())
        {
            message.insert("function_call".to_string(), function);
        }
    }

    // Synthesize a UUID when upstream omits a valid ID. Null or empty IDs reduce
    // downstream dedup_request_id to the constant "session:", causing global collisions
    // and INSERT OR REPLACE to overwrite earlier usage rows (C9).
    let id = if envelope_value_meaningful(&id) {
        id
    } else {
        json!(uuid::Uuid::new_v4().to_string())
    };

    let mut response = json!({
        "id": id,
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "message": Value::Object(message),
            "finish_reason": finish_reason,
        }],
    });
    if !usage.is_null() {
        response["usage"] = usage;
    }
    Ok(response)
}

/// Returns whether an envelope value is meaningful. Filters null, empty strings, and
/// numeric zero (including Azure content-filter's 0.0 placeholder) so placeholders do
/// not freeze id/model/created.
fn envelope_value_meaningful(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64() != Some(0.0),
        _ => true,
    }
}

/// Merges one tool_calls delta into a BTreeMap keyed by index. OpenAI streaming sends
/// id/name in the first delta and fragments arguments. Use delta.index when present,
/// otherwise the array position; complete message tool_calls often omit index and would
/// overwrite one another if all defaulted to zero.
fn merge_tool_call_delta(
    tool_calls: &mut std::collections::BTreeMap<usize, Value>,
    delta: &Value,
    fallback_index: usize,
) {
    let index = delta
        .get("index")
        .and_then(|i| i.as_u64())
        .map(|i| i as usize)
        .unwrap_or(fallback_index);
    let target = tool_calls.entry(index).or_insert_with(|| {
        json!({
            "id": "",
            "type": "function",
            "function": {"name": "", "arguments": ""}
        })
    });
    if let Some(v) = delta
        .get("id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        target["id"] = json!(v);
    }
    if let Some(func) = delta.get("function") {
        if let Some(name) = func
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        {
            target["function"]["name"] = json!(name);
        }
        // Append string arguments directly; serialize object/array arguments first.
        // Non-streaming message snapshots often return an object as a compatibility
        // deviation, and accepting only strings would execute tools with empty input (C16).
        match func.get("arguments") {
            Some(Value::String(args)) => {
                if let Some(existing) = target["function"]["arguments"].as_str() {
                    target["function"]["arguments"] = json!(format!("{existing}{args}"));
                }
            }
            Some(v @ (Value::Object(_) | Value::Array(_))) => {
                let serialized = serde_json::to_string(v).unwrap_or_default();
                if let Some(existing) = target["function"]["arguments"].as_str() {
                    target["function"]["arguments"] = json!(format!("{existing}{serialized}"));
                }
            }
            _ => {}
        }
    }
}

// ============================================================================
// Usage recording retained for Claude transformation.
// ============================================================================

fn log_forward_error(
    state: &ProxyState,
    ctx: &RequestContext,
    is_streaming: bool,
    error: &ProxyError,
) {
    use super::usage::logger::UsageLogger;

    let logger = UsageLogger::new(&state.db);
    let status_code = map_proxy_error_to_status(error);
    let error_message = get_error_message(error);
    if let Err(e) = logger.log_error_with_context(
        ctx.request_id().to_string(),
        ctx.correlation_id(),
        ctx.provider.id.clone(),
        ctx.app_type_str.to_string(),
        ctx.request_model.clone(),
        status_code,
        error_message,
        ctx.latency_ms(),
        is_streaming,
        Some(ctx.session_id.clone()),
        None,
    ) {
        log::warn!("Failed to log failed request: {e}");
    }
}

/// Records request usage.
///
/// `outbound_model` anchors per-request pricing to the model actually sent upstream.
/// It is the post-takeover mapped truth, or `request_model` when no mapping applies.
#[allow(clippy::too_many_arguments)]
async fn log_usage(
    state: &ProxyState,
    correlation_id: Option<String>,
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
    error_message: Option<String>,
) {
    use super::usage::logger::UsageLogger;

    if !usage_logging_enabled(state) {
        return;
    }

    let logger = UsageLogger::new(&state.db);

    let (multiplier, pricing_model_source) =
        logger.resolve_pricing_config(provider_id, app_type).await;
    let pricing_model = if pricing_model_source == PRICING_SOURCE_REQUEST {
        outbound_model
    } else {
        model
    };

    let request_id = usage.dedup_request_id();

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
        error_message,
        session_id,
        None, // provider_type
        is_streaming,
    ) {
        log::warn!("[USG-001] Failed to record usage: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::{
        body_looks_like_sse, body_snippet, chat_sse_to_response_value,
        chat_sse_to_response_value_for_normalization, codex_proxy_error_json, log_forward_error,
        normalize_chat_fallback, normalize_chat_value, responses_sse_to_response_value,
        should_emit_anthropic_reasoning, should_use_claude_transform_streaming, transform,
        translate_tokenize_count_response, upstream_body_parse_error,
    };
    use crate::{
        app_config::AppType,
        database::Database,
        provider::{Provider, ProviderMeta},
        proxy::{
            failover_switch::FailoverSwitchManager,
            handler_context::RequestContext,
            provider_router::ProviderRouter,
            providers::{
                codex_chat_history::CodexChatHistoryStore, gemini_shadow::GeminiShadowStore,
            },
            server::ProxyState,
            types::{ProxyConfig, ProxyStatus},
            ProxyError,
        },
        AppError,
    };
    use std::{collections::HashMap, sync::Arc};
    use tokio::sync::RwLock;

    #[test]
    fn tokenize_count_response_maps_scalar_count() {
        assert_eq!(
            translate_tokenize_count_response(
                br#"{"tokens":[1,2],"count":2,"max_model_len":1048576}"#
            )
            .expect("valid count"),
            br#"{"input_tokens":2}"#
        );
    }

    #[test]
    fn tokenize_count_response_rejects_batch_count() {
        assert!(matches!(
            translate_tokenize_count_response(
                br#"{"tokens":[[1],[2]],"count":[1,1],"max_model_len":1048576}"#
            ),
            Err(ProxyError::ForwardFailed(_))
        ));
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

    #[tokio::test]
    async fn forward_error_usage_uses_harness_request_id() -> Result<(), AppError> {
        let db = Arc::new(Database::memory().expect("memory db"));
        let mut provider = Provider::with_id(
            "nexus-provider".to_string(),
            "Nexus GLM-5.2".to_string(),
            serde_json::json!({
                "env": {
                    "ANTHROPIC_BASE_URL": "http://127.0.0.1:9",
                    "ANTHROPIC_AUTH_TOKEN": "dummy"
                }
            }),
            None,
        );
        provider.meta = Some(ProviderMeta {
            provider_type: Some("nexus".to_string()),
            ..ProviderMeta::default()
        });
        db.save_provider("claude-desktop", &provider)
            .expect("save provider");
        db.set_current_provider("claude-desktop", &provider.id)
            .expect("set current provider");

        let state = build_state(db.clone());
        let body = serde_json::json!({
            "model": "claude-haiku-4-5",
            "stream": true,
            "messages": []
        });
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-request-id", "req-from-harness".parse().unwrap());
        headers.insert(
            "x-claude-code-session-id",
            "session-from-harness".parse().unwrap(),
        );

        let ctx = RequestContext::new(
            &state,
            &body,
            &headers,
            AppType::ClaudeDesktop,
            "Claude Desktop",
            "claude-desktop",
        )
        .await
        .expect("request context");

        log_forward_error(
            &state,
            &ctx,
            true,
            &ProxyError::Timeout("provider did not respond".to_string()),
        );

        let conn = crate::database::lock_conn!(db.conn);
        let (request_id, correlation_id, session_id, status_code, is_streaming): (
            String,
            Option<String>,
            Option<String>,
            i64,
            i64,
        ) = conn
            .query_row(
                "SELECT request_id, correlation_id, session_id, status_code, is_streaming
                 FROM proxy_request_logs
                 WHERE request_id = 'req-from-harness'",
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
            )
            .expect("failed forward usage row");

        assert_eq!(request_id, "req-from-harness");
        assert_eq!(correlation_id.as_deref(), Some("req-from-harness"));
        assert_eq!(session_id.as_deref(), Some("session-from-harness"));
        assert_eq!(status_code, 504);
        assert_eq!(is_streaming, 1);
        Ok(())
    }

    #[test]
    fn anthropic_reasoning_visibility_honors_omitted_display() {
        for (thinking, expected) in [
            (serde_json::json!({}), false),
            (serde_json::json!({"thinking": {"type": "enabled"}}), true),
            (
                serde_json::json!({
                    "thinking": {"type": "adaptive", "display": "omitted"}
                }),
                false,
            ),
        ] {
            assert_eq!(should_emit_anthropic_reasoning(&thinking, true), expected);
            assert!(should_emit_anthropic_reasoning(&thinking, false));
        }
    }

    const LATE_CHAT_SSE: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"late\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n";

    fn fallback_error(anthropic: bool) -> serde_json::Value {
        let error = normalize_chat_fallback(
            LATE_CHAT_SSE,
            &axum::http::HeaderMap::new(),
            true,
            anthropic,
        )
        .unwrap_err();
        let ProxyError::UpstreamError { status, body } = error else {
            panic!("expected typed upstream error")
        };
        assert_eq!(status, 502);
        serde_json::from_str(&body.unwrap()).unwrap()
    }

    #[test]
    fn fallback_maps_late_output_to_protocol_error() {
        for anthropic in [true, false] {
            assert_eq!(
                fallback_error(anthropic)["error"]["code"],
                "stream_protocol_error"
            );
        }
    }

    #[test]
    fn nonstream_rejects_tool_finish_without_a_tool() {
        for anthropic in [true, false] {
            assert!(matches!(
                normalize_chat_value(
                    serde_json::json!({"choices": [{
                        "message": {"content": null},
                        "finish_reason": "tool_calls"
                    }]}),
                    anthropic,
                ),
                Err(ProxyError::UpstreamError { status: 502, .. })
            ));
        }
    }

    #[test]
    fn body_looks_like_sse_detects_unlabeled_sse_prefixes() {
        assert!(body_looks_like_sse("data: {\"id\":\"1\"}\n\n"));
        assert!(body_looks_like_sse("event: message\ndata: {}\n\n"));
        // The other two SSE field-line types may also appear first.
        assert!(body_looks_like_sse("id: 1\ndata: {}\n\n"));
        assert!(body_looks_like_sse("retry: 3000\ndata: {}\n\n"));
        // OpenRouter may emit a comment before the stream.
        assert!(body_looks_like_sse(
            ": OPENROUTER PROCESSING\n\ndata: {}\n\n"
        ));
        // BOM plus leading whitespace.
        assert!(body_looks_like_sse("\u{feff}\n  data: {}\n\n"));
        // Do not misclassify HTML interception pages or ordinary text as SSE.
        assert!(!body_looks_like_sse("<html><body>blocked</body></html>"));
        assert!(!body_looks_like_sse("Bad Gateway"));
        assert!(!body_looks_like_sse(""));
    }

    #[test]
    fn upstream_body_parse_error_carries_field_diagnostics() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("content-type", "text/html".parse().unwrap());
        headers.insert("content-encoding", "gzip".parse().unwrap());
        let parse_err = serde_json::from_str::<serde_json::Value>("<html>").unwrap_err();

        let err = upstream_body_parse_error(
            "Failed to parse upstream response",
            &parse_err,
            &headers,
            "<html>\nblocked</html>",
        );

        match err {
            ProxyError::TransformError(msg) => {
                assert!(msg.contains("content-type: text/html"), "{msg}");
                assert!(msg.contains("content-encoding: gzip"), "{msg}");
                assert!(msg.contains("<html>\\nblocked</html>"), "{msg}");
            }
            other => panic!("expected TransformError, got {other:?}"),
        }
    }

    #[test]
    fn upstream_body_parse_error_marks_missing_headers() {
        let headers = axum::http::HeaderMap::new();
        let parse_err = serde_json::from_str::<serde_json::Value>("data:").unwrap_err();

        let err = upstream_body_parse_error("x", &parse_err, &headers, "data: oops");

        match err {
            ProxyError::TransformError(msg) => {
                assert!(msg.contains("content-type: <none>"), "{msg}");
                assert!(msg.contains("content-encoding: <none>"), "{msg}");
            }
            other => panic!("expected TransformError, got {other:?}"),
        }
    }

    #[test]
    fn chat_sse_to_response_value_collects_reasoning_alias() {
        // OpenRouter/Kimi use a reasoning string; some gateways use an object.
        let sse = "data: {\"id\":\"c1\",\"model\":\"kimi-k2.6\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning\":\"think\"},\"finish_reason\":null}]}\n\n\
data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning\":{\"content\":\"ing\"},\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n";

        let response = chat_sse_to_response_value(sse).unwrap();

        assert_eq!(
            response["choices"][0]["message"]["reasoning_content"],
            "thinking"
        );
        assert_eq!(response["choices"][0]["message"]["content"], "ok");
    }

    #[test]
    fn chat_sse_to_response_value_collects_reasoning_details() {
        // Providers such as MiMo/OpenRouter may emit only reasoning_details arrays.
        // The shared extractor must preserve that reasoning.
        let sse = "data: {\"id\":\"c1\",\"model\":\"mimo\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_details\":[{\"type\":\"reasoning.text\",\"text\":\"think\"}]},\"finish_reason\":null}]}\n\n\
data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_details\":[{\"type\":\"reasoning.text\",\"text\":\"ing\"}],\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n";

        let response = chat_sse_to_response_value(sse).unwrap();

        assert_eq!(
            response["choices"][0]["message"]["reasoning_content"],
            "thinking"
        );
        assert_eq!(response["choices"][0]["message"]["content"], "ok");
    }

    #[test]
    fn responses_sse_to_response_value_handles_missing_trailing_blank_line() {
        // Mislabeled or nonconforming SSE may omit the blank line after response.completed.
        let sse = "event: response.completed\n\
data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_tail\",\"status\":\"completed\",\"model\":\"gpt-5.4\",\"output\":[],\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n";

        let response = responses_sse_to_response_value(sse).unwrap();

        assert_eq!(response["id"], "resp_tail");
    }

    #[test]
    fn responses_sse_to_response_value_ignores_truncated_trailing_block() {
        // A truncated residual tail must not invalidate an already complete aggregate.
        // The Codex OAuth path reuses this function.
        let sse = "event: response.completed\n\
data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_ok\",\"status\":\"completed\",\"model\":\"gpt-5.4\",\"output\":[],\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n\
\n\
event: response.extra\n\
data: {\"type\":\"resp";

        let response = responses_sse_to_response_value(sse).unwrap();

        assert_eq!(response["id"], "resp_ok");
    }

    #[test]
    fn chat_sse_to_response_value_skips_azure_placeholder_envelope() {
        // Azure content-filter preludes carry ""/0 placeholders that must not freeze
        // envelope fields.
        let sse = "data: {\"id\":\"\",\"model\":\"\",\"created\":0,\"object\":\"\",\"choices\":[],\"prompt_filter_results\":[]}\n\n\
data: {\"id\":\"chatcmpl-real\",\"model\":\"gpt-5.4\",\"created\":42,\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\n";

        let response = chat_sse_to_response_value(sse).unwrap();

        assert_eq!(response["id"], "chatcmpl-real");
        assert_eq!(response["model"], "gpt-5.4");
        assert_eq!(response["created"], 42);
    }

    #[test]
    fn chat_sse_to_response_value_tolerates_null_error_field() {
        // one-api gateways may attach `"error": null` to every chunk; it is not an
        // upstream error.
        let sse = "data: {\"id\":\"c1\",\"model\":\"m\",\"error\":null,\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\n";

        let response = chat_sse_to_response_value(sse).unwrap();

        assert_eq!(response["choices"][0]["message"]["content"], "hi");
    }

    #[test]
    fn chat_sse_to_response_value_first_finish_reason_wins() {
        // Kimi-k2.6 may send a trailing finish_reason after tool_use. Its `stop` must not
        // override the earlier `tool_calls`, matching streaming.rs first-wins.
        let sse = "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"f\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n\
data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n";

        let response = chat_sse_to_response_value(sse).unwrap();

        assert_eq!(response["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn chat_sse_to_response_value_unwraps_message_shaped_fake_stream() {
        // A fake-streaming relay wraps a complete chat.completion in one SSE message
        // event rather than a delta.
        let sse = "data: {\"id\":\"c1\",\"object\":\"chat.completion\",\"model\":\"m\",\"choices\":[{\"index\":0,\"message\":{\"role\":\"assistant\",\"content\":\"full answer\"},\"finish_reason\":\"stop\"}]}\n\n\
data: [DONE]\n\n";

        let response = chat_sse_to_response_value(sse).unwrap();

        assert_eq!(response["choices"][0]["message"]["content"], "full answer");
        assert_eq!(response["choices"][0]["finish_reason"], "stop");
    }

    #[test]
    fn chat_sse_to_response_value_message_snapshot_overrides_deltas() {
        // In a mixed stream, a complete message snapshot after deltas replaces those
        // deltas to prevent double counting.
        let sse = "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"par\"},\"finish_reason\":null}]}\n\n\
data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"message\":{\"role\":\"assistant\",\"content\":\"full\"},\"finish_reason\":\"stop\"}]}\n\n";

        let response = chat_sse_to_response_value(sse).unwrap();

        assert_eq!(response["choices"][0]["message"]["content"], "full");
    }

    #[test]
    fn chat_sse_to_response_value_backfills_sparse_tool_call_ids() {
        // Discard shells from index gaps and backfill a missing ID as tool_call_{idx}
        // using its original index.
        let sse = "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":1,\"function\":{\"name\":\"f2\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n";

        let response = chat_sse_to_response_value(sse).unwrap();

        let tool_calls = response["choices"][0]["message"]["tool_calls"]
            .as_array()
            .unwrap();
        assert_eq!(
            tool_calls.len(),
            1,
            "The empty shell at index 0 should be discarded"
        );
        assert_eq!(tool_calls[0]["id"], "tool_call_1");
        assert_eq!(tool_calls[0]["function"]["name"], "f2");
    }

    #[test]
    fn chat_sse_to_response_value_strips_bom_before_parsing() {
        // Sniffing accepts a BOM, so block parsing must strip it or silently lose the
        // first data line.
        let sse = "\u{feff}data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\n";

        let response = chat_sse_to_response_value(sse).unwrap();

        assert_eq!(response["choices"][0]["message"]["content"], "hi");
    }

    #[test]
    fn body_snippet_sanitizes_controls_and_truncates() {
        assert_eq!(
            body_snippet("<html>\r\nblocked\u{0}</html>", 120),
            "<html>\\nblocked\u{FFFD}</html>"
        );
        let long = "a".repeat(200);
        let snippet = body_snippet(&long, 120);
        assert_eq!(snippet.chars().count(), 121); // 120 characters plus the ellipsis.
        assert!(snippet.ends_with('…'));
    }

    #[test]
    fn chat_sse_to_response_value_aggregates_text_finish_reason_and_usage() {
        let sse = "data: {\"id\":\"chatcmpl-1\",\"object\":\"chat.completion.chunk\",\"created\":123,\"model\":\"gpt-5.4\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hel\"},\"finish_reason\":null}]}\n\n\
data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"},\"finish_reason\":null}]}\n\n\
data: {\"id\":\"chatcmpl-1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2,\"total_tokens\":12}}\n\n\
data: [DONE]\n\n";

        let response = chat_sse_to_response_value(sse).unwrap();

        assert_eq!(response["id"], "chatcmpl-1");
        assert_eq!(response["object"], "chat.completion");
        assert_eq!(response["model"], "gpt-5.4");
        assert_eq!(response["choices"][0]["message"]["role"], "assistant");
        assert_eq!(response["choices"][0]["message"]["content"], "Hello");
        assert_eq!(response["choices"][0]["finish_reason"], "stop");
        assert_eq!(response["usage"]["prompt_tokens"], 10);
    }

    #[test]
    fn chat_sse_to_response_value_merges_tool_call_argument_fragments() {
        let sse = "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n\
data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"city\\\":\"}}]},\"finish_reason\":null}]}\n\n\
data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"SF\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n\
data: [DONE]\n\n";

        let response = chat_sse_to_response_value(sse).unwrap();

        let tool_call = &response["choices"][0]["message"]["tool_calls"][0];
        assert_eq!(tool_call["id"], "call_1");
        assert_eq!(tool_call["function"]["name"], "get_weather");
        assert_eq!(tool_call["function"]["arguments"], "{\"city\":\"SF\"}");
        assert_eq!(response["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn chat_sse_to_response_value_collects_reasoning_content() {
        let sse = "data: {\"id\":\"c1\",\"model\":\"deepseek-r2\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"think\"},\"finish_reason\":null}]}\n\n\
data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"ing\",\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\n";

        let response = chat_sse_to_response_value(sse).unwrap();

        assert_eq!(
            response["choices"][0]["message"]["reasoning_content"],
            "thinking"
        );
        assert_eq!(response["choices"][0]["message"]["content"], "ok");
    }

    #[test]
    fn chat_sse_to_response_value_handles_missing_trailing_blank_line() {
        // A nonconforming or partial stream may omit the blank line after its last event.
        let sse = "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n";

        let response = chat_sse_to_response_value(sse).unwrap();

        assert_eq!(response["choices"][0]["message"]["content"], "hi");
    }

    #[test]
    fn chat_sse_to_response_value_handles_crlf_delimiters() {
        // Real HTTP SSE separates events with `\r\n\r\n`.
        let sse = "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\r\n\
\r\n\
data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\r\n\
\r\n\
data: [DONE]\r\n\
\r\n";

        let response = chat_sse_to_response_value(sse).unwrap();

        assert_eq!(response["choices"][0]["message"]["content"], "hi");
        assert_eq!(response["choices"][0]["finish_reason"], "stop");
    }

    #[test]
    fn chat_sse_to_response_value_propagates_upstream_error_event() {
        let sse = "data: {\"error\":{\"message\":\"rate limited by gateway\",\"code\":429}}\n\n";

        let err = chat_sse_to_response_value(sse).unwrap_err();
        match err {
            ProxyError::TransformError(msg) => assert!(msg.contains("rate limited by gateway")),
            other => panic!("expected TransformError, got {other:?}"),
        }
    }

    #[test]
    fn chat_sse_to_response_value_rejects_truncated_stream() {
        // A content delta without finish_reason or [DONE] is an undetectable
        // close-delimited truncation at the byte layer. Report truncation rather than
        // silently returning partial content.
        let sse = "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"par\"},\"finish_reason\":null}]}\n\n";

        let err = chat_sse_to_response_value(sse).unwrap_err();
        match err {
            ProxyError::TransformError(msg) => assert!(msg.contains("truncated")),
            other => panic!("expected TransformError, got {other:?}"),
        }
    }

    #[test]
    fn chat_sse_to_response_value_accepts_done_marker_without_finish_reason() {
        // A nonconforming upstream may omit finish_reason but end with [DONE]; accept it
        // as complete.
        let sse = "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n\
data: [DONE]\n\n";

        let response = chat_sse_to_response_value(sse).unwrap();

        assert_eq!(response["choices"][0]["message"]["content"], "hi");
        assert_eq!(
            response["choices"][0]["finish_reason"],
            serde_json::Value::Null
        );
    }

    #[test]
    fn chat_sse_to_response_value_rejects_stream_without_chunks() {
        let err = chat_sse_to_response_value(": keepalive\n\ndata: [DONE]\n\n").unwrap_err();
        match err {
            ProxyError::TransformError(msg) => {
                assert!(msg.contains("No chat completion choices"))
            }
            other => panic!("expected TransformError, got {other:?}"),
        }
    }

    #[test]
    fn chat_sse_to_response_value_rejects_choiceless_stream_despite_done() {
        // A metadata/usage-only chunk plus [DONE] has no choice payload. [DONE] alone
        // must not fabricate an empty success; saw_choice requires choice evidence.
        let sse = "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":0,\"total_tokens\":1}}\n\n\
data: [DONE]\n\n";

        let err = chat_sse_to_response_value(sse).unwrap_err();
        match err {
            ProxyError::TransformError(msg) => {
                assert!(msg.contains("No chat completion choices"), "{msg}")
            }
            other => panic!("expected TransformError, got {other:?}"),
        }
    }

    #[test]
    fn chat_sse_to_response_value_huge_tool_call_index_does_not_oom() {
        // C1: an upstream-controlled huge index must not densify an array and OOM the
        // process. BTreeMap uses one slot and retains the index for synthesized IDs.
        let sse = "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":4000000000,\"function\":{\"name\":\"f\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n";

        let response = chat_sse_to_response_value(sse).unwrap();
        let tool_calls = response["choices"][0]["message"]["tool_calls"]
            .as_array()
            .unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["id"], "tool_call_4000000000");
        assert_eq!(tool_calls[0]["function"]["name"], "f");
    }

    #[test]
    fn chat_sse_to_response_value_empty_delta_falls_back_to_message_snapshot() {
        // C3: when a choice has both empty delta:{} and a complete message snapshot, the
        // delta key must not discard message content while finish_reason bypasses guards.
        let sse = "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{},\"message\":{\"role\":\"assistant\",\"content\":\"full answer\"},\"finish_reason\":\"stop\"}]}\n\n\
data: [DONE]\n\n";

        let response = chat_sse_to_response_value(sse).unwrap();
        assert_eq!(response["choices"][0]["message"]["content"], "full answer");
        assert_eq!(response["choices"][0]["finish_reason"], "stop");
    }

    #[test]
    fn chat_sse_normalization_mode_preserves_strict_stream_selection() {
        let empty_delta = "data: {\"choices\":[{\"delta\":{},\"message\":{\"content\":\"ignored\"},\"finish_reason\":\"stop\"}]}\n\n";
        let response = chat_sse_to_response_value_for_normalization(empty_delta).unwrap();
        assert_eq!(response["choices"][0]["message"]["content"], "");

        let snapshots = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n\n",
            "data: {\"choices\":[{\"message\":{\"content\":\"b\"},\"finish_reason\":\"stop\"}]}\n\n"
        );
        let response = chat_sse_to_response_value_for_normalization(snapshots).unwrap();
        assert_eq!(response["choices"][0]["message"]["content"], "ab");

        let choices = "data: {\"choices\":[{\"index\":1,\"delta\":{\"content\":\"first\"},\"finish_reason\":\"stop\"},{\"index\":0,\"delta\":{\"content\":\"second\"},\"finish_reason\":\"stop\"}]}\n\n";
        let response = chat_sse_to_response_value_for_normalization(choices).unwrap();
        assert_eq!(response["choices"][0]["message"]["content"], "first");
    }

    #[test]
    fn chat_sse_to_response_value_empty_delta_scaffold_does_not_wipe_real_content() {
        // C3 inverse trap: when every chunk has a real delta plus an empty message shell,
        // the shell must not clear accumulated content. Prefer a non-empty delta.
        let sse = "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"message\":{},\"finish_reason\":null}]}\n\n\
data: {\"id\":\"c1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" there\"},\"message\":{},\"finish_reason\":\"stop\"}]}\n\n";

        let response = chat_sse_to_response_value(sse).unwrap();
        assert_eq!(response["choices"][0]["message"]["content"], "hi there");
    }

    #[test]
    fn chat_sse_to_response_value_object_form_tool_arguments_preserved() {
        // C16: serialize object-valued arguments in a message snapshot instead of
        // dropping them to empty tool input.
        let sse = "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"message\":{\"role\":\"assistant\",\"tool_calls\":[{\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":{\"city\":\"SF\"}}}]},\"finish_reason\":\"tool_calls\"}]}\n\n";

        let response = chat_sse_to_response_value(sse).unwrap();
        let args = response["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(args).unwrap();
        assert_eq!(parsed["city"], "SF");
    }

    #[test]
    fn chat_sse_to_response_value_collects_refusal() {
        // C15: include delta.refusal in visible content so a refusal does not become an
        // empty false success.
        let sse = "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"refusal\":\"I can't help with that.\"},\"finish_reason\":\"stop\"}]}\n\n";

        let response = chat_sse_to_response_value(sse).unwrap();
        assert_eq!(
            response["choices"][0]["message"]["content"],
            "I can't help with that."
        );
    }

    #[test]
    fn chat_sse_to_response_value_maps_legacy_function_call() {
        // C17: map legacy function_call to one tool_call. Mapping finish_reason to
        // tool_use without a tool block would stall the agent.
        let sse = "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"message\":{\"role\":\"assistant\",\"content\":null,\"function_call\":{\"name\":\"get_weather\",\"arguments\":\"{\\\"city\\\":\\\"SF\\\"}\"}},\"finish_reason\":\"function_call\"}]}\n\n";

        let response = chat_sse_to_response_value(sse).unwrap();
        let tc = &response["choices"][0]["message"]["tool_calls"][0];
        assert_eq!(tc["function"]["name"], "get_weather");
        assert_eq!(tc["function"]["arguments"], "{\"city\":\"SF\"}");
    }

    #[test]
    fn chat_sse_to_response_value_event_error_fails_even_after_complete_choice() {
        // C18: event:error must fail even after a complete choice when data has no error
        // key; do not disguise it as success.
        let sse = "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":\"stop\"}]}\n\n\
event: error\n\
data: {\"message\":\"insufficient_user_quota\",\"code\":429}\n\n";

        let err = chat_sse_to_response_value(sse).unwrap_err();
        match err {
            ProxyError::TransformError(msg) => {
                assert!(msg.contains("insufficient_user_quota"), "{msg}")
            }
            other => panic!("expected TransformError, got {other:?}"),
        }
    }

    #[test]
    fn chat_sse_to_response_value_tolerates_empty_error_placeholder() {
        // C12: empty-object or empty-message error placeholders must not invalidate a
        // successful stream.
        let sse = "data: {\"id\":\"c1\",\"model\":\"m\",\"error\":{},\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\n";

        let response = chat_sse_to_response_value(sse).unwrap();
        assert_eq!(response["choices"][0]["message"]["content"], "hi");
    }

    #[test]
    fn chat_sse_to_response_value_tolerates_truncated_residual_after_complete() {
        // C2: truncated JSON after a complete finish_reason block must not invalidate
        // the completed aggregate.
        let sse = "data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\n\
data: {\"usage\":{\"prompt_to";

        let response = chat_sse_to_response_value(sse).unwrap();
        assert_eq!(response["choices"][0]["message"]["content"], "hi");
    }

    #[test]
    fn chat_sse_to_response_value_float_zero_does_not_freeze_envelope() {
        // C14: a 0.0 created placeholder must not freeze the envelope; a real value may
        // replace it.
        let sse = "data: {\"id\":\"\",\"model\":\"\",\"created\":0.0,\"choices\":[]}\n\n\
data: {\"id\":\"chatcmpl-real\",\"model\":\"m\",\"created\":42,\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\n";

        let response = chat_sse_to_response_value(sse).unwrap();
        assert_eq!(response["created"], 42);
        assert_eq!(response["id"], "chatcmpl-real");
    }

    #[test]
    fn chat_sse_to_response_value_synthesizes_id_when_absent() {
        // C9: synthesize a unique non-empty ID when upstream omits one to prevent
        // downstream dedup from collapsing to a constant collision.
        let sse = "data: {\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\n";

        let r1 = chat_sse_to_response_value(sse).unwrap();
        let r2 = chat_sse_to_response_value(sse).unwrap();
        let id1 = r1["id"].as_str().unwrap();
        let id2 = r2["id"].as_str().unwrap();
        assert!(!id1.is_empty());
        assert_ne!(
            id1, id2,
            "Separate ID-less aggregates need distinct IDs to avoid dedup collisions"
        );
    }

    #[test]
    fn chat_sse_to_response_value_accepts_indented_data_lines() {
        // C4: indented data lines accepted by sniffing must also aggregate instead of
        // disappearing silently.
        let sse = "  data: {\"id\":\"c1\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\n";

        let response = chat_sse_to_response_value(sse).unwrap();
        assert_eq!(response["choices"][0]["message"]["content"], "hi");
    }

    #[test]
    fn responses_sse_completed_then_trailing_failed_keeps_success() {
        // C8: after response.completed, a complete response.failed in the residual tail
        // must not overturn success. The Codex OAuth aggregate path reuses this behavior.
        let sse = "event: response.completed\n\
data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_ok\",\"status\":\"completed\",\"model\":\"gpt-5.4\",\"output\":[]}}\n\n\
event: response.failed\n\
data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"boom\"}}}\n";

        let response = responses_sse_to_response_value(sse).unwrap();
        assert_eq!(response["id"], "resp_ok");
    }

    #[test]
    fn aggregated_chat_sse_round_trips_through_openai_to_anthropic() {
        // End-to-end: mislabeled SSE Content-Type -> aggregate -> existing non-streaming
        // transformer -> Anthropic JSON.
        let sse = "data: {\"id\":\"chatcmpl-9\",\"created\":1,\"model\":\"gpt-5.4\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hi\"},\"finish_reason\":null}]}\n\n\
data: {\"id\":\"chatcmpl-9\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":4,\"completion_tokens\":1,\"total_tokens\":5}}\n\n\
data: [DONE]\n\n";

        let aggregated = chat_sse_to_response_value(sse).unwrap();
        let anthropic = transform::openai_to_anthropic(aggregated).unwrap();

        assert_eq!(anthropic["model"], "gpt-5.4");
        assert_eq!(anthropic["content"][0]["type"], "text");
        assert_eq!(anthropic["content"][0]["text"], "Hi");
        assert_eq!(anthropic["stop_reason"], "end_turn");
    }

    #[test]
    fn codex_oauth_responses_force_streaming_even_if_client_sent_false() {
        assert!(should_use_claude_transform_streaming(
            false,
            false,
            "openai_responses",
            true,
        ));
    }

    #[test]
    fn upstream_sse_response_always_uses_streaming_path() {
        assert!(should_use_claude_transform_streaming(
            false,
            true,
            "openai_chat",
            false,
        ));
    }

    #[test]
    fn non_streaming_response_stays_non_streaming_for_regular_openai_responses() {
        assert!(!should_use_claude_transform_streaming(
            false,
            false,
            "openai_responses",
            false,
        ));
    }

    #[test]
    fn responses_sse_to_response_value_collects_output_items() {
        let sse = r#"event: response.output_item.done
data: {"type":"response.output_item.done","item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]}}

event: response.completed
data: {"type":"response.completed","response":{"id":"resp_1","status":"completed","model":"gpt-5.4","output":[],"usage":{"input_tokens":10,"output_tokens":2}}}

"#;

        let response = responses_sse_to_response_value(sse).unwrap();

        assert_eq!(response["id"], "resp_1");
        assert_eq!(response["output"][0]["type"], "message");
        assert_eq!(response["output"][0]["content"][0]["text"], "hello");
    }

    #[test]
    fn responses_sse_to_response_value_handles_crlf_delimiters() {
        // Real HTTP SSE separates events with `\r\n\r\n`. take_sse_block must support
        // both delimiter styles or every standards-compliant upstream, including the
        // Codex OAuth HTTPS backend, would fail with TransformError.
        let sse = "event: response.output_item.done\r\n\
data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"hi\"}]}}\r\n\
\r\n\
event: response.completed\r\n\
data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_crlf\",\"status\":\"completed\",\"model\":\"gpt-5.4\",\"output\":[],\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}\r\n\
\r\n";

        let response = responses_sse_to_response_value(sse).unwrap();

        assert_eq!(response["id"], "resp_crlf");
        assert_eq!(response["output"][0]["type"], "message");
        assert_eq!(response["output"][0]["content"][0]["text"], "hi");
    }

    #[test]
    fn responses_sse_to_response_value_returns_err_on_response_failed() {
        let sse = "event: response.failed\n\
data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"upstream blew up\"}}}\n\n";

        let err = responses_sse_to_response_value(sse).unwrap_err();
        match err {
            ProxyError::TransformError(msg) => assert!(msg.contains("upstream blew up")),
            other => panic!("expected TransformError, got {other:?}"),
        }
    }

    #[test]
    fn responses_sse_to_response_value_errors_when_no_completed_event() {
        let sse = "event: response.output_item.done\n\
data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"message\"}}\n\n";

        assert!(responses_sse_to_response_value(sse).is_err());
    }

    #[test]
    fn codex_proxy_forward_error_includes_context_and_cause() {
        let error = ProxyError::ForwardFailed("Connection failed: dns lookup failed".to_string());
        let body = codex_proxy_error_json("DeepSeek", "deepseek-chat", "/responses", &error);

        let message = body["error"]["message"].as_str().unwrap();
        assert!(message.contains("Nexus Composer local proxy failed"));
        assert!(message.contains("DeepSeek"));
        assert!(message.contains("deepseek-chat"));
        assert!(message.contains("/responses"));
        assert!(message.contains("dns lookup failed"));
        assert_eq!(body["error"]["code"], "cc_switch_forward_failed");
        assert_eq!(body["error"]["provider"], "DeepSeek");
        assert_eq!(body["error"]["model"], "deepseek-chat");
    }

    #[test]
    fn codex_proxy_upstream_error_normalizes_nonstandard_body() {
        let error = ProxyError::UpstreamError {
            status: 502,
            body: Some(
                r#"{"base_resp":{"status_code":2013,"status_msg":"upstream gateway failed"}}"#
                    .to_string(),
            ),
        };
        let body = codex_proxy_error_json("MiniMax", "abab6.5s", "/responses", &error);

        let message = body["error"]["message"].as_str().unwrap();
        assert!(message.contains("upstream_status: HTTP 502"));
        assert!(message.contains("upstream gateway failed"));
        assert_eq!(body["error"]["code"], 2013);
        assert_eq!(body["error"]["upstream_status"], 502);
    }

    #[test]
    fn codex_proxy_413_points_to_upstream_not_local_proxy() {
        // Simulate an upstream nginx 413 HTML page caused by client_max_body_size
        // (issue #666: long context, large images, or large logs exceed the body limit).
        let error = ProxyError::UpstreamError {
            status: 413,
            body: Some(
                "<html>\r\n<head><title>413 Request Entity Too Large</title></head>\r\n\
                 <body>\r\n<center><h1>413 Request Entity Too Large</h1></center>\r\n\
                 <hr><center>nginx/1.29.6</center>\r\n</body>\r\n</html>"
                    .to_string(),
            ),
        };
        let body = codex_proxy_error_json("HCAI", "gpt-5.5", "/responses", &error);

        let message = body["error"]["message"].as_str().unwrap();
        // Do not mislabel this as a local proxy failure.
        assert!(!message.contains("Nexus Composer local proxy failed"));
        // Identify upstream, the size limit, and an actionable recovery step.
        assert!(message.contains("413"));
        assert!(message.to_lowercase().contains("upstream"));
        assert!(message.contains("/compact"));
        // Do not echo the entire nginx HTML page to the user.
        assert!(!message.contains("<html>"));
        assert!(!message.contains("nginx/1.29.6"));
        // Preserve structured fields for programmatic consumption and UI rendering.
        assert_eq!(body["error"]["upstream_status"], 413);
        assert_eq!(body["error"]["provider"], "HCAI");
        assert_eq!(body["error"]["model"], "gpt-5.5");
        assert_eq!(body["error"]["endpoint"], "/responses");
    }
}
