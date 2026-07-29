//! Proxy service business logic layer.
//!
//! Provides proxy server startup, shutdown, and configuration management.

use crate::app_config::AppType;
use crate::config::{get_claude_settings_path, read_json_file, write_json_file};
use crate::database::Database;
use crate::error::AppError;
use crate::provider::{ClaudeDesktopMode, Provider};
use crate::proxy::server::ProxyServer;
use crate::proxy::switch_lock::SwitchLockManager;
use crate::proxy::types::*;
use crate::services::provider::{
    build_effective_settings_with_common_config, sync_codex_desktop_table,
    write_live_with_common_config,
};
use crate::store::AppState;
use serde_json::{json, Map, Value};
use std::str::FromStr;
use std::sync::Arc;
use tauri::Emitter;
use tokio::sync::RwLock;

/// Placeholder used during Live takeover to avoid missing-key warnings without exposing real tokens.
const PROXY_TOKEN_PLACEHOLDER: &str = crate::codex_config::CODEX_PROXY_TOKEN_PLACEHOLDER;

/// Model override fields that must be removed from Claude Live config during proxy takeover.
///
/// Reason: during takeover, `*_MODEL` must be written as stable Claude role aliases
/// by Nexus Composer and then mapped by the local proxy to the current provider's
/// real model. `*_MODEL_NAME` must also be taken over, or Claude Code can keep the
/// previous provider's display name in its model menu.
const CLAUDE_MODEL_OVERRIDE_ENV_KEYS: [&str; 9] = [
    "ANTHROPIC_MODEL",
    "ANTHROPIC_REASONING_MODEL", // legacy: deprecated, but old configs may still contain it
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME",
    // Legacy key (deprecated): older versions used this field for the small/fast model
    "ANTHROPIC_SMALL_FAST_MODEL",
];

const CLAUDE_TAKEOVER_HAIKU_MODEL: &str = "claude-haiku-4-5";
const CLAUDE_TAKEOVER_SONNET_MODEL: &str = "claude-sonnet-4-6";
const CLAUDE_TAKEOVER_OPUS_MODEL: &str = "claude-opus-4-8";
// Keep the uppercase form used in Claude Code docs examples; parsing is case-insensitive.
const CLAUDE_ONE_M_MARKER_FOR_CLIENT: &str = "[1M]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaudeTakeoverAuthPolicy {
    PreserveExistingOrAuthToken,
    ManagedAccount { keep_auth_token: bool },
}

#[derive(Clone)]
pub struct ProxyService {
    db: Arc<Database>,
    server: Arc<RwLock<Option<ProxyServer>>>,
    /// AppHandle passed into ProxyServer so failover can update the UI.
    app_handle: Arc<RwLock<Option<tauri::AppHandle>>>,
    switch_locks: SwitchLockManager,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct HotSwitchOutcome {
    pub logical_target_changed: bool,
}

fn current_claude_desktop_proxy_provider(db: &Database) -> Result<Option<Provider>, AppError> {
    let app = AppType::ClaudeDesktop;
    let Some(current_id) = crate::settings::get_effective_current_provider(db, &app)? else {
        return Ok(None);
    };
    let Some(provider) = db.get_provider_by_id(&current_id, app.as_str())? else {
        return Ok(None);
    };
    if !matches!(
        crate::claude_desktop_config::provider_mode(&provider),
        ClaudeDesktopMode::Proxy
    ) {
        return Ok(None);
    }

    Ok(Some(provider))
}

fn current_claude_desktop_uses_proxy(db: &Database) -> Result<bool, AppError> {
    Ok(current_claude_desktop_proxy_provider(db)?.is_some())
}

pub(crate) fn sync_current_claude_desktop_proxy_profile(state: &AppState) -> Result<(), AppError> {
    let Some(provider) = current_claude_desktop_proxy_provider(state.db.as_ref())? else {
        return Ok(());
    };

    write_live_with_common_config(state.db.as_ref(), &AppType::ClaudeDesktop, &provider)
}

impl ProxyService {
    pub fn new(db: Arc<Database>) -> Self {
        Self {
            db,
            server: Arc::new(RwLock::new(None)),
            app_handle: Arc::new(RwLock::new(None)),
            switch_locks: SwitchLockManager::new(),
        }
    }

    #[cfg(test)]
    fn apply_claude_takeover_fields(config: &mut Value, proxy_url: &str) {
        Self::apply_claude_takeover_fields_with_policy(
            config,
            proxy_url,
            ClaudeTakeoverAuthPolicy::PreserveExistingOrAuthToken,
        );
    }

    fn apply_claude_takeover_fields_for_provider(
        config: &mut Value,
        proxy_url: &str,
        provider: &Provider,
    ) {
        let auth_policy = if provider.uses_managed_account_auth() {
            // Codex-family providers, including ones detected only by base_url
            // without provider_type metadata, must keep the ANTHROPIC_AUTH_TOKEN
            // placeholder: Claude Code prompts for login if the key is missing (#3784).
            // Copilot keeps only the API_KEY placeholder to avoid conflicts with
            // keys managed by /login (#1049).
            ClaudeTakeoverAuthPolicy::ManagedAccount {
                keep_auth_token: !provider.is_github_copilot(),
            }
        } else {
            ClaudeTakeoverAuthPolicy::PreserveExistingOrAuthToken
        };
        // During Copilot/Codex takeover the live config may still belong to the
        // previous provider; the visible model must follow the target provider.
        let takeover_model_fields = if provider.uses_managed_account_auth() {
            Self::build_claude_takeover_model_fields(&provider.settings_config)
        } else {
            Self::build_claude_takeover_model_fields(config)
        };

        Self::apply_claude_takeover_fields_with_policy_and_models(
            config,
            proxy_url,
            auth_policy,
            takeover_model_fields,
        );
    }

    fn apply_claude_takeover_fields_with_policy(
        config: &mut Value,
        proxy_url: &str,
        auth_policy: ClaudeTakeoverAuthPolicy,
    ) {
        // Snapshot before remove/insert so we do not read the takeover aliases we just wrote.
        let takeover_model_fields = Self::build_claude_takeover_model_fields(config);

        Self::apply_claude_takeover_fields_with_policy_and_models(
            config,
            proxy_url,
            auth_policy,
            takeover_model_fields,
        );
    }

