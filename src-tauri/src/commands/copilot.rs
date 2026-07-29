//! GitHub Copilot Tauri Commands
//!
//! Provides Copilot OAuth commands with multi-account support.

use crate::proxy::providers::copilot_auth::{
    CopilotAuthManager, CopilotAuthStatus, CopilotModel, CopilotUsageResponse, GitHubAccount,
    GitHubDeviceCodeResponse,
};
use std::sync::Arc;
use tauri::State;
use tokio::sync::RwLock;

/// Shared Copilot authentication state.
pub struct CopilotAuthState(pub Arc<RwLock<CopilotAuthManager>>);

// ==================== Device Code Flow ====================

/// Start the device code flow.
///
/// Returns device and user codes for OAuth authentication.
#[tauri::command]
pub async fn copilot_start_device_flow(
    github_domain: Option<String>,
    state: State<'_, CopilotAuthState>,
) -> Result<GitHubDeviceCodeResponse, String> {
    let auth_manager = state.0.read().await;
    auth_manager
        .start_device_flow(github_domain.as_deref())
        .await
        .map_err(|e| e.to_string())
}

/// Poll for an OAuth token using the backward-compatible API.
///
/// Polls GitHub with the device code until the user authorizes access.
/// Returns true after authorization or false while pending.
#[tauri::command(rename_all = "camelCase")]
pub async fn copilot_poll_for_auth(
    device_code: String,
    github_domain: Option<String>,
    state: State<'_, CopilotAuthState>,
) -> Result<bool, String> {
    let auth_manager = state.0.write().await;
    match auth_manager
        .poll_for_token(&device_code, github_domain.as_deref())
        .await
    {
        Ok(Some(_account)) => {
            log::info!("[CopilotAuth] User authorized access");
            Ok(true)
        }
        Ok(None) => Ok(false),
        Err(crate::proxy::providers::copilot_auth::CopilotAuthError::AuthorizationPending) => {
            Ok(false)
        }
        Err(e) => {
            log::error!("[CopilotAuth] Polling failed: {e}");
            Err(e.to_string())
        }
    }
}

/// Poll for an OAuth token using the multi-account API.
///
/// Returns the newly added account after authorization.
#[tauri::command(rename_all = "camelCase")]
pub async fn copilot_poll_for_account(
    device_code: String,
    github_domain: Option<String>,
    state: State<'_, CopilotAuthState>,
) -> Result<Option<GitHubAccount>, String> {
    let auth_manager = state.0.write().await;
    match auth_manager
        .poll_for_token(&device_code, github_domain.as_deref())
        .await
    {
        Ok(account) => Ok(account),
        Err(crate::proxy::providers::copilot_auth::CopilotAuthError::AuthorizationPending) => {
            Ok(None)
        }
        Err(e) => {
            log::error!("[CopilotAuth] Polling failed: {e}");
            Err(e.to_string())
        }
    }
}

// ==================== Multi-Account Management ====================

/// Return all authenticated accounts.
#[tauri::command]
pub async fn copilot_list_accounts(
    state: State<'_, CopilotAuthState>,
) -> Result<Vec<GitHubAccount>, String> {
    let auth_manager = state.0.read().await;
    Ok(auth_manager.list_accounts().await)
}

/// Remove an account.
#[tauri::command(rename_all = "camelCase")]
pub async fn copilot_remove_account(
    account_id: String,
    state: State<'_, CopilotAuthState>,
) -> Result<(), String> {
    let auth_manager = state.0.write().await;
    auth_manager
        .remove_account(&account_id)
        .await
        .map_err(|e| e.to_string())
}

/// Set the default account.
#[tauri::command(rename_all = "camelCase")]
pub async fn copilot_set_default_account(
    account_id: String,
    state: State<'_, CopilotAuthState>,
) -> Result<(), String> {
    let auth_manager = state.0.write().await;
    auth_manager
        .set_default_account(&account_id)
        .await
        .map_err(|e| e.to_string())
}

// ==================== Status Queries ====================

/// Return authentication status for all accounts.
#[tauri::command]
pub async fn copilot_get_auth_status(
    state: State<'_, CopilotAuthState>,
) -> Result<CopilotAuthStatus, String> {
    let auth_manager = state.0.read().await;
    Ok(auth_manager.get_status().await)
}

/// Return whether any account is authenticated.
#[tauri::command]
pub async fn copilot_is_authenticated(state: State<'_, CopilotAuthState>) -> Result<bool, String> {
    let auth_manager = state.0.read().await;
    Ok(auth_manager.is_authenticated().await)
}

/// Clear all Copilot authentication.
#[tauri::command]
pub async fn copilot_logout(state: State<'_, CopilotAuthState>) -> Result<(), String> {
    let auth_manager = state.0.write().await;
    auth_manager.clear_auth().await.map_err(|e| e.to_string())
}

// ==================== Token Retrieval ====================

/// Return a valid Copilot token using the first account for compatibility.
///
/// Used internally for proxy requests.
#[tauri::command]
pub async fn copilot_get_token(state: State<'_, CopilotAuthState>) -> Result<String, String> {
    let auth_manager = state.0.read().await;
    auth_manager
        .get_valid_token()
        .await
        .map_err(|e| e.to_string())
}

/// Return a valid Copilot token for an account.
#[tauri::command(rename_all = "camelCase")]
pub async fn copilot_get_token_for_account(
    account_id: String,
    state: State<'_, CopilotAuthState>,
) -> Result<String, String> {
    let auth_manager = state.0.read().await;
    auth_manager
        .get_valid_token_for_account(&account_id)
        .await
        .map_err(|e| e.to_string())
}

// ==================== Models and Usage ====================

/// Return Copilot models using the first account for compatibility.
#[tauri::command]
pub async fn copilot_get_models(
    state: State<'_, CopilotAuthState>,
) -> Result<Vec<CopilotModel>, String> {
    let auth_manager = state.0.read().await;
    auth_manager.fetch_models().await.map_err(|e| e.to_string())
}

/// Return Copilot models for an account.
#[tauri::command(rename_all = "camelCase")]
pub async fn copilot_get_models_for_account(
    account_id: String,
    state: State<'_, CopilotAuthState>,
) -> Result<Vec<CopilotModel>, String> {
    let auth_manager = state.0.read().await;
    auth_manager
        .fetch_models_for_account(&account_id)
        .await
        .map_err(|e| e.to_string())
}

/// Return Copilot usage using the first account for compatibility.
#[tauri::command]
pub async fn copilot_get_usage(
    state: State<'_, CopilotAuthState>,
) -> Result<CopilotUsageResponse, String> {
    let auth_manager = state.0.read().await;
    auth_manager.fetch_usage().await.map_err(|e| e.to_string())
}

/// Return Copilot usage for an account.
#[tauri::command(rename_all = "camelCase")]
pub async fn copilot_get_usage_for_account(
    account_id: String,
    state: State<'_, CopilotAuthState>,
) -> Result<CopilotUsageResponse, String> {
    let auth_manager = state.0.read().await;
    auth_manager
        .fetch_usage_for_account(&account_id)
        .await
        .map_err(|e| e.to_string())
}
