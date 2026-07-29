//! Codex OAuth Tauri Commands
//!
//! Tauri commands for OpenAI ChatGPT Plus/Pro OAuth authentication.
//!
//! Most authentication commands are exposed through the generic `auth_*` commands; see
//! `commands::auth`. This module defines the state wrapper and Codex OAuth-specific commands
//! for querying subscription quotas and available models.

use crate::proxy::providers::codex_oauth_auth::CodexOAuthManager;
use crate::services::model_fetch::FetchedModel;
use crate::services::subscription::{query_codex_quota, CredentialStatus, SubscriptionQuota};
use std::sync::Arc;
use tauri::State;
use tokio::sync::RwLock;

/// Codex OAuth authentication state.
pub struct CodexOAuthState(pub Arc<RwLock<CodexOAuthManager>>);

/// Queries a Codex OAuth (ChatGPT Plus/Pro) subscription quota.
///
/// - Falls back to the `CodexOAuthManager` default account when `account_id` is absent.
/// - Returns `not_found` when there are no accounts, allowing `SubscriptionQuotaView` to stay hidden.
/// - Reuses `services::subscription::query_codex_quota`, keeping the wham/usage endpoint
///   protocol identical to the Codex CLI path.
#[tauri::command(rename_all = "camelCase")]
pub async fn get_codex_oauth_quota(
    account_id: Option<String>,
    state: State<'_, CodexOAuthState>,
) -> Result<SubscriptionQuota, String> {
    let manager = state.0.read().await;

    // Resolve the account ID: explicit account > default account > not_found.
    let resolved = match account_id {
        Some(id) => Some(id),
        None => manager.default_account_id().await,
    };
    let Some(id) = resolved else {
        return Ok(SubscriptionQuota::not_found("codex_oauth"));
    };

    // Obtain the access token, refreshing it automatically when necessary.
    let token = match manager.get_valid_token_for_account(&id).await {
        Ok(t) => t,
        Err(e) => {
            return Ok(SubscriptionQuota::error(
                "codex_oauth",
                CredentialStatus::Expired,
                format!("Codex OAuth token unavailable: {e}"),
            ));
        }
    };

    Ok(query_codex_quota(
        &token,
        Some(&id),
        "codex_oauth",
        "Codex OAuth access token expired or rejected. Please re-login via Nexus Composer.",
    )
    .await)
}

/// Returns the models available to Codex OAuth (ChatGPT Plus/Pro).
///
/// The ChatGPT Codex gateway uses `chatgpt.com/backend-api/codex/*`, not the OpenAI-compatible
/// `/v1/models` endpoint. This reuses a managed OAuth account's access token to query the model
/// list exposed directly by the Codex backend.
#[tauri::command(rename_all = "camelCase")]
pub async fn get_codex_oauth_models(
    account_id: Option<String>,
    state: State<'_, CodexOAuthState>,
) -> Result<Vec<FetchedModel>, String> {
    let manager = state.0.read().await;
    let resolved = match account_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        Some(id) => Some(id.to_string()),
        None => manager.default_account_id().await,
    };
    let Some(id) = resolved else {
        return Err("No ChatGPT account available".to_string());
    };

    let token = manager
        .get_valid_token_for_account(&id)
        .await
        .map_err(|e| format!("Codex OAuth token unavailable: {e}"))?;

    crate::services::codex_oauth_models::fetch_models_with_token(&token, &id).await
}
