//! Codex MCP synchronization and import module.
//!
//! Manages Codex MCP configuration:
//! - Imports from ~/.codex/config.toml.
//! - Synchronizes to ~/.codex/config.toml.
//! - Converts JSON to TOML.

use serde_json::{json, Value};
use std::collections::HashMap;

use crate::app_config::{McpApps, McpConfig, McpServer, MultiAppConfig};
use crate::error::AppError;

use super::validation::{extract_server_spec, validate_server_spec};

fn should_sync_codex_mcp() -> bool {
    // When Codex is not installed or initialized, ~/.codex does not exist. Respect user preferences
    // by skipping writes and removals without creating files or directories.
    crate::codex_config::get_codex_config_dir().exists()
}

/// Returns enabled MCP servers by filtering for enabled==true.
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

/// Imports MCP configuration from ~/.codex/config.toml into the unified structure introduced in v3.7.0.
///
/// Supported formats:
/// - Correct format: [mcp_servers.*], the official Codex standard.
/// - Incorrect format: [mcp.servers.*], read tolerantly to migrate miswritten configuration.
///
/// Existing servers enable Codex without overwriting other fields or application states.
pub fn import_from_codex(config: &mut MultiAppConfig) -> Result<usize, AppError> {
    let text = crate::codex_config::read_and_validate_codex_config_text()?;
    if text.trim().is_empty() {
        return Ok(0);
    }

    let root: toml::Table = toml::from_str(&text).map_err(|e| {
        AppError::McpValidation(format!("Failed to parse ~/.codex/config.toml: {e}"))
    })?;

    // Ensure the new structure exists.
    let servers = config.mcp.servers.get_or_insert_with(HashMap::new);

    let mut changed_total = 0usize;

    // Helper that processes one group of server tables.
    let mut import_servers_tbl = |servers_tbl: &toml::value::Table| {
        let mut changed = 0usize;
        for (id, entry_val) in servers_tbl.iter() {
            let Some(entry_tbl) = entry_val.as_table() else {
                continue;
            };

            // A missing type defaults to stdio.
            let typ = entry_tbl
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("stdio");

            // Build the JSON specification.
            let mut spec = serde_json::Map::new();
            spec.insert("type".into(), json!(typ));

            // Core fields that require explicit handling.
            let core_fields = match typ {
                "stdio" => vec!["type", "command", "args", "env", "cwd"],
                "http" | "sse" => vec!["type", "url", "http_headers"],
                _ => vec!["type"],
            };

            // 1. Process strongly typed core fields.
            match typ {
                "stdio" => {
                    if let Some(cmd) = entry_tbl.get("command").and_then(|v| v.as_str()) {
                        spec.insert("command".into(), json!(cmd));
                    }
                    if let Some(args) = entry_tbl.get("args").and_then(|v| v.as_array()) {
                        let arr = args
                            .iter()
                            .filter_map(|x| x.as_str())
                            .map(|s| json!(s))
                            .collect::<Vec<_>>();
                        if !arr.is_empty() {
                            spec.insert("args".into(), serde_json::Value::Array(arr));
                        }
                    }
                    if let Some(cwd) = entry_tbl.get("cwd").and_then(|v| v.as_str()) {
                        if !cwd.trim().is_empty() {
                            spec.insert("cwd".into(), json!(cwd));
                        }
                    }
                    if let Some(env_tbl) = entry_tbl.get("env").and_then(|v| v.as_table()) {
                        let mut env_json = serde_json::Map::new();
                        for (k, v) in env_tbl.iter() {
                            if let Some(sv) = v.as_str() {
                                env_json.insert(k.clone(), json!(sv));
                            }
                        }
                        if !env_json.is_empty() {
                            spec.insert("env".into(), serde_json::Value::Object(env_json));
                        }
                    }
                }
                "http" | "sse" => {
                    if let Some(url) = entry_tbl.get("url").and_then(|v| v.as_str()) {
                        spec.insert("url".into(), json!(url));
                    }
                    // Read from http_headers (correct Codex format) or headers (legacy) with priority to http_headers
                    let headers_tbl = entry_tbl
                        .get("http_headers")
                        .and_then(|v| v.as_table())
                        .or_else(|| entry_tbl.get("headers").and_then(|v| v.as_table()));

                    if let Some(headers_tbl) = headers_tbl {
                        let mut headers_json = serde_json::Map::new();
                        for (k, v) in headers_tbl.iter() {
                            if let Some(sv) = v.as_str() {
                                headers_json.insert(k.clone(), json!(sv));
                            }
                        }
                        if !headers_json.is_empty() {
                            spec.insert("headers".into(), serde_json::Value::Object(headers_json));
                        }
                    }
                }
                _ => {
                    log::warn!("Skipping Codex MCP entry '{id}' with unknown type '{typ}'");
                    return changed;
                }
            }

            // 2. Process extension and unknown fields through generic TOML → JSON conversion.
            for (key, toml_val) in entry_tbl.iter() {
                // Skip core fields already processed.
                if core_fields.contains(&key.as_str()) {
                    continue;
                }

                // Convert a generic TOML value to JSON.
                let json_val = match toml_val {
                    toml::Value::String(s) => Some(json!(s)),
                    toml::Value::Integer(i) => Some(json!(i)),
                    toml::Value::Float(f) => Some(json!(f)),
                    toml::Value::Boolean(b) => Some(json!(b)),
                    toml::Value::Array(arr) => {
                        // Support arrays of simple values only.
                        let json_arr: Vec<serde_json::Value> = arr
                            .iter()
                            .filter_map(|item| match item {
                                toml::Value::String(s) => Some(json!(s)),
                                toml::Value::Integer(i) => Some(json!(i)),
                                toml::Value::Float(f) => Some(json!(f)),
                                toml::Value::Boolean(b) => Some(json!(b)),
                                _ => None,
                            })
                            .collect();
                        if !json_arr.is_empty() {
                            Some(serde_json::Value::Array(json_arr))
                        } else {
                            log::debug!("Skipping complex array field '{key}' during TOML → JSON conversion");
                            None
                        }
                    }
                    toml::Value::Table(tbl) => {
                        // Convert a shallow table to a JSON object, accepting string values only.
                        let mut json_obj = serde_json::Map::new();
                        for (k, v) in tbl.iter() {
                            if let Some(s) = v.as_str() {
                                json_obj.insert(k.clone(), json!(s));
                            }
                        }
                        if !json_obj.is_empty() {
                            Some(serde_json::Value::Object(json_obj))
                        } else {
                            log::debug!("Skipping complex object field '{key}' during TOML → JSON conversion");
                            None
                        }
                    }
                    toml::Value::Datetime(_) => {
                        log::debug!(
                            "Skipping datetime field '{key}' during TOML → JSON conversion"
                        );
                        None
                    }
                };

                if let Some(val) = json_val {
                    spec.insert(key.clone(), val);
                    log::debug!("Imported extension field '{key}' = {toml_val:?}");
                }
            }

            let spec_v = serde_json::Value::Object(spec);

            // Validate entries independently and continue after one fails.
            if let Err(e) = validate_server_spec(&spec_v) {
                log::warn!("Skipping invalid Codex MCP entry '{id}': {e}");
                continue;
            }

            if let Some(existing) = servers.get_mut(id) {
                // Existing server: only enable Codex.
                if !existing.apps.codex {
                    existing.apps.codex = true;
                    changed += 1;
                    log::info!("Enabled Codex for MCP server '{id}'");
                }
            } else {
                // New server: enable only Codex by default.
                servers.insert(
                    id.clone(),
                    McpServer {
                        id: id.clone(),
                        name: id.clone(),
                        server: spec_v,
                        apps: McpApps {
                            claude: false,
                            codex: true,
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
        changed
    };

    // 1) Process mcp.servers.
    if let Some(mcp_val) = root.get("mcp") {
        if let Some(mcp_tbl) = mcp_val.as_table() {
            if let Some(servers_val) = mcp_tbl.get("servers") {
                if let Some(servers_tbl) = servers_val.as_table() {
                    changed_total += import_servers_tbl(servers_tbl);
                }
            }
        }
    }

    // 2) Process mcp_servers.
    if let Some(servers_val) = root.get("mcp_servers") {
        if let Some(servers_tbl) = servers_val.as_table() {
            changed_total += import_servers_tbl(servers_tbl);
        }
    }

    Ok(changed_total)
}

/// Writes Codex entries with enabled==true from config.json to ~/.codex/config.toml as TOML.
///
/// Format policy:
/// - The only correct format is the top-level [mcp_servers] table from the official Codex standard.
/// - Remove the incorrect [mcp.servers] format automatically when present.
/// - Read the existing config.toml and return an error for invalid syntax instead of overwriting it.
/// - Update only `mcp_servers` and preserve every other key.
/// - Write only enabled entries, removing mcp_servers when none are enabled.
pub fn sync_enabled_to_codex(config: &MultiAppConfig) -> Result<(), AppError> {
    if !should_sync_codex_mcp() {
        return Ok(());
    }
    use toml_edit::{Item, Table};

    // 1) Collect entries enabled for Codex.
    let enabled = collect_enabled_servers(&config.mcp.codex);

    // 2) Read existing config.toml text; preserve the invalid-TOML error instead of overwriting it.
    let base_text = crate::codex_config::read_and_validate_codex_config_text()?;

    // 3) Parse with toml_edit, accepting an empty file.
    let mut doc = if base_text.trim().is_empty() {
        toml_edit::DocumentMut::default()
    } else {
        base_text
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| AppError::McpValidation(format!("Failed to parse config.toml: {e}")))?
    };

    // 4) Remove the incorrect [mcp.servers] format when present.
    if let Some(mcp_item) = doc.get_mut("mcp") {
        if let Some(tbl) = mcp_item.as_table_like_mut() {
            if tbl.contains_key("servers") {
                log::warn!("Detected invalid MCP format [mcp.servers]; removing it and migrating to [mcp_servers]");
                tbl.remove("servers");
            }
        }
    }

    // 5) Build the target servers table with stable key ordering.
    if enabled.is_empty() {
        // Remove mcp_servers when no entries are enabled.
        doc.as_table_mut().remove("mcp_servers");
    } else {
        // Build the servers table.
        let mut servers_tbl = Table::new();
        let mut ids: Vec<_> = enabled.keys().cloned().collect();
        ids.sort();
        for id in ids {
            let spec = enabled.get(&id).expect("spec must exist");
            // Reuse the generic conversion function, including extension-field support.
            match json_server_to_toml_table(spec) {
                Ok(table) => {
                    servers_tbl[&id[..]] = Item::Table(table);
                }
                Err(err) => {
                    log::error!("Skipping invalid MCP server '{id}': {err}");
                }
            }
        }
        // Use the only correct format: [mcp_servers].
        doc["mcp_servers"] = Item::Table(servers_tbl);
    }

    // 6) Write only TOML without touching auth.json. toml_edit preserves comments, whitespace,
    // and ordering in unchanged regions where possible.
    let new_text = doc.to_string();
    let path = crate::codex_config::get_codex_config_path();
    crate::config::write_text_file(&path, &new_text)?;
    Ok(())
}

/// Synchronizes one MCP server to the live Codex configuration.
/// Always uses the official [mcp_servers] format and removes any incorrect [mcp.servers] data.
pub fn sync_single_server_to_codex(
    _config: &MultiAppConfig,
    id: &str,
    server_spec: &Value,
) -> Result<(), AppError> {
    if !should_sync_codex_mcp() {
        return Ok(());
    }
    use toml_edit::Item;

    // Read the existing config.toml.
    let config_path = crate::codex_config::get_codex_config_path();

    let mut doc = if config_path.exists() {
        let content =
            std::fs::read_to_string(&config_path).map_err(|e| AppError::io(&config_path, e))?;
        // Parse existing configuration, creating a new document on failure for tolerance.
        match content.parse::<toml_edit::DocumentMut>() {
            Ok(doc) => doc,
            Err(e) => {
                log::warn!("Failed to parse Codex config.toml: {e}; creating new configuration");
                toml_edit::DocumentMut::new()
            }
        }
    } else {
        toml_edit::DocumentMut::new()
    };

    // Remove the incorrect [mcp.servers] format when present.
    if let Some(mcp_item) = doc.get_mut("mcp") {
        if let Some(tbl) = mcp_item.as_table_like_mut() {
            if tbl.contains_key("servers") {
                log::warn!("Detected invalid MCP format [mcp.servers]; removing it and migrating to [mcp_servers]");
                tbl.remove("servers");
            }
        }
    }

    // Ensure the [mcp_servers] table exists.
    if !doc.contains_key("mcp_servers") {
        doc["mcp_servers"] = toml_edit::table();
    }

    // Convert the JSON server specification to a TOML table.
    let toml_table = json_server_to_toml_table(server_spec)?;

    // Use the only correct format: [mcp_servers].
    doc["mcp_servers"][id] = Item::Table(toml_table);

    // Write the file back.
    let new_text = doc.to_string();
    crate::config::write_text_file(&config_path, &new_text)?;

    Ok(())
}

/// Removes one MCP server from the live Codex configuration.
/// Deletes it from the correct [mcp_servers] table and cleans up data under incorrect [mcp.servers].
pub fn remove_server_from_codex(id: &str) -> Result<(), AppError> {
    if !should_sync_codex_mcp() {
        return Ok(());
    }
    let config_path = crate::codex_config::get_codex_config_path();

    if !config_path.exists() {
        return Ok(()); // No file means there is nothing to remove.
    }

    let content =
        std::fs::read_to_string(&config_path).map_err(|e| AppError::io(&config_path, e))?;

    // Parse existing configuration; return on failure because no content can be safely removed.
    let mut doc = match content.parse::<toml_edit::DocumentMut>() {
        Ok(doc) => doc,
        Err(e) => {
            log::warn!("Failed to parse Codex config.toml: {e}; skipping removal");
            return Ok(());
        }
    };

    // Remove from the correct location: [mcp_servers].
    if let Some(mcp_servers) = doc.get_mut("mcp_servers").and_then(|s| s.as_table_mut()) {
        mcp_servers.remove(id);
    }

    // Also remove data from the incorrect [mcp.servers] location when present.
    if let Some(mcp_table) = doc.get_mut("mcp").and_then(|t| t.as_table_mut()) {
        if let Some(servers) = mcp_table.get_mut("servers").and_then(|s| s.as_table_mut()) {
            if servers.remove(id).is_some() {
                log::warn!("Removed server '{id}' from invalid MCP format [mcp.servers]");
            }
        }
    }

    // Write the file back.
    let new_text = doc.to_string();
    crate::config::write_text_file(&config_path, &new_text)?;

    Ok(())
}

// ============================================================================
// TOML conversion helpers.
// ============================================================================

/// Generic JSON-to-TOML value converter supporting simple values and shallow nesting.
///
/// Supported conversions:
/// - String → TOML String
/// - Number (i64) → TOML Integer
/// - Number (f64) → TOML Float
/// - Boolean → TOML Boolean
/// - Array[simple values] → TOML array.
/// - Object → TOML inline table when every value is a string.
///
/// Unsupported types return None:
/// - null
/// - Deeply nested objects.
/// - Mixed-type arrays.
fn json_value_to_toml_item(value: &Value, field_name: &str) -> Option<toml_edit::Item> {
    use toml_edit::{Array, InlineTable, Item};

    match value {
        Value::String(s) => Some(toml_edit::value(s.as_str())),

        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(toml_edit::value(i))
            } else if let Some(f) = n.as_f64() {
                Some(toml_edit::value(f))
            } else {
                log::warn!("Skipping field '{field_name}': numeric value {n} cannot be converted");
                None
            }
        }

        Value::Bool(b) => Some(toml_edit::value(*b)),

        Value::Array(arr) => {
            // Support arrays of simple string, number, or boolean values only.
            let mut toml_arr = Array::default();
            let mut all_same_type = true;

            for item in arr {
                match item {
                    Value::String(s) => toml_arr.push(s.as_str()),
                    Value::Number(n) if n.is_i64() => {
                        if let Some(i) = n.as_i64() {
                            toml_arr.push(i);
                        } else {
                            all_same_type = false;
                            break;
                        }
                    }
                    Value::Number(n) if n.is_f64() => {
                        if let Some(f) = n.as_f64() {
                            toml_arr.push(f);
                        } else {
                            all_same_type = false;
                            break;
                        }
                    }
                    Value::Bool(b) => toml_arr.push(*b),
                    _ => {
                        all_same_type = false;
                        break;
                    }
                }
            }

            if all_same_type && !toml_arr.is_empty() {
                Some(Item::Value(toml_edit::Value::Array(toml_arr)))
            } else {
                log::warn!("Skipping field '{field_name}': unsupported mixed-type or nested array");
                None
            }
        }

        Value::Object(obj) => {
            // Support only shallow objects with string values as TOML inline tables.
            let mut inline_table = InlineTable::new();
            let mut all_strings = true;

            for (k, v) in obj {
                if let Some(s) = v.as_str() {
                    // InlineTable requires Value, while toml_edit::value() returns Item, so extract
                    // the inner Value.
                    inline_table.insert(k, s.into());
                } else {
                    all_strings = false;
                    break;
                }
            }

            if all_strings && !inline_table.is_empty() {
                Some(Item::Value(toml_edit::Value::InlineTable(inline_table)))
            } else {
                log::warn!("Skipping field '{field_name}': object contains a non-string value; use subtable syntax");
                None
            }
        }

        Value::Null => {
            log::debug!("Skipping field '{field_name}': TOML does not support null values");
            None
        }
    }
}

/// Converts a JSON MCP server specification to toml_edit::Table.
///
/// Strategy:
/// 1. Process core fields (type, command, args, url, headers, env, cwd) with strong types.
/// 2. Convert allowlisted extension fields such as timeout and retry automatically.
/// 3. Try the generic converter for every other unknown field.
fn json_server_to_toml_table(spec: &Value) -> Result<toml_edit::Table, AppError> {
    use toml_edit::{Array, Item, Table};

    let mut t = Table::new();
    let typ = spec.get("type").and_then(|v| v.as_str()).unwrap_or("stdio");
    t["type"] = toml_edit::value(typ);

    // Define core fields handled below and excluded from generic conversion.
    let core_fields = match typ {
        "stdio" => vec!["type", "command", "args", "env", "cwd"],
        "http" | "sse" => vec!["type", "url", "http_headers"],
        _ => vec!["type"],
    };

    // Define the allowlist of common optional Codex extension fields.
    let extended_fields = [
        // Generic fields.
        "timeout",
        "timeout_ms",
        "startup_timeout_ms",
        "startup_timeout_sec",
        "connection_timeout",
        "read_timeout",
        "debug",
        "log_level",
        "disabled",
        // stdio-specific fields.
        "shell",
        "encoding",
        "working_dir",
        "restart_on_exit",
        "max_restart_count",
        // HTTP/SSE-specific fields.
        "retry_count",
        "max_retry_attempts",
        "retry_delay",
        "cache_tools_list",
        "verify_ssl",
        "insecure",
        "proxy",
    ];

    // 1. Process strongly typed core fields.
    match typ {
        "stdio" => {
            let cmd = spec.get("command").and_then(|v| v.as_str()).unwrap_or("");
            t["command"] = toml_edit::value(cmd);

            if let Some(args) = spec.get("args").and_then(|v| v.as_array()) {
                let mut arr_v = Array::default();
                for a in args.iter().filter_map(|x| x.as_str()) {
                    arr_v.push(a);
                }
                if !arr_v.is_empty() {
                    t["args"] = Item::Value(toml_edit::Value::Array(arr_v));
                }
            }

            if let Some(cwd) = spec.get("cwd").and_then(|v| v.as_str()) {
                if !cwd.trim().is_empty() {
                    t["cwd"] = toml_edit::value(cwd);
                }
            }

            if let Some(env) = spec.get("env").and_then(|v| v.as_object()) {
                let mut env_tbl = Table::new();
                for (k, v) in env.iter() {
                    if let Some(s) = v.as_str() {
                        env_tbl[&k[..]] = toml_edit::value(s);
                    }
                }
                if !env_tbl.is_empty() {
                    t["env"] = Item::Table(env_tbl);
                }
            }
        }
        "http" | "sse" => {
            let url = spec.get("url").and_then(|v| v.as_str()).unwrap_or("");
            t["url"] = toml_edit::value(url);

            if let Some(headers) = spec.get("headers").and_then(|v| v.as_object()) {
                let mut h_tbl = Table::new();
                for (k, v) in headers.iter() {
                    if let Some(s) = v.as_str() {
                        h_tbl[&k[..]] = toml_edit::value(s);
                    }
                }
                if !h_tbl.is_empty() {
                    t["http_headers"] = Item::Table(h_tbl);
                }
            }
        }
        _ => {}
    }

    // 2. Process extension fields and other unknown fields.
    if let Some(obj) = spec.as_object() {
        for (key, value) in obj {
            // Skip core fields already processed.
            if core_fields.contains(&key.as_str()) {
                continue;
            }

            // Try the generic converter.
            if let Some(toml_item) = json_value_to_toml_item(value, key) {
                t[&key[..]] = toml_item;

                // Record extension-field conversion.
                if extended_fields.contains(&key.as_str()) {
                    log::debug!("Converted extension field '{key}' = {value:?}");
                } else {
                    log::info!("Converted custom field '{key}' = {value:?}");
                }
            }
        }
    }

    Ok(t)
}
