//! Gemini CLI session-log usage tracking.
//!
//! Extracts exact token usage from ~/.gemini/tmp/<project_hash>/chats/session-*.json.
//!
//! ## Data flow
//! ```text
//! ~/.gemini/tmp/*/chats/session-*.json → full parse → cost calculation → proxy_request_logs table
//! ```
//!
//! ## Differences from Claude and Codex parsers
//! - JSON rather than JSONL: each file is one object containing a messages array.
//! - No delta calculation: tokens are independent per-message values.
//! - No state recovery: values do not depend on an accumulated previous message.
//! - Natural deduplication: every message has a unique id.

use crate::database::{lock_conn, Database};
use crate::error::AppError;
use crate::gemini_config::get_gemini_dir;
use crate::proxy::usage::calculator::{CostCalculator, ModelPricing};
use crate::proxy::usage::parser::TokenUsage;
use crate::services::session_usage::{
    get_sync_state, metadata_modified_nanos, retag_legacy_remote_session_log, session_sync_key,
    update_sync_state, SessionSyncResult, SessionUsageImportOptions,
};
use crate::services::usage_stats::{
    find_model_pricing, should_skip_session_insert_with_proxy_dedup, DedupKey,
};
use rust_decimal::Decimal;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Token data extracted from a Gemini message.
#[derive(Debug)]
struct GeminiTokens {
    input: u32,
    output: u32,
    cached: u32,
    thoughts: u32,
}

/// Synchronizes Gemini usage data from JSON session logs.
pub fn sync_gemini_usage(db: &Database) -> Result<SessionSyncResult, AppError> {
    let gemini_dir = get_gemini_dir();
    sync_gemini_usage_from_dir(db, &gemini_dir)
}

/// Syncs session logs from a specific Gemini config directory.
pub fn sync_gemini_usage_from_dir(
    db: &Database,
    gemini_dir: &Path,
) -> Result<SessionSyncResult, AppError> {
    let options =
        SessionUsageImportOptions::local("gemini_session", "_gemini_session", "gemini_session");
    sync_gemini_usage_from_dir_with_options(db, gemini_dir, &options)
}

pub fn sync_gemini_usage_from_dir_with_options(
    db: &Database,
    gemini_dir: &Path,
    options: &SessionUsageImportOptions,
) -> Result<SessionSyncResult, AppError> {
    let files = collect_gemini_session_files(gemini_dir);

    let mut result = SessionSyncResult {
        imported: 0,
        skipped: 0,
        files_scanned: files.len() as u32,
        errors: vec![],
    };

    if files.is_empty() {
        return Ok(result);
    }

    for file_path in &files {
        match sync_single_gemini_file(db, file_path, gemini_dir, options) {
            Ok((imported, skipped)) => {
                result.imported += imported;
                result.skipped += skipped;
            }
            Err(e) => {
                let msg = format!(
                    "Failed to parse Gemini session file {}: {e}",
                    file_path.display()
                );
                log::warn!("[GEMINI-SYNC] {msg}");
                result.errors.push(msg);
            }
        }
    }

    if result.imported > 0 {
        log::info!(
            "[GEMINI-SYNC] Complete: imported {}, skipped {}, scanned {} files",
            result.imported,
            result.skipped,
            result.files_scanned
        );
    }

    Ok(result)
}

/// Collects all Gemini session JSON files.
fn collect_gemini_session_files(gemini_dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();

    let tmp_dir = gemini_dir.join("tmp");
    if !tmp_dir.is_dir() {
        return files;
    }

    // Traverse tmp/<project_hash>/chats/session-*.json.
    let project_dirs = match fs::read_dir(&tmp_dir) {
        Ok(entries) => entries,
        Err(_) => return files,
    };

    for entry in project_dirs.flatten() {
        let chats_dir = entry.path().join("chats");
        if !chats_dir.is_dir() {
            continue;
        }

        let chat_files = match fs::read_dir(&chats_dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };

        for file_entry in chat_files.flatten() {
            let path = file_entry.path();
            let is_session = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("session-") && n.ends_with(".json"))
                .unwrap_or(false);
            if is_session {
                files.push(path);
            }
        }
    }

    files
}

