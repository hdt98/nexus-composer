//! Tray menu management.
//!
//! Creates, updates, and handles system tray icons, menus, and events.

use once_cell::sync::Lazy;
use tauri::menu::{CheckMenuItem, Menu, MenuBuilder, MenuItem, Submenu, SubmenuBuilder};
use tauri::{Emitter, Manager};
use tauri_plugin_opener::OpenerExt;

use crate::app_config::AppType;
use crate::error::AppError;
use crate::store::AppState;

const TEMPLATE_TYPE_OFFICIAL_SUBSCRIPTION: &str = "official_subscription";
const H_TIER_NAMES: &[&str] = &[crate::services::subscription::TIER_FIVE_HOUR];
const W_TIER_NAMES: &[&str] = &[
    crate::services::subscription::TIER_WEEKLY_LIMIT,
    crate::services::subscription::TIER_SEVEN_DAY,
    crate::services::subscription::TIER_SEVEN_DAY_OPUS,
    crate::services::subscription::TIER_SEVEN_DAY_SONNET,
];
// Volcengine Agent/Coding Plan monthly window (5h / week / month tiers).
const M_TIER_NAMES: &[&str] = &[crate::services::subscription::TIER_MONTHLY];
const GEMINI_PRO_TIER_NAMES: &[&str] = &[crate::services::subscription::TIER_GEMINI_PRO];
const GEMINI_FLASH_TIER_NAMES: &[&str] = &[crate::services::subscription::TIER_GEMINI_FLASH];
const GEMINI_FLASH_LITE_TIER_NAMES: &[&str] =
    &[crate::services::subscription::TIER_GEMINI_FLASH_LITE];
const TIER_LABEL_GROUPS: &[(&str, &[&str])] = &[
    ("h", H_TIER_NAMES),
    ("w", W_TIER_NAMES),
    ("m", M_TIER_NAMES),
    ("p", GEMINI_PRO_TIER_NAMES),
    ("f", GEMINI_FLASH_TIER_NAMES),
    ("l", GEMINI_FLASH_LITE_TIER_NAMES),
];

fn official_provider_blocked_by_takeover(
    app_type: &AppType,
    takeover_active: bool,
    is_official: bool,
) -> bool {
    !matches!(app_type, AppType::Codex) && takeover_active && is_official
}

/// Submenu handles for each app section, used to update usage labels in place
/// instead of rebuilding the full menu.
/// `create_tray_menu` rewrites the whole table on each rebuild so the handles
/// always point at the active menu.
static TRAY_SECTION_SUBMENUS: Lazy<
    std::sync::Mutex<std::collections::HashMap<AppType, Submenu<tauri::Wry>>>,
> = Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Localized tray menu text.
#[derive(Clone, Copy)]
pub struct TrayTexts {
    pub show_main: &'static str,
    pub open_website: &'static str,
    pub no_providers_label: &'static str,
    pub lightweight_mode: &'static str,
    pub quit: &'static str,
    pub _auto_label: &'static str,
}

impl TrayTexts {
    pub fn from_language(language: &str) -> Self {
        match language {
            "vi" => Self {
                show_main: "Mở cửa sổ chính",
                open_website: "Mở trang web chính thức",
                no_providers_label: "(chưa có nhà cung cấp)",
                lightweight_mode: "Chế độ gọn nhẹ",
                quit: "Thoát",
                _auto_label: "Tự động (chuyển khi lỗi)",
            },
            _ => Self {
                show_main: "Open main window",
                open_website: "Open Official Website",
                no_providers_label: "(no providers)",
                lightweight_mode: "Lightweight Mode",
                quit: "Quit",
                _auto_label: "Auto (Failover)",
            },
        }
    }
}

/// Tray app section configuration.
pub struct TrayAppSection {
    pub app_type: AppType,
    pub prefix: &'static str,
    pub empty_id: &'static str,
    pub header_label: &'static str,
    pub log_name: &'static str,
}

/// Suffix for the Auto menu item.
pub const AUTO_SUFFIX: &str = "auto";
pub const TRAY_ID: &str = "nexus-composer";

pub const TRAY_SECTIONS: [TrayAppSection; 3] = [
    TrayAppSection {
        app_type: AppType::Claude,
        prefix: "claude_",
        empty_id: "claude_empty",
        header_label: "Claude",
        log_name: "Claude",
    },
    TrayAppSection {
        app_type: AppType::Codex,
        prefix: "codex_",
        empty_id: "codex_empty",
        header_label: "Codex",
        log_name: "Codex",
    },
    TrayAppSection {
        app_type: AppType::Gemini,
        prefix: "gemini_",
        empty_id: "gemini_empty",
        header_label: "Gemini",
        log_name: "Gemini",
    },
];

/// Color thresholds aligned with frontend `utilizationColor` semantics.
const UTIL_WARN_PCT: f64 = 70.0;
const UTIL_DANGER_PCT: f64 = 90.0;

fn emoji_for_utilization(pct: f64) -> &'static str {
    if pct >= UTIL_DANGER_PCT {
        "\u{1F534}" // 🔴
    } else if pct >= UTIL_WARN_PCT {
        "\u{1F7E0}" // 🟠
    } else {
        "\u{1F7E2}" // 🟢
    }
}

fn format_subscription_summary(
    quota: &crate::services::subscription::SubscriptionQuota,
) -> Option<String> {
    if !quota.success {
        return None;
    }

    let entries: Vec<(&str, f64)> = quota
        .tiers
        .iter()
        .map(|tier| (tier.name.as_str(), tier.utilization))
        .collect();
    let parts = labeled_tier_parts(&entries);

    if parts.is_empty() {
        return None;
    }

    // Use the highest utilization across selected tiers. Users care most about
    // how close they are to the limit.
    let worst = parts
        .iter()
        .map(|(_, u)| *u)
        .fold(f64::NEG_INFINITY, f64::max);
    if !worst.is_finite() {
        return None;
    }

    let emoji = emoji_for_utilization(worst);
    let body = parts
        .iter()
        .map(|(label, u)| format!("{label}{}%", u.round() as i64))
        .collect::<Vec<_>>()
        .join(" ");
    Some(format!("{emoji} {body}"))
}

