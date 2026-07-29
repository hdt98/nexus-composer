//! HTTP proxy server.
//!
//! Axum-based HTTP server for proxy requests.
//!
//! Uses a manual hyper HTTP/1.1 accept loop with `preserve_header_case(true)` so
//! that the original header-name casing from the CLI client is captured in a
//! `HeaderCaseMap` extension.  This map is later forwarded to the upstream via
//! the hyper-based HTTP client, producing wire-level header casing identical to
//! a direct (non-proxied) CLI request.

use super::{
    failover_switch::FailoverSwitchManager,
    handlers,
    log_codes::srv as log_srv,
    provider_router::ProviderRouter,
    providers::{codex_chat_history::CodexChatHistoryStore, gemini_shadow::GeminiShadowStore},
    types::*,
    ProxyError,
};
use crate::database::Database;
use axum::{
    extract::DefaultBodyLimit,
    routing::{any, get, post},
    Router,
};
use hyper_util::rt::TokioIo;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{oneshot, RwLock};
use tokio::task::JoinHandle;

/// Shared proxy server state.
#[derive(Clone)]
pub struct ProxyState {
    pub db: Arc<Database>,
    pub config: Arc<RwLock<ProxyConfig>>,
    pub status: Arc<RwLock<ProxyStatus>>,
    pub start_time: Arc<RwLock<Option<std::time::Instant>>>,
    /// Current provider for each app type: app_type -> (provider_id, provider_name).
    pub current_providers: Arc<RwLock<std::collections::HashMap<String, (String, String)>>>,
    /// Shared `ProviderRouter` whose circuit state persists across requests.
    pub provider_router: Arc<ProviderRouter>,
    /// Gemini Native shadow state for replaying `thoughtSignature` and tool calls.
    pub gemini_shadow: Arc<GeminiShadowStore>,
    /// Codex Chat bridge history for restoring tool calls referenced by `previous_response_id`.
    pub codex_chat_history: Arc<CodexChatHistoryStore>,
    /// App handle for emitting events and updating the tray menu.
    pub app_handle: Option<tauri::AppHandle>,
    /// Failover switch manager.
    pub failover_manager: Arc<FailoverSwitchManager>,
}

/// HTTP proxy server.
pub struct ProxyServer {
    config: ProxyConfig,
    state: ProxyState,
    shutdown_tx: Arc<RwLock<Option<oneshot::Sender<()>>>>,
    /// Server task handle used to wait for shutdown to complete.
    server_handle: Arc<RwLock<Option<JoinHandle<()>>>>,
}