    fn apply_claude_takeover_fields_with_policy_and_models(
        config: &mut Value,
        proxy_url: &str,
        auth_policy: ClaudeTakeoverAuthPolicy,
        takeover_model_fields: Vec<(&'static str, String)>,
    ) {
        if !config.is_object() {
            *config = json!({});
        }

        let root = config
            .as_object_mut()
            .expect("Claude config should be normalized to an object");
        let env = root.entry("env".to_string()).or_insert_with(|| json!({}));
        if !env.is_object() {
            *env = json!({});
        }

        let env = env
            .as_object_mut()
            .expect("Claude env should be normalized to an object");
        env.insert("ANTHROPIC_BASE_URL".to_string(), json!(proxy_url));

        for key in CLAUDE_MODEL_OVERRIDE_ENV_KEYS {
            env.remove(key);
        }

        for (key, value) in takeover_model_fields {
            env.insert(key.to_string(), Value::String(value));
        }

        let token_keys = [
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "OPENROUTER_API_KEY",
            "OPENAI_API_KEY",
        ];

        match auth_policy {
            ClaudeTakeoverAuthPolicy::PreserveExistingOrAuthToken => {
                let mut replaced_any = false;
                for key in token_keys {
                    if env.contains_key(key) {
                        env.insert(key.to_string(), json!(PROXY_TOKEN_PLACEHOLDER));
                        replaced_any = true;
                    }
                }

                if !replaced_any {
                    env.insert(
                        "ANTHROPIC_AUTH_TOKEN".to_string(),
                        json!(PROXY_TOKEN_PLACEHOLDER),
                    );
                }
            }
            ClaudeTakeoverAuthPolicy::ManagedAccount { keep_auth_token } => {
                for key in token_keys {
                    env.remove(key);
                }
                env.insert(
                    "ANTHROPIC_API_KEY".to_string(),
                    json!(PROXY_TOKEN_PLACEHOLDER),
                );
                if keep_auth_token {
                    // Inject unconditionally instead of "preserve only if present":
                    // the hot-switch path passes provider settings, which usually
                    // omit this key, and older takeover logic already removed it
                    // from existing user live configs.
                    env.insert(
                        "ANTHROPIC_AUTH_TOKEN".to_string(),
                        json!(PROXY_TOKEN_PLACEHOLDER),
                    );
                }
            }
        }
    }

    fn build_claude_takeover_model_fields(config: &Value) -> Vec<(&'static str, String)> {
        let Some(env) = config.get("env").and_then(Value::as_object) else {
            return Vec::new();
        };

        let default_model = Self::claude_env_string(env, "ANTHROPIC_MODEL");
        let small_fast_model = Self::claude_env_string(env, "ANTHROPIC_SMALL_FAST_MODEL");
        let haiku_model = Self::claude_env_string(env, "ANTHROPIC_DEFAULT_HAIKU_MODEL")
            .or(small_fast_model)
            .or(default_model);
        let sonnet_model = Self::claude_env_string(env, "ANTHROPIC_DEFAULT_SONNET_MODEL")
            .or(default_model)
            .or(small_fast_model);
        let opus_model = Self::claude_env_string(env, "ANTHROPIC_DEFAULT_OPUS_MODEL")
            .or(default_model)
            .or(small_fast_model);

        let mut fields = Vec::with_capacity(6);
        Self::push_claude_takeover_role_fields(
            &mut fields,
            env,
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME",
            CLAUDE_TAKEOVER_HAIKU_MODEL,
            false,
            haiku_model,
        );
        Self::push_claude_takeover_role_fields(
            &mut fields,
            env,
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
            "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME",
            CLAUDE_TAKEOVER_SONNET_MODEL,
            true,
            sonnet_model,
        );
        Self::push_claude_takeover_role_fields(
            &mut fields,
            env,
            "ANTHROPIC_DEFAULT_OPUS_MODEL",
            "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME",
            CLAUDE_TAKEOVER_OPUS_MODEL,
            true,
            opus_model,
        );
        fields
    }

    fn push_claude_takeover_role_fields(
        fields: &mut Vec<(&'static str, String)>,
        env: &Map<String, Value>,
        model_key: &'static str,
        name_key: &'static str,
        takeover_model: &'static str,
        supports_one_m: bool,
        upstream_model: Option<&str>,
    ) {
        let Some(upstream_model) = upstream_model else {
            return;
        };

        let mut client_model = takeover_model.to_string();
        if supports_one_m && Self::has_claude_one_m_marker(upstream_model) {
            client_model.push_str(CLAUDE_ONE_M_MARKER_FOR_CLIENT);
        }
        fields.push((model_key, client_model));

        let display_name = Self::claude_env_string(env, name_key)
            .map(str::to_string)
            .unwrap_or_else(|| Self::strip_claude_one_m_marker(upstream_model));
        if !display_name.is_empty() {
            fields.push((name_key, display_name));
        }
    }

    fn claude_env_string<'a>(env: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
        env.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    fn has_claude_one_m_marker(model: &str) -> bool {
        model
            .trim_end()
            .to_ascii_lowercase()
            .ends_with(crate::claude_desktop_config::ONE_M_CONTEXT_MARKER)
    }

    fn strip_claude_one_m_marker(model: &str) -> String {
        crate::proxy::model_mapper::strip_one_m_suffix_for_upstream(model)
            .trim()
            .to_string()
    }

    fn claude_provider_with_effective_settings(
        &self,
        provider: &Provider,
    ) -> Result<Provider, String> {
        let mut effective_provider = provider.clone();
        effective_provider.settings_config = build_effective_settings_with_common_config(
            self.db.as_ref(),
            &AppType::Claude,
            provider,
        )
        .map_err(|e| format!("Failed to build the effective Claude settings: {e}"))?;
        Ok(effective_provider)
    }

    pub async fn sync_claude_live_from_provider_while_proxy_active(
        &self,
        provider: &Provider,
    ) -> Result<(), String> {
        let effective_provider = self.claude_provider_with_effective_settings(provider)?;
        let mut effective_settings = effective_provider.settings_config.clone();
        let (proxy_url, _) = self.build_proxy_urls().await?;

        Self::apply_claude_takeover_fields_for_provider(
            &mut effective_settings,
            &proxy_url,
            &effective_provider,
        );
        self.write_claude_live(&effective_settings)?;
        Ok(())
    }

    pub async fn sync_codex_live_from_provider_while_proxy_active(
        &self,
        provider: &Provider,
    ) -> Result<(), String> {
        let existing_live = self.read_codex_live().ok();
        let mut effective_settings = build_effective_settings_with_common_config(
            self.db.as_ref(),
            &AppType::Codex,
            provider,
        )
        .map_err(|e| format!("Failed to build the effective Codex settings: {e}"))?;
        if let Some(existing_live) = existing_live.as_ref() {
            Self::preserve_codex_user_config_from_existing_config(
                &mut effective_settings,
                existing_live,
            )?;
            sync_codex_desktop_table(&mut effective_settings, existing_live)
                .map_err(|e| format!("Failed to preserve Codex desktop settings: {e}"))?;
        } else {
            sync_codex_desktop_table(&mut effective_settings, &json!({ "config": "" }))
                .map_err(|e| format!("Failed to clear stale Codex desktop settings: {e}"))?;
        }
        let (_, proxy_codex_base_url) = self.build_proxy_urls().await?;

        if let Some(auth) = effective_settings
            .get_mut("auth")
            .and_then(|v| v.as_object_mut())
        {
            auth.insert("OPENAI_API_KEY".to_string(), json!(PROXY_TOKEN_PLACEHOLDER));
        } else if let Some(root) = effective_settings.as_object_mut() {
            root.insert(
                "auth".to_string(),
                json!({ "OPENAI_API_KEY": PROXY_TOKEN_PLACEHOLDER }),
            );
        }

        let config_str = effective_settings
            .get("config")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let updated_config = Self::apply_codex_proxy_toml_config_for_provider(
            config_str,
            &proxy_codex_base_url,
            Some(provider),
        );
        effective_settings["config"] = json!(updated_config);
        Self::attach_codex_model_catalog_from_provider(&mut effective_settings, Some(provider));

        self.write_codex_takeover_live_for_provider(&effective_settings, Some(provider))?;
        Ok(())
    }

    fn get_current_provider_for_app(&self, app_type: &AppType) -> Result<Option<Provider>, String> {
        // During takeover the database target drives request routing, so it must
        // also drive the live profile. A crash/restart can leave the device
        // settings pointing at the previously restored Official profile even
        // though the database still selects a third-party provider.
        let database_current = self
            .db
            .get_current_provider(app_type.as_str())
            .map_err(|e| format!("Failed to read the {app_type:?} database provider: {e}"))?;
        let current_id = if let Some(database_id) = database_current {
            if crate::settings::get_current_provider(app_type).as_deref()
                != Some(database_id.as_str())
            {
                log::warn!(
                    "{app_type:?} takeover target differs from device settings; using database target {database_id} and repairing device settings"
                );
                crate::settings::set_current_provider(app_type, Some(&database_id)).map_err(
                    |e| format!("Failed to repair the {app_type:?} device provider: {e}"),
                )?;
            }
            Some(database_id)
        } else {
            crate::settings::get_effective_current_provider(&self.db, app_type)
                .map_err(|e| format!("Failed to get the current {app_type:?} provider: {e}"))?
        };

        let Some(current_id) = current_id else {
            return Ok(None);
        };

        self.db
            .get_provider_by_id(&current_id, app_type.as_str())
            .map_err(|e| format!("Failed to read the current {app_type:?} provider: {e}"))
    }

    fn require_current_provider_for_app(&self, app_type: &AppType) -> Result<Provider, String> {
        self.get_current_provider_for_app(app_type)?.ok_or_else(|| {
            format!(
                "The current {app_type:?} provider does not exist, so Live takeover cannot proceed"
            )
        })
    }

    /// Set the AppHandle during app initialization.
    pub fn set_app_handle(&self, handle: tauri::AppHandle) {
        futures::executor::block_on(async {
            *self.app_handle.write().await = Some(handle);
        });
    }

    pub(crate) async fn lock_switch_for_app(
        &self,
        app_type: &str,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        self.switch_locks.lock_for_app(app_type).await
    }

    /// Start the proxy server.
    pub async fn start(&self) -> Result<ProxyServerInfo, String> {
        // 1. Automatically set proxy_enabled = true on startup.
        let mut global_config = self
            .db
            .get_global_proxy_config()
            .await
            .map_err(|e| format!("Failed to get the global proxy configuration: {e}"))?;

        if !global_config.proxy_enabled {
            global_config.proxy_enabled = true;
            self.db
                .update_global_proxy_config(global_config.clone())
                .await
                .map_err(|e| format!("Failed to update the global proxy switch: {e}"))?;
        }

        // 2. Load the configuration.
        let config = self
            .db
            .get_proxy_config()
            .await
            .map_err(|e| format!("Failed to get the proxy configuration: {e}"))?;

        // 3. If already running, reconcile persisted state if needed and return the current info.
        if let Some(server) = self.server.read().await.as_ref() {
            let status = server.get_status().await;
            return Ok(ProxyServerInfo {
                address: status.address,
                port: status.port,
                // The original start time is not available precisely; the current
                // time is sufficient for UI display.
                started_at: chrono::Utc::now().to_rfc3339(),
            });
        }

        // 4. Create and start the server.
        let app_handle = self.app_handle.read().await.clone();
        let server = ProxyServer::new(config.clone(), self.db.clone(), app_handle);
        let info = server
            .start()
            .await
            .map_err(|e| format!("Failed to start the proxy server: {e}"))?;
        if let Err(e) = self
            .persist_ephemeral_listen_port_if_needed(&config, info.port)
            .await
        {
            let _ = server.stop().await;
            return Err(e);
        }

        // 5. Store the server instance.
        *self.server.write().await = Some(server);

        log::info!("Proxy server started: {}:{}", info.address, info.port);
        Ok(info)
    }

    async fn persist_ephemeral_listen_port_if_needed(
        &self,
        config: &ProxyConfig,
        actual_port: u16,
    ) -> Result<(), String> {
        if config.listen_port != 0 {
            return Ok(());
        }

        let mut resolved_config = config.clone();
        resolved_config.listen_port = actual_port;
        self.db
            .update_proxy_config(resolved_config)
            .await
            .map_err(|e| format!("Failed to persist the ephemeral proxy port: {e}"))
    }

    async fn start_before_takeover_if_ephemeral_port(&self) -> Result<bool, String> {
        let config = self
            .db
            .get_proxy_config()
            .await
            .map_err(|e| format!("Failed to get the proxy configuration: {e}"))?;
        if config.listen_port != 0 || self.is_running().await {
            return Ok(false);
        }

        self.start().await?;
        Ok(true)
    }

    /// Start the proxy server with Live config takeover.
    pub async fn start_with_takeover(&self) -> Result<ProxyServerInfo, String> {
        // 1. Back up each app's Live config.
        self.backup_live_configs().await?;

        // 2. Sync tokens from Live config into the database so the proxy sees the latest auth.
        if let Err(e) = self.sync_live_to_providers().await {
            // Takeover has not been written yet, but the backup may contain
            // sensitive data, so clean it up if possible.
            if let Err(clean_err) = self.db.delete_all_live_backups().await {
                log::warn!("Failed to delete Live backups: {clean_err}");
            }
            return Err(e);
        }

        // Port 0 requires starting the proxy first so we can use the real OS-assigned
        // port; otherwise takeover would write :0 into the Live config.
        let started_proxy_before_takeover =
            match self.start_before_takeover_if_ephemeral_port().await {
                Ok(started) => started,
                Err(e) => {
                    if let Err(clean_err) = self.db.delete_all_live_backups().await {
                        log::warn!("Failed to delete Live backups: {clean_err}");
                    }
                    return Err(e);
                }
            };

        // 3. Persist the takeover flag before writing takeover config so a
        // power loss or kill during takeover can still be detected and repaired
        // on the next startup.
        if let Err(e) = self.db.set_live_takeover_active(true).await {
            if let Err(clean_err) = self.db.delete_all_live_backups().await {
                log::warn!("Failed to delete Live backups: {clean_err}");
            }
            if started_proxy_before_takeover {
                let _ = self.stop().await;
            }
            return Err(format!("Failed to set the takeover state: {e}"));
        }

        // 4. Take over each app's Live config by writing proxy addresses and clearing tokens.
        if let Err(e) = self.takeover_live_configs().await {
            // Takeover may have partially written the config. Attempt to restore
            // the original config; if that fails, keep the flag and backup so
            // the next startup can repair it automatically.
            log::error!(
                "Failed to take over Live config; attempting to restore the original config: {e}"
            );
            match self.restore_live_configs().await {
                Ok(()) => {
                    let _ = self.db.set_live_takeover_active(false).await;
                    let _ = self.db.delete_all_live_backups().await;
                }
                Err(restore_err) => {
                    log::error!("Failed to restore the original config; keeping the backup for recovery on next startup: {restore_err}");
                }
            }
            if started_proxy_before_takeover {
                let _ = self.stop().await;
            }
            return Err(e);
        }

        // 5. Start the proxy server.
        match self.start().await {
            Ok(info) => Ok(info),
            Err(e) => {
                // Startup failed, so restore the original config.
                log::error!("Proxy startup failed; attempting to restore the original config: {e}");
                match self.restore_live_configs().await {
                    Ok(()) => {
                        let _ = self.db.set_live_takeover_active(false).await;
                        let _ = self.db.delete_all_live_backups().await;
                    }
                    Err(restore_err) => {
                        log::error!("Failed to restore the original config; keeping the backup for recovery on next startup: {restore_err}");
                    }
                }
                if started_proxy_before_takeover {
                    let _ = self.stop().await;
                }
                Err(e)
            }
        }
    }

    /// Get each app's takeover status, meaning whether its Live config points to the local proxy.
    pub async fn get_takeover_status(&self) -> Result<ProxyTakeoverStatus, String> {
        // Prefer proxy_config.enabled, while remaining compatible with legacy
        // live_backup-based detection.
        let claude_enabled = self
            .db
            .get_proxy_config_for_app("claude")
            .await
            .map(|c| c.enabled)
            .unwrap_or(false);
        let codex_enabled = self
            .db
            .get_proxy_config_for_app("codex")
            .await
            .map(|c| c.enabled)
            .unwrap_or(false);
        let gemini_enabled = self
            .db
            .get_proxy_config_for_app("gemini")
            .await
            .map(|c| c.enabled)
            .unwrap_or(false);
        // OpenCode and OpenClaw don't support proxy features, always return false
        let opencode_enabled = false;
        let openclaw_enabled = false;

        Ok(ProxyTakeoverStatus {
            claude: claude_enabled,
            codex: codex_enabled,
            gemini: gemini_enabled,
            opencode: opencode_enabled,
            openclaw: openclaw_enabled,
        })
    }

    /// Enable or disable Live takeover for the specified app.
    ///
    /// - Enable: automatically start the proxy service and take over only this app's Live config
    /// - Disable: restore only this app's Live config; stop the proxy if nothing else is taken over
    pub async fn set_takeover_for_app(&self, app_type: &str, enabled: bool) -> Result<(), String> {
        let app = AppType::from_str(app_type).map_err(|e| format!("Invalid app type: {e}"))?;
        let app_type_str = app.as_str();
        let _guard = self.switch_locks.lock_for_app(app_type_str).await;

        self.set_takeover_for_app_inner(app_type_str, enabled).await
    }

    /// Apply a takeover change while the caller holds this app's switch lock.
    pub(crate) async fn set_takeover_for_app_inner(
        &self,
        app_type: &str,
        enabled: bool,
    ) -> Result<(), String> {
        self.set_takeover_for_app_inner_with_health_reset(app_type, enabled, true)
            .await
    }

    /// Apply a takeover change while deferring health reset until a larger
    /// provider/routing transition has committed.
    pub(crate) async fn set_takeover_for_app_inner_deferred_health_reset(
        &self,
        app_type: &str,
        enabled: bool,
    ) -> Result<(), String> {
        self.set_takeover_for_app_inner_with_health_reset(app_type, enabled, false)
            .await
    }

    async fn set_takeover_for_app_inner_with_health_reset(
        &self,
        app_type: &str,
        enabled: bool,
        reset_health_on_disable: bool,
    ) -> Result<(), String> {
        let app = AppType::from_str(app_type).map_err(|e| format!("Invalid app type: {e}"))?;
        let app_type_str = app.as_str();

        if enabled {
            // 1) Start is idempotent and also reconciles the persisted global
            // master switch when a listener survived but that flag did not.
            self.start().await?;

            // 2) Reconcile all takeover signals before deciding whether this is
            // already active. A crash can leave enabled out of sync with the
            // backup/live placeholder, so enabled alone is not authoritative.
            let current_config = self
                .db
                .get_proxy_config_for_app(app_type_str)
                .await
                .map_err(|e| format!("Failed to get the {app_type_str} configuration: {e}"))?;
            let existing_backup = self
                .db
                .get_live_backup(app_type_str)
                .await
                .map_err(|e| format!("Failed to read the {app_type_str} backup: {e}"))?;
            let backup_is_usable = existing_backup.as_ref().is_some_and(|backup| {
                serde_json::from_str::<Value>(&backup.original_config)
                    .ok()
                    .is_some_and(|config| !Self::live_has_proxy_placeholder_for_app(&app, &config))
            });
            let live_taken_over = self.detect_takeover_in_live_config_for_app(&app);
            let live_matches_current_proxy = match self
                .live_takeover_matches_current_proxy(&app)
                .await
            {
                Ok(value) => value,
                Err(e) => {
                    log::warn!("Failed to inspect {app_type_str} takeover config; takeover rebuild will continue: {e}");
                    false
                }
            };

            // A healthy active takeover requires all three durable signals.
            if current_config.enabled && backup_is_usable && live_matches_current_proxy {
                // The endpoint can already be correct while the restored Codex
                // profile still advertises the previous GPT model. Reapply the
                // current database target so the Desktop model label/catalog and
                // the router agree after restart.
                if matches!(app, AppType::Codex) {
                    self.takeover_live_config_strict(&app).await?;
                }
                return Ok(());
            }

            if current_config.enabled || existing_backup.is_some() || live_taken_over {
                log::warn!(
                    "{app_type_str} takeover state is incomplete: enabled={} backup={} usable_backup={} live_taken_over={} live_matches_current_proxy={}; repairing",
                    current_config.enabled,
                    existing_backup.is_some(),
                    backup_is_usable,
                    live_taken_over,
                    live_matches_current_proxy
                );
            }

            // 3) Back up the Live config in strict mode: error if the target app config is missing.
            if backup_is_usable {
                self.restore_live_config_for_app_inner(&app).await?;
            } else {
                if existing_backup.is_some() {
                    self.db
                        .delete_live_backup(app_type_str)
                        .await
                        .map_err(|e| {
                            format!("Failed to delete the invalid {app_type_str} Live backup: {e}")
                        })?;
                }
                if live_taken_over {
                    self.restore_live_config_for_app_with_fallback_inner(&app)
                        .await?;
                }
                self.backup_live_config_strict(&app).await?;

                // 4) Sync Live tokens into the database for this app only.
                if let Err(e) = self.sync_live_to_provider(&app).await {
                    let _ = self.db.delete_live_backup(app_type_str).await;
                    return Err(e);
                }
            }

            // 5) Write takeover config for this app only.
            if let Err(e) = self.takeover_live_config_strict(&app).await {
                log::error!("{app_type_str} Live takeover failed; attempting restore: {e}");
                match self.restore_live_config_for_app_inner(&app).await {
                    Ok(()) => {
                        // Delete the backup only after a successful restore so
                        // failure cases keep their only rollback source.
                        let _ = self.db.delete_live_backup(app_type_str).await;
                    }
                    Err(restore_err) => {
                        log::error!(
                            "{app_type_str} Live restore failed; keeping the backup for recovery on next startup: {restore_err}"
                        );
                    }
                }
                return Err(e);
            }

            // 6) Set proxy_config.enabled = true.
            let mut updated_config = self
                .db
                .get_proxy_config_for_app(app_type_str)
                .await
                .map_err(|e| format!("Failed to get the {app_type_str} configuration: {e}"))?;
            updated_config.enabled = true;
            self.db
                .update_proxy_config_for_app(updated_config)
                .await
                .map_err(|e| format!("Failed to set {app_type_str} enabled state: {e}"))?;

            // 7) Maintain compatibility with legacy logic by writing the any-of flag.
            let _ = self.db.set_live_takeover_active(true).await;

            // 8) Warn if the current provider is official (risk of account ban via proxy)
            if let Ok(Some(current_id)) =
                crate::settings::get_effective_current_provider(&self.db, &app)
            {
                if let Ok(Some(provider)) = self.db.get_provider_by_id(&current_id, app_type_str) {
                    if provider.category.as_deref() == Some("official") {
                        if let Some(handle) = self.app_handle.read().await.as_ref() {
                            let _ = handle.emit(
                                "proxy-official-warning",
                                serde_json::json!({
                                    "appType": app_type_str,
                                    "providerName": provider.name,
                                }),
                            );
                        }
                    }
                }
            }

            return Ok(());
        }

        // When disabling takeover, inspect enabled state, backup state, and
        // Live placeholders together. A crash may persist only part of them;
        // those leftovers still need restore and cleanup, so enabled=false alone
        // is not enough to return early.
        let current_config = self
            .db
            .get_proxy_config_for_app(app_type_str)
            .await
            .map_err(|e| format!("Failed to get the {app_type_str} configuration: {e}"))?;
        let has_backup = self
            .db
            .get_live_backup(app_type_str)
            .await
            .map_err(|e| format!("Failed to get the {app_type_str} Live backup: {e}"))?
            .is_some();
        let live_taken_over = self.detect_takeover_in_live_config_for_app(&app);

        if !current_config.enabled && !has_backup && !live_taken_over {
            return Ok(()); // Not taken over; return idempotently.
        }

        // 1) Restore the Live config.
        //
        // This must use the with_fallback path: backup -> SSOT -> placeholder
        // cleanup. The simpler restore_live_config_for_app silently returns Ok(())
        // when the backup is missing, leaving behind takeover placeholders
        // (proxy address / PROXY_MANAGED token) and breaking the client.
        self.restore_live_config_for_app_with_fallback_inner(&app)
            .await?;

        // 2) Delete this app's backup to avoid storing sensitive tokens long term.
        self.db
            .delete_live_backup(app_type_str)
            .await
            .map_err(|e| format!("Failed to delete the {app_type_str} Live backup: {e}"))?;

        // 3) Set proxy_config.enabled = false.
        let mut updated_config = self
            .db
            .get_proxy_config_for_app(app_type_str)
            .await
            .map_err(|e| format!("Failed to get the {app_type_str} configuration: {e}"))?;
        updated_config.enabled = false;
        self.db
            .update_proxy_config_for_app(updated_config)
            .await
            .map_err(|e| format!("Failed to clear {app_type_str} enabled state: {e}"))?;

        // 4) Clear this app's health state to reset queue state when the proxy is disabled.
        // Atomic provider transitions defer this irreversible delete until
        // the target provider and Live config have both committed.
        if reset_health_on_disable {
            self.db
                .clear_provider_health_for_app(app_type_str)
                .await
                .map_err(|e| format!("Failed to clear {app_type_str} health state: {e}"))?;
        }

        // 5) If nothing else is taken over, update the legacy flag and stop the proxy service.
        // Check whether any other app still has enabled = true.
        let any_enabled = self
            .db
            .is_live_takeover_active()
            .await
            .map_err(|e| format!("Failed to inspect takeover state: {e}"))?;

        if !any_enabled {
            let _ = self.db.set_live_takeover_active(false).await;

            let desktop_uses_proxy = current_claude_desktop_uses_proxy(self.db.as_ref())
                .unwrap_or_else(|error| {
                    log::warn!(
                        "Failed to inspect the Claude Desktop route; keeping the proxy listener: {error}"
                    );
                    true
                });
            if !desktop_uses_proxy {
                if self.is_running().await {
                    let _ = self.stop().await;
                } else {
                    // A crash can leave the persisted master switch on after the
                    // listener disappeared. Reconcile it even when there is no
                    // server object for stop() to consume.
                    let mut global_config =
                        self.db.get_global_proxy_config().await.map_err(|e| {
                            format!("Failed to read the global proxy configuration: {e}")
                        })?;
                    if global_config.proxy_enabled {
                        global_config.proxy_enabled = false;
                        self.db
                            .update_global_proxy_config(global_config)
                            .await
                            .map_err(|e| format!("Failed to clear the global proxy switch: {e}"))?;
                    }
                }
            }
        }

        Ok(())
    }

    /// Sync tokens from Live config into the database.
    ///
    /// Call this before clearing Live tokens so the provider config in the
    /// database has the latest token values and the proxy reads correct auth.
    async fn sync_live_to_provider(&self, app_type: &AppType) -> Result<(), String> {
        let live_config = match app_type {
            AppType::Claude => self.read_claude_live()?,
            AppType::Codex => self.read_codex_live()?,
            AppType::Gemini => self.read_gemini_live()?,
            _ => return Err("This app does not support proxy features".to_string()),
        };

        self.sync_live_config_to_provider(app_type, &live_config)
            .await
    }

    async fn sync_live_config_to_provider(
        &self,
        app_type: &AppType,
        live_config: &Value,
    ) -> Result<(), String> {
        match app_type {
            AppType::Claude => {
                let provider_id =
                    crate::settings::get_effective_current_provider(&self.db, &AppType::Claude)
                        .map_err(|e| format!("Failed to get the current Claude provider: {e}"))?;

                if let Some(provider_id) = provider_id {
                    if let Ok(Some(mut provider)) =
                        self.db.get_provider_by_id(&provider_id, "claude")
                    {
                        if let Some(env) = live_config.get("env").and_then(|v| v.as_object()) {
                            let token_pair = [
                                "ANTHROPIC_AUTH_TOKEN",
                                "ANTHROPIC_API_KEY",
                                "OPENROUTER_API_KEY",
                                "OPENAI_API_KEY",
                            ]
                            .into_iter()
                            .find_map(|key| {
                                env.get(key)
                                    .and_then(|v| v.as_str())
                                    .map(|s| (key, s.trim()))
                            })
                            .filter(|(_, token)| {
                                !token.is_empty() && *token != PROXY_TOKEN_PLACEHOLDER
                            });

                            if let Some((token_key, token)) = token_pair {
                                let env_obj = provider
                                    .settings_config
                                    .get_mut("env")
                                    .and_then(|v| v.as_object_mut());

                                match env_obj {
                                    Some(obj) => {
                                        if token_key == "ANTHROPIC_AUTH_TOKEN"
                                            || token_key == "ANTHROPIC_API_KEY"
                                        {
                                            let mut updated = false;
                                            if obj.contains_key("ANTHROPIC_AUTH_TOKEN") {
                                                obj.insert(
                                                    "ANTHROPIC_AUTH_TOKEN".to_string(),
                                                    json!(token),
                                                );
                                                updated = true;
                                            }
                                            if obj.contains_key("ANTHROPIC_API_KEY") {
                                                obj.insert(
                                                    "ANTHROPIC_API_KEY".to_string(),
                                                    json!(token),
                                                );
                                                updated = true;
                                            }
                                            if !updated {
                                                obj.insert(token_key.to_string(), json!(token));
                                            }
                                        } else {
                                            obj.insert(token_key.to_string(), json!(token));
                                        }
                                    }
                                    None => {
                                        // Write at least one usable token.
                                        if provider.settings_config.is_null() {
                                            provider.settings_config = json!({});
                                        }

                                        if let Some(root) = provider.settings_config.as_object_mut()
                                        {
                                            root.insert(
                                                "env".to_string(),
                                                json!({ token_key: token }),
                                            );
                                        } else {
                                            log::warn!(
                                                "Claude provider settings_config has an invalid format (not an object); skipping token write (provider: {provider_id})"
                                            );
                                        }
                                    }
                                }

                                if let Err(e) = self.db.update_provider_settings_config(
                                    "claude",
                                    &provider_id,
                                    &provider.settings_config,
                                ) {
                                    log::warn!(
                                        "Failed to sync the Claude token into the database: {e}"
                                    );
                                } else {
                                    log::info!(
                                        "Synced the Claude token into the database (provider: {provider_id})"
                                    );
                                }
                            }
                        }
                    }
                }
            }
            AppType::Codex => {
                let provider_id =
                    crate::settings::get_effective_current_provider(&self.db, &AppType::Codex)
                        .map_err(|e| format!("Failed to get the current Codex provider: {e}"))?;

                if let Some(provider_id) = provider_id {
                    if let Ok(Some(mut provider)) =
                        self.db.get_provider_by_id(&provider_id, "codex")
                    {
                        if let Some(token) = live_config
                            .get("auth")
                            .and_then(|v| v.get("OPENAI_API_KEY"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.trim())
                            .filter(|s| !s.is_empty() && *s != PROXY_TOKEN_PLACEHOLDER)
                        {
                            if let Some(auth_obj) = provider
                                .settings_config
                                .get_mut("auth")
                                .and_then(|v| v.as_object_mut())
                            {
                                auth_obj.insert("OPENAI_API_KEY".to_string(), json!(token));
                            } else {
                                if provider.settings_config.is_null() {
                                    provider.settings_config = json!({});
                                }

                                if let Some(root) = provider.settings_config.as_object_mut() {
                                    root.insert(
                                        "auth".to_string(),
                                        json!({ "OPENAI_API_KEY": token }),
                                    );
                                } else {
                                    log::warn!(
                                        "Codex provider settings_config has an invalid format (not an object); skipping token write (provider: {provider_id})"
                                    );
                                }
                            }

                            if let Err(e) = self.db.update_provider_settings_config(
                                "codex",
                                &provider_id,
                                &provider.settings_config,
                            ) {
                                log::warn!("Failed to sync the Codex token into the database: {e}");
                            } else {
                                log::info!("Synced the Codex token into the database (provider: {provider_id})");
                            }
                        }
                    }
                }
            }
            AppType::Gemini => {
                let provider_id =
                    crate::settings::get_effective_current_provider(&self.db, &AppType::Gemini)
                        .map_err(|e| format!("Failed to get the current Gemini provider: {e}"))?;

                if let Some(provider_id) = provider_id {
                    if let Ok(Some(mut provider)) =
                        self.db.get_provider_by_id(&provider_id, "gemini")
                    {
                        if let Some(token) = live_config
                            .get("env")
                            .and_then(|v| v.get("GEMINI_API_KEY"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.trim())
                            .filter(|s| !s.is_empty() && *s != PROXY_TOKEN_PLACEHOLDER)
                        {
                            if let Some(env_obj) = provider
                                .settings_config
                                .get_mut("env")
                                .and_then(|v| v.as_object_mut())
                            {
                                env_obj.insert("GEMINI_API_KEY".to_string(), json!(token));
                            } else {
                                if provider.settings_config.is_null() {
                                    provider.settings_config = json!({});
                                }

                                if let Some(root) = provider.settings_config.as_object_mut() {
                                    root.insert(
                                        "env".to_string(),
                                        json!({ "GEMINI_API_KEY": token }),
                                    );
                                } else {
                                    log::warn!(
                                        "Gemini provider settings_config has an invalid format (not an object); skipping token write (provider: {provider_id})"
                                    );
                                }
                            }

                            if let Err(e) = self.db.update_provider_settings_config(
                                "gemini",
                                &provider_id,
                                &provider.settings_config,
                            ) {
                                log::warn!(
                                    "Failed to sync the Gemini token into the database: {e}"
                                );
                            } else {
                                log::info!(
                                    "Synced the Gemini token into the database (provider: {provider_id})"
                                );
                            }
                        }
                    }
                }
            }
            _ => {}
        }

        Ok(())
    }

    async fn sync_live_to_providers(&self) -> Result<(), String> {
        if let Ok(live_config) = self.read_claude_live() {
            self.sync_live_config_to_provider(&AppType::Claude, &live_config)
                .await?;
        }

        if let Ok(live_config) = self.read_codex_live() {
            self.sync_live_config_to_provider(&AppType::Codex, &live_config)
                .await?;
        }

        if let Ok(live_config) = self.read_gemini_live() {
            self.sync_live_config_to_provider(&AppType::Gemini, &live_config)
                .await?;
        }

        log::info!("Finished syncing Live config tokens");
        Ok(())
    }

    /// Stop the listener without changing persisted proxy or provider state.
    pub(crate) async fn stop_server(&self) -> Result<(), String> {
        if let Some(server) = self.server.write().await.take() {
            server
                .stop()
                .await
                .map_err(|e| format!("Failed to stop the proxy server: {e}"))?;

            log::info!("Proxy server stopped");
            Ok(())
        } else {
            Err("The proxy server is not running".to_string())
        }
    }

    /// Stop the proxy server.
    pub async fn stop(&self) -> Result<(), String> {
        self.stop_server().await?;

        // Persist the stopped state for the next launch.
        let mut global_config = self
            .db
            .get_global_proxy_config()
            .await
            .map_err(|e| format!("Failed to read the global proxy configuration: {e}"))?;

        if global_config.proxy_enabled {
            global_config.proxy_enabled = false;
            if let Err(e) = self.db.update_global_proxy_config(global_config).await {
                log::warn!("Failed to persist the global proxy switch: {e}");
            }
        }

        Ok(())
    }

    /// Stop the proxy server and restore Live config for manual shutdown.
    ///
    /// This clears proxy state from the settings table, so startup will not auto-restore it.
    pub async fn stop_with_restore(&self) -> Result<(), String> {
        // 1. Stop the proxy server, but continue restore logic even if it is already down.
        if let Err(e) = self.stop().await {
            log::warn!("Failed to stop the proxy server; continuing with Live restore: {e}");
        }

        // 2. Restore the original Live config.
        self.restore_live_configs().await?;

        // 3. Clear takeover state in proxy_config for legacy compatibility.
        self.db
            .set_live_takeover_active(false)
            .await
            .map_err(|e| format!("Failed to clear the takeover state: {e}"))?;

        // 4. Clear enabled state for all apps. Manual shutdown should not auto-restore next time.
        for app_type in ["claude", "codex", "gemini"] {
            if let Ok(mut config) = self.db.get_proxy_config_for_app(app_type).await {
                if config.enabled {
                    config.enabled = false;
                    if let Err(e) = self.db.update_proxy_config_for_app(config).await {
                        log::warn!("Failed to clear {app_type} enabled state: {e}");
                    }
                }
            }
        }

        // 5. Delete backups.
        self.db
            .delete_all_live_backups()
            .await
            .map_err(|e| format!("Failed to delete backups: {e}"))?;

        // 6. Reset health state so health badges return to normal.
        self.db
            .clear_all_provider_health()
            .await
            .map_err(|e| format!("Failed to reset health state: {e}"))?;

        // Intentionally keep failover queue and switch state for the next proxy start.
        log::info!("Proxy stopped and Live config restored");
        Ok(())
    }

    /// Stop the proxy server and restore Live config while keeping settings-table proxy state.
    ///
    /// Used during normal shutdown so the next startup can auto-restore proxy state.
    pub async fn stop_with_restore_keep_state(&self) -> Result<(), String> {
        // 1. Stop the proxy server, but continue restore logic even if it is already down.
        if let Err(e) = self.stop_server().await {
            log::warn!("Failed to stop the proxy server; continuing with Live restore: {e}");
        }

        // 2. Restore the original Live config.
        self.restore_live_configs().await?;

        // 3. Update the legacy live_takeover_active flag in proxy_config.
        // Keep proxy_config.enabled so the next startup can auto-restore.
        if let Ok(mut config) = self.db.get_proxy_config().await {
            config.live_takeover_active = false;
            let _ = self.db.update_proxy_config(config).await;
        }

        // 4. Delete backups because Live config has already been restored.
        self.db
            .delete_all_live_backups()
            .await
            .map_err(|e| format!("Failed to delete backups: {e}"))?;

        // 5. Reset health state.
        self.db
            .clear_all_provider_health()
            .await
            .map_err(|e| format!("Failed to reset health state: {e}"))?;

        log::info!("Proxy stopped and Live config restored; proxy state was kept for automatic restore on next startup");
        Ok(())
    }

    /// Back up each app's Live config.
    async fn backup_live_configs(&self) -> Result<(), String> {
        // Claude
        if let Ok(config) = self.read_claude_live() {
            // Skip Live configs already taken over by the proxy so takeover
            // placeholders are not stored as the "original Live" backup.
            // Otherwise a later start_with_takeover in a bad historical state
            // (Live already contains placeholders) can overwrite a good backup
            // with proxy config, and stop() would then restore those
            // placeholders back into Live, leaving it stuck at 127.0.0.1:15721.
            if Self::live_has_proxy_placeholder_for_app(&AppType::Claude, &config) {
                log::warn!("claude Live is already taken over by the proxy; skipping backup so proxy config is not frozen into the backup slot. The next stop will rebuild Live from SSOT");
            } else {
                let json_str = serde_json::to_string(&config)
                    .map_err(|e| format!("Failed to serialize the Claude config: {e}"))?;
                self.db
                    .save_live_backup("claude", &json_str)
                    .await
                    .map_err(|e| format!("Failed to back up the Claude config: {e}"))?;
            }
        }

        // Codex
        if let Ok(mut config) = self.read_codex_live() {
            if Self::live_has_proxy_placeholder_for_app(&AppType::Codex, &config) {
                log::warn!("codex Live is already taken over by the proxy; skipping backup so proxy config is not frozen into the backup slot. The next stop will rebuild Live from SSOT");
            } else {
                self.normalize_codex_live_backup_route(&mut config)?;
                let json_str = serde_json::to_string(&config)
                    .map_err(|e| format!("Failed to serialize the Codex config: {e}"))?;
                self.db
                    .save_live_backup("codex", &json_str)
                    .await
                    .map_err(|e| format!("Failed to back up the Codex config: {e}"))?;
            }
        }

        // Gemini
        if let Ok(config) = self.read_gemini_live() {
            if Self::live_has_proxy_placeholder_for_app(&AppType::Gemini, &config) {
                log::warn!("gemini Live is already taken over by the proxy; skipping backup so proxy config is not frozen into the backup slot. The next stop will rebuild Live from SSOT");
            } else {
                let json_str = serde_json::to_string(&config)
                    .map_err(|e| format!("Failed to serialize the Gemini config: {e}"))?;
                self.db
                    .save_live_backup("gemini", &json_str)
                    .await
                    .map_err(|e| format!("Failed to back up the Gemini config: {e}"))?;
            }
        }

        log::info!("Backed up Live config for all apps");
        Ok(())
    }

    /// Back up the specified app's Live config in strict mode: missing target config is an error.
    async fn backup_live_config_strict(&self, app_type: &AppType) -> Result<(), String> {
        let (app_type_str, mut config) = match app_type {
            AppType::Claude => ("claude", self.read_claude_live()?),
            AppType::Codex => ("codex", self.read_codex_live()?),
            AppType::Gemini => ("gemini", self.read_gemini_live()?),
            _ => return Err("This app does not support proxy features".to_string()),
        };

        // Skip Live configs already taken over by the proxy so takeover
        // placeholders are not stored as the "original Live" backup. See the
        // comment in backup_live_configs for the full rationale.
        if Self::live_has_proxy_placeholder_for_app(app_type, &config) {
            log::warn!(
                "{app_type_str} Live is already taken over by the proxy; skipping backup so proxy config is not frozen into the backup slot. The next stop will rebuild Live from SSOT"
            );
            return Ok(());
        }

        if matches!(app_type, AppType::Codex) {
            self.normalize_codex_live_backup_route(&mut config)?;
        }
        let json_str = serde_json::to_string(&config)
            .map_err(|e| format!("Failed to serialize the {app_type_str} config: {e}"))?;
        self.db
            .save_live_backup(app_type_str, &json_str)
            .await
            .map_err(|e| format!("Failed to back up the {app_type_str} config: {e}"))?;

        Ok(())
    }

    fn normalize_codex_live_backup_route(&self, config: &mut Value) -> Result<(), String> {
        let provider = self.get_current_provider_for_app(&AppType::Codex)?;
        crate::codex_config::apply_codex_unified_session_bucket_to_settings(
            provider
                .as_ref()
                .and_then(|provider| provider.category.as_deref()),
            config,
        )
        .map_err(|e| format!("Failed to normalize Codex backup history route: {e}"))
    }

    /// Build proxy URLs for writing into Live config, including 0.0.0.0 and IPv6 handling.
    async fn build_proxy_urls(&self) -> Result<(String, String), String> {
        let config = self
            .db
            .get_proxy_config()
            .await
            .map_err(|e| format!("Failed to get the proxy configuration: {e}"))?;

        // listen_address can be 0.0.0.0 to listen on all interfaces, but
        // clients cannot connect to 0.0.0.0. Prefer the local loopback address
        // when writing config back into each app.
        let connect_host = match config.listen_address.as_str() {
            "0.0.0.0" => "127.0.0.1".to_string(),
            "::" => "::1".to_string(),
            _ => config.listen_address.clone(),
        };
        let connect_host_for_url = if connect_host.contains(':') && !connect_host.starts_with('[') {
            format!("[{connect_host}]")
        } else {
            connect_host
        };

        let mut listen_port = config.listen_port;
        if let Some(server) = self.server.read().await.as_ref() {
            let status = server.get_status().await;
            if status.running {
                listen_port = status.port;
            }
        }
        if listen_port == 0 {
            return Err("The proxy listen port is 0, but the proxy server is not running, so a takeover address cannot be generated".to_string());
        }

        let proxy_origin = format!("http://{}:{}", connect_host_for_url, listen_port);
        let proxy_url = proxy_origin.clone();
        let proxy_codex_base_url = format!("{}/v1", proxy_origin.trim_end_matches('/'));

        Ok((proxy_url, proxy_codex_base_url))
    }

    /// Take over each app's Live config by writing proxy addresses.
    ///
    /// The proxy server already distinguishes app types by API endpoint:
    /// - `/v1/messages` → Claude
    /// - `/v1/chat/completions`, `/v1/responses` → Codex
    /// - `/v1beta/*` → Gemini
    ///
    /// No app-specific URL prefix is needed.
    async fn takeover_live_configs(&self) -> Result<(), String> {
        let (proxy_url, proxy_codex_base_url) = self.build_proxy_urls().await?;

        // Claude: rewrite ANTHROPIC_BASE_URL and replace real tokens with placeholders.
        if let Ok(mut live_config) = self.read_claude_live() {
            let claude_provider = self.require_current_provider_for_app(&AppType::Claude)?;
            let claude_provider = self.claude_provider_with_effective_settings(&claude_provider)?;
            Self::apply_claude_takeover_fields_for_provider(
                &mut live_config,
                &proxy_url,
                &claude_provider,
            );
            self.write_claude_live(&live_config)?;
            log::info!("Claude Live config taken over; proxy address: {proxy_url}");
        }

        // Codex: rewrite config.toml base_url and auth.json OPENAI_API_KEY.
        if let Ok(mut live_config) = self.read_codex_live() {
            // 1. Rewrite OPENAI_API_KEY in auth.json using the placeholder.
            if let Some(auth) = live_config.get_mut("auth").and_then(|v| v.as_object_mut()) {
                auth.insert("OPENAI_API_KEY".to_string(), json!(PROXY_TOKEN_PLACEHOLDER));
            }

            // 2. Rewrite base_url in config.toml.
            let config_str = live_config
                .get("config")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let codex_provider = self
                .get_current_provider_for_app(&AppType::Codex)
                .ok()
                .flatten();
            let updated_config = Self::apply_codex_proxy_toml_config_for_provider(
                config_str,
                &proxy_codex_base_url,
                codex_provider.as_ref(),
            );
            live_config["config"] = json!(updated_config);
            Self::attach_codex_model_catalog_from_provider(
                &mut live_config,
                codex_provider.as_ref(),
            );

            self.write_codex_takeover_live_for_provider(&live_config, codex_provider.as_ref())?;
            log::info!("Codex Live config taken over; proxy address: {proxy_codex_base_url}");
        }

        // Gemini: rewrite GOOGLE_GEMINI_BASE_URL and replace the real token with a placeholder.
        if let Ok(mut live_config) = self.read_gemini_live() {
            if let Some(env) = live_config.get_mut("env").and_then(|v| v.as_object_mut()) {
                env.insert("GOOGLE_GEMINI_BASE_URL".to_string(), json!(&proxy_url));
                // Use a placeholder to avoid missing-key warnings.
                env.insert("GEMINI_API_KEY".to_string(), json!(PROXY_TOKEN_PLACEHOLDER));
            } else {
                live_config["env"] = json!({
                    "GOOGLE_GEMINI_BASE_URL": &proxy_url,
                    "GEMINI_API_KEY": PROXY_TOKEN_PLACEHOLDER
                });
            }
            self.write_gemini_live(&live_config)?;
            log::info!("Gemini Live config taken over; proxy address: {proxy_url}");
        }

        Ok(())
    }

    /// Take over the specified app's Live config in strict mode: missing target config is an error.
    async fn takeover_live_config_strict(&self, app_type: &AppType) -> Result<(), String> {
        let (proxy_url, proxy_codex_base_url) = self.build_proxy_urls().await?;

        match app_type {
            AppType::Claude => {
                let mut live_config = self.read_claude_live()?;
                let claude_provider = self.require_current_provider_for_app(&AppType::Claude)?;
                let claude_provider =
                    self.claude_provider_with_effective_settings(&claude_provider)?;
                Self::apply_claude_takeover_fields_for_provider(
                    &mut live_config,
                    &proxy_url,
                    &claude_provider,
                );
                self.write_claude_live(&live_config)?;
                log::info!("Claude Live config taken over; proxy address: {proxy_url}");
            }
            AppType::Codex => {
                let mut live_config = self.read_codex_live()?;

                if let Some(auth) = live_config.get_mut("auth").and_then(|v| v.as_object_mut()) {
                    auth.insert("OPENAI_API_KEY".to_string(), json!(PROXY_TOKEN_PLACEHOLDER));
                }

                let config_str = live_config
                    .get("config")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let codex_provider = self.require_current_provider_for_app(&AppType::Codex)?;
                let updated_config = Self::apply_codex_proxy_toml_config_for_provider(
                    config_str,
                    &proxy_codex_base_url,
                    Some(&codex_provider),
                );
                live_config["config"] = json!(updated_config);
                Self::attach_codex_model_catalog_from_provider(
                    &mut live_config,
                    Some(&codex_provider),
                );

                self.write_codex_takeover_live_for_provider(&live_config, Some(&codex_provider))?;
                log::info!("Codex Live config taken over; proxy address: {proxy_codex_base_url}");
            }
            AppType::Gemini => {
                let mut live_config = self.read_gemini_live()?;

                if let Some(env) = live_config.get_mut("env").and_then(|v| v.as_object_mut()) {
                    env.insert("GOOGLE_GEMINI_BASE_URL".to_string(), json!(&proxy_url));
                    env.insert("GEMINI_API_KEY".to_string(), json!(PROXY_TOKEN_PLACEHOLDER));
                } else {
                    live_config["env"] = json!({
                        "GOOGLE_GEMINI_BASE_URL": &proxy_url,
                        "GEMINI_API_KEY": PROXY_TOKEN_PLACEHOLDER
                    });
                }

                self.write_gemini_live(&live_config)?;
                log::info!("Gemini Live config taken over; proxy address: {proxy_url}");
            }
            _ => return Err("This app does not support proxy features".to_string()),
        }

        Ok(())
    }

    /// Take over the specified app's Live config on a best-effort basis: skip missing or unreadable configs.
    async fn takeover_live_config_best_effort(&self, app_type: &AppType) -> Result<(), String> {
        let (proxy_url, proxy_codex_base_url) = self.build_proxy_urls().await?;

        match app_type {
            AppType::Claude => {
                if let Ok(mut live_config) = self.read_claude_live() {
                    let claude_provider = self
                        .get_current_provider_for_app(&AppType::Claude)
                        .ok()
                        .flatten();
                    if let Some(provider) = claude_provider.as_ref() {
                        let provider = self.claude_provider_with_effective_settings(provider)?;
                        Self::apply_claude_takeover_fields_for_provider(
                            &mut live_config,
                            &proxy_url,
                            &provider,
                        );
                    } else {
                        Self::apply_claude_takeover_fields_with_policy(
                            &mut live_config,
                            &proxy_url,
                            ClaudeTakeoverAuthPolicy::PreserveExistingOrAuthToken,
                        );
                    }
                    let _ = self.write_claude_live(&live_config);
                }
            }
            AppType::Codex => {
                if let Ok(mut live_config) = self.read_codex_live() {
                    if let Some(auth) = live_config.get_mut("auth").and_then(|v| v.as_object_mut())
                    {
                        auth.insert("OPENAI_API_KEY".to_string(), json!(PROXY_TOKEN_PLACEHOLDER));
                    }

                    let config_str = live_config
                        .get("config")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let codex_provider = self
                        .get_current_provider_for_app(&AppType::Codex)
                        .ok()
                        .flatten();
                    let updated_config = Self::apply_codex_proxy_toml_config_for_provider(
                        config_str,
                        &proxy_codex_base_url,
                        codex_provider.as_ref(),
                    );
                    live_config["config"] = json!(updated_config);
                    Self::attach_codex_model_catalog_from_provider(
                        &mut live_config,
                        codex_provider.as_ref(),
                    );

                    let _ = self.write_codex_takeover_live_for_provider(
                        &live_config,
                        codex_provider.as_ref(),
                    );
                }
            }
            AppType::Gemini => {
                if let Ok(mut live_config) = self.read_gemini_live() {
                    if let Some(env) = live_config.get_mut("env").and_then(|v| v.as_object_mut()) {
                        env.insert("GOOGLE_GEMINI_BASE_URL".to_string(), json!(&proxy_url));
                        env.insert("GEMINI_API_KEY".to_string(), json!(PROXY_TOKEN_PLACEHOLDER));
                    } else {
                        live_config["env"] = json!({
                            "GOOGLE_GEMINI_BASE_URL": &proxy_url,
                            "GEMINI_API_KEY": PROXY_TOKEN_PLACEHOLDER
                        });
                    }

                    let _ = self.write_gemini_live(&live_config);
                }
            }
            _ => {}
        }

        Ok(())
    }

    async fn restore_live_config_for_app_inner(&self, app_type: &AppType) -> Result<(), String> {
        match app_type {
            AppType::Claude => {
                if let Ok(Some(backup)) = self.db.get_live_backup("claude").await {
                    let config: Value = serde_json::from_str(&backup.original_config)
                        .map_err(|e| format!("Failed to parse the Claude backup: {e}"))?;
                    self.write_claude_live(&config)?;
                    log::info!("Claude Live config restored");
                }
            }
            AppType::Codex => {
                if let Ok(Some(backup)) = self.db.get_live_backup("codex").await {
                    let config: Value = serde_json::from_str(&backup.original_config)
                        .map_err(|e| format!("Failed to parse the Codex backup: {e}"))?;
                    self.write_codex_live(&config)?;
                    log::info!("Codex Live config restored");
                }
            }
            AppType::Gemini => {
                if let Ok(Some(backup)) = self.db.get_live_backup("gemini").await {
                    let config: Value = serde_json::from_str(&backup.original_config)
                        .map_err(|e| format!("Failed to parse the Gemini backup: {e}"))?;
                    self.write_gemini_live(&config)?;
                    log::info!("Gemini Live config restored");
                }
            }
            _ => {}
        }

        Ok(())
    }

    /// Restore the original Live config.
    async fn restore_live_configs(&self) -> Result<(), String> {
        let mut errors = Vec::new();

        for app_type in [AppType::Claude, AppType::Codex, AppType::Gemini] {
            if let Err(e) = self
                .restore_live_config_for_app_with_fallback(&app_type)
                .await
            {
                errors.push(e);
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    async fn restore_live_config_for_app_with_fallback(
        &self,
        app_type: &AppType,
    ) -> Result<(), String> {
        let _guard = self.switch_locks.lock_for_app(app_type.as_str()).await;
        self.restore_live_config_for_app_with_fallback_inner(app_type)
            .await
    }

    async fn restore_live_config_for_app_with_fallback_inner(
        &self,
        app_type: &AppType,
    ) -> Result<(), String> {
        let app_type_str = app_type.as_str();

        // 1) Prefer restoring from the Live backup. This is the only reliable
        // source of the original Live config.
        let backup = self
            .db
            .get_live_backup(app_type_str)
            .await
            .map_err(|e| format!("Failed to get the {app_type_str} Live backup: {e}"))?;
        if let Some(backup) = backup {
            match serde_json::from_str::<Value>(&backup.original_config) {
                Ok(config) => {
                    // If the backup itself already contains proxy placeholders
                    // because of a bad historical state, do not restore it
                    // directly or Live can stay pinned to 127.0.0.1:15721 after stop.
                    // Fall through to the SSOT rebuild below instead.
                    if Self::live_has_proxy_placeholder_for_app(app_type, &config) {
                        log::warn!(
                            "{app_type_str} backup already contains proxy placeholders from an abnormal historical state; skipping the backup and rebuilding Live from SSOT"
                        );
                    } else {
                        self.write_live_config_for_app(app_type, &config)?;
                        log::info!("{app_type_str} Live config restored from backup");
                        return Ok(());
                    }
                }
                Err(error) => {
                    log::warn!(
                        "{app_type_str} Live backup could not be parsed; falling back to SSOT or placeholder cleanup: {error}"
                    );
                }
            }
        }

        // 2) Fallback: the backup is missing, but Live still contains takeover
        // placeholders from a crash or older bug.
        if !self.detect_takeover_in_live_config_for_app(app_type) {
            return Ok(());
        }

        // 2.1) Prefer rebuilding Live from SSOT (the current provider), which is
        // more usable than field cleanup.
        match self.restore_live_from_ssot_for_app(app_type) {
            Ok(true) => {
                log::info!("{app_type_str} Live config restored from SSOT without a backup");
                return Ok(());
            }
            Ok(false) => {
                log::warn!(
                    "{app_type_str} Live backup is missing and SSOT restore is unavailable; attempting takeover placeholder cleanup"
                );
            }
            Err(e) => {
                log::error!(
                    "{app_type_str} Live backup is missing and SSOT restore failed; attempting takeover placeholder cleanup: {e}"
                );
            }
        }

        // 2.2) Final fallback: remove placeholders and local proxy addresses as
        // best we can so Live does not stay stuck in takeover state.
        self.cleanup_takeover_placeholders_in_live_for_app(app_type)?;
        log::info!(
            "{app_type_str} Live takeover placeholders cleaned up without a backup fallback"
        );
        Ok(())
    }

    fn write_live_config_for_app(&self, app_type: &AppType, config: &Value) -> Result<(), String> {
        match app_type {
            AppType::Claude => self.write_claude_live(config),
            AppType::Codex => self.write_codex_live(config),
            AppType::Gemini => self.write_gemini_live(config),
            _ => Err("This app does not support proxy features".to_string()),
        }
    }

    pub fn detect_takeover_in_live_config_for_app(&self, app_type: &AppType) -> bool {
        match app_type {
            AppType::Claude => match self.read_claude_live() {
                Ok(config) => Self::is_claude_live_taken_over(&config),
                Err(_) => false,
            },
            AppType::Codex => match self.read_codex_live() {
                Ok(config) => Self::is_codex_live_taken_over(&config),
                Err(_) => false,
            },
            AppType::Gemini => match self.read_gemini_live() {
                Ok(config) => Self::is_gemini_live_taken_over(&config),
                Err(_) => false,
            },
            _ => false,
        }
    }

    /// When the Live backup is missing, try rewriting Live from SSOT, the current provider,
    /// to release placeholder takeover.
    ///
    /// Returns:
    /// - Ok(true): rewrite succeeded
    /// - Ok(false): rewrite is unavailable because the current provider is missing,
    ///   does not exist, or already contains placeholders
    fn restore_live_from_ssot_for_app(&self, app_type: &AppType) -> Result<bool, String> {
        let current_id = crate::settings::get_effective_current_provider(&self.db, app_type)
            .map_err(|e| format!("Failed to get the current {app_type:?} provider: {e}"))?;

        let Some(current_id) = current_id else {
            return Ok(false);
        };

        let providers = self
            .db
            .get_all_providers(app_type.as_str())
            .map_err(|e| format!("Failed to read the {app_type:?} provider list: {e}"))?;

        let Some(provider) = providers.get(&current_id) else {
            return Ok(false);
        };

        // Do not write provider config back into Live if it already contains
        // takeover placeholders from a bad historical state. That would only
        // freeze the placeholders into Live. Return Ok(false) so the caller
        // falls back to placeholder cleanup.
        if Self::live_has_proxy_placeholder_for_app(app_type, &provider.settings_config) {
            log::warn!(
                "{app_type:?} current provider config contains proxy takeover placeholders, likely leftover from an import during takeover; skipping SSOT rewrite and falling back to placeholder cleanup"
            );
            return Ok(false);
        }

        write_live_with_common_config(self.db.as_ref(), app_type, provider)
            .map_err(|e| format!("Failed to write the {app_type:?} Live config: {e}"))?;

        Ok(true)
    }

    fn cleanup_takeover_placeholders_in_live_for_app(
        &self,
        app_type: &AppType,
    ) -> Result<(), String> {
        match app_type {
            AppType::Claude => self.cleanup_claude_takeover_placeholders_in_live(),
            AppType::Codex => self.cleanup_codex_takeover_placeholders_in_live(),
            AppType::Gemini => self.cleanup_gemini_takeover_placeholders_in_live(),
            _ => Ok(()),
        }
    }

    fn is_local_proxy_url(url: &str) -> bool {
        let url = url.trim();
        if !url.starts_with("http://") {
            return false;
        }
        let rest = &url["http://".len()..];
        rest.starts_with("127.0.0.1")
            || rest.starts_with("localhost")
            || rest.starts_with("0.0.0.0")
            || rest.starts_with("[::1]")
            || rest.starts_with("[::]")
            || rest.starts_with("::1")
            || rest.starts_with("::")
    }

    fn proxy_urls_match(actual: &str, expected: &str) -> bool {
        actual.trim().trim_end_matches('/') == expected.trim().trim_end_matches('/')
    }

    fn codex_config_has_base_url_matching(
        config_text: &str,
        predicate: impl Fn(&str) -> bool,
    ) -> bool {
        let Ok(doc) = toml::from_str::<toml::Value>(config_text) else {
            return false;
        };

        let active_provider = doc
            .get("model_provider")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|id| !id.is_empty());

        if let Some(provider_id) = active_provider {
            if doc
                .get("model_providers")
                .and_then(|value| value.get(provider_id))
                .and_then(|value| value.get("base_url"))
                .and_then(|value| value.as_str())
                .is_some_and(&predicate)
            {
                return true;
            }
        }

        doc.get("base_url")
            .and_then(|value| value.as_str())
            .is_some_and(predicate)
    }

    async fn live_takeover_matches_current_proxy(
        &self,
        app_type: &AppType,
    ) -> Result<bool, String> {
        let (proxy_url, proxy_codex_base_url) = self.build_proxy_urls().await?;

        match app_type {
            AppType::Claude => {
                let config = self.read_claude_live()?;
                let base_url_matches = config
                    .get("env")
                    .and_then(|value| value.get("ANTHROPIC_BASE_URL"))
                    .and_then(|value| value.as_str())
                    .is_some_and(|url| Self::proxy_urls_match(url, &proxy_url));
                Ok(Self::is_claude_live_taken_over(&config) && base_url_matches)
            }
            AppType::Codex => {
                let config = self.read_codex_live()?;
                let base_url_matches = config
                    .get("config")
                    .and_then(|value| value.as_str())
                    .is_some_and(|config_text| {
                        Self::codex_config_has_base_url_matching(config_text, |url| {
                            Self::proxy_urls_match(url, &proxy_codex_base_url)
                        })
                    });
                Ok(Self::codex_live_has_proxy_placeholder(&config) && base_url_matches)
            }
            AppType::Gemini => {
                let config = self.read_gemini_live()?;
                let base_url_matches = config
                    .get("env")
                    .and_then(|value| value.get("GOOGLE_GEMINI_BASE_URL"))
                    .and_then(|value| value.as_str())
                    .is_some_and(|url| Self::proxy_urls_match(url, &proxy_url));
                Ok(Self::is_gemini_live_taken_over(&config) && base_url_matches)
            }
            _ => Ok(false),
        }
    }

    fn cleanup_claude_takeover_placeholders_in_live(&self) -> Result<(), String> {
        let mut config = self.read_claude_live()?;

        let Some(env) = config.get_mut("env").and_then(|v| v.as_object_mut()) else {
            return Ok(());
        };

        for key in [
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "OPENROUTER_API_KEY",
            "OPENAI_API_KEY",
        ] {
            if env.get(key).and_then(|v| v.as_str()) == Some(PROXY_TOKEN_PLACEHOLDER) {
                env.remove(key);
            }
        }

        if env
            .get("ANTHROPIC_BASE_URL")
            .and_then(|v| v.as_str())
            .map(Self::is_local_proxy_url)
            .unwrap_or(false)
        {
            env.remove("ANTHROPIC_BASE_URL");
        }

        self.write_claude_live(&config)?;
        Ok(())
    }

    fn cleanup_codex_takeover_placeholders_in_live(&self) -> Result<(), String> {
        let mut config = self.read_codex_live()?;

        if let Some(auth) = config.get_mut("auth").and_then(|v| v.as_object_mut()) {
            if auth.get("OPENAI_API_KEY").and_then(|v| v.as_str()) == Some(PROXY_TOKEN_PLACEHOLDER)
            {
                auth.remove("OPENAI_API_KEY");
            }
        }

        if let Some(cfg_str) = config.get("config").and_then(|v| v.as_str()) {
            let updated = Self::remove_local_toml_base_url(cfg_str);
            let updated =
                crate::codex_config::remove_codex_experimental_bearer_token_if(&updated, |token| {
                    token == PROXY_TOKEN_PLACEHOLDER
                })
                .map_err(|e| format!("Failed to clean up Codex takeover placeholders: {e}"))?;
            config["config"] = json!(updated);
        }

        self.write_codex_live(&config)?;
        Ok(())
    }

    /// Remove the local proxy base_url from TOML, delegated to the shared codex_config implementation.
    fn remove_local_toml_base_url(toml_str: &str) -> String {
        crate::codex_config::remove_codex_toml_base_url_if(toml_str, Self::is_local_proxy_url)
    }

    fn cleanup_gemini_takeover_placeholders_in_live(&self) -> Result<(), String> {
        let mut config = self.read_gemini_live()?;

        let Some(env) = config.get_mut("env").and_then(|v| v.as_object_mut()) else {
            return Ok(());
        };

        if env.get("GEMINI_API_KEY").and_then(|v| v.as_str()) == Some(PROXY_TOKEN_PLACEHOLDER) {
            env.remove("GEMINI_API_KEY");
        }

        if env
            .get("GOOGLE_GEMINI_BASE_URL")
            .and_then(|v| v.as_str())
            .map(Self::is_local_proxy_url)
            .unwrap_or(false)
        {
            env.remove("GOOGLE_GEMINI_BASE_URL");
        }

        self.write_gemini_live(&config)?;
        Ok(())
    }

    /// Check whether Live takeover is active.
    pub async fn is_takeover_active(&self) -> Result<bool, String> {
        let status = self.get_takeover_status().await?;
        Ok(status.claude || status.codex || status.gemini)
    }

    /// Recover from an abnormal exit during startup.
    ///
    /// Call this when leftover Live backups are detected. It restores Live
    /// config, clears takeover flags, and deletes backups.
    pub async fn recover_from_crash(&self) -> Result<(), String> {
        // 1. Restore Live config.
        self.restore_live_configs().await?;

        // 2. Clear takeover flags.
        self.db
            .set_live_takeover_active(false)
            .await
            .map_err(|e| format!("Failed to clear the takeover state: {e}"))?;

        // 3. Delete backups.
        self.db
            .delete_all_live_backups()
            .await
            .map_err(|e| format!("Failed to delete backups: {e}"))?;

        log::info!("Recovered Live config from an abnormal exit");
        Ok(())
    }

    /// Detect whether Live config is stuck in leftover takeover state.
    ///
    /// This is a fallback signal for startup recovery when the database backup
    /// is missing but the Live files already contain proxy placeholders.
    pub fn detect_takeover_in_live_configs(&self) -> bool {
        if let Ok(config) = self.read_claude_live() {
            if Self::is_claude_live_taken_over(&config) {
                return true;
            }
        }

        if let Ok(config) = self.read_codex_live() {
            if Self::is_codex_live_taken_over(&config) {
                return true;
            }
        }

        if let Ok(config) = self.read_gemini_live() {
            if Self::is_gemini_live_taken_over(&config) {
                return true;
            }
        }

        false
    }

    fn is_claude_live_taken_over(config: &Value) -> bool {
        let env = match config.get("env").and_then(|v| v.as_object()) {
            Some(env) => env,
            None => return false,
        };

        for key in [
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "OPENROUTER_API_KEY",
            "OPENAI_API_KEY",
        ] {
            if env.get(key).and_then(|v| v.as_str()) == Some(PROXY_TOKEN_PLACEHOLDER) {
                return true;
            }
        }

        false
    }

    fn codex_live_has_proxy_placeholder(config: &Value) -> bool {
        if config
            .get("auth")
            .and_then(|v| v.as_object())
            .and_then(|auth| auth.get("OPENAI_API_KEY"))
            .and_then(|v| v.as_str())
            == Some(PROXY_TOKEN_PLACEHOLDER)
        {
            return true;
        }

        config
            .get("config")
            .and_then(|v| v.as_str())
            .and_then(crate::codex_config::extract_codex_experimental_bearer_token)
            .as_deref()
            == Some(PROXY_TOKEN_PLACEHOLDER)
    }

    fn is_codex_live_taken_over(config: &Value) -> bool {
        Self::codex_live_has_proxy_placeholder(config)
    }

    fn is_gemini_live_taken_over(config: &Value) -> bool {
        let env = match config.get("env").and_then(|v| v.as_object()) {
            Some(env) => env,
            None => return false,
        };
        env.get("GEMINI_API_KEY").and_then(|v| v.as_str()) == Some(PROXY_TOKEN_PLACEHOLDER)
    }

    /// Check whether the given Live or backup config has already been taken over by the proxy.
    ///
    /// This detects abnormal historical states where the backup actually stores
    /// proxy config. In that case the backup is untrustworthy: the backup path
    /// must not write it and the restore path must not read it, or proxy
    /// placeholders would be written back into Live unchanged and leave it
    /// permanently stuck on the proxy address. Both cases should fall back to
    /// SSOT rebuild.
    fn live_has_proxy_placeholder_for_app(app_type: &AppType, config: &Value) -> bool {
        match app_type {
            AppType::Claude => Self::is_claude_live_taken_over(config),
            AppType::Codex => Self::codex_live_has_proxy_placeholder(config),
            AppType::Gemini => Self::is_gemini_live_taken_over(config),
            _ => false,
        }
    }

    /// Update the Live backup from provider config during hot switching in proxy mode.
    ///
    /// Unlike backup_live_configs(), this generates the backup from the
    /// provider's settings_config instead of reading Live files, because Live
    /// has already been taken over by the proxy.
    pub async fn update_live_backup_from_provider(
        &self,
        app_type: &str,
        provider: &Provider,
    ) -> Result<(), String> {
        let _guard = self.switch_locks.lock_for_app(app_type).await;
        self.update_live_backup_from_provider_inner(app_type, provider)
            .await
    }

    /// For callers that already hold the per-app switch lock.
    async fn update_live_backup_from_provider_inner(
        &self,
        app_type: &str,
        provider: &Provider,
    ) -> Result<(), String> {
        let app_type_enum =
            AppType::from_str(app_type).map_err(|_| format!("Unknown app type: {app_type}"))?;
        let mut effective_settings =
            build_effective_settings_with_common_config(self.db.as_ref(), &app_type_enum, provider)
                .map_err(|e| format!("Failed to build the effective {app_type} settings: {e}"))?;

        if matches!(app_type_enum, AppType::Codex) {
            effective_settings
                .as_object_mut()
                .ok_or_else(|| "Codex backup must be a JSON object".to_string())?
                .insert(
                    "apiFormat".to_string(),
                    json!(provider
                        .meta
                        .as_ref()
                        .and_then(|meta| meta.api_format.as_deref())
                        .unwrap_or("openai_chat")),
                );
            let existing_backup_value = self
                .db
                .get_live_backup(app_type)
                .await
                .map_err(|e| format!("Failed to read the existing {app_type} backup: {e}"))?
                .map(|backup| {
                    serde_json::from_str::<Value>(&backup.original_config)
                        .map_err(|e| format!("Failed to parse the existing {app_type} backup: {e}"))
                })
                .transpose()?;

            let existing_live = self.read_codex_live().ok();
            if let Some(existing_live) = existing_live.as_ref() {
                crate::codex_config::capture_live_codex_official_auth_snapshot()
                    .map_err(|e| format!("Failed to preserve Codex OAuth snapshot: {e}"))?;
                Self::preserve_codex_user_config_from_existing_config(
                    &mut effective_settings,
                    existing_live,
                )?;
                Self::preserve_codex_oauth_auth_in_backup(&mut effective_settings, existing_live)?;
            }
            if let Some(existing_value) = existing_backup_value.as_ref() {
                Self::preserve_codex_user_config_from_existing_config(
                    &mut effective_settings,
                    existing_value,
                )?;
                Self::preserve_codex_oauth_auth_in_backup(&mut effective_settings, existing_value)?;
            }
            if let Some(saved_auth) = crate::codex_config::read_codex_official_auth_snapshot()
                .map_err(|e| format!("Failed to read preserved Codex OAuth: {e}"))?
            {
                Self::preserve_codex_oauth_auth_in_backup(
                    &mut effective_settings,
                    &json!({ "auth": saved_auth }),
                )?;
            }

            if let Some(authoritative_settings) =
                existing_live.as_ref().or(existing_backup_value.as_ref())
            {
                sync_codex_desktop_table(&mut effective_settings, authoritative_settings)
                    .map_err(|e| format!("Failed to preserve Codex desktop settings: {e}"))?;
            } else {
                sync_codex_desktop_table(&mut effective_settings, &json!({ "config": "" }))
                    .map_err(|e| format!("Failed to clear stale Codex desktop settings: {e}"))?;
            }

            // The backup is the restore source when proxy takeover is released,
            // so it must carry the same shared history route as the live config.
            crate::codex_config::apply_codex_unified_session_bucket_to_settings(
                provider.category.as_deref(),
                &mut effective_settings,
            )
            .map_err(|e| format!("Failed to inject the unified session route: {e}"))?;
        }

        let backup_json = match app_type_enum {
            AppType::Claude => serde_json::to_string(&effective_settings)
                .map_err(|e| format!("Failed to serialize the Claude config: {e}"))?,
            AppType::Codex => serde_json::to_string(&effective_settings)
                .map_err(|e| format!("Failed to serialize the Codex config: {e}"))?,
            AppType::Gemini => {
                // Gemini takeover only changes .env; settings.json, including
                // mcpServers, stays untouched.
                let env_backup = if let Some(env) = effective_settings.get("env") {
                    json!({ "env": env })
                } else {
                    json!({ "env": {} })
                };
                serde_json::to_string(&env_backup)
                    .map_err(|e| format!("Failed to serialize the Gemini config: {e}"))?
            }
            _ => return Err(format!("Unknown app type: {app_type}")),
        };

        self.db
            .save_live_backup(app_type, &backup_json)
            .await
            .map_err(|e| format!("Failed to update the {app_type} backup: {e}"))?;

        log::info!("Updated the {app_type} Live backup during hot switch");
        Ok(())
    }

    pub async fn hot_switch_provider(
        &self,
        app_type: &str,
        provider_id: &str,
    ) -> Result<HotSwitchOutcome, String> {
        let _guard = self.switch_locks.lock_for_app(app_type).await;
        self.hot_switch_provider_inner(app_type, provider_id).await
    }

    pub(crate) async fn hot_switch_provider_inner(
        &self,
        app_type: &str,
        provider_id: &str,
    ) -> Result<HotSwitchOutcome, String> {
        let app_type_enum =
            AppType::from_str(app_type).map_err(|_| format!("Invalid app type: {app_type}"))?;
        let provider = self
            .db
            .get_provider_by_id(provider_id, app_type)
            .map_err(|e| format!("Failed to read the provider: {e}"))?
            .ok_or_else(|| format!("Provider does not exist: {provider_id}"))?;

        // Defense-in-depth: block official providers during proxy takeover
        if provider.category.as_deref() == Some("official") {
            return Err("Cannot switch to an official provider during proxy takeover".to_string());
        }

        let logical_target_changed =
            crate::settings::get_effective_current_provider(&self.db, &app_type_enum)
                .map_err(|e| format!("Failed to read the current provider: {e}"))?
                .as_deref()
                != Some(provider_id);

        let has_backup = self
            .db
            .get_live_backup(app_type_enum.as_str())
            .await
            .map_err(|e| format!("Failed to read the {app_type} backup: {e}"))?
            .is_some();
        let live_taken_over = self.detect_takeover_in_live_config_for_app(&app_type_enum);
        let should_sync_backup = has_backup || live_taken_over;

        let previous_local_provider = crate::settings::get_current_provider(&app_type_enum);
        crate::settings::set_current_provider(&app_type_enum, Some(provider_id))
            .map_err(|e| format!("Failed to update the local current provider: {e}"))?;
        if let Err(error) = self
            .db
            .set_current_provider(app_type_enum.as_str(), provider_id)
        {
            let rollback = crate::settings::set_current_provider(
                &app_type_enum,
                previous_local_provider.as_deref(),
            );
            return Err(match rollback {
                Ok(()) => format!("Failed to update the current provider: {error}"),
                Err(rollback_error) => {
                    format!(
                        "Failed to update the current provider: {error}; \
                         failed to restore the local current provider: {rollback_error}"
                    )
                }
            });
        }

        if should_sync_backup {
            self.update_live_backup_from_provider_inner(app_type, &provider)
                .await?;

            if matches!(app_type_enum, AppType::Claude) {
                self.sync_claude_live_from_provider_while_proxy_active(&provider)
                    .await?;
            } else if live_taken_over && matches!(app_type_enum, AppType::Codex) {
                self.sync_codex_live_from_provider_while_proxy_active(&provider)
                    .await?;
            }
        }

        if has_backup && !live_taken_over && matches!(app_type_enum, AppType::Codex) {
            let mut effective_settings = build_effective_settings_with_common_config(
                self.db.as_ref(),
                &AppType::Codex,
                &provider,
            )
            .map_err(|e| format!("Failed to build the effective Codex settings: {e}"))?;
            let live_settings = self.read_codex_live()?;
            sync_codex_desktop_table(&mut effective_settings, &live_settings)
                .map_err(|e| format!("Failed to preserve Codex desktop settings: {e}"))?;
            let auth = effective_settings
                .get("auth")
                .ok_or_else(|| "Codex provider is missing auth configuration".to_string())?;
            let config_str = effective_settings.get("config").and_then(|v| v.as_str());
            let profile = crate::codex_config::CodexCatalogToolProfile::from_api_format(
                provider.meta.as_ref().and_then(|m| m.api_format.as_deref()),
            );

            crate::codex_config::write_codex_provider_live_with_catalog(
                &effective_settings,
                provider.category.as_deref(),
                auth,
                config_str,
                profile,
            )
            .map_err(|e| format!("Failed to write the Codex config: {e}"))?;
        }

        if let Some(server) = self.server.read().await.as_ref() {
            server
                .set_active_target(app_type_enum.as_str(), &provider.id, &provider.name)
                .await;
        }

        Ok(HotSwitchOutcome {
            logical_target_changed,
        })
    }

    #[cfg(test)]
    async fn lock_switch_for_test(&self, app_type: &str) -> tokio::sync::OwnedMutexGuard<()> {
        self.switch_locks.lock_for_app(app_type).await
    }

    fn preserve_codex_user_config_from_existing_config(
        target_settings: &mut Value,
        existing_config: &Value,
    ) -> Result<(), String> {
        let target_obj = target_settings
            .as_object_mut()
            .ok_or_else(|| "Codex backup must be a JSON object".to_string())?;

        let target_config = target_obj
            .get("config")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let mut target_doc = if target_config.trim().is_empty() {
            toml_edit::DocumentMut::new()
        } else {
            target_config
                .parse::<toml_edit::DocumentMut>()
                .map_err(|e| format!("Failed to parse the new Codex config.toml: {e}"))?
        };

        let existing_config = existing_config
            .get("config")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if existing_config.trim().is_empty() {
            target_obj.insert("config".to_string(), json!(target_doc.to_string()));
            return Ok(());
        }

        let existing_doc = existing_config
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| format!("Failed to parse the existing Codex backup: {e}"))?;

        for (key, item) in existing_doc.as_table().iter() {
            if target_doc.get(key).is_none() {
                target_doc.insert(key, item.clone());
            }
        }

        if let Some(existing_mcp_servers) = existing_doc.get("mcp_servers") {
            match target_doc.get_mut("mcp_servers") {
                Some(target_mcp_servers) => {
                    if let (Some(target_table), Some(existing_table)) = (
                        target_mcp_servers.as_table_like_mut(),
                        existing_mcp_servers.as_table_like(),
                    ) {
                        for (server_id, server_item) in existing_table.iter() {
                            if target_table.get(server_id).is_none() {
                                target_table.insert(server_id, server_item.clone());
                            }
                        }
                    } else {
                        log::warn!(
                            "Codex config contains a non-table mcp_servers section; skipping MCP merge"
                        );
                    }
                }
                None => {
                    target_doc["mcp_servers"] = existing_mcp_servers.clone();
                }
            }
        }

        target_obj.insert("config".to_string(), json!(target_doc.to_string()));
        Ok(())
    }

    fn preserve_codex_oauth_auth_in_backup(
        target_settings: &mut Value,
        existing_backup: &Value,
    ) -> Result<(), String> {
        if !crate::settings::preserve_codex_official_auth_on_switch() {
            return Ok(());
        }

        let Some(existing_auth) = existing_backup
            .get("auth")
            .filter(|auth| crate::codex_config::codex_auth_has_oauth_login_material(auth))
            .cloned()
        else {
            return Ok(());
        };

        let Some(target_obj) = target_settings.as_object_mut() else {
            return Ok(());
        };

        let provider_auth = target_obj.get("auth").cloned().unwrap_or_else(|| json!({}));
        if let Some(config_text) = target_obj.get("config").and_then(|value| value.as_str()) {
            let live_config = crate::codex_config::prepare_codex_provider_live_config(
                &provider_auth,
                config_text,
            )
            .map_err(|e| format!("Failed to update the Codex backup config: {e}"))?;
            target_obj.insert("config".to_string(), json!(live_config));
        }
        target_obj.insert("auth".to_string(), existing_auth);

        Ok(())
    }

    /// Switch providers in proxy mode by hot-switching and refreshing proxy-safe Live display fields when needed.
    pub async fn switch_proxy_target(
        &self,
        app_type: &str,
        provider_id: &str,
    ) -> Result<(), String> {
        let outcome = self.hot_switch_provider(app_type, provider_id).await?;

        if outcome.logical_target_changed {
            log::info!("Proxy mode: switched the target provider for {app_type} to {provider_id}");
        } else {
            log::debug!(
                "Proxy mode: {app_type} is already aligned with target provider {provider_id}"
            );
        }
        Ok(())
    }

    // ==================== Live config read/write helpers ====================

    /// Update base_url in a TOML string, delegated to the shared codex_config implementation.
    fn update_toml_base_url(toml_str: &str, new_url: &str) -> String {
        crate::codex_config::update_codex_toml_field(toml_str, "base_url", new_url)
            .unwrap_or_else(|_| toml_str.to_string())
    }

    fn nexus_codex_provider_display_name(provider: &Provider) -> Option<&'static str> {
        let is_nexus = provider
            .meta
            .as_ref()
            .and_then(|meta| meta.provider_type.as_deref())
            == Some("nexus");
        is_nexus.then_some("Nexus")
    }

    fn repair_nexus_codex_provider_selector(toml_str: &str) -> String {
        let Ok(mut doc) = toml_str.parse::<toml_edit::DocumentMut>() else {
            return toml_str.to_string();
        };
        let Some(selector_key) = doc
            .get("model_provider")
            .and_then(|item| item.as_str())
            .filter(|value| *value != "nexus" && value.eq_ignore_ascii_case("nexus"))
            .map(str::to_string)
        else {
            return toml_str.to_string();
        };
        let Some(providers) = doc
            .get_mut("model_providers")
            .and_then(|item| item.as_table_mut())
        else {
            return toml_str.to_string();
        };
        if !providers.contains_key("nexus") {
            return toml_str.to_string();
        }
        if let Some(case_variant) = providers
            .get(&selector_key)
            .and_then(|item| item.as_table())
            .cloned()
        {
            if let Some(canonical) = providers
                .get_mut("nexus")
                .and_then(|item| item.as_table_mut())
            {
                for (key, item) in &case_variant {
                    canonical.insert(key, item.clone());
                }
            }
            providers.remove(&selector_key);
        }

        let Some(selector) = doc
            .get_mut("model_provider")
            .and_then(|item| item.as_value_mut())
        else {
            return toml_str.to_string();
        };
        let decor = selector.decor().clone();
        *selector = toml_edit::Value::from("nexus");
        *selector.decor_mut() = decor;
        doc.to_string()
    }

    /// During Codex takeover, the local client must keep using the Responses wire API against the proxy.
    /// Whether the real upstream uses Chat Completions is decided by provider config and converted inside the proxy.
    fn apply_codex_proxy_toml_config_for_provider(
        toml_str: &str,
        proxy_url: &str,
        provider: Option<&Provider>,
    ) -> String {
        let nexus_display_name = provider.and_then(Self::nexus_codex_provider_display_name);
        let updated = nexus_display_name
            .map(|_| Self::repair_nexus_codex_provider_selector(toml_str))
            .unwrap_or_else(|| toml_str.to_string());
        let updated = Self::update_toml_base_url(&updated, proxy_url);
        let mut updated =
            crate::codex_config::update_codex_toml_field(&updated, "wire_api", "responses")
                .unwrap_or(updated);

        if let Some(upstream_model) =
            provider.and_then(crate::proxy::providers::codex_provider_upstream_model)
        {
            updated =
                crate::codex_config::update_codex_toml_field(&updated, "model", &upstream_model)
                    .unwrap_or(updated);
        }

        if let Some(display_name) = nexus_display_name {
            updated = crate::codex_config::update_codex_toml_field(&updated, "name", display_name)
                .unwrap_or(updated);
        }

        updated
    }

    fn attach_codex_model_catalog_from_provider(
        live_config: &mut Value,
        provider: Option<&Provider>,
    ) {
        let Some(provider) = provider else {
            return;
        };

        let model_catalog = provider
            .settings_config
            .get("modelCatalog")
            .cloned()
            .unwrap_or_else(|| json!({ "models": [] }));

        if let Some(root) = live_config.as_object_mut() {
            root.insert("modelCatalog".to_string(), model_catalog);
        }
    }

    fn read_claude_live(&self) -> Result<Value, String> {
        let path = get_claude_settings_path();
        if !path.exists() {
            return Err("Claude config file does not exist".to_string());
        }

        let mut value: Value =
            read_json_file(&path).map_err(|e| format!("Failed to read the Claude config: {e}"))?;

        if value.is_null() {
            value = json!({});
        }

        if !value.is_object() {
            let kind = match &value {
                Value::Null => "null",
                Value::Bool(_) => "boolean",
                Value::Number(_) => "number",
                Value::String(_) => "string",
                Value::Array(_) => "array",
                Value::Object(_) => "object",
            };
            return Err(format!(
                "Claude config file has an invalid format: the root node must be a JSON object, but is currently {kind}. Path: {}",
                path.display()
            ));
        }

        Ok(value)
    }

    fn write_claude_live(&self, config: &Value) -> Result<(), String> {
        let path = get_claude_settings_path();
        let settings = crate::services::provider::sanitize_claude_settings_for_live(config);
        write_json_file(&path, &settings)
            .map_err(|e| format!("Failed to write the Claude config: {e}"))
    }

    fn read_codex_live(&self) -> Result<Value, String> {
        crate::codex_config::read_codex_live_settings()
            .map_err(|e| format!("Failed to read the Codex Live config: {e}"))
    }

    fn write_codex_live(&self, config: &Value) -> Result<(), String> {
        self.write_codex_live_verbatim(config)
    }

    fn write_codex_live_for_provider(
        &self,
        config: &Value,
        provider: Option<&Provider>,
    ) -> Result<(), String> {
        let Some(provider) = provider else {
            if crate::settings::preserve_codex_official_auth_on_switch() {
                if let (Some(auth), Some(config_str)) = (
                    config.get("auth"),
                    config.get("config").and_then(|v| v.as_str()),
                ) {
                    if auth.get("OPENAI_API_KEY").and_then(|v| v.as_str())
                        == Some(PROXY_TOKEN_PLACEHOLDER)
                    {
                        let live_config = crate::codex_config::prepare_codex_provider_live_config(
                            auth, config_str,
                        )
                        .map_err(|e| format!("Failed to write the Codex config: {e}"))?;
                        crate::codex_config::write_codex_live_config_atomic(Some(&live_config))
                            .map_err(|e| format!("Failed to write the Codex config: {e}"))?;
                        return Ok(());
                    }
                }
            }

            return self.write_codex_live_verbatim(config);
        };

        let auth = config
            .get("auth")
            .ok_or_else(|| "Codex config is missing the auth field".to_string())?;
        let config_str = config.get("config").and_then(|v| v.as_str());
        let profile = crate::codex_config::CodexCatalogToolProfile::from_api_format(
            provider.meta.as_ref().and_then(|m| m.api_format.as_deref()),
        );

        crate::codex_config::write_codex_provider_live_with_catalog(
            config,
            provider.category.as_deref(),
            auth,
            config_str,
            profile,
        )
        .map_err(|e| format!("Failed to write the Codex config: {e}"))
    }

    fn codex_auth_has_proxy_placeholder(auth: &Value) -> bool {
        auth.get("OPENAI_API_KEY").and_then(|v| v.as_str()) == Some(PROXY_TOKEN_PLACEHOLDER)
    }

    fn write_codex_takeover_live_for_provider(
        &self,
        config: &Value,
        provider: Option<&Provider>,
    ) -> Result<(), String> {
        if crate::settings::preserve_codex_official_auth_on_switch() {
            if let Some(auth) = config
                .get("auth")
                .filter(|auth| Self::codex_auth_has_proxy_placeholder(auth))
            {
                let config_str = config.get("config").and_then(|v| v.as_str()).unwrap_or("");
                let profile = crate::codex_config::CodexCatalogToolProfile::from_api_format(
                    provider.and_then(|p| p.meta.as_ref()?.api_format.as_deref()),
                );
                let prepared_config =
                    crate::codex_config::prepare_codex_live_config_text_with_optional_catalog(
                        config, config_str, profile,
                    )
                    .map_err(|e| format!("Failed to write the Codex config: {e}"))?;
                let prepared_config = if let Some(provider) = provider {
                    crate::codex_config::apply_codex_unified_session_bucket_to_config(
                        provider.category.as_deref(),
                        &prepared_config,
                    )
                    .map_err(|e| format!("Failed to write Codex config: {e}"))?
                } else {
                    prepared_config
                };
                let live_config =
                    crate::codex_config::prepare_codex_provider_live_config(auth, &prepared_config)
                        .map_err(|e| format!("Failed to write the Codex config: {e}"))?;
                crate::codex_config::write_codex_live_config_atomic(Some(&live_config))
                    .map_err(|e| format!("Failed to write the Codex config: {e}"))?;
                return Ok(());
            }
        }

        self.write_codex_live_for_provider(config, provider)
    }

    fn write_codex_live_verbatim(&self, config: &Value) -> Result<(), String> {
        use crate::codex_config::{get_codex_auth_path, get_codex_config_path};

        let auth = config.get("auth");
        let config_str = config.get("config").and_then(|v| v.as_str());

        // Decide the config.toml text ONCE, before splitting on auth. A stored
        // Codex backup comes in two shapes needing opposite handling:
        //  - snapshot backup (`read_codex_live_settings`): no inline `modelCatalog`;
        //    the config text already carries the live `model_catalog_json` pointer
        //    → keep raw, or projection would strip it.
        //  - provider-rebuilt backup (`update_live_backup_from_provider`): inline
        //    `modelCatalog` (DB SSOT) with a pointer-less config text → project,
        //    or the mapping is lost on restore.
        // The projection decision is orthogonal to auth: a provider-rebuilt backup
        // can pair an inline `modelCatalog` with empty/absent `auth.json` (the key
        // living in the config's `experimental_bearer_token`). Computing it up here
        // keeps every config-writing branch — write-auth, delete-auth, no-auth —
        // consistent instead of letting the empty-auth path skip projection.
        // Provider-rebuilt backups carry apiFormat. Legacy backups do not carry
        // enough provider identity to infer the tool profile safely, so retain
        // the historical ProxyChat fallback for them.
        let profile = config
            .get("apiFormat")
            .and_then(Value::as_str)
            .map(|api_format| {
                crate::codex_config::CodexCatalogToolProfile::from_api_format(Some(api_format))
            })
            .unwrap_or(crate::codex_config::CodexCatalogToolProfile::ProxyChat);
        let prepared_cfg = config_str
            .map(|cfg| {
                crate::codex_config::prepare_codex_live_config_text_with_optional_catalog(
                    config, cfg, profile,
                )
            })
            .transpose()
            .map_err(|e| format!("Failed to write the Codex config: {e}"))?;
        let preserve_live_oauth =
            auth.is_some_and(crate::codex_config::should_preserve_live_codex_oauth_for_restore);

        match (auth, prepared_cfg.as_deref()) {
            (Some(auth), Some(cfg)) => {
                let auth_path = get_codex_auth_path();
                if preserve_live_oauth {
                    let config_path = get_codex_config_path();
                    crate::config::write_text_file(&config_path, cfg)
                        .map_err(|e| format!("Failed to write Codex config: {e}"))?;
                } else if auth.as_object().is_some_and(|obj| obj.is_empty()) {
                    let _ = crate::config::delete_file(&auth_path);
                    let config_path = get_codex_config_path();
                    crate::config::write_text_file(&config_path, cfg)
                        .map_err(|e| format!("Failed to write Codex config: {e}"))?;
                } else if crate::codex_config::codex_stored_auth_regresses_live_on_disk(auth) {
                    // The ChatGPT login stored in the backup is older than the
                    // one on disk, or belongs to another account. This can
                    // happen if the user logs in again or changes subscription
                    // during takeover. Restore only config so stale workspace
                    // credentials are not written back.
                    let config_path = get_codex_config_path();
                    crate::config::write_text_file(&config_path, cfg)
                        .map_err(|e| format!("Failed to write Codex config: {e}"))?;
                } else {
                    crate::codex_config::write_codex_live_atomic(auth, Some(cfg))
                        .map_err(|e| format!("Failed to write the Codex config: {e}"))?;
                }
            }
            (Some(auth), None) => {
                if !preserve_live_oauth
                    && !crate::codex_config::codex_stored_auth_regresses_live_on_disk(auth)
                {
                    let auth_path = get_codex_auth_path();
                    write_json_file(&auth_path, auth)
                        .map_err(|e| format!("Failed to write Codex auth: {e}"))?;
                }
            }
            (None, Some(cfg)) => {
                let config_path = get_codex_config_path();
                crate::config::write_text_file(&config_path, cfg)
                    .map_err(|e| format!("Failed to write Codex config: {e}"))?;
            }
            (None, None) => {}
        }

        Ok(())
    }

    fn read_gemini_live(&self) -> Result<Value, String> {
        use crate::gemini_config::{env_to_json, get_gemini_env_path, read_gemini_env};

        let env_path = get_gemini_env_path();
        if !env_path.exists() {
            return Err("Gemini .env file does not exist".to_string());
        }

        let env_map = read_gemini_env().map_err(|e| format!("Failed to read Gemini env: {e}"))?;
        Ok(env_to_json(&env_map))
    }

    fn write_gemini_live(&self, config: &Value) -> Result<(), String> {
        use crate::gemini_config::{json_to_env, write_gemini_env_atomic};

        let env_map =
            json_to_env(config).map_err(|e| format!("Failed to convert the Gemini config: {e}"))?;
        write_gemini_env_atomic(&env_map)
            .map_err(|e| format!("Failed to write Gemini env: {e}"))?;
        Ok(())
    }

    // ==================== Existing methods ====================

    /// Get server status.
    pub async fn get_status(&self) -> Result<ProxyStatus, String> {
        if let Some(server) = self.server.read().await.as_ref() {
            Ok(server.get_status().await)
        } else {
            // Return the default status when the server is not running.
            Ok(ProxyStatus {
                running: false,
                ..Default::default()
            })
        }
    }

    /// Get proxy configuration.
    pub async fn get_config(&self) -> Result<ProxyConfig, String> {
        self.db
            .get_proxy_config()
            .await
            .map_err(|e| format!("Failed to get the proxy configuration: {e}"))
    }

    /// Update proxy configuration.
    pub async fn update_config(&self, config: &ProxyConfig) -> Result<(), String> {
        // Keep the old config so we can decide whether a restart is required.
        let previous = self
            .db
            .get_proxy_config()
            .await
            .map_err(|e| format!("Failed to get the proxy configuration: {e}"))?;

        // Save to the database while preserving live_takeover_active.
        let mut new_config = config.clone();
        new_config.live_takeover_active = previous.live_takeover_active;

        self.db
            .update_proxy_config(new_config.clone())
            .await
            .map_err(|e| format!("Failed to save the proxy configuration: {e}"))?;

        // Inspect current server state.
        let mut server_guard = self.server.write().await;
        if server_guard.is_none() {
            return Ok(());
        }

        // Restart only if address or port changed.
        let require_restart = new_config.listen_address != previous.listen_address
            || new_config.listen_port != previous.listen_port;

        if require_restart {
            if let Some(server) = server_guard.take() {
                server
                    .stop()
                    .await
                    .map_err(|e| format!("Failed to stop the proxy server before restart: {e}"))?;
            }

            let app_handle = self.app_handle.read().await.clone();
            let new_server = ProxyServer::new(new_config.clone(), self.db.clone(), app_handle);
            let info = new_server
                .start()
                .await
                .map_err(|e| format!("Failed to restart the proxy server: {e}"))?;
            if let Err(e) = self
                .persist_ephemeral_listen_port_if_needed(&new_config, info.port)
                .await
            {
                let _ = new_server.stop().await;
                return Err(e);
            }

            *server_guard = Some(new_server);
            log::info!("Proxy configuration updated and the server automatically restarted with the latest settings");

            // If any app is currently taken over, update the proxy address in
            // its Live config as well, or the client will still point to the old port.
            drop(server_guard);
            if let Ok(takeover) = self.get_takeover_status().await {
                let mut updated_any = false;

                if takeover.claude {
                    self.takeover_live_config_best_effort(&AppType::Claude)
                        .await?;
                    updated_any = true;
                }
                if takeover.codex {
                    self.takeover_live_config_best_effort(&AppType::Codex)
                        .await?;
                    updated_any = true;
                }
                if takeover.gemini {
                    self.takeover_live_config_best_effort(&AppType::Gemini)
                        .await?;
                    updated_any = true;
                }

                if updated_any {
                    log::info!("Updated proxy addresses in Live config");
                }
            }

            return Ok(());
        } else if let Some(server) = server_guard.as_ref() {
            server.apply_runtime_config(&new_config).await;
            log::info!(
                "Proxy configuration applied at runtime without restarting the proxy server"
            );
        }

        Ok(())
    }

    /// Check whether the server is running.
    pub async fn is_running(&self) -> bool {
        self.server.read().await.is_some()
    }

    pub(crate) async fn reconcile_active_target_for_app(
        &self,
        app_type: &str,
        target: Option<(&str, &str)>,
    ) {
        if let Some(server) = self.server.read().await.as_ref() {
            if let Some((provider_id, provider_name)) = target {
                server
                    .set_active_target(app_type, provider_id, provider_name)
                    .await;
            } else {
                server.clear_active_target(app_type).await;
            }
        }
    }

    /// Hot-update circuit breaker configuration.
    ///
    /// If the proxy server is running, apply the new config to all existing circuit breaker instances.
    pub async fn update_circuit_breaker_configs(
        &self,
        config: crate::proxy::CircuitBreakerConfig,
    ) -> Result<(), String> {
        if let Some(server) = self.server.read().await.as_ref() {
            server.update_circuit_breaker_configs(config).await;
            log::info!("Hot-updated circuit breaker config for the running proxy");
        } else {
            log::debug!("Proxy server is not running; circuit breaker config will apply on the next startup");
        }
        Ok(())
    }

    /// Hot-update circuit breaker configuration for a specific app.
    pub async fn update_circuit_breaker_config_for_app(
        &self,
        app_type: &str,
        config: crate::proxy::CircuitBreakerConfig,
    ) -> Result<(), String> {
        if let Some(server) = self.server.read().await.as_ref() {
            server
                .update_circuit_breaker_config_for_app(app_type, config)
                .await;
            log::info!("Hot-updated running circuit breaker config for {app_type}");
        } else {
            log::debug!("{app_type} circuit breaker config will apply on the next proxy startup");
        }
        Ok(())
    }

    /// Reset the circuit breaker for a specific provider.
    ///
    /// If the proxy server is running, immediately reset the in-memory circuit breaker state.
    pub async fn reset_provider_circuit_breaker(
        &self,
        provider_id: &str,
        app_type: &str,
    ) -> Result<(), String> {
        if let Some(server) = self.server.read().await.as_ref() {
            server
                .reset_provider_circuit_breaker(provider_id, app_type)
                .await;
            log::info!("Reset the circuit breaker for provider {provider_id} (app: {app_type})");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{ClaudeDesktopModelRoute, LocalProxyRequestOverrides, ProviderMeta};
    use serial_test::serial;
    use std::{
        env,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Mutex,
        },
    };
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

    fn assert_env_str(env: &Map<String, Value>, key: &str, expected: Option<&str>) {
        assert_eq!(env.get(key).and_then(|value| value.as_str()), expected);
    }

    async fn use_ephemeral_proxy_port(db: &Arc<Database>) {
        let mut proxy_config = db.get_proxy_config().await.expect("get test proxy config");
        proxy_config.listen_port = 0;
        db.update_proxy_config(proxy_config)
            .await
            .expect("set test proxy config to an ephemeral port");
    }

    #[tokio::test]
    #[serial]
    async fn codex_chat_bridge_merges_narration_into_history_tool_calls() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let request_count = Arc::new(AtomicUsize::new(0));
        let upstream_app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post({
                let requests = Arc::clone(&requests);
                let request_count = Arc::clone(&request_count);
                move |axum::Json(body): axum::Json<Value>| {
                    let requests = Arc::clone(&requests);
                    let request_count = Arc::clone(&request_count);
                    async move {
                        requests.lock().expect("capture Chat request").push(body);
                        request_count.fetch_add(1, Ordering::SeqCst);
                        (
                            [(
                                axum::http::header::CONTENT_TYPE,
                                "text/event-stream",
                            )],
                            concat!(
                                "data: {\"id\":\"chatcmpl_done\",\"model\":\"kimi-k3\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"All done.\"},\"finish_reason\":\"stop\"}]}\n\n",
                                "data: [DONE]\n\n"
                            ),
                        )
                    }
                }
            }),
        );
        let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock Chat upstream");
        let upstream_address = upstream_listener
            .local_addr()
            .expect("read mock Chat upstream address");
        let upstream_task = tokio::spawn(async move {
            axum::serve(upstream_listener, upstream_app)
                .await
                .expect("serve mock Chat upstream");
        });

        let db = Arc::new(Database::memory().expect("init db"));
        let state = crate::store::AppState::new(db.clone());
        use_ephemeral_proxy_port(&db).await;
        let mut provider = Provider::with_id(
            "kimi-chat-loop".to_string(),
            "Nexus Kimi-K3".to_string(),
            json!({
                "base_url": format!("http://{upstream_address}/v1"),
                "apiKey": "test-upstream-token",
                "model": "kimi-k3",
                "modelCatalog": {
                    "models": [{
                        "model": "kimi-k3",
                        "inputModalities": ["text"]
                    }]
                }
            }),
            None,
        );
        provider.meta = Some(ProviderMeta {
            api_format: Some("openai_chat".to_string()),
            provider_type: Some("nexus".to_string()),
            ..Default::default()
        });
        db.save_provider(AppType::Codex.as_str(), &provider)
            .expect("save Kimi Chat provider");
        db.set_current_provider(AppType::Codex.as_str(), &provider.id)
            .expect("select Kimi Chat provider");
        crate::settings::set_current_provider(&AppType::Codex, Some(&provider.id))
            .expect("select local Kimi Chat provider");

        let proxy = state.proxy_service.start().await.expect("start proxy");
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("build test client");
        let response = client
            .post(format!("http://127.0.0.1:{}/v1/responses", proxy.port))
            .bearer_auth(PROXY_TOKEN_PLACEHOLDER)
            .json(&json!({
                "model": "kimi-k3",
                "instructions": "Keep normal commentary while completing the task.",
                "input": [
                    {
                        "type": "message",
                        "role": "user",
                        "content": [{"type": "input_text", "text": "Inspect the provider."}]
                    },
                    {
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": "Checking the provider now."}]
                    },
                    {
                        "type": "function_call",
                        "call_id": "call_1",
                        "name": "exec",
                        "arguments": "{}"
                    },
                    {
                        "type": "function_call_output",
                        "call_id": "call_1",
                        "output": "ok"
                    }
                ],
                "tools": [{
                    "type": "function",
                    "name": "exec",
                    "description": "Run a command",
                    "parameters": {"type": "object", "properties": {}}
                }],
                "stream": true
            }))
            .send()
            .await
            .expect("send Codex Responses request");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let downstream = response.text().await.expect("read Responses stream");

        state.proxy_service.stop().await.expect("stop proxy");
        upstream_task.abort();

        assert_eq!(downstream.matches("event: response.completed").count(), 1);
        assert_eq!(request_count.load(Ordering::SeqCst), 1);

        let requests = requests.lock().expect("read captured Chat requests");
        assert_eq!(requests.len(), 1);
        let messages = requests[0]["messages"].as_array().expect("chat messages");
        let assistant_tool_calls: Vec<&Value> = messages
            .iter()
            .filter(|message| {
                message["role"] == "assistant"
                    && message["tool_calls"]
                        .as_array()
                        .is_some_and(|calls| !calls.is_empty())
            })
            .collect();
        assert_eq!(assistant_tool_calls.len(), 1);
        assert_eq!(
            assistant_tool_calls[0]["content"], "Checking the provider now.",
            "mid-turn narration must ride on the assistant tool_calls message"
        );
        let bare_narration = messages.iter().any(|message| {
            message["role"] == "assistant"
                && message.get("tool_calls").is_none()
                && message["content"] == "Checking the provider now."
        });
        assert!(!bare_narration, "no bare narration-only assistant messages");
        assert_eq!(
            messages[0]["content"],
            "Keep normal commentary while completing the task."
        );
        let serialized_requests =
            serde_json::to_string(&*requests).expect("serialize captured requests");
        assert!(!serialized_requests.contains("MUST NOT send narration"));
        assert!(!serialized_requests.contains("Agent-loop control"));
    }

    async fn running_codex_base_url(service: &ProxyService) -> String {
        let status = service.get_status().await.expect("get proxy status");
        format!("http://127.0.0.1:{}/v1", status.port)
    }

    fn select_invalid_desktop_provider(db: &Arc<Database>) {
        let mut provider = Provider::with_id(
            "invalid-desktop".to_string(),
            "Invalid Desktop provider".to_string(),
            json!({"env": {"ANTHROPIC_BASE_URL": "https://example.com/v1"}}),
            None,
        );
        provider.meta = Some(ProviderMeta {
            claude_desktop_mode: Some(crate::provider::ClaudeDesktopMode::Proxy),
            ..Default::default()
        });
        db.save_provider(AppType::ClaudeDesktop.as_str(), &provider)
            .expect("save invalid Desktop provider");
        db.set_current_provider(AppType::ClaudeDesktop.as_str(), &provider.id)
            .expect("select invalid Desktop provider");
        crate::settings::set_current_provider(&AppType::ClaudeDesktop, Some(&provider.id))
            .expect("select local Desktop provider");
    }

    fn select_desktop_proxy_provider(db: &Arc<Database>) {
        let mut provider = Provider::with_id(
            "desktop".to_string(),
            "Nexus GLM-5.2".to_string(),
            json!({"env": {
                "ANTHROPIC_BASE_URL": "https://my-tenant-2-glm52-sonle-tp4.onenexus-do.cloud/v1",
                "ANTHROPIC_AUTH_TOKEN": "test-token"
            }}),
            None,
        );
        provider.meta = Some(ProviderMeta {
            claude_desktop_mode: Some(crate::provider::ClaudeDesktopMode::Proxy),
            api_format: Some("openai_chat".to_string()),
            provider_type: Some("nexus".to_string()),
            managed_nexus_preset_version: Some(7),
            claude_desktop_model_routes: std::collections::HashMap::from([(
                "claude-sonnet-5".to_string(),
                crate::provider::ClaudeDesktopModelRoute {
                    model: "GLM-5.2-FP8".to_string(),
                    label_override: None,
                    supports_1m: Some(true),
                },
            )]),
            ..Default::default()
        });
        db.save_provider(AppType::ClaudeDesktop.as_str(), &provider)
            .expect("save Desktop provider");
        db.set_current_provider(AppType::ClaudeDesktop.as_str(), &provider.id)
            .expect("select Desktop provider");
        crate::settings::set_current_provider(&AppType::ClaudeDesktop, Some(&provider.id))
            .expect("select local Desktop provider");
    }

    #[cfg(any(target_os = "macos", windows))]
    #[tokio::test]
    #[serial]
    async fn manual_start_isolated_from_mcp_sync_and_restores_desktop_profile() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let state = crate::store::AppState::new(db.clone());
        use_ephemeral_proxy_port(&db).await;
        let global = db
            .get_global_proxy_config()
            .await
            .expect("get global proxy config");
        assert!(!global.proxy_enabled);

        select_desktop_proxy_provider(&db);

        std::fs::write(crate::config::get_default_claude_mcp_path(), "{invalid")
            .expect("seed unrelated invalid MCP config");
        db.save_mcp_server(&crate::app_config::McpServer {
            id: "unrelated".to_string(),
            name: "Unrelated MCP".to_string(),
            server: json!({"command": "true"}),
            apps: crate::app_config::McpApps {
                claude: true,
                ..Default::default()
            },
            description: None,
            homepage: None,
            docs: None,
            tags: Vec::new(),
        })
        .expect("save MCP fixture");
        assert!(
            crate::services::mcp::McpService::sync_all_enabled(&state).is_err(),
            "fixture must prove unrelated MCP synchronization is failing"
        );

        crate::services::provider::ProviderService::sync_current_provider_for_app(
            &state,
            AppType::ClaudeDesktop,
        )
        .expect_err("unresolved port must fail the first profile apply");

        crate::commands::start_proxy_server_inner(&state)
            .await
            .expect("manually start proxy and retry Desktop profile");

        assert!(state.proxy_service.is_running().await);
        let config = db
            .get_proxy_config()
            .await
            .expect("get resolved proxy config");
        assert_ne!(config.listen_port, 0);
        let status = crate::claude_desktop_config::get_status(&db, true)
            .expect("read Desktop profile status");
        assert!(status.configured);
        assert_eq!(status.actual_base_url, status.expected_base_url);

        state
            .proxy_service
            .stop_with_restore_keep_state()
            .await
            .expect("gracefully stop proxy");
        assert!(
            db.get_global_proxy_config()
                .await
                .expect("read preserved proxy state")
                .proxy_enabled
        );

        let restarted = crate::store::AppState::new(db.clone());
        crate::restore_proxy_state_on_startup(&restarted).await;
        assert!(restarted.proxy_service.is_running().await);
        let status = crate::claude_desktop_config::get_status(&db, true)
            .expect("read restored Desktop profile status");
        assert_eq!(status.actual_base_url, status.expected_base_url);
        restarted
            .proxy_service
            .stop()
            .await
            .expect("stop restarted test proxy");
    }

    #[cfg(any(target_os = "macos", windows))]
    #[tokio::test]
    #[serial]
    async fn manual_start_rolls_back_when_desktop_profile_is_invalid() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let state = crate::store::AppState::new(db.clone());
        use_ephemeral_proxy_port(&db).await;
        select_invalid_desktop_provider(&db);

        let error = crate::commands::start_proxy_server_inner(&state)
            .await
            .expect_err("invalid Desktop profile must fail proxy start");
        assert!(error.contains("model route"), "unexpected error: {error}");
        assert!(!state.proxy_service.is_running().await);
        assert!(
            !db.get_global_proxy_config()
                .await
                .expect("read rolled-back proxy state")
                .proxy_enabled
        );
    }

    #[cfg(any(target_os = "macos", windows))]
    #[tokio::test]
    #[serial]
    async fn manual_start_keeps_existing_proxy_when_desktop_profile_is_invalid() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let state = crate::store::AppState::new(db.clone());
        use_ephemeral_proxy_port(&db).await;
        state.proxy_service.start().await.expect("start proxy");
        select_invalid_desktop_provider(&db);

        crate::commands::start_proxy_server_inner(&state)
            .await
            .expect_err("invalid Desktop profile must fail profile sync");
        assert!(state.proxy_service.is_running().await);
        assert!(
            db.get_global_proxy_config()
                .await
                .expect("read preserved proxy state")
                .proxy_enabled
        );
        state
            .proxy_service
            .stop_server()
            .await
            .expect("stop existing proxy listener");
        assert!(
            db.get_global_proxy_config()
                .await
                .expect("read preserved proxy state after listener stop")
                .proxy_enabled
        );
    }

