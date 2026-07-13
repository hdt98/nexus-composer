mod app_config;
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
mod project_links;
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

pub(crate) fn redact_url_for_log(url_str: &str) -> String {
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

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DeepLinkErrorPayload {
    error: String,
    context: String,
}

fn deep_link_error_payload(url_str: &str, error: String) -> DeepLinkErrorPayload {
    DeepLinkErrorPayload {
        error,
        context: redact_url_for_log(url_str),
    }
}

fn is_supported_deeplink_url(url: &str) -> bool {
    url.starts_with("nexus://") || url.starts_with("ccswitch://")
}

#[cfg(any(target_os = "linux", test))]
fn linux_deep_link_handler_needs_registration(contents: Option<&str>) -> bool {
    let Some(contents) = contents else {
        return true;
    };

    !["x-scheme-handler/nexus", "x-scheme-handler/ccswitch"]
        .iter()
        .all(|scheme| {
            contents
                .lines()
                .filter_map(|line| line.trim().strip_prefix("MimeType="))
                .flat_map(|value| value.split(';'))
                .any(|mime_type| mime_type == *scheme)
        })
}

/// Bring the main window back to a user-visible, focused state.
pub(crate) fn present_main_window(app: &tauri::AppHandle, reason: &str) {
    if let Some(window) = app.get_webview_window("main") {
        #[cfg(target_os = "windows")]
        {
            let _ = window.set_skip_taskbar(false);
        }
        #[cfg(target_os = "macos")]
        {
            tray::apply_tray_policy(app, true);
        }
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        #[cfg(target_os = "linux")]
        {
            linux_fix::nudge_main_window(window.clone());
        }
        log::info!("Main window presented: {reason}");
    }
}

/// Handle a Nexus or legacy CC Switch deep-link URL through the shared path.
///
/// Parses the URL, emits `deeplink-import` or `deeplink-error` to the frontend,
/// and optionally focuses the main window after success.
fn handle_deeplink_url(
    app: &tauri::AppHandle,
    url_str: &str,
    focus_main_window: bool,
    source: &str,
) -> bool {
    if !is_supported_deeplink_url(url_str) {
        return false;
    }

    let redacted_url = redact_url_for_log(url_str);
    log::info!("✓ Deep link URL detected from {source}: {redacted_url}");

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
                present_main_window(app, "deep link");
            }
        }
        Err(e) => {
            log::error!("✗ Failed to parse deep link URL: {e}");

            if let Err(emit_err) = app.emit(
                "deeplink-error",
                deep_link_error_payload(url_str, e.to_string()),
            ) {
                log::error!("✗ Failed to emit deeplink-error event: {emit_err}");
            }
        }
    }

    true
}