impl ProxyServer {
    pub fn new(
        config: ProxyConfig,
        db: Arc<Database>,
        app_handle: Option<tauri::AppHandle>,
    ) -> Self {
        // Share one provider router so circuit state persists across requests.
        let provider_router = Arc::new(ProviderRouter::new(db.clone()));
        // Create the failover switch manager.
        let failover_manager = Arc::new(FailoverSwitchManager::new(db.clone()));

        let state = ProxyState {
            db,
            config: Arc::new(RwLock::new(config.clone())),
            status: Arc::new(RwLock::new(ProxyStatus::default())),
            start_time: Arc::new(RwLock::new(None)),
            current_providers: Arc::new(RwLock::new(std::collections::HashMap::new())),
            provider_router,
            gemini_shadow: Arc::new(GeminiShadowStore::default()),
            codex_chat_history: Arc::new(CodexChatHistoryStore::default()),
            app_handle,
            failover_manager,
        };

        Self {
            config,
            state,
            shutdown_tx: Arc::new(RwLock::new(None)),
            server_handle: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn start(&self) -> Result<ProxyServerInfo, ProxyError> {
        // Reject duplicate starts.
        if self.shutdown_tx.read().await.is_some() {
            return Err(ProxyError::AlreadyRunning);
        }

        let addr: SocketAddr =
            format!("{}:{}", self.config.listen_address, self.config.listen_port)
                .parse()
                .map_err(|e| ProxyError::BindFailed(format!("Invalid address: {e}")))?;

        // Create the shutdown channel.
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        // Build routes.
        let app = self.build_router();

        // Bind the listener.
        let listener = tokio::net::TcpListener::bind(&addr)
            .await
            .map_err(|e| ProxyError::BindFailed(e.to_string()))?;
        let local_addr = listener
            .local_addr()
            .map_err(|e| ProxyError::BindFailed(e.to_string()))?;
        let actual_port = local_addr.port();

        log::info!(
            "[{}] Proxy server listening on {local_addr}",
            log_srv::STARTED
        );

        // Update the global proxy port for system proxy detection.
        crate::proxy::http_client::set_proxy_port(actual_port);

        // Store the shutdown handle.
        *self.shutdown_tx.write().await = Some(shutdown_tx);

        // Update status.
        let mut status = self.state.status.write().await;
        status.running = true;
        status.address = self.config.listen_address.clone();
        status.port = actual_port;
        drop(status);

        // Record the start time.
        *self.state.start_time.write().await = Some(std::time::Instant::now());

        // Start a manual hyper HTTP/1.1 accept loop with `preserve_header_case` enabled
        // to capture the original casing of client request headers.
        let state = self.state.clone();
        let handle = tokio::spawn(async move {
            let mut shutdown_rx = shutdown_rx;
            loop {
                tokio::select! {
                    result = listener.accept() => {
                        let (stream, _remote_addr) = match result {
                            Ok(v) => v,
                            Err(e) => {
                                log::error!("[{SRV}] Accept failed: {e}", SRV = log_srv::ACCEPT_ERR);
                                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                                continue;
                            }
                        };

                        let app = app.clone();
                        tokio::spawn(async move {
                            // Peek raw TCP bytes to capture original header casing
                            // before hyper parses (and lowercases) the header names.
                            let original_cases = {
                                let mut peek_buf = vec![0u8; 8192];
                                match stream.peek(&mut peek_buf).await {
                                    Ok(n) => {
                                        let cases = super::hyper_client::OriginalHeaderCases::from_raw_bytes(&peek_buf[..n]);
                                        log::debug!(
                                            "[ProxyServer] Peeked {} bytes, captured {} header casings",
                                            n, cases.cases.len()
                                        );
                                        cases
                                    }
                                    Err(e) => {
                                        log::debug!("[ProxyServer] peek failed (non-fatal): {e}");
                                        super::hyper_client::OriginalHeaderCases::default()
                                    }
                                }
                            };

                            // Bridge the axum router (`tower::Service`) to hyper with `service_fn`.
                            let service = hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                                let mut router = app.clone();
                                let cases = original_cases.clone();
                                async move {
                                    // Convert `hyper::body::Incoming` to `axum::body::Body` while
                                    // preserving extensions.
                                    let (mut parts, body) = req.into_parts();

                                    // Insert our own header case map alongside hyper's internal one
                                    parts.extensions.insert(cases);

                                    let body = axum::body::Body::new(body);
                                    let axum_req = http::Request::from_parts(parts, body);
                                    <Router as tower::Service<http::Request<axum::body::Body>>>::call(&mut router, axum_req).await
                                }
                            });

                            if let Err(e) = hyper::server::conn::http1::Builder::new()
                                .preserve_header_case(true)
                                .serve_connection(TokioIo::new(stream), service)
                                .await
                            {
                                // Connection resets and broken pipes are common for proxies.
                                log::debug!("[{SRV}] connection error: {e}", SRV = log_srv::CONN_ERR);
                            }
                        });
                    }
                    _ = &mut shutdown_rx => {
                        break;
                    }
                }
            }

            // Update status after the server stops.
            state.status.write().await.running = false;
            *state.start_time.write().await = None;
        });

        // Store the server task handle.
        *self.server_handle.write().await = Some(handle);

        Ok(ProxyServerInfo {
            address: self.config.listen_address.clone(),
            port: actual_port,
            started_at: chrono::Utc::now().to_rfc3339(),
        })
    }

    pub async fn stop(&self) -> Result<(), ProxyError> {
        // 1. Send the shutdown signal.
        if let Some(tx) = self.shutdown_tx.write().await.take() {
            let _ = tx.send(());
        } else {
            return Err(ProxyError::NotRunning);
        }

        // 2. Wait up to five seconds for the server task to finish.
        if let Some(handle) = self.server_handle.write().await.take() {
            match tokio::time::timeout(std::time::Duration::from_secs(5), handle).await {
                Ok(Ok(())) => {
                    log::info!("[{}] Proxy server stopped", log_srv::STOPPED);
                    Ok(())
                }
                Ok(Err(e)) => {
                    log::warn!("[{}] Proxy server task failed: {e}", log_srv::TASK_ERROR);
                    Err(ProxyError::StopFailed(e.to_string()))
                }
                Err(_) => {
                    log::warn!(
                        "[{}] Proxy server shutdown timed out after 5 seconds; continuing",
                        log_srv::STOP_TIMEOUT
                    );
                    Err(ProxyError::StopTimeout)
                }
            }
        } else {
            Ok(())
        }
    }