    #[tokio::test]
    #[serial]
    async fn claude_desktop_nexus_route_preserves_user_input_by_default() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let captured_upstream = Arc::new(Mutex::new(None::<Value>));
        let captured_tokenize = Arc::new(Mutex::new(None::<Value>));
        let upstream_app = axum::Router::new()
            .route(
                "/v1/chat/completions",
                axum::routing::post({
                    let captured_upstream = captured_upstream.clone();
                    move |axum::Json(body): axum::Json<Value>| {
                        let captured_upstream = captured_upstream.clone();
                        async move {
                            *captured_upstream.lock().expect("capture upstream request") =
                                Some(body);
                            (
                                [(
                                    axum::http::header::CONTENT_TYPE,
                                    "text/event-stream",
                                )],
                                concat!(
                                    "data: {\"id\":\"chatcmpl_desktop\",\"model\":\"GLM-5.2-FP8\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"Inspect safely.\"}}]}\n\n",
                                    "data: {\"id\":\"chatcmpl_desktop\",\"model\":\"GLM-5.2-FP8\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"{\\\"city\\\":\\\"Hanoi\\\"}\"}}]}}]}\n\n",
                                    "data: {\"id\":\"chatcmpl_desktop\",\"model\":\"GLM-5.2-FP8\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":12,\"completion_tokens\":4}}\n\n",
                                    "data: [DONE]\n\n"
                                ),
                            )
                        }
                    }
                }),
            )
            .route(
                "/v1/tokenize",
                axum::routing::post({
                    let captured_tokenize = captured_tokenize.clone();
                    move |axum::Json(body): axum::Json<Value>| {
                        let captured_tokenize = captured_tokenize.clone();
                        async move {
                            *captured_tokenize.lock().expect("capture tokenize request") =
                                Some(body);
                            axum::Json(json!({"tokens": [1, 2, 3], "count": 17}))
                        }
                    }
                }),
            );
        let upstream_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock upstream");
        let upstream_address = upstream_listener
            .local_addr()
            .expect("read mock upstream address");
        let upstream_task = tokio::spawn(async move {
            axum::serve(upstream_listener, upstream_app)
                .await
                .expect("serve mock upstream");
        });

        let db = Arc::new(Database::memory().expect("init db"));
        let state = crate::store::AppState::new(db.clone());
        use_ephemeral_proxy_port(&db).await;

        let mut provider = Provider::with_id(
            "desktop-nexus".to_string(),
            "Nexus GLM-5.2".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_BASE_URL": format!("http://{upstream_address}/v1"),
                    "ANTHROPIC_AUTH_TOKEN": "test-upstream-token"
                },
                "modelCatalog": {
                    "models": [{
                        "model": "GLM-5.2-FP8",
                        "inputModalities": ["text"]
                    }]
                }
            }),
            None,
        );
        provider.meta = Some(ProviderMeta {
            claude_desktop_mode: Some(crate::provider::ClaudeDesktopMode::Proxy),
            claude_desktop_model_routes: std::collections::HashMap::from([(
                "claude-sonnet-5".to_string(),
                ClaudeDesktopModelRoute {
                    model: "GLM-5.2-FP8".to_string(),
                    label_override: Some("GLM-5.2-FP8".to_string()),
                    supports_1m: Some(true),
                },
            )]),
            api_format: Some("openai_chat".to_string()),
            local_proxy_request_overrides: Some(LocalProxyRequestOverrides {
                body: Some(json!({
                    "max_tokens": 65_536,
                    "chat_template_kwargs": {
                        "enable_thinking": true,
                        "clear_thinking": false
                    }
                })),
                ..Default::default()
            }),
            managed_nexus_preset_version: Some(7),
            provider_type: Some("nexus".to_string()),
            ..Default::default()
        });
        db.save_provider(AppType::ClaudeDesktop.as_str(), &provider)
            .expect("save Desktop provider");
        db.set_current_provider(AppType::ClaudeDesktop.as_str(), &provider.id)
            .expect("select Desktop provider");
        crate::settings::set_current_provider(&AppType::ClaudeDesktop, Some(&provider.id))
            .expect("select local Desktop provider");

        let proxy = state.proxy_service.start().await.expect("start proxy");
        let gateway_token = crate::claude_desktop_config::get_or_create_gateway_token(db.as_ref())
            .expect("create gateway token");
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("build test client");
        let count_response = client
            .post(format!(
                "http://127.0.0.1:{}/claude-desktop/v1/messages/count_tokens",
                proxy.port
            ))
            .bearer_auth(&gateway_token)
            .header("anthropic-version", "2023-06-01")
            .json(&json!({
                "model": "claude-sonnet-5",
                "messages": [
                    {
                        "role": "assistant",
                        "content": [
                            {
                                "type": "thinking",
                                "thinking": "prior proof checkpoint",
                                "signature": "sig"
                            },
                            {"type": "text", "text": "Checkpoint saved."}
                        ]
                    },
                    {
                        "role": "user",
                        "content": [
                            {"type": "text", "text": "Continue from the image."},
                            {
                                "type": "image",
                                "source": {
                                    "type": "base64",
                                    "media_type": "image/png",
                                    "data": "COUNT_IMAGE_MUST_NOT_REACH_UPSTREAM"
                                }
                            }
                        ]
                    }
                ]
            }))
            .send()
            .await
            .expect("send Desktop count request");
        assert_eq!(count_response.status(), reqwest::StatusCode::OK);
        assert_eq!(
            count_response
                .json::<Value>()
                .await
                .expect("read Desktop count response"),
            json!({"input_tokens": 17})
        );
        let count_upstream = captured_tokenize
            .lock()
            .expect("read captured tokenize request")
            .clone()
            .expect("mock upstream should receive one tokenize request");
        assert_eq!(count_upstream["model"], "GLM-5.2-FP8");
        let count_messages = count_upstream["messages"]
            .as_array()
            .expect("OpenAI count messages");
        let count_assistant = count_messages
            .iter()
            .find(|message| message["role"] == "assistant")
            .expect("count request assistant history");
        assert_eq!(
            count_assistant["reasoning_content"],
            "prior proof checkpoint"
        );
        assert_eq!(
            count_upstream["chat_template_kwargs"],
            json!({"enable_thinking": true, "clear_thinking": false})
        );
        let serialized_count =
            serde_json::to_string(&count_upstream).expect("serialize count request");
        assert!(!serialized_count.contains(crate::proxy::media_sanitizer::UNSUPPORTED_IMAGE_MARKER));
        assert!(
            serialized_count.contains("COUNT_IMAGE_MUST_NOT_REACH_UPSTREAM"),
            "media fallback is behavior-changing and must remain opt-in"
        );
        assert!(count_upstream.get("max_tokens").is_none());
        assert!(count_upstream.get("stream").is_none());
        assert_eq!(
            db.conn
                .lock()
                .expect("lock test database")
                .query_row("SELECT COUNT(*) FROM proxy_request_logs", [], |row| {
                    row.get::<_, i64>(0)
                })
                .expect("count Usage rows"),
            0,
            "count_tokens must not create a Usage row"
        );

        let response = client
            .post(format!(
                "http://127.0.0.1:{}/claude-desktop/v1/messages",
                proxy.port
            ))
            .bearer_auth(&gateway_token)
            .header("anthropic-version", "2023-06-01")
            .json(&json!({
                "model": "claude-sonnet-5",
                "max_tokens": 4_096,
                "stream": true,
                "thinking": {"type": "enabled", "budget_tokens": 1_024},
                "messages": [{
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "Inspect the attached image."},
                        {
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": "image/png",
                                "data": "MUST_NOT_REACH_UPSTREAM"
                            }
                        }
                    ]
                }],
                "tools": [{
                    "name": "get_weather",
                    "description": "Get the weather",
                    "input_schema": {
                        "type": "object",
                        "properties": {"city": {"type": "string"}},
                        "required": ["city"]
                    }
                }]
            }))
            .send()
            .await
            .expect("send Desktop request");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert!(response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream")));
        let downstream = response.text().await.expect("read Desktop stream");
        let upstream = captured_upstream
            .lock()
            .expect("read captured upstream request")
            .clone()
            .expect("mock upstream should receive one request");

        state.proxy_service.stop().await.expect("stop proxy");
        upstream_task.abort();

        for (pointer, expected) in [
            ("/model", json!("GLM-5.2-FP8")),
            ("/max_tokens", json!(65_536)),
            ("/chat_template_kwargs/enable_thinking", json!(true)),
            ("/chat_template_kwargs/clear_thinking", json!(false)),
            ("/stream", json!(true)),
            ("/stream_options/include_usage", json!(true)),
            ("/tools/0/function/name", json!("get_weather")),
            (
                "/messages/0/content/0/text",
                json!("Inspect the attached image."),
            ),
            ("/messages/0/content/1/type", json!("image_url")),
        ] {
            assert_eq!(upstream.pointer(pointer), Some(&expected), "{pointer}");
        }
        let serialized_upstream = serde_json::to_string(&upstream).expect("serialize upstream");
        assert!(serialized_upstream.contains("MUST_NOT_REACH_UPSTREAM"));
        assert!(serialized_upstream.contains("data:image/png;base64"));
        assert!(serialized_upstream.contains("\"image_url\""));
        assert!(
            !serialized_upstream.contains(crate::proxy::media_sanitizer::UNSUPPORTED_IMAGE_MARKER)
        );

        let events: Vec<Value> = downstream
            .split("\n\n")
            .filter_map(|block| {
                let data = block
                    .lines()
                    .find_map(|line| line.strip_prefix("data:").map(str::trim))?;
                serde_json::from_str(data).ok()
            })
            .collect();
        let has_event = |pointer: &str, expected: Value| {
            events
                .iter()
                .any(|event| event.pointer(pointer) == Some(&expected))
        };
        assert!(has_event("/delta/thinking", json!("Inspect safely.")));
        assert!(events.iter().any(|event| {
            event.pointer("/content_block/type") == Some(&json!("tool_use"))
                && event.pointer("/content_block/name") == Some(&json!("get_weather"))
        }));
        assert!(has_event(
            "/delta/partial_json",
            json!(r#"{"city":"Hanoi"}"#)
        ));
        assert!(has_event("/delta/stop_reason", json!("tool_use")));
        assert!(has_event("/type", json!("message_stop")));
        for forbidden in ["<think>", "</think>", "reasoning_content", "tool_calls"] {
            assert!(
                !downstream.contains(forbidden),
                "downstream stream leaked upstream protocol field: {forbidden}"
            );
        }
    }

    fn seed_codex_model_template() {
        let codex_dir = crate::codex_config::get_codex_config_dir();
        std::fs::create_dir_all(&codex_dir).expect("create codex dir");
        std::fs::write(
            codex_dir.join("models_cache.json"),
            serde_json::to_string(&serde_json::json!({
                "models": [{
                    "slug": "gpt-5.5",
                    "display_name": "GPT-5.5",
                    "model_messages": { "instructions_template": "t" },
                    "additional_speed_tiers": [],
                    "context_window": 128000
                }]
            }))
            .expect("serialize models_cache"),
        )
        .expect("write models_cache.json");
    }

    #[test]
    fn managed_account_claude_takeover_uses_api_key_placeholder() {
        let mut provider = Provider::with_id(
            "copilot".to_string(),
            "GitHub Copilot".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_BASE_URL": "https://api.githubcopilot.com",
                    "ANTHROPIC_MODEL": "claude-haiku-4.5"
                }
            }),
            None,
        );
        provider.meta = Some(ProviderMeta {
            provider_type: Some("github_copilot".to_string()),
            ..Default::default()
        });

        let mut live_config = provider.settings_config.clone();
        ProxyService::apply_claude_takeover_fields_for_provider(
            &mut live_config,
            "http://127.0.0.1:15721",
            &provider,
        );

        let env = live_config
            .get("env")
            .and_then(|value| value.as_object())
            .expect("env should exist");
        assert_eq!(
            env.get("ANTHROPIC_API_KEY")
                .and_then(|value| value.as_str()),
            Some(PROXY_TOKEN_PLACEHOLDER)
        );
        assert!(
            env.get("ANTHROPIC_AUTH_TOKEN").is_none(),
            "managed OAuth providers should avoid Claude Auth Token login semantics"
        );
    }

    #[test]
    fn managed_account_claude_takeover_sources_copilot_models_from_provider() {
        let mut provider = Provider::with_id(
            "copilot".to_string(),
            "GitHub Copilot".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_BASE_URL": "https://api.githubcopilot.com",
                    "ANTHROPIC_MODEL": "claude-sonnet-4.6",
                    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "claude-haiku-4.5",
                    "ANTHROPIC_DEFAULT_SONNET_MODEL": "claude-sonnet-4.6",
                    "ANTHROPIC_DEFAULT_OPUS_MODEL": "claude-sonnet-4.6"
                }
            }),
            None,
        );
        provider.meta = Some(ProviderMeta {
            provider_type: Some("github_copilot".to_string()),
            ..Default::default()
        });

        let mut live_config = json!({
            "env": {
                "ANTHROPIC_BASE_URL": "https://stale.example.com",
                "ANTHROPIC_API_KEY": "stale-key",
                "ANTHROPIC_MODEL": "stale-model",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL": "stale-haiku",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME": "Stale Haiku",
                "ANTHROPIC_DEFAULT_SONNET_MODEL": "stale-sonnet",
                "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME": "Stale Sonnet",
                "ANTHROPIC_DEFAULT_OPUS_MODEL": "stale-opus",
                "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME": "Stale Opus"
            }
        });
        ProxyService::apply_claude_takeover_fields_for_provider(
            &mut live_config,
            "http://127.0.0.1:15721",
            &provider,
        );

        let env = live_config
            .get("env")
            .and_then(|value| value.as_object())
            .expect("env should exist");
        assert_env_str(env, "ANTHROPIC_MODEL", None);
        assert_env_str(
            env,
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            Some("claude-haiku-4-5"),
        );
        assert_env_str(
            env,
            "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME",
            Some("claude-haiku-4.5"),
        );
        assert_env_str(
            env,
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
            Some("claude-sonnet-4-6"),
        );
        assert_env_str(
            env,
            "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME",
            Some("claude-sonnet-4.6"),
        );
        assert_env_str(env, "ANTHROPIC_DEFAULT_OPUS_MODEL", Some("claude-opus-4-8"));
        assert_env_str(
            env,
            "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME",
            Some("claude-sonnet-4.6"),
        );
        assert_env_str(env, "ANTHROPIC_API_KEY", Some(PROXY_TOKEN_PLACEHOLDER));
        assert_env_str(env, "ANTHROPIC_AUTH_TOKEN", None);
    }

    #[test]
    fn managed_account_claude_takeover_sources_codex_models_from_provider() {
        let mut provider = Provider::with_id(
            "codex".to_string(),
            "Codex".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_BASE_URL": "https://chatgpt.com/backend-api/codex",
                    "ANTHROPIC_MODEL": "gpt-5.4",
                    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "gpt-5.4-mini",
                    "ANTHROPIC_DEFAULT_SONNET_MODEL": "gpt-5.4",
                    "ANTHROPIC_DEFAULT_OPUS_MODEL": "gpt-5.4"
                }
            }),
            None,
        );
        provider.meta = Some(ProviderMeta {
            provider_type: Some("codex_oauth".to_string()),
            ..Default::default()
        });

        let mut live_config = json!({
            "env": {
                "ANTHROPIC_BASE_URL": "https://stale.example.com",
                "ANTHROPIC_AUTH_TOKEN": "stale-token",
                "ANTHROPIC_MODEL": "stale-model",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL": "stale-haiku",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME": "Stale Haiku",
                "ANTHROPIC_DEFAULT_SONNET_MODEL": "stale-sonnet",
                "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME": "Stale Sonnet",
                "ANTHROPIC_DEFAULT_OPUS_MODEL": "stale-opus",
                "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME": "Stale Opus"
            }
        });
        ProxyService::apply_claude_takeover_fields_for_provider(
            &mut live_config,
            "http://127.0.0.1:15721",
            &provider,
        );

        let env = live_config
            .get("env")
            .and_then(|value| value.as_object())
            .expect("env should exist");
        assert_env_str(env, "ANTHROPIC_MODEL", None);
        assert_env_str(
            env,
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            Some("claude-haiku-4-5"),
        );
        assert_env_str(
            env,
            "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME",
            Some("gpt-5.4-mini"),
        );
        assert_env_str(
            env,
            "ANTHROPIC_DEFAULT_SONNET_MODEL",
            Some("claude-sonnet-4-6"),
        );
        assert_env_str(env, "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME", Some("gpt-5.4"));
        assert_env_str(env, "ANTHROPIC_DEFAULT_OPUS_MODEL", Some("claude-opus-4-8"));
        assert_env_str(env, "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME", Some("gpt-5.4"));
        assert_env_str(env, "ANTHROPIC_API_KEY", Some(PROXY_TOKEN_PLACEHOLDER));
        assert_env_str(env, "ANTHROPIC_AUTH_TOKEN", Some(PROXY_TOKEN_PLACEHOLDER));
    }

    #[test]
    fn managed_account_claude_takeover_codex_injects_auth_token_without_preexisting_key() {
        let mut provider = Provider::with_id(
            "codex".to_string(),
            "Codex".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_BASE_URL": "https://chatgpt.com/backend-api/codex"
                }
            }),
            None,
        );
        provider.meta = Some(ProviderMeta {
            provider_type: Some("codex_oauth".to_string()),
            ..Default::default()
        });

        // Fresh install and hot-switch shape: the input env has no token keys at all.
        let mut live_config = provider.settings_config.clone();
        ProxyService::apply_claude_takeover_fields_for_provider(
            &mut live_config,
            "http://127.0.0.1:15721",
            &provider,
        );

        let env = live_config
            .get("env")
            .and_then(|value| value.as_object())
            .expect("env should exist");
        assert_env_str(env, "ANTHROPIC_API_KEY", Some(PROXY_TOKEN_PLACEHOLDER));
        assert_env_str(env, "ANTHROPIC_AUTH_TOKEN", Some(PROXY_TOKEN_PLACEHOLDER));
    }

    #[test]
    fn managed_account_claude_takeover_codex_by_base_url_keeps_auth_token() {
        // A managed Codex provider detected only by base_url without provider_type
        // metadata must also keep the AUTH_TOKEN placeholder because it shares
        // the same policy-selection predicate family.
        let provider = Provider::with_id(
            "codex-url-only".to_string(),
            "Codex (URL only)".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_BASE_URL": "https://chatgpt.com/backend-api/codex"
                }
            }),
            None,
        );
        assert!(provider.uses_managed_account_auth());
        assert!(!provider.is_codex_oauth());

        let mut live_config = provider.settings_config.clone();
        ProxyService::apply_claude_takeover_fields_for_provider(
            &mut live_config,
            "http://127.0.0.1:15721",
            &provider,
        );

        let env = live_config
            .get("env")
            .and_then(|value| value.as_object())
            .expect("env should exist");
        assert_env_str(env, "ANTHROPIC_API_KEY", Some(PROXY_TOKEN_PLACEHOLDER));
        assert_env_str(env, "ANTHROPIC_AUTH_TOKEN", Some(PROXY_TOKEN_PLACEHOLDER));
    }

    #[test]
    fn managed_account_claude_takeover_copilot_removes_stale_auth_token() {
        let mut provider = Provider::with_id(
            "copilot".to_string(),
            "GitHub Copilot".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_BASE_URL": "https://api.githubcopilot.com"
                }
            }),
            None,
        );
        provider.meta = Some(ProviderMeta {
            provider_type: Some("github_copilot".to_string()),
            ..Default::default()
        });

        let mut live_config = json!({
            "env": {
                "ANTHROPIC_BASE_URL": "https://stale.example.com",
                "ANTHROPIC_AUTH_TOKEN": "stale-token"
            }
        });
        ProxyService::apply_claude_takeover_fields_for_provider(
            &mut live_config,
            "http://127.0.0.1:15721",
            &provider,
        );

        let env = live_config
            .get("env")
            .and_then(|value| value.as_object())
            .expect("env should exist");
        assert_env_str(env, "ANTHROPIC_API_KEY", Some(PROXY_TOKEN_PLACEHOLDER));
        assert_env_str(env, "ANTHROPIC_AUTH_TOKEN", None);
    }

    #[test]
    fn normal_claude_takeover_without_token_keeps_auth_token_fallback() {
        let mut live_config = json!({
            "env": {
                "ANTHROPIC_BASE_URL": "https://api.example.com",
                "ANTHROPIC_MODEL": "claude-haiku-4.5"
            }
        });

        ProxyService::apply_claude_takeover_fields(&mut live_config, "http://127.0.0.1:15721");

        assert_eq!(
            live_config
                .get("env")
                .and_then(|env| env.get("ANTHROPIC_AUTH_TOKEN"))
                .and_then(|value| value.as_str()),
            Some(PROXY_TOKEN_PLACEHOLDER)
        );
        assert!(
            live_config
                .get("env")
                .and_then(|env| env.get("ANTHROPIC_API_KEY"))
                .is_none(),
            "non-managed providers should retain the legacy fallback behavior"
        );
    }

    #[tokio::test]
    #[serial]
    async fn start_with_takeover_ephemeral_port_writes_actual_live_url() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        use_ephemeral_proxy_port(&db).await;
        let service = ProxyService::new(db.clone());

        let provider = Provider::with_id(
            "p1".to_string(),
            "P1".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_API_KEY": "provider-key",
                    "ANTHROPIC_BASE_URL": "https://api.anthropic.com"
                }
            }),
            None,
        );
        db.save_provider("claude", &provider)
            .expect("save provider");
        db.set_current_provider("claude", "p1")
            .expect("set db current provider");
        crate::settings::set_current_provider(&AppType::Claude, Some("p1"))
            .expect("set local current provider");
        service
            .write_claude_live(&json!({
                "env": {
                    "ANTHROPIC_API_KEY": "live-key",
                    "ANTHROPIC_BASE_URL": "https://api.anthropic.com"
                }
            }))
            .expect("seed claude live config");

        let info = service
            .start_with_takeover()
            .await
            .expect("start proxy with takeover");
        assert_ne!(info.port, 0, "OS should assign a concrete port");

        let stored_config = db.get_proxy_config().await.expect("read proxy config");
        assert_eq!(
            stored_config.listen_port, info.port,
            "resolved dynamic port should be persisted for DB-only proxy URL paths"
        );

        let live = service.read_claude_live().expect("read taken-over live");
        let base_url = live
            .get("env")
            .and_then(|env| env.get("ANTHROPIC_BASE_URL"))
            .and_then(|value| value.as_str())
            .expect("taken-over base url");
        assert_eq!(base_url, format!("http://127.0.0.1:{}", info.port));
        assert!(
            !base_url.contains(":0"),
            "takeover must never write an unresolved :0 port"
        );

        service
            .stop_with_restore()
            .await
            .expect("stop proxy and restore live config");
    }

    #[test]
    #[serial]
    fn codex_custom_provider_live_write_preserves_oauth_auth_json() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");
        crate::settings::update_settings(crate::settings::AppSettings {
            preserve_codex_official_auth_on_switch: true,
            ..Default::default()
        })
        .expect("enable Codex official auth preservation");

        let db = Arc::new(Database::memory().expect("init db"));
        let service = ProxyService::new(db);
        let oauth_auth = json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": "oauth-id",
                "access_token": "oauth-access"
            }
        });
        crate::codex_config::write_codex_live_atomic(
            &oauth_auth,
            Some(
                r#"model_provider = "openai"
model = "gpt-5-codex"
"#,
            ),
        )
        .expect("seed live OAuth auth");

        let mut provider = Provider::with_id(
            "rightcode".to_string(),
            "RightCode".to_string(),
            json!({
                "auth": {
                    "OPENAI_API_KEY": "rightcode-key"
                },
                "config": r#"model_provider = "rightcode"
model = "gpt-5-codex"

[model_providers.rightcode]
name = "RightCode"
base_url = "https://rightcode.example/v1"
wire_api = "responses"
"#
            }),
            None,
        );
        provider.category = Some("custom".to_string());
        let takeover_settings = json!({
            "auth": {
                "OPENAI_API_KEY": PROXY_TOKEN_PLACEHOLDER
            },
            "config": r#"model_provider = "rightcode"
model = "gpt-5-codex"

[model_providers.rightcode]
name = "RightCode"
base_url = "http://127.0.0.1:15721/v1"
wire_api = "responses"
"#
        });

        service
            .write_codex_live_for_provider(&takeover_settings, Some(&provider))
            .expect("write provider-driven Codex live config");

        let live_auth: Value =
            crate::config::read_json_file(&crate::codex_config::get_codex_auth_path())
                .expect("read live auth");
        assert_eq!(
            live_auth, oauth_auth,
            "third-party Codex proxy writes must not overwrite ChatGPT OAuth login state"
        );

        let live_config = std::fs::read_to_string(crate::codex_config::get_codex_config_path())
            .expect("read live config");
        assert!(
            live_config.contains("experimental_bearer_token"),
            "proxy placeholder should move into config.toml instead of auth.json"
        );
        assert!(
            live_config.contains(PROXY_TOKEN_PLACEHOLDER),
            "live config should carry the proxy placeholder token"
        );

        crate::settings::update_settings(crate::settings::AppSettings::default())
            .expect("reset settings");
    }

    #[tokio::test]
    #[serial]
    async fn codex_takeover_preserves_oauth_and_routes_unified_history_to_custom() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");
        crate::settings::update_settings(crate::settings::AppSettings {
            preserve_codex_official_auth_on_switch: true,
            unify_codex_session_history: true,
            ..Default::default()
        })
        .expect("enable Codex auth preservation and unified history");

        let db = Arc::new(Database::memory().expect("init db"));
        let service = ProxyService::new(db.clone());
        let oauth_auth = json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": "oauth-id",
                "access_token": "oauth-access"
            }
        });
        let deepseek_live_config = r#"model_provider = "deepseek"
