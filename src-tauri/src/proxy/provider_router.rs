//! Provider router module
//!
//! Selects and manages proxy target providers with smart failover.

use crate::app_config::AppType;
use crate::database::Database;
use crate::error::AppError;
use crate::provider::Provider;
use crate::proxy::circuit_breaker::{AllowResult, CircuitBreaker, CircuitBreakerConfig};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Provider router
pub struct ProviderRouter {
    /// Database handle
    db: Arc<Database>,
    /// Circuit breaker registry. Key format: "app_type:provider_id"
    circuit_breakers: Arc<RwLock<HashMap<String, Arc<CircuitBreaker>>>>,
}

impl ProviderRouter {
    /// Creates a new provider router.
    pub fn new(db: Arc<Database>) -> Self {
        Self {
            db,
            circuit_breakers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Selects available providers with failover support.
    ///
    /// Returns the available providers in priority order:
    /// - When failover is disabled: return only the current provider
    /// - When failover is enabled: use only the failover queue in queue order
    pub async fn select_providers(&self, app_type: &str) -> Result<Vec<Provider>, AppError> {
        let mut result = Vec::new();
        let mut total_providers = 0usize;
        let mut circuit_open_count = 0usize;

        // Check whether automatic failover is enabled for this app from proxy_config.
        let (auto_failover_enabled, takeover_enabled) = match self
            .db
            .get_proxy_config_for_app(app_type)
            .await
        {
            Ok(config) => (config.auto_failover_enabled, config.enabled),
            Err(e) => {
                log::error!(
                        "[{app_type}] failed to read proxy_config: {e}; defaulting failover to disabled"
                    );
                (false, false)
            }
        };

        if auto_failover_enabled {
            // Failover enabled: try only the queue order.
            let all_providers = self.db.get_all_providers(app_type)?;

            // Use the DAO order so backend selection matches the UI.
            let ordered_ids: Vec<String> = self
                .db
                .get_failover_queue(app_type)?
                .into_iter()
                .map(|item| item.provider_id)
                .collect();

            total_providers = ordered_ids.len();

            for provider_id in ordered_ids {
                let Some(provider) = all_providers.get(&provider_id).cloned() else {
                    continue;
                };

                let circuit_key = format!("{app_type}:{}", provider.id);
                let breaker = self.get_or_create_circuit_breaker(&circuit_key).await;

                if breaker.is_available().await {
                    result.push(provider);
                } else {
                    circuit_open_count += 1;
                }
            }
        } else {
            // Failover disabled: use only the current provider and skip breaker checks.
            let app_enum = AppType::from_str(app_type).ok();
            let current_id = if takeover_enabled {
                // During takeover the database is the proxy's routing source of
                // truth. A failed/interrupted switch can leave the device-local
                // setting stale; preferring it here can route Official through
                // the proxy even though Official intentionally has no base_url.
                let database_current = self.db.get_current_provider(app_type)?;
                if let (Some(app_enum), Some(database_id)) =
                    (app_enum.as_ref(), database_current.as_deref())
                {
                    if crate::settings::get_current_provider(app_enum).as_deref()
                        != Some(database_id)
                    {
                        log::warn!(
                            "[{app_type}] takeover current-provider mismatch; repairing device setting to database target {database_id}"
                        );
                        if let Err(error) =
                            crate::settings::set_current_provider(app_enum, Some(database_id))
                        {
                            log::error!(
                                "[{app_type}] failed to repair device current provider: {error}"
                            );
                        }
                    }
                }
                database_current
            } else {
                app_enum
                    .as_ref()
                    .and_then(|app_enum| {
                        crate::settings::get_effective_current_provider(&self.db, app_enum)
                            .ok()
                            .flatten()
                    })
                    .or_else(|| self.db.get_current_provider(app_type).ok().flatten())
            };

            if let Some(current_id) = current_id {
                if let Some(current) = self.db.get_provider_by_id(&current_id, app_type)? {
                    if takeover_enabled && current.category.as_deref() == Some("official") {
                        return Err(AppError::Message(format!(
                            "Invalid {app_type} takeover target: Official providers cannot be routed through the local proxy"
                        )));
                    }
                    total_providers = 1;
                    result.push(current);
                }
            }
        }

        if result.is_empty() {
            if total_providers > 0 && circuit_open_count == total_providers {
                log::warn!("[{app_type}] [FO-004] all providers are circuit-open");
                return Err(AppError::AllProvidersCircuitOpen);
            } else {
                log::warn!("[{app_type}] [FO-005] no providers configured");
                return Err(AppError::NoProvidersConfigured);
            }
        }

        Ok(result)
    }

    /// Acquires a circuit breaker permit before a request runs.
    ///
    /// - Closed: allow immediately
    /// - Open: after timeout, move to HalfOpen and allow one probe
    /// - HalfOpen: allow probes under the rate limit
    ///
    /// Callers must release the HalfOpen slot through `record_result()` after
    /// the request finishes, or the provider can remain stuck out of probing.
    pub async fn allow_provider_request(&self, provider_id: &str, app_type: &str) -> AllowResult {
        let circuit_key = format!("{app_type}:{provider_id}");
        let breaker = self.get_or_create_circuit_breaker(&circuit_key).await;
        breaker.allow_request().await
    }

    /// Records a provider request result.
    pub async fn record_result(
        &self,
        provider_id: &str,
        app_type: &str,
        used_half_open_permit: bool,
        success: bool,
        error_msg: Option<String>,
    ) -> Result<(), AppError> {
        // 1. Load the per-app circuit breaker config.
        let failure_threshold = match self.db.get_proxy_config_for_app(app_type).await {
            Ok(app_config) => app_config.circuit_failure_threshold,
            Err(_) => 5, // Default value
        };

        // 2. Update the circuit breaker state.
        let circuit_key = format!("{app_type}:{provider_id}");
        let breaker = self.get_or_create_circuit_breaker(&circuit_key).await;

        if success {
            breaker.record_success(used_half_open_permit).await;
        } else {
            breaker.record_failure(used_half_open_permit).await;
        }

        // 3. Update database health using the configured threshold.
        self.db
            .update_provider_health_with_threshold(
                provider_id,
                app_type,
                success,
                error_msg.clone(),
                failure_threshold,
            )
            .await?;

        Ok(())
    }

    /// Resets a circuit breaker manually.
    pub async fn reset_circuit_breaker(&self, circuit_key: &str) {
        let breakers = self.circuit_breakers.read().await;
        if let Some(breaker) = breakers.get(circuit_key) {
            breaker.reset().await;
        }
    }

    /// Resets the circuit breaker for a specific provider.
    pub async fn reset_provider_breaker(&self, provider_id: &str, app_type: &str) {
        let circuit_key = format!("{app_type}:{provider_id}");
        self.reset_circuit_breaker(&circuit_key).await;
    }

    /// Releases only the HalfOpen permit without affecting health stats.
    ///
    /// Used for neutral flows such as rectifiers where the request outcome
    /// should not affect provider health but must still release the probe slot.
    pub async fn release_permit_neutral(
        &self,
        provider_id: &str,
        app_type: &str,
        used_half_open_permit: bool,
    ) {
        if !used_half_open_permit {
            return;
        }
        let circuit_key = format!("{app_type}:{provider_id}");
        let breaker = self.get_or_create_circuit_breaker(&circuit_key).await;
        breaker.release_half_open_permit();
    }

    /// Updates all circuit breaker configs in place.
    pub async fn update_all_configs(&self, config: CircuitBreakerConfig) {
        let breakers = self.circuit_breakers.read().await;
        for breaker in breakers.values() {
            breaker.update_config(config.clone()).await;
        }
    }

    /// Updates configs for already-created breakers under one app.
    pub async fn update_app_configs(&self, app_type: &str, config: CircuitBreakerConfig) {
        let prefix = format!("{app_type}:");
        let breakers = self.circuit_breakers.read().await;
        for (key, breaker) in breakers.iter() {
            if key.starts_with(&prefix) {
                breaker.update_config(config.clone()).await;
            }
        }
    }

    /// Gets circuit breaker stats.
    #[allow(dead_code)]
    pub async fn get_circuit_breaker_stats(
        &self,
        provider_id: &str,
        app_type: &str,
    ) -> Option<crate::proxy::circuit_breaker::CircuitBreakerStats> {
        let circuit_key = format!("{app_type}:{provider_id}");
        let breakers = self.circuit_breakers.read().await;

        if let Some(breaker) = breakers.get(&circuit_key) {
            Some(breaker.get_stats().await)
        } else {
            None
        }
    }

    /// Gets or creates a circuit breaker.
    async fn get_or_create_circuit_breaker(&self, key: &str) -> Arc<CircuitBreaker> {
        // Try the read lock first.
        {
            let breakers = self.circuit_breakers.read().await;
            if let Some(breaker) = breakers.get(key) {
                return breaker.clone();
            }
        }

        // Create it under a write lock if missing.
        let mut breakers = self.circuit_breakers.write().await;

        // Double-check to avoid a race.
        if let Some(breaker) = breakers.get(key) {
            return breaker.clone();
        }

        // Extract app_type from the key: "app_type:provider_id".
        let app_type = key.split(':').next().unwrap_or("claude");

        // Read the breaker config per app.
        let config = match self.db.get_proxy_config_for_app(app_type).await {
            Ok(app_config) => crate::proxy::circuit_breaker::CircuitBreakerConfig {
                failure_threshold: app_config.circuit_failure_threshold,
                success_threshold: app_config.circuit_success_threshold,
                timeout_seconds: app_config.circuit_timeout_seconds as u64,
                error_rate_threshold: app_config.circuit_error_rate_threshold,
                min_requests: app_config.circuit_min_requests,
            },
            Err(_) => crate::proxy::circuit_breaker::CircuitBreakerConfig::default(),
        };

        let breaker = Arc::new(CircuitBreaker::new(config));
        breakers.insert(key.to_string(), breaker.clone());

        breaker
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;
    use serde_json::json;
    use serial_test::serial;
    use std::env;
    use tempfile::TempDir;

    struct TempHome {
        #[allow(dead_code)]
        dir: TempDir,
        original_home: Option<String>,
        original_userprofile: Option<String>,
        original_test_home: Option<String>,
    }

    impl TempHome {
        fn new() -> Self {
            let dir = TempDir::new().expect("failed to create temp home");
            let original_home = env::var("HOME").ok();
            let original_userprofile = env::var("USERPROFILE").ok();
            let original_test_home = env::var("CC_SWITCH_TEST_HOME").ok();

            env::set_var("HOME", dir.path());
            env::set_var("USERPROFILE", dir.path());
            env::set_var("CC_SWITCH_TEST_HOME", dir.path());
            crate::settings::reload_settings().expect("reload settings");

            Self {
                dir,
                original_home,
                original_userprofile,
                original_test_home,
            }
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            match &self.original_home {
                Some(value) => env::set_var("HOME", value),
                None => env::remove_var("HOME"),
            }

            match &self.original_userprofile {
                Some(value) => env::set_var("USERPROFILE", value),
                None => env::remove_var("USERPROFILE"),
            }

            match &self.original_test_home {
                Some(value) => env::set_var("CC_SWITCH_TEST_HOME", value),
                None => env::remove_var("CC_SWITCH_TEST_HOME"),
            }
        }
    }

    #[tokio::test]
    #[serial]
    async fn test_provider_router_creation() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());
        let router = ProviderRouter::new(db);

        let breaker = router.get_or_create_circuit_breaker("claude:test").await;
        assert!(breaker.allow_request().await.allowed);
    }

    #[tokio::test]
    #[serial]
    async fn test_failover_disabled_uses_current_provider() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        let provider_a =
            Provider::with_id("a".to_string(), "Provider A".to_string(), json!({}), None);
        let provider_b =
            Provider::with_id("b".to_string(), "Provider B".to_string(), json!({}), None);

        db.save_provider("claude", &provider_a).unwrap();
        db.save_provider("claude", &provider_b).unwrap();
        db.set_current_provider("claude", "a").unwrap();
        db.add_to_failover_queue("claude", "b").unwrap();

        let router = ProviderRouter::new(db.clone());
        let providers = router.select_providers("claude").await.unwrap();

        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].id, "a");
    }

    #[tokio::test]
    #[serial]
    async fn takeover_repairs_stale_local_provider_and_routes_database_target() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        let mut official = Provider::with_id(
            "codex-official".to_string(),
            "OpenAI Official".to_string(),
            json!({"config": "model = \"gpt-5.5\""}),
            None,
        );
        official.category = Some("official".to_string());
        let mut glm = Provider::with_id(
            "nexus-glm".to_string(),
            "Nexus GLM-5.2".to_string(),
            json!({"base_url": "https://glm.example/v1"}),
            None,
        );
        glm.category = Some("third_party".to_string());

        db.save_provider("codex", &official).unwrap();
        db.save_provider("codex", &glm).unwrap();
        db.set_current_provider("codex", &glm.id).unwrap();
        crate::settings::set_current_provider(&AppType::Codex, Some(&official.id)).unwrap();

        let mut config = db.get_proxy_config_for_app("codex").await.unwrap();
        config.enabled = true;
        db.update_proxy_config_for_app(config).await.unwrap();

        let router = ProviderRouter::new(db);
        let providers = router.select_providers("codex").await.unwrap();

        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].id, glm.id);
        assert_eq!(
            crate::settings::get_current_provider(&AppType::Codex),
            Some(glm.id)
        );
    }

    #[tokio::test]
    #[serial]
    async fn test_failover_enabled_uses_queue_order_ignoring_current() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        // Set sort_index to control order: b=1, a=2.
        let mut provider_a =
            Provider::with_id("a".to_string(), "Provider A".to_string(), json!({}), None);
        provider_a.sort_index = Some(2);
        let mut provider_b =
            Provider::with_id("b".to_string(), "Provider B".to_string(), json!({}), None);
        provider_b.sort_index = Some(1);

        db.save_provider("claude", &provider_a).unwrap();
        db.save_provider("claude", &provider_b).unwrap();
        db.set_current_provider("claude", "a").unwrap();

        db.add_to_failover_queue("claude", "b").unwrap();
        db.add_to_failover_queue("claude", "a").unwrap();

        // Enable automatic failover with the newer proxy_config API.
        let mut config = db.get_proxy_config_for_app("claude").await.unwrap();
        config.auto_failover_enabled = true;
        db.update_proxy_config_for_app(config).await.unwrap();

        let router = ProviderRouter::new(db.clone());
        let providers = router.select_providers("claude").await.unwrap();

        assert_eq!(providers.len(), 2);
        // With failover enabled, select only by queue order and ignore current.
        assert_eq!(providers[0].id, "b");
        assert_eq!(providers[1].id, "a");
    }

    #[tokio::test]
    #[serial]
    async fn test_failover_enabled_uses_queue_only_even_if_current_not_in_queue() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        let provider_a =
            Provider::with_id("a".to_string(), "Provider A".to_string(), json!({}), None);
        let mut provider_b =
            Provider::with_id("b".to_string(), "Provider B".to_string(), json!({}), None);
        provider_b.sort_index = Some(1);

        db.save_provider("claude", &provider_a).unwrap();
        db.save_provider("claude", &provider_b).unwrap();
        db.set_current_provider("claude", "a").unwrap();

        // Add only b to the failover queue to model a common config where the
        // current provider is not in the queue.
        db.add_to_failover_queue("claude", "b").unwrap();

        let mut config = db.get_proxy_config_for_app("claude").await.unwrap();
        config.auto_failover_enabled = true;
        db.update_proxy_config_for_app(config).await.unwrap();

        let router = ProviderRouter::new(db.clone());
        let providers = router.select_providers("claude").await.unwrap();

        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].id, "b");
    }

    #[tokio::test]
    #[serial]
    async fn test_select_providers_does_not_consume_half_open_permit() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        db.update_circuit_breaker_config(&CircuitBreakerConfig {
            failure_threshold: 1,
            timeout_seconds: 0,
            ..Default::default()
        })
        .await
        .unwrap();

        let provider_a =
            Provider::with_id("a".to_string(), "Provider A".to_string(), json!({}), None);
        let provider_b =
            Provider::with_id("b".to_string(), "Provider B".to_string(), json!({}), None);

        db.save_provider("claude", &provider_a).unwrap();
        db.save_provider("claude", &provider_b).unwrap();

        db.add_to_failover_queue("claude", "a").unwrap();
        db.add_to_failover_queue("claude", "b").unwrap();

        // Enable automatic failover with the newer proxy_config API.
        let mut config = db.get_proxy_config_for_app("claude").await.unwrap();
        config.auto_failover_enabled = true;
        db.update_proxy_config_for_app(config).await.unwrap();

        let router = ProviderRouter::new(db.clone());

        router
            .record_result("b", "claude", false, false, Some("fail".to_string()))
            .await
            .unwrap();

        let providers = router.select_providers("claude").await.unwrap();
        assert_eq!(providers.len(), 2);

        assert!(router.allow_provider_request("b", "claude").await.allowed);
    }

    #[tokio::test]
    #[serial]
    async fn test_release_permit_neutral_frees_half_open_slot() {
        let _home = TempHome::new();
        let db = Arc::new(Database::memory().unwrap());

        // Configure the breaker: trip after 1 failure and immediately enter
        // HalfOpen with a 0-second timeout.
        db.update_circuit_breaker_config(&CircuitBreakerConfig {
            failure_threshold: 1,
            timeout_seconds: 0,
            ..Default::default()
        })
        .await
        .unwrap();

        let provider_a =
            Provider::with_id("a".to_string(), "Provider A".to_string(), json!({}), None);
        db.save_provider("claude", &provider_a).unwrap();
        db.add_to_failover_queue("claude", "a").unwrap();

        // Enable automatic failover.
        let mut config = db.get_proxy_config_for_app("claude").await.unwrap();
        config.auto_failover_enabled = true;
        db.update_proxy_config_for_app(config).await.unwrap();

        let router = ProviderRouter::new(db.clone());

        // Trip the breaker with one failure.
        router
            .record_result("a", "claude", false, false, Some("fail".to_string()))
            .await
            .unwrap();

        // First request: acquire the HalfOpen probe slot.
        let first = router.allow_provider_request("a", "claude").await;
        assert!(first.allowed);
        assert!(first.used_half_open_permit);

        // The second request should be denied because the slot is occupied.
        let second = router.allow_provider_request("a", "claude").await;
        assert!(!second.allowed);

        // Release the slot through release_permit_neutral without affecting health.
        router
            .release_permit_neutral("a", "claude", first.used_half_open_permit)
            .await;

        // The third request should be allowed after the slot is released.
        let third = router.allow_provider_request("a", "claude").await;
        assert!(third.allowed);
        assert!(third.used_half_open_permit);
    }
}