    pub async fn get_status(&self) -> ProxyStatus {
        let mut status = self.state.status.read().await.clone();

        // Calculate uptime.
        if let Some(start) = *self.state.start_time.read().await {
            status.uptime_seconds = start.elapsed().as_secs();
        }

        // Read the current provider for each app type.
        let current_providers = self.state.current_providers.read().await;
        status.active_targets = current_providers
            .iter()
            .map(|(app_type, (provider_id, provider_name))| ActiveTarget {
                app_type: app_type.clone(),
                provider_id: provider_id.clone(),
                provider_name: provider_name.clone(),
            })
            .collect();

        status
    }

    /// Updates the target provider shown in `active_targets` for an app type.
    ///
    /// This does not imply that the provider has handled a request. It lets the UI
    /// immediately reflect hot switches and the P1 selection when failover is enabled.
    pub async fn set_active_target(&self, app_type: &str, provider_id: &str, provider_name: &str) {
        let mut current_providers = self.state.current_providers.write().await;
        current_providers.insert(
            app_type.to_string(),
            (provider_id.to_string(), provider_name.to_string()),
        );
    }

    pub async fn clear_active_target(&self, app_type: &str) {
        self.state.current_providers.write().await.remove(app_type);
    }

    fn build_router(&self) -> Router {
        Router::new()
            // Health checks
            .route("/health", get(handlers::health_check))
            .route("/status", get(handlers::get_status))
            // Claude API, with and without a prefix
            .route("/v1/messages", post(handlers::handle_messages))
            .route("/claude/v1/messages", post(handlers::handle_messages))
            .route("/v1/messages/count_tokens", post(handlers::handle_messages))
            .route(
                "/claude/v1/messages/count_tokens",
                post(handlers::handle_messages),
            )
            // Local Claude Desktop third-party gateway with a separate provider namespace
            .route(
                "/claude-desktop/v1/models",
                get(handlers::handle_claude_desktop_models),
            )
            .route(
                "/claude-desktop/v1/messages",
                post(handlers::handle_claude_desktop_messages),
            )
            .route(
                "/claude-desktop/v1/messages/count_tokens",
                post(handlers::handle_claude_desktop_messages),
            )
            // OpenAI Chat Completions API for Codex CLI, with and without a prefix
            .route("/chat/completions", post(handlers::handle_chat_completions))
            .route(
                "/v1/chat/completions",
                post(handlers::handle_chat_completions),
            )
            .route(
                "/v1/v1/chat/completions",
                post(handlers::handle_chat_completions),
            )
            .route(
                "/codex/v1/chat/completions",
                post(handlers::handle_chat_completions),
            )
            // OpenAI Models API (Codex CLI reachability check)
            .route("/models", get(handlers::handle_models))
            .route("/v1/models", get(handlers::handle_models))
            // OpenAI Responses API for Codex CLI, with and without a prefix
            .route("/responses", post(handlers::handle_responses))
            .route("/v1/responses", post(handlers::handle_responses))
            .route("/v1/v1/responses", post(handlers::handle_responses))
            .route("/codex/v1/responses", post(handlers::handle_responses))
            // OpenAI Responses Compact API pass-through for Codex CLI remote compaction
            .route(
                "/responses/compact",
                post(handlers::handle_responses_compact),
            )
            .route(
                "/v1/responses/compact",
                post(handlers::handle_responses_compact),
            )
            .route(
                "/v1/v1/responses/compact",
                post(handlers::handle_responses_compact),
            )
            .route(
                "/codex/v1/responses/compact",
                post(handlers::handle_responses_compact),
            )
            // Gemini API, with and without a prefix
            //
            // Use `any(..)` for all HTTP methods. In addition to POST `:generateContent`,
            // `:streamGenerateContent`, and `:countTokens`, Gemini SDK and CLI clients send
            // requests to read-only endpoints such as GET `/models` and GET `/models/<id>`.
            // A POST-only route would return 404 before local usage accounting, rectification,
            // and failover run.
            .route("/v1beta/*path", any(handlers::handle_gemini))
            .route("/gemini/v1beta/*path", any(handlers::handle_gemini))
            // Route the Gemini GA `/v1` path for the original SDK.
            .route("/gemini/v1/*path", any(handlers::handle_gemini))
            // Raise the default body limit to avoid 413 Payload Too Large.
            .layer(DefaultBodyLimit::max(200 * 1024 * 1024))
            .with_state(self.state.clone())
    }