fn labeled_tier_parts(entries: &[(&str, f64)]) -> Vec<(&'static str, f64)> {
    let mut parts = Vec::new();
    for &(label, tier_names) in TIER_LABEL_GROUPS {
        let max_utilization = entries
            .iter()
            .filter(|(name, _)| tier_names.contains(name))
            .map(|(_, utilization)| *utilization)
            .filter(|utilization| utilization.is_finite())
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        if let Some(utilization) = max_utilization {
            parts.push((label, utilization));
        }
    }
    parts
}

fn tier_pct(data: &crate::provider::UsageData) -> Option<f64> {
    match (data.used, data.total) {
        (Some(used), Some(total)) if total > 0.0 => Some(used / total * 100.0),
        _ => None,
    }
}

fn format_script_summary(result: &crate::provider::UsageResult) -> Option<String> {
    if !result.success {
        return None;
    }
    let data = result.data.as_ref()?;
    if data.is_empty() {
        return None;
    }

    // The commands::provider token_plan / official_subscription branches
    // flatten each SubscriptionQuota tier into one UsageData entry with the
    // tier name stored in plan_name. Rebuild the short tray labels from
    // plan_name here. Other usage results (Copilot / balance / custom script)
    // use the fallback path.
    let entries: Vec<(&str, f64)> = data
        .iter()
        .filter_map(|d| Some((d.plan_name.as_deref()?, tier_pct(d)?)))
        .collect();
    let parts = labeled_tier_parts(&entries);
    if !parts.is_empty() {
        let worst = parts
            .iter()
            .map(|(_, u)| *u)
            .fold(f64::NEG_INFINITY, f64::max);
        let emoji = emoji_for_utilization(worst);
        let body = parts
            .iter()
            .map(|(label, u)| format!("{label}{}%", u.round() as i64))
            .collect::<Vec<_>>()
            .join(" ");
        return Some(format!("{emoji} {body}"));
    }

    let first = data.first()?;
    let pct = tier_pct(first)?;
    let emoji = emoji_for_utilization(pct);
    let plan = first.plan_name.as_deref().unwrap_or("");
    let rounded = pct.round() as i64;
    if plan.is_empty() {
        Some(format!("{} {}%", emoji, rounded))
    } else {
        Some(format!("{} {} {}%", emoji, plan, rounded))
    }
}

fn provider_uses_official_subscription(provider: &crate::provider::Provider) -> bool {
    provider
        .meta
        .as_ref()
        .and_then(|m| m.usage_script.as_ref())
        .map(|script| {
            script.enabled
                && script.template_type.as_deref() == Some(TEMPLATE_TYPE_OFFICIAL_SUBSCRIPTION)
        })
        .unwrap_or(false)
}

fn format_usage_suffix(
    app_state: &AppState,
    app_type: &AppType,
    provider: &crate::provider::Provider,
    provider_id: &str,
) -> Option<String> {
    // Only use cached script usage while the script is still enabled. If it was
    // disabled or removed, invalidate the stale cache entry to avoid future
    // rebuilds reusing expired data.
    let is_official_provider = provider.category.as_deref() == Some("official");
    let can_use_script = provider.has_usage_script_enabled()
        && (!is_official_provider || provider_uses_official_subscription(provider));
    if can_use_script {
        // Prefer script cache results first, covering Copilot / coding_plan /
        // balance / custom scripts. Borrow the entry to avoid cloning the full
        // UsageResult.
        if let Some(Some(s)) =
            app_state
                .usage_cache
                .with_script(app_type, provider_id, format_script_summary)
        {
            return Some(format!(" · {s}"));
        }
        if provider_uses_official_subscription(provider) {
            if let Some(Some(s)) = app_state
                .usage_cache
                .with_subscription(app_type, format_subscription_summary)
            {
                return Some(format!(" · {s}"));
            }
        }
    } else {
        app_state
            .usage_cache
            .invalidate_script(app_type, provider_id);
    }

    if !provider_uses_official_subscription(provider) {
        app_state.usage_cache.invalidate_subscription(app_type);
    }
    None
}

/// Sort providers by `sort_index`, then `created_at`, then `name`.
fn sort_providers(
    providers: &indexmap::IndexMap<String, crate::provider::Provider>,
) -> Vec<(&String, &crate::provider::Provider)> {
    let mut sorted: Vec<_> = providers.iter().collect();
    sorted.sort_by(|(_, a), (_, b)| {
        match (a.sort_index, b.sort_index) {
            (Some(idx_a), Some(idx_b)) => return idx_a.cmp(&idx_b),
            (Some(_), None) => return std::cmp::Ordering::Less,
            (None, Some(_)) => return std::cmp::Ordering::Greater,
            _ => {}
        }

        match (a.created_at, b.created_at) {
            (Some(time_a), Some(time_b)) => return time_a.cmp(&time_b),
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            _ => {}
        }

        a.name.cmp(&b.name)
    });
    sorted
}

