//! Tauri commands for the proxy service
//!
//! Provides APIs for the frontend.

use crate::error::AppError;
use crate::proxy::types::*;
use crate::proxy::{CircuitBreakerConfig, CircuitBreakerStats};
use crate::store::AppState;

/// Start the proxy server without taking over live configuration.
pub(crate) async fn start_proxy_server_inner(state: &AppState) -> Result<ProxyServerInfo, String> {
    let started_here = !state.proxy_service.is_running().await;
    let info = state.proxy_service.start().await?;
    if let Err(error) = crate::services::proxy::sync_current_claude_desktop_proxy_profile(state) {
        if started_here {
            let rollback = state.proxy_service.stop().await;
            return Err(match rollback {
                Ok(()) => format!("Failed to apply the Claude Desktop proxy profile: {error}"),
                Err(rollback_error) => format!(
                    "Failed to apply the Claude Desktop proxy profile: {error}; \
                     failed to stop the proxy after that error: {rollback_error}"
                ),
            });
        }
        return Err(format!(
            "Failed to apply the Claude Desktop proxy profile: {error}"
        ));
    }
    Ok(info)
}

#[tauri::command]
pub async fn start_proxy_server(
    state: tauri::State<'_, AppState>,
) -> Result<ProxyServerInfo, String> {
    start_proxy_server_inner(state.inner()).await
}

/// Stop the proxy server without restoring or clearing live takeover state.
#[tauri::command]
pub async fn stop_proxy_server(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let takeover = state.proxy_service.get_takeover_status().await?;
    if takeover.claude
        || takeover.codex
        || takeover.gemini
        || takeover.opencode
        || takeover.openclaw
    {
        return Err(
            "Some applications are still under proxy takeover. Disable takeover in settings before stopping local routing."
                .to_string(),
        );
    }

    state.proxy_service.stop().await
}

/// Stop the proxy server and restore live configuration.
#[tauri::command]
pub async fn stop_proxy_with_restore(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.proxy_service.stop_with_restore().await
}

/// Return the takeover status for each application.
#[tauri::command]
pub async fn get_proxy_takeover_status(
    state: tauri::State<'_, AppState>,
) -> Result<ProxyTakeoverStatus, String> {
    state.proxy_service.get_takeover_status().await
}

/// Enable or disable takeover for an application.
#[tauri::command]
pub async fn set_proxy_takeover_for_app(
    state: tauri::State<'_, AppState>,
    app_type: String,
    enabled: bool,
) -> Result<(), String> {
    state
        .proxy_service
        .set_takeover_for_app(&app_type, enabled)
        .await
}

/// Return the proxy server status.
#[tauri::command]
pub async fn get_proxy_status(state: tauri::State<'_, AppState>) -> Result<ProxyStatus, String> {
    state.proxy_service.get_status().await
}

/// Return the proxy configuration.
#[tauri::command]
pub async fn get_proxy_config(state: tauri::State<'_, AppState>) -> Result<ProxyConfig, String> {
    state.proxy_service.get_config().await
}

/// Update the proxy configuration.
#[tauri::command]
pub async fn update_proxy_config(
    state: tauri::State<'_, AppState>,
    config: ProxyConfig,
) -> Result<(), String> {
    state.proxy_service.update_config(&config).await
}

// ==================== Global & Per-App Config ====================

/// Return the global proxy configuration.
///
/// Includes shared proxy, listen address, port, and logging settings.
#[tauri::command]
pub async fn get_global_proxy_config(
    state: tauri::State<'_, AppState>,
) -> Result<GlobalProxyConfig, String> {
    let db = &state.db;
    db.get_global_proxy_config()
        .await
        .map_err(|e| e.to_string())
}

/// Update the global proxy configuration.
///
/// Updates the shared fields for the claude, codex, and gemini rows.
#[tauri::command]
pub async fn update_global_proxy_config(
    state: tauri::State<'_, AppState>,
    config: GlobalProxyConfig,
) -> Result<(), String> {
    let db = &state.db;
    db.update_global_proxy_config(config)
        .await
        .map_err(|e| e.to_string())
}

/// Return the proxy configuration for an application.
///
/// Includes enabled state, automatic failover, timeouts, and circuit breaker settings.
#[tauri::command]
pub async fn get_proxy_config_for_app(
    state: tauri::State<'_, AppState>,
    app_type: String,
) -> Result<AppProxyConfig, String> {
    let db = &state.db;
    db.get_proxy_config_for_app(&app_type)
        .await
        .map_err(|e| e.to_string())
}