    /// Updates runtime configuration without restarting the server.
    pub async fn apply_runtime_config(&self, config: &ProxyConfig) {
        *self.state.config.write().await = config.clone();
    }

    /// Updates circuit breaker configuration at runtime.
    ///
    /// Applies the new configuration to every existing circuit breaker.
    pub async fn update_circuit_breaker_configs(
        &self,
        config: super::circuit_breaker::CircuitBreakerConfig,
    ) {
        self.state.provider_router.update_all_configs(config).await;
    }

    pub async fn update_circuit_breaker_config_for_app(
        &self,
        app_type: &str,
        config: super::circuit_breaker::CircuitBreakerConfig,
    ) {
        self.state
            .provider_router
            .update_app_configs(app_type, config)
            .await;
    }

    /// Resets the circuit breaker for a provider.
    pub async fn reset_provider_circuit_breaker(&self, provider_id: &str, app_type: &str) {
        self.state
            .provider_router
            .reset_provider_breaker(provider_id, app_type)
            .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app_config::AppType,
        provider::{Provider, ProviderMeta},
    };
    use axum::{
        body::Body,
        http::{Request, StatusCode, Uri},
        response::IntoResponse,
        Json,
    };
    use http_body_util::BodyExt;
    use serde_json::{json, Value};

    struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

    impl<T> Drop for AbortOnDrop<T> {
        fn drop(&mut self) {
            self.0.abort();
        }
    }

    fn save_provider(
        db: &Database,
        id: &str,
        base_url: &str,
        api_format: &str,
        managed_nexus: bool,
    ) {
        let mut provider = Provider::with_id(
            id.to_string(),
            "Nexus GLM-5.2".to_string(),
            json!({"env": {
                "ANTHROPIC_BASE_URL": base_url,
                "ANTHROPIC_AUTH_TOKEN": "upstream-token"
            }}),
            None,
        );
        provider.meta = Some(
            serde_json::from_value::<ProviderMeta>(json!({
                "apiFormat": api_format,
                "providerType": managed_nexus.then_some("nexus"),
                "localProxyRequestOverrides": {
                    "body": {
                        "max_tokens": 65_536,
                        "chat_template_kwargs": {
                            "enable_thinking": true,
                            "clear_thinking": false
                        }
                    }
                }
            }))
            .expect("build provider metadata"),
        );
        db.save_provider(AppType::Claude.as_str(), &provider)
            .expect("save count_tokens provider");
        db.set_current_provider(AppType::Claude.as_str(), id)
            .expect("select count_tokens provider");
    }

    async fn dispatch(router: &Router, path: &str, body: Value) -> axum::response::Response {
        let request = Request::builder()
            .method(http::Method::POST)
            .uri(path)
            .header(http::header::CONTENT_TYPE, "application/json")
            .header(http::header::AUTHORIZATION, "Bearer proxy-managed")
            .body(Body::from(body.to_string()))
            .expect("build count_tokens request");
        let mut router = router.clone();
        tower::Service::call(&mut router, request)
            .await
            .expect("route count_tokens request")
    }

    fn usage_count(db: &Database) -> i64 {
        let conn = db.conn.lock().expect("lock test database");
        conn.query_row("SELECT COUNT(*) FROM proxy_request_logs", [], |row| {
            row.get(0)
        })
        .expect("count usage rows")
    }

