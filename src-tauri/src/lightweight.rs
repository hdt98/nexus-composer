use std::sync::atomic::{AtomicBool, Ordering};

use tauri::Manager;

static LIGHTWEIGHT_MODE: AtomicBool = AtomicBool::new(false);

pub fn enter_lightweight_mode(app: &tauri::AppHandle) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.set_skip_taskbar(true);
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Err(e) = crate::tray::apply_tray_policy(app, false) {
            log::warn!("Failed to apply lightweight tray policy: {e}");
        }
    }

    if let Some(window) = app.get_webview_window("main") {
        crate::save_window_state_before_exit(app);
        window
            .destroy()
            .map_err(|e| format!("Failed to destroy the main window: {e}"))?;
    }
    // else: already in lightweight mode or window not found, just set the flag

    LIGHTWEIGHT_MODE.store(true, Ordering::Release);
    crate::tray::refresh_tray_menu(app);
    log::info!("Entered lightweight mode");
    Ok(())
}

pub fn exit_lightweight_mode(app: &tauri::AppHandle) -> Result<(), String> {
    use tauri::WebviewWindowBuilder;

    if app.get_webview_window("main").is_some() {
        let presentation = crate::present_main_window(app, "exit lightweight mode");
        LIGHTWEIGHT_MODE.store(false, Ordering::Release);
        crate::tray::refresh_tray_menu(app);
        presentation?;
        log::info!("Exited lightweight mode");
        return Ok(());
    }

    let window_config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == "main")
        .ok_or("Main window configuration was not found")?;

    WebviewWindowBuilder::from_config(app, window_config)
        .map_err(|e| format!("Failed to load main window configuration: {e}"))?
        .build()
        .map_err(|e| format!("Failed to create the main window: {e}"))?;

    let presentation = crate::present_main_window(app, "recreate after lightweight mode");
    LIGHTWEIGHT_MODE.store(false, Ordering::Release);
    crate::tray::refresh_tray_menu(app);
    presentation?;
    log::info!("Exited lightweight mode");
    Ok(())
}

pub fn is_lightweight_mode() -> bool {
    LIGHTWEIGHT_MODE.load(Ordering::Acquire)
}
