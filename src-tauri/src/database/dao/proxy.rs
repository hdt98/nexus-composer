//! Proxy data-access layer.
//!
//! Handles database operations for proxy configuration, provider health, and usage.

use std::{collections::HashSet, str::FromStr};

use crate::error::AppError;
use crate::proxy::types::*;
use rust_decimal::Decimal;

use super::super::{lock_conn, Database};

pub(crate) const PRICING_SOURCE_RESPONSE: &str = "response";
pub(crate) const PRICING_SOURCE_REQUEST: &str = "request";

pub(crate) fn validate_cost_multiplier(value: &str) -> Result<Decimal, AppError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(AppError::localized(
            "error.multiplierEmpty",
            "Multiplier cannot be empty",
            "Multiplier cannot be empty",
        ));
    }
    let parsed = Decimal::from_str(trimmed).map_err(|e| {
        AppError::localized(
            "error.invalidMultiplier",
            format!("Invalid multiplier: {value} - {e}"),
            format!("Invalid multiplier: {value} - {e}"),
        )
    })?;
    if parsed < Decimal::ZERO {
        return Err(AppError::localized(
            "error.invalidMultiplier",
            format!("Invalid multiplier: {value} - multiplier cannot be negative"),
            format!("Invalid multiplier: {value} - multiplier cannot be negative"),
        ));
    }
    Ok(parsed)
}

pub(crate) fn validate_pricing_source(value: &str) -> Result<&str, AppError> {
    let trimmed = value.trim();
    if trimmed == PRICING_SOURCE_RESPONSE || trimmed == PRICING_SOURCE_REQUEST {
        Ok(trimmed)
    } else {
        Err(AppError::localized(
            "error.invalidPricingMode",
            format!("Invalid pricing mode: {value}"),
            format!("Invalid pricing mode: {value}"),
        ))
    }
}

impl Database {
    // ==================== Global Proxy Config ====================