model = "deepseek-v4-flash"

[model_providers.deepseek]
name = "DeepSeek"
base_url = "https://api.deepseek.com/v1"
wire_api = "responses"
experimental_bearer_token = "deepseek-key"
"#;
        crate::codex_config::write_codex_live_atomic(&oauth_auth, Some(deepseek_live_config))
            .expect("seed live OAuth auth with DeepSeek config");

        let mut provider = Provider::with_id(
            "deepseek".to_string(),
            "DeepSeek".to_string(),
            json!({
                "auth": {
                    "OPENAI_API_KEY": "deepseek-key"
                },
                "config": r#"model_provider = "deepseek"
model = "deepseek-v4-flash"

[model_providers.deepseek]
name = "DeepSeek"
base_url = "https://api.deepseek.com/v1"
wire_api = "responses"
"#
            }),
            None,
        );
        provider.category = Some("cn_official".to_string());
        db.save_provider("codex", &provider)
            .expect("save DeepSeek provider");
        db.set_current_provider("codex", "deepseek")
            .expect("set current provider");
        crate::settings::set_current_provider(&AppType::Codex, Some("deepseek"))
            .expect("set local current provider");

        service
            .backup_live_config_strict(&AppType::Codex)
            .await
            .expect("back up Codex live config");
        service
            .takeover_live_config_strict(&AppType::Codex)
            .await
            .expect("take over Codex live config");

        let live_auth: Value =
            crate::config::read_json_file(&crate::codex_config::get_codex_auth_path())
                .expect("read live auth");
        assert_eq!(
            live_auth, oauth_auth,
            "Codex takeover should not overwrite ChatGPT OAuth auth when preservation is enabled"
        );

        let backup = db
            .get_live_backup("codex")
            .await
            .expect("read Codex backup")
            .expect("Codex backup exists");
        let backup: Value =
            serde_json::from_str(&backup.original_config).expect("parse Codex backup");
        assert_eq!(
            backup.get("auth"),
            Some(&oauth_auth),
            "the restore backup should retain the live ChatGPT OAuth cache"
        );
        let backup_config = backup
            .get("config")
            .and_then(Value::as_str)
            .expect("backup config");
        let backup_doc = backup_config
            .parse::<toml_edit::DocumentMut>()
            .expect("parse backup config");
        assert_eq!(
            backup_doc
                .get("model_provider")
                .and_then(|item| item.as_str()),
            Some(crate::codex_config::CC_SWITCH_CODEX_MODEL_PROVIDER_ID),
            "releasing takeover should not restore a split third-party history route"
        );

        let live_config = std::fs::read_to_string(crate::codex_config::get_codex_config_path())
            .expect("read live config");
        let live_doc = live_config
            .parse::<toml_edit::DocumentMut>()
            .expect("parse live config");
        assert_eq!(
            live_doc
                .get("model_provider")
                .and_then(|item| item.as_str()),
            Some(crate::codex_config::CC_SWITCH_CODEX_MODEL_PROVIDER_ID),
            "unified history should route third-party Codex sessions through the shared custom bucket"
        );
        assert!(
            live_doc["model_providers"]
                .get(crate::codex_config::CC_SWITCH_CODEX_MODEL_PROVIDER_ID)
                .is_some(),
            "the active third-party provider table should move with the shared custom route"
        );
        assert!(
            live_config.contains(PROXY_TOKEN_PLACEHOLDER),
            "takeover placeholder should move into config.toml"
        );
        assert!(
            service.detect_takeover_in_live_config_for_app(&AppType::Codex),
            "Codex takeover detection should recognize config.toml placeholders"
        );

        service
            .restore_live_config_for_app_with_fallback(&AppType::Codex)
            .await
            .expect("restore Codex live config");
        let restored_auth: Value =
            crate::config::read_json_file(&crate::codex_config::get_codex_auth_path())
                .expect("read restored auth");
        assert_eq!(
            restored_auth, oauth_auth,
            "releasing takeover should preserve ChatGPT OAuth"
        );
        let restored_config = std::fs::read_to_string(crate::codex_config::get_codex_config_path())
            .expect("read restored config");
        let restored_doc = restored_config
            .parse::<toml_edit::DocumentMut>()
            .expect("parse restored config");
        assert_eq!(
            restored_doc
                .get("model_provider")
                .and_then(|item| item.as_str()),
            Some(crate::codex_config::CC_SWITCH_CODEX_MODEL_PROVIDER_ID),
            "releasing takeover should retain the shared history route"
        );

        crate::settings::update_settings(crate::settings::AppSettings::default())
            .expect("reset settings");
    }

    #[tokio::test]
    #[serial]
    async fn codex_takeover_preserves_oauth_auth_json_even_when_provider_category_is_official() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");
        crate::settings::update_settings(crate::settings::AppSettings {
            preserve_codex_official_auth_on_switch: true,
            ..Default::default()
        })
        .expect("enable Codex official auth preservation");

        let db = Arc::new(Database::memory().expect("init db"));
        let service = ProxyService::new(db.clone());
        let oauth_auth = json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": "oauth-id",
                "access_token": "oauth-access"
            }
        });
        let deepseek_live_config = r#"model_provider = "deepseek"