/// Handle provider tray events.
pub fn handle_provider_tray_event(app: &tauri::AppHandle, event_id: &str) -> bool {
    for section in TRAY_SECTIONS.iter() {
        if let Some(suffix) = event_id.strip_prefix(section.prefix) {
            // Handle Auto click.
            if suffix == AUTO_SUFFIX {
                log::info!("Switching {0} to Auto mode", section.log_name);
                let app_handle = app.clone();
                let app_type = section.app_type.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    if let Err(e) = handle_auto_click(&app_handle, &app_type) {
                        log::error!("Failed to switch {0} to Auto mode: {e}", section.log_name);
                    }
                });
                return true;
            }

            // Handle provider click.
            log::info!("Switching {0} provider to {suffix}", section.log_name);
            let app_handle = app.clone();
            let provider_id = suffix.to_string();
            let app_type = section.app_type.clone();
            tauri::async_runtime::spawn_blocking(move || {
                if let Err(e) = handle_provider_click(&app_handle, &app_type, &provider_id) {
                    log::error!("Failed to switch {0} provider: {e}", section.log_name);
                }
            });
            return true;
        }
    }
    false
}

/// Handle Auto click by enabling proxy and auto-failover.
fn handle_auto_click(app: &tauri::AppHandle, app_type: &AppType) -> Result<(), AppError> {
    if let Some(app_state) = app.try_state::<AppState>() {
        let app_type_str = app_type.as_str();

        // Strong consistency: when Auto mode turns on, switch to queue P1
        // immediately (P1 -> P2 -> ...). If the queue is empty, try to add the
        // current provider as P1 automatically so the user does not get stuck
        // unable to enable Auto mode.
        let mut queue = app_state.db.get_failover_queue(app_type_str)?;
        if queue.is_empty() {
            let current_id =
                crate::settings::get_effective_current_provider(&app_state.db, app_type)?;
            let Some(current_id) = current_id else {
                return Err(AppError::Message(
                    "Failover queue is empty and no current provider is set, so Auto mode cannot be enabled".to_string(),
                ));
            };
            app_state
                .db
                .add_to_failover_queue(app_type_str, &current_id)?;
            queue = app_state.db.get_failover_queue(app_type_str)?;
        }

        let p1_provider_id = queue
            .first()
            .map(|item| item.provider_id.clone())
            .ok_or_else(|| {
                AppError::Message(
                    "Failover queue is empty, so Auto mode cannot be enabled".to_string(),
                )
            })?;

        // Actually enable failover: start the proxy service, take over the app,
        // and enable auto-failover.
        let proxy_service = &app_state.proxy_service;

        // 1) Ensure the proxy service is running. This also sets
        // `proxy_enabled = true`.
        let is_running = futures::executor::block_on(proxy_service.is_running());
        if !is_running {
            log::info!("[Tray] Auto mode: starting proxy service");
            if let Err(e) = futures::executor::block_on(proxy_service.start()) {
                log::error!("[Tray] Failed to start proxy service: {e}");
                return Err(AppError::Message(format!(
                    "Failed to start proxy service: {e}"
                )));
            }
        }

        // 2) Apply live-config takeover so this app is actually routed through
        // the proxy.
        log::info!("[Tray] Auto mode: applying takeover for {app_type_str}");
        if let Err(e) =
            futures::executor::block_on(proxy_service.set_takeover_for_app(app_type_str, true))
        {
            log::error!("[Tray] Failed to apply takeover: {e}");
            return Err(AppError::Message(format!("Failed to apply takeover: {e}")));
        }

        // 3) Set `auto_failover_enabled = true`.
        app_state
            .db
            .set_proxy_flags_sync(app_type_str, true, true)?;

        // 3.1) Switch to queue P1 immediately. This is a hot switch: update
        // only DB/settings/backup, not the live config.
        if let Err(e) = futures::executor::block_on(
            proxy_service.switch_proxy_target(app_type_str, &p1_provider_id),
        ) {
            log::error!("[Tray] Failed to switch Auto mode to queue P1: {e}");
            return Err(AppError::Message(format!(
                "Failed to switch Auto mode to queue P1: {e}"
            )));
        }

        // 4) Update the tray menu.
        if let Ok(new_menu) = create_tray_menu(app, app_state.inner()) {
            if let Some(tray) = app.tray_by_id(TRAY_ID) {
                let _ = tray.set_menu(Some(new_menu));
            }
        }

        // 5) Emit events to the renderer.
        let event_data = serde_json::json!({
            "appType": app_type_str,
            "proxyEnabled": true,
            "autoFailoverEnabled": true,
            "providerId": p1_provider_id
        });
        if let Err(e) = app.emit("proxy-flags-changed", event_data.clone()) {
            log::error!("Failed to emit proxy-flags-changed event: {e}");
        }
        // Emit provider-switched for backward compatibility. An Auto switch is
        // still a provider switch.
        if let Err(e) = app.emit("provider-switched", event_data) {
            log::error!("Failed to emit provider-switched event: {e}");
        }
    }
    Ok(())
}

/// Handle provider click by disabling auto-failover and switching providers.
fn handle_provider_click(
    app: &tauri::AppHandle,
    app_type: &AppType,
    provider_id: &str,
) -> Result<(), AppError> {
    if let Some(app_state) = app.try_state::<AppState>() {
        let app_type_str = app_type.as_str();

        // Read the current proxy state. Keep `enabled` unchanged and only turn
        // off auto-failover.
        let (proxy_enabled, _) = app_state.db.get_proxy_flags_sync(app_type_str);
        app_state
            .db
            .set_proxy_flags_sync(app_type_str, proxy_enabled, false)?;

        // Switch providers. Providers that require local routing do not start
        // the proxy automatically here; users enable that manually in the UI.
        crate::services::ProviderService::switch(app_state.inner(), app_type.clone(), provider_id)?;

        // Update the tray menu.
        if let Ok(new_menu) = create_tray_menu(app, app_state.inner()) {
            if let Some(tray) = app.tray_by_id(TRAY_ID) {
                let _ = tray.set_menu(Some(new_menu));
            }
        }

        // Emit events to the renderer.
        let event_data = serde_json::json!({
            "appType": app_type_str,
            "proxyEnabled": proxy_enabled,
            "autoFailoverEnabled": false,
            "providerId": provider_id
        });
        if let Err(e) = app.emit("proxy-flags-changed", event_data.clone()) {
            log::error!("Failed to emit proxy-flags-changed event: {e}");
        }
        // Emit provider-switched for backward compatibility.
        if let Err(e) = app.emit("provider-switched", event_data) {
            log::error!("Failed to emit provider-switched event: {e}");
        }
    }
    Ok(())
}

