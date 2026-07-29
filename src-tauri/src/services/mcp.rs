use indexmap::IndexMap;
use std::collections::HashMap;

use crate::app_config::{AppType, McpServer};
use crate::error::AppError;
use crate::mcp;
use crate::store::AppState;

/// MCP business logic using the unified structure introduced in v3.7.0.
pub struct McpService;

impl McpService {
    /// Returns all MCP servers in the unified structure.
    pub fn get_all_servers(state: &AppState) -> Result<IndexMap<String, McpServer>, AppError> {
        state.db.get_all_mcp_servers()
    }

    /// Adds or updates an MCP server.
    pub fn upsert_server(state: &AppState, server: McpServer) -> Result<(), AppError> {
        // Read the previous state so an application deselected during editing can be removed from
        // its live configuration.
        let prev_apps = state
            .db
            .get_all_mcp_servers()?
            .get(&server.id)
            .map(|s| s.apps.clone())
            .unwrap_or_default();

        state.db.save_mcp_server(&server)?;

        // When an application changes from enabled to disabled, remove the server from its live
        // configuration.
        if prev_apps.claude && !server.apps.claude {
            Self::remove_server_from_app(state, &server.id, &AppType::Claude)?;
        }
        if prev_apps.codex && !server.apps.codex {
            Self::remove_server_from_app(state, &server.id, &AppType::Codex)?;
        }
        if prev_apps.gemini && !server.apps.gemini {
            Self::remove_server_from_app(state, &server.id, &AppType::Gemini)?;
        }
        if prev_apps.opencode && !server.apps.opencode {
            Self::remove_server_from_app(state, &server.id, &AppType::OpenCode)?;
        }
        if prev_apps.hermes && !server.apps.hermes {
            Self::remove_server_from_app(state, &server.id, &AppType::Hermes)?;
        }

        // Synchronize to every enabled application.
        Self::sync_server_to_apps(state, &server)?;

        Ok(())
    }