/// Synchronize one Gemini session JSON file and return `(imported, skipped)`.
fn sync_single_gemini_file(
    db: &Database,
    file_path: &Path,
    sync_root: &Path,
    options: &SessionUsageImportOptions,
) -> Result<(u32, u32), AppError> {
    let sync_key = session_sync_key(file_path, sync_root, options);
    // Read file metadata.
    let metadata = fs::metadata(file_path)
        .map_err(|e| AppError::Config(format!("Failed to read file metadata: {e}")))?;
    let file_modified = metadata_modified_nanos(&metadata);

    // Check synchronization state.
    let (last_modified, _last_offset) = get_sync_state(db, &sync_key)?;

    // Skip an unchanged file.
    if file_modified <= last_modified {
        return Ok((0, 0));
    }

    // Read and parse the entire JSON file.
    let content = fs::read_to_string(file_path)
        .map_err(|e| AppError::Config(format!("Failed to read file: {e}")))?;
    let value: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| AppError::Config(format!("Failed to parse JSON: {e}")))?;

    // Extract the top-level sessionId.
    let session_id = value
        .get("sessionId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Iterate through the messages array.
    let messages = match value.get("messages").and_then(|v| v.as_array()) {
        Some(msgs) => msgs,
        None => return Ok((0, 0)),
    };

    let mut imported: u32 = 0;
    let mut skipped: u32 = 0;
    let mut gemini_msg_count: i64 = 0;

    for msg in messages {
        // Process only messages with type == "gemini".
        if msg.get("type").and_then(|t| t.as_str()) != Some("gemini") {
            continue;
        }

        // Extract the tokens object.
        let tokens_obj = match msg.get("tokens") {
            Some(t) if t.is_object() => t,
            _ => continue,
        };

        let tokens = parse_gemini_tokens(tokens_obj);
        if tokens.input == 0 && tokens.output == 0 && tokens.thoughts == 0 && tokens.cached == 0 {
            continue; // Skip an empty message whose token counts are all zero.
        }

        gemini_msg_count += 1;

        // Extract the message ID and model.
        let message_id = msg.get("id").and_then(|v| v.as_str()).unwrap_or("unknown");
        let model = msg
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let timestamp = msg.get("timestamp").and_then(|v| v.as_str());

        // Generate a unique request_id.
        let session_id_str = session_id.as_deref().unwrap_or("unknown");
        let request_id =
            options.request_id(&format!("gemini_session:{session_id_str}:{message_id}"));

        match insert_gemini_session_entry(
            db,
            &request_id,
            &tokens,
            model,
            session_id.as_deref(),
            timestamp,
            options,
        ) {
            Ok(true) => imported += 1,
            Ok(false) => skipped += 1,
            Err(e) => {
                log::warn!("[GEMINI-SYNC] Failed to insert request {request_id}: {e}");
                skipped += 1;
            }
        }
    }

    // Update synchronization state.
    update_sync_state(db, &sync_key, file_modified, gemini_msg_count)?;

    Ok((imported, skipped))
}

/// Extracts token data from a tokens JSON object.
fn parse_gemini_tokens(tokens: &serde_json::Value) -> GeminiTokens {
    GeminiTokens {
        input: tokens.get("input").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        output: tokens.get("output").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        cached: tokens.get("cached").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        thoughts: tokens.get("thoughts").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
    }
}

