//! Proxy Session - request session management
//!
//! Creates a session context for each proxy request and tracks state and
//! metadata across the full request lifecycle.
//!
//! ## Session ID extraction
//!
//! Supports extracting a Session ID from client requests so multiple requests
//! in the same conversation can be correlated:
//! - Claude: extract from `metadata.user_id` (format:
//!   `user_xxx_session_yyy`) or `metadata.session_id`
//! - Codex: extract from client session/thread headers, `client_metadata`, or
//!   legacy `metadata.session_id`
//! - Others: generate a new UUID

use axum::http::HeaderMap;
use std::time::Instant;
use uuid::Uuid;

/// Client request format
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ClientFormat {
    /// Claude Messages API (/v1/messages)
    Claude,
    /// Codex Response API (/v1/responses)
    Codex,
    /// OpenAI Chat Completions API (/v1/chat/completions)
    OpenAI,
    /// Gemini API (/v1beta/models/*/generateContent)
    Gemini,
    /// Gemini CLI API (/v1internal/models/*/generateContent)
    GeminiCli,
    /// Unknown format
    Unknown,
}

#[allow(dead_code)]
impl ClientFormat {
    /// Detect the format from the request path
    pub fn from_path(path: &str) -> Self {
        if path.contains("/v1/messages") {
            ClientFormat::Claude
        } else if path.contains("/v1/responses") {
            ClientFormat::Codex
        } else if path.contains("/v1/chat/completions") {
            ClientFormat::OpenAI
        } else if path.contains("/v1internal/") && path.contains("generateContent") {
            // Gemini CLI uses the /v1internal/ path
            ClientFormat::GeminiCli
        } else if (path.contains("/v1beta/") || path.contains("/v1/"))
            && path.contains("generateContent")
        {
            // Gemini API uses the /v1beta/ or /v1/ path
            ClientFormat::Gemini
        } else if path.contains("generateContent") {
            // Generic Gemini endpoint
            ClientFormat::Gemini
        } else {
            ClientFormat::Unknown
        }
    }

    /// Detect the format from the request body as a fallback
    pub fn from_body(body: &serde_json::Value) -> Self {
        // Claude shape: messages array + model field + no response_format
        if body.get("messages").is_some()
            && body.get("model").is_some()
            && body.get("response_format").is_none()
            && body.get("contents").is_none()
        {
            // Distinguish Claude from OpenAI
            if body.get("max_tokens").is_some() {
                return ClientFormat::Claude;
            }
            return ClientFormat::OpenAI;
        }

        // Codex shape: input field
        if body.get("input").is_some() {
            return ClientFormat::Codex;
        }

        // Gemini shape: contents array
        if body.get("contents").is_some() {
            return ClientFormat::Gemini;
        }

        ClientFormat::Unknown
    }