    /// Deletes an MCP server.
    pub fn delete_server(state: &AppState, id: &str) -> Result<bool, AppError> {
        let server = state.db.get_all_mcp_servers()?.shift_remove(id);

        if let Some(server) = server {
            state.db.delete_mcp_server(id)?;

            // Remove it from every application's live configuration.
            Self::remove_server_from_all_apps(state, id, &server)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Toggles enablement for a specified application.
    pub fn toggle_app(
        state: &AppState,
        server_id: &str,
        app: AppType,
        enabled: bool,
    ) -> Result<(), AppError> {
        let mut servers = state.db.get_all_mcp_servers()?;

        if let Some(server) = servers.get_mut(server_id) {
            server.apps.set_enabled_for(&app, enabled);
            state.db.save_mcp_server(server)?;

            // Synchronize to the corresponding application.
            if enabled {
                Self::sync_server_to_app(state, server, &app)?;
            } else {
                Self::remove_server_from_app(state, server_id, &app)?;
            }
        }

        Ok(())
    }

    /// Synchronizes an MCP server to all enabled applications.
    fn sync_server_to_apps(_state: &AppState, server: &McpServer) -> Result<(), AppError> {
        for app in server.apps.enabled_apps() {
            Self::sync_server_to_app_no_config(server, &app)?;
        }

        Ok(())
    }

    /// Synchronizes an MCP server to a specified application.
    fn sync_server_to_app(
        _state: &AppState,
        server: &McpServer,
        app: &AppType,
    ) -> Result<(), AppError> {
        Self::sync_server_to_app_no_config(server, app)
    }

    fn sync_server_to_app_no_config(server: &McpServer, app: &AppType) -> Result<(), AppError> {
        match app {
            AppType::Claude => {
                mcp::sync_single_server_to_claude(&Default::default(), &server.id, &server.server)?;
            }
            AppType::ClaudeDesktop => {
                log::debug!(
                    "Claude Desktop 3P profiles do not use Nexus Composer MCP sync, skipping"
                );
            }
            AppType::Codex => {
                // Codex uses TOML format, must use the correct function
                mcp::sync_single_server_to_codex(&Default::default(), &server.id, &server.server)?;
            }
            AppType::Gemini => {
                mcp::sync_single_server_to_gemini(&Default::default(), &server.id, &server.server)?;
            }
            AppType::OpenCode => {
                mcp::sync_single_server_to_opencode(
                    &Default::default(),
                    &server.id,
                    &server.server,
                )?;
            }
            AppType::OpenClaw => {
                // OpenClaw MCP support is still in development (Issue #4834)
                // Skip for now
                log::debug!("OpenClaw MCP support is still in development, skipping sync");
            }
            AppType::Hermes => {
                mcp::sync_single_server_to_hermes(&Default::default(), &server.id, &server.server)?;
            }
        }
        Ok(())
    }

    /// Removes an MCP server from every application where it was enabled.
    fn remove_server_from_all_apps(
        state: &AppState,
        id: &str,
        server: &McpServer,
    ) -> Result<(), AppError> {
        // Remove it from every previously enabled application.
        for app in server.apps.enabled_apps() {
            Self::remove_server_from_app(state, id, &app)?;
        }
        Ok(())
    }

    fn remove_server_from_app(_state: &AppState, id: &str, app: &AppType) -> Result<(), AppError> {
        match app {
            AppType::Claude => mcp::remove_server_from_claude(id)?,
            AppType::ClaudeDesktop => {
                log::debug!(
                    "Claude Desktop 3P profiles do not use Nexus Composer MCP sync, skipping"
                );
            }
            AppType::Codex => mcp::remove_server_from_codex(id)?,
            AppType::Gemini => mcp::remove_server_from_gemini(id)?,
            AppType::OpenCode => {
                mcp::remove_server_from_opencode(id)?;
            }
            AppType::OpenClaw => {
                // OpenClaw MCP support is still in development
                log::debug!("OpenClaw MCP support is still in development, skipping remove");
            }
            AppType::Hermes => {
                mcp::remove_server_from_hermes(id)?;
            }
        }
        Ok(())
    }

    /// Manually synchronizes all enabled MCP servers to their applications.
    pub fn sync_all_enabled(state: &AppState) -> Result<(), AppError> {
        let servers = Self::get_all_servers(state)?;

        for app in AppType::all() {
            if matches!(app, AppType::OpenClaw | AppType::ClaudeDesktop) {
                continue;
            }

            for server in servers.values() {
                if server.apps.is_enabled_for(&app) {
                    Self::sync_server_to_app(state, server, &app)?;
                } else {
                    Self::remove_server_from_app(state, &server.id, &app)?;
                }
            }
        }

        Ok(())
    }

    // ========================================================================
    // Compatibility layer for deprecated v3.6.x commands, scheduled for removal in v4.0.
    // ========================================================================

    /// Deprecated: returns MCP servers for a specified application through the legacy API.
    #[deprecated(since = "3.7.0", note = "Use get_all_servers instead")]
    pub fn get_servers(
        state: &AppState,
        app: AppType,
    ) -> Result<HashMap<String, serde_json::Value>, AppError> {
        let all_servers = Self::get_all_servers(state)?;
        let mut result = HashMap::new();

        for (id, server) in all_servers {
            if server.apps.is_enabled_for(&app) {
                result.insert(id, server.server);
            }
        }

        Ok(result)
    }

    /// Deprecated: sets MCP server enablement for a specified application through the legacy API.
    #[deprecated(since = "3.7.0", note = "Use toggle_app instead")]
    pub fn set_enabled(
        state: &AppState,
        app: AppType,
        id: &str,
        enabled: bool,
    ) -> Result<bool, AppError> {
        Self::toggle_app(state, id, app, enabled)?;
        Ok(true)
    }

    /// Deprecated: synchronizes enabled MCP servers to a specified application through the legacy API.
    #[deprecated(since = "3.7.0", note = "Use sync_all_enabled instead")]
    pub fn sync_enabled(state: &AppState, app: AppType) -> Result<(), AppError> {
        let servers = Self::get_all_servers(state)?;

        for server in servers.values() {
            if server.apps.is_enabled_for(&app) {
                Self::sync_server_to_app(state, server, &app)?;
            }
        }

        Ok(())
    }

    /// Imports MCP configuration from Claude into the unified v3.7.0 structure.
    pub fn import_from_claude(state: &AppState) -> Result<usize, AppError> {
        // Create a temporary MultiAppConfig for import.
        let mut temp_config = crate::app_config::MultiAppConfig::default();

        // Invoke the existing import logic from mcp.rs.
        let count = crate::mcp::import_from_claude(&mut temp_config)?;

        let mut new_count = 0;

        // Save imported servers to the database when any exist.
        if count > 0 {
            if let Some(servers) = &temp_config.mcp.servers {
                let mut existing = state.db.get_all_mcp_servers()?;
                for server in servers.values() {
                    // Existing server: enable only Claude without overwriting other fields, matching
                    // import-module semantics.
                    let to_save = if let Some(existing_server) = existing.get(&server.id) {
                        let mut merged = existing_server.clone();
                        merged.apps.claude = true;
                        merged
                    } else {
                        // A genuinely new server.
                        new_count += 1;
                        server.clone()
                    };

                    state.db.save_mcp_server(&to_save)?;
                    existing.insert(to_save.id.clone(), to_save.clone());

                    // Import reads existing configuration and must not write back to any application's
                    // live configuration. Write only after explicit editing, toggling, or manual sync.
                }
            }
        }

        Ok(new_count)
    }

    /// Imports MCP configuration from Codex into the unified v3.7.0 structure.
    pub fn import_from_codex(state: &AppState) -> Result<usize, AppError> {
        // Create a temporary MultiAppConfig for import.
        let mut temp_config = crate::app_config::MultiAppConfig::default();

        // Invoke the existing import logic from mcp.rs.
        let count = crate::mcp::import_from_codex(&mut temp_config)?;

        let mut new_count = 0;

        // Save imported servers to the database when any exist.
        if count > 0 {
            if let Some(servers) = &temp_config.mcp.servers {
                let mut existing = state.db.get_all_mcp_servers()?;
                for server in servers.values() {
                    // Existing server: enable only Codex without overwriting other fields, matching
                    // import-module semantics.
                    let to_save = if let Some(existing_server) = existing.get(&server.id) {
                        let mut merged = existing_server.clone();
                        merged.apps.codex = true;
                        merged
                    } else {
                        // A genuinely new server.
                        new_count += 1;
                        server.clone()
                    };

                    state.db.save_mcp_server(&to_save)?;
                    existing.insert(to_save.id.clone(), to_save.clone());

                    // Import reads existing configuration and must not write back to any application's
                    // live configuration. Write only after explicit editing, toggling, or manual sync.
                }
            }
        }

        Ok(new_count)
    }

    /// Imports MCP configuration from Gemini into the unified v3.7.0 structure.
    pub fn import_from_gemini(state: &AppState) -> Result<usize, AppError> {
        // Create a temporary MultiAppConfig for import.
        let mut temp_config = crate::app_config::MultiAppConfig::default();

        // Invoke the existing import logic from mcp.rs.
        let count = crate::mcp::import_from_gemini(&mut temp_config)?;

        let mut new_count = 0;

        // Save imported servers to the database when any exist.
        if count > 0 {
            if let Some(servers) = &temp_config.mcp.servers {
                let mut existing = state.db.get_all_mcp_servers()?;
                for server in servers.values() {
                    // Existing server: enable only Gemini without overwriting other fields, matching
                    // import-module semantics.
                    let to_save = if let Some(existing_server) = existing.get(&server.id) {
                        let mut merged = existing_server.clone();
                        merged.apps.gemini = true;
                        merged
                    } else {
                        // A genuinely new server.
                        new_count += 1;
                        server.clone()
                    };

                    state.db.save_mcp_server(&to_save)?;
                    existing.insert(to_save.id.clone(), to_save.clone());

                    // Import reads existing configuration and must not write back to any application's
                    // live configuration. Write only after explicit editing, toggling, or manual sync.
                }
            }
        }

        Ok(new_count)
    }

    /// Imports MCP configuration from OpenCode, supported since v3.9.2.
    pub fn import_from_opencode(state: &AppState) -> Result<usize, AppError> {
        // Create a temporary MultiAppConfig for import.
        let mut temp_config = crate::app_config::MultiAppConfig::default();

        // Invoke the existing import logic from mcp/opencode.rs.
        let count = crate::mcp::import_from_opencode(&mut temp_config)?;

        let mut new_count = 0;

        // Save imported servers to the database when any exist.
        if count > 0 {
            if let Some(servers) = &temp_config.mcp.servers {
                let mut existing = state.db.get_all_mcp_servers()?;
                for server in servers.values() {
                    // Existing server: enable only OpenCode without overwriting other fields, matching
                    // import-module semantics.
                    let to_save = if let Some(existing_server) = existing.get(&server.id) {
                        let mut merged = existing_server.clone();
                        merged.apps.opencode = true;
                        merged
                    } else {
                        // A genuinely new server.
                        new_count += 1;
                        server.clone()
                    };

                    state.db.save_mcp_server(&to_save)?;
                    existing.insert(to_save.id.clone(), to_save.clone());

                    // Import reads existing configuration and must not write back to any application's
                    // live configuration. Write only after explicit editing, toggling, or manual sync.
                }
            }
        }

        Ok(new_count)
    }

    /// Imports MCP configuration from Hermes.
    pub fn import_from_hermes(state: &AppState) -> Result<usize, AppError> {
        // Create a temporary MultiAppConfig for import.
        let mut temp_config = crate::app_config::MultiAppConfig::default();

        // Invoke the import logic from mcp/hermes.rs.
        let count = crate::mcp::import_from_hermes(&mut temp_config)?;

        let mut new_count = 0;

        // Save imported servers to the database when any exist.
        if count > 0 {
            if let Some(servers) = &temp_config.mcp.servers {
                let mut existing = state.db.get_all_mcp_servers()?;
                for server in servers.values() {
                    // Existing server: enable only Hermes without overwriting other fields, matching
                    // import-module semantics.
                    let to_save = if let Some(existing_server) = existing.get(&server.id) {
                        let mut merged = existing_server.clone();
                        merged.apps.hermes = true;
                        merged
                    } else {
                        // A genuinely new server.
                        new_count += 1;
                        server.clone()
                    };

                    state.db.save_mcp_server(&to_save)?;
                    existing.insert(to_save.id.clone(), to_save.clone());

                    // Import reads existing configuration and must not write back to any application's
                    // live configuration. Write only after explicit editing, toggling, or manual sync.
                }
            }
        }

        Ok(new_count)
    }
}