    fn count_body() -> Value {
        json!({
            "model": "GLM-5.2-FP8[1m]",
            "system": "Work from the saved proof state.",
            "messages": [
                {
                    "role": "assistant",
                    "content": [
                        {"type": "thinking", "thinking": "prior proof checkpoint", "signature": "sig"},
                        {"type": "text", "text": "Checkpoint saved."}
                    ]
                },
                {"role": "user", "content": "Continue the proof."}
            ],
            "tools": [{
                "name": "verify",
                "description": "Verify the proof",
                "input_schema": {"type": "object", "properties": {}}
            }],
            "tool_choice": {"type": "auto"}
        })
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn count_tokens_preserves_managed_and_native_request_semantics() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (capture_tx, mut capture_rx) = tokio::sync::mpsc::unbounded_channel();
        let (native_tx, mut native_rx) = tokio::sync::mpsc::unbounded_channel();
        let upstream = Router::new()
            .route(
                "/v1/tokenize",
                post(move |Json(body): Json<Value>| {
                    let capture_tx = capture_tx.clone();
                    async move {
                        let is_error = body["messages"]
                            .as_array()
                            .and_then(|messages| messages.last())
                            .is_some_and(|message| message["content"] == "force error");
                        capture_tx.send(body).expect("capture tokenize request");
                        if is_error {
                            (StatusCode::BAD_REQUEST, Json(json!({"error": "bad count"})))
                                .into_response()
                        } else {
                            Json(json!({
                                "tokens": [1, 2, 3],
                                "count": 17,
                                "max_model_len": 1_048_576
                            }))
                            .into_response()
                        }
                    }
                }),
            )
            .route(
                "/v1/messages/count_tokens",
                post(move |uri: Uri, Json(body): Json<Value>| async move {
                    native_tx.send((uri, body)).expect("capture native request");
                    Json(json!({"input_tokens": 9}))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind count_tokens upstream");
        let upstream_addr = listener.local_addr().expect("read upstream address");
        let _upstream_task = AbortOnDrop(tokio::spawn(async move {
            axum::serve(listener, upstream).await
        }));

        let db = Arc::new(Database::memory().expect("create test database"));
        let provider_id = crate::settings::get_current_provider(&AppType::Claude)
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| "count-claude".to_string());
        save_provider(
            &db,
            &provider_id,
            &format!("http://{upstream_addr}/v1"),
            "openai_chat",
            true,
        );
        let server = ProxyServer::new(
            ProxyConfig {
                listen_port: 0,
                ..ProxyConfig::default()
            },
            db.clone(),
            None,
        );
        let router = server.build_router();

        let response = dispatch(&router, "/claude/v1/messages/count_tokens", count_body()).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("read count response")
            .to_bytes();
        assert_eq!(body, bytes::Bytes::from_static(br#"{"input_tokens":17}"#));

        let captured = capture_rx.recv().await.expect("captured tokenize request");
        let messages = captured["messages"].as_array().expect("OpenAI messages");
        let assistant = messages
            .iter()
            .find(|message| message["role"] == "assistant")
            .expect("assistant history");
        assert_eq!(assistant["reasoning_content"], "prior proof checkpoint");
        assert_eq!(captured["tools"][0]["function"]["name"], "verify");
        assert_eq!(captured["tool_choice"], "auto");
        assert_eq!(
            captured["chat_template_kwargs"],
            json!({"enable_thinking": true, "clear_thinking": false})
        );
        assert!(captured.get("max_tokens").is_none());
        assert!(captured.get("stream").is_none());

        let response = dispatch(
            &router,
            "/v1/messages/count_tokens",
            json!({
                "model": "GLM-5.2-FP8[1m]",
                "messages": [{"role": "user", "content": "force error"}]
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            response
                .into_body()
                .collect()
                .await
                .expect("read count error")
                .to_bytes(),
            bytes::Bytes::from_static(br#"{"error":"bad count"}"#)
        );

        save_provider(
            &db,
            &provider_id,
            &format!("http://{upstream_addr}/v1"),
            "anthropic",
            false,
        );
        let native_body = json!({
            "model": "claude-sonnet-4-5",
            "messages": [{"role": "user", "content": "count natively"}]
        });
        let response = dispatch(
            &router,
            "/v1/messages/count_tokens?beta=true",
            native_body.clone(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .into_body()
                .collect()
                .await
                .expect("read native count response")
                .to_bytes(),
            bytes::Bytes::from_static(br#"{"input_tokens":9}"#)
        );
        let (uri, body) = native_rx.recv().await.expect("captured native request");
        assert_eq!(uri.path(), "/v1/messages/count_tokens");
        assert_eq!(uri.query(), Some("beta=true"));
        assert_eq!(body, native_body);
        assert_eq!(usage_count(&db), 0);
    }
}