    /// Returns global proxy configuration fields.
    ///
    /// Reads from the Claude row; all three rows mirror these values.
    pub async fn get_global_proxy_config(&self) -> Result<GlobalProxyConfig, AppError> {
        // Limit the connection scope so its lock is not held across await.
        let result = {
            let conn = lock_conn!(self.conn);
            conn.query_row(
                "SELECT proxy_enabled, listen_address, listen_port, enable_logging
                 FROM proxy_config WHERE app_type = 'claude'",
                [],
                |row| {
                    Ok(GlobalProxyConfig {
                        proxy_enabled: row.get::<_, i32>(0)? != 0,
                        listen_address: row.get(1)?,
                        listen_port: row.get::<_, i32>(2)? as u16,
                        enable_logging: row.get::<_, i32>(3)? != 0,
                    })
                },
            )
        };
        // The connection was released at the end of the block.

        match result {
            Ok(config) => Ok(config),
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                // Create default configuration when absent.
                self.init_proxy_config_rows().await?;
                Ok(GlobalProxyConfig {
                    proxy_enabled: false,
                    listen_address: "127.0.0.1".to_string(),
                    listen_port: 15721,
                    enable_logging: true,
                })
            }
            Err(e) => Err(AppError::Database(e.to_string())),
        }
    }

    /// Updates global proxy configuration in all three mirrored rows.
    pub async fn update_global_proxy_config(
        &self,
        config: GlobalProxyConfig,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);

        conn.execute(
            "UPDATE proxy_config SET
                proxy_enabled = ?1,
                listen_address = ?2,
                listen_port = ?3,
                enable_logging = ?4,
                updated_at = datetime('now')",
            rusqlite::params![
                if config.proxy_enabled { 1 } else { 0 },
                config.listen_address,
                config.listen_port as i32,
                if config.enable_logging { 1 } else { 0 },
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(())
    }

    /// Returns the default cost multiplier.
    pub async fn get_default_cost_multiplier(&self, app_type: &str) -> Result<String, AppError> {
        let result = {
            let conn = lock_conn!(self.conn);
            conn.query_row(
                "SELECT default_cost_multiplier FROM proxy_config WHERE app_type = ?1",
                [app_type],
                |row| row.get(0),
            )
        };

        match result {
            Ok(value) => Ok(value),
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                self.init_proxy_config_rows().await?;
                Ok("1".to_string())
            }
            Err(e) => Err(AppError::Database(e.to_string())),
        }
    }

    /// Sets the default cost multiplier.
    pub async fn set_default_cost_multiplier(
        &self,
        app_type: &str,
        value: &str,
    ) -> Result<(), AppError> {
        validate_cost_multiplier(value)?;
        let trimmed = value.trim();

        // Ensure the row exists.
        self.ensure_proxy_config_row_exists(app_type)?;

        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE proxy_config SET
                default_cost_multiplier = ?2,
                updated_at = datetime('now')
             WHERE app_type = ?1",
            rusqlite::params![app_type, trimmed],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(())
    }

    /// Returns the pricing-mode source.
    pub async fn get_pricing_model_source(&self, app_type: &str) -> Result<String, AppError> {
        let result = {
            let conn = lock_conn!(self.conn);
            conn.query_row(
                "SELECT pricing_model_source FROM proxy_config WHERE app_type = ?1",
                [app_type],
                |row| row.get(0),
            )
        };

        match result {
            Ok(value) => Ok(value),
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                self.init_proxy_config_rows().await?;
                Ok(PRICING_SOURCE_RESPONSE.to_string())
            }
            Err(e) => Err(AppError::Database(e.to_string())),
        }
    }

    /// Sets the pricing-mode source.
    pub async fn set_pricing_model_source(
        &self,
        app_type: &str,
        value: &str,
    ) -> Result<(), AppError> {
        let trimmed = validate_pricing_source(value)?;

        // Ensure the row exists.
        self.ensure_proxy_config_row_exists(app_type)?;

        let conn = lock_conn!(self.conn);
        conn.execute(
            "UPDATE proxy_config SET
                pricing_model_source = ?2,
                updated_at = datetime('now')
             WHERE app_type = ?1",
            rusqlite::params![app_type, trimmed],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(())
    }

    /// Save multiplier and pricing-model source updates in one SQLite
    /// transaction. Every value is validated before the first write so a bad
    /// row (or any database failure) cannot leave a partially saved catalog.
    pub async fn save_pricing_defaults_atomically(
        &self,
        updates: &[(String, String, String)],
    ) -> Result<(), AppError> {
        if updates.is_empty() {
            return Err(AppError::Config(
                "At least one pricing default update is required".to_string(),
            ));
        }

        let mut normalized = Vec::with_capacity(updates.len());
        let mut seen_apps = HashSet::with_capacity(updates.len());
        for (app_type, multiplier, source) in updates {
            let app_type = app_type.trim();
            if !matches!(app_type, "claude" | "codex" | "gemini") {
                return Err(AppError::Config(format!(
                    "Unsupported pricing app type: {app_type}"
                )));
            }
            if !seen_apps.insert(app_type.to_string()) {
                return Err(AppError::Config(format!(
                    "Duplicate pricing app type: {app_type}"
                )));
            }
            validate_cost_multiplier(multiplier)?;
            let source = validate_pricing_source(source)?;
            normalized.push((
                app_type.to_string(),
                multiplier.trim().to_string(),
                source.to_string(),
            ));
        }

        let mut conn = lock_conn!(self.conn);
        let tx = conn.transaction().map_err(|e| {
            AppError::Database(format!("Failed to begin pricing defaults update: {e}"))
        })?;

        for (app_type, multiplier, source) in normalized {
            let (retries, first_byte, idle, failures, successes, timeout, rate, minimum) =
                match app_type.as_str() {
                    "claude" => (6, 90, 180, 8, 3, 90, 0.7, 15),
                    "gemini" => (5, 60, 120, 4, 2, 60, 0.6, 10),
                    _ => (3, 60, 120, 4, 2, 60, 0.6, 10),
                };
            tx.execute(
                "INSERT OR IGNORE INTO proxy_config (
                    app_type, max_retries,
                    streaming_first_byte_timeout, streaming_idle_timeout, non_streaming_timeout,
                    circuit_failure_threshold, circuit_success_threshold, circuit_timeout_seconds,
                    circuit_error_rate_threshold, circuit_min_requests
                 ) VALUES (?1, ?2, ?3, ?4, 600, ?5, ?6, ?7, ?8, ?9)",
                rusqlite::params![
                    app_type, retries, first_byte, idle, failures, successes, timeout, rate,
                    minimum
                ],
            )
            .map_err(|e| AppError::Database(format!("Failed to ensure pricing row: {e}")))?;
            tx.execute(
                "UPDATE proxy_config SET
                    default_cost_multiplier = ?2,
                    pricing_model_source = ?3,
                    updated_at = datetime('now')
                 WHERE app_type = ?1",
                rusqlite::params![app_type, multiplier, source],
            )
            .map_err(|e| AppError::Database(format!("Failed to update pricing defaults: {e}")))?;
        }

        tx.commit().map_err(|e| {
            AppError::Database(format!("Failed to commit pricing defaults update: {e}"))
        })
    }

    /// Returns application-level proxy configuration.
    pub async fn get_proxy_config_for_app(
        &self,
        app_type: &str,
    ) -> Result<AppProxyConfig, AppError> {
        // Limit the connection scope so its lock is not held across await.
        let app_type_owned = app_type.to_string();
        let result = {
            let conn = lock_conn!(self.conn);
            conn.query_row(
                "SELECT app_type, enabled, auto_failover_enabled,
                        max_retries, streaming_first_byte_timeout, streaming_idle_timeout, non_streaming_timeout,
                        circuit_failure_threshold, circuit_success_threshold, circuit_timeout_seconds,
                        circuit_error_rate_threshold, circuit_min_requests
                 FROM proxy_config WHERE app_type = ?1",
                [app_type],
                |row| {
                    Ok(AppProxyConfig {
                        app_type: row.get(0)?,
                        enabled: row.get::<_, i32>(1)? != 0,
                        auto_failover_enabled: row.get::<_, i32>(2)? != 0,
                        max_retries: row.get::<_, i32>(3)? as u32,
                        streaming_first_byte_timeout: row.get::<_, i32>(4)? as u32,
                        streaming_idle_timeout: row.get::<_, i32>(5)? as u32,
                        non_streaming_timeout: row.get::<_, i32>(6)? as u32,
                        circuit_failure_threshold: row.get::<_, i32>(7)? as u32,
                        circuit_success_threshold: row.get::<_, i32>(8)? as u32,
                        circuit_timeout_seconds: row.get::<_, i32>(9)? as u32,
                        circuit_error_rate_threshold: row.get(10)?,
                        circuit_min_requests: row.get::<_, i32>(11)? as u32,
                    })
                },
            )
        };
        // The connection was released at the end of the block.

        match result {
            Ok(config) => Ok(config),
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                // Create default configuration when absent.
                self.init_proxy_config_rows().await?;
                Ok(AppProxyConfig {
                    app_type: app_type_owned,
                    enabled: false,
                    auto_failover_enabled: false,
                    max_retries: 3,
                    streaming_first_byte_timeout: 60,
                    streaming_idle_timeout: 120,
                    non_streaming_timeout: 600,
                    circuit_failure_threshold: 4,
                    circuit_success_threshold: 2,
                    circuit_timeout_seconds: 60,
                    circuit_error_rate_threshold: 0.6,
                    circuit_min_requests: 10,
                })
            }
            Err(e) => Err(AppError::Database(e.to_string())),
        }
    }

    /// Updates application-level proxy configuration.
    pub async fn update_proxy_config_for_app(
        &self,
        config: AppProxyConfig,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);

        conn.execute(
            "UPDATE proxy_config SET
                enabled = ?2,
                auto_failover_enabled = ?3,
                max_retries = ?4,
                streaming_first_byte_timeout = ?5,
                streaming_idle_timeout = ?6,
                non_streaming_timeout = ?7,
                circuit_failure_threshold = ?8,
                circuit_success_threshold = ?9,
                circuit_timeout_seconds = ?10,
                circuit_error_rate_threshold = ?11,
                circuit_min_requests = ?12,
                updated_at = datetime('now')
             WHERE app_type = ?1",
            rusqlite::params![
                config.app_type,
                if config.enabled { 1 } else { 0 },
                if config.auto_failover_enabled { 1 } else { 0 },
                config.max_retries as i32,
                config.streaming_first_byte_timeout as i32,
                config.streaming_idle_timeout as i32,
                config.non_streaming_timeout as i32,
                config.circuit_failure_threshold as i32,
                config.circuit_success_threshold as i32,
                config.circuit_timeout_seconds as i32,
                config.circuit_error_rate_threshold,
                config.circuit_min_requests as i32,
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(())
    }

    /// Synchronously ensures a proxy_config row exists for app_type, for set_* calls.
    ///
    /// Uses the same per-application defaults as schema.rs seeds.
    fn ensure_proxy_config_row_exists(&self, app_type: &str) -> Result<(), AppError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| AppError::Lock(e.to_string()))?;

        // Select per-application defaults matching schema.rs seeds.
        let (retries, fb_timeout, idle_timeout, cb_fail, cb_succ, cb_timeout, cb_rate, cb_min) =
            match app_type {
                "claude" => (6, 90, 180, 8, 3, 90, 0.7, 15),
                "codex" => (3, 60, 120, 4, 2, 60, 0.6, 10),
                "gemini" => (5, 60, 120, 4, 2, 60, 0.6, 10),
                _ => (3, 60, 120, 4, 2, 60, 0.6, 10), // Default values.
            };

        conn.execute(
            "INSERT OR IGNORE INTO proxy_config (
                app_type, max_retries,
                streaming_first_byte_timeout, streaming_idle_timeout, non_streaming_timeout,
                circuit_failure_threshold, circuit_success_threshold, circuit_timeout_seconds,
                circuit_error_rate_threshold, circuit_min_requests
            ) VALUES (?1, ?2, ?3, ?4, 600, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                app_type,
                retries,
                fb_timeout,
                idle_timeout,
                cb_fail,
                cb_succ,
                cb_timeout,
                cb_rate,
                cb_min
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(())
    }

    /// Initializes the three proxy_config rows.
    ///
    /// Uses the same per-application defaults as schema.rs seeds.
    async fn init_proxy_config_rows(&self) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);

        // Match schema.rs seeds. Claude uses more aggressive retries and timeouts.
        conn.execute(
            "INSERT OR IGNORE INTO proxy_config (
                app_type, max_retries,
                streaming_first_byte_timeout, streaming_idle_timeout, non_streaming_timeout,
                circuit_failure_threshold, circuit_success_threshold, circuit_timeout_seconds,
                circuit_error_rate_threshold, circuit_min_requests
            ) VALUES ('claude', 6, 90, 180, 600, 8, 3, 90, 0.7, 15)",
            [],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        // Codex uses standard defaults.
        conn.execute(
            "INSERT OR IGNORE INTO proxy_config (
                app_type, max_retries,
                streaming_first_byte_timeout, streaming_idle_timeout, non_streaming_timeout,
                circuit_failure_threshold, circuit_success_threshold, circuit_timeout_seconds,
                circuit_error_rate_threshold, circuit_min_requests
            ) VALUES ('codex', 3, 60, 120, 600, 4, 2, 60, 0.6, 10)",
            [],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        // Gemini uses slightly more retries.
        conn.execute(
            "INSERT OR IGNORE INTO proxy_config (
                app_type, max_retries,
                streaming_first_byte_timeout, streaming_idle_timeout, non_streaming_timeout,
                circuit_failure_threshold, circuit_success_threshold, circuit_timeout_seconds,
                circuit_error_rate_threshold, circuit_min_requests
            ) VALUES ('gemini', 5, 60, 120, 600, 4, 2, 60, 0.6, 10)",
            [],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(())
    }

    // Legacy proxy configuration retained for compatibility.

    /// Returns Claude-row proxy configuration through the legacy interface.
    pub async fn get_proxy_config(&self) -> Result<ProxyConfig, AppError> {
        // Limit the connection scope so its lock is not held across await.
        let result = {
            let conn = lock_conn!(self.conn);
            conn.query_row(
                "SELECT listen_address, listen_port, max_retries,
                        enable_logging,
                        streaming_first_byte_timeout, streaming_idle_timeout, non_streaming_timeout
                 FROM proxy_config WHERE app_type = 'claude'",
                [],
                |row| {
                    Ok(ProxyConfig {
                        listen_address: row.get(0)?,
                        listen_port: row.get::<_, i32>(1)? as u16,
                        max_retries: row.get::<_, i32>(2)? as u8,
                        request_timeout: 600, // Deprecated field; return its default.
                        enable_logging: row.get::<_, i32>(3)? != 0,
                        live_takeover_active: false, // Deprecated field.
                        streaming_first_byte_timeout: row.get::<_, i32>(4).unwrap_or(60) as u64,
                        streaming_idle_timeout: row.get::<_, i32>(5).unwrap_or(120) as u64,
                        non_streaming_timeout: row.get::<_, i32>(6).unwrap_or(600) as u64,
                    })
                },
            )
        };
        // The connection was released at the end of the block.

        match result {
            Ok(config) => Ok(config),
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                // Initialize default configuration when absent.
                self.init_proxy_config_rows().await?;
                Ok(ProxyConfig::default())
            }
            Err(e) => Err(AppError::Database(e.to_string())),
        }
    }

    /// Updates shared fields in all three rows through the legacy interface.
    pub async fn update_proxy_config(&self, config: ProxyConfig) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);

        // Update shared fields in all three rows.
        conn.execute(
            "UPDATE proxy_config SET
                listen_address = ?1,
                listen_port = ?2,
                max_retries = ?3,
                enable_logging = ?4,
                streaming_first_byte_timeout = ?5,
                streaming_idle_timeout = ?6,
                non_streaming_timeout = ?7,
                updated_at = datetime('now')",
            rusqlite::params![
                config.listen_address,
                config.listen_port as i32,
                config.max_retries as i32,
                if config.enable_logging { 1 } else { 0 },
                config.streaming_first_byte_timeout as i32,
                config.streaming_idle_timeout as i32,
                config.non_streaming_timeout as i32,
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(())
    }

    /// Sets live takeover for legacy callers by updating enabled.
    pub async fn set_live_takeover_active(&self, _active: bool) -> Result<(), AppError> {
        // The old field is replaced by enabled. Keep this no-op for compatibility.
        Ok(())
    }

    /// Checks whether live takeover is active.
    ///
    /// Checks whether any application has enabled=true.
    pub async fn is_live_takeover_active(&self) -> Result<bool, AppError> {
        let conn = lock_conn!(self.conn);
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM proxy_config WHERE enabled = 1",
                [],
                |row| row.get(0),
            )
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(count > 0)
    }

    // ==================== Provider Health ====================

    /// Returns provider health state.
    pub async fn get_provider_health(
        &self,
        provider_id: &str,
        app_type: &str,
    ) -> Result<ProviderHealth, AppError> {
        let result = {
            let conn = lock_conn!(self.conn);

            conn.query_row(
                "SELECT provider_id, app_type, is_healthy, consecutive_failures,
                        last_success_at, last_failure_at, last_error, updated_at
                 FROM provider_health
                 WHERE provider_id = ?1 AND app_type = ?2",
                rusqlite::params![provider_id, app_type],
                |row| {
                    Ok(ProviderHealth {
                        provider_id: row.get(0)?,
                        app_type: row.get(1)?,
                        is_healthy: row.get::<_, i64>(2)? != 0,
                        consecutive_failures: row.get::<_, i64>(3)? as u32,
                        last_success_at: row.get(4)?,
                        last_failure_at: row.get(5)?,
                        last_error: row.get(6)?,
                        updated_at: row.get(7)?,
                    })
                },
            )
        };

        match result {
            Ok(health) => Ok(health),
            // Missing state is healthy; reopening after a clear starts normally.
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(ProviderHealth {
                provider_id: provider_id.to_string(),
                app_type: app_type.to_string(),
                is_healthy: true,
                consecutive_failures: 0,
                last_success_at: None,
                last_failure_at: None,
                last_error: None,
                updated_at: chrono::Utc::now().to_rfc3339(),
            }),
            Err(e) => Err(AppError::Database(e.to_string())),
        }
    }

    /// Updates provider health state.
    ///
    /// Uses the default threshold of five. Prefer
    /// `update_provider_health_with_threshold` with the configured threshold.
    pub async fn update_provider_health(
        &self,
        provider_id: &str,
        app_type: &str,
        success: bool,
        error_msg: Option<String>,
    ) -> Result<(), AppError> {
        // Keep the default aligned with CircuitBreakerConfig::default().
        self.update_provider_health_with_threshold(provider_id, app_type, success, error_msg, 5)
            .await
    }

    /// Updates provider health using an explicit threshold.
    ///
    /// # Arguments
    /// * `failure_threshold` - consecutive failures before marking unhealthy.
    pub async fn update_provider_health_with_threshold(
        &self,
        provider_id: &str,
        app_type: &str,
        success: bool,
        error_msg: Option<String>,
        failure_threshold: u32,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);

        let now = chrono::Utc::now().to_rfc3339();

        // Read current state first.
        let current = conn.query_row(
            "SELECT consecutive_failures FROM provider_health
             WHERE provider_id = ?1 AND app_type = ?2",
            rusqlite::params![provider_id, app_type],
            |row| Ok(row.get::<_, i64>(0)? as u32),
        );

        let (is_healthy, consecutive_failures) = if success {
            // Success resets the failure count.
            (1, 0)
        } else {
            // Failure increments the failure count.
            let failures = current.unwrap_or(0) + 1;
            // Apply the supplied threshold instead of a hard-coded value.
            let healthy = if failures >= failure_threshold { 0 } else { 1 };
            (healthy, failures)
        };

        let (last_success_at, last_failure_at) = if success {
            (Some(now.clone()), None)
        } else {
            (None, Some(now.clone()))
        };

        // UPSERT
        conn.execute(
            "INSERT OR REPLACE INTO provider_health
             (provider_id, app_type, is_healthy, consecutive_failures,
              last_success_at, last_failure_at, last_error, updated_at)
             VALUES (?1, ?2, ?3, ?4,
                     COALESCE(?5, (SELECT last_success_at FROM provider_health
                                   WHERE provider_id = ?1 AND app_type = ?2)),
                     COALESCE(?6, (SELECT last_failure_at FROM provider_health
                                   WHERE provider_id = ?1 AND app_type = ?2)),
                     ?7, ?8)",
            rusqlite::params![
                provider_id,
                app_type,
                is_healthy,
                consecutive_failures as i64,
                last_success_at,
                last_failure_at,
                error_msg,
                &now,
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(())
    }

    /// Resets provider health state.
    pub async fn reset_provider_health(
        &self,
        provider_id: &str,
        app_type: &str,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);

        conn.execute(
            "DELETE FROM provider_health WHERE provider_id = ?1 AND app_type = ?2",
            rusqlite::params![provider_id, app_type],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        log::debug!("Reset health status for provider {provider_id} (app: {app_type})");

        Ok(())
    }

    /// Clears health state for one application when its proxy is disabled.
    pub async fn clear_provider_health_for_app(&self, app_type: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);

        conn.execute(
            "DELETE FROM provider_health WHERE app_type = ?1",
            [app_type],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        log::debug!("Cleared provider health records for app {app_type}");
        Ok(())
    }

    /// Clears all provider health state when the proxy stops.
    pub async fn clear_all_provider_health(&self) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);

        conn.execute("DELETE FROM provider_health", [])
            .map_err(|e| AppError::Database(e.to_string()))?;

        log::debug!("Cleared all provider health records");
        Ok(())
    }

    // ==================== Circuit Breaker Config (Legacy Compatibility) ====================

    /// Returns circuit-breaker settings from the Claude row for legacy callers.
    ///
    /// Circuit-breaker settings now live per application in proxy_config. Prefer
    /// get_proxy_config_for_app; this method remains for compatibility.
    pub async fn get_circuit_breaker_config(
        &self,
    ) -> Result<crate::proxy::circuit_breaker::CircuitBreakerConfig, AppError> {
        // Limit the connection scope so its lock is not held across await.
        let result = {
            let conn = lock_conn!(self.conn);
            conn.query_row(
                "SELECT circuit_failure_threshold, circuit_success_threshold, circuit_timeout_seconds,
                        circuit_error_rate_threshold, circuit_min_requests
                 FROM proxy_config WHERE app_type = 'claude'",
                [],
                |row| {
                    Ok(crate::proxy::circuit_breaker::CircuitBreakerConfig {
                        failure_threshold: row.get::<_, i32>(0)? as u32,
                        success_threshold: row.get::<_, i32>(1)? as u32,
                        timeout_seconds: row.get::<_, i64>(2)? as u64,
                        error_rate_threshold: row.get(3)?,
                        min_requests: row.get::<_, i32>(4)? as u32,
                    })
                },
            )
        };
        // The connection was released at the end of the block.

        match result {
            Ok(config) => Ok(config),
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                // Initialize default configuration when absent.
                self.init_proxy_config_rows().await?;
                Ok(crate::proxy::circuit_breaker::CircuitBreakerConfig::default())
            }
            Err(e) => Err(AppError::Database(e.to_string())),
        }
    }

    /// Updates circuit-breaker settings in all three rows for legacy callers.
    ///
    /// Circuit-breaker settings now live in proxy_config. Prefer
    /// update_proxy_config_for_app; this remains for compatibility.
    pub async fn update_circuit_breaker_config(
        &self,
        config: &crate::proxy::circuit_breaker::CircuitBreakerConfig,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);

        // Update circuit-breaker fields in all three rows.
        conn.execute(
            "UPDATE proxy_config SET
                circuit_failure_threshold = ?1,
                circuit_success_threshold = ?2,
                circuit_timeout_seconds = ?3,
                circuit_error_rate_threshold = ?4,
                circuit_min_requests = ?5,
                updated_at = datetime('now')",
            rusqlite::params![
                config.failure_threshold as i32,
                config.success_threshold as i32,
                config.timeout_seconds as i64,
                config.error_rate_threshold,
                config.min_requests as i32,
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(())
    }

    // ==================== Live Backup ====================

    /// Saves a live-configuration backup.
    pub async fn save_live_backup(
        &self,
        app_type: &str,
        config_json: &str,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);
        let now = chrono::Utc::now().to_rfc3339();

        conn.execute(
            "INSERT OR REPLACE INTO proxy_live_backup (app_type, original_config, backed_up_at)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![app_type, config_json, now],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        log::info!("Backed up {app_type} live configuration");
        Ok(())
    }

    /// Checks whether any live-configuration backup exists.
    pub async fn has_any_live_backup(&self) -> Result<bool, AppError> {
        let conn = lock_conn!(self.conn);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM proxy_live_backup", [], |row| {
                row.get(0)
            })
            .map_err(|e| AppError::Database(e.to_string()))?;
        Ok(count > 0)
    }

    /// Returns a live-configuration backup.
    pub async fn get_live_backup(&self, app_type: &str) -> Result<Option<LiveBackup>, AppError> {
        let conn = lock_conn!(self.conn);

        let result = conn.query_row(
            "SELECT app_type, original_config, backed_up_at FROM proxy_live_backup WHERE app_type = ?1",
            rusqlite::params![app_type],
            |row| {
                Ok(LiveBackup {
                    app_type: row.get(0)?,
                    original_config: row.get(1)?,
                    backed_up_at: row.get(2)?,
                })
            },
        );

        match result {
            Ok(backup) => Ok(Some(backup)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AppError::Database(e.to_string())),
        }
    }

    /// Deletes a live-configuration backup.
    pub async fn delete_live_backup(&self, app_type: &str) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);

        conn.execute(
            "DELETE FROM proxy_live_backup WHERE app_type = ?1",
            rusqlite::params![app_type],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        log::info!("Deleted {app_type} live-configuration backup");
        Ok(())
    }

    /// Deletes every live-configuration backup.
    pub async fn delete_all_live_backups(&self) -> Result<(), AppError> {
        let conn = lock_conn!(self.conn);

        conn.execute("DELETE FROM proxy_live_backup", [])
            .map_err(|e| AppError::Database(e.to_string()))?;

        log::info!("Deleted all live-configuration backups");
        Ok(())
    }

    // ==================== Sync Methods for Tray Menu ====================

    /// Synchronously returns an application's proxy and automatic-failover state.
    ///
    /// Intended for synchronous paths such as tray-menu construction. Returns
    /// `(enabled, auto_failover_enabled)`.
    pub fn get_proxy_flags_sync(&self, app_type: &str) -> (bool, bool) {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return (false, false),
        };

        conn.query_row(
            "SELECT enabled, auto_failover_enabled FROM proxy_config WHERE app_type = ?1",
            [app_type],
            |row| Ok((row.get::<_, i32>(0)? != 0, row.get::<_, i32>(1)? != 0)),
        )
        .unwrap_or((false, false))
    }

    /// Synchronously sets an application's proxy and automatic-failover state.
    ///
    /// Intended for synchronous paths such as tray-menu clicks.
    pub fn set_proxy_flags_sync(
        &self,
        app_type: &str,
        enabled: bool,
        auto_failover_enabled: bool,
    ) -> Result<(), AppError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| AppError::Database(format!("Mutex lock failed: {e}")))?;

        conn.execute(
            "UPDATE proxy_config SET enabled = ?2, auto_failover_enabled = ?3, updated_at = datetime('now') WHERE app_type = ?1",
            rusqlite::params![
                app_type,
                if enabled { 1 } else { 0 },
                if auto_failover_enabled { 1 } else { 0 },
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::database::Database;
    use crate::error::AppError;

    #[tokio::test]
    async fn test_default_cost_multiplier_round_trip() -> Result<(), AppError> {
        let db = Database::memory()?;

        let default = db.get_default_cost_multiplier("claude").await?;
        assert_eq!(default, "1");

        db.set_default_cost_multiplier("claude", "1.5").await?;
        let updated = db.get_default_cost_multiplier("claude").await?;
        assert_eq!(updated, "1.5");

        Ok(())
    }

    #[tokio::test]
    async fn test_default_cost_multiplier_validation() -> Result<(), AppError> {
        let db = Database::memory()?;

        let err = db
            .set_default_cost_multiplier("claude", "not-a-number")
            .await
            .unwrap_err();
        // AppError::localized returns AppError::Localized variant
        assert!(matches!(
            err,
            AppError::Localized {
                key: "error.invalidMultiplier",
                ..
            }
        ));

        Ok(())
    }

    #[tokio::test]
    async fn test_pricing_model_source_round_trip_and_validation() -> Result<(), AppError> {
        let db = Database::memory()?;

        let default = db.get_pricing_model_source("claude").await?;
        assert_eq!(default, "response");

        db.set_pricing_model_source("claude", "request").await?;
        let updated = db.get_pricing_model_source("claude").await?;
        assert_eq!(updated, "request");

        let err = db
            .set_pricing_model_source("claude", "invalid")
            .await
            .unwrap_err();
        // AppError::localized returns AppError::Localized variant
        assert!(matches!(
            err,
            AppError::Localized {
                key: "error.invalidPricingMode",
                ..
            }
        ));

        let err = db
            .set_default_cost_multiplier("claude", "-0.5")
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            AppError::Localized {
                key: "error.invalidMultiplier",
                ..
            }
        ));

        Ok(())
    }

    #[tokio::test]
    async fn test_pricing_defaults_are_saved_together() -> Result<(), AppError> {
        let db = Database::memory()?;
        let updates = vec![
            (
                "claude".to_string(),
                "1.25".to_string(),
                "request".to_string(),
            ),
            (
                "codex".to_string(),
                "0.8".to_string(),
                "response".to_string(),
            ),
            ("gemini".to_string(), "2".to_string(), "request".to_string()),
        ];

        db.save_pricing_defaults_atomically(&updates).await?;

        assert_eq!(db.get_default_cost_multiplier("claude").await?, "1.25");
        assert_eq!(db.get_pricing_model_source("claude").await?, "request");
        assert_eq!(db.get_default_cost_multiplier("codex").await?, "0.8");
        assert_eq!(db.get_pricing_model_source("codex").await?, "response");
        assert_eq!(db.get_default_cost_multiplier("gemini").await?, "2");
        assert_eq!(db.get_pricing_model_source("gemini").await?, "request");

        Ok(())
    }

    #[tokio::test]
    async fn test_pricing_defaults_roll_back_on_database_failure() -> Result<(), AppError> {
        let db = Database::memory()?;
        {
            let conn = db.conn.lock().expect("lock conn");
            conn.execute_batch(
                "CREATE TRIGGER fail_codex_pricing_update
                 BEFORE UPDATE OF default_cost_multiplier, pricing_model_source ON proxy_config
                 WHEN NEW.app_type = 'codex'
                 BEGIN
                     SELECT RAISE(ABORT, 'forced pricing update failure');
                 END;",
            )
            .expect("create failure trigger");
        }
        let updates = vec![
            (
                "claude".to_string(),
                "1.25".to_string(),
                "request".to_string(),
            ),
            (
                "codex".to_string(),
                "0.8".to_string(),
                "request".to_string(),
            ),
        ];

        let error = db
            .save_pricing_defaults_atomically(&updates)
            .await
            .expect_err("second row must fail");
        assert!(error.to_string().contains("forced pricing update failure"));
        assert_eq!(db.get_default_cost_multiplier("claude").await?, "1");
        assert_eq!(db.get_pricing_model_source("claude").await?, "response");
        assert_eq!(db.get_default_cost_multiplier("codex").await?, "1");
        assert_eq!(db.get_pricing_model_source("codex").await?, "response");

        Ok(())
    }
}