model = "deepseek-v4-flash"

[model_providers.deepseek]
name = "DeepSeek"
base_url = "https://api.deepseek.com/v1"
wire_api = "responses"
experimental_bearer_token = "deepseek-key"
"#;
        crate::codex_config::write_codex_live_atomic(&oauth_auth, Some(deepseek_live_config))
            .expect("seed live OAuth auth with DeepSeek config");

        let mut provider = Provider::with_id(
            "deepseek".to_string(),
            "DeepSeek".to_string(),
            json!({
                "auth": {
                    "OPENAI_API_KEY": "deepseek-key"
                },
                "config": r#"model_provider = "deepseek"
model = "deepseek-v4-flash"

[model_providers.deepseek]
name = "DeepSeek"
base_url = "https://api.deepseek.com/v1"
wire_api = "responses"
"#
            }),
            None,
        );
        provider.category = Some("official".to_string());
        db.save_provider("codex", &provider)
            .expect("save misclassified DeepSeek provider");
        db.set_current_provider("codex", "deepseek")
            .expect("set current provider");
        crate::settings::set_current_provider(&AppType::Codex, Some("deepseek"))
            .expect("set local current provider");

        service
            .takeover_live_config_strict(&AppType::Codex)
            .await
            .expect("take over Codex live config");

        let live_auth: Value =
            crate::config::read_json_file(&crate::codex_config::get_codex_auth_path())
                .expect("read live auth");
        assert_eq!(
            live_auth, oauth_auth,
            "Codex takeover must not rewrite auth.json when preservation is enabled, even if provider category is stale or misclassified"
        );

        let live_config = std::fs::read_to_string(crate::codex_config::get_codex_config_path())
            .expect("read live config");
        assert!(
            live_config.contains(PROXY_TOKEN_PLACEHOLDER),
            "takeover placeholder should move into config.toml"
        );

        crate::settings::update_settings(crate::settings::AppSettings::default())
            .expect("reset settings");
    }

    #[tokio::test]
    #[serial]
    async fn codex_set_takeover_for_app_preserves_oauth_auth_json_when_preserve_enabled() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");
        crate::settings::update_settings(crate::settings::AppSettings {
            preserve_codex_official_auth_on_switch: true,
            ..Default::default()
        })
        .expect("enable Codex official auth preservation");

        let db = Arc::new(Database::memory().expect("init db"));
        use_ephemeral_proxy_port(&db).await;
        let service = ProxyService::new(db.clone());
        let oauth_auth = json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": "oauth-id",
                "access_token": "oauth-access"
            }
        });
        let deepseek_live_config = r#"model_provider = "deepseek"
model = "deepseek-v4-flash"

[model_providers.deepseek]
name = "DeepSeek"
base_url = "https://api.deepseek.com/v1"
wire_api = "responses"
experimental_bearer_token = "deepseek-key"
"#;
        crate::codex_config::write_codex_live_atomic(&oauth_auth, Some(deepseek_live_config))
            .expect("seed live OAuth auth with DeepSeek config");

        let mut provider = Provider::with_id(
            "deepseek".to_string(),
            "DeepSeek".to_string(),
            json!({
                "auth": {
                    "OPENAI_API_KEY": "deepseek-key"
                },
                "config": r#"model_provider = "deepseek"
model = "deepseek-v4-flash"

[model_providers.deepseek]
name = "DeepSeek"
base_url = "https://api.deepseek.com/v1"
wire_api = "responses"
"#
            }),
            None,
        );
        provider.category = Some("official".to_string());
        db.save_provider("codex", &provider)
            .expect("save misclassified DeepSeek provider");
        db.set_current_provider("codex", "deepseek")
            .expect("set current provider");
        crate::settings::set_current_provider(&AppType::Codex, Some("deepseek"))
            .expect("set local current provider");

        service
            .set_takeover_for_app("codex", true)
            .await
            .expect("enable Codex takeover");

        let live_auth: Value =
            crate::config::read_json_file(&crate::codex_config::get_codex_auth_path())
                .expect("read live auth");
        assert_eq!(
            live_auth, oauth_auth,
            "the public takeover command path must not rewrite auth.json when preservation is enabled"
        );

        service
            .set_takeover_for_app("codex", false)
            .await
            .expect("disable Codex takeover");
        assert!(
            !service.is_running().await,
            "the final Codex takeover consumer should stop an otherwise unused listener"
        );
        crate::settings::update_settings(crate::settings::AppSettings::default())
            .expect("reset settings");
    }

    #[cfg(any(target_os = "macos", windows))]
    #[tokio::test]
    #[serial]
    async fn codex_final_takeover_disable_keeps_desktop_listener() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");
        crate::settings::update_settings(crate::settings::AppSettings {
            preserve_codex_official_auth_on_switch: true,
            ..Default::default()
        })
        .expect("enable Codex official auth preservation");

        let db = Arc::new(Database::memory().expect("init db"));
        use_ephemeral_proxy_port(&db).await;
        let state = crate::store::AppState::new(db.clone());
        let service = state.proxy_service.clone();
        let oauth_auth = json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": "oauth-id",
                "access_token": "oauth-access"
            }
        });
        let deepseek_live_config = r#"model_provider = "deepseek"
model = "deepseek-v4-flash"

