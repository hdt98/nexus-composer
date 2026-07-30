//! Panic hook module
//!
//! Captures panic details in `<app_config_dir>/crash.log`, which defaults to
//! `~/.nexus-composer/crash.log`, for crash diagnosis.

use std::fs::OpenOptions;
use std::io::Write;
use std::panic;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Application version from Cargo.toml.
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

static APP_CONFIG_DIR: OnceLock<PathBuf> = OnceLock::new();

pub fn init_app_config_dir(dir: PathBuf) {
    let _ = APP_CONFIG_DIR.set(dir);
}

/// Return the crash log path.
fn get_crash_log_path() -> PathBuf {
    APP_CONFIG_DIR
        .get()
        .map(|dir| dir.join("crash.log"))
        .unwrap_or_else(crate::config::get_app_crash_log_path)
}

/// Return the log directory path.
pub fn get_log_dir() -> PathBuf {
    APP_CONFIG_DIR
        .get()
        .map(|dir| dir.join("logs"))
        .unwrap_or_else(crate::config::get_app_log_dir)
}

/// Return environment information without panicking.
fn get_system_info() -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let family = std::env::consts::FAMILY;

    // Get the current working directory safely.
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    // Get current thread information safely.
    let thread = std::thread::current();
    let thread_name = thread.name().unwrap_or("unnamed");
    let thread_id = format!("{:?}", thread.id());

    format!(
        "OS: {os} ({family})\n\
         Arch: {arch}\n\
         App Version: {APP_VERSION}\n\
         Working Dir: {cwd}\n\
         Thread: {thread_name} (ID: {thread_id})"
    )
}

/// Install a panic hook that writes crash details to the log file.
///
/// Call during startup to record all panics. The log contains:
/// - Timestamp.
/// - Application version and system information.
/// - Panic message.
/// - Source location.
/// - Full backtrace.
pub fn setup_panic_hook() {
    // Enable backtraces in release builds.
    if std::env::var("RUST_BACKTRACE").is_err() {
        std::env::set_var("RUST_BACKTRACE", "1");
    }

    let default_hook = panic::take_hook();

    panic::set_hook(Box::new(move |panic_info| {
        let log_path = get_crash_log_path();

        // Ensure the log directory exists.
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        // Build crash details while protecting timestamp formatting from nested panics.
        let timestamp = std::panic::catch_unwind(|| {
            chrono::Local::now()
                .format("%Y-%m-%d %H:%M:%S%.3f")
                .to_string()
        })
        .unwrap_or_else(|_| {
            // Fall back to a Unix timestamp if chrono panics.
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| format!("unix:{}.{:03}", d.as_secs(), d.subsec_millis()))
                .unwrap_or_else(|_| "unknown".to_string())
        });

        // Get system information.
        let system_info = std::panic::catch_unwind(get_system_info)
            .unwrap_or_else(|_| "Failed to get system info".to_string());

        // Extract the panic message from supported payload types.
        let message = if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            // Fall back to the Display representation.
            format!("{panic_info}")
        };

        // Get the source location.
        let location = if let Some(loc) = panic_info.location() {
            format!(
                "File: {}\n         Line: {}\n         Column: {}",
                loc.file(),
                loc.line(),
                loc.column()
            )
        } else {
            "Unknown location".to_string()
        };

        // Capture the full backtrace.
        let backtrace = std::backtrace::Backtrace::force_capture();
        let backtrace_str = format!("{backtrace}");

        // Format the crash log entry.
        let separator = "=".repeat(80);
        let sub_separator = "-".repeat(40);
        let crash_entry = format!(
            r#"
{separator}
[CRASH REPORT] {timestamp}
{separator}

{sub_separator}
System Information
{sub_separator}
{system_info}

{sub_separator}
Error Details
{sub_separator}
Message: {message}

Location: {location}

{sub_separator}
Stack Trace (Backtrace)
{sub_separator}
{backtrace_str}

{separator}
"#
        );

        // Append to the crash log.
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&log_path) {
            let _ = file.write_all(crash_entry.as_bytes());
            let _ = file.flush();

            // Report the crash log path to stderr.
            eprintln!(
                "\n[Nexus Composer] Crash log saved to: {}",
                log_path.display()
            );
        }

        // Also emit the crash report to stderr for debugging.
        eprintln!("{crash_entry}");

        // Invoke the default hook.
        default_hook(panic_info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crash_log_path() {
        let path = get_crash_log_path();
        assert!(path.ends_with("crash.log"));
        assert!(path.to_string_lossy().contains(".nexus-composer"));
    }

    #[test]
    fn test_system_info() {
        let info = get_system_info();
        assert!(info.contains("OS:"));
        assert!(info.contains("Arch:"));
        assert!(info.contains("App Version:"));
    }
}