/// Create the dynamic tray menu.
pub fn create_tray_menu(
    app: &tauri::AppHandle,
    app_state: &AppState,
) -> Result<Menu<tauri::Wry>, AppError> {
    let app_settings = crate::settings::get_settings();
    let tray_texts = TrayTexts::from_language(app_settings.language.as_deref().unwrap_or("en"));

    // Get visible apps setting, default to all visible
    let visible_apps = app_settings.visible_apps.unwrap_or_default();

    let mut menu_builder = MenuBuilder::new(app);
    let mut section_handles: std::collections::HashMap<AppType, Submenu<tauri::Wry>> =
        std::collections::HashMap::new();

    // Top section: open the main window / open the official website.
    let show_main_item =
        MenuItem::with_id(app, "show_main", tray_texts.show_main, true, None::<&str>).map_err(
            |e| AppError::Message(format!("Failed to create Open Main Window menu item: {e}")),
        )?;
    let open_website_item = MenuItem::with_id(
        app,
        "open_website",
        tray_texts.open_website,
        true,
        None::<&str>,
    )
    .map_err(|e| {
        AppError::Message(format!(
            "Failed to create Open Official Website menu item: {e}"
        ))
    })?;
    menu_builder = menu_builder
        .item(&show_main_item)
        .item(&open_website_item)
        .separator();

    // Collapse each app type into a submenu so long provider lists stay
    // manageable.
    for section in TRAY_SECTIONS.iter() {
        if !visible_apps.is_visible(&section.app_type) {
            continue;
        }

        let app_type_str = section.app_type.as_str();
        let providers = app_state.db.get_all_providers(app_type_str)?;

        let current_id =
            crate::settings::get_effective_current_provider(&app_state.db, &section.app_type)?
                .unwrap_or_default();

        if providers.is_empty() {
            // No providers: show a disabled placeholder item.
            let label = format!("{} {}", section.header_label, tray_texts.no_providers_label);
            let empty_item = MenuItem::with_id(app, section.empty_id, &label, false, None::<&str>)
                .map_err(|e| {
                    AppError::Message(format!(
                        "Failed to create {0} empty placeholder item: {e}",
                        section.log_name
                    ))
                })?;
            menu_builder = menu_builder.item(&empty_item);
        } else {
            let current_provider = providers.get(&current_id);
            let submenu_label = match current_provider {
                Some(p) => {
                    let suffix = format_usage_suffix(app_state, &section.app_type, p, &current_id)
                        .unwrap_or_default();
                    format!("{} · {}{}", section.header_label, p.name, suffix)
                }
                None => section.header_label.to_string(),
            };
            let submenu_id = format!("submenu_{}", app_type_str);

            // Listener liveness is not takeover ownership. A crash can leave the
            // durable route enabled while the listener is down, and backup/live
            // residue also remains owned until recovery completes.
            let takeover_enabled =
                futures::executor::block_on(app_state.db.get_proxy_config_for_app(app_type_str))
                    .map(|config| config.enabled)
                    .unwrap_or(false);
            let has_takeover_residue =
                futures::executor::block_on(app_state.db.get_live_backup(app_type_str))
                    .ok()
                    .flatten()
                    .is_some()
                    || app_state
                        .proxy_service
                        .detect_takeover_in_live_config_for_app(&section.app_type);
            let is_app_taken_over = takeover_enabled || has_takeover_residue;

            let mut submenu_builder = SubmenuBuilder::with_id(app, &submenu_id, &submenu_label);

            for (id, provider) in sort_providers(&providers) {
                let is_current = current_id == *id;
                let is_official_blocked = official_provider_blocked_by_takeover(
                    &section.app_type,
                    is_app_taken_over,
                    provider.category.as_deref() == Some("official"),
                );
                let label = if is_official_blocked {
                    format!("{} \u{26D4}", &provider.name) // ⛔ emoji
                } else {
                    provider.name.clone()
                };
                let item = CheckMenuItem::with_id(
                    app,
                    format!("{}{}", section.prefix, id),
                    &label,
                    !is_official_blocked, // disabled when blocked
                    is_current,
                    None::<&str>,
                )
                .map_err(|e| {
                    AppError::Message(format!(
                        "Failed to create {0} menu item: {e}",
                        section.log_name
                    ))
                })?;
                submenu_builder = submenu_builder.item(&item);
            }

            let submenu = submenu_builder.build().map_err(|e| {
                AppError::Message(format!(
                    "Failed to build {0} submenu: {e}",
                    section.log_name
                ))
            })?;
            section_handles.insert(section.app_type.clone(), submenu.clone());
            menu_builder = menu_builder.item(&submenu);
        }

        menu_builder = menu_builder.separator();
    }

    let lightweight_item = CheckMenuItem::with_id(
        app,
        "lightweight_mode",
        tray_texts.lightweight_mode,
        true,
        crate::lightweight::is_lightweight_mode(),
        None::<&str>,
    )
    .map_err(|e| AppError::Message(format!("Failed to create Lightweight Mode menu item: {e}")))?;

    menu_builder = menu_builder.item(&lightweight_item).separator();

    // Quit item. The separator was already added in the section loop above.
    let quit_item = MenuItem::with_id(app, "quit", tray_texts.quit, true, None::<&str>)
        .map_err(|e| AppError::Message(format!("Failed to create Quit menu item: {e}")))?;

    menu_builder = menu_builder.item(&quit_item);

    let menu = menu_builder
        .build()
        .map_err(|e| AppError::Message(format!("Failed to build tray menu: {e}")))?;

    *TRAY_SECTION_SUBMENUS
        .lock()
        .unwrap_or_else(|p| p.into_inner()) = section_handles;

    Ok(menu)
}