/// Tauri command that refreshes the tray menu.
#[tauri::command]
async fn update_tray_menu(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<bool, String> {
    match tray::create_tray_menu(&app, state.inner()) {
        Ok(new_menu) => {
            if let Some(tray) = app.tray_by_id(tray::TRAY_ID) {
                tray.set_menu(Some(new_menu))
                    .map_err(|e| format!("Failed to update the tray menu: {e}"))?;
                return Ok(true);
            }
            Ok(false)
        }
        Err(err) => {
            log::error!("Failed to create the tray menu: {err}");
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
    // Install the panic hook, writing crashes to <app_config_dir>/crash.log
    // (default ~/.cc-switch/crash.log).
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
                    log::error!("Failed to recreate the window when leaving lightweight mode: {e}");
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
            present_main_window(app, "single-instance activation");
        }));
    }

    let builder = builder
        // Register the deep-link plugin for macOS AppleEvents and other platforms.
        .plugin(tauri_plugin_deep_link::init())
        // Intercept window close and apply the configured minimize-to-tray behavior.
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // Database-too-new recovery has no tray entry; close exits so the app
                // cannot remain invisibly in the background.
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
                        tray::apply_tray_policy(window.app_handle(), false);
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

            // Load Store overrides before resolving paths for logs, database, and other data.
            app_store::refresh_app_config_dir_override(app.handle());
            panic_hook::init_app_config_dir(crate::config::get_app_config_dir());
            #[cfg(target_os = "windows")]
            set_windows_app_user_model_id(app.handle());

            // Initialize single-file logging at <app_config_dir>/logs/nexus-composer.log.
            {
                use tauri_plugin_log::{RotationStrategy, Target, TargetKind, TimezoneStrategy};

                let log_dir = panic_hook::get_log_dir();

                // Ensure the log directory exists.
                if let Err(e) = std::fs::create_dir_all(&log_dir) {
                    eprintln!("Failed to create the log directory: {e}");
                }

                // Delete the previous log on startup for single-file replacement.
                let log_file_path = log_dir.join("nexus-composer.log");
                let _ = std::fs::remove_file(&log_file_path);

                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        // Start at Trace so log::set_max_level() can adjust it later.
                        .level(log::LevelFilter::Trace)
                        .targets([
                            Target::new(TargetKind::Stdout),
                            Target::new(TargetKind::Folder {
                                path: log_dir,
                                file_name: Some("nexus-composer".into()),
                            }),
                        ])
                        // Rotate at the size limit after deleting the old startup file.
                        // KeepSome computes n - 2, so 2 is the minimum safe value and
                        // represents retaining no rotated files.
                        .rotation_strategy(RotationStrategy::KeepSome(2))
                        // Limit the single file to 1 GiB.
                        .max_file_size(1024 * 1024 * 1024)
                        .timezone_strategy(TimezoneStrategy::UseLocal)
                        .build(),
                )?;
            }

            // Inject AppHandle into usage_events after logging starts, enabling write
            // paths without their own handle to emit `usage-log-recorded`.
            usage_events::init(app.handle().clone());

            // Initialize the database.
            let app_config_dir = crate::config::get_app_config_dir();
            let db_path = app_config_dir.join("cc-switch.db");
            let json_path = app_config_dir.join("config.json");

            // Determine whether config.json must migrate to SQLite.
            let has_json = json_path.exists();
            let has_db = db_path.exists();

            // Validate config.json before creating the database. If loading fails and
            // the user exits, no database is left behind and the next launch can retry.
            let migration_config = if !has_db && has_json {
                log::info!("Detected legacy configuration; validating it");

                // Loop so the user can retry loading the file.
                loop {
                    match crate::app_config::MultiAppConfig::load() {
                        Ok(config) => {
                            log::info!("Configuration file loaded successfully");
                            break Some(config);
                        }
                        Err(e) => {
                            log::error!("Failed to load legacy configuration: {e}");
                            // Ask the user through a system dialog.
                            if !show_migration_error_dialog(app.handle(), &e.to_string()) {
                                // The database does not exist yet, so a later launch can retry.
                                log::info!("User chose to exit");
                                std::process::exit(1);
                            }
                            // Continue the loop after Retry.
                            log::info!("User chose to retry loading configuration");
                        }
                    }
                }
            } else {
                None
            };

            // Create the database and run schema migrations.
            //
            // v3.8.* upgrades usually enter SQLite schema migration here. Surface a
            // clear error for corruption, permissions, or a newer user_version rather
            // than appearing to crash or fail to open.
            //
            // Preflight a newer database before create_tables performs any DROP/ALTER
            // DDL, preventing this older app from writing a schema it cannot understand.
            match crate::database::Database::stored_user_version_exceeds_supported(&db_path) {
                Ok(Some(version)) => {
                    log::warn!("Database version v{version} is too new; directing the user to upgrade");
                    crate::init_status::set_init_error(crate::init_status::InitErrorPayload {
                        path: db_path.display().to_string(),
                        error: format!(
                            "Database version {version} is newer than this app supports ({}). Upgrade the app and try again.",
                            crate::database::SCHEMA_VERSION
                        ),
                        kind: Some("db_version_too_new".to_string()),
                        db_version: Some(version),
                        supported_version: Some(crate::database::SCHEMA_VERSION),
                    });
                    // The main window starts hidden, so force the recovery UI visible.
                    present_main_window(app.handle(), "database recovery");
                    return Ok(());
                }
                Ok(None) => {}
                Err(e) => {
                    log::warn!("Database-version preflight failed; continuing normal initialization: {e}");
                }
            }

            let db = loop {
                match crate::database::Database::init() {
                    Ok(db) => break Arc::new(db),
                    Err(e) => {
                        log::error!("Failed to init database: {e}");

                        if !show_database_init_error_dialog(app.handle(), &db_path, &e.to_string())
                        {
                            log::info!("User chose to exit");
                            std::process::exit(1);
                        }

                        log::info!("User chose to retry database initialization");
                    }
                }
            };

            // Migrate any preloaded configuration.
            if let Some(config) = migration_config {
                log::info!("Starting data migration");

                match db.migrate_from_json(&config) {
                    Ok(_) => {
                        log::info!("Configuration migration succeeded");
                        // Expose success for a frontend toast.
                        crate::init_status::set_migration_success();
                        // Archive rather than delete the legacy file so it can be recovered.
                        let archive_path = json_path.with_extension("json.migrated");
                        if let Err(e) = std::fs::rename(&json_path, &archive_path) {
                            log::warn!("Failed to archive the legacy configuration file: {e}");
                        } else {
                            log::info!("Legacy configuration archived as config.json.migrated");
                        }
                    }
                    Err(e) => {
                        // A migration failure after successful loading is rare, such as
                        // a full disk; log it and continue importing existing configuration.
                        log::error!("Configuration migration failed: {e}; importing from existing configuration");
                    }
                }
            }

            let app_state = AppState::new(db);

            match app_state.db.normalize_legacy_nexus_provider_names() {
                Ok(count) if count > 0 => {
                    log::info!("Normalized {count} legacy Nexus GLM-5.2 provider name(s)");
                }
                Ok(_) => {}
                Err(e) => log::warn!("Failed to normalize legacy Nexus provider names: {e}"),
            }

            // Store AppHandle for UI updates during proxy failover.
            app_state.proxy_service.set_app_handle(app.handle().clone());

            // ============================================================
            // Evaluate imports per table so one data class cannot block another.
            // ============================================================

            // 1. Initialize default Skill repositories; the helper skips a non-empty table.
            match app_state.db.init_default_skill_repos() {
                Ok(count) if count > 0 => {
                    log::info!("✓ Initialized {count} default skill repositories");
                }
                Ok(_) => {} // Silently skip a non-empty table.
                Err(e) => log::warn!("✗ Failed to initialize default skill repos: {e}"),
            }

            // 1.1. After schema v3 migration, import Skills from application
            // directories into the SSOT when skills_ssot_migration_pending is true.
            match app_state.db.get_setting("skills_ssot_migration_pending") {
                Ok(Some(flag)) if flag == "true" || flag == "1" => {
                    // Never clear and rebuild when the user already has v3 Skill data.
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
                                // Retain the pending flag so the next launch can retry.
                            }
                        }
                    }
                }
                Ok(_) => {} // Silently skip when migration is not pending.
                Err(e) => log::warn!("✗ Failed to read skills migration flag: {e}"),
            }

            // 1.5. Import live configuration and seed official Claude/Codex/Gemini presets.
            //
            // Import before seeding intentionally captures user-managed settings.json,
            // auth.json, or .env as the current "default" provider, then appends official
            // presets as non-current. Backfill protects the original live data on switch.
            //
            // Capture first-launch state so new users see the Nexus Composer welcome
            // dialog. On read failure, omit it rather than disrupting the user.
            let first_run_already_confirmed = crate::settings::get_settings()
                .first_run_notice_confirmed
                .unwrap_or(false);
            let fresh_install_at_startup =
                app_state.db.is_providers_empty().unwrap_or(false);

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

            {
                let db_for_codex_history_migration = app_state.db.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    match crate::codex_history_migration::maybe_migrate_codex_third_party_history_provider_bucket(
                        &db_for_codex_history_migration,
                    ) {
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

                    // Retry incomplete official-history migration at startup when
                    // unified sessions remain enabled, such as after a locked file.
                    // The function gates itself and skips when the toggle is off.
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

            // fresh_install_at_startup filters existing or acknowledged installations.
            // Only the frontend writes acknowledgement after the user confirms it.
            if !first_run_already_confirmed && fresh_install_at_startup {
                log::info!("✓ First-run welcome notice pending");
            }

            // 1.6. Synchronize OpenCode/OpenClaw live providers into the database.
            //
            // Additive-mode import is idempotent by ID and skips existing providers,
            // so running on every launch is safe. New installations see live providers
            // immediately, while external live-file changes synchronize after restart
            // without the old manual Import Current Configuration action.
            //
            // read_*_config returns empty defaults for absent files, so a fresh
            // installation without live data follows Ok(0) without noisy errors.
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

            // 2. Import local OMO configuration when no OMO provider exists in the database.
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

            // 3. Import MCP servers when their table is empty.
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

            // 4. Import prompt files when their table is empty.
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

            // Migrate legacy app_config_dir configuration to Store.
            if let Err(e) = app_store::migrate_app_config_dir_from_settings(app.handle()) {
                log::warn!("Failed to migrate app_config_dir: {e}");
            }

            // Do not save unconditionally during startup; that could overwrite user data.

            // Register the deep-link URL handler through DeepLinkExt.
            log::info!("=== Registering deep-link URL handler ===");

            // Linux and Windows debug builds require explicit registration.
            #[cfg(any(target_os = "linux", all(debug_assertions, windows)))]
            {
                #[cfg(target_os = "linux")]
                {
                    // Tauri writes this handler beneath the app-specific data directory.
                    // Existing Nexus installs may have a handler created before the legacy
                    // ccswitch scheme was restored, so existence alone is insufficient.
                    let should_register = app.path().data_dir().map_or(true, |data_dir| {
                        let handler =
                            data_dir.join("applications/nexus-composer-handler.desktop");
                        let contents = std::fs::read_to_string(handler).ok();
                        linux_deep_link_handler_needs_registration(contents.as_deref())
                    });

                    if should_register {
                        if let Err(e) = app.deep_link().register_all() {
                            log::error!("✗ Failed to register deep link schemes: {}", e);
                        } else {
                            log::info!("✓ Deep link schemes registered (Linux)");
                        }
                    } else {
                        log::info!("⊘ Deep link handler already registers all configured schemes");
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

            // Register the cross-platform URL callback.
            app.deep_link().on_open_url({
                let app_handle = app.handle().clone();
                move |event| {
                    log::info!("=== Deep Link Event Received (on_open_url) ===");
                    let urls = event.urls();
                    log::info!("Received {} URL(s)", urls.len());

                    if crate::lightweight::is_lightweight_mode() {
                        if let Err(e) = crate::lightweight::exit_lightweight_mode(&app_handle) {
                            log::error!("Failed to recreate the window when leaving lightweight mode: {e}");
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
                .tooltip("Nexus Composer") // Hover tooltip.
                .on_tray_icon_event(|tray, event| match event {
                    // Refresh usage asynchronously on tray hover/click so the next
                    // menu view has fresher values. The helper debounces for 10 seconds.
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

            // Use the platform-specific tray icon; macOS uses a template for light/dark mode.
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
            // Store the same instance globally to avoid divergence from duplicates.
            app.manage(app_state);

            // Load and apply logging configuration from the database.
            {
                let db = &app.state::<AppState>().db;
                if let Ok(log_config) = db.get_log_config() {
                    log::set_max_level(log_config.to_level_filter());
                    log::info!(
                        "Loaded logging configuration: enabled={}, level={}",
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

            // Initialize CodexOAuthManager for ChatGPT Plus/Pro proxying.
            {
                use crate::proxy::providers::codex_oauth_auth::CodexOAuthManager;
                use commands::CodexOAuthState;
                use tokio::sync::RwLock;

                let app_config_dir = crate::config::get_app_config_dir();
                let codex_oauth_manager = CodexOAuthManager::new(app_config_dir);
                app.manage(CodexOAuthState(Arc::new(RwLock::new(codex_oauth_manager))));
                log::info!("✓ CodexOAuthManager initialized");
            }

            // Initialize the global outbound-proxy HTTP client.
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

            // Recover from abnormal exit and restore proxy state.
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = app_handle.state::<AppState>();

                // Live backups may indicate takeover during the previous abnormal exit.
                let has_backups = match state.db.has_any_live_backup().await {
                    Ok(v) => v,
                    Err(e) => {
                        log::error!("Failed to inspect live backups: {e}");
                        false
                    }
                };
                // Check whether live configuration still contains takeover placeholders.
                let live_taken_over = state.proxy_service.detect_takeover_in_live_configs();

                if has_backups || live_taken_over {
                    log::warn!("Detected takeover residue from an abnormal exit; restoring live configuration");
                    if let Err(e) = state.proxy_service.recover_from_crash().await {
                        log::error!("Failed to restore live configuration: {e}");
                    } else {
                        log::info!("Live configuration restored");
                    }
                }

                initialize_common_config_snippets(&state);

                // Restore proxy service from state recorded in settings.
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

                // Synchronize session-log usage at startup and every 60 seconds.
                let db_for_session_sync = state.db.clone();
                tauri::async_runtime::spawn(async move {
                    const SESSION_SYNC_INTERVAL_SECS: u64 = 60;

                    fn run_step<T>(name: &str, result: Result<T, crate::error::AppError>) {
                        if let Err(e) = result {
                            log::warn!("{name} failed: {e}");
                        }
                    }

                    let db = &db_for_session_sync;

                    // Initial synchronization.
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

                    // Periodic synchronization.
                    let mut interval = tokio::time::interval(std::time::Duration::from_secs(
                        SESSION_SYNC_INTERVAL_SECS,
                    ));
                    interval.tick().await; // skip immediate first tick
                    loop {
                        interval.tick().await;
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
                    }
                });
            });

            // Linux: disable WebKitGTK acceleration to prevent a blank window after EGL failure.
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

            // Apply silent-startup settings to main-window visibility.
            let settings = crate::settings::get_settings();
            if let Some(window) = app.get_webview_window("main") {
                // Synchronize decorations before first display to avoid title-bar
                // flicker. This is Linux-only and fixes Wayland window controls.
                #[cfg(target_os = "linux")]
                let _ = window.set_decorations(!settings.use_app_window_controls);
                if settings.silent_startup {
                    // Silent startup keeps the window hidden.
                    let _ = window.hide();
                    #[cfg(target_os = "windows")]
                    let _ = window.set_skip_taskbar(true);
                    #[cfg(target_os = "macos")]
                    tray::apply_tray_policy(app.handle(), false);
                    log::info!("Silent startup: main window hidden");
                } else {
                    // Normal startup shows the window.
                    present_main_window(app.handle(), "normal startup");
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
            commands::save_pricing_defaults,
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
            commands::reset_model_pricing_to_defaults,
            commands::check_provider_limits,
            // Session usage sync
            commands::sync_session_usage,
            commands::get_usage_data_sources,
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
            commands::get_openclaw_workspace_paths,
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

    let mut restart_requested = false;
    app.run(move |app_handle, event| {
        // Handle exit requests on every platform.
        if let RunEvent::ExitRequested { api, code, .. } = &event {
            match classify_exit_request(*code) {
                // None is runtime-generated, such as after a hidden WebView is
                // reclaimed and no window survives. Prevent exit and keep the tray alive.
                ExitRequestAction::StayInTray => {
                    log::info!("Runtime requested exit without a live window; keeping the tray process running");
                    api.prevent_exit();
                    return;
                }
                // RESTART_EXIT_CODE comes from app.restart().
                // Tauri ignores prevent_exit here, exits the event loop, then re-execs
                // after RunEvent::Exit (macOS resolves the executable via updated Info.plist).
                //
                // Never reuse the asynchronous cleanup below. Its Tokio thread calls
                // save_window_state, holding the plugin lock while querying geometry
                // from the main thread. During event-loop exit, the plugin Exit hook
                // waits for that lock, causing a permanent restart deadlock (#3998).
                //
                // Let Tauri handle restart: the plugin saves window state on the main
                // thread, normal Drop removes the tray icon, the new instance resumes
                // proxy/live takeover, and command-driven database writes are already complete.
                ExitRequestAction::DeferToTauriRestart => {
                    restart_requested = true;
                    log::info!("Received restart request (code={code:?}); delegating re-exec to Tauri");
                    return;
                }
                // Other Some values are explicit app.exit(), such as Exit from the tray;
                // perform cleanup before termination.
                ExitRequestAction::CleanupAndExit => {}
            }

            log::info!("Received explicit user exit (code={code:?}); starting cleanup");
            api.prevent_exit();

            let app_handle = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                save_window_state_before_exit(&app_handle);
                cleanup_before_exit(&app_handle).await;
                // Remove the tray icon before std::process::exit. Direct termination
                // bypasses Tauri Drop and Windows NIM_DELETE, leaving a stale icon until hover.
                remove_tray_icon_before_exit(&app_handle);
                log::info!("Cleanup complete; exiting application");

                // Briefly allow pending I/O, such as database writes, to flush.
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;

                // Use std::process::exit to avoid another ExitRequested event.
                std::process::exit(0);
            });
            return;
        }

        // Native macOS application-menu, Cmd-Q, and Dock Quit can reach the
        // final Exit event after an ExitRequested(None) even though we called
        // prevent_exit above. That path bypasses the tray's controlled exit and
        // must restore taken-over Live configs before Tauri tears down state.
        // Restart remains intentionally untouched to avoid the window-state
        // plugin deadlock documented in the restart branch above.
        if let RunEvent::Exit = &event {
            if should_run_final_exit_cleanup(restart_requested) {
                log::info!("Final native exit detected; restoring managed Live configurations");
                tauri::async_runtime::block_on(cleanup_before_exit(app_handle));
            } else {
                log::info!("Final restart exit detected; skipping Live configuration restoration");
            }
            return;
        }

        #[cfg(target_os = "macos")]
        {
            match event {
                // macOS emits Reopen when the Dock icon reactivates the app; restore the main window.
                RunEvent::Reopen { .. } => {
                    if let Some(window) = app_handle.get_webview_window("main") {
                        drop(window);
                        present_main_window(app_handle, "macOS reopen");
                    } else if crate::lightweight::is_lightweight_mode() {
                        if let Err(e) = crate::lightweight::exit_lightweight_mode(app_handle) {
                            log::error!("Failed to recreate the window when leaving lightweight mode: {e}");
                        }
                    }
                }
                // Handle Nexus and legacy CC Switch custom-URL events.
                RunEvent::Opened { urls } => {
                    if let Some(url) = urls.first() {
                        let url_str = url.to_string();
                        log::info!(
                            "RunEvent::Opened with URL: {}",
                            redact_url_for_log(&url_str)
                        );

                        if is_supported_deeplink_url(&url_str) {
                            if crate::lightweight::is_lightweight_mode() {
                                if let Err(e) = crate::lightweight::exit_lightweight_mode(app_handle)
                                {
                                    log::error!("Failed to recreate the window when leaving lightweight mode: {e}");
                                }
                            }

                            // Parse and broadcast through the same path as single_instance.
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
                                        deep_link_error_payload(&url_str, e.to_string()),
                                    ) {
                                        log::error!(
                                            "Failed to emit deep link error event from RunEvent::Opened: {emit_err}"
                                        );
                                    }
                                }
                            }

                            // Ensure the main window is visible.
                            present_main_window(app_handle, "opened URL");
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
// Application exit cleanup.
// ============================================================

/// Clean up before application exit.
///
/// Stop a running proxy and restore live configuration so Claude Code, Codex, and
/// Gemini are not left corrupted. stop_with_restore_keep_state preserves proxy
/// state in settings for automatic restoration at the next launch.
pub async fn cleanup_before_exit(app_handle: &tauri::AppHandle) {
    if let Some(state) = app_handle.try_state::<store::AppState>() {
        let proxy_service = &state.proxy_service;

        // Even if the proxy crashed or stopped, takeover placeholders/backups may remain.
        let has_backups = match state.db.has_any_live_backup().await {
            Ok(v) => v,
            Err(e) => {
                log::error!("Failed to inspect live backups during exit: {e}");
                false
            }
        };
        let live_taken_over = proxy_service.detect_takeover_in_live_configs();
        let needs_restore = has_backups || live_taken_over;

        if needs_restore {
            log::info!("Detected takeover residue; restoring live configuration while preserving proxy state");
            // keep_state retains proxy state in settings.
            if let Err(e) = proxy_service.stop_with_restore_keep_state().await {
                log::error!("Failed to restore live configuration during exit: {e}");
            } else {
                log::info!("Restored live configuration; proxy state will resume on next launch");
            }
            return;
        }

        // Outside takeover mode, stop a running proxy without restoration.
        if proxy_service.is_running().await {
            log::info!("Detected a running proxy server; stopping it");
            if let Err(e) = proxy_service.stop().await {
                log::error!("Failed to stop the proxy during exit: {e}");
            }
            log::info!("Proxy server cleanup complete");
        }
    }
}

/// Explicitly remove the icon from the system tray.
///
/// `std::process::exit` bypasses `TrayIcon::drop()` and Windows `NIM_DELETE`,
/// leaving a cached dead icon until the Shell repaints on hover.
///
/// `set_visible(false)` follows `WM_USER_HIDE_TRAYICON` through tray-icon's
/// remove_tray_icon to `Shell_NotifyIconW(NIM_DELETE)`, removing it before exit.
/// The same call has safe hide/remove semantics on other platforms.
pub(crate) fn remove_tray_icon_before_exit(app_handle: &tauri::AppHandle) {
    if let Some(tray) = app_handle.tray_by_id(tray::TRAY_ID) {
        if let Err(e) = tray.set_visible(false) {
            log::warn!("Failed to remove the tray icon during exit: {e}");
        } else {
            log::info!("Explicitly removed the system-tray icon");
        }
    }
}

// ============================================================
// Restore proxy state at startup.
// ============================================================

/// Restore proxy service at startup from proxy_config state.
///
/// If any application has proxy_config.enabled=true, start the proxy and take over
/// that application's live configuration.
async fn restore_proxy_state_on_startup(state: &store::AppState) {
    // Collect applications requiring takeover restoration from proxy_config.enabled.
    let mut apps_to_restore = Vec::new();
    for app_type in ["claude", "codex", "gemini"] {
        if let Ok(config) = state.db.get_proxy_config_for_app(app_type).await {
            if config.enabled {
                apps_to_restore.push(app_type);
            }
        }
    }

    if apps_to_restore.is_empty() {
        log::debug!("No proxy state requires restoration at startup");
        return;
    }

    log::info!("Previous proxy state requires restoration for: {apps_to_restore:?}");

    // Restore takeover state application by application.
    for app_type in apps_to_restore {
        match state
            .proxy_service
            .set_takeover_for_app(app_type, true)
            .await
        {
            Ok(()) => {
                log::info!("Restored proxy takeover for {app_type}");
            }
            Err(e) => {
                log::error!("Failed to restore proxy takeover for {app_type}: {e}");
                // Clear failed state so the next launch does not repeat indefinitely.
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
// Migration-error dialog helpers.
// ============================================================

fn locale_language_code(locale: &str) -> &str {
    locale
        .trim()
        .split(['_', '-', '.', '@'])
        .next()
        .unwrap_or_default()
}

fn select_vietnamese_locale(
    saved_language: Option<&str>,
    lc_all: Option<&str>,
    lc_messages: Option<&str>,
    lang: Option<&str>,
) -> bool {
    match saved_language.map(locale_language_code) {
        Some(code) if code.eq_ignore_ascii_case("vi") => return true,
        Some(code) if code.eq_ignore_ascii_case("en") => return false,
        _ => {}
    }

    [lc_all, lc_messages, lang]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|locale| !locale.is_empty())
        .map(locale_language_code)
        .is_some_and(|code| code.eq_ignore_ascii_case("vi"))
}

/// Detect whether dialogs should use Vietnamese, preferring the saved app
/// language over process locale variables. GUI launches often do not inherit a
/// useful `LANG`, so the persisted setting is the authoritative source.
fn is_vietnamese_locale() -> bool {
    let settings = crate::settings::get_settings();
    let lc_all = std::env::var("LC_ALL").ok();
    let lc_messages = std::env::var("LC_MESSAGES").ok();
    let lang = std::env::var("LANG").ok();

    select_vietnamese_locale(
        settings.language.as_deref(),
        lc_all.as_deref(),
        lc_messages.as_deref(),
        lang.as_deref(),
    )
}

/// Show the legacy-configuration migration failure dialog.
/// Returns true for retry and false for exit.
fn show_migration_error_dialog(app: &tauri::AppHandle, error: &str) -> bool {
    let use_vietnamese = is_vietnamese_locale();
    let title = if use_vietnamese {
        "Di chuyển cấu hình thất bại"
    } else {
        "Migration Failed"
    };

    let message = if use_vietnamese {
        format!(
            "Đã xảy ra lỗi khi di chuyển cấu hình từ phiên bản cũ:\n\n{error}\n\n\
            Dữ liệu của bạn chưa bị mất; tệp cấu hình cũ vẫn được giữ lại.\n\
            Hãy cân nhắc quay lại phiên bản Nexus Composer cũ hơn để bảo vệ dữ liệu.\n\n\
            Chọn 'Thử lại' để thử di chuyển lần nữa\n\
            Chọn 'Thoát' để đóng ứng dụng"
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

    let retry_text = if use_vietnamese {
        "Thử lại"
    } else {
        "Retry"
    };
    let exit_text = if use_vietnamese { "Thoát" } else { "Exit" };

    // blocking_show waits synchronously. OkCancelCustom returns true for the first
    // Retry button and false for the second Exit button.
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

/// Show the database initialization/schema-migration failure dialog.
/// Returns true for retry and false for exit.
fn show_database_init_error_dialog(
    app: &tauri::AppHandle,
    db_path: &std::path::Path,
    error: &str,
) -> bool {
    let use_vietnamese = is_vietnamese_locale();
    let title = if use_vietnamese {
        "Khởi tạo cơ sở dữ liệu thất bại"
    } else {
        "Database Initialization Failed"
    };

    let message = if use_vietnamese {
        format!(
            "Đã xảy ra lỗi khi khởi tạo hoặc di chuyển cơ sở dữ liệu:\n\n{error}\n\n\
            Đường dẫn tệp cơ sở dữ liệu:\n{db}\n\n\
            Dữ liệu của bạn chưa bị mất; ứng dụng sẽ không tự động xóa tệp cơ sở dữ liệu.\n\
            Nguyên nhân thường gặp: phiên bản cơ sở dữ liệu mới hơn, tệp hỏng, thiếu quyền hoặc thiếu dung lượng đĩa.\n\n\
            Đề xuất:\n\
            1) Sao lưu toàn bộ thư mục cấu hình, bao gồm cơ sở dữ liệu và các tệp sao lưu\n\
            2) Nếu cơ sở dữ liệu thuộc phiên bản mới hơn, hãy nâng cấp Nexus Composer\n\
            3) Nếu lỗi xuất hiện sau khi nâng cấp, hãy quay lại phiên bản cũ để xuất/sao lưu rồi nâng cấp lại\n\n\
            Chọn 'Thử lại' để khởi tạo lại\n\
            Chọn 'Thoát' để đóng ứng dụng",
            db = db_path.display()
        )
    } else {
        format!(
            "An error occurred while initializing or migrating the database:\n\n{error}\n\n\
            Database file path:\n{db}\n\n\
            Your data is NOT lost - the app will not delete the database automatically.\n\
            Common causes include: newer database version, corrupted file, permission issues, or low disk space.\n\n\
            Suggestions:\n\
            1) Back up the entire config directory, including the database and backup files\n\
            2) If you see “database version is newer”, please upgrade Nexus Composer\n\
            3) If this happened right after upgrading, consider rolling back to export/backup then upgrade again\n\n\
            Click 'Retry' to attempt initialization again\n\
            Click 'Exit' to close the program",
            db = db_path.display()
        )
    };

    let retry_text = if use_vietnamese {
        "Thử lại"
    } else {
        "Retry"
    };
    let exit_text = if use_vietnamese { "Thoát" } else { "Exit" };

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
// Exit-request classification.
// ============================================================

/// Three `RunEvent::ExitRequested` sources that require distinct handling.
///
/// Critical constraint: Tauri silently ignores `prevent_exit()` for restart
/// (`code == RESTART_EXIT_CODE`). The event loop exits and invokes plugin
/// RunEvent::Exit hooks, so concurrent custom cleanup can contend for the same
/// state and deadlock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExitRequestAction {
    /// `code == None`: runtime-generated, such as after reclaiming the hidden
    /// WebView. Prevent exit and keep the tray process running.
    StayInTray,
    /// `code == RESTART_EXIT_CODE`: app.restart(). Do not
    /// intercept or run custom cleanup; use Tauri's re-exec path.
    DeferToTauriRestart,
    /// Other `Some(_)`: explicit user exit, such as the tray action. Run complete
    /// asynchronous cleanup before terminating.
    CleanupAndExit,
}

fn classify_exit_request(code: Option<i32>) -> ExitRequestAction {
    match code {
        None => ExitRequestAction::StayInTray,
        Some(tauri::RESTART_EXIT_CODE) => ExitRequestAction::DeferToTauriRestart,
        Some(_) => ExitRequestAction::CleanupAndExit,
    }
}

fn should_run_final_exit_cleanup(restart_requested: bool) -> bool {
    !restart_requested
}

// ============================================================
// Explicitly persist window state before a user-initiated exit.
// ============================================================

fn window_state_flags() -> StateFlags {
    StateFlags::POSITION | StateFlags::SIZE | StateFlags::MAXIMIZED
}

/// The application intercepts ExitRequested and eventually calls
/// `std::process::exit(0)`, so persist before termination rather than bypassing the
/// window-state plugin's default exit hook.
pub fn save_window_state_before_exit(app_handle: &tauri::AppHandle) {
    if let Err(err) = app_handle.save_window_state(window_state_flags()) {
        log::error!("Failed to save window state before exit: {err}");
    } else {
        log::info!("Saved window state before exit");
    }
}

/// Explicitly release the single-instance lock.
///
/// macOS single-instance uses `/tmp/{identifier}.sock`. Direct
/// `std::process::exit(0)` paths bypass its RunEvent::Exit cleanup, so destroying
/// before restart prevents the new process from connecting to a stale listener.
pub fn destroy_single_instance_lock(app_handle: &tauri::AppHandle) {
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    tauri_plugin_single_instance::destroy(app_handle);
}

/// Restart after removing the tray icon and releasing the single-instance lock.
///
/// `tauri::process::restart` spawns a process and exits directly, bypassing event-
/// loop shutdown, cleanup_before_exit, and plugin Exit hooks. The caller handles
/// window state and proxy/live restoration; this function handles tray and lock.
///
/// Deliberately avoid `AppHandle::cleanup_before_exit()`: it drops the tray icon on
/// the caller thread, while macOS NSStatusItem requires the main thread.
/// `set_visible(false)` delegates through run_item_main_thread and is thread-safe.
pub fn restart_process(app_handle: &tauri::AppHandle) -> ! {
    remove_tray_icon_before_exit(app_handle);
    destroy_single_instance_lock(app_handle);
    tauri::process::restart(&app_handle.env());
}

#[cfg(test)]
mod tests {
    use super::{
        classify_exit_request, deep_link_error_payload, is_supported_deeplink_url,
        linux_deep_link_handler_needs_registration, redact_url_for_log, select_vietnamese_locale,
        should_run_final_exit_cleanup, ExitRequestAction,
    };

    #[test]
    fn accepts_current_and_legacy_deep_link_schemes() {
        assert!(is_supported_deeplink_url("nexus://v1/import"));
        assert!(is_supported_deeplink_url("ccswitch://v1/import"));
        assert!(!is_supported_deeplink_url("https://example.com"));
        assert!(!is_supported_deeplink_url("nexus-invalid://v1/import"));
    }

    #[test]
    fn deep_link_log_redaction_preserves_routing_without_query_values() {
        let raw = "nexus://v1/import?resource=provider&app=codex&name=Test&apiKey=sk-secret-123&config=eyJ0b2tlbiI6InNlY3JldCJ9&usageAccessToken=usage-secret#private-fragment";
        let redacted = redact_url_for_log(raw);

        assert_eq!(
            redacted,
            "nexus://v1/import?[keys:apiKey,app,config,name,resource,usageAccessToken]"
        );
        for secret in [
            "sk-secret-123",
            "eyJ0b2tlbiI6InNlY3JldCJ9",
            "usage-secret",
            "private-fragment",
        ] {
            assert!(!redacted.contains(secret));
        }

        // Redaction is only for logging. The parser still receives the original URL.
        let parsed = crate::deeplink::parse_deeplink_url(raw).unwrap();
        assert_eq!(parsed.api_key.as_deref(), Some("sk-secret-123"));
        assert_eq!(parsed.config.as_deref(), Some("eyJ0b2tlbiI6InNlY3JldCJ9"));
        assert_eq!(parsed.usage_access_token.as_deref(), Some("usage-secret"));
    }

    #[test]
    fn malformed_url_log_redaction_drops_query_and_fragment() {
        let redacted = redact_url_for_log(
            "not a URL?apiKey=malformed-secret&config=embedded-secret#fragment-secret",
        );

        assert_eq!(redacted, "not a URL?[redacted]");
        assert!(!redacted.contains("malformed-secret"));
        assert!(!redacted.contains("embedded-secret"));
        assert!(!redacted.contains("fragment-secret"));
    }

    #[test]
    fn deep_link_error_event_payload_never_contains_query_or_fragment_values() {
        let raw = "nexus://v2/import?apiKey=api-query-secret&config=config-query-secret&token=token-query-secret#fragment-secret";
        let payload = deep_link_error_payload(raw, "Unsupported protocol version: v2".to_string());
        let value = serde_json::to_value(&payload).expect("serialize deep-link error payload");
        let serialized = value.to_string();

        assert_eq!(
            value["context"],
            "nexus://v2/import?[keys:apiKey,config,token]"
        );
        assert_eq!(value["error"], "Unsupported protocol version: v2");
        assert!(value.get("url").is_none());

        for secret in [
            "api-query-secret",
            "config-query-secret",
            "token-query-secret",
            "fragment-secret",
        ] {
            assert!(!serialized.contains(secret));
        }
    }

    #[test]
    fn linux_deep_link_handler_is_refreshed_when_a_scheme_is_missing() {
        assert!(linux_deep_link_handler_needs_registration(None));
        assert!(linux_deep_link_handler_needs_registration(Some(
            "MimeType=x-scheme-handler/nexus;"
        )));
        assert!(linux_deep_link_handler_needs_registration(Some(
            "# legacy ccswitch handler mentioned in a comment\nMimeType=x-scheme-handler/nexus;"
        )));
        assert!(!linux_deep_link_handler_needs_registration(Some(
            "MimeType=x-scheme-handler/nexus;x-scheme-handler/ccswitch;"
        )));
    }

    #[test]
    fn flatpak_desktop_entry_accepts_both_deep_link_schemes() {
        let desktop_entry = include_str!("../../flatpak/com.nexuscomposer.desktop.desktop");

        assert!(desktop_entry
            .lines()
            .any(|line| line == "Exec=nexus-composer %u"));
        assert!(desktop_entry
            .lines()
            .any(|line| { line == "MimeType=x-scheme-handler/nexus;x-scheme-handler/ccswitch;" }));
    }

    #[test]
    fn saved_dialog_language_overrides_process_locale() {
        assert!(select_vietnamese_locale(
            Some("vi"),
            Some("en_US.UTF-8"),
            None,
            None
        ));
        assert!(!select_vietnamese_locale(
            Some("en"),
            Some("vi_VN.UTF-8"),
            None,
            None
        ));
    }

    #[test]
    fn dialog_locale_fallback_uses_standard_environment_precedence() {
        assert!(!select_vietnamese_locale(
            None,
            Some("en_US.UTF-8"),
            Some("vi_VN.UTF-8"),
            Some("vi_VN.UTF-8")
        ));
        assert!(select_vietnamese_locale(
            None,
            Some(""),
            Some("vi_VN.UTF-8"),
            Some("en_US.UTF-8")
        ));
    }

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

    #[test]
    fn native_final_exit_runs_cleanup_fallback() {
        assert!(should_run_final_exit_cleanup(false));
    }

    #[test]
    fn restart_final_exit_skips_cleanup_fallback() {
        assert!(!should_run_final_exit_cleanup(true));
    }
}