    /// Convert to a string
    pub fn as_str(&self) -> &'static str {
        match self {
            ClientFormat::Claude => "claude",
            ClientFormat::Codex => "codex",
            ClientFormat::OpenAI => "openai",
            ClientFormat::Gemini => "gemini",
            ClientFormat::GeminiCli => "gemini_cli",
            ClientFormat::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for ClientFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Proxy session
///
/// Holds context for the full request lifecycle
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ProxySession {
    /// Unique session ID
    pub session_id: String,
    /// Request start time
    pub start_time: Instant,
    /// HTTP method
    pub method: String,
    /// Request URL
    pub request_url: String,
    /// User-Agent
    pub user_agent: Option<String>,
    /// Client request format
    pub client_format: ClientFormat,
    /// Selected provider ID
    pub provider_id: Option<String>,
    /// Model name
    pub model: Option<String>,
    /// Whether the request is streaming
    pub is_streaming: bool,
}

#[allow(dead_code)]
impl ProxySession {
    /// Create a session from a request
    pub fn from_request(
        method: &str,
        request_url: &str,
        user_agent: Option<&str>,
        body: Option<&serde_json::Value>,
    ) -> Self {
        // Detect the client format
        let mut client_format = ClientFormat::from_path(request_url);
        if client_format == ClientFormat::Unknown {
            if let Some(body) = body {
                client_format = ClientFormat::from_body(body);
            }
        }

        // Detect whether the request is streaming
        let is_streaming = body
            .and_then(|b| b.get("stream"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Extract the model name
        let model = body
            .and_then(|b| b.get("model"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Self {
            session_id: Uuid::new_v4().to_string(),
            start_time: Instant::now(),
            method: method.to_string(),
            request_url: request_url.to_string(),
            user_agent: user_agent.map(|s| s.to_string()),
            client_format,
            provider_id: None,
            model,
            is_streaming,
        }
    }

    /// Set the provider ID
    pub fn with_provider(mut self, provider_id: &str) -> Self {
        self.provider_id = Some(provider_id.to_string());
        self
    }

    /// Get request latency in milliseconds
    pub fn latency_ms(&self) -> u64 {
        self.start_time.elapsed().as_millis() as u64
    }
}

// ============================================================================
// Session ID extractor
// ============================================================================

/// Session ID source
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionIdSource {
    /// Extracted from metadata.user_id (Claude)
    MetadataUserId,
    /// Extracted from metadata.session_id
    MetadataSessionId,
    /// Extracted from headers (Codex)
    Header,
    /// Newly generated
    Generated,
}

/// Session ID extraction result
#[derive(Debug, Clone)]
pub struct SessionIdResult {
    /// Extracted or generated Session ID
    pub session_id: String,
    /// Session ID source
    pub source: SessionIdSource,
    /// Whether the ID came from the client instead of being generated
    pub client_provided: bool,
}

const MAX_CORRELATION_ID_LEN: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HarnessIdentity {
    CodexThread(String),
    ClaudeCodeSession(String),
}

pub(crate) fn extract_request_id(headers: &HeaderMap) -> String {
    bounded_header(headers, "x-request-id").unwrap_or_else(|| Uuid::new_v4().to_string())
}

pub(crate) fn extract_harness_identity(
    headers: &HeaderMap,
    body: &serde_json::Value,
    client_format: &str,
) -> Option<HarnessIdentity> {
    match client_format {
        "codex" | "openai" => first_bounded_header(headers, &["x-codex-thread-id", "thread-id"])
            .or_else(|| bounded_json_pointer(body, "/client_metadata/thread_id"))
            .map(HarnessIdentity::CodexThread),
        "claude" | "claude-desktop" => ["x-claude-code-session-id", "claude-code-session-id"]
            .into_iter()
            .find_map(|name| bounded_header(headers, name))
            .or_else(|| {
                body.pointer("/metadata/user_id")
                    .and_then(|value| value.as_str())
                    .and_then(claude_code_session_from_user_id)
            })
            .map(HarnessIdentity::ClaudeCodeSession),
        _ => None,
    }
}

fn first_bounded_header(headers: &HeaderMap, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| bounded_header(headers, name))
}

fn bounded_header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(bounded_value)
}

fn bounded_value(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value.len() <= MAX_CORRELATION_ID_LEN).then(|| value.to_string())
}

fn bounded_json_pointer(body: &serde_json::Value, pointer: &str) -> Option<String> {
    body.pointer(pointer)
        .and_then(|value| value.as_str())
        .and_then(bounded_value)
}

fn claude_code_session_from_user_id(user_id: &str) -> Option<String> {
    user_id.strip_prefix("user_")?;
    let session_id = parse_session_from_user_id(user_id)?;
    Uuid::parse_str(&session_id).ok()?;
    bounded_value(&session_id)
}

/// Extract or generate a Session ID from a request
///
/// Lightweight implementation that only extracts a session_id for logging and
/// does not perform complex session management.
///
/// ## Extraction priority
///
/// ### Claude requests
/// 1. `metadata.user_id` (format: `user_xxx_session_yyy`) -> extract the `yyy`
///    segment
/// 2. `metadata.session_id` -> use directly
/// 3. Generate a new UUID
///
/// ### Codex requests
/// 1. Headers: Codex thread/session identity
/// 2. `client_metadata.session_id` / `client_metadata.thread_id`
/// 3. Legacy `metadata.session_id`
/// 4. Generate a new UUID
///
/// ## Example
///
/// ```ignore
/// let result = extract_session_id(&headers, &body, "claude");
/// println!("Session ID: {} (from {:?})", result.session_id, result.source);
/// ```
pub fn extract_session_id(
    headers: &HeaderMap,
    body: &serde_json::Value,
    client_format: &str,
) -> SessionIdResult {
    if client_format == "claude" || client_format == "claude-desktop" {
        if let Some(result) = extract_claude_session(headers, body) {
            return result;
        }
    }

    // Special handling for Codex requests
    if client_format == "codex" || client_format == "openai" {
        if let Some(result) = extract_codex_session(headers, body) {
            return result;
        }
    }

    // Claude requests: extract from metadata
    if let Some(result) = extract_from_metadata(body) {
        return result;
    }

    // Fallback: generate a new Session ID
    generate_new_session_id()
}

/// Extract a Claude Session ID
fn extract_claude_session(
    headers: &HeaderMap,
    body: &serde_json::Value,
) -> Option<SessionIdResult> {
    for header_name in &["x-claude-code-session-id", "claude-code-session-id"] {
        if let Some(value) = headers.get(*header_name) {
            if let Ok(session_id) = value.to_str() {
                if !session_id.is_empty() {
                    return Some(SessionIdResult {
                        session_id: session_id.to_string(),
                        source: SessionIdSource::Header,
                        client_provided: true,
                    });
                }
            }
        }
    }

    extract_from_metadata(body)
}

/// Extract a Codex Session ID
fn extract_codex_session(headers: &HeaderMap, body: &serde_json::Value) -> Option<SessionIdResult> {
    let candidates = [
        (
            first_bounded_header(headers, &["session-id", "session_id", "x-session-id"]),
            SessionIdSource::Header,
        ),
        (
            bounded_json_pointer(body, "/client_metadata/session_id"),
            SessionIdSource::MetadataSessionId,
        ),
        (
            first_bounded_header(headers, &["x-codex-thread-id", "thread-id"]),
            SessionIdSource::Header,
        ),
        (
            bounded_json_pointer(body, "/client_metadata/thread_id"),
            SessionIdSource::MetadataSessionId,
        ),
    ];
    for (session_id, source) in candidates {
        if let Some(session_id) = session_id.filter(|value| value.len() > 20) {
            return Some(SessionIdResult {
                session_id: format!("codex_{session_id}"),
                source,
                client_provided: true,
            });
        }
    }

    // 2. Extract from body.metadata.session_id
    if let Some(session_id) = body
        .get("metadata")
        .and_then(|m| m.get("session_id"))
        .and_then(|v| v.as_str())
    {
        if session_id.len() > 10 {
            return Some(SessionIdResult {
                session_id: format!("codex_{session_id}"),
                source: SessionIdSource::MetadataSessionId,
                client_provided: true,
            });
        }
    }

    // previous_response_id is a response cursor in the Responses protocol, not
    // a stable session identity.
    // In Chat/Responses bridging, this value usually comes from the random
    // upstream response id returned for each turn.
    // Treating it as a prompt_cache_key or Codex session header would rotate
    // the cache key on every request.

    None
}

/// Extract a Session ID from metadata (Claude)
fn extract_from_metadata(body: &serde_json::Value) -> Option<SessionIdResult> {
    let metadata = body.get("metadata")?;

    // 1. Extract from metadata.user_id (format: user_xxx_session_yyy)
    if let Some(user_id) = metadata.get("user_id").and_then(|v| v.as_str()) {
        if let Some(session_id) = parse_session_from_user_id(user_id) {
            return Some(SessionIdResult {
                session_id,
                source: SessionIdSource::MetadataUserId,
                client_provided: true,
            });
        }
    }

    // 2. Extract directly from metadata.session_id
    if let Some(session_id) = metadata.get("session_id").and_then(|v| v.as_str()) {
        if !session_id.is_empty() {
            return Some(SessionIdResult {
                session_id: session_id.to_string(),
                source: SessionIdSource::MetadataSessionId,
                client_provided: true,
            });
        }
    }

    None
}

/// Parse a session_id from user_id
///
/// Format: `user_identifier_session_actual_session_id`
pub(super) fn parse_session_from_user_id(user_id: &str) -> Option<String> {
    // Find the "_session_" separator
    if let Some(pos) = user_id.find("_session_") {
        let session_id = &user_id[pos + 9..]; // "_session_" has length 9
        if !session_id.is_empty() {
            return Some(session_id.to_string());
        }
    }
    None
}

/// Generate a new Session ID
fn generate_new_session_id() -> SessionIdResult {
    SessionIdResult {
        session_id: Uuid::new_v4().to_string(),
        source: SessionIdSource::Generated,
        client_provided: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_client_format_from_path_claude() {
        assert_eq!(
            ClientFormat::from_path("/v1/messages"),
            ClientFormat::Claude
        );
        assert_eq!(
            ClientFormat::from_path("/api/v1/messages"),
            ClientFormat::Claude
        );
    }

    #[test]
    fn test_client_format_from_path_codex() {
        assert_eq!(
            ClientFormat::from_path("/v1/responses"),
            ClientFormat::Codex
        );
    }

    #[test]
    fn test_client_format_from_path_openai() {
        assert_eq!(
            ClientFormat::from_path("/v1/chat/completions"),
            ClientFormat::OpenAI
        );
    }

    #[test]
    fn test_client_format_from_path_gemini() {
        assert_eq!(
            ClientFormat::from_path("/v1beta/models/gemini-pro:generateContent"),
            ClientFormat::Gemini
        );
    }

    #[test]
    fn test_client_format_from_path_gemini_cli() {
        assert_eq!(
            ClientFormat::from_path("/v1internal/models/gemini-pro:generateContent"),
            ClientFormat::GeminiCli
        );
    }

    #[test]
    fn test_client_format_from_body_claude() {
        let body = json!({
            "model": "claude-3-5-sonnet",
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 1024
        });
        assert_eq!(ClientFormat::from_body(&body), ClientFormat::Claude);
    }

    #[test]
    fn test_client_format_from_body_codex() {
        let body = json!({
            "input": "Write a function"
        });
        assert_eq!(ClientFormat::from_body(&body), ClientFormat::Codex);
    }

    #[test]
    fn test_client_format_from_body_gemini() {
        let body = json!({
            "contents": [{"parts": [{"text": "Hello"}]}]
        });
        assert_eq!(ClientFormat::from_body(&body), ClientFormat::Gemini);
    }

    #[test]
    fn test_session_id_uniqueness() {
        let session1 = ProxySession::from_request("POST", "/v1/messages", None, None);
        let session2 = ProxySession::from_request("POST", "/v1/messages", None, None);
        assert_ne!(session1.session_id, session2.session_id);
    }

    #[test]
    fn test_session_from_request() {
        let body = json!({
            "model": "claude-3-5-sonnet",
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 1024,
            "stream": true
        });

        let session =
            ProxySession::from_request("POST", "/v1/messages", Some("Mozilla/5.0"), Some(&body));

        assert_eq!(session.method, "POST");
        assert_eq!(session.request_url, "/v1/messages");
        assert_eq!(session.user_agent, Some("Mozilla/5.0".to_string()));
        assert_eq!(session.client_format, ClientFormat::Claude);
        assert_eq!(session.model, Some("claude-3-5-sonnet".to_string()));
        assert!(session.is_streaming);
    }

    #[test]
    fn test_session_with_provider() {
        let session = ProxySession::from_request("POST", "/v1/messages", None, None)
            .with_provider("provider-123");

        assert_eq!(session.provider_id, Some("provider-123".to_string()));
    }

    #[test]
    fn test_client_format_as_str() {
        assert_eq!(ClientFormat::Claude.as_str(), "claude");
        assert_eq!(ClientFormat::Codex.as_str(), "codex");
        assert_eq!(ClientFormat::OpenAI.as_str(), "openai");
        assert_eq!(ClientFormat::Gemini.as_str(), "gemini");
        assert_eq!(ClientFormat::GeminiCli.as_str(), "gemini_cli");
        assert_eq!(ClientFormat::Unknown.as_str(), "unknown");
    }

    // ========== Session ID extraction tests ==========

    #[test]
    fn test_extract_session_from_claude_metadata_user_id() {
        let headers = HeaderMap::new();
        let body = json!({
            "model": "claude-3-5-sonnet",
            "messages": [{"role": "user", "content": "Hello"}],
            "metadata": {
                "user_id": "user_john_doe_session_abc123def456"
            }
        });

        let result = extract_session_id(&headers, &body, "claude");

        assert_eq!(result.session_id, "abc123def456");
        assert_eq!(result.source, SessionIdSource::MetadataUserId);
        assert!(result.client_provided);
    }

    #[test]
    fn test_extract_session_from_claude_metadata_session_id() {
        let headers = HeaderMap::new();
        let body = json!({
            "model": "claude-3-5-sonnet",
            "messages": [{"role": "user", "content": "Hello"}],
            "metadata": {
                "session_id": "my-session-123"
            }
        });

        let result = extract_session_id(&headers, &body, "claude");

        assert_eq!(result.session_id, "my-session-123");
        assert_eq!(result.source, SessionIdSource::MetadataSessionId);
        assert!(result.client_provided);
    }

    #[test]
    fn test_extract_session_from_claude_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-claude-code-session-id",
            "d937243f-2702-4f20-97b6-c9682235ab81".parse().unwrap(),
        );
        let body = json!({
            "model": "claude-3-5-sonnet",
            "messages": [{"role": "user", "content": "Hello"}]
        });

        let result = extract_session_id(&headers, &body, "claude");

        assert_eq!(result.session_id, "d937243f-2702-4f20-97b6-c9682235ab81");
        assert_eq!(result.source, SessionIdSource::Header);
        assert!(result.client_provided);
    }

    #[test]
    fn test_extract_session_from_claude_desktop_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-claude-code-session-id",
            "desktop-session-123".parse().unwrap(),
        );
        let body = json!({
            "model": "claude-haiku-4-5",
            "messages": [{"role": "user", "content": "Hello"}]
        });

        let result = extract_session_id(&headers, &body, "claude-desktop");

        assert_eq!(result.session_id, "desktop-session-123");
        assert_eq!(result.source, SessionIdSource::Header);
        assert!(result.client_provided);
    }

