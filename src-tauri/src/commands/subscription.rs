use std::str::FromStr;
use tauri::{Emitter, State};

use crate::app_config::AppType;
use crate::services::subscription::{CredentialStatus, SubscriptionQuota};
use crate::store::AppState;

/// Queries an official subscription quota.
///
/// Reads existing OAuth credentials from the CLI tool and calls the official API for quota data.
/// Every outcome, including business failures and transport errors, is written to `UsageCache`,
/// triggers a tray refresh, and emits `usage-cache-updated`, so React Query and the tray share the
/// latest snapshot. After a failure snapshot is written, `format_subscription_summary` returns
/// `None` because `success=false`, removing stale quota suffixes from the tray. Errors are returned
/// unchanged to the renderer so React Query's onError remains observable.
#[tauri::command]
pub async fn get_subscription_quota(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    tool: String,
) -> Result<SubscriptionQuota, String> {
    let inner = crate::services::subscription::get_subscription_quota(&tool).await;
    let snapshot = match &inner {
        Ok(q) => q.clone(),
        // A transport error does not establish a credential failure. Use Valid to indicate that
        // communication or parsing failed without marking the credentials invalid.
        Err(err_msg) => SubscriptionQuota::error(&tool, CredentialStatus::Valid, err_msg.clone()),
    };
    if let Ok(app_type) = AppType::from_str(&tool) {
        let payload = serde_json::json!({
            "kind": "subscription",
            "appType": app_type.as_str(),
            "data": &snapshot,
        });
        if let Err(e) = app.emit("usage-cache-updated", payload) {
            log::error!("Failed to emit usage-cache-updated (subscription): {e}");
        }
        state.usage_cache.put_subscription(app_type, snapshot);
        crate::tray::schedule_tray_refresh(&app);
    }
    inner
}
