//! Claude MCP synchronization and import

use serde_json::Value;
use std::collections::HashMap;

use crate::app_config::{McpApps, McpConfig, McpServer, MultiAppConfig};
use crate::error::AppError;

use super::validation::{extract_server_spec, validate_server_spec};

fn should_sync_claude_mcp() -> bool {
    // When Claude is not installed or initialized, ~/.claude and ~/.claude.json are absent.
    // Respect user preference by skipping writes and deletes without creating any paths.
    crate::config::get_claude_config_dir().exists() || crate::config::get_claude_mcp_path().exists()
}

/// Return MCP servers whose enabled field is true.
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

/// Project enabled config.json entries into ~/.claude.json.
pub fn sync_enabled_to_claude(config: &MultiAppConfig) -> Result<(), AppError> {
    if !should_sync_claude_mcp() {
        return Ok(());
    }
    let enabled = collect_enabled_servers(&config.mcp.claude);
    crate::claude_mcp::set_mcp_servers_map(&enabled)
}

/// Import mcpServers from ~/.claude.json into the unified v3.7.0+ structure.
/// Existing servers enable Claude without overwriting other fields or application states.
pub fn import_from_claude(config: &mut MultiAppConfig) -> Result<usize, AppError> {
    let text_opt = crate::claude_mcp::read_mcp_json()?;
    let Some(text) = text_opt else { return Ok(0) };

    let v: Value = serde_json::from_str(&text)
        .map_err(|e| AppError::McpValidation(format!("Failed to parse ~/.claude.json: {e}")))?;
    let Some(map) = v.get("mcpServers").and_then(|x| x.as_object()) else {
        return Ok(0);
    };

    // Ensure the unified structure exists.
    let servers = config.mcp.servers.get_or_insert_with(HashMap::new);

    let mut changed = 0;
    let mut errors = Vec::new();

    for (id, spec) in map.iter() {
        // Collect per-entry validation failures without stopping the import.
        if let Err(e) = validate_server_spec(spec) {
            log::warn!("Skipping invalid MCP server '{id}': {e}");
            errors.push(format!("{id}: {e}"));
            continue;
        }

        if let Some(existing) = servers.get_mut(id) {
            // Enable Claude on an existing server only.
            if !existing.apps.claude {
                existing.apps.claude = true;
                changed += 1;
                log::info!("Enabled Claude for MCP server '{id}'");
            }
        } else {
            // Create a server enabled only for Claude by default.
            servers.insert(
                id.clone(),
                McpServer {
                    id: id.clone(),
                    name: id.clone(),
                    server: spec.clone(),
                    apps: McpApps {
                        claude: true,
                        codex: false,
                        gemini: false,
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
            "Import completed with {} failures: {:?}",
            errors.len(),
            errors
        );
    }

    Ok(changed)
}

/// Synchronize one MCP server to the Claude live configuration.
pub fn sync_single_server_to_claude(
    _config: &MultiAppConfig,
    id: &str,
    server_spec: &Value,
) -> Result<(), AppError> {
    if !should_sync_claude_mcp() {
        return Ok(());
    }
    // Read the current MCP configuration.
    let current = crate::claude_mcp::read_mcp_servers_map()?;

    // Add the selected server to the existing server map.
    let mut updated = current;
    updated.insert(id.to_string(), server_spec.clone());

    // Write the updated map.
    crate::claude_mcp::set_mcp_servers_map(&updated)
}

/// Remove one MCP server from the Claude live configuration.
pub fn remove_server_from_claude(id: &str) -> Result<(), AppError> {
    if !should_sync_claude_mcp() {
        return Ok(());
    }
    // Read the current MCP configuration.
    let mut current = crate::claude_mcp::read_mcp_servers_map()?;

    // Remove the selected server.
    current.remove(id);

    // Write the updated map.
    crate::claude_mcp::set_mcp_servers_map(&current)
}
