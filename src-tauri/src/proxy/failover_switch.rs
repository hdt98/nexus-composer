//! Failover switch module
//!
//! Handles provider switching after successful failover, including:
//! - deduplication so multiple requests do not trigger the same switch
//! - tray menu updates
//! - frontend event emission

use crate::database::Database;
use crate::error::AppError;
use std::collections::HashSet;
use std::sync::Arc;
use tauri::{Emitter, Manager};
use tokio::sync::RwLock;

/// Failover switch manager
///
/// Handles post-failover provider switching so the UI reflects the active provider.
#[derive(Clone)]
pub struct FailoverSwitchManager {
    /// Switches currently in progress. Key: "app_type:provider_id".
    pending_switches: Arc<RwLock<HashSet<String>>>,
    db: Arc<Database>,
}

impl FailoverSwitchManager {
    pub fn new(db: Arc<Database>) -> Self {
        Self {
            pending_switches: Arc::new(RwLock::new(HashSet::new())),
            db,
        }
    }

    /// Attempts to perform a failover switch.
    ///
    /// Skips the switch if the same target is already in progress.
    ///
    /// # Returns
    /// - `Ok(true)` - the switch ran successfully
    /// - `Ok(false)` - the same switch was already in progress and was skipped
    /// - `Err(e)` - the switch flow failed
    pub async fn try_switch(
        &self,
        app_handle: Option<&tauri::AppHandle>,
        app_type: &str,
        provider_id: &str,
        provider_name: &str,
    ) -> Result<bool, AppError> {
        let switch_key = format!("{app_type}:{provider_id}");

        // Deduplicate concurrent attempts for the same switch target.
        {
            let mut pending = self.pending_switches.write().await;
            if pending.contains(&switch_key) {
                log::debug!(
                    "[Failover] switch already in progress, skipping: {app_type} -> {provider_id}"
                );
                return Ok(false);
            }
            pending.insert(switch_key.clone());
        }

        // Execute the switch and always clear the pending marker afterward.
        let result = self
            .do_switch(app_handle, app_type, provider_id, provider_name)
            .await;

        // Clear the pending marker.
        {
            let mut pending = self.pending_switches.write().await;
            pending.remove(&switch_key);
        }

        result
    }

    async fn do_switch(
        &self,
        app_handle: Option<&tauri::AppHandle>,
        app_type: &str,
        provider_id: &str,
        provider_name: &str,
    ) -> Result<bool, AppError> {
        // Check whether this app is currently taken over by the proxy
        // (enabled=true). Only taken-over apps can perform failover switching.
        let app_enabled = match self.db.get_proxy_config_for_app(app_type).await {
            Ok(config) => config.enabled,
            Err(e) => {
                log::warn!("[FO-002] failed to read {app_type} config: {e}; skipping switch");
                return Ok(false);
            }
        };

        if !app_enabled {
            log::debug!("[Failover] proxy not enabled for {app_type}; skipping switch");
            return Ok(false);
        }

        log::info!("[FO-001] switching: {app_type} -> {provider_name}");

        let mut switched = false;

        if let Some(app) = app_handle {
            if let Some(app_state) = app.try_state::<crate::store::AppState>() {
                switched = app_state
                    .proxy_service
                    .hot_switch_provider(app_type, provider_id)
                    .await
                    .map_err(AppError::Message)?
                    .logical_target_changed;

                if !switched {
                    return Ok(false);
                }

                if let Ok(new_menu) = crate::tray::create_tray_menu(app, app_state.inner()) {
                    if let Some(tray) = app.tray_by_id(crate::tray::TRAY_ID) {
                        if let Err(e) = tray.set_menu(Some(new_menu)) {
                            log::error!("[Failover] failed to update tray menu: {e}");
                        }
                    }
                }
            }

            // Emit the event to the frontend.
            let event_data = serde_json::json!({
                "appType": app_type,
                "providerId": provider_id,
                "source": "failover"  // Marks failover as the source.
            });
            if let Err(e) = app.emit("provider-switched", event_data) {
                log::error!("[Failover] failed to emit event: {e}");
            }
        }

        Ok(switched)
    }
}
