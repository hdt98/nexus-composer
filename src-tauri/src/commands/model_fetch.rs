//! Model-list retrieval commands.
//!
//! Provides Tauri commands for fetching available models in provider forms.

use crate::services::model_fetch::{self, FetchedModel};

/// Fetches the available models for a provider.
///
/// Uses an OpenAI-compatible GET /v1/models endpoint. An explicit `models_url`
/// takes precedence; otherwise candidates are generated from baseURL, including
/// a fallback with any Anthropic-compatible suffix removed, and tried in order.
#[tauri::command(rename_all = "camelCase")]
pub async fn fetch_models_for_config(
    base_url: String,
    api_key: String,
    is_full_url: Option<bool>,
    models_url: Option<String>,
    custom_user_agent: Option<String>,
) -> Result<Vec<FetchedModel>, String> {
    // Share parse_custom_user_agent with forwarding and diagnostics. Ignore an invalid UA
    // instead of blocking model retrieval.
    let user_agent = crate::provider::parse_custom_user_agent(custom_user_agent.as_deref())
        .ok()
        .flatten();
    model_fetch::fetch_models(
        &base_url,
        &api_key,
        is_full_url.unwrap_or(false),
        models_url.as_deref(),
        user_agent,
    )
    .await
}
