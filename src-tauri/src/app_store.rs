use serde_json::Value;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};
use tauri_plugin_store::StoreExt;

use crate::error::AppError;

/// Store key.
const STORE_KEY_APP_CONFIG_DIR: &str = "app_config_dir_override";

/// Caches the current app_config_dir override without retaining an AppHandle.
static APP_CONFIG_DIR_OVERRIDE: OnceLock<RwLock<Option<PathBuf>>> = OnceLock::new();

fn override_cache() -> &'static RwLock<Option<PathBuf>> {
    APP_CONFIG_DIR_OVERRIDE.get_or_init(|| RwLock::new(None))
}

fn update_cached_override(value: Option<PathBuf>) {
    if let Ok(mut guard) = override_cache().write() {
        *guard = value;
    }
}

/// Returns the cached app_config_dir override.
pub fn get_app_config_dir_override() -> Option<PathBuf> {
    override_cache().read().ok()?.clone()
}

fn read_override_from_store(app: &tauri::AppHandle) -> Option<PathBuf> {
    // An isolated test/dev home must never inherit a persisted production
    // app-data override, otherwise a smoke test could migrate real user data.
    if crate::config::get_test_home_dir().is_some() {
        return None;
    }

    let store = match app.store_builder("app_paths.json").build() {
        Ok(store) => store,
        Err(e) => {
            log::warn!("Failed to create Store: {e}");
            return None;
        }
    };

    match store.get(STORE_KEY_APP_CONFIG_DIR) {
        Some(Value::String(path_str)) => {
            let path_str = path_str.trim();
            if path_str.is_empty() {
                return None;
            }

            let path = resolve_path(path_str);

            if !path.exists() {
                log::warn!(
                    "The app_config_dir configured in Store does not exist: {path:?}\n\
                     The default path will be used."
                );
                return None;
            }

            log::info!("Using app_config_dir from Store: {path:?}");
            Some(path)
        }
        Some(_) => {
            log::warn!("{STORE_KEY_APP_CONFIG_DIR} in Store has the wrong type; expected a string");
            None
        }
        None => None,
    }
}

/// Refreshes the app_config_dir override from Store and updates the cache.
pub fn refresh_app_config_dir_override(app: &tauri::AppHandle) -> Option<PathBuf> {
    let value = read_override_from_store(app);
    update_cached_override(value.clone());
    value
}

/// Writes app_config_dir to Tauri Store.
pub fn set_app_config_dir_to_store(
    app: &tauri::AppHandle,
    path: Option<&str>,
) -> Result<(), AppError> {
    let store = app
        .store_builder("app_paths.json")
        .build()
        .map_err(|e| AppError::Message(format!("Failed to create Store: {e}")))?;

    match path {
        Some(p) => {
            let trimmed = p.trim();
            if !trimmed.is_empty() {
                store.set(STORE_KEY_APP_CONFIG_DIR, Value::String(trimmed.to_string()));
                log::info!("Wrote app_config_dir to Store: {trimmed}");
            } else {
                store.delete(STORE_KEY_APP_CONFIG_DIR);
                log::info!("Removed app_config_dir from Store");
            }
        }
        None => {
            store.delete(STORE_KEY_APP_CONFIG_DIR);
            log::info!("Removed app_config_dir from Store");
        }
    }

    store
        .save()
        .map_err(|e| AppError::Message(format!("Failed to save Store: {e}")))?;

    refresh_app_config_dir_override(app);
    Ok(())
}

/// Resolves a path, including paths beginning with `~`.
fn resolve_path(raw: &str) -> PathBuf {
    if raw == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    } else if let Some(stripped) = raw.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    } else if let Some(stripped) = raw.strip_prefix("~\\") {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    }

    PathBuf::from(raw)
}

/// Legacy entry point for migrating app_config_dir from settings.json to Store.
pub fn migrate_app_config_dir_from_settings(app: &tauri::AppHandle) -> Result<(), AppError> {
    // app_config_dir has been removed from settings.json. This function remains for compatibility
    // but no longer migrates. Users of an older override must configure it again in Store.
    log::info!("app_config_dir migration has been removed; configure it again in settings");

    let _ = refresh_app_config_dir_override(app);
    Ok(())
}