/// Update app-section submenu titles in place when the usage suffix changes.
/// This avoids `set_menu`, which closes a menu while the user has it open.
/// Handles come from the last `create_tray_menu` call. If none exist yet,
/// nothing happens.
fn update_tray_usage_labels(app: &tauri::AppHandle) {
    let Some(app_state) = app.try_state::<AppState>() else {
        return;
    };
    let handles = match TRAY_SECTION_SUBMENUS.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };

    for section in TRAY_SECTIONS.iter() {
        let Some(submenu) = handles.get(&section.app_type) else {
            continue;
        };
        let Ok(providers) = app_state.db.get_all_providers(section.app_type.as_str()) else {
            continue;
        };
        let Ok(Some(current_id)) =
            crate::settings::get_effective_current_provider(&app_state.db, &section.app_type)
        else {
            continue;
        };
        let Some(provider) = providers.get(&current_id) else {
            continue;
        };
        let suffix = format_usage_suffix(&app_state, &section.app_type, provider, &current_id)
            .unwrap_or_default();
        let new_label = format!("{} · {}{}", section.header_label, provider.name, suffix);
        if let Err(e) = submenu.set_text(&new_label) {
            log::debug!(
                "[Tray] Failed to update {0} submenu title: {e}",
                section.log_name
            );
        }
    }
}

pub fn refresh_tray_menu(app: &tauri::AppHandle) {
    use crate::store::AppState;

    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(new_menu) = create_tray_menu(app, state.inner()) {
            if let Some(tray) = app.tray_by_id(TRAY_ID) {
                if let Err(e) = tray.set_menu(Some(new_menu)) {
                    log::error!("Failed to refresh tray menu: {e}");
                }
            }
        }
    }
}

#[cfg(target_os = "macos")]
pub fn apply_tray_policy(app: &tauri::AppHandle, dock_visible: bool) -> Result<(), String> {
    use tauri::ActivationPolicy;

    let desired_policy = if dock_visible {
        ActivationPolicy::Regular
    } else {
        ActivationPolicy::Accessory
    };
    let mut errors = Vec::new();

    if let Err(err) = app.set_activation_policy(desired_policy) {
        errors.push(format!("failed to set activation policy: {err}"));
    }
    if let Err(err) = app.set_dock_visibility(dock_visible) {
        errors.push(format!("failed to set Dock visibility: {err}"));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

/// Handle tray menu events.
pub fn handle_tray_menu_event(app: &tauri::AppHandle, event_id: &str) {
    log::info!("Handling tray menu event: {event_id}");

    match event_id {
        "show_main" => {
            if app.get_webview_window("main").is_some() {
                if let Err(e) = crate::present_main_window(app, "tray menu") {
                    log::error!("Failed to present main window from tray: {e}");
                }
            } else if crate::lightweight::is_lightweight_mode() {
                if let Err(e) = crate::lightweight::exit_lightweight_mode(app) {
                    log::error!("Failed to rebuild window while leaving lightweight mode: {e}");
                }
            }
        }
        "open_website" => {
            if let Err(e) = app
                .opener()
                .open_url("https://github.com/hdt98/nexus-composer", None::<String>)
            {
                log::error!("Failed to open official website: {e}");
            }
        }
        "lightweight_mode" => {
            if crate::lightweight::is_lightweight_mode() {
                if let Err(e) = crate::lightweight::exit_lightweight_mode(app) {
                    log::error!("Failed to leave lightweight mode: {e}");
                }
            } else if let Err(e) = crate::lightweight::enter_lightweight_mode(app) {
                log::error!("Failed to enter lightweight mode: {e}");
            }
        }
        "quit" => {
            log::info!("Exiting application");
            app.exit(0);
        }
        _ => {
            if handle_provider_tray_event(app, event_id) {
                return;
            }
            log::warn!("Unhandled menu event: {event_id}");
        }
    }
}

static LAST_TRAY_USAGE_REFRESH: std::sync::Mutex<Option<std::time::Instant>> =
    std::sync::Mutex::new(None);
const MIN_TRAY_USAGE_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// Coalesce multiple fast "soft usage-title refresh" triggers. When several
/// usage commands succeed during one refresh batch, emit only one in-place
/// `set_text` pass. Use a soft update instead of `refresh_tray_menu` so macOS
/// does not close a menu the user currently has open.
static TRAY_REBUILD_SCHEDULED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub fn schedule_tray_refresh(app: &tauri::AppHandle) {
    use std::sync::atomic::Ordering;
    if TRAY_REBUILD_SCHEDULED.swap(true, Ordering::AcqRel) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        // Use a 50 ms coalescing window so multiple writes from one React Query
        // wave or tray refresh batch share one title update.
        std::thread::sleep(std::time::Duration::from_millis(50));
        TRAY_REBUILD_SCHEDULED.store(false, Ordering::Release);
        update_tray_usage_labels(&app);
    });
}