[model_providers.deepseek]
name = "DeepSeek"
base_url = "https://api.deepseek.com/v1"
wire_api = "responses"
experimental_bearer_token = "deepseek-key"
"#;
        crate::codex_config::write_codex_live_atomic(&oauth_auth, Some(deepseek_live_config))
            .expect("seed live OAuth auth with DeepSeek config");

        let mut provider = Provider::with_id(
            "deepseek".to_string(),
            "DeepSeek".to_string(),
            json!({
                "auth": {
                    "OPENAI_API_KEY": "deepseek-key"
                },
                "config": r#"model_provider = "deepseek"
model = "deepseek-v4-flash"

[model_providers.deepseek]
name = "DeepSeek"
base_url = "https://api.deepseek.com/v1"
wire_api = "responses"
"#
            }),
            None,
        );
        provider.category = Some("official".to_string());
        db.save_provider("codex", &provider)
            .expect("save misclassified DeepSeek provider");
        db.set_current_provider("codex", "deepseek")
            .expect("set current provider");
        crate::settings::set_current_provider(&AppType::Codex, Some("deepseek"))
            .expect("set local current provider");
        select_desktop_proxy_provider(&db);
        crate::commands::start_proxy_server_inner(&state)
            .await
            .expect("start listener and apply Desktop profile");

        service
            .set_takeover_for_app("codex", true)
            .await
            .expect("enable Codex takeover");

        let live_auth: Value =
            crate::config::read_json_file(&crate::codex_config::get_codex_auth_path())
                .expect("read live auth");
        assert_eq!(
            live_auth, oauth_auth,
            "the public takeover command path must not rewrite auth.json when preservation is enabled"
        );

        service
            .set_takeover_for_app("codex", false)
            .await
            .expect("disable Codex takeover");
        assert!(
            service.is_running().await,
            "disabling the final takeover must keep the listener used by Claude Desktop"
        );
        assert!(
            db.get_global_proxy_config()
                .await
                .expect("read mixed-use proxy state")
                .proxy_enabled,
            "the listener's persisted enabled state must remain true"
        );
        let desktop_status = crate::claude_desktop_config::get_status(&db, true)
            .expect("read mixed-use Desktop status");
        assert_eq!(
            desktop_status.actual_base_url, desktop_status.expected_base_url,
            "the active Desktop profile must still target the running listener"
        );
        service
            .stop_server()
            .await
            .expect("stop mixed-use test listener");
        crate::settings::update_settings(crate::settings::AppSettings::default())
            .expect("reset settings");
    }

    #[tokio::test]
    #[serial]
    async fn codex_sync_current_to_live_during_takeover_preserves_oauth_auth_json() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");
        crate::settings::update_settings(crate::settings::AppSettings {
            preserve_codex_official_auth_on_switch: true,
            ..Default::default()
        })
        .expect("enable Codex official auth preservation");

        let db = Arc::new(Database::memory().expect("init db"));
        use_ephemeral_proxy_port(&db).await;
        let state = crate::store::AppState::new(db.clone());
        let oauth_auth = json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": "oauth-id",
                "access_token": "oauth-access"
            }
        });
        let deepseek_live_config = r#"model_provider = "deepseek"
model = "deepseek-v4-flash"

[model_providers.deepseek]
name = "DeepSeek"
base_url = "https://api.deepseek.com/v1"
wire_api = "responses"
experimental_bearer_token = "deepseek-key"
"#;
        crate::codex_config::write_codex_live_atomic(&oauth_auth, Some(deepseek_live_config))
            .expect("seed live OAuth auth with DeepSeek config");

        let mut provider = Provider::with_id(
            "deepseek".to_string(),
            "DeepSeek".to_string(),
            json!({
                "auth": {
                    "OPENAI_API_KEY": "deepseek-key"
                },
                "config": r#"model_provider = "deepseek"
model = "deepseek-v4-flash"

[model_providers.deepseek]
name = "DeepSeek"
base_url = "https://api.deepseek.com/v1"
wire_api = "responses"
"#
            }),
            None,
        );
        provider.category = Some("official".to_string());
        db.save_provider("codex", &provider)
            .expect("save misclassified DeepSeek provider");
        db.set_current_provider("codex", "deepseek")
            .expect("set current provider");
        crate::settings::set_current_provider(&AppType::Codex, Some("deepseek"))
            .expect("set local current provider");

        state
            .proxy_service
            .set_takeover_for_app("codex", true)
            .await
            .expect("enable Codex takeover");

        crate::services::provider::ProviderService::sync_current_to_live(&state)
            .expect("sync current providers while Codex is taken over");

        let live_auth: Value =
            crate::config::read_json_file(&crate::codex_config::get_codex_auth_path())
                .expect("read live auth");
        assert_eq!(
            live_auth, oauth_auth,
            "post-change provider sync must not rewrite Codex auth.json during takeover"
        );

        let backup = db
            .get_live_backup("codex")
            .await
            .expect("get live backup")
            .expect("backup exists");
        let backup_value: Value =
            serde_json::from_str(&backup.original_config).expect("parse backup");
        assert_eq!(
            backup_value.get("auth"),
            Some(&oauth_auth),
            "provider-derived takeover backup should preserve official OAuth auth"
        );
        assert!(
            backup_value
                .get("config")
                .and_then(|value| value.as_str())
                .is_some_and(|config| config.contains("deepseek-key")),
            "provider token should be carried by config.toml in the restore backup"
        );

        state
            .proxy_service
            .set_takeover_for_app("codex", false)
            .await
            .expect("disable Codex takeover");
        let restored_auth: Value =
            crate::config::read_json_file(&crate::codex_config::get_codex_auth_path())
                .expect("read restored auth");
        assert_eq!(
            restored_auth, oauth_auth,
            "turning takeover off should restore the preserved official OAuth auth"
        );

        crate::settings::update_settings(crate::settings::AppSettings::default())
            .expect("reset settings");
    }

    #[tokio::test]
    #[serial]
    async fn codex_sync_current_to_live_during_takeover_activation_keeps_proxy_live_config() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");
        crate::settings::update_settings(crate::settings::AppSettings {
            preserve_codex_official_auth_on_switch: true,
            ..Default::default()
        })
        .expect("enable Codex official auth preservation");

        let db = Arc::new(Database::memory().expect("init db"));
        let state = crate::store::AppState::new(db.clone());
        let oauth_auth = json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": "oauth-id",
                "access_token": "oauth-access"
            }
        });
        let deepseek_live_config = r#"model_provider = "deepseek"
model = "deepseek-v4-flash"

[model_providers.deepseek]
name = "DeepSeek"
base_url = "https://api.deepseek.com/v1"
wire_api = "responses"
experimental_bearer_token = "deepseek-key"
"#;
        crate::codex_config::write_codex_live_atomic(&oauth_auth, Some(deepseek_live_config))
            .expect("seed live OAuth auth with DeepSeek config");

        let mut provider = Provider::with_id(
            "deepseek".to_string(),
            "DeepSeek".to_string(),
            json!({
                "auth": {
                    "OPENAI_API_KEY": "deepseek-key"
                },
                "config": r#"model_provider = "deepseek"
model = "deepseek-v4-flash"

[model_providers.deepseek]
name = "DeepSeek"
base_url = "https://api.deepseek.com/v1"
wire_api = "responses"
"#
            }),
            None,
        );
        provider.category = Some("official".to_string());
        db.save_provider("codex", &provider)
            .expect("save misclassified DeepSeek provider");
        db.set_current_provider("codex", "deepseek")
            .expect("set current provider");
        crate::settings::set_current_provider(&AppType::Codex, Some("deepseek"))
            .expect("set local current provider");

        state
            .proxy_service
            .backup_live_config_strict(&AppType::Codex)
            .await
            .expect("backup Codex live config");
        state
            .proxy_service
            .takeover_live_config_strict(&AppType::Codex)
            .await
            .expect("take over Codex live config");
        assert!(
            !db.get_proxy_config_for_app("codex")
                .await
                .expect("get Codex proxy config")
                .enabled,
            "this reproduces the activation window before set_takeover_for_app marks enabled=true"
        );

        crate::services::provider::ProviderService::sync_current_to_live(&state)
            .expect("sync current providers during takeover activation");

        let live_auth: Value =
            crate::config::read_json_file(&crate::codex_config::get_codex_auth_path())
                .expect("read live auth");
        assert_eq!(
            live_auth, oauth_auth,
            "activation-time provider sync must not rewrite Codex OAuth auth.json"
        );

        let live_config = std::fs::read_to_string(crate::codex_config::get_codex_config_path())
            .expect("read live config");
        assert!(
            live_config.contains(PROXY_TOKEN_PLACEHOLDER),
            "activation-time provider sync must keep the proxy bearer placeholder"
        );
        assert!(
            live_config.contains("http://127.0.0.1"),
            "activation-time provider sync must keep the local proxy base_url"
        );
        assert!(
            state
                .proxy_service
                .detect_takeover_in_live_config_for_app(&AppType::Codex),
            "Codex live config should still be detected as taken over"
        );

        crate::settings::update_settings(crate::settings::AppSettings::default())
            .expect("reset settings");
    }

    #[tokio::test]
    #[serial]
    async fn codex_set_takeover_rebuilds_stale_enabled_state_without_overwriting_backup() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");
        crate::settings::update_settings(crate::settings::AppSettings {
            preserve_codex_official_auth_on_switch: true,
            ..Default::default()
        })
        .expect("enable Codex official auth preservation");

        let db = Arc::new(Database::memory().expect("init db"));
        use_ephemeral_proxy_port(&db).await;
        let service = ProxyService::new(db.clone());
        let oauth_auth = json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": "oauth-id",
                "access_token": "oauth-access"
            }
        });
        let original_deepseek_config = r#"model_provider = "deepseek"
model = "deepseek-v4-flash"

[model_providers.deepseek]
name = "DeepSeek"
base_url = "https://api.deepseek.com/v1"
wire_api = "responses"
experimental_bearer_token = "deepseek-key"
"#;
        let stale_live_config = r#"model_provider = "deepseek"
model = "deepseek-v4-flash"

[model_providers.deepseek]
name = "DeepSeek"
base_url = "https://api.deepseek.com/v1"
wire_api = "responses"
experimental_bearer_token = "PROXY_MANAGED"
"#;
        crate::codex_config::write_codex_live_atomic(&oauth_auth, Some(stale_live_config))
            .expect("seed stale Codex live config");

        let mut provider = Provider::with_id(
            "deepseek".to_string(),
            "DeepSeek".to_string(),
            json!({
                "auth": {
                    "OPENAI_API_KEY": "deepseek-key"
                },
                "config": r#"model_provider = "deepseek"
model = "deepseek-v4-flash"

[model_providers.deepseek]
name = "DeepSeek"
base_url = "https://api.deepseek.com/v1"
wire_api = "responses"
"#
            }),
            None,
        );
        provider.category = Some("official".to_string());
        let mut stale_official = Provider::with_id(
            "codex-official".to_string(),
            "OpenAI Official".to_string(),
            json!({ "config": "model = \"gpt-5.6-sol\"" }),
            None,
        );
        stale_official.category = Some("official".to_string());
        db.save_provider("codex", &provider)
            .expect("save misclassified DeepSeek provider");
        db.save_provider("codex", &stale_official)
            .expect("save stale Official provider");
        db.set_current_provider("codex", "deepseek")
            .expect("set current provider");
        crate::settings::set_current_provider(&AppType::Codex, Some(&stale_official.id))
            .expect("seed stale Official device provider");
        db.save_live_backup(
            "codex",
            &serde_json::to_string(&json!({
                "auth": oauth_auth,
                "config": original_deepseek_config
            }))
            .expect("serialize original backup"),
        )
        .await
        .expect("seed original live backup");
        let mut proxy_config = db
            .get_proxy_config_for_app("codex")
            .await
            .expect("get Codex proxy config");
        proxy_config.enabled = true;
        db.update_proxy_config_for_app(proxy_config)
            .await
            .expect("mark Codex takeover enabled");

        service
            .set_takeover_for_app("codex", true)
            .await
            .expect("rebuild Codex takeover");
        assert_eq!(
            crate::settings::get_current_provider(&AppType::Codex).as_deref(),
            Some("deepseek"),
            "takeover should repair the device provider from the database target"
        );

        let live_auth: Value =
            crate::config::read_json_file(&crate::codex_config::get_codex_auth_path())
                .expect("read live auth");
        assert_eq!(
            live_auth, oauth_auth,
            "repairing stale takeover must restore the preserved OAuth auth from backup"
        );

        let live_config = std::fs::read_to_string(crate::codex_config::get_codex_config_path())
            .expect("read live config");
        let expected_base_url = running_codex_base_url(&service).await;
        assert!(
            live_config.contains(&expected_base_url),
            "stale enabled takeover must be rebuilt to the current proxy base_url"
        );
        assert!(
            live_config.contains(PROXY_TOKEN_PLACEHOLDER),
            "rebuilt takeover should keep the proxy bearer placeholder"
        );
        assert!(
            live_config.contains("model = \"deepseek-v4-flash\""),
            "the repaired Desktop profile should advertise the routed model"
        );
        assert!(
            service
                .live_takeover_matches_current_proxy(&AppType::Codex)
                .await
                .expect("detect rebuilt Codex takeover"),
            "rebuilt Codex live config should match the active proxy address"
        );

        let stale_running_profile = live_config.replace("deepseek-v4-flash", "gpt-5.6-sol");
        crate::codex_config::write_codex_live_atomic(&oauth_auth, Some(&stale_running_profile))
            .expect("simulate a stale GPT label on a healthy takeover");
        service
            .set_takeover_for_app("codex", true)
            .await
            .expect("reconcile the healthy Codex takeover");
        let reconciled_config =
            std::fs::read_to_string(crate::codex_config::get_codex_config_path())
                .expect("read reconciled Codex config");
        assert!(
            reconciled_config.contains("model = \"deepseek-v4-flash\""),
            "an idempotent restore should repair a stale model label"
        );

        let backup = db
            .get_live_backup("codex")
            .await
            .expect("get Codex live backup")
            .expect("backup exists");
        let backup_value: Value =
            serde_json::from_str(&backup.original_config).expect("parse backup");
        assert_eq!(
            backup_value.get("auth"),
            Some(&oauth_auth),
            "rebuilding stale takeover must not overwrite the original OAuth backup"
        );
        assert!(
            backup_value
                .get("config")
                .and_then(|value| value.as_str())
                .is_some_and(|config| config.contains("deepseek-key")
                    && !config.contains("http://127.0.0.1")),
            "backup should remain the restorable DeepSeek config, not the proxy config"
        );

        service
            .set_takeover_for_app("codex", false)
            .await
            .expect("disable Codex takeover");
        crate::settings::update_settings(crate::settings::AppSettings::default())
            .expect("reset settings");
    }

    #[tokio::test]
    #[serial]
    async fn codex_takeover_preserve_disabled_uses_legacy_auth_write_path() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");
        crate::settings::update_settings(crate::settings::AppSettings {
            preserve_codex_official_auth_on_switch: false,
            ..Default::default()
        })
        .expect("disable Codex official auth preservation");

        let db = Arc::new(Database::memory().expect("init db"));
        let service = ProxyService::new(db.clone());
        let oauth_auth = json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": "oauth-id",
                "access_token": "oauth-access"
            }
        });
        let deepseek_live_config = r#"model_provider = "deepseek"
model = "deepseek-v4-flash"