/// Update the proxy configuration for an application.
///
/// Updates enabled state, automatic failover, timeouts, and circuit breaker settings.
#[tauri::command]
pub async fn update_proxy_config_for_app(
    state: tauri::State<'_, AppState>,
    config: AppProxyConfig,
) -> Result<(), String> {
    let db = &state.db;
    let app_type = config.app_type.clone();
    let circuit_config = CircuitBreakerConfig::from(&config);

    db.update_proxy_config_for_app(config)
        .await
        .map_err(|e| e.to_string())?;

    state
        .proxy_service
        .update_circuit_breaker_config_for_app(&app_type, circuit_config)
        .await
}

async fn get_default_cost_multiplier_internal(
    state: &AppState,
    app_type: &str,
) -> Result<String, AppError> {
    let db = &state.db;
    db.get_default_cost_multiplier(app_type).await
}

#[cfg_attr(not(feature = "test-hooks"), doc(hidden))]
pub async fn get_default_cost_multiplier_test_hook(
    state: &AppState,
    app_type: &str,
) -> Result<String, AppError> {
    get_default_cost_multiplier_internal(state, app_type).await
}

/// Return the default cost multiplier.
#[tauri::command]
pub async fn get_default_cost_multiplier(
    state: tauri::State<'_, AppState>,
    app_type: String,
) -> Result<String, String> {
    get_default_cost_multiplier_internal(&state, &app_type)
        .await
        .map_err(|e| e.to_string())
}

async fn set_default_cost_multiplier_internal(
    state: &AppState,
    app_type: &str,
    value: &str,
) -> Result<(), AppError> {
    let db = &state.db;
    db.set_default_cost_multiplier(app_type, value).await
}

#[cfg_attr(not(feature = "test-hooks"), doc(hidden))]
pub async fn set_default_cost_multiplier_test_hook(
    state: &AppState,
    app_type: &str,
    value: &str,
) -> Result<(), AppError> {
    set_default_cost_multiplier_internal(state, app_type, value).await
}

/// Set the default cost multiplier.
#[tauri::command]
pub async fn set_default_cost_multiplier(
    state: tauri::State<'_, AppState>,
    app_type: String,
    value: String,
) -> Result<(), String> {
    set_default_cost_multiplier_internal(&state, &app_type, &value)
        .await
        .map_err(|e| e.to_string())
}

async fn get_pricing_model_source_internal(
    state: &AppState,
    app_type: &str,
) -> Result<String, AppError> {
    let db = &state.db;
    db.get_pricing_model_source(app_type).await
}

#[cfg_attr(not(feature = "test-hooks"), doc(hidden))]
pub async fn get_pricing_model_source_test_hook(
    state: &AppState,
    app_type: &str,
) -> Result<String, AppError> {
    get_pricing_model_source_internal(state, app_type).await
}

/// Return the pricing model source.
#[tauri::command]
pub async fn get_pricing_model_source(
    state: tauri::State<'_, AppState>,
    app_type: String,
) -> Result<String, String> {
    get_pricing_model_source_internal(&state, &app_type)
        .await
        .map_err(|e| e.to_string())
}

async fn set_pricing_model_source_internal(
    state: &AppState,
    app_type: &str,
    value: &str,
) -> Result<(), AppError> {
    let db = &state.db;
    db.set_pricing_model_source(app_type, value).await
}

#[cfg_attr(not(feature = "test-hooks"), doc(hidden))]
pub async fn set_pricing_model_source_test_hook(
    state: &AppState,
    app_type: &str,
    value: &str,
) -> Result<(), AppError> {
    set_pricing_model_source_internal(state, app_type, value).await
}

/// Set the pricing model source.
#[tauri::command]
pub async fn set_pricing_model_source(
    state: tauri::State<'_, AppState>,
    app_type: String,
    value: String,
) -> Result<(), String> {
    set_pricing_model_source_internal(&state, &app_type, &value)
        .await
        .map_err(|e| e.to_string())
}

/// Return whether the proxy server is running.
#[tauri::command]
pub async fn is_proxy_running(state: tauri::State<'_, AppState>) -> Result<bool, String> {
    Ok(state.proxy_service.is_running().await)
}

/// Return whether live takeover is active.
#[tauri::command]
pub async fn is_live_takeover_active(state: tauri::State<'_, AppState>) -> Result<bool, String> {
    state.proxy_service.is_takeover_active().await
}

/// Hot-switch providers in proxy mode.
#[tauri::command]
pub async fn switch_proxy_provider(
    state: tauri::State<'_, AppState>,
    app_type: String,
    provider_id: String,
) -> Result<(), String> {
    // Block official providers during proxy takeover
    let provider = state
        .db
        .get_provider_by_id(&provider_id, &app_type)
        .map_err(|e| format!("Failed to read provider: {e}"))?
        .ok_or_else(|| format!("Provider not found: {provider_id}"))?;
    if provider.category.as_deref() == Some("official") {
        return Err("Cannot switch to an official provider during proxy takeover".to_string());
    }

    state
        .proxy_service
        .switch_proxy_target(&app_type, &provider_id)
        .await
}

