//! General settings data-access object.
//!
//! Provides key-value storage for general settings.

use crate::database::{lock_conn, Database};
use crate::error::AppError;
use rusqlite::params;

impl Database {
    const LEGACY_COMMON_CONFIG_MIGRATED_KEY: &'static str = "common_config_legacy_migrated_v1";

    fn config_snippet_cleared_key(app_type: &str) -> String {
        format!("common_config_{app_type}_cleared")
    }

    /// Returns a setting value.
    pub fn get_setting(&self, key: &str) -> Result<Option<String>, AppError> {
        let conn = lock_conn!(self.conn);
        let mut stmt = conn
            .prepare("SELECT value FROM settings WHERE key = ?1")
            .map_err(|e| AppError::Database(e.to_string()))?;

        let mut rows = stmt
            .query(params![key])
            .map_err(|e| AppError::Database(e.to_string()))?;

        if let Some(row) = rows.next().map_err(|e| AppError::Database(e.to_string()))? {
            Ok(Some(
                row.get(0).map_err(|e| AppError::Database(e.to_string()))?,
            ))
        } else {
            Ok(None)
        }
    }

    /// Reads a flag with boolean semantics: `"true"` or `"1"` means true; every other value is false.
    ///
    /// Used for one-time startup flags such as `official_providers_seeded` and
    /// `first_run_notice_shown`. Unlike legacy helpers such as
    /// `is_legacy_common_config_migrated`, which accept only `"true"`, this also accepts `"1"` for
    /// compatibility with the existing `init_default_official_providers` representation.
    pub fn get_bool_flag(&self, key: &str) -> Result<bool, AppError> {
        Ok(matches!(
            self.get_setting(key)?.as_deref(),
            Some("true") | Some("1")
        ))
    }

