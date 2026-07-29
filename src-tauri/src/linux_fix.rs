//! Linux-specific main-window recovery workaround.
//!
//! Addresses a Tauri 2.x issue where the UI does not respond to clicks after startup on some Linux
//! distributions, especially under Wayland or specific WebKitGTK versions:
//!
//! - **Failure mode A** (Tauri #10746 / wry #637): the webview does not receive keyboard focus after
//!   `show()`, so X11/Wayland consumes the first click to activate the window instead of forwarding it.
//! - **Failure mode B**: GTK surface and WebKitWebView input-region size negotiation fails on the
//!   `visible:false` → `show()` path. The entire window remains unresponsive until another
//!   `size_allocate`, such as maximizing and restoring it.
//!
//! This module exports [`nudge_main_window`], which invisibly reproduces the manual maximize/restore
//! workaround through explicit set_focus plus a ±1 px synthetic resize. Every path that presents the
//! main window, including normal startup, deep-link activation, single_instance callbacks, tray
//! show_main, and lightweight-mode exit, should call it after the existing `set_focus()`.

use std::time::Duration;

use tauri::{PhysicalSize, WebviewWindow};

/// Delay after webview realization so the GTK main loop can process the event.
/// The community-derived 200 ms value is long enough for set_focus to work without making the
/// initial interaction delay noticeable.
const REALIZE_WAIT: Duration = Duration::from_millis(200);

/// Interval between the two steps of the ±1 px synthetic resize, allowing GTK to process the first
/// `size_allocate` before receiving the second. Tao's Linux size API is asynchronous through
/// `gtk_window_resize` and compositor configuration, so 100 ms prevents the compositor from
/// coalescing both resizes.
const RESIZE_GAP: Duration = Duration::from_millis(100);

/// Additional delay before reading the reconciled size. After 200 ms + 100 ms + 500 ms, about
/// 800 ms total, verify that the window returned to its original size. This gives compositors enough
/// time to drain their resize queues.
const RECONCILE_WAIT: Duration = Duration::from_millis(500);

/// Runs the Linux-specific focus and surface-reactivation sequence on the main window.
///
/// Fire-and-forget: spawns an asynchronous task that completes after roughly 250 ms while the caller
/// returns immediately without blocking the UI.
pub(crate) fn nudge_main_window(window: WebviewWindow) {
    // First set_focus: the webview may not be realized yet, so this often has no effect, but it is
    // inexpensive and thread-safe through run_on_main_thread.
    let _ = window.set_focus();

    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(REALIZE_WAIT).await;

        // Second set_focus: the webview is now realized, so this works on most distributions and
        // removes failure mode A.
        let _ = window.set_focus();

        // Synthetic resize: read inner_size, add 1 px, then restore it. This triggers GTK
        // size-allocate → WebKitWebViewBase::size_allocate and reattaches the input surface,
        // removing failure mode B.
        //
        // Use PhysicalSize to avoid logical-coordinate drift across DPI values, and saturating_add
        // to prevent overflow at extreme dimensions.
        match window.inner_size() {
            Ok(original) => {
                let bumped = PhysicalSize::new(original.width.saturating_add(1), original.height);
                let _ = window.set_size(bumped);
                tokio::time::sleep(RESIZE_GAP).await;
                let _ = window.set_size(original);
                log::info!("Linux: applied focus and surface reactivation to the main window");

                // Reconcile the size after reading it back. Tao's Linux size API is asynchronous:
                // `set_size` only enqueues a resize in the GTK main loop, and a compositor may coalesce
                // consecutive requests, especially the second `set_size(original)`, leaving width+1.
                // After the compositor drains its queue, read the actual size and issue one fallback
                // `set_size(original)` if drift remains.
                //
                // Known limitation: tiling Wayland compositors such as sway, river, and Hyprland may
                // ignore `set_size` entirely. Reconciliation then reports zero drift because both calls
                // are no-ops, even though failure mode B remains. Users must work around this with
                // GDK_BACKEND=x11, as documented in the README.
                tokio::time::sleep(RECONCILE_WAIT).await;
                match window.inner_size() {
                    Ok(after) => {
                        if after.width != original.width || after.height != original.height {
                            log::info!(
                                "Linux nudge size drift: expected={}x{}, got={}x{}; applied compensation",
                                original.width,
                                original.height,
                                after.width,
                                after.height
                            );
                            let _ = window.set_size(original);
                            // Final verification: if size still differs after compensation, warn users
                            // and developers that reconciliation failed. The window remains at an
                            // unexpected size, usually +1 px, in this extreme fallback case.
                            if let Ok(final_size) = window.inner_size() {
                                if final_size.width != original.width
                                    || final_size.height != original.height
                                {
                                    log::warn!(
                                        "Linux nudge size still differs after compensation: expected={}x{}, got={}x{}",
                                        original.width,
                                        original.height,
                                        final_size.width,
                                        final_size.height
                                    );
                                }
                            }
                        }
                    }
                    Err(e) => {
                        log::warn!(
                            "Linux nudge: failed to read inner_size for reconciliation: {e}"
                        );
                    }
                }
            }
            Err(e) => {
                // This failure path is rare. Applying only set_focus is still better than doing
                // nothing, so a resize failure must not disable the entire workaround.
                log::warn!(
                    "Linux nudge: failed to read inner_size; skipping synthetic resize: {e}"
                );
            }
        }
    });
}