/// Refresh usage for the current provider of each visible app in parallel.
/// Success and failure results both flow into `UsageCache` through each
/// command's write-through path, and `schedule_tray_refresh` coalesces the
/// resulting menu update. An internal 10 second throttle prevents request
/// storms when the cursor repeatedly enters and leaves the tray. If the mutex
/// becomes poisoned, continue using the last observed state instead of blocking
/// forever.
///
/// The refresh surface is intentionally aligned with `format_usage_suffix`.
/// Each hover triggers at most `TRAY_SECTIONS.len()` external requests, and
/// only explicitly enabled usage queries can issue requests, including official
/// subscriptions, coding_plan, balance, Copilot, and custom scripts.
pub(crate) async fn refresh_all_usage_in_tray(app: &tauri::AppHandle) {
    use crate::commands::CopilotAuthState;
    use futures::future::join_all;

    {
        let mut guard = LAST_TRAY_USAGE_REFRESH
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = std::time::Instant::now();
        if let Some(last) = *guard {
            if now.duration_since(last) < MIN_TRAY_USAGE_REFRESH_INTERVAL {
                return;
            }
        }
        *guard = Some(now);
    }

    let Some(app_state) = app.try_state::<AppState>() else {
        return;
    };

    // Match `create_tray_menu`: hidden apps do not participate in external API
    // queries. This avoids wasting requests on unused apps, hitting rate
    // limits, or repeatedly logging auth failures.
    let visible_apps = crate::settings::get_settings()
        .visible_apps
        .unwrap_or_default();

    let mut script_futures = Vec::new();

    for section in TRAY_SECTIONS.iter() {
        if !visible_apps.is_visible(&section.app_type) {
            continue;
        }

        let app_type_str = section.app_type.as_str();
        let log_name = section.log_name;

        // Resolve the effective current provider. If it is unset or errors,
        // skip quietly to match `create_tray_menu`.
        let current_id =
            match crate::settings::get_effective_current_provider(&app_state.db, &section.app_type)
            {
                Ok(Some(id)) => id,
                Ok(None) => continue,
                Err(e) => {
                    log::warn!("[Tray] Failed to read current {log_name} provider: {e}");
                    continue;
                }
            };
        // Only the current provider is needed. Query by id to avoid loading the
        // full provider list for the app into memory on this hot path.
        let current = match app_state.db.get_provider_by_id(&current_id, app_type_str) {
            Ok(Some(p)) => p,
            Ok(None) => continue,
            Err(e) => {
                log::warn!("[Tray] Failed to read current {log_name} provider: {e}");
                continue;
            }
        };

        // Keep the same priority rules as `format_usage_suffix`: only explicitly
        // enabled usage queries issue requests.
        let is_official_provider = current.category.as_deref() == Some("official");
        if current.has_usage_script_enabled()
            && (!is_official_provider || provider_uses_official_subscription(&current))
        {
            let app_clone = app.clone();
            let state = app.state::<AppState>();
            let copilot_state = app.state::<CopilotAuthState>();
            let provider_id = current_id.clone();
            let app_str = app_type_str.to_string();
            script_futures.push(async move {
                if let Err(e) = crate::commands::queryProviderUsage(
                    app_clone,
                    state,
                    copilot_state,
                    provider_id.clone(),
                    app_str,
                )
                .await
                {
                    log::debug!(
                        "[Tray] Failed to refresh usage for {log_name} provider {provider_id}: {e}"
                    );
                }
            });
        }
    }

    join_all(script_futures).await;
}

#[cfg(test)]
mod tests {
    use super::{
        format_script_summary, format_subscription_summary, official_provider_blocked_by_takeover,
        TrayTexts, TRAY_ID,
    };
    use crate::app_config::AppType;
    use crate::provider::{UsageData, UsageResult};
    use crate::services::subscription::{
        CredentialStatus, QuotaTier, SubscriptionQuota, TIER_FIVE_HOUR, TIER_GEMINI_FLASH,
        TIER_GEMINI_FLASH_LITE, TIER_GEMINI_PRO, TIER_MONTHLY, TIER_SEVEN_DAY, TIER_SEVEN_DAY_OPUS,
        TIER_SEVEN_DAY_SONNET, TIER_WEEKLY_LIMIT,
    };

    #[test]
    fn tray_id_is_unique_to_app() {
        assert_eq!(TRAY_ID, "nexus-composer");
        assert_ne!(TRAY_ID, "main");
    }

    #[test]
    fn tray_blocks_non_atomic_official_switches_during_takeover() {
        assert!(official_provider_blocked_by_takeover(
            &AppType::Claude,
            true,
            true
        ));
        assert!(official_provider_blocked_by_takeover(
            &AppType::Gemini,
            true,
            true
        ));
        assert!(!official_provider_blocked_by_takeover(
            &AppType::Codex,
            true,
            true
        ));
        assert!(!official_provider_blocked_by_takeover(
            &AppType::Claude,
            false,
            true
        ));
    }

    #[test]
    fn vietnamese_tray_labels_are_natural_and_complete() {
        let texts = TrayTexts::from_language("vi");

        assert_eq!(texts.show_main, "Mở cửa sổ chính");
        assert_eq!(texts.open_website, "Mở trang web chính thức");
        assert_eq!(texts.no_providers_label, "(chưa có nhà cung cấp)");
        assert_eq!(texts.lightweight_mode, "Chế độ gọn nhẹ");
        assert_eq!(texts.quit, "Thoát");
        assert_eq!(texts._auto_label, "Tự động (chuyển khi lỗi)");
    }

    #[test]
    fn unsupported_tray_language_falls_back_to_english() {
        let texts = TrayTexts::from_language("unsupported");

        assert_eq!(texts.show_main, "Open main window");
        assert_eq!(texts.open_website, "Open Official Website");
        assert_eq!(texts.no_providers_label, "(no providers)");
        assert_eq!(texts.lightweight_mode, "Lightweight Mode");
        assert_eq!(texts.quit, "Quit");
        assert_eq!(texts._auto_label, "Auto (Failover)");
    }