    /// Sets a value.
    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            params![key, value],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(())
    }

    // --- Common configuration snippet ---

    /// Returns the common configuration snippet.
    pub fn get_config_snippet(&self, app_type: &str) -> Result<Option<String>, AppError> {
        self.get_setting(&format!("common_config_{app_type}"))
    }

    /// Returns whether the user explicitly cleared the common configuration snippet.
    pub fn is_config_snippet_cleared(&self, app_type: &str) -> Result<bool, AppError> {
        Ok(self
            .get_setting(&Self::config_snippet_cleared_key(app_type))?
            .as_deref()
            == Some("true"))
    }

    /// Sets whether the common configuration snippet was explicitly cleared.
    pub fn set_config_snippet_cleared(
        &self,
        app_type: &str,
        cleared: bool,
    ) -> Result<(), AppError> {
        let key = Self::config_snippet_cleared_key(app_type);
        if cleared {
            self.set_setting(&key, "true")
        } else {
            let conn = lock_conn!(self.conn);
            conn.execute("DELETE FROM settings WHERE key = ?1", params![key])
                .map_err(|e| AppError::Database(e.to_string()))?;
            Ok(())
        }
    }

    /// Returns whether automatic extraction of the common snippet from live configuration is enabled.
    pub fn should_auto_extract_config_snippet(&self, app_type: &str) -> Result<bool, AppError> {
        Ok(self.get_config_snippet(app_type)?.is_none()
            && !self.is_config_snippet_cleared(app_type)?)
    }

    /// Returns whether the legacy common-configuration migration has run.
    pub fn is_legacy_common_config_migrated(&self) -> Result<bool, AppError> {
        Ok(self
            .get_setting(Self::LEGACY_COMMON_CONFIG_MIGRATED_KEY)?
            .as_deref()
            == Some("true"))
    }

    /// Marks the legacy common-configuration migration complete.
    pub fn set_legacy_common_config_migrated(&self, migrated: bool) -> Result<(), AppError> {
        if migrated {
            self.set_setting(Self::LEGACY_COMMON_CONFIG_MIGRATED_KEY, "true")
        } else {
            let conn = lock_conn!(self.conn);
            conn.execute(
                "DELETE FROM settings WHERE key = ?1",
                params![Self::LEGACY_COMMON_CONFIG_MIGRATED_KEY],
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
            Ok(())
        }
    }

    /// Sets the common configuration snippet.
    pub fn set_config_snippet(
        &self,
        app_type: &str,
        snippet: Option<String>,
    ) -> Result<(), AppError> {
        let key = format!("common_config_{app_type}");
        if let Some(value) = snippet {
            self.set_setting(&key, &value)
        } else {
            // Delete the value when it is None.
            let conn = lock_conn!(self.conn);
            conn.execute("DELETE FROM settings WHERE key = ?1", params![key])
                .map_err(|e| AppError::Database(e.to_string()))?;
            Ok(())
        }
    }

    // --- Global outbound proxy ---

    /// Storage key for the global proxy URL.
    const GLOBAL_PROXY_URL_KEY: &'static str = "global_proxy_url";

    /// Returns the global outbound-proxy URL.
    ///
    /// None means the proxy is absent or cleared and connections are direct.
    /// Some(url) means a proxy is configured.
    pub fn get_global_proxy_url(&self) -> Result<Option<String>, AppError> {
        self.get_setting(Self::GLOBAL_PROXY_URL_KEY)
    }

    /// Sets the global outbound-proxy URL.
    ///
    /// - A non-empty string enables the proxy.
    /// - An empty string or None clears the proxy for direct connections.
    pub fn set_global_proxy_url(&self, url: Option<&str>) -> Result<(), AppError> {
        match url {
            Some(u) if !u.trim().is_empty() => {
                self.set_setting(Self::GLOBAL_PROXY_URL_KEY, u.trim())
            }
            _ => {
                // Clear proxy settings.
                let conn = lock_conn!(self.conn);
                conn.execute(
                    "DELETE FROM settings WHERE key = ?1",
                    params![Self::GLOBAL_PROXY_URL_KEY],
                )
                .map_err(|e| AppError::Database(e.to_string()))?;
                Ok(())
            }
        }
    }

    // --- Deprecated proxy-takeover state; use proxy_config.enabled instead ---

    /// Returns proxy-takeover state for a specified application.
    ///
    /// **Deprecated**: use `proxy_config.enabled` instead.
    /// This method reads legacy data only during database migration.
    #[deprecated(
        since = "3.9.0",
        note = "Use get_proxy_config_for_app().enabled instead"
    )]
    pub fn get_proxy_takeover_enabled(&self, app_type: &str) -> Result<bool, AppError> {
        let key = format!("proxy_takeover_{app_type}");
        match self.get_setting(&key)? {
            Some(value) => Ok(value == "true"),
            None => Ok(false),
        }
    }

    /// Sets proxy-takeover state for a specified application.
    ///
    /// **Deprecated**: use `proxy_config.enabled` instead.
    #[deprecated(
        since = "3.9.0",
        note = "Use update_proxy_config_for_app() to modify the enabled field"
    )]
    pub fn set_proxy_takeover_enabled(
        &self,
        app_type: &str,
        enabled: bool,
    ) -> Result<(), AppError> {
        let key = format!("proxy_takeover_{app_type}");
        let value = if enabled { "true" } else { "false" };
        self.set_setting(&key, value)
    }

    /// Returns whether proxy takeover is enabled for any application.
    ///
    /// **Deprecated**: use `is_live_takeover_active()` instead.
    #[deprecated(since = "3.9.0", note = "Use is_live_takeover_active() instead")]
    pub fn has_any_proxy_takeover(&self) -> Result<bool, AppError> {
        let conn = lock_conn!(self.conn);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM settings WHERE key LIKE 'proxy_takeover_%' AND value = 'true'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(count > 0)
    }

    /// Clears all proxy-takeover state by setting every proxy_takeover_* value to false.
    ///
    /// **Deprecated**: the settings table no longer stores proxy state.
    #[deprecated(
        since = "3.9.0",
        note = "Use update_proxy_config_for_app() to clear each application's enabled field"
    )]
    pub fn clear_all_proxy_takeover(&self) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE settings SET value = 'false' WHERE key LIKE 'proxy_takeover_%'",
            [],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;
        log::info!("Cleared all proxy-takeover state");
        Ok(())
    }

    // --- Rectifier configuration ---

    /// Returns rectifier configuration.
    ///
    /// Returns the saved rectifier config, or safe disabled defaults when absent.
    pub fn get_rectifier_config(&self) -> Result<crate::proxy::types::RectifierConfig, AppError> {
        match self.get_setting("rectifier_config")? {
            Some(json) => serde_json::from_str(&json).map_err(|e| {
                AppError::Database(format!("Failed to parse rectifier configuration: {e}"))
            }),
            None => Ok(crate::proxy::types::RectifierConfig::default()),
        }
    }

    /// Updates rectifier configuration.
    pub fn set_rectifier_config(
        &self,
        config: &crate::proxy::types::RectifierConfig,
    ) -> Result<(), AppError> {
        let json = serde_json::to_string(config).map_err(|e| {
            AppError::Database(format!("Failed to serialize rectifier configuration: {e}"))
        })?;
        self.set_setting("rectifier_config", &json)
    }

    // --- Optimizer configuration ---

    /// Returns optimizer configuration.
    ///
    /// Returns disabled defaults when configuration is absent.
    pub fn get_optimizer_config(&self) -> Result<crate::proxy::types::OptimizerConfig, AppError> {
        match self.get_setting("optimizer_config")? {
            Some(json) => serde_json::from_str(&json).map_err(|e| {
                AppError::Database(format!("Failed to parse optimizer configuration: {e}"))
            }),
            None => Ok(crate::proxy::types::OptimizerConfig::default()),
        }
    }

    /// Updates optimizer configuration.
    pub fn set_optimizer_config(
        &self,
        config: &crate::proxy::types::OptimizerConfig,
    ) -> Result<(), AppError> {
        let json = serde_json::to_string(config).map_err(|e| {
            AppError::Database(format!("Failed to serialize optimizer configuration: {e}"))
        })?;
        self.set_setting("optimizer_config", &json)
    }

    // --- Copilot optimizer configuration ---

    /// Returns Copilot optimizer configuration.
    ///
    /// Returns the saved config, or safe defaults that do not rewrite requests.
    pub fn get_copilot_optimizer_config(
        &self,
    ) -> Result<crate::proxy::types::CopilotOptimizerConfig, AppError> {
        match self.get_setting("copilot_optimizer_config")? {
            Some(json) => serde_json::from_str(&json).map_err(|e| {
                AppError::Database(format!(
                    "Failed to parse Copilot optimizer configuration: {e}"
                ))
            }),
            None => Ok(crate::proxy::types::CopilotOptimizerConfig::default()),
        }
    }

    /// Updates Copilot optimizer configuration.
    pub fn set_copilot_optimizer_config(
        &self,
        config: &crate::proxy::types::CopilotOptimizerConfig,
    ) -> Result<(), AppError> {
        let json = serde_json::to_string(config).map_err(|e| {
            AppError::Database(format!(
                "Failed to serialize Copilot optimizer configuration: {e}"
            ))
        })?;
        self.set_setting("copilot_optimizer_config", &json)
    }

    // --- Log configuration ---

    /// Returns log configuration.
    pub fn get_log_config(&self) -> Result<crate::proxy::types::LogConfig, AppError> {
        match self.get_setting("log_config")? {
            Some(json) => serde_json::from_str(&json)
                .map_err(|e| AppError::Database(format!("Failed to parse log configuration: {e}"))),
            None => Ok(crate::proxy::types::LogConfig::default()),
        }
    }

    /// Updates log configuration.
    pub fn set_log_config(&self, config: &crate::proxy::types::LogConfig) -> Result<(), AppError> {
        let json = serde_json::to_string(config).map_err(|e| {
            AppError::Database(format!("Failed to serialize log configuration: {e}"))
        })?;
        self.set_setting("log_config", &json)
    }
}
