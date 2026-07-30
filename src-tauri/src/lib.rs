mod app_config;
mod app_storage;
mod app_store;
mod auto_launch;
mod claude_desktop_config;
mod claude_mcp;
mod claude_plugin;
mod codex_config;
mod codex_history_migration;
mod codex_state_db;
mod commands;
mod config;
mod database;
mod deeplink;
mod error;
mod gemini_config;
mod gemini_mcp;
pub mod hermes_config;
mod init_status;
mod lightweight;
#[cfg(target_os = "linux")]
mod linux_fix;
mod mcp;
mod openclaw_config;
mod opencode_config;
mod panic_hook;
mod prompt;
mod prompt_files;
mod provider;
mod provider_defaults;
mod proxy;
mod services;
mod session_manager;
mod settings;
mod store;

mod tray;
mod usage_events;
mod usage_script;

pub use app_config::{AppType, InstalledSkill, McpApps, McpServer, MultiAppConfig, SkillApps};
pub use codex_config::{get_codex_auth_path, get_codex_config_path, write_codex_live_atomic};
pub use commands::open_provider_terminal;
pub use commands::*;
pub use config::{get_claude_mcp_path, get_claude_settings_path, read_json_file};
pub use database::Database;
pub use deeplink::{import_provider_from_deeplink, parse_deeplink_url, DeepLinkImportRequest};
pub use error::AppError;
pub use mcp::{
    import_from_claude, import_from_codex, import_from_gemini, remove_server_from_claude,
    remove_server_from_codex, remove_server_from_gemini, sync_enabled_to_claude,
    sync_enabled_to_codex, sync_enabled_to_gemini, sync_single_server_to_claude,
    sync_single_server_to_codex, sync_single_server_to_gemini,
};
pub use provider::{Provider, ProviderMeta};
pub use services::{
    skill::{migrate_skills_to_ssot, ImportSkillSelection},
    ConfigService, EndpointLatency, McpService, PromptService, ProviderService, ProxyService,
    SkillService, SpeedtestService,
};
pub use settings::{update_settings, AppSettings};
pub use store::AppState;
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

use std::sync::Arc;
#[cfg(target_os = "macos")]
use tauri::image::Image;
use tauri::tray::{TrayIconBuilder, TrayIconEvent};
use tauri::RunEvent;
use tauri::{Emitter, Manager};
use tauri_plugin_window_state::{AppHandleExt, StateFlags};

#[cfg(target_os = "windows")]
fn set_windows_app_user_model_id(app: &tauri::AppHandle) {
    let app_id = app.config().identifier.clone();
    let wide_app_id: Vec<u16> = app_id.encode_utf16().chain(std::iter::once(0)).collect();

    let result = unsafe {
        windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID(wide_app_id.as_ptr())
    };

    if result < 0 {
        log::warn!("Failed to set Windows AppUserModelID: 0x{result:08X}");
    } else {
        log::debug!("Windows AppUserModelID set to {app_id}");
    }
}

fn redact_url_for_log(url_str: &str) -> String {
    match url::Url::parse(url_str) {
        Ok(url) => {
            let mut output = format!("{}://", url.scheme());
            if let Some(host) = url.host_str() {
                output.push_str(host);
            }
            output.push_str(url.path());

            let mut keys: Vec<String> = url.query_pairs().map(|(k, _)| k.to_string()).collect();
            keys.sort();
            keys.dedup();

            if !keys.is_empty() {
                output.push_str("?[keys:");
                output.push_str(&keys.join(","));
                output.push(']');
            }

            output
        }
        Err(_) => {
            let base = url_str.split('#').next().unwrap_or(url_str);
            match base.split_once('?') {
                Some((prefix, _)) => format!("{prefix}?[redacted]"),
                None => base.to_string(),
            }
        }
    }
}

/// Restore the main window after the app has been hidden in the tray.
pub(crate) fn present_main_window(app: &tauri::AppHandle, reason: &str) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or_else(|| format!("main window not found while handling {reason}"))?;

    let mut errors = Vec::new();

    #[cfg(target_os = "windows")]
    if let Err(e) = window.set_skip_taskbar(false) {
        errors.push(format!("failed to restore taskbar entry: {e}"));
    }

    // A close-to-tray switches macOS to Accessory. Restore Regular before
    // presenting the window; showing it while still Accessory can leave it
    // absent from both CoreGraphics and Accessibility despite returning Ok.
    #[cfg(target_os = "macos")]
    if let Err(e) = tray::apply_tray_policy(app, true) {
        errors.push(e);
    }

    if let Err(e) = window.unminimize() {
        errors.push(format!("failed to unminimize: {e}"));
    }
    if let Err(e) = window.show() {
        errors.push(format!("failed to show: {e}"));
    }
    if let Err(e) = window.set_focus() {
        errors.push(format!("failed to focus: {e}"));
    }

    #[cfg(target_os = "linux")]
    linux_fix::nudge_main_window(window);

    if !errors.is_empty() {
        return Err(format!("{reason}: {}", errors.join("; ")));
    }

    log::info!("Main window presented: {reason}");
    Ok(())
}

/// Handle `nexus://` deep-link URLs in one place.
///
/// - Parse the URL
/// - Emit `deeplink-import` / `deeplink-error` to the frontend
/// - Optionally focus the main window on success
fn handle_deeplink_url(
    app: &tauri::AppHandle,
    url_str: &str,
    focus_main_window: bool,
    source: &str,
) -> bool {
    if !url_str.starts_with("nexus://") {
        return false;
    }

    let redacted_url = redact_url_for_log(url_str);
    log::info!("✓ Deep link URL detected from {source}: {redacted_url}");
    log::debug!("Deep link URL (raw) from {source}: {url_str}");

    match crate::deeplink::parse_deeplink_url(url_str) {
        Ok(request) => {
            log::info!(
                "✓ Successfully parsed deep link: resource={}, app={:?}, name={:?}",
                request.resource,
                request.app,
                request.name
            );

            if let Err(e) = app.emit("deeplink-import", &request) {
                log::error!("✗ Failed to emit deeplink-import event: {e}");
            } else {
                log::info!("✓ Emitted deeplink-import event to frontend");
            }

            if focus_main_window {
                if let Err(e) = present_main_window(app, "deep link") {
                    log::error!("Failed to present main window for deep link: {e}");
                }
            }
        }
        Err(e) => {
            log::error!("✗ Failed to parse deep link URL: {e}");

            if let Err(emit_err) = app.emit(
                "deeplink-error",
                serde_json::json!({
                    "url": url_str,
                    "error": e.to_string()
                }),
            ) {
                log::error!("✗ Failed to emit deeplink-error event: {emit_err}");
            }
        }
    }

    true
}