[model_providers.deepseek]
name = "DeepSeek"
base_url = "https://api.deepseek.com/v1"
wire_api = "responses"
"#;
        crate::codex_config::write_codex_live_atomic(&oauth_auth, Some(deepseek_live_config))
            .expect("seed live OAuth auth with DeepSeek config");

        let mut provider = Provider::with_id(
            "deepseek".to_string(),
            "DeepSeek".to_string(),
            json!({
                "auth": {
                    "OPENAI_API_KEY": "deepseek-key"
                },
                "config": r#"model_provider = "deepseek"
model = "deepseek-v4-flash"

[model_providers.deepseek]
name = "DeepSeek"
base_url = "https://api.deepseek.com/v1"
wire_api = "responses"
"#
            }),
            None,
        );
        provider.category = Some("cn_official".to_string());
        db.save_provider("codex", &provider)
            .expect("save DeepSeek provider");
        db.set_current_provider("codex", "deepseek")
            .expect("set current provider");
        crate::settings::set_current_provider(&AppType::Codex, Some("deepseek"))
            .expect("set local current provider");

        service
            .takeover_live_config_strict(&AppType::Codex)
            .await
            .expect("take over Codex live config");

        let live_auth: Value =
            crate::config::read_json_file(&crate::codex_config::get_codex_auth_path())
                .expect("read live auth");
        assert_eq!(
            live_auth
                .get("OPENAI_API_KEY")
                .and_then(|value| value.as_str()),
            Some(PROXY_TOKEN_PLACEHOLDER),
            "disabled preservation should keep the legacy auth.json takeover placeholder"
        );
        assert_eq!(
            live_auth
                .get("tokens")
                .and_then(|tokens| tokens.get("access_token"))
                .and_then(|value| value.as_str()),
            Some("oauth-access"),
            "the new config-only takeover branch must not run when preservation is disabled"
        );

        let live_config = std::fs::read_to_string(crate::codex_config::get_codex_config_path())
            .expect("read live config");
        assert!(
            !live_config.contains(PROXY_TOKEN_PLACEHOLDER),
            "disabled preservation should not move the takeover placeholder into config.toml"
        );

        crate::settings::update_settings(crate::settings::AppSettings::default())
            .expect("reset settings");
    }

    #[test]
    #[serial]
    fn codex_takeover_cleanup_removes_config_placeholder_without_touching_oauth_auth() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let service = ProxyService::new(db);
        let oauth_auth = json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": "oauth-id",
                "access_token": "oauth-access"
            }
        });
        crate::codex_config::write_codex_live_atomic(
            &oauth_auth,
            Some(
                r#"model_provider = "deepseek"
model = "deepseek-v4-flash"

[model_providers.deepseek]
name = "DeepSeek"
base_url = "http://127.0.0.1:15721/v1"
wire_api = "responses"
experimental_bearer_token = "PROXY_MANAGED"
"#,
            ),
        )
        .expect("seed taken-over Codex live config");

        assert!(
            service.detect_takeover_in_live_config_for_app(&AppType::Codex),
            "config.toml placeholder should be detected before cleanup"
        );

        service
            .cleanup_codex_takeover_placeholders_in_live()
            .expect("cleanup Codex takeover placeholders");

        let live_auth: Value =
            crate::config::read_json_file(&crate::codex_config::get_codex_auth_path())
                .expect("read live auth");
        assert_eq!(
            live_auth, oauth_auth,
            "cleanup should preserve ChatGPT OAuth auth"
        );

        let live_config = std::fs::read_to_string(crate::codex_config::get_codex_config_path())
            .expect("read live config");
        assert!(
            !live_config.contains(PROXY_TOKEN_PLACEHOLDER),
            "cleanup should remove config.toml proxy bearer placeholder"
        );
        assert!(
            !live_config.contains("http://127.0.0.1:15721"),
            "cleanup should remove local proxy base_url"
        );
    }

    #[test]
    #[serial]
    fn codex_ssot_restore_removes_legacy_proxy_key_from_newer_oauth_login() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");
        crate::settings::update_settings(crate::settings::AppSettings {
            preserve_codex_official_auth_on_switch: true,
            ..Default::default()
        })
        .expect("enable Codex official auth preservation");

        let db = Arc::new(Database::memory().expect("init db"));
        let service = ProxyService::new(db.clone());
        let mut provider = Provider::with_id(
            "official-account".to_string(),
            "Official account".to_string(),
            json!({
                "auth": {
                    "auth_mode": "chatgpt",
                    "last_refresh": "2026-07-23T21:53:07Z",
                    "tokens": {
                        "id_token": "stored-id",
                        "access_token": "stored-access",
                        "account_id": "same-account"
                    }
                },
                "config": "model = \"gpt-5.6-sol\"\n"
            }),
            None,
        );
        provider.category = Some("official".to_string());
        db.save_provider("codex", &provider)
            .expect("save official provider");
        db.set_current_provider("codex", &provider.id)
            .expect("set current official provider");
        crate::settings::set_current_provider(&AppType::Codex, Some(&provider.id))
            .expect("set local current provider");

        let newer_live_auth = json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": PROXY_TOKEN_PLACEHOLDER,
            "last_refresh": "2026-07-25T11:42:34Z",
            "tokens": {
                "id_token": "newer-id",
                "access_token": "newer-access",
                "account_id": "same-account"
            }
        });
        crate::codex_config::write_codex_live_atomic(&newer_live_auth, Some("model = \"stale\"\n"))
            .expect("seed newer OAuth login with legacy placeholder");
        let snapshot_path =
            crate::config::get_app_config_dir().join("codex_official_auth_snapshot.json");
        crate::config::write_json_file(&snapshot_path, &newer_live_auth)
            .expect("seed preserved OAuth snapshot with legacy placeholder");

        assert!(
            service
                .restore_live_from_ssot_for_app(&AppType::Codex)
                .expect("restore current official provider"),
            "SSOT restore should run"
        );

        let repaired_auth: Value =
            crate::config::read_json_file(&crate::codex_config::get_codex_auth_path())
                .expect("read repaired auth");
        assert_eq!(
            repaired_auth
                .pointer("/tokens/access_token")
                .and_then(Value::as_str),
            Some("newer-access"),
            "newer OAuth tokens must remain authoritative"
        );
        assert!(
            repaired_auth.get("OPENAI_API_KEY").is_none(),
            "the legacy proxy placeholder must not survive official-login restoration"
        );
        let repaired_snapshot: Value =
            crate::config::read_json_file(&snapshot_path).expect("read repaired OAuth snapshot");
        assert_eq!(
            repaired_snapshot
                .pointer("/tokens/access_token")
                .and_then(Value::as_str),
            Some("newer-access"),
            "snapshot repair must preserve newer OAuth tokens"
        );
        assert!(
            repaired_snapshot.get("OPENAI_API_KEY").is_none(),
            "the legacy proxy placeholder must not survive in the durable OAuth snapshot"
        );

        crate::settings::update_settings(crate::settings::AppSettings::default())
            .expect("reset settings");
    }

    #[test]
    #[serial]
    fn codex_custom_provider_live_write_can_overwrite_auth_when_preserve_disabled() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");
        crate::settings::update_settings(crate::settings::AppSettings {
            preserve_codex_official_auth_on_switch: false,
            ..Default::default()
        })
        .expect("disable Codex official auth preservation");

        let db = Arc::new(Database::memory().expect("init db"));
        let service = ProxyService::new(db);
        let oauth_auth = json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": "oauth-id",
                "access_token": "oauth-access"
            }
        });
        crate::codex_config::write_codex_live_atomic(
            &oauth_auth,
            Some(
                r#"model_provider = "openai"
model = "gpt-5-codex"
"#,
            ),
        )
        .expect("seed live OAuth auth");

        let mut provider = Provider::with_id(
            "rightcode".to_string(),
            "RightCode".to_string(),
            json!({
                "auth": {
                    "OPENAI_API_KEY": "rightcode-key"
                },
                "config": r#"model_provider = "rightcode"
model = "gpt-5-codex"

[model_providers.rightcode]
name = "RightCode"
base_url = "https://rightcode.example/v1"
wire_api = "responses"
"#
            }),
            None,
        );
        provider.category = Some("custom".to_string());
        let takeover_auth = json!({
            "OPENAI_API_KEY": PROXY_TOKEN_PLACEHOLDER
        });
        let takeover_settings = json!({
            "auth": takeover_auth,
            "config": r#"model_provider = "rightcode"
model = "gpt-5-codex"

[model_providers.rightcode]
name = "RightCode"
base_url = "http://127.0.0.1:15721/v1"
wire_api = "responses"
"#
        });

        service
            .write_codex_live_for_provider(&takeover_settings, Some(&provider))
            .expect("write provider-driven Codex live config");

        let live_auth: Value =
            crate::config::read_json_file(&crate::codex_config::get_codex_auth_path())
                .expect("read live auth");
        assert_eq!(
            live_auth,
            json!({
                "OPENAI_API_KEY": PROXY_TOKEN_PLACEHOLDER
            }),
            "disabled preservation should let third-party switches overwrite auth.json"
        );

        let live_config = std::fs::read_to_string(crate::codex_config::get_codex_config_path())
            .expect("read live config");
        assert!(
            !live_config.contains("experimental_bearer_token"),
            "provider token should stay in auth.json when preservation is disabled"
        );

        crate::settings::update_settings(crate::settings::AppSettings::default())
            .expect("reset settings");
    }

    #[test]
    fn update_toml_base_url_updates_active_model_provider_base_url() {
        let input = r#"
model_provider = "any"
model = "gpt-5.1-codex"
disable_response_storage = true

[model_providers.any]
name = "any"
base_url = "https://anyrouter.top/v1"
wire_api = "responses"
requires_openai_auth = true
"#;

        let new_url = "http://127.0.0.1:5000/v1";
        let output = ProxyService::update_toml_base_url(input, new_url);

        let parsed: toml::Value =
            toml::from_str(&output).expect("updated config should be valid TOML");

        let base_url = parsed
            .get("model_providers")
            .and_then(|v| v.get("any"))
            .and_then(|v| v.get("base_url"))
            .and_then(|v| v.as_str())
            .expect("model_providers.any.base_url should exist");

        assert_eq!(base_url, new_url);
        assert!(
            parsed.get("base_url").is_none(),
            "should not write top-level base_url"
        );

        let wire_api = parsed
            .get("model_providers")
            .and_then(|v| v.get("any"))
            .and_then(|v| v.get("wire_api"))
            .and_then(|v| v.as_str())
            .expect("model_providers.any.wire_api should exist");
        assert_eq!(wire_api, "responses");
    }

    #[test]
    fn apply_codex_proxy_toml_config_forces_local_responses_wire_api() {
        let input = r#"
model_provider = "chat_only"
model = "gpt-5.1-codex"

[model_providers.chat_only]
name = "Chat Only"
base_url = "https://chat-only.example/v1"
wire_api = "chat"
"#;

        let proxy_url = "http://127.0.0.1:5000/v1";
        let output =
            ProxyService::apply_codex_proxy_toml_config_for_provider(input, proxy_url, None);
        let parsed: toml::Value =
            toml::from_str(&output).expect("updated config should be valid TOML");

        let provider = parsed
            .get("model_providers")
            .and_then(|v| v.get("chat_only"))
            .expect("model_providers.chat_only should exist");

        assert_eq!(
            provider.get("base_url").and_then(|v| v.as_str()),
            Some(proxy_url)
        );
        assert_eq!(
            provider.get("wire_api").and_then(|v| v.as_str()),
            Some("responses")
        );
    }

    #[test]
    fn apply_codex_proxy_toml_config_keeps_model_and_normalizes_nexus_name() {
        let input = r#"
model_provider = "Nexus" # keep selector comment
model = "GLM-5.2-FP8"

[model_providers.nexus]
name = "nexus_glm"
base_url = "https://example.invalid/v1"
wire_api = "responses"

[model_providers.Nexus]
wire_api = "responses"
stream_idle_timeout_ms = 900000
"#;
        let mut provider = Provider::with_id(
            "nexus-glm-5-2".to_string(),
            "Nexus GLM-5.2".to_string(),
            json!({
                "config": input
            }),
            None,
        );
        provider.meta = Some(ProviderMeta {
            api_format: Some("openai_chat".to_string()),
            provider_type: Some("nexus".to_string()),
            ..Default::default()
        });

        let proxy_url = "http://127.0.0.1:5000/v1";
        let output = ProxyService::apply_codex_proxy_toml_config_for_provider(
            input,
            proxy_url,
            Some(&provider),
        );
        let parsed: toml::Value =
            toml::from_str(&output).expect("updated config should be valid TOML");

        assert_eq!(
            parsed.get("model").and_then(|v| v.as_str()),
            Some("GLM-5.2-FP8")
        );
        assert_eq!(
            parsed.get("model_provider").and_then(|v| v.as_str()),
            Some("nexus")
        );
        let nexus = parsed
            .get("model_providers")
            .and_then(|v| v.get("nexus"))
            .expect("canonical provider should be active");
        assert_eq!(
            nexus.get("base_url").and_then(|v| v.as_str()),
            Some(proxy_url)
        );
        assert_eq!(nexus.get("name").and_then(|v| v.as_str()), Some("Nexus"));
        assert_eq!(
            nexus
                .get("stream_idle_timeout_ms")
                .and_then(|v| v.as_integer()),
            Some(900000)
        );
        assert!(parsed
            .get("model_providers")
            .and_then(|v| v.get("Nexus"))
            .is_none());
        assert!(output.contains("# keep selector comment"));
    }

    #[test]
    fn apply_codex_proxy_toml_config_preserves_other_active_nexus_tables() {
        let cases = [
            (
                r#"model_provider = "custom" # keep custom selector
model = "GLM-5.2-FP8"
[model_providers.custom]
name = "Custom GLM"
custom_option = "keep"
"#,
                "custom",
                "# keep custom selector",
            ),
            (
                r#"model_provider = "Nexus" # keep uppercase selector
model = "GLM-5.2-FP8"
[model_providers.Nexus]
name = "User Nexus"
custom_option = "keep"
"#,
                "Nexus",
                "# keep uppercase selector",
            ),
        ];

        for (input, expected_key, expected_comment) in cases {
            let mut provider = Provider::with_id(
                "nexus-glm-5-2".to_string(),
                "Nexus GLM-5.2".to_string(),
                json!({ "config": input }),
                None,
            );
            provider.meta = Some(ProviderMeta {
                api_format: Some("openai_chat".to_string()),
                provider_type: Some("nexus".to_string()),
                ..Default::default()
            });
            let output = ProxyService::apply_codex_proxy_toml_config_for_provider(
                input,
                "http://127.0.0.1:5000/v1",
                Some(&provider),
            );
            let parsed: toml::Value = toml::from_str(&output).expect("valid TOML");
            let providers = parsed.get("model_providers").expect("model_providers");
            let active = providers
                .get(expected_key)
                .expect("expected provider should remain active");

            assert_eq!(parsed["model_provider"].as_str(), Some(expected_key));
            assert_eq!(active["name"].as_str(), Some("Nexus"));
            assert_eq!(active["custom_option"].as_str(), Some("keep"));
            assert!(providers.get("nexus").is_none());
            assert!(output.contains(expected_comment));
        }
    }

    #[test]
    fn apply_codex_proxy_toml_config_preserves_model_for_responses_provider() {
        let input = r#"
model_provider = "responses"
model = "upstream-responses-model"

[model_providers.responses]
name = "Responses"
base_url = "https://responses.example/v1"
wire_api = "responses"
"#;
        let mut provider = Provider::with_id(
            "responses".to_string(),
            "Responses".to_string(),
            json!({
                "config": input
            }),
            None,
        );
        provider.meta = Some(ProviderMeta {
            api_format: Some("openai_responses".to_string()),
            ..Default::default()
        });

        let output = ProxyService::apply_codex_proxy_toml_config_for_provider(
            input,
            "http://127.0.0.1:5000/v1",
            Some(&provider),
        );
        let parsed: toml::Value =
            toml::from_str(&output).expect("updated config should be valid TOML");

        assert_eq!(
            parsed.get("model").and_then(|v| v.as_str()),
            Some("upstream-responses-model")
        );
    }

    #[test]
    fn apply_codex_proxy_toml_config_restores_upstream_model_for_responses_provider() {
        let input = r#"
model_provider = "responses"
model = "gpt-5.4"

[model_providers.responses]
name = "Responses"
base_url = "http://127.0.0.1:5000/v1"
wire_api = "responses"
"#;
        let mut provider = Provider::with_id(
            "responses".to_string(),
            "Responses".to_string(),
            json!({
                "config": r#"model_provider = "responses"
model = "upstream-responses-model"

[model_providers.responses]
name = "Responses"
base_url = "https://responses.example/v1"
wire_api = "responses"
"#
            }),
            None,
        );
        provider.meta = Some(ProviderMeta {
            api_format: Some("openai_responses".to_string()),
            ..Default::default()
        });

        let output = ProxyService::apply_codex_proxy_toml_config_for_provider(
            input,
            "http://127.0.0.1:5000/v1",
            Some(&provider),
        );
        let parsed: toml::Value =
            toml::from_str(&output).expect("updated config should be valid TOML");

        assert_eq!(
            parsed.get("model").and_then(|v| v.as_str()),
            Some("upstream-responses-model")
        );
    }

    #[test]
    fn update_toml_base_url_falls_back_to_top_level_base_url() {
        let input = r#"
model = "gpt-5.1-codex"
"#;

        let new_url = "http://127.0.0.1:5000/v1";
        let output = ProxyService::update_toml_base_url(input, new_url);

        let parsed: toml::Value =
            toml::from_str(&output).expect("updated config should be valid TOML");

        let base_url = parsed
            .get("base_url")
            .and_then(|v| v.as_str())
            .expect("base_url should exist");

        assert_eq!(base_url, new_url);
    }

    #[tokio::test]
    #[serial]
    async fn sync_claude_token_does_not_add_anthropic_api_key() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let service = ProxyService::new(db.clone());

        let provider = Provider::with_id(
            "p1".to_string(),
            "P1".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_BASE_URL": "https://api.anthropic.com",
                    "ANTHROPIC_AUTH_TOKEN": "stale"
                }
            }),
            None,
        );
        db.save_provider("claude", &provider)
            .expect("save provider");
        db.set_current_provider("claude", "p1")
            .expect("set current provider");

        let live_config = json!({
            "env": {
                "ANTHROPIC_AUTH_TOKEN": "fresh"
            }
        });

        service
            .sync_live_config_to_provider(&AppType::Claude, &live_config)
            .await
            .expect("sync");

        let updated = db
            .get_provider_by_id("p1", "claude")
            .expect("get provider")
            .expect("provider exists");
        let env = updated
            .settings_config
            .get("env")
            .and_then(|v| v.as_object())
            .expect("env object");

        assert_eq!(
            env.get("ANTHROPIC_AUTH_TOKEN").and_then(|v| v.as_str()),
            Some("fresh")
        );
        assert!(
            !env.contains_key("ANTHROPIC_API_KEY"),
            "should not add ANTHROPIC_API_KEY when absent"
        );
    }

    #[tokio::test]
    #[serial]
    async fn sync_claude_token_respects_existing_api_key_field() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let service = ProxyService::new(db.clone());

        let provider = Provider::with_id(
            "p1".to_string(),
            "P1".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_BASE_URL": "https://api.anthropic.com",
                    "ANTHROPIC_API_KEY": "stale"
                }
            }),
            None,
        );
        db.save_provider("claude", &provider)
            .expect("save provider");
        db.set_current_provider("claude", "p1")
            .expect("set current provider");

        let live_config = json!({
            "env": {
                "ANTHROPIC_AUTH_TOKEN": "fresh"
            }
        });

        service
            .sync_live_config_to_provider(&AppType::Claude, &live_config)
            .await
            .expect("sync");

        let updated = db
            .get_provider_by_id("p1", "claude")
            .expect("get provider")
            .expect("provider exists");
        let env = updated
            .settings_config
            .get("env")
            .and_then(|v| v.as_object())
            .expect("env object");

        assert_eq!(
            env.get("ANTHROPIC_API_KEY").and_then(|v| v.as_str()),
            Some("fresh")
        );
        assert!(
            !env.contains_key("ANTHROPIC_AUTH_TOKEN"),
            "should not add ANTHROPIC_AUTH_TOKEN when absent"
        );
    }

    #[tokio::test]
    #[serial]
    async fn switch_proxy_target_updates_live_backup_when_taken_over() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let service = ProxyService::new(db.clone());

        let provider_a = Provider::with_id(
            "a".to_string(),
            "A".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_API_KEY": "a-key"
                }
            }),
            None,
        );
        let provider_b = Provider::with_id(
            "b".to_string(),
            "B".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_API_KEY": "b-key"
                }
            }),
            None,
        );
        db.save_provider("claude", &provider_a)
            .expect("save provider a");
        db.save_provider("claude", &provider_b)
            .expect("save provider b");
        db.set_current_provider("claude", "a")
            .expect("set current provider");

        // Simulate an already-taken-over state with an existing Live backup.
        // The content does not matter because hot switching will rewrite it.
        db.save_live_backup("claude", "{\"env\":{}}")
            .await
            .expect("seed live backup");

        service
            .switch_proxy_target("claude", "b")
            .await
            .expect("switch proxy target");

        // Assert that local settings synchronized the current provider.
        assert_eq!(
            crate::settings::get_current_provider(&AppType::Claude).as_deref(),
            Some("b")
        );

        // Assert that the Live backup was updated to the target provider config
        // so stop_with_restore can restore it later.
        let backup = db
            .get_live_backup("claude")
            .await
            .expect("get live backup")
            .expect("backup exists");
        let expected = serde_json::to_string(&provider_b.settings_config).expect("serialize");
        assert_eq!(backup.original_config, expected);
    }

    #[tokio::test]
    #[serial]
    async fn hot_switch_provider_updates_claude_live_while_preserving_takeover_fields() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let service = ProxyService::new(db.clone());

        let provider_a = Provider::with_id(
            "a".to_string(),
            "A".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_API_KEY": "a-key",
                    "ANTHROPIC_BASE_URL": "https://api.a.example",
                    "ANTHROPIC_MODEL": "claude-old"
                },
                "permissions": { "allow": ["Bash"] }
            }),
            None,
        );
        let provider_b = Provider::with_id(
            "b".to_string(),
            "B".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_API_KEY": "b-key",
                    "ANTHROPIC_BASE_URL": "https://api.b.example",
                    "ANTHROPIC_MODEL": "claude-new",
                    "ANTHROPIC_DEFAULT_HAIKU_MODEL": "deepseek-v4-flash",
                    "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME": "DeepSeek V4 Flash",
                    "ANTHROPIC_DEFAULT_SONNET_MODEL": "deepseek-v4-pro[1M]",
                    "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME": "DeepSeek V4 Pro",
                    "ANTHROPIC_DEFAULT_OPUS_MODEL": "deepseek-v4-ultra [1m]"
                },
                "permissions": { "allow": ["Read"] }
            }),
            None,
        );

        db.save_provider("claude", &provider_a)
            .expect("save provider a");
        db.save_provider("claude", &provider_b)
            .expect("save provider b");
        db.set_current_provider("claude", "a")
            .expect("set current provider");
        crate::settings::set_current_provider(&AppType::Claude, Some("a"))
            .expect("set local current provider");
        db.save_live_backup(
            "claude",
            &serde_json::to_string(&provider_a.settings_config).expect("serialize provider a"),
        )
        .await
        .expect("seed live backup");
        service
            .write_claude_live(&json!({
                "env": {
                    "ANTHROPIC_BASE_URL": "http://127.0.0.1:15721",
                    "ANTHROPIC_API_KEY": PROXY_TOKEN_PLACEHOLDER,
                    "ANTHROPIC_MODEL": "stale-model",
                    "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME": "Stale Sonnet"
                },
                "permissions": { "allow": ["Bash"] }
            }))
            .expect("seed taken-over live file");

        service
            .hot_switch_provider("claude", "b")
            .await
            .expect("hot switch provider");

        let live = service.read_claude_live().expect("read live config");
        assert_eq!(
            live.get("permissions"),
            provider_b.settings_config.get("permissions"),
            "provider-derived live settings should be refreshed"
        );
        assert_eq!(
            live.get("env")
                .and_then(|env| env.get("ANTHROPIC_API_KEY"))
                .and_then(|v| v.as_str()),
            Some(PROXY_TOKEN_PLACEHOLDER),
            "takeover token placeholder should be preserved"
        );
        assert_eq!(
            live.get("env")
                .and_then(|env| env.get("ANTHROPIC_BASE_URL"))
                .and_then(|v| v.as_str()),
            Some("http://127.0.0.1:15721"),
            "takeover proxy URL should remain active"
        );
        assert!(
            live.get("env")
                .and_then(|env| env.get("ANTHROPIC_MODEL"))
                .is_none(),
            "fallback model override should be removed in takeover mode"
        );
        let live_env = live
            .get("env")
            .and_then(|env| env.as_object())
            .expect("live env");
        assert_eq!(
            live_env
                .get("ANTHROPIC_DEFAULT_HAIKU_MODEL")
                .and_then(|v| v.as_str()),
            Some("claude-haiku-4-5"),
            "takeover mode should expose a stable Haiku role model"
        );
        assert_eq!(
            live_env
                .get("ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME")
                .and_then(|v| v.as_str()),
            Some("DeepSeek V4 Flash"),
            "model menu should show the current provider Haiku display name"
        );
        assert_eq!(
            live_env
                .get("ANTHROPIC_DEFAULT_SONNET_MODEL")
                .and_then(|v| v.as_str()),
            Some("claude-sonnet-4-6[1M]"),
            "Sonnet role should carry the local 1M declaration for Claude Code"
        );
        assert_eq!(
            live_env
                .get("ANTHROPIC_DEFAULT_SONNET_MODEL_NAME")
                .and_then(|v| v.as_str()),
            Some("DeepSeek V4 Pro"),
            "stale model display names should be replaced during hot switch"
        );
        assert_eq!(
            live_env
                .get("ANTHROPIC_DEFAULT_OPUS_MODEL")
                .and_then(|v| v.as_str()),
            Some("claude-opus-4-8[1M]"),
            "Opus role should preserve the current provider 1M capability marker"
        );
        assert_eq!(
            live_env
                .get("ANTHROPIC_DEFAULT_OPUS_MODEL_NAME")
                .and_then(|v| v.as_str()),
            Some("deepseek-v4-ultra"),
            "implicit display names should strip the local 1M marker"
        );

        let backup = db
            .get_live_backup("claude")
            .await
            .expect("get live backup")
            .expect("backup exists");
        let expected = serde_json::to_string(&provider_b.settings_config).expect("serialize");
        assert_eq!(backup.original_config, expected);
    }

    #[tokio::test]
    #[serial]
    async fn hot_switch_provider_serializes_same_app_switches() {
        use tokio::time::{sleep, Duration};

        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let service = ProxyService::new(db.clone());

        let provider_a = Provider::with_id(
            "a".to_string(),
            "A".to_string(),
            json!({ "env": { "ANTHROPIC_API_KEY": "a-key" } }),
            None,
        );
        let provider_b = Provider::with_id(
            "b".to_string(),
            "B".to_string(),
            json!({ "env": { "ANTHROPIC_API_KEY": "b-key" } }),
            None,
        );
        let provider_c = Provider::with_id(
            "c".to_string(),
            "C".to_string(),
            json!({ "env": { "ANTHROPIC_API_KEY": "c-key" } }),
            None,
        );

        db.save_provider("claude", &provider_a)
            .expect("save provider a");
        db.save_provider("claude", &provider_b)
            .expect("save provider b");
        db.save_provider("claude", &provider_c)
            .expect("save provider c");
        db.set_current_provider("claude", "a")
            .expect("set current provider");
        crate::settings::set_current_provider(&AppType::Claude, Some("a"))
            .expect("set local current provider");
        db.save_live_backup("claude", "{\"env\":{}}")
            .await
            .expect("seed live backup");

        let guard = service.lock_switch_for_test("claude").await;
        let service_for_b = service.clone();
        let service_for_c = service.clone();

        let switch_b = tokio::spawn(async move {
            service_for_b
                .hot_switch_provider("claude", "b")
                .await
                .expect("switch to b")
        });
        sleep(Duration::from_millis(20)).await;
        let switch_c = tokio::spawn(async move {
            service_for_c
                .hot_switch_provider("claude", "c")
                .await
                .expect("switch to c")
        });

        sleep(Duration::from_millis(20)).await;
        drop(guard);

        let outcome_b = switch_b.await.expect("join switch b");
        let outcome_c = switch_c.await.expect("join switch c");
        assert!(outcome_b.logical_target_changed);
        assert!(outcome_c.logical_target_changed);

        assert_eq!(
            crate::settings::get_effective_current_provider(&db, &AppType::Claude)
                .expect("effective current"),
            Some("c".to_string())
        );
        assert_eq!(
            crate::settings::get_current_provider(&AppType::Claude).as_deref(),
            Some("c")
        );
        assert_eq!(
            db.get_current_provider("claude").expect("db current"),
            Some("c".to_string())
        );

        let backup = db
            .get_live_backup("claude")
            .await
            .expect("get live backup")
            .expect("backup exists");
        let expected = serde_json::to_string(&provider_c.settings_config).expect("serialize");
        assert_eq!(backup.original_config, expected);
    }

    #[tokio::test]
    #[serial]
    async fn restore_waits_for_hot_switch_and_restores_latest_backup() {
        use tokio::time::{sleep, Duration};

        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let service = ProxyService::new(db.clone());

        let provider_a = Provider::with_id(
            "a".to_string(),
            "A".to_string(),
            json!({ "env": { "ANTHROPIC_API_KEY": "a-key" } }),
            None,
        );
        let provider_b = Provider::with_id(
            "b".to_string(),
            "B".to_string(),
            json!({ "env": { "ANTHROPIC_API_KEY": "b-key" } }),
            None,
        );

        db.save_provider("claude", &provider_a)
            .expect("save provider a");
        db.save_provider("claude", &provider_b)
            .expect("save provider b");
        db.set_current_provider("claude", "a")
            .expect("set current provider");
        crate::settings::set_current_provider(&AppType::Claude, Some("a"))
            .expect("set local current provider");
        db.save_live_backup(
            "claude",
            &serde_json::to_string(&provider_a.settings_config).expect("serialize provider a"),
        )
        .await
        .expect("seed live backup");
        service
            .write_claude_live(&json!({ "env": { "ANTHROPIC_API_KEY": "stale" } }))
            .expect("seed live file");

        let guard = service.lock_switch_for_test("claude").await;
        let service_for_switch = service.clone();
        let service_for_restore = service.clone();

        let switch_to_b = tokio::spawn(async move {
            service_for_switch
                .hot_switch_provider("claude", "b")
                .await
                .expect("switch to b")
        });
        sleep(Duration::from_millis(20)).await;
        let restore = tokio::spawn(async move {
            service_for_restore
                .restore_live_config_for_app_with_fallback(&AppType::Claude)
                .await
                .expect("restore claude live")
        });

        sleep(Duration::from_millis(20)).await;
        drop(guard);

        let outcome = switch_to_b.await.expect("join switch");
        restore.await.expect("join restore");
        assert!(outcome.logical_target_changed);

        assert_eq!(
            crate::settings::get_effective_current_provider(&db, &AppType::Claude)
                .expect("effective current"),
            Some("b".to_string())
        );

        let backup = db
            .get_live_backup("claude")
            .await
            .expect("get live backup")
            .expect("backup exists");
        let expected = serde_json::to_string(&provider_b.settings_config).expect("serialize");
        assert_eq!(backup.original_config, expected);
        assert_eq!(
            service.read_claude_live().expect("read live"),
            provider_b.settings_config
        );
    }

    #[tokio::test]
    #[serial]
    async fn update_live_backup_from_provider_applies_claude_common_config() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        db.set_config_snippet(
            "claude",
            Some(
                serde_json::json!({
                    "includeCoAuthoredBy": false
                })
                .to_string(),
            ),
        )
        .expect("set common config snippet");

        let service = ProxyService::new(db.clone());

        let mut provider = Provider::with_id(
            "p1".to_string(),
            "P1".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": "token",
                    "ANTHROPIC_BASE_URL": "https://claude.example"
                }
            }),
            None,
        );
        provider.meta = Some(ProviderMeta {
            common_config_enabled: Some(true),
            ..Default::default()
        });

        service
            .update_live_backup_from_provider("claude", &provider)
            .await
            .expect("update live backup");

        let backup = db
            .get_live_backup("claude")
            .await
            .expect("get live backup")
            .expect("backup exists");
        let stored: Value =
            serde_json::from_str(&backup.original_config).expect("parse backup json");

        assert_eq!(
            stored.get("includeCoAuthoredBy").and_then(|v| v.as_bool()),
            Some(false),
            "common config should be applied into Claude restore backup"
        );
    }

    #[tokio::test]
    #[serial]
    async fn update_live_backup_from_provider_applies_codex_common_config() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        db.set_config_snippet(
            "codex",
            Some("disable_response_storage = true\n".to_string()),
        )
        .expect("set common config snippet");

        let service = ProxyService::new(db.clone());

        let mut provider = Provider::with_id(
            "p1".to_string(),
            "P1".to_string(),
            json!({
                "auth": {
                    "OPENAI_API_KEY": "token"
                },
                "config": r#"model_provider = "any"
model = "gpt-5"

[model_providers.any]
base_url = "https://codex.example/v1"
"#
            }),
            None,
        );
        provider.meta = Some(ProviderMeta {
            common_config_enabled: Some(true),
            ..Default::default()
        });

        service
            .update_live_backup_from_provider("codex", &provider)
            .await
            .expect("update live backup");

        let backup = db
            .get_live_backup("codex")
            .await
            .expect("get live backup")
            .expect("backup exists");
        let stored: Value =
            serde_json::from_str(&backup.original_config).expect("parse backup json");
        let config = stored
            .get("config")
            .and_then(|v| v.as_str())
            .expect("config string");

        assert!(
            config.contains("disable_response_storage = true"),
            "common config should be applied into Codex restore backup"
        );
    }

    #[tokio::test]
    #[serial]
    async fn update_live_backup_from_provider_preserves_codex_user_config() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let service = ProxyService::new(db.clone());

        db.save_live_backup(
            "codex",
            &serde_json::to_string(&json!({
                "auth": {
                    "OPENAI_API_KEY": "old-token"
                },
                "config": r#"model_provider = "any"
model = "gpt-4"

[model_providers.any]
base_url = "https://old.example/v1"

[desktop]
appearanceTheme = "dark"
dock-icon-preference = "codex-system"

[mcp_servers.echo]
command = "npx"
args = ["echo-server"]
"#
            }))
            .expect("serialize seed backup"),
        )
        .await
        .expect("seed live backup");

        let provider = Provider::with_id(
            "p2".to_string(),
            "P2".to_string(),
            json!({
                "auth": {
                    "OPENAI_API_KEY": "new-token"
                },
                "config": r#"model_provider = "any"
model = "gpt-5"

[model_providers.any]
base_url = "https://new.example/v1"
"#
            }),
            None,
        );

        service
            .update_live_backup_from_provider("codex", &provider)
            .await
            .expect("update live backup");

        let backup = db
            .get_live_backup("codex")
            .await
            .expect("get live backup")
            .expect("backup exists");
        let stored: Value =
            serde_json::from_str(&backup.original_config).expect("parse backup json");
        let config = stored
            .get("config")
            .and_then(|v| v.as_str())
            .expect("config string");

        assert!(
            config.contains("[mcp_servers.echo]"),
            "existing Codex MCP section should survive proxy hot-switch backup update"
        );
        assert!(
            config.contains("appearanceTheme = \"dark\"")
                && config.contains("dock-icon-preference = \"codex-system\""),
            "existing Codex desktop preferences should survive backup update"
        );
        assert!(
            config.contains("https://new.example/v1"),
            "provider-specific base_url should still update to the new provider"
        );
    }

    #[tokio::test]
    #[serial]
    async fn hot_switch_codex_provider_preserves_provider_model_provider_in_backup_and_restore() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let service = ProxyService::new(db.clone());

        let provider_a = Provider::with_id(
            "a".to_string(),
            "RightCode".to_string(),
            json!({
                "auth": {
                    "OPENAI_API_KEY": "rightcode-key"
                },
                "config": r#"model_provider = "rightcode"
model = "gpt-5.4"
model_context_window = 1048576
model_auto_compact_token_limit = 252000

[model_providers.rightcode]
name = "RightCode"
base_url = "https://rightcode.example/v1"
wire_api = "responses"
requires_openai_auth = true

[desktop]
appearanceTheme = "light"
dock-icon-preference = "classic"
"#
            }),
            None,
        );
        let provider_b = Provider::with_id(
            "b".to_string(),
            "AiHubMix".to_string(),
            json!({
                "auth": {
                    "OPENAI_API_KEY": "aihubmix-key"
                },
                "config": r#"model_provider = "aihubmix"
model = "gpt-5.4"
model_context_window = 131072
model_auto_compact_token_limit = 100000

[model_providers.aihubmix]
name = "AiHubMix"
base_url = "https://aihubmix.example/v1"
wire_api = "responses"
requires_openai_auth = true

[desktop]
show-ultra-in-model-picker-slider = false
appearanceTheme = "light"
dock-icon-preference = "classic"
animations-enabled = true
provider-only-setting = "stale"
"#
            }),
            None,
        );

        db.save_provider("codex", &provider_a)
            .expect("save provider a");
        db.save_provider("codex", &provider_b)
            .expect("save provider b");
        db.set_current_provider("codex", "a")
            .expect("set current provider");
        crate::settings::set_current_provider(&AppType::Codex, Some("a"))
            .expect("set local current provider");
        db.save_live_backup(
            "codex",
            &serde_json::to_string(&provider_a.settings_config).expect("serialize provider a"),
        )
        .await
        .expect("seed live backup");
        let takeover_config = r#"model_provider = "rightcode"
model = "gpt-5.4"
model_context_window = 1048576
model_auto_compact_token_limit = 252000

[model_providers.rightcode]
name = "RightCode"
base_url = "http://127.0.0.1:15721/v1"
wire_api = "responses"
requires_openai_auth = true

[desktop]
show-ultra-in-model-picker-slider = true
appearanceTheme = "dark"
dock-icon-preference = "codex-system"
animations-enabled = false
sidebar-density = "compact"
"#;
        service
            .write_codex_live(&json!({
                "auth": {
                    "OPENAI_API_KEY": PROXY_TOKEN_PLACEHOLDER
                },
                "config": takeover_config
            }))
            .expect("seed taken-over Codex live config");

        service
            .hot_switch_provider("codex", "b")
            .await
            .expect("hot switch Codex provider");

        let backup = db
            .get_live_backup("codex")
            .await
            .expect("get live backup")
            .expect("backup exists");
        let stored: Value =
            serde_json::from_str(&backup.original_config).expect("parse backup json");
        let backup_config = stored
            .get("config")
            .and_then(|v| v.as_str())
            .expect("backup config string");
        let parsed_backup: toml::Value =
            toml::from_str(backup_config).expect("parse backup config");
        let expected_desktop: toml::Value =
            toml::from_str(takeover_config).expect("parse expected desktop table");
        assert_eq!(
            parsed_backup.get("model_provider").and_then(|v| v.as_str()),
            Some("aihubmix"),
            "provider-derived restore backup should preserve the provider's model_provider"
        );
        assert_eq!(
            parsed_backup
                .get("model_context_window")
                .and_then(|v| v.as_integer()),
            Some(131072),
            "provider-derived restore backup should refresh the selected provider's context window"
        );
        assert_eq!(
            parsed_backup
                .get("model_auto_compact_token_limit")
                .and_then(|v| v.as_integer()),
            Some(100000),
            "provider-derived restore backup should refresh the selected provider's compact limit"
        );
        let backup_model_providers = parsed_backup
            .get("model_providers")
            .and_then(|v| v.as_table())
            .expect("backup model_providers");
        assert!(backup_model_providers.get("custom").is_none());
        assert_eq!(
            backup_model_providers
                .get("aihubmix")
                .and_then(|v| v.get("base_url"))
                .and_then(|v| v.as_str()),
            Some("https://aihubmix.example/v1"),
            "provider id should point at the hot-switched provider endpoint"
        );
        assert_eq!(
            parsed_backup.get("desktop"),
            expected_desktop.get("desktop"),
            "the restore backup must use the entire live desktop table, not stale provider values"
        );

        let live = service.read_codex_live().expect("read Codex live config");
        let live_config = live
            .get("config")
            .and_then(|v| v.as_str())
            .expect("live config string");
        let parsed_live: toml::Value = toml::from_str(live_config).expect("parse live config");
        assert_eq!(
            parsed_live.get("model_provider").and_then(|v| v.as_str()),
            Some("aihubmix"),
            "hot-switched Codex live config should expose the selected provider"
        );
        assert_eq!(
            parsed_live
                .get("model_context_window")
                .and_then(|v| v.as_integer()),
            Some(131072),
            "hot-switched Codex live config should use the selected provider's context window"
        );
        assert_eq!(
            parsed_live
                .get("model_auto_compact_token_limit")
                .and_then(|v| v.as_integer()),
            Some(100000),
            "hot-switched Codex live config should use the selected provider's compact limit"
        );
        assert_eq!(
            parsed_live
                .get("model_providers")
                .and_then(|v| v.get("aihubmix"))
                .and_then(|v| v.get("name"))
                .and_then(|v| v.as_str()),
            Some("AiHubMix"),
            "Codex app provider label should follow the selected provider"
        );
        assert_eq!(
            parsed_live
                .get("model_providers")
                .and_then(|v| v.get("aihubmix"))
                .and_then(|v| v.get("base_url"))
                .and_then(|v| v.as_str()),
            Some("http://127.0.0.1:15721/v1"),
            "taken-over live config should stay pointed at the local proxy"
        );
        assert_eq!(
            parsed_live.get("desktop"),
            expected_desktop.get("desktop"),
            "hot-switch synchronization must use the entire live desktop table"
        );

        let first_backup = backup.original_config.clone();
        let first_live = live_config.to_string();
        service
            .hot_switch_provider("codex", "b")
            .await
            .expect("repeat hot switch Codex provider");
        let repeated_backup = db
            .get_live_backup("codex")
            .await
            .expect("get repeated live backup")
            .expect("repeated backup exists");
        let repeated_live = service
            .read_codex_live()
            .expect("read repeated Codex live config");
        assert_eq!(
            repeated_backup.original_config, first_backup,
            "repeating the same hot switch should leave the restore backup byte-identical"
        );
        assert_eq!(
            repeated_live.get("config").and_then(Value::as_str),
            Some(first_live.as_str()),
            "repeating the same hot switch should leave live config.toml byte-identical"
        );

        service
            .restore_live_config_for_app_with_fallback(&AppType::Codex)
            .await
            .expect("restore Codex live config");

        let live = service.read_codex_live().expect("read Codex live config");
        let live_config = live
            .get("config")
            .and_then(|v| v.as_str())
            .expect("live config string");
        let parsed_live: toml::Value = toml::from_str(live_config).expect("parse live config");
        assert_eq!(
            parsed_live.get("model_provider").and_then(|v| v.as_str()),
            Some("aihubmix"),
            "restored Codex live config should preserve the provider's model_provider"
        );
        assert_eq!(
            parsed_live.get("desktop"),
            expected_desktop.get("desktop"),
            "restored Codex live config should preserve the authoritative desktop table"
        );
        assert_eq!(
            live.get("auth")
                .and_then(|auth| auth.get("OPENAI_API_KEY"))
                .and_then(|v| v.as_str()),
            Some("aihubmix-key"),
            "restore should still use the hot-switched provider auth"
        );
    }

    #[tokio::test]
    #[serial]
    async fn hot_switch_codex_chat_provider_updates_live_provider_display() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let service = ProxyService::new(db.clone());

        let provider_a = Provider::with_id(
            "a".to_string(),
            "Responses".to_string(),
            json!({
                "auth": {
                    "OPENAI_API_KEY": "responses-key"
                },
                "config": r#"model_provider = "stable"
model = "responses-model"

[model_providers.stable]
name = "Stable"
base_url = "https://responses.example/v1"
wire_api = "responses"
requires_openai_auth = true
"#
            }),
            None,
        );
        let mut provider_b = Provider::with_id(
            "b".to_string(),
            "DeepSeek".to_string(),
            json!({
                "auth": {
                    "OPENAI_API_KEY": "deepseek-key"
                },
                "config": r#"model_provider = "deepseek"
model = "deepseek-v4-flash"

[model_providers.deepseek]
name = "DeepSeek"
base_url = "https://api.deepseek.com/v1"
wire_api = "responses"
requires_openai_auth = true
"#
            }),
            None,
        );
        provider_b.meta = Some(ProviderMeta {
            api_format: Some("openai_chat".to_string()),
            ..Default::default()
        });

        db.save_provider("codex", &provider_a)
            .expect("save provider a");
        db.save_provider("codex", &provider_b)
            .expect("save provider b");
        db.set_current_provider("codex", "a")
            .expect("set current provider");
        crate::settings::set_current_provider(&AppType::Codex, Some("a"))
            .expect("set local current provider");
        db.save_live_backup(
            "codex",
            &serde_json::to_string(&provider_a.settings_config).expect("serialize provider a"),
        )
        .await
        .expect("seed live backup");
        service
            .write_codex_live(&json!({
                "auth": {
                    "OPENAI_API_KEY": PROXY_TOKEN_PLACEHOLDER
                },
                "config": r#"model_provider = "stable"
model = "responses-model"

[model_providers.stable]
name = "Stable"
base_url = "http://127.0.0.1:15721/v1"
wire_api = "responses"
requires_openai_auth = true
"#
            }))
            .expect("seed taken-over Codex live config");

        service
            .hot_switch_provider("codex", "b")
            .await
            .expect("hot switch Codex provider");

        let live = service.read_codex_live().expect("read Codex live config");
        let live_config = live
            .get("config")
            .and_then(|v| v.as_str())
            .expect("live config string");
        let parsed_live: toml::Value = toml::from_str(live_config).expect("parse live config");

        assert_eq!(
            parsed_live.get("model_provider").and_then(|v| v.as_str()),
            Some("deepseek")
        );
        assert_eq!(
            parsed_live
                .get("model_providers")
                .and_then(|v| v.get("deepseek"))
                .and_then(|v| v.get("name"))
                .and_then(|v| v.as_str()),
            Some("DeepSeek")
        );
        assert_eq!(
            parsed_live
                .get("model_providers")
                .and_then(|v| v.get("deepseek"))
                .and_then(|v| v.get("base_url"))
                .and_then(|v| v.as_str()),
            Some("http://127.0.0.1:15721/v1")
        );
        assert_eq!(
            parsed_live.get("model").and_then(|v| v.as_str()),
            Some("deepseek-v4-flash")
        );
        assert_eq!(
            live.get("auth")
                .and_then(|auth| auth.get("OPENAI_API_KEY"))
                .and_then(|v| v.as_str()),
            Some(PROXY_TOKEN_PLACEHOLDER)
        );
    }

    #[tokio::test]
    #[serial]
    async fn update_live_backup_from_provider_keeps_new_codex_mcp_entries_on_conflict() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let service = ProxyService::new(db.clone());

        db.save_live_backup(
            "codex",
            &serde_json::to_string(&json!({
                "auth": {
                    "OPENAI_API_KEY": "old-token"
                },
                "config": r#"[mcp_servers.shared]
command = "old-command"

[mcp_servers.legacy]
command = "legacy-command"
"#
            }))
            .expect("serialize seed backup"),
        )
        .await
        .expect("seed live backup");

        let provider = Provider::with_id(
            "p2".to_string(),
            "P2".to_string(),
            json!({
                "auth": {
                    "OPENAI_API_KEY": "new-token"
                },
                "config": r#"[mcp_servers.shared]
command = "new-command"

[mcp_servers.latest]
command = "latest-command"
"#
            }),
            None,
        );

        service
            .update_live_backup_from_provider("codex", &provider)
            .await
            .expect("update live backup");

        let backup = db
            .get_live_backup("codex")
            .await
            .expect("get live backup")
            .expect("backup exists");
        let stored: Value =
            serde_json::from_str(&backup.original_config).expect("parse backup json");
        let config = stored
            .get("config")
            .and_then(|v| v.as_str())
            .expect("config string");
        let parsed: toml::Value = toml::from_str(config).expect("parse merged codex config");

        let mcp_servers = parsed
            .get("mcp_servers")
            .expect("mcp_servers should be present");
        assert_eq!(
            mcp_servers
                .get("shared")
                .and_then(|v| v.get("command"))
                .and_then(|v| v.as_str()),
            Some("new-command"),
            "new provider/common-config MCP definition should win on conflict"
        );
        assert_eq!(
            mcp_servers
                .get("legacy")
                .and_then(|v| v.get("command"))
                .and_then(|v| v.as_str()),
            Some("legacy-command"),
            "backup-only MCP entries should still be preserved"
        );
        assert_eq!(
            mcp_servers
                .get("latest")
                .and_then(|v| v.get("command"))
                .and_then(|v| v.as_str()),
            Some("latest-command"),
            "new MCP entries should remain in the restore backup"
        );
    }

    #[tokio::test]
    #[serial]
    async fn provider_switch_with_restored_codex_backup_refreshes_catalog_and_common_config() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");
        seed_codex_model_template();

        let db = Arc::new(Database::memory().expect("init db"));
        let state = crate::store::AppState::new(db.clone());

        db.set_config_snippet(
            "codex",
            Some(
                r#"[mcp_servers.shared]
command = "shared-command"
"#
                .to_string(),
            ),
        )
        .expect("set common config snippet");

        let proxy_config = ProxyConfig {
            listen_port: 0,
            ..Default::default()
        };
        db.update_proxy_config(proxy_config)
            .await
            .expect("set test proxy config");
        state
            .proxy_service
            .start()
            .await
            .expect("start proxy server");

        let config_a = r#"model_provider = "provider-a"
