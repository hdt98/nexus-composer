//! MCP server configuration validation.

use serde_json::Value;

use crate::error::AppError;

/// Basic validation: accepts stdio/http/sse or an omitted type, which means stdio,
/// and verifies that the required fields are present.
pub fn validate_server_spec(spec: &Value) -> Result<(), AppError> {
    if !spec.is_object() {
        return Err(AppError::McpValidation(
            "An MCP server connection definition must be a JSON object".into(),
        ));
    }
    let t_opt = spec.get("type").and_then(|x| x.as_str());
    // Accept stdio/http/sse. Treat a missing type as stdio, matching common .mcp.json files.
    let is_stdio = t_opt.map(|t| t == "stdio").unwrap_or(true);
    let is_http = t_opt.map(|t| t == "http").unwrap_or(false);
    let is_sse = t_opt.map(|t| t == "sse").unwrap_or(false);

    if !(is_stdio || is_http || is_sse) {
        return Err(AppError::McpValidation(
            "MCP server type must be 'stdio', 'http', or 'sse' (or omitted for stdio)".into(),
        ));
    }

    if is_stdio {
        let cmd = spec.get("command").and_then(|x| x.as_str()).unwrap_or("");
        if cmd.trim().is_empty() {
            return Err(AppError::McpValidation(
                "An stdio MCP server is missing the command field".into(),
            ));
        }
    }
    if is_http {
        let url = spec.get("url").and_then(|x| x.as_str()).unwrap_or("");
        if url.trim().is_empty() {
            return Err(AppError::McpValidation(
                "An HTTP MCP server is missing the url field".into(),
            ));
        }
    }
    if is_sse {
        let url = spec.get("url").and_then(|x| x.as_str()).unwrap_or("");
        if url.trim().is_empty() {
            return Err(AppError::McpValidation(
                "An SSE MCP server is missing the url field".into(),
            ));
        }
    }
    Ok(())
}

/// Extracts the server specification from an MCP entry.
pub fn extract_server_spec(entry: &Value) -> Result<Value, AppError> {
    let obj = entry.as_object().ok_or_else(|| {
        AppError::McpValidation("An MCP server entry must be a JSON object".into())
    })?;
    let server = obj.get("server").ok_or_else(|| {
        AppError::McpValidation("An MCP server entry is missing the server field".into())
    })?;

    if !server.is_object() {
        return Err(AppError::McpValidation(
            "The server field of an MCP server entry must be a JSON object".into(),
        ));
    }

    Ok(server.clone())
}
