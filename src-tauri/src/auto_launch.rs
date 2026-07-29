use crate::error::AppError;
use auto_launch::{AutoLaunch, AutoLaunchBuilder};

/// Returns the macOS .app bundle path.
/// Converts `/path/to/Nexus Composer.app/Contents/MacOS/Nexus Composer` to `/path/to/Nexus Composer.app`.
#[cfg(target_os = "macos")]
fn get_macos_app_bundle_path(exe_path: &std::path::Path) -> Option<std::path::PathBuf> {
    let path_str = exe_path.to_string_lossy();
    // Find the .app/Contents/MacOS/ pattern.
    if let Some(app_pos) = path_str.find(".app/Contents/MacOS/") {
        let app_bundle_end = app_pos + 4; // End of ".app".
        Some(std::path::PathBuf::from(&path_str[..app_bundle_end]))
    } else {
        None
    }
}

/// Initializes an AutoLaunch instance.
fn get_auto_launch() -> Result<AutoLaunch, AppError> {
    let app_name = "Nexus Composer";
    let exe_path = std::env::current_exe()
        .map_err(|e| AppError::Message(format!("Cannot determine the application path: {e}")))?;

    // macOS needs the .app bundle path; otherwise the AppleScript login item opens a terminal.
    #[cfg(target_os = "macos")]
    let app_path = get_macos_app_bundle_path(&exe_path).unwrap_or(exe_path);

    #[cfg(not(target_os = "macos"))]
    let app_path = exe_path;

    // Use AutoLaunchBuilder to abstract platform differences.
    // macOS: the default AppleScript method requires an .app bundle path.
    // Windows/Linux: registry or XDG autostart.
    let auto_launch = AutoLaunchBuilder::new()
        .set_app_name(app_name)
        .set_app_path(&app_path.to_string_lossy())
        .build()
        .map_err(|e| AppError::Message(format!("Failed to create AutoLaunch: {e}")))?;

    Ok(auto_launch)
}

/// Enables launch at startup.
pub fn enable_auto_launch() -> Result<(), AppError> {
    let auto_launch = get_auto_launch()?;
    auto_launch
        .enable()
        .map_err(|e| AppError::Message(format!("Failed to enable launch at startup: {e}")))?;
    log::info!("Enabled launch at startup");
    Ok(())
}

/// Disables launch at startup.
pub fn disable_auto_launch() -> Result<(), AppError> {
    let auto_launch = get_auto_launch()?;
    auto_launch
        .disable()
        .map_err(|e| AppError::Message(format!("Failed to disable launch at startup: {e}")))?;
    log::info!("Disabled launch at startup");
    Ok(())
}

/// Returns whether launch at startup is enabled.
pub fn is_auto_launch_enabled() -> Result<bool, AppError> {
    let auto_launch = get_auto_launch()?;
    auto_launch
        .is_enabled()
        .map_err(|e| AppError::Message(format!("Failed to check launch-at-startup status: {e}")))
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn test_get_macos_app_bundle_path_valid() {
        let exe_path =
            std::path::Path::new("/Applications/Nexus Composer.app/Contents/MacOS/Nexus Composer");
        let result = get_macos_app_bundle_path(exe_path);
        assert_eq!(
            result,
            Some(std::path::PathBuf::from("/Applications/Nexus Composer.app"))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_get_macos_app_bundle_path_with_spaces() {
        let exe_path = std::path::Path::new(
            "/Users/test/My Apps/Nexus Composer.app/Contents/MacOS/Nexus Composer",
        );
        let result = get_macos_app_bundle_path(exe_path);
        assert_eq!(
            result,
            Some(std::path::PathBuf::from(
                "/Users/test/My Apps/Nexus Composer.app"
            ))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_get_macos_app_bundle_path_not_in_bundle() {
        let exe_path = std::path::Path::new("/usr/local/bin/nexus-composer");
        let result = get_macos_app_bundle_path(exe_path);
        assert_eq!(result, None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_get_macos_app_bundle_path_dev_build() {
        // Development paths are usually outside an .app bundle.
        let exe_path = std::path::Path::new("/Users/dev/project/target/debug/nexus-composer");
        let result = get_macos_app_bundle_path(exe_path);
        assert_eq!(result, None);
    }
}