// ==================== Failover Commands ====================

/// Return provider health status.
#[tauri::command]
pub async fn get_provider_health(
    state: tauri::State<'_, AppState>,
    provider_id: String,
    app_type: String,
) -> Result<ProviderHealth, String> {
    let db = &state.db;
    db.get_provider_health(&provider_id, &app_type)
        .await
        .map_err(|e| e.to_string())
}

/// Reset the circuit breaker.
///
/// After reset, checks whether to return to a higher-priority provider:
/// 1. Check whether automatic failover is enabled.
/// 2. Switch when the recovered provider has a lower queue_order.
#[tauri::command]
pub async fn reset_circuit_breaker(
    app_handle: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    provider_id: String,
    app_type: String,
) -> Result<(), String> {
    // 1. Reset health state in the database.
    let db = &state.db;
    db.update_provider_health(&provider_id, &app_type, true, None)
        .await
        .map_err(|e| e.to_string())?;

    // 2. Reset in-memory circuit breaker state when the proxy is running.
    state
        .proxy_service
        .reset_provider_circuit_breaker(&provider_id, &app_type)
        .await?;

    // 3. Check proxy_config to determine whether to return to a higher-priority provider.
    // Run only when takeover and automatic failover are enabled for the application.
    let (app_enabled, auto_failover_enabled) = match db.get_proxy_config_for_app(&app_type).await {
        Ok(config) => (config.enabled, config.auto_failover_enabled),
        Err(e) => {
            log::error!("[{app_type}] Failed to read proxy_config: {e}, defaulting to disabled");
            (false, false)
        }
    };

    if app_enabled && auto_failover_enabled && state.proxy_service.is_running().await {
        // Get the current provider ID.
        let current_id = db
            .get_current_provider(&app_type)
            .map_err(|e| e.to_string())?;

        if let Some(current_id) = current_id {
            // Get the failover queue.
            let queue = db
                .get_failover_queue(&app_type)
                .map_err(|e| e.to_string())?;

            // Find the restored and current providers by sort_index.
            let restored_order = queue
                .iter()
                .find(|item| item.provider_id == provider_id)
                .and_then(|item| item.sort_index);

            let current_order = queue
                .iter()
                .find(|item| item.provider_id == current_id)
                .and_then(|item| item.sort_index);

            // Switch when the restored provider has a lower sort_index.
            if let (Some(restored), Some(current)) = (restored_order, current_order) {
                if restored < current {
                    log::info!(
                        "[Recovery] Provider {provider_id} recovered with higher priority (P{restored} vs P{current}); switching automatically"
                    );

                    // Get the provider name for logs and events.
                    let provider_name = db
                        .get_all_providers(&app_type)
                        .ok()
                        .and_then(|providers| providers.get(&provider_id).map(|p| p.name.clone()))
                        .unwrap_or_else(|| provider_id.clone());

                    // Create the failover switch manager and perform the switch.
                    let switch_manager =
                        crate::proxy::failover_switch::FailoverSwitchManager::new(db.clone());
                    if let Err(e) = switch_manager
                        .try_switch(Some(&app_handle), &app_type, &provider_id, &provider_name)
                        .await
                    {
                        log::error!("[Recovery] Automatic switch failed: {e}");
                    }
                }
            }
        }
    }

    Ok(())
}

/// Return the circuit breaker configuration.
#[tauri::command]
pub async fn get_circuit_breaker_config(
    state: tauri::State<'_, AppState>,
) -> Result<CircuitBreakerConfig, String> {
    let db = &state.db;
    db.get_circuit_breaker_config()
        .await
        .map_err(|e| e.to_string())
}

/// Update the circuit breaker configuration.
#[tauri::command]
pub async fn update_circuit_breaker_config(
    state: tauri::State<'_, AppState>,
    config: CircuitBreakerConfig,
) -> Result<(), String> {
    let db = &state.db;

    // 1. Update the database configuration.
    db.update_circuit_breaker_config(&config)
        .await
        .map_err(|e| e.to_string())?;

    // 2. Hot-update the in-memory circuit breaker configuration.
    state
        .proxy_service
        .update_circuit_breaker_configs(config)
        .await?;

    Ok(())
}

/// Return circuit breaker statistics when the proxy server is running.
#[tauri::command]
pub async fn get_circuit_breaker_stats(
    state: tauri::State<'_, AppState>,
    provider_id: String,
    app_type: String,
) -> Result<Option<CircuitBreakerStats>, String> {
    // This requires access to the running proxy server's in-memory state.
    // Return None until ProxyService exposes that state.
    let _ = (state, provider_id, app_type);
    Ok(None)
}
