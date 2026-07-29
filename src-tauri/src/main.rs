// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Set WebKit environment variables on Linux to avoid DMA-BUF rendering issues.
    // On some systems, such as Debian 13.2 with an Nvidia GPU, WebKitGTK's DMA-BUF renderer can
    // produce a blank or black window. See https://github.com/tauri-apps/tauri/issues/9394.
    #[cfg(target_os = "linux")]
    {
        if std::env::var("WEBKIT_DISABLE_DMABUF_RENDERER").is_err() {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
        // Disable WebKitGTK compositing to avoid webview crashes during resize and surface
        // negotiation issues under some Wayland compositors, where the UI stops receiving clicks
        // until the window is maximized and restored. See tauri-apps/tauri#9394.
        if std::env::var("WEBKIT_DISABLE_COMPOSITING_MODE").is_err() {
            std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
        }

        // The AppImage GTK startup hook (linuxdeploy-plugin-gtk.sh) unconditionally exports
        // GDK_BACKEND=x11 to force XWayland and avoid an older Wayland crash (tauri-apps/tauri#8541).
        // On newer Wayland + NVIDIA systems, however, forced XWayland can prevent the WebKitGTK
        // webview from receiving pointer events and can render black after resize. Native Wayland
        // fixes this, and the original crash no longer reproduces with WebKitGTK 2.52. Because the
        // hook overwrites the user's GDK_BACKEND, CC_SWITCH_GDK_BACKEND=wayland provides an escape
        // hatch that the hook does not modify while leaving the default behavior unchanged.
        if let Ok(backend) = std::env::var("CC_SWITCH_GDK_BACKEND") {
            if !backend.is_empty() {
                std::env::set_var("GDK_BACKEND", backend);
            }
        }
    }

    nexus_composer_lib::run();
}
