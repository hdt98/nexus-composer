//! Real-time usage statistics refresh events
//!
//! Emits `usage-log-recorded` when proxy logs, session synchronization, archives, or other paths
//! add data to `proxy_request_logs`, allowing UsageDashboard to invalidate its query immediately.
//!
//! Design:
//! - A global AppHandle avoids carrying it through log-writing paths.
//! - A 200 ms debounce combines bursts such as streaming log writes into one event.
//! - Notification failures are logged without blocking or propagating to writers.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use tauri::{AppHandle, Emitter};

/// Event name observed by the frontend.
pub const EVENT_USAGE_LOG_RECORDED: &str = "usage-log-recorded";

/// Debounce window that combines notifications within 200 ms.
const DEBOUNCE_WINDOW: Duration = Duration::from_millis(200);

static APP_HANDLE: OnceLock<AppHandle> = OnceLock::new();

/// True while a scheduled task is waiting to emit; later notifications join that task.
static EMIT_SCHEDULED: AtomicBool = AtomicBool::new(false);

/// Initialize the AppHandle once during application setup.
///
/// Repeated calls are harmless because OnceLock accepts only the first value, but startup should
/// call this only once from `lib.rs::run`.
pub fn init(handle: AppHandle) {
    if APP_HANDLE.set(handle).is_err() {
        log::debug!("Ignored repeated usage_events::init call");
    } else {
        log::info!("[usage-event] AppHandle initialized; event delivery enabled");
    }
}

/// Notify the frontend that a usage log was recorded.
///
/// Callers do not need an AppHandle and may call from any thread or write path. The internal
/// 200 ms debounce does not block the calling thread.
pub fn notify_log_recorded() {
    // Ignore notifications before setup or in tests without an AppHandle.
    let Some(handle) = APP_HANDLE.get() else {
        return;
    };

    // Merge into an already scheduled task without spawning another thread.
    if EMIT_SCHEDULED.swap(true, Ordering::AcqRel) {
        return;
    }

    let handle = handle.clone();
    std::thread::spawn(move || {
        std::thread::sleep(DEBOUNCE_WINDOW);
        // Clear the flag before emitting so a concurrent notification schedules the next window.
        EMIT_SCHEDULED.store(false, Ordering::Release);

        if let Err(e) = handle.emit(EVENT_USAGE_LOG_RECORDED, ()) {
            log::warn!("Failed to emit {EVENT_USAGE_LOG_RECORDED}: {e}");
        }
    });
}