    fn make_quota(tool: &str, success: bool, tiers: Vec<QuotaTier>) -> SubscriptionQuota {
        SubscriptionQuota {
            tool: tool.to_string(),
            credential_status: CredentialStatus::Valid,
            credential_message: None,
            success,
            tiers,
            extra_usage: None,
            error: None,
            queried_at: Some(0),
        }
    }

    fn tier(name: &str, utilization: f64) -> QuotaTier {
        QuotaTier {
            name: name.to_string(),
            utilization,
            resets_at: None,
            used_value_usd: None,
            max_value_usd: None,
        }
    }

    #[test]
    fn claude_summary_uses_h_and_w_labels() {
        let quota = make_quota(
            "claude",
            true,
            vec![tier("five_hour", 9.0), tier("seven_day", 27.0)],
        );
        let s = format_subscription_summary(&quota).expect("should format");
        assert!(s.contains("h9%"), "expected h9% in {s}");
        assert!(s.contains("w27%"), "expected w27% in {s}");
    }

    #[test]
    fn gemini_summary_uses_p_and_f_labels() {
        let quota = make_quota(
            "gemini",
            true,
            vec![tier("gemini_pro", 15.0), tier("gemini_flash", 42.0)],
        );
        let s = format_subscription_summary(&quota).expect("should format");
        assert!(s.contains("p15%"), "expected p15% in {s}");
        assert!(s.contains("f42%"), "expected f42% in {s}");
    }

    #[test]
    fn gemini_summary_includes_all_three_tiers() {
        let quota = make_quota(
            "gemini",
            true,
            vec![
                tier("gemini_pro", 5.0),
                tier("gemini_flash", 42.0),
                tier("gemini_flash_lite", 80.0),
            ],
        );
        let s = format_subscription_summary(&quota).expect("should format");
        assert!(s.contains("p5%"), "expected p5% in {s}");
        assert!(s.contains("f42%"), "expected f42% in {s}");
        assert!(s.contains("l80%"), "expected l80% in {s}");
    }

    #[test]
    fn gemini_summary_lite_only_still_renders() {
        // If flash_lite is the only tier returned by the API, it should still
        // render so the tray does not go blank while the frontend footer shows
        // usage.
        let quota = make_quota("gemini", true, vec![tier("gemini_flash_lite", 80.0)]);
        let s = format_subscription_summary(&quota).expect("should format");
        assert!(s.contains("l80%"), "expected l80% in {s}");
    }

    #[test]
    fn gemini_summary_emoji_reflects_highest_tier_including_lite() {
        // When lite has the highest utilization, the emoji must be red and must
        // not be hidden by pro or flash.
        let quota = make_quota(
            "gemini",
            true,
            vec![
                tier("gemini_pro", 10.0),
                tier("gemini_flash", 20.0),
                tier("gemini_flash_lite", 95.0),
            ],
        );
        let s = format_subscription_summary(&quota).unwrap();
        assert!(
            s.starts_with("\u{1F534}"),
            "expected red emoji (lite worst) in {s}"
        );
    }

    #[test]
    fn worst_emoji_reflects_highest_utilization() {
        // Red is `\u{1F534}`. Any tier at or above 90% should render red.
        let quota = make_quota(
            "claude",
            true,
            vec![tier("five_hour", 10.0), tier("seven_day", 95.0)],
        );
        let s = format_subscription_summary(&quota).unwrap();
        assert!(s.starts_with("\u{1F534}"), "expected red emoji in {s}");
    }

    #[test]
    fn subscription_summary_week_aliases_use_highest_utilization() {
        let quota = make_quota(
            "claude",
            true,
            vec![
                tier(TIER_FIVE_HOUR, 10.0),
                tier(TIER_SEVEN_DAY_OPUS, 20.0),
                tier(TIER_SEVEN_DAY_SONNET, 95.0),
            ],
        );
        let s = format_subscription_summary(&quota).unwrap();
        assert!(s.contains("w95%"), "expected w95% in {s}");
        assert!(s.starts_with("\u{1F534}"), "expected red emoji in {s}");
    }

    #[test]
    fn failure_quota_returns_none() {
        let quota = make_quota("claude", false, vec![tier("five_hour", 50.0)]);
        assert!(format_subscription_summary(&quota).is_none());
    }

    #[test]
    fn unknown_tiers_return_none() {
        let quota = make_quota("claude", true, vec![tier("one_hour", 80.0)]);
        assert!(format_subscription_summary(&quota).is_none());
    }

    #[test]
    fn gemini_without_any_known_tiers_returns_none() {
        // A degraded response with none of the known pro / flash / flash_lite
        // tiers should return None.
        let quota = make_quota("gemini", true, vec![tier("some_future_tier", 80.0)]);
        assert!(format_subscription_summary(&quota).is_none());
    }

    fn usage_data(plan_name: Option<&str>, utilization: f64) -> UsageData {
        UsageData {
            plan_name: plan_name.map(String::from),
            extra: None,
            is_valid: Some(true),
            invalid_message: None,
            total: Some(100.0),
            used: Some(utilization),
            remaining: Some(100.0 - utilization),
            unit: Some("%".to_string()),
        }
    }

    fn usage_result(success: bool, data: Vec<UsageData>) -> UsageResult {
        UsageResult {
            success,
            data: if data.is_empty() { None } else { Some(data) },
            error: None,
        }
    }

    #[test]
    fn script_summary_token_plan_two_tiers() {
        let r = usage_result(
            true,
            vec![
                usage_data(Some(TIER_FIVE_HOUR), 12.0),
                usage_data(Some(TIER_WEEKLY_LIMIT), 80.0),
            ],
        );
        let s = format_script_summary(&r).expect("should format");
        assert!(s.contains("h12%"), "expected h12% in {s}");
        assert!(s.contains("w80%"), "expected w80% in {s}");
        assert!(s.starts_with("\u{1F7E0}"), "expected orange emoji in {s}");
    }