model = "model-a"

[model_providers.provider-a]
name = "ProviderA"
base_url = "https://provider-a.example/v1"
wire_api = "responses"
requires_openai_auth = true

[desktop]
appearanceTheme = "dark"
animations-enabled = false
live-only-setting = "keep"
"#;
        let config_b = r#"model_provider = "provider-b"
model = "model-b"

[model_providers.provider-b]
name = "ProviderB"
base_url = "https://provider-b.example/v1"
wire_api = "responses"
requires_openai_auth = true

[desktop]
appearanceTheme = "light"
animations-enabled = true
provider-only-setting = "stale"
"#;

        let provider_a = Provider::with_id(
            "a".to_string(),
            "ProviderA".to_string(),
            serde_json::json!({
                "auth": { "OPENAI_API_KEY": "key-a" },
                "config": config_a,
                "modelCatalog": { "models": [{ "model": "model-a" }] }
            }),
            None,
        );
        let mut provider_b = Provider::with_id(
            "b".to_string(),
            "ProviderB".to_string(),
            serde_json::json!({
                "auth": { "OPENAI_API_KEY": "key-b" },
                "config": config_b,
                "modelCatalog": { "models": [{ "model": "model-b" }] }
            }),
            None,
        );
        provider_b.meta = Some(ProviderMeta {
            common_config_enabled: Some(true),
            ..Default::default()
        });

        db.save_provider("codex", &provider_a)
            .expect("save provider a");
        db.save_provider("codex", &provider_b)
            .expect("save provider b");
        db.set_current_provider("codex", "a")
            .expect("set current provider a");
        crate::settings::set_current_provider(&AppType::Codex, Some("a"))
            .expect("set local current provider a");

        state
            .proxy_service
            .write_codex_live_for_provider(&provider_a.settings_config, Some(&provider_a))
            .expect("seed live codex config");
        assert!(
            !state
                .proxy_service
                .detect_takeover_in_live_config_for_app(&AppType::Codex),
            "seeded live config should not be proxy-taken-over"
        );

        db.save_live_backup(
            "codex",
            &serde_json::to_string(&provider_a.settings_config).expect("serialize backup"),
        )
        .await
        .expect("seed restored backup");

        crate::services::provider::ProviderService::switch(&state, AppType::Codex, "b")
            .expect("provider switch to provider b");
        state.proxy_service.stop().await.expect("stop proxy server");

        let catalog_path = crate::codex_config::get_codex_model_catalog_path();
        assert!(
            catalog_path.exists(),
            "nexus-model-catalog.json must be created on provider switch"
        );
        let catalog_text = std::fs::read_to_string(&catalog_path).expect("read catalog json");
        let catalog: serde_json::Value =
            serde_json::from_str(&catalog_text).expect("parse catalog json");
        let slugs: Vec<&str> = catalog
            .get("models")
            .and_then(|m| m.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| e.get("slug").and_then(|s| s.as_str()))
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            slugs.contains(&"model-b"),
            "catalog must contain provider B's model after switch; got: {slugs:?}"
        );
        assert!(
            !slugs.contains(&"model-a"),
            "catalog must not contain stale provider A model after switch; got: {slugs:?}"
        );

        let config_path = crate::codex_config::get_codex_config_path();
        let config_text = std::fs::read_to_string(&config_path).expect("read config.toml");
        assert!(
            config_text.contains("model_catalog_json"),
            "config.toml must reference model_catalog_json after switch"
        );
        assert!(
            config_text.contains("[mcp_servers.shared]"),
            "config.toml must keep common config after switch"
        );
        assert!(
            config_text.contains(r#"command = "shared-command""#),
            "config.toml must include common config content after switch"
        );
        let parsed: toml::Value = toml::from_str(&config_text).expect("parse switched config");
        let expected: toml::Value = toml::from_str(config_a).expect("parse expected config");
        assert_eq!(
            parsed.get("desktop"),
            expected.get("desktop"),
            "a restored-backup hot switch must keep the whole live desktop table"
        );
    }

    #[tokio::test]
    #[serial]
    async fn provider_switch_with_restored_codex_backup_propagates_catalog_write_errors() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");
        seed_codex_model_template();

        let db = Arc::new(Database::memory().expect("init db"));
        let state = crate::store::AppState::new(db.clone());

        let proxy_config = ProxyConfig {
            listen_port: 0,
            ..Default::default()
        };
        db.update_proxy_config(proxy_config)
            .await
            .expect("set test proxy config");
        state
            .proxy_service
            .start()
            .await
            .expect("start proxy server");

        let config_a = r#"model_provider = "provider-a"
model = "model-a"

[model_providers.provider-a]
name = "ProviderA"
base_url = "https://provider-a.example/v1"
wire_api = "responses"
requires_openai_auth = true
"#;
        let config_b = r#"model_provider = "provider-b"
model = "model-b"

[model_providers.provider-b]
name = "ProviderB"
base_url = "https://provider-b.example/v1"
wire_api = "responses"
requires_openai_auth = true
"#;

        let provider_a = Provider::with_id(
            "a".to_string(),
            "ProviderA".to_string(),
            serde_json::json!({
                "auth": { "OPENAI_API_KEY": "key-a" },
                "config": config_a,
                "modelCatalog": { "models": [{ "model": "model-a" }] }
            }),
            None,
        );
        let provider_b = Provider::with_id(
            "b".to_string(),
            "ProviderB".to_string(),
            serde_json::json!({
                "auth": { "OPENAI_API_KEY": "key-b" },
                "config": config_b,
                "modelCatalog": { "models": [{ "model": "model-b" }] }
            }),
            None,
        );

        db.save_provider("codex", &provider_a)
            .expect("save provider a");
        db.save_provider("codex", &provider_b)
            .expect("save provider b");
        db.set_current_provider("codex", "a")
            .expect("set current provider a");
        crate::settings::set_current_provider(&AppType::Codex, Some("a"))
            .expect("set local current provider a");

        state
            .proxy_service
            .write_codex_live_for_provider(&provider_a.settings_config, Some(&provider_a))
            .expect("seed live codex config");
        assert!(
            !state
                .proxy_service
                .detect_takeover_in_live_config_for_app(&AppType::Codex),
            "seeded live config should not be proxy-taken-over"
        );

        db.save_live_backup(
            "codex",
            &serde_json::to_string(&provider_a.settings_config).expect("serialize backup"),
        )
        .await
        .expect("seed restored backup");

        let catalog_path = crate::codex_config::get_codex_model_catalog_path();
        if catalog_path.exists() {
            std::fs::remove_file(&catalog_path).expect("remove catalog file");
        }
        std::fs::create_dir_all(&catalog_path).expect("turn catalog path into directory");

        let err = crate::services::provider::ProviderService::switch(&state, AppType::Codex, "b")
            .expect_err("provider switch should fail when catalog cannot be written");
        state.proxy_service.stop().await.expect("stop proxy server");

        let message = err.to_string();
        assert!(
            message.contains("Failed to write the Codex config")
                || message.contains("atomic replacement failed"),
            "switch should surface catalog write failure, got: {message}"
        );
    }

    /// Regression: turning proxy takeover off restores Live from the backup. The
    /// backup snapshot is `read_codex_live_settings()` output (`{auth, config}`,
    /// never an inline `modelCatalog`). The restore must NOT route the config
    /// through catalog projection, which would see no specs and strip the
    /// `model_catalog_json` pointer — silently dropping the user's Codex model
    /// mapping from Live even though the DB SSOT still holds it.
    #[tokio::test]
    #[serial]
    async fn codex_restore_from_backup_preserves_model_catalog_pointer() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let service = ProxyService::new(db.clone());

        // Pre-takeover Live state: config.toml points at the Nexus Composer generated
        // catalog file, and that file exists on disk (takeover never touches it).
        let catalog_path = crate::codex_config::get_codex_model_catalog_path();
        if let Some(parent) = catalog_path.parent() {
            std::fs::create_dir_all(parent).expect("create codex dir");
        }
        std::fs::write(
            &catalog_path,
            r#"{"models":[{"slug":"deepseek-v4-flash"}]}"#,
        )
        .expect("seed generated catalog file");

        let pointer = catalog_path.to_string_lossy().replace('\\', "/");
        let backup_config = format!(
            "model_provider = \"custom\"\n\
             model = \"deepseek-v4-flash\"\n\
             model_catalog_json = \"{pointer}\"\n\n\
             [model_providers.custom]\n\
             name = \"DeepSeek\"\n\
             base_url = \"https://api.deepseek.example/v1\"\n\
             wire_api = \"responses\"\n"
        );
        let backup_json = serde_json::to_string(&json!({
            "auth": { "OPENAI_API_KEY": "deepseek-key" },
            "config": backup_config,
        }))
        .expect("serialize backup");
        db.save_live_backup("codex", &backup_json)
            .await
            .expect("seed live backup");

        // Turning takeover off restores Live from this backup.
        service
            .restore_live_config_for_app_with_fallback(&AppType::Codex)
            .await
            .expect("restore codex live from backup");

        let restored = std::fs::read_to_string(crate::codex_config::get_codex_config_path())
            .expect("read restored config.toml");
        assert!(
            restored.contains("model_catalog_json"),
            "restore must preserve the model_catalog_json pointer, got:\n{restored}"
        );
        assert!(
            restored.contains(pointer.as_str()),
            "restored pointer must still reference the Nexus Composer generated catalog file"
        );
    }

    #[tokio::test]
    #[serial]
    async fn codex_restore_third_party_backup_preserves_live_oauth_when_enabled() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");
        crate::settings::update_settings(crate::settings::AppSettings {
            preserve_codex_official_auth_on_switch: true,
            ..Default::default()
        })
        .expect("enable Codex official auth preservation");

        let db = Arc::new(Database::memory().expect("init db"));
        let service = ProxyService::new(db.clone());
        let oauth_auth = json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": "oauth-id",
                "access_token": "oauth-access"
            }
        });
        crate::codex_config::write_codex_live_atomic(
            &oauth_auth,
            Some(
                "model_provider = \"custom\"\n\
                 [model_providers.custom]\n\
                 name = \"Proxy\"\n\
                 base_url = \"http://127.0.0.1:15721/v1\"\n\
                 wire_api = \"responses\"\n",
            ),
        )
        .expect("seed live OAuth auth and proxy config");

        let backup = json!({
            "auth": { "OPENAI_API_KEY": "provider-key" },
            "config": "model_provider = \"nexus\"\n\
                       model = \"glm52-logged\"\n\
                       [model_providers.nexus]\n\
                       name = \"Nexus\"\n\
                       base_url = \"https://nexus.example/v1\"\n\
                       wire_api = \"responses\"\n",
        });
        db.save_live_backup("codex", &backup.to_string())
            .await
            .expect("seed stale third-party backup");

        service
            .restore_live_config_for_app_with_fallback(&AppType::Codex)
            .await
            .expect("restore Codex backup");

        let restored_auth: Value =
            crate::config::read_json_file(&crate::codex_config::get_codex_auth_path())
                .expect("read restored auth");
        assert_eq!(
            restored_auth, oauth_auth,
            "restoring a third-party backup must not replace a live ChatGPT OAuth login"
        );
        let restored_config = std::fs::read_to_string(crate::codex_config::get_codex_config_path())
            .expect("read restored config");
        assert!(
            restored_config.contains("https://nexus.example/v1"),
            "the third-party route should still be restored"
        );

        crate::settings::update_settings(crate::settings::AppSettings::default())
            .expect("reset settings");
    }

    /// Regression: a hot-switch during takeover rebuilds the backup from the DB
    /// provider (`update_live_backup_from_provider`), so the backup carries an
    /// inline `modelCatalog` (DB SSOT) but a `config.toml` text WITHOUT a
    /// `model_catalog_json` pointer. Restoring that backup must project the
    /// inline catalog — (re)generating both the catalog file and the pointer —
    /// or the Codex model mapping vanishes from Live after takeover-off.
    #[tokio::test]
    #[serial]
    async fn codex_restore_from_backup_projects_inline_model_catalog() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let service = ProxyService::new(db.clone());

        // Catalog projection needs a model template; seed `models_cache.json`
        // with the template slug so we don't depend on the `codex` CLI.
        let codex_dir = crate::codex_config::get_codex_config_dir();
        std::fs::create_dir_all(&codex_dir).expect("create codex dir");
        std::fs::write(
            codex_dir.join("models_cache.json"),
            r#"{"models":[{"slug":"gpt-5.5"}]}"#,
        )
        .expect("seed models_cache template");

        // Provider-rebuilt backup shape: inline modelCatalog, pointer-less config.
        let backup_json = serde_json::to_string(&json!({
            "auth": { "OPENAI_API_KEY": "deepseek-key" },
            "config": "model_provider = \"custom\"\nmodel = \"deepseek-v4-flash\"\n\n[model_providers.custom]\nname = \"DeepSeek\"\nbase_url = \"https://api.deepseek.example/v1\"\nwire_api = \"responses\"\n",
            "modelCatalog": {
                "models": [
                    { "model": "deepseek-v4-flash", "displayName": "DeepSeek V4 Flash", "contextWindow": 1_000_000 }
                ]
            }
        }))
        .expect("serialize backup");
        db.save_live_backup("codex", &backup_json)
            .await
            .expect("seed live backup");

        service
            .restore_live_config_for_app_with_fallback(&AppType::Codex)
            .await
            .expect("restore codex live from backup");

        let restored = std::fs::read_to_string(crate::codex_config::get_codex_config_path())
            .expect("read restored config.toml");
        let catalog_path = crate::codex_config::get_codex_model_catalog_path();
        assert!(
            restored.contains("model_catalog_json"),
            "restore must (re)generate the model_catalog_json pointer from inline catalog, got:\n{restored}"
        );
        assert!(
            catalog_path.exists(),
            "restore must generate the nexus-composer catalog file on disk"
        );
        let catalog: Value = serde_json::from_str(
            &std::fs::read_to_string(&catalog_path).expect("read generated catalog"),
        )
        .expect("parse generated catalog");
        let slugs: Vec<&str> = catalog
            .get("models")
            .and_then(|m| m.as_array())
            .expect("catalog models")
            .iter()
            .filter_map(|m| m.get("slug").and_then(|s| s.as_str()))
            .collect();
        assert!(
            slugs.contains(&"deepseek-v4-flash"),
            "generated catalog must contain the inline model, got slugs: {slugs:?}"
        );
    }

    #[tokio::test]
    #[serial]
    async fn codex_restore_marked_native_backup_preserves_tool_profile() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let service = ProxyService::new(db.clone());
        let mut provider = Provider::with_id(
            "native".to_string(),
            "Native".to_string(),
            json!({
                "auth": { "OPENAI_API_KEY": "native-key" },
                "config": "model_provider = \"custom\"\nmodel = \"native-model\"\n",
                "modelCatalog": { "models": [{ "model": "native-model" }] }
            }),
            None,
        );
        provider.meta = Some(ProviderMeta {
            api_format: Some("openai_responses".to_string()),
            ..Default::default()
        });

        service
            .update_live_backup_from_provider("codex", &provider)
            .await
            .expect("rebuild native backup");
        let stored = db
            .get_live_backup("codex")
            .await
            .expect("read native backup")
            .expect("native backup exists");
        let backup: Value =
            serde_json::from_str(&stored.original_config).expect("parse native backup");
        assert_eq!(backup["apiFormat"], "openai_responses");

        service
            .restore_live_config_for_app_with_fallback(&AppType::Codex)
            .await
            .expect("restore native backup");
        let catalog: Value = serde_json::from_str(
            &std::fs::read_to_string(crate::codex_config::get_codex_model_catalog_path())
                .expect("read generated catalog"),
        )
        .expect("parse generated catalog");
        let entry = &catalog["models"][0];
        assert_eq!(entry["tool_mode"], "direct");
        assert!(entry.get("apply_patch_tool_type").is_none());
        assert!(entry.get("web_search_tool_type").is_none());
    }

    #[tokio::test]
    #[serial]
    async fn codex_restore_legacy_backup_does_not_infer_profile_from_matching_catalog() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let service = ProxyService::new(db.clone());
        let catalog = json!({"models": [{"model": "shared-model"}]});
        let mut current = Provider::with_id(
            "native".to_string(),
            "Native".to_string(),
            json!({"modelCatalog": catalog}),
            None,
        );
        current.meta = Some(ProviderMeta {
            api_format: Some("openai_responses".to_string()),
            ..Default::default()
        });
        db.save_provider("codex", &current)
            .expect("save current provider");
        db.set_current_provider("codex", &current.id)
            .expect("select current provider");
        crate::settings::set_current_provider(&AppType::Codex, Some(&current.id))
            .expect("select local current provider");

        let backup = json!({
            "auth": {"OPENAI_API_KEY": "chat-key"},
            "config": "model_provider = \"custom\"\nmodel = \"shared-model\"\n",
            "modelCatalog": catalog
        });
        db.save_live_backup("codex", &backup.to_string())
            .await
            .expect("save legacy chat backup");

        service
            .restore_live_config_for_app_with_fallback(&AppType::Codex)
            .await
            .expect("restore legacy chat backup");
        let catalog: Value = serde_json::from_str(
            &std::fs::read_to_string(crate::codex_config::get_codex_model_catalog_path())
                .expect("read generated catalog"),
        )
        .expect("parse generated catalog");
        let entry = &catalog["models"][0];
        assert_ne!(entry["tool_mode"], "direct");
        assert!(entry.get("apply_patch_tool_type").is_some());
    }

    #[tokio::test]
    #[serial]
    async fn codex_backup_rejects_non_object_provider_settings() {
        for settings in [Value::Null, json!([])] {
            let db = Arc::new(Database::memory().expect("init db"));
            let service = ProxyService::new(db.clone());
            let provider =
                Provider::with_id("invalid".to_string(), "Invalid".to_string(), settings, None);

            let error = service
                .update_live_backup_from_provider("codex", &provider)
                .await
                .expect_err("non-object settings must be rejected");
            assert_eq!(error, "Codex backup must be a JSON object");
            assert!(db
                .get_live_backup("codex")
                .await
                .expect("read backup")
                .is_none());
        }
    }

    /// Regression: a provider-rebuilt backup can pair an inline `modelCatalog`
    /// with EMPTY `auth.json` (`{}`) — the bearer-token / Mobile-compat shape
    /// where the API key lives in the config's `experimental_bearer_token`. The
    /// empty-auth restore branch deletes `auth.json` and writes config raw; it
    /// must still project the inline catalog (decision is orthogonal to auth), or
    /// the model mapping vanishes on takeover-off for this provider shape.
    #[tokio::test]
    #[serial]
    async fn codex_restore_empty_auth_backup_still_projects_inline_catalog() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let service = ProxyService::new(db.clone());

        let codex_dir = crate::codex_config::get_codex_config_dir();
        std::fs::create_dir_all(&codex_dir).expect("create codex dir");
        std::fs::write(
            codex_dir.join("models_cache.json"),
            r#"{"models":[{"slug":"gpt-5.5"}]}"#,
        )
        .expect("seed models_cache template");

        // Empty auth.json + key carried in config.toml's experimental_bearer_token,
        // plus the inline modelCatalog (DB SSOT).
        let backup_json = serde_json::to_string(&json!({
            "auth": {},
            "config": "model_provider = \"custom\"\nmodel = \"deepseek-v4-flash\"\n\n[model_providers.custom]\nname = \"DeepSeek\"\nbase_url = \"https://api.deepseek.example/v1\"\nwire_api = \"responses\"\nexperimental_bearer_token = \"sk-deepseek\"\n",
            "modelCatalog": {
                "models": [ { "model": "deepseek-v4-flash", "displayName": "DeepSeek V4 Flash" } ]
            }
        }))
        .expect("serialize backup");
        db.save_live_backup("codex", &backup_json)
            .await
            .expect("seed live backup");

        service
            .restore_live_config_for_app_with_fallback(&AppType::Codex)
            .await
            .expect("restore codex live from backup");

        let restored = std::fs::read_to_string(crate::codex_config::get_codex_config_path())
            .expect("read restored config.toml");
        assert!(
            restored.contains("model_catalog_json"),
            "empty-auth restore must still project the inline catalog pointer, got:\n{restored}"
        );
        assert!(
            crate::codex_config::get_codex_model_catalog_path().exists(),
            "empty-auth restore must generate the nexus-composer catalog file"
        );
        assert!(
            !crate::codex_config::get_codex_auth_path().exists(),
            "empty-auth restore must delete auth.json rather than write an empty one"
        );
    }

    /// Regression: when the backup row itself contains the proxy placeholder
    /// (a corrupted state where previous start/stop cycles saved the proxy
    /// config as the "original Live"), restore must NOT write it back to Live.
    /// It should fall through to the SSOT (current provider) path and rebuild
    /// Live from the provider DB instead.
    #[tokio::test]
    #[serial]
    async fn restore_falls_through_to_ssot_when_backup_is_proxy_placeholder() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let service = ProxyService::new(db.clone());

        // Seed DB with a current provider that has a real API key
        let provider = Provider::with_id(
            "p1".to_string(),
            "P1".to_string(),
            json!({
                "env": {
                    "ANTHROPIC_BASE_URL": "https://api.minimaxi.com/anthropic",
                    "ANTHROPIC_API_KEY": "real-key-from-db"
                }
            }),
            None,
        );
        db.save_provider("claude", &provider)
            .expect("save provider");
        db.set_current_provider("claude", "p1")
            .expect("set current provider");

        // Seed backup with proxy placeholder (the corrupted state)
        let corrupted_backup = serde_json::to_string(&json!({
            "env": {
                "ANTHROPIC_AUTH_TOKEN": PROXY_TOKEN_PLACEHOLDER,
                "ANTHROPIC_BASE_URL": "http://127.0.0.1:15721"
            }
        }))
        .expect("serialize corrupted backup");
        db.save_live_backup("claude", &corrupted_backup)
            .await
            .expect("seed corrupted backup");

        // Seed Live with the same proxy placeholder (matches the corrupted state)
        service
            .write_claude_live(&json!({
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": PROXY_TOKEN_PLACEHOLDER,
                    "ANTHROPIC_BASE_URL": "http://127.0.0.1:15721"
                }
            }))
            .expect("seed taken-over live file");

        // Restore: must NOT use the corrupted backup
        service
            .restore_live_config_for_app_with_fallback(&AppType::Claude)
            .await
            .expect("restore should succeed via SSOT");

        // The backup should still be the corrupted one (we didn't touch it on this path)
        let backup_after = db
            .get_live_backup("claude")
            .await
            .expect("get backup")
            .expect("backup still exists");
        assert_eq!(
            backup_after.original_config, corrupted_backup,
            "restore must NOT overwrite the corrupted backup"
        );

        // Live should now reflect the SSOT (provider DB), NOT the proxy URL
        let restored_live = service.read_claude_live().expect("read live");
        let restored_url = restored_live
            .get("env")
            .and_then(|env| env.get("ANTHROPIC_BASE_URL"))
            .and_then(|v| v.as_str());
        assert_eq!(
            restored_url,
            Some("https://api.minimaxi.com/anthropic"),
            "Live must be rebuilt from SSOT, not from the corrupted backup"
        );
        let restored_key = restored_live
            .get("env")
            .and_then(|env| env.get("ANTHROPIC_API_KEY"))
            .and_then(|v| v.as_str());
        assert_eq!(
            restored_key,
            Some("real-key-from-db"),
            "Live must carry the real API key from the provider DB"
        );
        assert_ne!(
            restored_live
                .get("env")
                .and_then(|env| env.get("ANTHROPIC_AUTH_TOKEN"))
                .and_then(|v| v.as_str()),
            Some(PROXY_TOKEN_PLACEHOLDER),
            "Live must not still carry the proxy placeholder"
        );
    }

    /// Regression: when Live is already a proxy placeholder (a corrupted state
    /// where previous stop failed to restore), backup must NOT overwrite a
    /// previously-good backup with the proxy config. This prevents the bug
    /// where stop-then-start cycles permanently corrupt the backup.
    #[tokio::test]
    #[serial]
    async fn backup_skips_when_live_is_already_proxy_placeholder() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let service = ProxyService::new(db.clone());

        // Seed a GOOD backup (the "real" original Live)
        let good_backup = serde_json::to_string(&json!({
            "env": {
                "ANTHROPIC_BASE_URL": "https://api.minimaxi.com/anthropic",
                "ANTHROPIC_AUTH_TOKEN": "real-token"
            }
        }))
        .expect("serialize good backup");
        db.save_live_backup("claude", &good_backup)
            .await
            .expect("seed good backup");

        // Seed Live with proxy placeholder (the corrupted state)
        service
            .write_claude_live(&json!({
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": PROXY_TOKEN_PLACEHOLDER,
                    "ANTHROPIC_BASE_URL": "http://127.0.0.1:15721"
                }
            }))
            .expect("seed taken-over live file");

        // Call backup_live_config_strict: must skip
        service
            .backup_live_config_strict(&AppType::Claude)
            .await
            .expect("backup should succeed (no-op when live is placeholder)");

        // The good backup must still be intact
        let backup_after = db
            .get_live_backup("claude")
            .await
            .expect("get backup")
            .expect("backup still exists");
        assert_eq!(
            backup_after.original_config, good_backup,
            "must not overwrite a good backup with a proxy placeholder"
        );
    }

    /// Regression: when ALL apps have Live=proxy-placeholder (worst-case
    /// corrupted state), the bulk `backup_live_configs` path used by
    /// `start_with_takeover` must skip every save — instead of overwriting
    /// good backups with the proxy config.
    #[tokio::test]
    #[serial]
    async fn bulk_backup_skips_all_when_live_is_proxy_placeholder() {
        let _home = TempHome::new();
        crate::settings::reload_settings().expect("reload settings");

        let db = Arc::new(Database::memory().expect("init db"));
        let service = ProxyService::new(db.clone());

        // Seed good backups for all three apps
        let good_backup = serde_json::to_string(&json!({
            "env": {
                "ANTHROPIC_AUTH_TOKEN": "real-token"
            }
        }))
        .expect("serialize good backup");
        db.save_live_backup("claude", &good_backup)
            .await
            .expect("seed claude backup");

        let codex_good_backup = serde_json::to_string(&json!({
            "auth": { "OPENAI_API_KEY": "real-codex-token" }
        }))
        .expect("serialize codex good backup");
        db.save_live_backup("codex", &codex_good_backup)
            .await
            .expect("seed codex backup");

        let gemini_good_backup = serde_json::to_string(&json!({
            "env": { "GEMINI_API_KEY": "real-gemini-key" }
        }))
        .expect("serialize gemini good backup");
        db.save_live_backup("gemini", &gemini_good_backup)
            .await
            .expect("seed gemini backup");

        // Seed all three Live files with proxy placeholders
        service
            .write_claude_live(&json!({
                "env": {
                    "ANTHROPIC_AUTH_TOKEN": PROXY_TOKEN_PLACEHOLDER,
                    "ANTHROPIC_BASE_URL": "http://127.0.0.1:15721"
                }
            }))
            .expect("seed claude live");
        let codex_dir = crate::codex_config::get_codex_config_dir();
        std::fs::create_dir_all(&codex_dir).expect("create codex dir");
        std::fs::write(
            crate::codex_config::get_codex_config_path(),
            r#"model_provider = "custom"

[model_providers.custom]
name = "Custom"
base_url = "http://127.0.0.1:15721/v1"
wire_api = "chat"
experimental_bearer_token = "PROXY_MANAGED"
"#,
        )
        .expect("seed codex config.toml");
        std::fs::write(
            crate::codex_config::get_codex_auth_path(),
            r#"{"OPENAI_API_KEY":"PROXY_MANAGED"}"#,
        )
        .expect("seed codex auth.json");
        let gemini_env_path = crate::gemini_config::get_gemini_env_path();
        if let Some(parent) = gemini_env_path.parent() {
            std::fs::create_dir_all(parent).expect("create gemini dir");
        }
        std::fs::write(&gemini_env_path, "GEMINI_API_KEY=PROXY_MANAGED\n")
            .expect("seed gemini env");

        // Call bulk backup: must skip all three apps
        service
            .backup_live_configs()
            .await
            .expect("bulk backup should succeed (no-op when all live are placeholders)");

        // All three good backups must still be intact
        for (app_type, original) in [
            ("claude", good_backup.as_str()),
            ("codex", codex_good_backup.as_str()),
            ("gemini", gemini_good_backup.as_str()),
        ] {
            let backup_after = db
                .get_live_backup(app_type)
                .await
                .expect("get backup")
                .expect("backup still exists");
            assert_eq!(
                backup_after.original_config, original,
                "must not overwrite good backup for {app_type} with proxy placeholder"
            );
        }
    }
}
