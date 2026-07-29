//! Provider Adapters Module
//!
//! This module provides provider adapters with a unified interface for
//! different upstream provider implementations.
//!
//! ## Module layout
//! - `adapter`: defines the `ProviderAdapter` trait
//! - `auth`: authentication types and strategies
//! - `claude`: Claude (Anthropic) adapter
//! - `codex`: Codex (OpenAI) adapter
//! - `gemini`: Gemini (Google) adapter
//! - `models`: API data models
//! - `transform`: format transformation

mod adapter;
mod auth;
pub(crate) mod chat_reasoning;
mod claude;
mod codex;
pub(crate) mod codex_chat_common;
pub mod codex_chat_history;
pub mod codex_oauth_auth;
pub mod copilot_auth;
pub mod copilot_model_map;
mod gemini;
pub(crate) mod gemini_schema;
pub mod gemini_shadow;
pub mod models;
pub mod streaming;
pub mod streaming_codex_chat;
pub mod streaming_gemini;
pub mod streaming_responses;
pub mod transform;
pub mod transform_codex_chat;
pub mod transform_gemini;
pub mod transform_responses;

use crate::app_config::AppType;
use crate::provider::Provider;
use serde::{Deserialize, Serialize};

// Public exports.
pub use adapter::ProviderAdapter;
pub use auth::{AuthInfo, AuthStrategy};
pub use claude::{
    claude_api_format_needs_transform, get_claude_api_format,
    normalize_anthropic_messages_for_provider, transform_claude_request_for_api_format,
    ClaudeAdapter,
};
pub use codex::CodexAdapter;
pub use codex::{
    apply_codex_chat_upstream_model, codex_provider_upstream_model,
    codex_provider_uses_chat_completions, resolve_codex_chat_reasoning_config,
    should_convert_codex_responses_to_chat,
};
pub use gemini::GeminiAdapter;

/// Provider type enumeration.
///
/// Distinguishes concrete provider implementations and determines
/// authentication and request handling behavior.
/// This is more granular than `AppType` and supports multiple variants under
/// the same `AppType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderType {
    /// Anthropic official API (`x-api-key` + `anthropic-version`).
    Claude,
    /// Claude relay service (Bearer auth only, no `x-api-key`).
    ClaudeAuth,
    /// OpenAI Codex Response API
    Codex,
    /// Google Gemini API (`x-goog-api-key`).
    Gemini,
    /// Google Gemini CLI (OAuth Bearer).
    GeminiCli,
    /// OpenRouter (now supports the Claude Code-compatible interface, so
    /// passthrough is the default and the legacy transform is kept as backup).
    OpenRouter,
    /// GitHub Copilot (OAuth + Copilot token; requires Anthropic <-> OpenAI
    /// transformation).
    GitHubCopilot,
    /// OpenAI Codex (ChatGPT Plus/Pro OAuth; requires Anthropic <-> Responses
    /// API transformation).
    CodexOAuth,
}

impl ProviderType {
    /// Whether format transformation is required.
    ///
    /// OpenRouter previously required Anthropic-to-OpenAI transformation.
    /// That transform is now disabled by default because OpenRouter supports
    /// the Claude Code-compatible interface.
    /// GitHub Copilot still requires Anthropic-to-OpenAI transformation.
    #[allow(dead_code)]
    pub fn needs_transform(&self) -> bool {
        match self {
            ProviderType::GitHubCopilot => true,
            ProviderType::CodexOAuth => true,
            ProviderType::OpenRouter => false,
            _ => false,
        }
    }