/// Inserts one Gemini session record into proxy_request_logs.
fn insert_gemini_session_entry(
    db: &Database,
    request_id: &str,
    tokens: &GeminiTokens,
    model: &str,
    session_id: Option<&str>,
    timestamp: Option<&str>,
    options: &SessionUsageImportOptions,
) -> Result<bool, AppError> {
    let conn = lock_conn!(db.conn);

    let created_at = timestamp
        .and_then(|ts| {
            chrono::DateTime::parse_from_rfc3339(ts)
                .ok()
                .map(|dt| dt.timestamp())
        })
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        });

    // Add thoughts to output because reasoning tokens are priced as output.
    let output_tokens = tokens.output + tokens.thoughts;

    let dedup_key = DedupKey {
        app_type: "gemini",
        model,
        input_tokens: tokens.input,
        output_tokens,
        cache_read_tokens: tokens.cached,
        cache_creation_tokens: 0,
        created_at,
    };
    if let Some(result) =
        retag_legacy_remote_session_log(&conn, request_id, "gemini", model, options)?
    {
        return Ok(result);
    }
    if should_skip_session_insert_with_proxy_dedup(
        &conn,
        request_id,
        &dedup_key,
        options.dedup_with_proxy,
    )? {
        return Ok(false);
    }

    // Calculate cost.
    let usage = TokenUsage {
        input_tokens: tokens.input,
        output_tokens,
        cache_read_tokens: tokens.cached,
        cache_creation_tokens: 0,
        model: Some(model.to_string()),
        message_id: None,
    };

    let pricing = find_gemini_pricing(&conn, model);
    let multiplier = Decimal::from(1);
    let (input_cost, output_cost, cache_read_cost, cache_creation_cost, total_cost) = match pricing
    {
        Some(p) => {
            let cost = CostCalculator::calculate_for_app("gemini", &usage, &p, multiplier);
            (
                cost.input_cost.to_string(),
                cost.output_cost.to_string(),
                cost.cache_read_cost.to_string(),
                cost.cache_creation_cost.to_string(),
                cost.total_cost.to_string(),
            )
        }
        None => (
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
        ),
    };

    // Use UPSERT: insert new records and update token counts and costs for existing records because
    // a full Gemini reread may contain revised values.
    conn.execute(
        "INSERT INTO proxy_request_logs (
            request_id, provider_id, app_type, model, request_model,
            input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
            input_cost_usd, output_cost_usd, cache_read_cost_usd, cache_creation_cost_usd, total_cost_usd,
            latency_ms, first_token_ms, status_code, error_message, session_id,
            provider_type, is_streaming, cost_multiplier, created_at, data_source
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)
        ON CONFLICT(request_id) DO UPDATE SET
            model = excluded.model,
            input_tokens = excluded.input_tokens,
            output_tokens = excluded.output_tokens,
            cache_read_tokens = excluded.cache_read_tokens,
            input_cost_usd = excluded.input_cost_usd,
            output_cost_usd = excluded.output_cost_usd,
            cache_read_cost_usd = excluded.cache_read_cost_usd,
            cache_creation_cost_usd = excluded.cache_creation_cost_usd,
            total_cost_usd = excluded.total_cost_usd
        WHERE input_tokens != excluded.input_tokens
           OR output_tokens != excluded.output_tokens
           OR cache_read_tokens != excluded.cache_read_tokens
           OR model != excluded.model",
        rusqlite::params![
            request_id,
            options.provider_id.as_str(),
            "gemini",            // app_type
            model,
            model,               // request_model = model
            tokens.input,
            output_tokens,
            tokens.cached,
            0i64,                // cache_creation_tokens
            input_cost,
            output_cost,
            cache_read_cost,
            cache_creation_cost,
            total_cost,
            0i64,                // latency_ms
            Option::<i64>::None, // first_token_ms
            200i64,              // status_code
            Option::<String>::None, // error_message
            session_id.map(|s| s.to_string()),
            Some(options.provider_type.clone()),
            1i64,                // is_streaming
            "1.0",               // cost_multiplier
            created_at,
            options.data_source.as_str(),
        ],
    )
    .map_err(|e| AppError::Database(format!("Failed to insert the Gemini session log: {e}")))?;

    // changes() > 0 means inserted or updated; zero means values were identical and nothing changed.
    let changed = conn.changes() > 0;
    if changed {
        crate::usage_events::notify_log_recorded();
    }
    Ok(changed)
}