    #[test]
    fn script_summary_token_plan_worst_drives_emoji() {
        let r = usage_result(
            true,
            vec![
                usage_data(Some(TIER_FIVE_HOUR), 20.0),
                usage_data(Some(TIER_WEEKLY_LIMIT), 95.0),
            ],
        );
        let s = format_script_summary(&r).unwrap();
        assert!(s.starts_with("\u{1F534}"), "expected red emoji in {s}");
    }

    #[test]
    fn script_summary_token_plan_five_hour_only() {
        let r = usage_result(true, vec![usage_data(Some(TIER_FIVE_HOUR), 8.0)]);
        let s = format_script_summary(&r).expect("should format");
        assert!(s.contains("h8%"), "expected h8% in {s}");
        assert!(
            !s.contains("plan_name"),
            "plan_name should not leak into label: {s}"
        );
    }

    #[test]
    fn script_summary_token_plan_weekly_only() {
        let r = usage_result(true, vec![usage_data(Some(TIER_WEEKLY_LIMIT), 50.0)]);
        let s = format_script_summary(&r).expect("should format");
        assert!(s.contains("w50%"), "expected w50% in {s}");
    }

    #[test]
    fn script_summary_token_plan_volcengine_three_tiers_with_monthly() {
        // Volcengine Agent Plan returns 5h / week / month tiers. The tray
        // should include the monthly `m` bucket instead of silently dropping it.
        let r = usage_result(
            true,
            vec![
                usage_data(Some(TIER_FIVE_HOUR), 25.0),
                usage_data(Some(TIER_WEEKLY_LIMIT), 30.0),
                usage_data(Some(TIER_MONTHLY), 42.0),
            ],
        );
        let s = format_script_summary(&r).expect("should format");
        assert!(s.contains("h25%"), "expected h25% in {s}");
        assert!(s.contains("w30%"), "expected w30% in {s}");
        assert!(s.contains("m42%"), "expected m42% in {s}");
    }

    #[test]
    fn script_summary_token_plan_monthly_only_renders_label_not_raw_name() {
        // When only the monthly bucket is active, render the `m` label instead
        // of falling back to the raw `monthly` machine name.
        let r = usage_result(true, vec![usage_data(Some(TIER_MONTHLY), 60.0)]);
        let s = format_script_summary(&r).expect("should format");
        assert!(s.contains("m60%"), "expected m60% in {s}");
        assert!(
            !s.contains("monthly"),
            "raw tier name should not leak into label: {s}"
        );
    }

    #[test]
    fn script_summary_official_subscription_claude_uses_h_and_w_labels() {
        let r = usage_result(
            true,
            vec![
                usage_data(Some(TIER_FIVE_HOUR), 12.0),
                usage_data(Some(TIER_SEVEN_DAY), 80.0),
            ],
        );
        let s = format_script_summary(&r).expect("should format");
        assert!(s.contains("h12%"), "expected h12% in {s}");
        assert!(s.contains("w80%"), "expected w80% in {s}");
        assert!(
            !s.contains(TIER_SEVEN_DAY),
            "tier machine name should not leak into label: {s}"
        );
    }

    #[test]
    fn script_summary_week_aliases_use_highest_utilization() {
        let r = usage_result(
            true,
            vec![
                usage_data(Some(TIER_FIVE_HOUR), 10.0),
                usage_data(Some(TIER_SEVEN_DAY_OPUS), 20.0),
                usage_data(Some(TIER_SEVEN_DAY_SONNET), 95.0),
            ],
        );
        let s = format_script_summary(&r).unwrap();
        assert!(s.contains("w95%"), "expected w95% in {s}");
        assert!(s.starts_with("\u{1F534}"), "expected red emoji in {s}");
    }

    #[test]
    fn script_summary_official_subscription_gemini_uses_short_labels() {
        let r = usage_result(
            true,
            vec![
                usage_data(Some(TIER_GEMINI_PRO), 15.0),
                usage_data(Some(TIER_GEMINI_FLASH), 42.0),
                usage_data(Some(TIER_GEMINI_FLASH_LITE), 80.0),
            ],
        );
        let s = format_script_summary(&r).expect("should format");
        assert!(s.contains("p15%"), "expected p15% in {s}");
        assert!(s.contains("f42%"), "expected f42% in {s}");
        assert!(s.contains("l80%"), "expected l80% in {s}");
        assert!(
            !s.contains("gemini_"),
            "Gemini tier machine names should not leak into label: {s}"
        );
    }

    #[test]
    fn script_summary_single_bucket_fallback_with_plan_name() {
        let r = usage_result(true, vec![usage_data(Some("Copilot Pro"), 40.0)]);
        let s = format_script_summary(&r).expect("should format");
        assert!(s.contains("Copilot Pro"), "expected plan name in {s}");
        assert!(s.contains("40%"), "expected 40% in {s}");
        assert!(
            !s.contains("h40%"),
            "must not relabel non-token-plan data as h: {s}"
        );
    }

    #[test]
    fn script_summary_single_bucket_fallback_without_plan_name() {
        let r = usage_result(true, vec![usage_data(None, 15.0)]);
        let s = format_script_summary(&r).expect("should format");
        assert_eq!(s, "\u{1F7E2} 15%", "expected emoji + pct only, got {s}");
    }

    #[test]
    fn script_summary_failure_returns_none() {
        let r = usage_result(false, vec![usage_data(Some(TIER_FIVE_HOUR), 12.0)]);
        assert!(format_script_summary(&r).is_none());
    }

    #[test]
    fn script_summary_empty_data_returns_none() {
        let r = usage_result(true, vec![]);
        assert!(format_script_summary(&r).is_none());
    }
}