    /// Get the default endpoint.
    #[allow(dead_code)]
    pub fn default_endpoint(&self) -> &'static str {
        match self {
            ProviderType::Claude | ProviderType::ClaudeAuth => "https://api.anthropic.com",
            ProviderType::Codex => "https://api.openai.com",
            ProviderType::Gemini | ProviderType::GeminiCli => {
                "https://generativelanguage.googleapis.com"
            }
            ProviderType::OpenRouter => "https://openrouter.ai/api",
            ProviderType::GitHubCopilot => "https://api.githubcopilot.com",
            ProviderType::CodexOAuth => "https://chatgpt.com/backend-api/codex",
        }
    }

    /// Infer the provider type from `AppType` and provider configuration.
    ///
    /// Uses fields such as `base_url`, `auth_mode`, and API key format to
    /// infer the concrete provider type.
    #[allow(dead_code)]
    pub fn from_app_type_and_config(app_type: &AppType, provider: &Provider) -> Self {
        match app_type {
            AppType::Claude | AppType::ClaudeDesktop => {
                if get_claude_api_format(provider) == "gemini_native" {
                    let adapter = ClaudeAdapter::new();
                    return match adapter.extract_auth(provider).map(|auth| auth.strategy) {
                        Some(AuthStrategy::GoogleOAuth) => ProviderType::GeminiCli,
                        _ => ProviderType::Gemini,
                    };
                }

                // Detect GitHub Copilot.
                if let Some(meta) = provider.meta.as_ref() {
                    if meta.provider_type.as_deref() == Some("github_copilot") {
                        return ProviderType::GitHubCopilot;
                    }
                    if meta.provider_type.as_deref() == Some("codex_oauth") {
                        return ProviderType::CodexOAuth;
                    }
                }

                // Detect GitHub Copilot by `base_url`.
                let adapter = ClaudeAdapter::new();
                if let Ok(base_url) = adapter.extract_base_url(provider) {
                    if base_url.contains("githubcopilot.com") {
                        return ProviderType::GitHubCopilot;
                    }
                    // Detect OpenRouter.
                    if base_url.contains("openrouter.ai") {
                        return ProviderType::OpenRouter;
                    }
                }
                // Detect relay services that use Bearer-only authentication.
                // `ProviderMeta` does not expose an `auth_mode` field directly,
                // so inspect the values under `settings_config`.
                // Check `auth_mode` in `settings_config`.
                if let Some(auth_mode) = provider
                    .settings_config
                    .get("auth_mode")
                    .and_then(|v| v.as_str())
                {
                    if auth_mode == "bearer_only" {
                        return ProviderType::ClaudeAuth;
                    }
                }
                // Check `auth_mode` in `env`.
                if let Some(env) = provider.settings_config.get("env") {
                    if let Some(auth_mode) = env.get("AUTH_MODE").and_then(|v| v.as_str()) {
                        if auth_mode == "bearer_only" {
                            return ProviderType::ClaudeAuth;
                        }
                    }
                }
                ProviderType::Claude
            }
            AppType::Codex => ProviderType::Codex,
            AppType::Gemini => {
                // Detect CLI mode (OAuth).
                let adapter = GeminiAdapter::new();
                if let Some(auth) = adapter.extract_auth(provider) {
                    let key = &auth.api_key;
                    // OAuth access tokens start with `ya29.`.
                    if key.starts_with("ya29.") {
                        return ProviderType::GeminiCli;
                    }
                    // JSON-formatted OAuth credentials.
                    if key.starts_with('{') {
                        return ProviderType::GeminiCli;
                    }
                }
                ProviderType::Gemini
            }
            AppType::OpenCode | AppType::OpenClaw | AppType::Hermes => {
                // These apps don't support proxy, fallback to Codex-like type
                ProviderType::Codex
            }
        }
    }

    /// Convert to a string representation.
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderType::Claude => "claude",
            ProviderType::ClaudeAuth => "claude_auth",
            ProviderType::Codex => "codex",
            ProviderType::Gemini => "gemini",
            ProviderType::GeminiCli => "gemini_cli",
            ProviderType::OpenRouter => "openrouter",
            ProviderType::GitHubCopilot => "github_copilot",
            ProviderType::CodexOAuth => "codex_oauth",
        }
    }
}

impl std::fmt::Display for ProviderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for ProviderType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "claude" => Ok(ProviderType::Claude),
            "claude_auth" | "claude-auth" => Ok(ProviderType::ClaudeAuth),
            "codex" => Ok(ProviderType::Codex),
            "gemini" => Ok(ProviderType::Gemini),
            "gemini_cli" | "gemini-cli" => Ok(ProviderType::GeminiCli),
            "openrouter" => Ok(ProviderType::OpenRouter),
            "github_copilot" | "github-copilot" | "githubcopilot" => {
                Ok(ProviderType::GitHubCopilot)
            }
            "codex_oauth" | "codex-oauth" | "codexoauth" => Ok(ProviderType::CodexOAuth),
            _ => Err(format!("Invalid provider type: {s}")),
        }
    }
}

/// Get the adapter for the given `AppType`.
pub fn get_adapter(app_type: &AppType) -> Box<dyn ProviderAdapter> {
    match app_type {
        AppType::Claude | AppType::ClaudeDesktop => Box::new(ClaudeAdapter::new()),
        AppType::Codex => Box::new(CodexAdapter::new()),
        AppType::Gemini => Box::new(GeminiAdapter::new()),
        AppType::OpenCode | AppType::OpenClaw | AppType::Hermes => {
            // These apps don't support proxy, fallback to Codex adapter
            Box::new(CodexAdapter::new())
        }
    }
}