/// Tauri command to refresh the tray menu.
#[tauri::command]
async fn update_tray_menu(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<bool, String> {
    match tray::create_tray_menu(&app, state.inner()) {
        Ok(new_menu) => {
            if let Some(tray) = app.tray_by_id(tray::TRAY_ID) {
                tray.set_menu(Some(new_menu))
                    .map_err(|e| format!("failed to update tray menu: {e}"))?;
                return Ok(true);
            }
            Ok(false)
        }
        Err(err) => {
            log::error!("Failed to create tray menu: {err}");
            Ok(false)
        }
    }
}

#[cfg(target_os = "macos")]
fn macos_tray_icon() -> Option<Image<'static>> {
    const ICON_BYTES: &[u8] = include_bytes!("../icons/tray/macos/statusbar_template_3x.png");

    match Image::from_bytes(ICON_BYTES) {
        Ok(icon) => Some(icon),
        Err(err) => {
            log::warn!("Failed to load macOS tray icon: {err}");
            None
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Install a panic hook so crashes are logged to
    // <app_config_dir>/crash.log (default: ~/.nexus-composer/crash.log).
    panic_hook::setup_panic_hook();

    let mut builder = tauri::Builder::default();

    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            log::info!("=== Single Instance Callback Triggered ===");
            log::debug!("Args count: {}", args.len());
            for (i, arg) in args.iter().enumerate() {
                log::debug!("  arg[{i}]: {}", redact_url_for_log(arg));
            }

            if crate::lightweight::is_lightweight_mode() {
                if let Err(e) = crate::lightweight::exit_lightweight_mode(app) {
                    log::error!("Failed to rebuild window while leaving lightweight mode: {e}");
                }
            }

            // Check for deep link URL in args (mainly for Windows/Linux command line)
            let mut found_deeplink = false;
            for arg in &args {
                if handle_deeplink_url(app, arg, false, "single_instance args") {
                    found_deeplink = true;
                    break;
                }
            }

            if !found_deeplink {
                log::info!("ℹ No deep link URL found in args (this is expected on macOS when launched via system)");
            }

            // Show and focus window regardless
            if let Err(e) = present_main_window(app, "single-instance activation") {
                log::error!("Failed to present main window after activation: {e}");
            }
        }));
    }

    let builder = builder
        // Register the deep-link plugin for macOS AppleEvents and other platforms.
        .plugin(tauri_plugin_deep_link::init())
        // Intercept window close and decide whether to minimize to tray.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // Recovery mode for a newer database version has no tray entry to bring
                // the app back, so closing should exit instead of hiding in the background.
                let in_db_recovery = crate::init_status::get_init_error()
                    .map(|p| p.kind.as_deref() == Some("db_version_too_new"))
                    .unwrap_or(false);
                if in_db_recovery {
                    api.prevent_close();
                    window.app_handle().exit(0);
                    return;
                }

                let settings = crate::settings::get_settings();

                if settings.minimize_to_tray_on_close {
                    api.prevent_close();
                    let _ = window.hide();
                    #[cfg(target_os = "windows")]
                    {
                        let _ = window.set_skip_taskbar(true);
                    }
                    #[cfg(target_os = "macos")]
                    {
                        if let Err(e) = tray::apply_tray_policy(window.app_handle(), false) {
                            log::warn!("Failed to apply close-to-tray policy: {e}");
                        }
                    }
                } else {
                    api.prevent_close();
                    window.app_handle().exit(0);
                }
            }
        })
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(window_state_flags())
                .build(),
        )
        .setup(|app| {
            let _ = rustls::crypto::ring::default_provider().install_default();

            // Refresh Store-backed path overrides before later path reads (logs, DB, etc.).
            app_store::refresh_app_config_dir_override(app.handle());

            // Resolve and, when needed, promote the authoritative app-data layout
            // before any settings, log, backup, Skill, or database file is opened.
            // SQLite schema migration remains a separate later phase.
            let storage_preparation = loop {
                match app_storage::prepare_app_storage(crate::database::SCHEMA_VERSION) {
                    Ok(app_storage::StoragePreparation::Ready {
                        database_path,
                        report_path,
                        migrated,
                    }) => break (database_path, report_path, migrated),
                    Ok(app_storage::StoragePreparation::DatabaseTooNew { path, version }) => {
                        crate::init_status::set_init_error(crate::init_status::InitErrorPayload {
                            path: path.display().to_string(),
                            error: format!(
                                "Database version is newer than supported ({version}). This app only supports {}. Upgrade the app and try again.",
                                crate::database::SCHEMA_VERSION
                            ),
                            kind: Some("db_version_too_new".to_string()),
                            db_version: Some(version),
                            supported_version: Some(crate::database::SCHEMA_VERSION),
                        });
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                        return Ok(());
                    }
                    Err(error) => {
                        let database_path = crate::config::get_app_database_path();
                        eprintln!("Failed to prepare Nexus Composer storage: {error}");
                        if !show_database_init_error_dialog(
                            app.handle(),
                            &database_path,
                            &error.to_string(),
                        ) {
                            std::process::exit(1);
                        }
                    }
                }
            };
            panic_hook::init_app_config_dir(crate::config::get_app_config_dir());
            #[cfg(target_os = "windows")]
            set_windows_app_user_model_id(app.handle());

            // Register the updater plugin on desktop targets.
            #[cfg(desktop)]
            {
                if let Err(e) = app
                    .handle()
                    .plugin(tauri_plugin_updater::Builder::new().build())
                {
                    // If updater config is incomplete (for example missing a pubkey),
                    // skip it without aborting app startup.
                    log::warn!("Failed to initialize Updater plugin; skipping it: {e}");
                }
            }
            // Initialize logging (single file at
            // <app_config_dir>/logs/nexus-composer.log).
            {
                use tauri_plugin_log::{RotationStrategy, Target, TargetKind, TimezoneStrategy};

                let log_dir = panic_hook::get_log_dir();

                // Ensure the log directory exists.
                if let Err(e) = std::fs::create_dir_all(&log_dir) {
                    eprintln!("Failed to create log directory: {e}");
                }

                // Remove the previous log file on startup to keep single-file behavior.
                let log_file_path = log_dir.join("nexus-composer.log");
                let _ = std::fs::remove_file(&log_file_path);

                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        // Start at Trace so log::set_max_level() can lower it later.
                        .level(log::LevelFilter::Trace)
                        .targets([
                            Target::new(TargetKind::Stdout),
                            Target::new(TargetKind::Folder {
                                path: log_dir,
                                file_name: Some("nexus-composer".into()),
                            }),
                        ])
                        // Single-file mode: delete the previous file on startup and rotate
                        // only after the file reaches the size limit.
                        // Note: KeepSome(n) internally computes n - 2, so n = 1 underflows.
                        // KeepSome(2) is the smallest safe value and keeps no rotated files.
                        .rotation_strategy(RotationStrategy::KeepSome(2))
                        // Cap the single log file at 1 GB.
                        .max_file_size(1024 * 1024 * 1024)
                        .timezone_strategy(TimezoneStrategy::UseLocal)
                        .build(),
                )?;
            }

            // Provide AppHandle to usage_events so code paths without their own handle
            // can still emit `usage-log-recorded` to the frontend.
            // This must happen after logging is initialized so init logs are emitted.
            usage_events::init(app.handle().clone());

            if storage_preparation.2 {
                if let Some(report_path) = storage_preparation.1.as_ref() {
                    log::info!(
                        "Application storage migration completed; report: {}",
                        report_path.display()
                    );
                } else {
                    log::info!("Application storage migration completed");
                }
            }

            // Initialize the database.
            let app_config_dir = crate::config::get_app_config_dir();
            let db_path = storage_preparation.0;
            let json_path = app_config_dir.join("config.json");

            // Check whether config.json still needs migration into SQLite.
            let has_json = json_path.exists();
            let has_db = db_path.exists();
            let had_nexus_state_at_startup = has_db || has_json;

            // If migration is needed, validate config.json before creating the DB so
            // a failed load can exit cleanly and retry on the next startup.
            let migration_config = if !has_db && has_json {
                log::info!("Detected legacy config file; validating it before migration...");

                // Allow the user to retry loading the config file.
                loop {
                    match crate::app_config::MultiAppConfig::load() {
                        Ok(config) => {
                            log::info!("✓ Legacy config file loaded successfully");
                            break Some(config);
                        }
                        Err(e) => {
                            log::error!("Failed to load legacy config file: {e}");
                            // Show a system dialog so the user can choose how to proceed.
                            if !show_migration_error_dialog(app.handle(), &e.to_string()) {
                                // If the user exits here, the DB still has not been created,
                                // so the next startup can retry cleanly.
                                log::info!("User chose to exit the app");
                                std::process::exit(1);
                            }
                            // Retry loading if the user requested it.
                            log::info!("User chose to retry loading the config file");
                        }
                    }
                }
            } else {
                None
            };

            // Create the database now, including schema migrations.
            //
            // Users upgrading from v3.8.* usually go through SQLite schema migration
            // here. If migration fails (corruption, permissions, newer user_version,
            // etc.), surface a clear message instead of a silent launch failure.
            //
            // Preflight: if the on-disk database version is newer than this app can
            // understand, enter recovery before any schema writes. `create_tables`
            // performs DDL such as DROP/ALTER, which must not touch a newer DB.
            match crate::database::Database::stored_user_version_exceeds_supported(&db_path) {
                Ok(Some(version)) => {
                    log::warn!(
                        "Database version is newer than supported (v{version}); directing user to upgrade in-app"
                    );
                    crate::init_status::set_init_error(crate::init_status::InitErrorPayload {
                        path: db_path.display().to_string(),
                        error: format!(
                            "Database version is newer than supported ({version}). This app only supports {}. Upgrade the app and try again.",
                            crate::database::SCHEMA_VERSION
                        ),
                        kind: Some("db_version_too_new".to_string()),
                        db_version: Some(version),
                        supported_version: Some(crate::database::SCHEMA_VERSION),
                    });
                    // The main window defaults to visible:false, so force it open for recovery.
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                    return Ok(());
                }
                Ok(None) => {}
                Err(e) => {
                    log::warn!("Failed to preflight database version; continuing normal init: {e}");
                }
            }

            let db = loop {
                match crate::database::Database::init() {
                    Ok(db) => break Arc::new(db),
                    Err(e) => {
                        log::error!("Failed to init database: {e}");

                        if !show_database_init_error_dialog(app.handle(), &db_path, &e.to_string())
                        {
                            log::info!("User chose to exit the app");
                            std::process::exit(1);
                        }

                        log::info!("User chose to retry database initialization");
                    }
                }
            };

            // Run config migration if a validated config was preloaded.
            if let Some(config) = migration_config {
                log::info!("Starting data migration...");

                match db.migrate_from_json(&config) {
                    Ok(_) => {
                        log::info!("✓ Configuration migrated successfully");
                        // Flag migration success so the frontend can show a toast.
                        crate::init_status::set_migration_success();
                        // Archive the old config file by renaming it instead of deleting it.
                        let archive_path = json_path.with_extension("json.migrated");
                        if let Err(e) = std::fs::rename(&json_path, &archive_path) {
                            log::warn!("Failed to archive legacy config file: {e}");
                        } else {
                            log::info!("✓ Legacy config archived as config.json.migrated");
                        }
                    }
                    Err(e) => {
                        // A successfully loaded config almost never fails here except for
                        // cases like disk-full; log it and continue importing existing data.
                        log::error!("Configuration migration failed: {e}; continuing with existing config import");
                    }
                }
            }

            let app_state = AppState::new(db);

            // Provide AppHandle for UI updates during proxy failover.
            app_state.proxy_service.set_app_handle(app.handle().clone());

            match codex_config::repair_codex_official_auth_proxy_placeholder() {
                Ok(true) => {
                    log::info!("✓ Removed stale Codex proxy key from preserved OAuth login")
                }
                Ok(false) => {}
                Err(error) => {
                    log::warn!("Failed to repair preserved Codex OAuth login: {error}")
                }
            }

            // ============================================================
            // Per-table import decisions: each data category is checked independently.
            // ============================================================

            // 1. Initialize default skill repositories. Internal checks already skip
            // this when the table is not empty.
            match app_state.db.init_default_skill_repos() {
                Ok(count) if count > 0 => {
                    log::info!("✓ Initialized {count} default skill repositories");
                }
                Ok(_) => {} // Table already populated; skip quietly.
                Err(e) => log::warn!("✗ Failed to initialize default skill repos: {e}"),
            }

            // 1.1. Unified skill migration: once the database reaches the v3 layout,
            // automatically import per-app skill directories into SSOT when
            // `settings.skills_ssot_migration_pending = true`.
            match app_state.db.get_setting("skills_ssot_migration_pending") {
                Ok(Some(flag)) if flag == "true" || flag == "1" => {
                    // Safety guard: do not wipe and rebuild if the user already has v3-era
                    // skill data in place.
                    let has_existing = app_state
                        .db
                        .get_all_installed_skills()
                        .map(|skills| !skills.is_empty())
                        .unwrap_or(false);

                    if has_existing {
                        log::info!(
                            "Detected skills_ssot_migration_pending but skills table not empty; skipping auto import."
                        );
                        let _ = app_state
                            .db
                            .set_setting("skills_ssot_migration_pending", "false");
                    } else {
                        match crate::services::skill::migrate_skills_to_ssot(&app_state.db) {
                            Ok(count) => {
                                log::info!("✓ Auto imported {count} skill(s) into SSOT");
                                if count > 0 {
                                    crate::init_status::set_skills_migration_result(count);
                                }
                                let _ = app_state
                                    .db
                                    .set_setting("skills_ssot_migration_pending", "false");
                            }
                            Err(e) => {
                                log::warn!("✗ Failed to auto import legacy skills to SSOT: {e}");
                                crate::init_status::set_skills_migration_error(e.to_string());
                                // Keep the pending flag so startup can retry later.
                            }
                        }
                    }
                }
                Ok(_) => {} // Migration flag not enabled; skip quietly.
                Err(e) => log::warn!("✗ Failed to read skills migration flag: {e}"),
            }

            // 1.5. Auto-import live configs and seed official preset providers
            // (Claude / Codex / Gemini).
            //
            // Import first and seed second by design. First turn the user's manually
            // configured settings.json / auth.json / .env into the current "default"
            // provider, then append official presets with `is_current = false`.
            // That allows fallback logic to preserve the original live config if the
            // user later switches to an official preset.
            //
            // Capture a first-run snapshot. Brand-new installs show the welcome notice.
            // If the read fails, default to not showing it to avoid unnecessary friction.
            let first_run_already_confirmed =
                crate::settings::get_settings().first_run_notice_confirmed.unwrap_or(false);
            let fresh_install_at_startup = app_state.db.is_providers_empty().unwrap_or(false);

            for app_type in
                crate::app_config::AppType::all().filter(|t| !t.is_additive_mode())
            {
                if !crate::services::provider::should_import_default_config_on_startup(
                    &app_state,
                    &app_type,
                )
                .unwrap_or(false)
                {
                    log::debug!(
                        "○ {} already has providers; live import skipped",
                        app_type.as_str()
                    );
                    continue;
                }

                match crate::services::provider::import_default_config(
                    &app_state,
                    app_type.clone(),
                ) {
                    Ok(true) => log::info!(
                        "✓ Imported live config for {} as default provider",
                        app_type.as_str()
                    ),
                    Ok(false) => log::debug!(
                        "○ {} already has providers; live import skipped",
                        app_type.as_str()
                    ),
                    Err(e) => log::debug!(
                        "○ No live config to import for {}: {e}",
                        app_type.as_str()
                    ),
                }
            }

            match app_state.db.init_default_official_providers() {
                Ok(count) if count > 0 => {
                    log::info!("✓ Seeded {count} official provider(s)");
                }
                Ok(_) => {}
                Err(e) => log::warn!("✗ Failed to seed official providers: {e}"),
            }

            for app_type in [
                app_config::AppType::Claude,
                app_config::AppType::ClaudeDesktop,
                app_config::AppType::Codex,
            ] {
                let current_id = services::provider::ProviderService::current(
                    &app_state,
                    app_type.clone(),
                )
                .ok()
                .filter(|id| !id.is_empty());
                match app_state
                    .db
                    .migrate_managed_nexus_for_app(&app_type, current_id.as_deref())
                {
                    Ok(true) => {
                        if let Err(error) =
                            services::provider::ProviderService::sync_current_provider_for_app(
                                &app_state,
                                app_type.clone(),
                            )
                        {
                            log::warn!(
                                "Failed to apply updated {} Nexus provider: {error}",
                                app_type.as_str()
                            );
                        }
                    }
                    Ok(false) => {}
                    Err(error) => log::warn!(
                        "Skipped {} Nexus provider migration: {error}",
                        app_type.as_str()
                    ),
                }
            }

            match codex_config::repair_live_codex_model_catalog_path() {
                Ok(true) => log::info!("Updated Codex model catalog to an absolute path"),
                Ok(false) => {}
                Err(error) => log::warn!("Failed to update Codex model catalog path: {error}"),
            }

            {
                let db_for_codex_history_migration = app_state.db.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    match crate::codex_history_migration::repair_codex_thread_visibility() {
                        Ok(outcome) if outcome.repaired_state_rows > 0 => {
                            log::info!(
                                "✓ Repaired hidden Codex thread metadata: state_rows={}",
                                outcome.repaired_state_rows
                            );
                        }
                        Ok(_) => {}
                        Err(e) => {
                            log::warn!("✗ Codex thread visibility repair failed: {e}");
                        }
                    }

                    let history_migration = if had_nexus_state_at_startup {
                        crate::codex_history_migration::maybe_migrate_codex_third_party_history_provider_bucket(
                            &db_for_codex_history_migration,
                        )
                    } else {
                        crate::codex_history_migration::preserve_codex_history_for_fresh_install()
                    };
                    match history_migration {
                        Ok(outcome) => {
                            if let Some(reason) = outcome.skipped_reason {
                                log::debug!("○ Codex history provider bucket migration skipped: {reason}");
                            } else {
                                log::info!(
                                    "✓ Codex history provider bucket migration completed: sources={}, jsonl_files={}, state_rows={}",
                                    outcome.source_provider_ids.len(),
                                    outcome.migrated_jsonl_files,
                                    outcome.migrated_state_rows
                                );
                            }
                        }
                        Err(e) => {
                            log::warn!("✗ Codex history provider bucket migration failed: {e}");
                        }
                    }

                    match crate::codex_history_migration::maybe_migrate_codex_provider_template_bucket(
                        &db_for_codex_history_migration,
                    ) {
                        Ok(outcome) => {
                            if let Some(reason) = outcome.skipped_reason {
                                log::debug!("○ Codex provider template bucket migration skipped: {reason}");
                            } else if !outcome.migrated_provider_ids.is_empty() {
                                log::info!(
                                    "✓ Codex provider template bucket migration completed: providers={}",
                                    outcome.migrated_provider_ids.len()
                                );
                            }
                        }
                        Err(e) => {
                            log::warn!("✗ Codex provider template bucket migration failed: {e}");
                        }
                    }

                    // Retry official-history unification during startup if the feature
                    // is enabled and the previous attempt did not finish (for example
                    // due to a locked file). The function self-gates and skips when off.
                    match crate::codex_history_migration::maybe_migrate_codex_official_history_to_unified_bucket() {
                        Ok(outcome) => {
                            if let Some(reason) = outcome.skipped_reason {
                                log::debug!("○ Codex official history unify migration skipped: {reason}");
                            } else {
                                log::info!(
                                    "✓ Codex official history unify migration completed: jsonl_files={}, state_rows={}",
                                    outcome.migrated_jsonl_files,
                                    outcome.migrated_state_rows
                                );
                            }
                        }
                        Err(e) => {
                            log::warn!("✗ Codex official history unify migration failed: {e}");
                        }
                    }
                });
            }

            // Existing users and already-confirmed paths are gated by
            // `fresh_install_at_startup`; do not write anything here.
            // The frontend alone persists this field when the user explicitly confirms
            // the welcome notice.
            if !first_run_already_confirmed && fresh_install_at_startup {
                log::info!("✓ First-run welcome notice pending");
            }

            // 1.6. Automatically sync OpenCode / OpenClaw live providers into the DB.
            //
            // The additive-mode import functions for OpenCode / OpenClaw are idempotent
            // by provider ID. Existing providers are skipped, so running them on every
            // startup is safe. That gives fresh installs immediate visibility into live
            // providers and also syncs external live-file edits after restart.
            //
            // The underlying `read_*_config` helpers return empty defaults when the file
            // is missing, so fresh installs without live files hit `Ok(0)` without noisy
            // error logs.
            match crate::services::provider::import_opencode_providers_from_live(&app_state) {
                Ok(count) if count > 0 => {
                    log::info!("✓ Imported {count} OpenCode provider(s) from live config");
                }
                Ok(_) => log::debug!("○ No new OpenCode providers to import"),
                Err(e) => log::warn!("✗ Failed to import OpenCode providers: {e}"),
            }
            match crate::services::provider::import_openclaw_providers_from_live(&app_state) {
                Ok(count) if count > 0 => {
                    log::info!("✓ Imported {count} OpenClaw provider(s) from live config");
                }
                Ok(_) => log::debug!("○ No new OpenClaw providers to import"),
                Err(e) => log::warn!("✗ Failed to import OpenClaw providers: {e}"),
            }
            match crate::services::provider::import_hermes_providers_from_live(&app_state) {
                Ok(count) if count > 0 => {
                    log::info!("✓ Imported {count} Hermes provider(s) from live config");
                }
                Ok(_) => log::debug!("○ No new Hermes providers to import"),
                Err(e) => log::warn!("✗ Failed to import Hermes providers: {e}"),
            }

            // 2. Import OMO config from local files when the DB has no OMO provider.
            {
                let has_omo = app_state
                    .db
                    .get_all_providers("opencode")
                    .map(|providers| providers.values().any(|p| p.category.as_deref() == Some("omo")))
                    .unwrap_or(false);
                if !has_omo {
                    match crate::services::OmoService::import_from_local(&app_state, &crate::services::omo::STANDARD) {
                        Ok(provider) => {
                            log::info!("✓ Imported OMO config from local as provider '{}'", provider.name);
                        }
                        Err(AppError::OmoConfigNotFound) => {
                            log::debug!("○ No OMO config to import");
                        }
                        Err(e) => {
                            log::warn!("✗ Failed to import OMO config from local: {e}");
                        }
                    }
                }
            }

            // 2.3 OMO Slim config import (when no omo-slim provider in DB, import from local)
            {
                let has_omo_slim = app_state
                    .db
                    .get_all_providers("opencode")
                    .map(|providers| {
                        providers
                            .values()
                            .any(|p| p.category.as_deref() == Some("omo-slim"))
                    })
                    .unwrap_or(false);
                if !has_omo_slim {
                    match crate::services::OmoService::import_from_local(&app_state, &crate::services::omo::SLIM) {
                        Ok(provider) => {
                            log::info!(
                                "✓ Imported OMO Slim config from local as provider '{}'",
                                provider.name
                            );
                        }
                        Err(AppError::OmoConfigNotFound) => {
                            log::debug!("○ No OMO Slim config to import");
                        }
                        Err(e) => {
                            log::warn!("✗ Failed to import OMO Slim config from local: {e}");
                        }
                    }
                }
            }

            // 3. Import MCP server config when the table is empty.
            if app_state.db.is_mcp_table_empty().unwrap_or(false) {
                log::info!("MCP table empty, importing from live configurations...");

                match crate::services::mcp::McpService::import_from_claude(&app_state) {
                    Ok(count) if count > 0 => {
                        log::info!("✓ Imported {count} MCP server(s) from Claude");
                    }
                    Ok(_) => log::debug!("○ No Claude MCP servers found to import"),
                    Err(e) => log::warn!("✗ Failed to import Claude MCP: {e}"),
                }

                match crate::services::mcp::McpService::import_from_codex(&app_state) {
                    Ok(count) if count > 0 => {
                        log::info!("✓ Imported {count} MCP server(s) from Codex");
                    }
                    Ok(_) => log::debug!("○ No Codex MCP servers found to import"),
                    Err(e) => log::warn!("✗ Failed to import Codex MCP: {e}"),
                }

                match crate::services::mcp::McpService::import_from_gemini(&app_state) {
                    Ok(count) if count > 0 => {
                        log::info!("✓ Imported {count} MCP server(s) from Gemini");
                    }
                    Ok(_) => log::debug!("○ No Gemini MCP servers found to import"),
                    Err(e) => log::warn!("✗ Failed to import Gemini MCP: {e}"),
                }

                match crate::services::mcp::McpService::import_from_opencode(&app_state) {
                    Ok(count) if count > 0 => {
                        log::info!("✓ Imported {count} MCP server(s) from OpenCode");
                    }
                    Ok(_) => log::debug!("○ No OpenCode MCP servers found to import"),
                    Err(e) => log::warn!("✗ Failed to import OpenCode MCP: {e}"),
                }

                match crate::services::mcp::McpService::import_from_hermes(&app_state) {
                    Ok(count) if count > 0 => {
                        log::info!("✓ Imported {count} MCP server(s) from Hermes");
                    }
                    Ok(_) => log::debug!("○ No Hermes MCP servers found to import"),
                    Err(e) => log::warn!("✗ Failed to import Hermes MCP: {e}"),
                }
            }

            // 4. Import prompt files when the table is empty.
            if app_state.db.is_prompts_table_empty().unwrap_or(false) {
                log::info!("Prompts table empty, importing from live configurations...");

                for app in [
                    crate::app_config::AppType::Claude,
                    crate::app_config::AppType::Codex,
                    crate::app_config::AppType::Gemini,
                    crate::app_config::AppType::OpenCode,
                    crate::app_config::AppType::OpenClaw,
                    crate::app_config::AppType::Hermes,
                ] {
                    match crate::services::prompt::PromptService::import_from_file_on_first_launch(
                        &app_state,
                        app.clone(),
                    ) {
                        Ok(count) if count > 0 => {
                            log::info!("✓ Imported {count} prompt(s) for {}", app.as_str());
                        }
                        Ok(_) => log::debug!("○ No prompt file found for {}", app.as_str()),
                        Err(e) => log::warn!("✗ Failed to import prompt for {}: {e}", app.as_str()),
                    }
                }
            }

            // Migrate the legacy `app_config_dir` setting into Store.
            if let Err(e) = app_store::migrate_app_config_dir_from_settings(app.handle()) {
                log::warn!("Failed to migrate app_config_dir: {e}");
            }

            // Do not save unconditionally during startup; avoid accidentally overwriting
            // user configuration.

            // Register the deep-link URL handler using the correct DeepLinkExt API.
            log::info!("=== Registering deep-link URL handler ===");

            // Linux and Windows debug builds require explicit registration.
            #[cfg(any(target_os = "linux", all(debug_assertions, windows)))]
            {
                #[cfg(target_os = "linux")]
                {
                    // Use Tauri's path API to get correct path (includes app identifier)
                    // tauri-plugin-deep-link writes to: ~/.local/share/com.nexuscomposer.desktop/applications/nexus-composer-handler.desktop
                    // Only register if .desktop file doesn't exist to avoid overwriting user customizations
                    let should_register = app
                        .path()
                        .data_dir()
                        .map(|d| !d.join("applications/nexus-composer-handler.desktop").exists())
                        .unwrap_or(true);

                    if should_register {
                        if let Err(e) = app.deep_link().register_all() {
                            log::error!("✗ Failed to register deep link schemes: {}", e);
                        } else {
                            log::info!("✓ Deep link schemes registered (Linux)");
                        }
                    } else {
                        log::info!("⊘ Deep link handler already exists, skipping registration");
                    }
                }

                #[cfg(all(debug_assertions, windows))]
                {
                    if let Err(e) = app.deep_link().register_all() {
                        log::error!("✗ Failed to register deep link schemes: {}", e);
                    } else {
                        log::info!("✓ Deep link schemes registered (Windows debug)");
                    }
                }
            }

            // Register the URL-open callback for all platforms.
            app.deep_link().on_open_url({
                let app_handle = app.handle().clone();
                move |event| {
                    log::info!("=== Deep Link Event Received (on_open_url) ===");
                    let urls = event.urls();
                    log::info!("Received {} URL(s)", urls.len());

                    if crate::lightweight::is_lightweight_mode() {
                        if let Err(e) = crate::lightweight::exit_lightweight_mode(&app_handle) {
                            log::error!("Failed to rebuild window while leaving lightweight mode: {e}");
                        }
                    }

                    for (i, url) in urls.iter().enumerate() {
                        let url_str = url.as_str();
                        log::debug!("  URL[{i}]: {}", redact_url_for_log(url_str));

                        if handle_deeplink_url(&app_handle, url_str, true, "on_open_url") {
                            break; // Process only first nexus:// URL
                        }
                    }
                }
            });
            log::info!("✓ Deep-link URL handler registered");

            // Create the dynamic tray menu.
            let menu = tray::create_tray_menu(app.handle(), &app_state)?;

            // Build the tray icon.
            let mut tray_builder = TrayIconBuilder::with_id(tray::TRAY_ID)
                .tooltip("Nexus Composer") // Tooltip shown on hover.
                .on_tray_icon_event(|tray, event| match event {
                    // Refresh cached usage asynchronously on hover/click so the next
                    // menu open sees fresher numbers. The helper internally debounces
                    // refreshes for 10 seconds.
                    TrayIconEvent::Enter { .. } | TrayIconEvent::Click { .. } => {
                        let app = tray.app_handle().clone();
                        tauri::async_runtime::spawn(async move {
                            crate::tray::refresh_all_usage_in_tray(&app).await;
                        });
                    }
                    _ => log::debug!("unhandled event {event:?}"),
                })
                .menu(&menu)
                .on_menu_event(|app, event| {
                    tray::handle_tray_menu_event(app, &event.id.0);
                })
                .show_menu_on_left_click(true);

            // Use the platform-appropriate tray icon. macOS prefers a template icon so
            // the system can adapt it for light and dark appearances.
            #[cfg(target_os = "macos")]
            {
                if let Some(icon) = macos_tray_icon() {
                    tray_builder = tray_builder.icon(icon).icon_as_template(true);
                } else if let Some(icon) = app.default_window_icon() {
                    log::warn!("Falling back to default window icon for tray");
                    tray_builder = tray_builder.icon(icon.clone());
                } else {
                    log::warn!("Failed to load macOS tray icon for tray");
                }
            }

            #[cfg(not(target_os = "macos"))]
            {
                if let Some(icon) = app.default_window_icon() {
                    tray_builder = tray_builder.icon(icon.clone());
                } else {
                    log::warn!("Failed to get default window icon for tray");
                }
            }

            let _tray = tray_builder.build(app)?;
            crate::services::webdav_auto_sync::start_worker(
                app_state.db.clone(),
                app.handle().clone(),
            );
            crate::services::s3_auto_sync::start_worker(
                app_state.db.clone(),
                app.handle().clone(),
            );
            // Inject the same instance into global state to avoid duplicate-service skew.
            app.manage(app_state);

            // Load and apply log configuration from the database.
            {
                let db = &app.state::<AppState>().db;
                if let Ok(log_config) = db.get_log_config() {
                    log::set_max_level(log_config.to_level_filter());
                    log::info!(
                        "Loaded log configuration: enabled={}, level={}",
                        log_config.enabled,
                        log_config.level
                    );
                }
            }

            // Initialize SkillService.
            let skill_service = SkillService::new();
            app.manage(commands::skill::SkillServiceState(Arc::new(skill_service)));

            // Initialize CopilotAuthManager.
            {
                use crate::proxy::providers::copilot_auth::CopilotAuthManager;
                use commands::CopilotAuthState;
                use tokio::sync::RwLock;

                let app_config_dir = crate::config::get_app_config_dir();
                let copilot_auth_manager = CopilotAuthManager::new(app_config_dir);
                app.manage(CopilotAuthState(Arc::new(RwLock::new(copilot_auth_manager))));
                log::info!("✓ CopilotAuthManager initialized");
            }

            // Initialize CodexOAuthManager (ChatGPT Plus/Pro proxy auth).
            {
                use crate::proxy::providers::codex_oauth_auth::CodexOAuthManager;
                use commands::CodexOAuthState;
                use tokio::sync::RwLock;

                let app_config_dir = crate::config::get_app_config_dir();
                let codex_oauth_manager = CodexOAuthManager::new(app_config_dir);
                app.manage(CodexOAuthState(Arc::new(RwLock::new(codex_oauth_manager))));
                log::info!("✓ CodexOAuthManager initialized");
            }

            // Initialize the global outbound proxy HTTP client.
            {
                let db = &app.state::<AppState>().db;
                let proxy_url = db.get_global_proxy_url().ok().flatten();

                if let Err(e) = crate::proxy::http_client::init(proxy_url.as_deref()) {
                    log::error!(
                        "[GlobalProxy] [GP-005] Failed to initialize with saved config: {e}"
                    );

                    // Clear invalid proxy configuration.
                    if proxy_url.is_some() {
                        log::warn!(
                            "[GlobalProxy] [GP-006] Clearing invalid proxy config from database"
                        );
                        if let Err(clear_err) = db.set_global_proxy_url(None) {
                            log::error!(
                                "[GlobalProxy] [GP-007] Failed to clear invalid config: {clear_err}"
                            );
                        }
                    }

                    // Reinitialize in direct mode.
                    if let Err(fallback_err) = crate::proxy::http_client::init(None) {
                        log::error!(
                            "[GlobalProxy] [GP-008] Failed to initialize direct connection: {fallback_err}"
                        );
                    }
                }
            }

            // Recover from unclean exit state and restore proxy state automatically.
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = app_handle.state::<AppState>();

                // Check for live-config backups, which indicate the previous run may
                // have crashed while takeover was active.
                let has_backups = match state.db.has_any_live_backup().await {
                    Ok(v) => v,
                    Err(e) => {
                        log::error!("Failed to check Live backups: {e}");
                        false
                    }
                };
                // Check whether live configs still show takeover placeholders.
                let live_taken_over = state.proxy_service.detect_takeover_in_live_configs();

                if has_backups || live_taken_over {
                    log::warn!("Detected leftover takeover state from a previous abnormal exit; restoring Live config...");
                    if let Err(e) = state.proxy_service.recover_from_crash().await {
                        log::error!("Failed to restore Live config: {e}");
                    } else {
                        log::info!("Live config restored");
                    }
                }

                initialize_common_config_snippets(&state);

                // Restore proxy services based on state stored in the settings table.
                restore_proxy_state_on_startup(&state).await;

                // Periodic backup check (on startup)
                if let Err(e) = state.db.periodic_backup_if_needed() {
                    log::warn!("Periodic backup failed on startup: {e}");
                }

                // Periodic maintenance timer: run once per day while the app is running
                let db_for_timer = state.db.clone();
                tauri::async_runtime::spawn(async move {
                    const PERIODIC_MAINTENANCE_INTERVAL_SECS: u64 = 24 * 60 * 60;
                    let mut interval = tokio::time::interval(std::time::Duration::from_secs(
                        PERIODIC_MAINTENANCE_INTERVAL_SECS,
                    ));
                    interval.tick().await; // skip immediate first tick (already checked above)
                    loop {
                        interval.tick().await;
                        if let Err(e) = db_for_timer.periodic_backup_if_needed() {
                            log::warn!("Periodic maintenance timer failed: {e}");
                        }
                    }
                });

                // Session log usage sync: run once on startup, then every 60 seconds.
                let db_for_session_sync = state.db.clone();
                tauri::async_runtime::spawn(async move {
                    const SESSION_SYNC_INTERVAL_SECS: u64 = 60;

                    fn run_step<T>(name: &str, result: Result<T, crate::error::AppError>) {
                        if let Err(e) = result {
                            log::warn!("{name} failed: {e}");
                        }
                    }

                    // These synchronizers perform blocking filesystem and SQLite work.
                    // Keep the startup pass off Tauri's async runtime so a large local
                    // history cannot starve WebView commands and paint work.
                    let initial_db = db_for_session_sync.clone();
                    if let Err(error) = tauri::async_runtime::spawn_blocking(move || {
                        let db = &initial_db;
                        run_step(
                            "Usage cost startup backfill",
                            db.backfill_missing_usage_costs(),
                        );
                        run_step(
                            "Session usage initial sync",
                            crate::services::session_usage::sync_claude_session_logs(db),
                        );
                        run_step(
                            "Codex usage initial sync",
                            crate::services::session_usage_codex::sync_codex_usage(db),
                        );
                        run_step(
                            "Gemini usage initial sync",
                            crate::services::session_usage_gemini::sync_gemini_usage(db),
                        );
                        run_step(
                            "OpenCode usage initial sync",
                            crate::services::session_usage_opencode::sync_opencode_usage(db),
                        );
                    })
                    .await
                    {
                        log::warn!("Session usage initial sync task failed: {error}");
                    }

                    // Periodic sync.
                    let mut interval = tokio::time::interval(std::time::Duration::from_secs(
                        SESSION_SYNC_INTERVAL_SECS,
                    ));
                    interval.tick().await; // skip immediate first tick
                    loop {
                        interval.tick().await;
                        let periodic_db = db_for_session_sync.clone();
                        if let Err(error) = tauri::async_runtime::spawn_blocking(move || {
                            let db = &periodic_db;
                            run_step(
                                "Session usage periodic sync",
                                crate::services::session_usage::sync_claude_session_logs(db),
                            );
                            run_step(
                                "Codex usage periodic sync",
                                crate::services::session_usage_codex::sync_codex_usage(db),
                            );
                            run_step(
                                "Gemini usage periodic sync",
                                crate::services::session_usage_gemini::sync_gemini_usage(db),
                            );
                            run_step(
                                "OpenCode usage periodic sync",
                                crate::services::session_usage_opencode::sync_opencode_usage(db),
                            );
                        })
                        .await
                        {
                            log::warn!("Session usage periodic sync task failed: {error}");
                        }
                    }
                });
            });

            // Linux: disable WebKitGTK hardware acceleration to avoid EGL init failures
            // that can leave the window blank.
            #[cfg(target_os = "linux")]
            {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.with_webview(|webview| {
                        use webkit2gtk::{WebViewExt, SettingsExt, HardwareAccelerationPolicy};
                        let wk_webview = webview.inner();
                        if let Some(settings) = WebViewExt::settings(&wk_webview) {
                            SettingsExt::set_hardware_acceleration_policy(&settings, HardwareAccelerationPolicy::Never);
                            log::info!("Disabled WebKitGTK hardware acceleration");
                        }
                    });
                }
            }

            // Silent startup: decide whether to show the main window from settings.
            let settings = crate::settings::get_settings();
            if let Some(window) = app.get_webview_window("main") {
                // Sync decoration state before first show to avoid a title-bar flicker
                // after the frontend loads.
                // Linux only: fixes unusable system window buttons under Wayland.
                #[cfg(target_os = "linux")]
                let _ = window.set_decorations(!settings.use_app_window_controls);
                if settings.silent_startup {
                    // Silent startup: keep the window hidden.
                    let _ = window.hide();
                    #[cfg(target_os = "windows")]
                    let _ = window.set_skip_taskbar(true);
                    #[cfg(target_os = "macos")]
                    if let Err(e) = tray::apply_tray_policy(app.handle(), false) {
                        log::warn!("Failed to apply silent-startup tray policy: {e}");
                    }
                    log::info!("Silent startup enabled: main window hidden");
                } else {
                    // Normal startup: show the main window.
                    let _ = window.show();
                    log::info!("Normal startup: main window shown");

                    // Linux: work around first-launch UI stalls (Tauri #10746 + wry #637).
                    // When startup leaves the webview unfocused and surface sizing
                    // negotiation incomplete, clicks can be ignored. This focus + nudge
                    // sequence is the invisible equivalent of maximize-then-restore.
                    #[cfg(target_os = "linux")]
                    {
                        linux_fix::nudge_main_window(window.clone());
                    }
                }
            }


            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_providers,
            commands::get_current_provider,
            commands::add_provider,
            commands::update_provider,
            commands::delete_provider,
            commands::remove_provider_from_live_config,
            commands::switch_provider,
            commands::get_ssh_config_hosts,
            commands::apply_provider_to_remote,
            commands::inspect_remote_provider,
            commands::import_remote_provider,
            commands::sync_remote_session_usage,
            commands::get_remote_agent_status,
            commands::deploy_remote_agent,
            commands::switch_remote_agent_provider,
            commands::stop_remote_agent,
            commands::get_remote_agent_logs,
            commands::import_default_config,
            commands::get_claude_desktop_status,
            commands::get_claude_desktop_default_routes,
            commands::import_claude_desktop_providers_from_claude,
            commands::ensure_claude_desktop_official_provider,
            commands::get_claude_config_status,
            commands::get_config_status,
            commands::get_claude_code_config_path,
            commands::get_config_dir,
            commands::open_config_folder,
            commands::pick_directory,
            commands::open_external,
            commands::get_init_error,
            commands::get_migration_result,
            commands::get_skills_migration_result,
            commands::get_app_config_path,
            commands::open_app_config_folder,
            commands::get_claude_common_config_snippet,
            commands::set_claude_common_config_snippet,
            commands::get_common_config_snippet,
            commands::set_common_config_snippet,
            commands::extract_common_config_snippet,
            commands::read_live_provider_settings,
            commands::get_settings,
            commands::save_settings,
            commands::has_codex_unify_history_backup,
            commands::restore_codex_unified_history,
            commands::get_rectifier_config,
            commands::set_rectifier_config,
            commands::get_optimizer_config,
            commands::set_optimizer_config,
            commands::get_copilot_optimizer_config,
            commands::set_copilot_optimizer_config,
            commands::get_log_config,
            commands::set_log_config,
            commands::restart_app,
            commands::install_update_and_restart,
            commands::check_app_update_available,
            commands::check_for_updates,
            commands::is_portable_mode,
            commands::copy_text_to_clipboard,
            commands::get_claude_plugin_status,
            commands::read_claude_plugin_config,
            commands::apply_claude_plugin_config,
            commands::is_claude_plugin_applied,
            commands::apply_claude_onboarding_skip,
            commands::clear_claude_onboarding_skip,
            // Claude MCP management
            commands::get_claude_mcp_status,
            commands::read_claude_mcp_config,
            commands::upsert_claude_mcp_server,
            commands::delete_claude_mcp_server,
            commands::validate_mcp_command,
            // usage query
            commands::queryProviderUsage,
            commands::testUsageScript,
            // subscription quota
            commands::get_subscription_quota,
            commands::get_codex_oauth_quota,
            commands::get_codex_oauth_models,
            commands::get_coding_plan_quota,
            commands::get_balance,
            // New MCP via config.json (SSOT)
            commands::get_mcp_config,
            commands::upsert_mcp_server_in_config,
            commands::delete_mcp_server_in_config,
            commands::set_mcp_enabled,
            // Unified MCP management
            commands::get_mcp_servers,
            commands::upsert_mcp_server,
            commands::delete_mcp_server,
            commands::toggle_mcp_app,
            commands::import_mcp_from_apps,
            // Prompt management
            commands::get_prompts,
            commands::upsert_prompt,
            commands::delete_prompt,
            commands::enable_prompt,
            commands::import_prompt_from_file,
            commands::get_current_prompt_file_content,
            // model list fetch (OpenAI-compatible /v1/models)
            commands::fetch_models_for_config,
            // ours: endpoint speed test + custom endpoint management
            commands::test_api_endpoints,
            commands::get_custom_endpoints,
            commands::add_custom_endpoint,
            commands::remove_custom_endpoint,
            commands::update_endpoint_last_used,
            // app_config_dir override via Store
            commands::get_app_config_dir_override,
            commands::set_app_config_dir_override,
            // provider sort order management
            commands::update_providers_sort_order,
            // theirs: config import/export and dialogs
            commands::export_config_to_file,
            commands::import_config_from_file,
            commands::webdav_test_connection,
            commands::webdav_sync_upload,
            commands::webdav_sync_download,
            commands::webdav_sync_save_settings,
            commands::webdav_sync_fetch_remote_info,
            commands::s3_test_connection,
            commands::s3_sync_upload,
            commands::s3_sync_download,
            commands::s3_sync_save_settings,
            commands::s3_sync_fetch_remote_info,
            commands::save_file_dialog,
            commands::open_file_dialog,
            commands::open_zip_file_dialog,
            commands::create_db_backup,
            commands::list_db_backups,
            commands::restore_db_backup,
            commands::rename_db_backup,
            commands::delete_db_backup,
            commands::sync_current_providers_live,
            // Deep link import
            commands::parse_deeplink,
            commands::merge_deeplink_config,
            commands::import_from_deeplink,
            commands::import_from_deeplink_unified,
            update_tray_menu,
            // Environment variable management
            commands::check_env_conflicts,
            commands::delete_env_vars,
            commands::restore_env_backup,
            // Skill management (v3.10.0+ unified)
            commands::get_installed_skills,
            commands::get_skill_backups,
            commands::delete_skill_backup,
            commands::install_skill_unified,
            commands::uninstall_skill_unified,
            commands::restore_skill_backup,
            commands::toggle_skill_app,
            commands::scan_unmanaged_skills,
            commands::import_skills_from_apps,
            commands::discover_available_skills,
            commands::check_skill_updates,
            commands::update_skill,
            commands::migrate_skill_storage,
            commands::search_skills_sh,
            // Skill management (legacy API compatibility)
            commands::get_skills,
            commands::get_skills_for_app,
            commands::install_skill,
            commands::install_skill_for_app,
            commands::uninstall_skill,
            commands::uninstall_skill_for_app,
            commands::get_skill_repos,
            commands::add_skill_repo,
            commands::remove_skill_repo,
            commands::install_skills_from_zip,
            // Auto launch
            commands::set_auto_launch,
            commands::get_auto_launch_status,
            // Proxy server management
            commands::start_proxy_server,
            commands::stop_proxy_server,
            commands::stop_proxy_with_restore,
            commands::get_proxy_takeover_status,
            commands::set_proxy_takeover_for_app,
            commands::get_proxy_status,
            commands::get_proxy_config,
            commands::update_proxy_config,
            // Global & Per-App Config
            commands::get_global_proxy_config,
            commands::update_global_proxy_config,
            commands::get_proxy_config_for_app,
            commands::update_proxy_config_for_app,
            commands::get_default_cost_multiplier,
            commands::set_default_cost_multiplier,
            commands::get_pricing_model_source,
            commands::set_pricing_model_source,
            commands::is_proxy_running,
            commands::is_live_takeover_active,
            commands::switch_proxy_provider,
            // Proxy failover commands
            commands::get_provider_health,
            commands::reset_circuit_breaker,
            commands::get_circuit_breaker_config,
            commands::update_circuit_breaker_config,
            commands::get_circuit_breaker_stats,
            // Failover queue management
            commands::get_failover_queue,
            commands::get_available_providers_for_failover,
            commands::add_to_failover_queue,
            commands::remove_from_failover_queue,
            commands::get_auto_failover_enabled,
            commands::set_auto_failover_enabled,
            // Usage statistics
            commands::get_usage_summary,
            commands::get_usage_summary_by_app,
            commands::get_usage_trends,
            commands::get_provider_stats,
            commands::get_model_stats,
            commands::get_request_logs,
            commands::get_request_detail,
            commands::get_model_pricing,
            commands::update_model_pricing,
            commands::delete_model_pricing,
            commands::check_provider_limits,
            // Session usage sync
            commands::sync_session_usage,
            commands::get_usage_data_sources,
            commands::delete_remote_usage_data,
            // Stream health check
            commands::stream_check_provider,
            commands::stream_check_all_providers,
            commands::get_stream_check_config,
            commands::save_stream_check_config,
            // Session manager
            commands::list_sessions,
            commands::get_session_messages,
            commands::delete_session,
            commands::delete_sessions,
            commands::launch_session_terminal,
            commands::get_tool_versions,
            commands::run_tool_lifecycle_action,
            commands::probe_tool_installations,
            // Provider terminal
            commands::open_provider_terminal,
            // Universal Provider management
            commands::get_universal_providers,
            commands::get_universal_provider,
            commands::upsert_universal_provider,
            commands::delete_universal_provider,
            commands::sync_universal_provider,
            // OpenCode specific
            commands::import_opencode_providers_from_live,
            commands::get_opencode_live_provider_ids,
            // OpenClaw specific
            commands::import_openclaw_providers_from_live,
            commands::get_openclaw_live_provider_ids,
            commands::get_openclaw_live_provider,
            commands::scan_openclaw_config_health,
            commands::get_openclaw_default_model,
            commands::set_openclaw_default_model,
            commands::get_openclaw_model_catalog,
            commands::set_openclaw_model_catalog,
            commands::get_openclaw_agents_defaults,
            commands::set_openclaw_agents_defaults,
            commands::get_openclaw_env,
            commands::set_openclaw_env,
            commands::get_openclaw_tools,
            commands::set_openclaw_tools,
            // Hermes specific
            commands::import_hermes_providers_from_live,
            commands::get_hermes_live_provider_ids,
            commands::get_hermes_live_provider,
            commands::get_hermes_model_config,
            commands::open_hermes_web_ui,
            commands::launch_hermes_dashboard,
            commands::get_hermes_memory,
            commands::set_hermes_memory,
            commands::get_hermes_memory_limits,
            commands::set_hermes_memory_enabled,
            // Global upstream proxy
            commands::get_global_proxy_url,
            commands::set_global_proxy_url,
            commands::test_proxy_url,
            commands::get_upstream_proxy_status,
            commands::scan_local_proxies,
            // Window theme control
            commands::set_window_theme,
            // Generic managed auth commands
            commands::auth_start_login,
            commands::auth_poll_for_account,
            commands::auth_list_accounts,
            commands::auth_get_status,
            commands::auth_remove_account,
            commands::auth_set_default_account,
            commands::auth_logout,
            // Copilot OAuth commands (multi-account support)
            commands::copilot_start_device_flow,
            commands::copilot_poll_for_auth,
            commands::copilot_poll_for_account,
            commands::copilot_list_accounts,
            commands::copilot_remove_account,
            commands::copilot_set_default_account,
            commands::copilot_get_auth_status,
            commands::copilot_logout,
            commands::copilot_is_authenticated,
            commands::copilot_get_token,
            commands::copilot_get_token_for_account,
            commands::copilot_get_models,
            commands::copilot_get_models_for_account,
            commands::copilot_get_usage,
            commands::copilot_get_usage_for_account,
            // OMO commands
            commands::read_omo_local_file,
            commands::get_current_omo_provider_id,
            commands::disable_current_omo,
            commands::read_omo_slim_local_file,
            commands::get_current_omo_slim_provider_id,
            commands::disable_current_omo_slim,
            // Workspace files (OpenClaw)
            commands::read_workspace_file,
            commands::write_workspace_file,
            // Daily memory files (OpenClaw workspace)
            commands::list_daily_memory_files,
            commands::read_daily_memory_file,
            commands::write_daily_memory_file,
            commands::delete_daily_memory_file,
            commands::search_daily_memory_files,
            commands::open_workspace_directory,
            // lightweight mode (for testing or low-resource environments)
            commands::enter_lightweight_mode,
            commands::exit_lightweight_mode,
            commands::is_lightweight_mode,
        ]);

    let app = builder
        .build(tauri::generate_context!())
        .expect("error while running tauri application");

    app.run(|app_handle, event| {
        // Handle exit requests on all platforms.
        if let RunEvent::ExitRequested { api, code, .. } = &event {
            match classify_exit_request(*code) {
                // `code == None` means the runtime triggered the exit automatically
                // (for example because a hidden window's WebView was reclaimed).
                // Keep the app alive in the tray.
                ExitRequestAction::StayInTray => {
                    log::info!("Runtime-triggered exit request with no live windows; keeping the app running in the tray");
                    api.prevent_exit();
                    return;
                }
                // `code == RESTART_EXIT_CODE` means `app.restart()` or updater relaunch.
                // On this path Tauri ignores `prevent_exit()`, exits the event loop, then
                // re-execs the new binary after `RunEvent::Exit` (including updated
                // executable naming on macOS via Info.plist).
                //
                // Never reuse the async cleanup task below for this path. That task calls
                // `save_window_state` from a tokio thread while holding the window-state
                // plugin lock and querying geometry on the main thread. At restart time
                // the main thread is exiting the event loop and the plugin's own
                // `RunEvent::Exit` hook waits on the same lock, causing a permanent
                // deadlock (#3998).
                //
                // Let Tauri handle restart normally:
                // - Window state is saved by the plugin's Exit hook on the main thread
                // - The tray icon is cleaned up by Tauri's own cleanup_before_exit / Drop
                // - Proxy / Live config does not need restoration because the new process
                //   immediately takes over again
                // - No extra 100 ms flush wait is needed; command-driven DB writes are
                //   already complete, matching normal Tauri restart behavior
                ExitRequestAction::DeferToTauriRestart => {
                    log::info!("Received restart request (code={code:?}); deferring to Tauri's default re-exec flow");
                    return;
                }
                // Any other `Some(_)` means the user explicitly requested exit
                // (for example from the tray menu), so run cleanup first.
                ExitRequestAction::CleanupAndExit => {}
            }

            log::info!("Received user-initiated exit request (code={code:?}); starting cleanup...");
            api.prevent_exit();

            let app_handle = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                save_window_state_before_exit(&app_handle);
                cleanup_before_exit(&app_handle).await;
                // Explicitly remove the tray icon before `std::process::exit`.
                // A direct process exit bypasses Tauri runtime Drop, so Windows Shell
                // never receives NIM_DELETE and the dead icon can remain cached until
                // the tray repaints on hover.
                remove_tray_icon_before_exit(&app_handle);
                log::info!("Cleanup finished; exiting app");

                // Briefly wait so pending I/O such as DB writes reaches disk.
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;

                // Use `std::process::exit` to avoid triggering ExitRequested again.
                std::process::exit(0);
            });
            return;
        }

        #[cfg(target_os = "macos")]
        {
            match event {
                // macOS emits Reopen when the Dock icon reactivates the app, so restore
                // the main window manually here.
                RunEvent::Reopen { .. } => {
                    if app_handle.get_webview_window("main").is_some() {
                        if let Err(e) = present_main_window(app_handle, "macOS reopen") {
                            log::error!("Failed to present main window after reopen: {e}");
                        }
                    } else if crate::lightweight::is_lightweight_mode() {
                        if let Err(e) = crate::lightweight::exit_lightweight_mode(app_handle) {
                            log::error!("Failed to rebuild window while leaving lightweight mode: {e}");
                        }
                    }
                }
                // Handle open events triggered by the custom URL scheme (for example `nexus://...`).
                RunEvent::Opened { urls } => {
                    if let Some(url) = urls.first() {
                        let url_str = url.to_string();
                        log::info!("RunEvent::Opened with URL: {url_str}");

                        if url_str.starts_with("nexus://") {
                            if crate::lightweight::is_lightweight_mode() {
                                if let Err(e) = crate::lightweight::exit_lightweight_mode(app_handle)
                                {
                                    log::error!("Failed to rebuild window while leaving lightweight mode: {e}");
                                }
                            }

                            // Parse and broadcast the deep-link event using the same logic
                            // as the single-instance handler.
                            match crate::deeplink::parse_deeplink_url(&url_str) {
                                Ok(request) => {
                                    log::info!(
                                        "Successfully parsed deep link from RunEvent::Opened: resource={}, app={:?}",
                                        request.resource,
                                        request.app
                                    );

                                    if let Err(e) =
                                        app_handle.emit("deeplink-import", &request)
                                    {
                                        log::error!(
                                            "Failed to emit deep link event from RunEvent::Opened: {e}"
                                        );
                                    }
                                }
                                Err(e) => {
                                    log::error!(
                                        "Failed to parse deep link URL from RunEvent::Opened: {e}"
                                    );

                                    if let Err(emit_err) = app_handle.emit(
                                        "deeplink-error",
                                        serde_json::json!({
                                            "url": url_str,
                                            "error": e.to_string()
                                        }),
                                    ) {
                                        log::error!(
                                            "Failed to emit deep link error event from RunEvent::Opened: {emit_err}"
                                        );
                                    }
                                }
                            }

                            // Ensure the main window is visible.
                            if let Err(e) = present_main_window(app_handle, "opened URL") {
                                log::error!("Failed to present main window for opened URL: {e}");
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = (app_handle, event);
        }
    });
}

// ============================================================
// App-exit cleanup
// ============================================================

/// Cleanup work that must happen before the app exits.
///
/// Check proxy-server state before exit. If the proxy is running, stop it and
/// restore the Live config so Claude Code / Codex / Gemini do not remain in a
/// broken state. `stop_with_restore_keep_state` preserves settings-table proxy
/// state so startup can restore it later.
pub async fn cleanup_before_exit(app_handle: &tauri::AppHandle) {
    if let Some(state) = app_handle.try_state::<store::AppState>() {
        let proxy_service = &state.proxy_service;

        // Exit also needs a fallback path: the proxy may already have crashed or
        // stopped while takeover artifacts still remain in Live config.
        let has_backups = match state.db.has_any_live_backup().await {
            Ok(v) => v,
            Err(e) => {
                log::error!("Failed to check Live backups during exit: {e}");
                false
            }
        };
        let live_taken_over = proxy_service.detect_takeover_in_live_configs();
        let needs_restore = has_backups || live_taken_over;

        if needs_restore {
            log::info!("Detected leftover takeover state; restoring Live config while preserving proxy state...");
            // Use the keep_state variant so the settings-table proxy state survives.
            if let Err(e) = proxy_service.stop_with_restore_keep_state().await {
                log::error!("Failed to restore Live config during exit: {e}");
            } else {
                log::info!("Live config restored and proxy state preserved for next startup");
            }
            return;
        }

        // Without takeover, just stop the proxy if it is still running.
        if proxy_service.is_running().await {
            log::info!("Proxy server is still running; stopping it...");
            if let Err(e) = proxy_service.stop_server().await {
                log::error!("Failed to stop proxy during exit: {e}");
            }
            log::info!("Proxy server cleanup finished");
        }
    }
}

/// Explicitly remove the tray icon from the system tray.
///
/// `std::process::exit` bypasses the Tauri runtime, so `TrayIcon::drop()` does
/// not run and Windows Shell never receives `NIM_DELETE`. The result is a dead
/// cached icon that can remain visible until the tray repaints on hover.
///
/// `set_visible(false)` uses the `WM_USER_HIDE_TRAYICON` path, which triggers
/// tray-icon's internal `remove_tray_icon` ->
/// `Shell_NotifyIconW(NIM_DELETE)` before the process ends. On other platforms
/// it is also a normal hide/remove operation, so it is safe as a cross-platform
/// fallback.
pub(crate) fn remove_tray_icon_before_exit(app_handle: &tauri::AppHandle) {
    if let Some(tray) = app_handle.tray_by_id(tray::TRAY_ID) {
        if let Err(e) = tray.set_visible(false) {
            log::warn!("Failed to remove tray icon during exit: {e}");
        } else {
            log::info!("Removed tray icon from the system tray explicitly");
        }
    }
}

// ============================================================
// Restore proxy state at startup
// ============================================================

/// Restore proxy services automatically at startup from `proxy_config`.
///
/// If any app has `proxy_config.enabled = true`, start the proxy service and
/// reapply takeover to that app's Live config.
async fn restore_proxy_state_on_startup(state: &store::AppState) {
    // Collect apps that still need takeover restored.
    let mut apps_to_restore = Vec::new();
    for app_type in ["claude", "codex", "gemini"] {
        if let Ok(config) = state.db.get_proxy_config_for_app(app_type).await {
            if config.enabled {
                apps_to_restore.push(app_type);
            }
        }
    }

    let global_proxy_enabled = state
        .db
        .get_global_proxy_config()
        .await
        .map(|config| config.proxy_enabled)
        .unwrap_or(false);

    if apps_to_restore.is_empty() && !global_proxy_enabled {
        log::debug!("No proxy state needs restoration at startup");
        return;
    }

    log::info!("Detected proxy state to restore from the previous run: {apps_to_restore:?}");

    // Restore takeover app by app.
    for app_type in apps_to_restore {
        match state
            .proxy_service
            .set_takeover_for_app(app_type, true)
            .await
        {
            Ok(()) => {
                log::info!("✓ Restored proxy takeover state for {app_type}");
            }
            Err(e) => {
                log::error!("✗ Failed to restore proxy takeover state for {app_type}: {e}");
                // Clear the flag on failure so the next startup does not loop forever.
                if let Err(clear_err) = state
                    .proxy_service
                    .set_takeover_for_app(app_type, false)
                    .await
                {
                    log::error!("Failed to clear proxy state for {app_type}: {clear_err}");
                }
            }
        }
    }

    if global_proxy_enabled && !state.proxy_service.is_running().await {
        if let Err(error) = state.proxy_service.start().await {
            log::error!("Failed to restore the local proxy: {error}");
            return;
        }
    }

    if state.proxy_service.is_running().await {
        if let Err(error) = services::proxy::sync_current_claude_desktop_proxy_profile(state) {
            log::error!("Failed to restore the Claude Desktop proxy profile: {error}");
        }
    }
}

fn initialize_common_config_snippets(state: &store::AppState) {
    // Auto-extract common config snippets from clean live files when snippet is missing.
    // This must run before proxy takeover is restored on startup, otherwise we'd read
    // proxy-placeholder configs instead of the user's actual live settings.
    for app_type in crate::app_config::AppType::all() {
        if !state
            .db
            .should_auto_extract_config_snippet(app_type.as_str())
            .unwrap_or(false)
        {
            continue;
        }

        let settings = match crate::services::provider::ProviderService::read_live_settings(
            app_type.clone(),
        ) {
            Ok(s) => s,
            Err(_) => continue,
        };

        match crate::services::provider::ProviderService::extract_common_config_snippet_from_settings(
            app_type.clone(),
            &settings,
        ) {
            Ok(snippet) if !snippet.is_empty() && snippet != "{}" => {
                match state.db.set_config_snippet(app_type.as_str(), Some(snippet)) {
                    Ok(()) => {
                        let _ = state.db.set_config_snippet_cleared(app_type.as_str(), false);
                        log::info!(
                            "✓ Auto-extracted common config snippet for {}",
                            app_type.as_str()
                        );
                    }
                    Err(e) => log::warn!(
                        "✗ Failed to save config snippet for {}: {e}",
                        app_type.as_str()
                    ),
                }
            }
            Ok(_) => log::debug!(
                "○ Live config for {} has no extractable common fields",
                app_type.as_str()
            ),
            Err(e) => log::warn!(
                "✗ Failed to extract config snippet for {}: {e}",
                app_type.as_str()
            ),
        }
    }

    let should_run_legacy_migration = state
        .db
        .is_legacy_common_config_migrated()
        .map(|done| !done)
        .unwrap_or(true);

    if should_run_legacy_migration {
        for app_type in [
            crate::app_config::AppType::Claude,
            crate::app_config::AppType::Codex,
            crate::app_config::AppType::Gemini,
        ] {
            if let Err(e) = crate::services::provider::ProviderService::migrate_legacy_common_config_usage_if_needed(
                state,
                app_type.clone(),
            ) {
                log::warn!(
                    "✗ Failed to migrate legacy common-config usage for {}: {e}",
                    app_type.as_str()
                );
            }
        }

        if let Err(e) = state.db.set_legacy_common_config_migrated(true) {
            log::warn!("✗ Failed to persist legacy common-config migration flag: {e}");
        }
    }
}

// ============================================================
// Migration error dialog helpers
// ============================================================

/// Detect whether the native process locale prefers Vietnamese.
fn is_vietnamese_locale() -> bool {
    std::env::var("LANG")
        .or_else(|_| std::env::var("LC_ALL"))
        .or_else(|_| std::env::var("LC_MESSAGES"))
        .map(|lang| lang.starts_with("vi"))
        .unwrap_or(false)
}

/// Show the migration error dialog.
/// Returns true when the user chooses retry, false when the user exits.
fn show_migration_error_dialog(app: &tauri::AppHandle, error: &str) -> bool {
    let title = if is_vietnamese_locale() {
        "Di chuyển cấu hình thất bại"
    } else {
        "Migration Failed"
    };

    let message = if is_vietnamese_locale() {
        format!(
            "Đã xảy ra lỗi khi di chuyển cấu hình từ phiên bản cũ:\n\n{error}\n\n\
            Dữ liệu của bạn chưa bị mất; tệp cấu hình cũ vẫn được giữ lại.\n\
            Hãy cân nhắc quay lại phiên bản Nexus Composer cũ hơn để bảo vệ dữ liệu.\n\n\
            Nhấn 'Thử lại' để thử di chuyển lại\n\
            Nhấn 'Thoát' để đóng ứng dụng"
        )
    } else {
        format!(
            "An error occurred while migrating configuration:\n\n{error}\n\n\
            Your data is NOT lost - the old config file is still preserved.\n\
            Consider rolling back to an older Nexus Composer version.\n\n\
            Click 'Retry' to attempt migration again\n\
            Click 'Exit' to close the program"
        )
    };

    let retry_text = if is_vietnamese_locale() {
        "Thử lại"
    } else {
        "Retry"
    };
    let exit_text = if is_vietnamese_locale() {
        "Thoát"
    } else {
        "Exit"
    };

    // Wait synchronously for the user's choice.
    // OkCancelCustom returns true for the first button (retry) and false for the second (exit).
    app.dialog()
        .message(&message)
        .title(title)
        .kind(MessageDialogKind::Error)
        .buttons(MessageDialogButtons::OkCancelCustom(
            retry_text.to_string(),
            exit_text.to_string(),
        ))
        .blocking_show()
}

/// Show the database initialization / schema migration error dialog.
/// Returns true when the user chooses retry, false when the user exits.
fn show_database_init_error_dialog(
    app: &tauri::AppHandle,
    db_path: &std::path::Path,
    error: &str,
) -> bool {
    let title = if is_vietnamese_locale() {
        "Khởi tạo cơ sở dữ liệu thất bại"
    } else {
        "Database Initialization Failed"
    };

    let message = if is_vietnamese_locale() {
        format!(
            "Đã xảy ra lỗi khi khởi tạo hoặc nâng cấp cấu trúc cơ sở dữ liệu:\n\n{error}\n\n\
            Đường dẫn tệp cơ sở dữ liệu:\n{db}\n\n\
            Dữ liệu của bạn chưa bị mất; ứng dụng sẽ không tự động xóa tệp cơ sở dữ liệu.\n\
            Nguyên nhân thường gặp: phiên bản cơ sở dữ liệu mới hơn, tệp bị hỏng, thiếu quyền truy cập hoặc ổ đĩa sắp hết dung lượng.\n\n\
            Gợi ý:\n\
            1) Sao lưu toàn bộ thư mục cấu hình (bao gồm nexus-composer.db)\n\
            2) Nếu thấy thông báo về phiên bản cơ sở dữ liệu mới hơn, hãy nâng cấp Nexus Composer\n\
            3) Nếu lỗi xảy ra ngay sau khi nâng cấp, hãy quay lại phiên bản cũ để xuất hoặc sao lưu dữ liệu rồi nâng cấp lại\n\n\
            Nhấn 'Thử lại' để thử khởi tạo lại\n\
            Nhấn 'Thoát' để đóng ứng dụng",
            db = db_path.display()
        )
    } else {
        format!(
            "An error occurred while initializing or migrating the database:\n\n{error}\n\n\
            Database file path:\n{db}\n\n\
            Your data is NOT lost - the app will not delete the database automatically.\n\
            Common causes include: newer database version, corrupted file, permission issues, or low disk space.\n\n\
            Suggestions:\n\
            1) Back up the entire config directory (including nexus-composer.db)\n\
            2) If you see “database version is newer”, please upgrade Nexus Composer\n\
            3) If this happened right after upgrading, consider rolling back to export/backup then upgrade again\n\n\
            Click 'Retry' to attempt initialization again\n\
            Click 'Exit' to close the program",
            db = db_path.display()
        )
    };

    let retry_text = if is_vietnamese_locale() {
        "Thử lại"
    } else {
        "Retry"
    };
    let exit_text = if is_vietnamese_locale() {
        "Thoát"
    } else {
        "Exit"
    };

    app.dialog()
        .message(&message)
        .title(title)
        .kind(MessageDialogKind::Error)
        .buttons(MessageDialogButtons::OkCancelCustom(
            retry_text.to_string(),
            exit_text.to_string(),
        ))
        .blocking_show()
}

// ============================================================
// Exit request classification
// ============================================================

/// `RunEvent::ExitRequested` has three distinct sources that must be handled differently.
///
/// Key constraint: on restart requests (`code == RESTART_EXIT_CODE`), Tauri
/// silently ignores `prevent_exit()` (see `ExitRequestApi::prevent_exit`), so
/// the event loop will continue shutting down and fire plugin `RunEvent::Exit`
/// hooks. Any custom cleanup running concurrently can deadlock on shared state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExitRequestAction {
    /// `code == None`: runtime-triggered exit (for example a hidden window's
    /// WebView was reclaimed), so keep the app alive in the tray.
    StayInTray,
    /// `code == RESTART_EXIT_CODE`: restart requested by `app.restart()` or
    /// updater relaunch. Do not intercept or run custom cleanup.
    DeferToTauriRestart,
    /// Any other `Some(_)`: user-initiated exit, so run full async cleanup first.
    CleanupAndExit,
}

fn classify_exit_request(code: Option<i32>) -> ExitRequestAction {
    match code {
        None => ExitRequestAction::StayInTray,
        Some(tauri::RESTART_EXIT_CODE) => ExitRequestAction::DeferToTauriRestart,
        Some(_) => ExitRequestAction::CleanupAndExit,
    }
}

// ============================================================
// Explicitly persist window state before user-initiated exit
// ============================================================

fn window_state_flags() -> StateFlags {
    StateFlags::POSITION | StateFlags::SIZE | StateFlags::MAXIMIZED
}

/// The app's exit flow intercepts `ExitRequested` and eventually calls
/// `std::process::exit(0)`, so persist window state manually before the process
/// ends to avoid bypassing the plugin's normal exit hook.
pub fn save_window_state_before_exit(app_handle: &tauri::AppHandle) {
    if let Err(err) = app_handle.save_window_state(window_state_flags()) {
        log::error!("Failed to save window state before exit: {err}");
    } else {
        log::info!("Saved window state before exit");
    }
}

/// Explicitly release the single-instance lock.
///
/// macOS uses `/tmp/{identifier}.sock` for single-instance coordination. Some
/// paths call `std::process::exit(0)` directly, which bypasses the plugin's
/// `RunEvent::Exit` cleanup hook. Destroying the lock before restart prevents a
/// new process from connecting to the stale listener and exiting immediately.
pub fn destroy_single_instance_lock(app_handle: &tauri::AppHandle) {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    tauri_plugin_single_instance::destroy(app_handle);
}

/// Restart the app after cleaning up the tray icon and releasing the lock.
///
/// `tauri::process::restart` spawns a new process and then exits directly,
/// bypassing the event-loop shutdown path. That means Tauri's own
/// `cleanup_before_exit` and plugin `RunEvent::Exit` hooks do not run. The
/// caller must compensate for window-state and proxy/Live restoration, while
/// this function handles the tray icon and single-instance lock.
///
/// Intentionally do not call `AppHandle::cleanup_before_exit()`. It drops the
/// tray icon on the calling thread, but macOS requires NSStatusItem work on the
/// main thread. `set_visible(false)` goes through `run_item_main_thread`, which
/// is safe across threads (see `remove_tray_icon_before_exit`).
pub fn restart_process(app_handle: &tauri::AppHandle) -> ! {
    remove_tray_icon_before_exit(app_handle);
    destroy_single_instance_lock(app_handle);
    tauri::process::restart(&app_handle.env());
}

#[cfg(test)]
mod tests {
    use super::{classify_exit_request, ExitRequestAction};

    #[test]
    fn no_code_keeps_app_alive_in_tray() {
        assert_eq!(classify_exit_request(None), ExitRequestAction::StayInTray);
    }

    #[test]
    fn restart_exit_code_defers_to_tauri_default_restart() {
        assert_eq!(
            classify_exit_request(Some(tauri::RESTART_EXIT_CODE)),
            ExitRequestAction::DeferToTauriRestart
        );
    }

    #[test]
    fn user_exit_codes_run_cleanup_then_exit() {
        assert_eq!(
            classify_exit_request(Some(0)),
            ExitRequestAction::CleanupAndExit
        );
        assert_eq!(
            classify_exit_request(Some(1)),
            ExitRequestAction::CleanupAndExit
        );
    }
}
