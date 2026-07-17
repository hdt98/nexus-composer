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
            .map_err(|e| format!("销毁主窗口失败: {e}"))?;
    }
    // else: already in lightweight mode or window not found, just set the flag

    LIGHTWEIGHT_MODE.store(true, Ordering::Release);
    crate::tray::refresh_tray_menu(app);
    log::info!("进入轻量模式");
    Ok(())
}

pub fn exit_lightweight_mode(app: &tauri::AppHandle) -> Result<(), String> {
    use tauri::WebviewWindowBuilder;

    if app.get_webview_window("main").is_some() {
        let presentation = crate::present_main_window(app, "exit lightweight mode");
        LIGHTWEIGHT_MODE.store(false, Ordering::Release);
        crate::tray::refresh_tray_menu(app);
        presentation?;
        log::info!("退出轻量模式");
        return Ok(());
    }

    let window_config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == "main")
        .ok_or("主窗口配置未找到")?;

    WebviewWindowBuilder::from_config(app, window_config)
        .map_err(|e| format!("加载主窗口配置失败: {e}"))?
        .build()
        .map_err(|e| format!("创建主窗口失败: {e}"))?;

    let presentation = crate::present_main_window(app, "recreate after lightweight mode");
    LIGHTWEIGHT_MODE.store(false, Ordering::Release);
    crate::tray::refresh_tray_menu(app);
    presentation?;
    log::info!("退出轻量模式");
    Ok(())
}

pub fn is_lightweight_mode() -> bool {
    LIGHTWEIGHT_MODE.load(Ordering::Acquire)
}