/// Get the adapter for the given `ProviderType`.
#[allow(dead_code)]
pub fn get_adapter_for_provider_type(provider_type: &ProviderType) -> Box<dyn ProviderAdapter> {
    match provider_type {
        ProviderType::Claude
        | ProviderType::ClaudeAuth
        | ProviderType::OpenRouter
        | ProviderType::GitHubCopilot
        | ProviderType::CodexOAuth => Box::new(ClaudeAdapter::new()),
        ProviderType::Codex => Box::new(CodexAdapter::new()),
        ProviderType::Gemini | ProviderType::GeminiCli => Box::new(GeminiAdapter::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn create_provider(config: serde_json::Value) -> Provider {
        Provider {
            id: "test".to_string(),
            name: "Test Provider".to_string(),
            settings_config: config,
            website_url: None,
            category: None,
            created_at: None,
            sort_index: None,
            notes: None,
            meta: None,
            icon: None,
            icon_color: None,
            in_failover_queue: false,
        }
    }

    #[test]
    fn test_provider_type_needs_transform() {
        assert!(!ProviderType::Claude.needs_transform());
        assert!(!ProviderType::ClaudeAuth.needs_transform());
        assert!(!ProviderType::Codex.needs_transform());
        assert!(!ProviderType::Gemini.needs_transform());
        assert!(!ProviderType::GeminiCli.needs_transform());
        assert!(!ProviderType::OpenRouter.needs_transform());
        assert!(ProviderType::GitHubCopilot.needs_transform());
    }

    #[test]
    fn test_provider_type_default_endpoint() {
        assert_eq!(
            ProviderType::Claude.default_endpoint(),
            "https://api.anthropic.com"
        );
        assert_eq!(
            ProviderType::ClaudeAuth.default_endpoint(),
            "https://api.anthropic.com"
        );
        assert_eq!(
            ProviderType::Codex.default_endpoint(),
            "https://api.openai.com"
        );
        assert_eq!(
            ProviderType::Gemini.default_endpoint(),
            "https://generativelanguage.googleapis.com"
        );
        assert_eq!(
            ProviderType::GeminiCli.default_endpoint(),
            "https://generativelanguage.googleapis.com"
        );
        assert_eq!(
            ProviderType::OpenRouter.default_endpoint(),
            "https://openrouter.ai/api"
        );
        assert_eq!(
            ProviderType::GitHubCopilot.default_endpoint(),
            "https://api.githubcopilot.com"
        );
    }

    #[test]
    fn test_provider_type_from_str() {
        assert_eq!(
            "claude".parse::<ProviderType>().unwrap(),
            ProviderType::Claude
        );
        assert_eq!(
            "claude_auth".parse::<ProviderType>().unwrap(),
            ProviderType::ClaudeAuth
        );
        assert_eq!(
            "claude-auth".parse::<ProviderType>().unwrap(),
            ProviderType::ClaudeAuth
        );
        assert_eq!(
            "codex".parse::<ProviderType>().unwrap(),
            ProviderType::Codex
        );
        assert_eq!(
            "gemini".parse::<ProviderType>().unwrap(),
            ProviderType::Gemini
        );
        assert_eq!(
            "gemini_cli".parse::<ProviderType>().unwrap(),
            ProviderType::GeminiCli
        );
        assert_eq!(
            "gemini-cli".parse::<ProviderType>().unwrap(),
            ProviderType::GeminiCli
        );
        assert_eq!(
            "openrouter".parse::<ProviderType>().unwrap(),
            ProviderType::OpenRouter
        );
        assert_eq!(
            "github_copilot".parse::<ProviderType>().unwrap(),
            ProviderType::GitHubCopilot
        );
        assert_eq!(
            "github-copilot".parse::<ProviderType>().unwrap(),
            ProviderType::GitHubCopilot
        );
        assert_eq!(
            "githubcopilot".parse::<ProviderType>().unwrap(),
            ProviderType::GitHubCopilot
        );
        assert!("invalid".parse::<ProviderType>().is_err());
    }

    #[test]
    fn test_provider_type_as_str() {
        assert_eq!(ProviderType::Claude.as_str(), "claude");
        assert_eq!(ProviderType::ClaudeAuth.as_str(), "claude_auth");
        assert_eq!(ProviderType::Codex.as_str(), "codex");
        assert_eq!(ProviderType::Gemini.as_str(), "gemini");
        assert_eq!(ProviderType::GeminiCli.as_str(), "gemini_cli");
        assert_eq!(ProviderType::OpenRouter.as_str(), "openrouter");
        assert_eq!(ProviderType::GitHubCopilot.as_str(), "github_copilot");
    }

    #[test]
    fn test_provider_type_serde() {
        // Test serialization
        let claude = ProviderType::Claude;
        let serialized = serde_json::to_string(&claude).unwrap();
        assert_eq!(serialized, "\"claude\"");

        let claude_auth = ProviderType::ClaudeAuth;
        let serialized = serde_json::to_string(&claude_auth).unwrap();
        assert_eq!(serialized, "\"claude_auth\"");

        // Test deserialization
        let deserialized: ProviderType = serde_json::from_str("\"claude\"").unwrap();
        assert_eq!(deserialized, ProviderType::Claude);

        let deserialized: ProviderType = serde_json::from_str("\"gemini_cli\"").unwrap();
        assert_eq!(deserialized, ProviderType::GeminiCli);
    }

    #[test]
    fn test_from_app_type_claude_direct() {
        let provider = create_provider(json!({
            "env": {
                "ANTHROPIC_BASE_URL": "https://api.anthropic.com",
                "ANTHROPIC_AUTH_TOKEN": "sk-ant-test"
            }
        }));

        let provider_type = ProviderType::from_app_type_and_config(&AppType::Claude, &provider);
        assert_eq!(provider_type, ProviderType::Claude);
    }

    #[test]
    fn test_from_app_type_claude_openrouter() {
        let provider = create_provider(json!({
            "env": {
                "ANTHROPIC_BASE_URL": "https://openrouter.ai/api",
                "OPENROUTER_API_KEY": "sk-or-test"
            }
        }));

        let provider_type = ProviderType::from_app_type_and_config(&AppType::Claude, &provider);
        assert_eq!(provider_type, ProviderType::OpenRouter);
    }

    #[test]
    fn test_from_app_type_claude_auth() {
        let provider = create_provider(json!({
            "env": {
                "ANTHROPIC_BASE_URL": "https://some-proxy.com",
                "ANTHROPIC_AUTH_TOKEN": "sk-test"
            },
            "auth_mode": "bearer_only"
        }));

        let provider_type = ProviderType::from_app_type_and_config(&AppType::Claude, &provider);
        assert_eq!(provider_type, ProviderType::ClaudeAuth);
    }

    #[test]
    fn test_from_app_type_codex() {
        let provider = create_provider(json!({
            "env": {
                "OPENAI_API_KEY": "sk-test"
            }
        }));

        let provider_type = ProviderType::from_app_type_and_config(&AppType::Codex, &provider);
        assert_eq!(provider_type, ProviderType::Codex);
    }

    #[test]
    fn test_from_app_type_gemini_api_key() {
        let provider = create_provider(json!({
            "env": {
                "GEMINI_API_KEY": "AIza-test-key"
            }
        }));

        let provider_type = ProviderType::from_app_type_and_config(&AppType::Gemini, &provider);
        assert_eq!(provider_type, ProviderType::Gemini);
    }

    #[test]
    fn test_from_app_type_gemini_cli_oauth() {
        let provider = create_provider(json!({
            "env": {
                "GEMINI_API_KEY": "ya29.test-access-token"
            }
        }));

        let provider_type = ProviderType::from_app_type_and_config(&AppType::Gemini, &provider);
        assert_eq!(provider_type, ProviderType::GeminiCli);
    }

    #[test]
    fn test_from_app_type_gemini_cli_json() {
        let provider = create_provider(json!({
            "env": {
                "GEMINI_API_KEY": "{\"access_token\":\"ya29.test\",\"refresh_token\":\"1//test\"}"
            }
        }));

        let provider_type = ProviderType::from_app_type_and_config(&AppType::Gemini, &provider);
        assert_eq!(provider_type, ProviderType::GeminiCli);
    }

    #[test]
    fn test_get_adapter_for_provider_type() {
        let adapter = get_adapter_for_provider_type(&ProviderType::Claude);
        assert_eq!(adapter.name(), "Claude");

        let adapter = get_adapter_for_provider_type(&ProviderType::ClaudeAuth);
        assert_eq!(adapter.name(), "Claude");

        let adapter = get_adapter_for_provider_type(&ProviderType::OpenRouter);
        assert_eq!(adapter.name(), "Claude");

        let adapter = get_adapter_for_provider_type(&ProviderType::GitHubCopilot);
        assert_eq!(adapter.name(), "Claude");

        let adapter = get_adapter_for_provider_type(&ProviderType::Codex);
        assert_eq!(adapter.name(), "Codex");

        let adapter = get_adapter_for_provider_type(&ProviderType::Gemini);
        assert_eq!(adapter.name(), "Gemini");

        let adapter = get_adapter_for_provider_type(&ProviderType::GeminiCli);
        assert_eq!(adapter.name(), "Gemini");
    }
}