/// Finds Gemini model pricing.
fn find_gemini_pricing(conn: &rusqlite::Connection, model_id: &str) -> Option<ModelPricing> {
    find_model_pricing(conn, model_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collect_gemini_session_files_nonexistent() {
        let files = collect_gemini_session_files(Path::new("/nonexistent/path"));
        assert!(files.is_empty());
    }

    #[test]
    fn test_insert_gemini_session_skips_matching_proxy_log() -> Result<(), AppError> {
        let db = Database::memory()?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute(
                "INSERT INTO proxy_request_logs (
                    request_id, provider_id, app_type, model, request_model,
                    input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
                    total_cost_usd, latency_ms, status_code, created_at, data_source
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    "gemini-proxy",
                    "google",
                    "gemini",
                    "gemini-2.5-pro",
                    "gemini-2.5-pro",
                    10,
                    7,
                    1,
                    0,
                    "0.01",
                    100,
                    200,
                    1000,
                    "proxy"
                ],
            )?;
        }

        let tokens = GeminiTokens {
            input: 10,
            output: 2,
            cached: 1,
            thoughts: 5,
        };
        let options =
            SessionUsageImportOptions::local("gemini_session", "_gemini_session", "gemini_session");
        let inserted = insert_gemini_session_entry(
            &db,
            "gemini-session-dup",
            &tokens,
            "gemini-2.5-pro",
            Some("session-1"),
            Some("1970-01-01T00:16:45Z"),
            &options,
        )?;
        assert!(!inserted);

        let conn = lock_conn!(db.conn);
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM proxy_request_logs", [], |row| {
            row.get(0)
        })?;
        assert_eq!(count, 1);

        Ok(())
    }

    #[test]
    fn test_parse_gemini_tokens() {
        let json: serde_json::Value = serde_json::json!({
            "input": 8522,
            "output": 29,
            "cached": 3138,
            "thoughts": 405,
            "tool": 0,
            "total": 8956
        });
        let tokens = parse_gemini_tokens(&json);
        assert_eq!(tokens.input, 8522);
        assert_eq!(tokens.output, 29);
        assert_eq!(tokens.cached, 3138);
        assert_eq!(tokens.thoughts, 405);
        // output + thoughts = 29 + 405 = 434 for pricing.
        assert_eq!(tokens.output + tokens.thoughts, 434);
    }

    #[test]
    fn test_parse_gemini_tokens_missing_fields() {
        // Missing fields should return zero.
        let json: serde_json::Value = serde_json::json!({
            "input": 100,
            "output": 50
        });
        let tokens = parse_gemini_tokens(&json);
        assert_eq!(tokens.input, 100);
        assert_eq!(tokens.output, 50);
        assert_eq!(tokens.cached, 0);
        assert_eq!(tokens.thoughts, 0);
    }

    #[test]
    fn test_parse_gemini_tokens_all_zero() {
        let json: serde_json::Value = serde_json::json!({
            "input": 0,
            "output": 0,
            "cached": 0,
            "thoughts": 0,
            "tool": 0,
            "total": 0
        });
        let tokens = parse_gemini_tokens(&json);
        assert_eq!(tokens.input, 0);
        assert_eq!(tokens.output, 0);
        // Synchronization skips all-zero values, including cached=0.
        assert!(
            tokens.input == 0 && tokens.output == 0 && tokens.thoughts == 0 && tokens.cached == 0
        );
    }

    #[test]
    fn test_parse_gemini_tokens_cache_only_not_skipped() {
        // Do not skip a cache-only hit with input/output/thoughts=0 but cached>0.
        let json: serde_json::Value = serde_json::json!({
            "input": 0,
            "output": 0,
            "cached": 5000,
            "thoughts": 0
        });
        let tokens = parse_gemini_tokens(&json);
        assert_eq!(tokens.cached, 5000);
        // Skip only when all four fields are zero.
        let should_skip =
            tokens.input == 0 && tokens.output == 0 && tokens.thoughts == 0 && tokens.cached == 0;
        assert!(!should_skip, "A cache-only hit must not be skipped");
    }
}