    #[test]
    fn test_extract_session_from_claude_header_precedes_metadata() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-claude-code-session-id",
            "header-session-123".parse().unwrap(),
        );
        let body = json!({
            "model": "claude-3-5-sonnet",
            "messages": [{"role": "user", "content": "Hello"}],
            "metadata": {
                "session_id": "my-session-123"
            }
        });

        let result = extract_session_id(&headers, &body, "claude");

        assert_eq!(result.session_id, "header-session-123");
        assert_eq!(result.source, SessionIdSource::Header);
        assert!(result.client_provided);
    }

    #[test]
    fn test_codex_previous_response_id_is_not_stable_session_identity() {
        let headers = HeaderMap::new();
        let body = json!({
            "input": "Write a function",
            "previous_response_id": "resp_abc123def456789"
        });

        let result = extract_session_id(&headers, &body, "codex");

        assert!(!result.session_id.is_empty());
        assert_eq!(result.source, SessionIdSource::Generated);
        assert!(!result.client_provided);
    }

    #[test]
    fn test_extract_codex_desktop_session_and_current_thread() {
        let session_id = "019f6022-fd7f-78e3-ba3f-a1168a613f3d";
        let thread_id = "019f6023-1111-7222-8333-444455556666";
        let mut headers = HeaderMap::new();
        headers.insert("session-id", session_id.parse().unwrap());
        headers.insert("thread-id", thread_id.parse().unwrap());
        let body = json!({
            "input": "hello",
            "client_metadata": {"session_id": session_id, "thread_id": thread_id}
        });

        let result = extract_session_id(&headers, &body, "codex");

        assert_eq!(result.session_id, format!("codex_{session_id}"));
        assert_eq!(result.source, SessionIdSource::Header);
        assert!(result.client_provided);
        assert_eq!(
            extract_harness_identity(&headers, &body, "codex"),
            Some(HarnessIdentity::CodexThread(thread_id.to_string()))
        );

        let body_only = extract_session_id(&HeaderMap::new(), &body, "codex");
        assert_eq!(body_only.session_id, format!("codex_{session_id}"));
        assert_eq!(body_only.source, SessionIdSource::MetadataSessionId);
        assert_eq!(
            extract_harness_identity(&HeaderMap::new(), &body, "codex"),
            Some(HarnessIdentity::CodexThread(thread_id.to_string()))
        );
    }

    #[test]
    fn test_extract_session_generates_new_when_not_found() {
        let headers = HeaderMap::new();
        let body = json!({
            "model": "claude-3-5-sonnet",
            "messages": [{"role": "user", "content": "Hello"}]
        });

        let result = extract_session_id(&headers, &body, "claude");

        assert!(!result.session_id.is_empty());
        assert_eq!(result.source, SessionIdSource::Generated);
        assert!(!result.client_provided);
    }

    #[test]
    fn test_parse_session_from_user_id() {
        assert_eq!(
            parse_session_from_user_id("user_john_session_abc123"),
            Some("abc123".to_string())
        );
        assert_eq!(
            parse_session_from_user_id("my_app_session_xyz789"),
            Some("xyz789".to_string())
        );
        // Note: "_session_" is the separator, so the following string matches
        assert_eq!(
            parse_session_from_user_id("no_session_marker"),
            Some("marker".to_string())
        );
        // Case with no "_session_" separator
        assert_eq!(parse_session_from_user_id("user_john_abc123"), None);
        assert_eq!(parse_session_from_user_id("_session_"), None);
    }

    #[test]
    fn request_id_is_preserved_when_bounded_otherwise_generated() {
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", " request-123 ".parse().unwrap());
        assert_eq!(extract_request_id(&headers), "request-123");

        headers.insert(
            "x-request-id",
            "x".repeat(MAX_CORRELATION_ID_LEN + 1).parse().unwrap(),
        );
        assert!(Uuid::parse_str(&extract_request_id(&headers)).is_ok());
    }

    #[test]
    fn harness_identity_uses_only_proven_sources() {
        let mut codex_headers = HeaderMap::new();
        codex_headers.insert("session-id", "generic-session".parse().unwrap());
        codex_headers.insert("thread-id", " ".parse().unwrap());
        assert!(extract_harness_identity(&codex_headers, &json!({}), "codex").is_none());
        codex_headers.insert("thread-id", "thread-123".parse().unwrap());
        assert_eq!(
            extract_harness_identity(&codex_headers, &json!({}), "codex"),
            Some(HarnessIdentity::CodexThread("thread-123".to_string()))
        );
        codex_headers.insert(
            "thread-id",
            "x".repeat(MAX_CORRELATION_ID_LEN + 1).parse().unwrap(),
        );
        assert!(extract_harness_identity(&codex_headers, &json!({}), "codex").is_none());

        let claude_body = json!({
            "metadata": {
                "user_id": "user_abc_session_d937243f-2702-4f20-97b6-c9682235ab81"
            }
        });
        assert_eq!(
            extract_harness_identity(&HeaderMap::new(), &claude_body, "claude"),
            Some(HarnessIdentity::ClaudeCodeSession(
                "d937243f-2702-4f20-97b6-c9682235ab81".to_string()
            ))
        );
        let mut claude_headers = HeaderMap::new();
        claude_headers.insert("x-claude-code-session-id", "header-123".parse().unwrap());
        assert_eq!(
            extract_harness_identity(&claude_headers, &json!({}), "claude"),
            Some(HarnessIdentity::ClaudeCodeSession("header-123".to_string()))
        );
        assert_eq!(
            extract_harness_identity(&claude_headers, &json!({}), "claude-desktop"),
            Some(HarnessIdentity::ClaudeCodeSession("header-123".to_string()))
        );
        assert_eq!(
            extract_harness_identity(
                &HeaderMap::new(),
                &json!({"metadata": {"user_id": "no_session_marker", "session_id": "generic"}}),
                "claude"
            ),
            None
        );
    }
}
