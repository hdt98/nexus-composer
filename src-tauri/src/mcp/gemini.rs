//! Gemini MCP synchronization and import.

use serde_json::Value;
use std::collections::HashMap;

use crate::app_config::{McpApps, McpConfig, McpServer, MultiAppConfig};
use crate::error::AppError;

use super::validation::{extract_server_spec, validate_server_spec};

fn should_sync_gemini_mcp() -> bool {
    // The ~/.gemini directory is absent when Gemini is not installed or initialized.
    // Honor the user's setup by skipping writes and deletes without creating files or directories.
    crate::gemini_config::get_gemini_dir().exists()
}

/// Returns MCP servers whose `enabled` field is true.
fn collect_enabled_servers(cfg: &McpConfig) -> HashMap<String, Value> {
    let mut out = HashMap::new();
    for (id, entry) in cfg.servers.iter() {
        let enabled = entry
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !enabled {
            continue;
        }
        match extract_server_spec(entry) {
            Ok(spec) => {
                out.insert(id.clone(), spec);
            }
            Err(err) => {
                log::warn!("Skipping invalid MCP entry '{id}': {err}");
            }
        }
    }
    out
}

/// Writes enabled Gemini entries from config.json to the Gemini MCP configuration.
pub fn sync_enabled_to_gemini(config: &MultiAppConfig) -> Result<(), AppError> {
    if !should_sync_gemini_mcp() {
        return Ok(());
    }
    let enabled = collect_enabled_servers(&config.mcp.gemini);
    crate::gemini_mcp::set_mcp_servers_map(&enabled)
}

/// Imports the Gemini MCP configuration into the unified model (v3.7.0+).
/// Existing servers enable Gemini without replacing other fields or application states.
pub fn import_from_gemini(config: &mut MultiAppConfig) -> Result<usize, AppError> {
    let map = crate::gemini_mcp::read_mcp_servers_map()?;
    if map.is_empty() {
        return Ok(0);
    }

    // Ensure the unified server map exists.
    let servers = config.mcp.servers.get_or_insert_with(HashMap::new);

    let mut changed = 0;
    let mut errors = Vec::new();

    for (id, spec) in map.iter() {
        // Collect invalid entries without stopping the remaining imports.
        if let Err(e) = validate_server_spec(spec) {
            log::warn!("Skipping invalid MCP server '{id}': {e}");
            errors.push(format!("{id}: {e}"));
            continue;
        }

        if let Some(existing) = servers.get_mut(id) {
            // Only enable Gemini on an existing server.
            if !existing.apps.gemini {
                existing.apps.gemini = true;
                changed += 1;
                log::info!("Enabled Gemini for MCP server '{id}'");
            }
        } else {
            // Enable only Gemini by default for a new server.
            servers.insert(
                id.clone(),
                McpServer {
                    id: id.clone(),
                    name: id.clone(),
                    server: spec.clone(),
                    apps: McpApps {
                        claude: false,
                        codex: false,
                        gemini: true,
                        opencode: false,
                        hermes: false,
                    },
                    description: None,
                    homepage: None,
                    docs: None,
                    tags: Vec::new(),
                },
            );
            changed += 1;
            log::info!("Imported new MCP server '{id}'");
        }
    }

    if !errors.is_empty() {
        log::warn!(
            "Import completed with {} failed entries: {:?}",
            errors.len(),
            errors
        );
    }

    Ok(changed)
}

/// Synchronizes one MCP server to the live Gemini configuration.
pub fn sync_single_server_to_gemini(
    _config: &MultiAppConfig,
    id: &str,
    server_spec: &Value,
) -> Result<(), AppError> {
    if !should_sync_gemini_mcp() {
        return Ok(());
    }
    // Read the current MCP configuration.
    let mut current = crate::gemini_mcp::read_mcp_servers_map()?;

    // Add or update the server.
    current.insert(id.to_string(), server_spec.clone());

    // Write the updated configuration.
    crate::gemini_mcp::set_mcp_servers_map(&current)
}

/// Removes one MCP server from the live Gemini configuration.
pub fn remove_server_from_gemini(id: &str) -> Result<(), AppError> {
    if !should_sync_gemini_mcp() {
        return Ok(());
    }
    // Read the current MCP configuration.
    let mut current = crate::gemini_mcp::read_mcp_servers_map()?;

    // Remove the requested server.
    current.remove(id);

    // Write the updated configuration.
    crate::gemini_mcp::set_mcp_servers_map(&current)
}
