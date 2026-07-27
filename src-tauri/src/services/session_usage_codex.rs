//! Codex session log usage tracking
//!
//! Extracts precise token usage data from JSONL session files under ~/.codex/sessions/,
//! replacing the original state_5.sqlite estimation approach.
//!
//! ## Data flow
//! ```text
//! ~/.codex/sessions/YYYY/MM/DD/*.jsonl → incremental parsing → delta calculation → cost calculation → proxy_request_logs table
//! ```
//!
//! ## Parsed event types
//! - `session_meta` → extract session_id
//! - `turn_context` → extract current model
//! - `event_msg` (type=token_count) → extract cumulative token usage, calculate delta

use crate::codex_config::get_codex_config_dir;
use crate::database::{lock_conn, Database};
use crate::error::AppError;
use crate::proxy::usage::calculator::{CostCalculator, ModelPricing};
use crate::proxy::usage::parser::TokenUsage;
use crate::services::session_usage::{
    get_sync_state, metadata_sync_marker, update_sync_state, update_sync_state_after_records,
    SessionSyncResult,
};
use crate::services::usage_stats::{find_model_pricing, should_skip_session_insert, DedupKey};
use rust_decimal::Decimal;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Cumulative token usage (tracking total_token_usage field)
#[derive(Debug, Clone, Default)]
struct CumulativeTokens {
    input: u64,
    cached_input: u64,
    output: u64,
}

/// Token delta for a single API call
#[derive(Debug)]
struct DeltaTokens {
    input: u32,
    cached_input: u32,
    output: u32,
}

impl DeltaTokens {
    fn is_zero(&self) -> bool {
        self.input == 0 && self.cached_input == 0 && self.output == 0
    }
}

/// Runtime state during single file parsing
struct FileParseState {
    session_id: Option<String>,
    current_model: String,
    prev_total: Option<CumulativeTokens>,
    event_index: u32,
}

/// Normalize Codex model name
///
/// Processing rules (in order):
/// 1. Lowercase: `GLM-4.6` → `glm-4.6`
/// 2. Strip provider prefix: `openai/gpt-5.4` → `gpt-5.4`
/// 3. Strip ISO date suffix: `gpt-5.4-2026-03-05` → `gpt-5.4`
/// 4. Strip compact date suffix: `gpt-5.4-20260305` → `gpt-5.4`
pub(crate) fn normalize_codex_model(raw: &str) -> String {
    // Step 1: lowercase
    let mut name = raw.to_lowercase();

    // Step 2: strip "provider/" prefix (e.g., openai/, azure/)
    if let Some(pos) = name.rfind('/') {
        name = name[pos + 1..].to_string();
    }

    // Step 3: strip ISO date suffix -YYYY-MM-DD (exactly 11 chars)
    if name.len() > 11 && name.is_char_boundary(name.len() - 11) {
        let suffix = &name[name.len() - 11..];
        if suffix.is_ascii()
            && suffix.as_bytes()[0] == b'-'
            && suffix[1..5].chars().all(|c| c.is_ascii_digit())
            && suffix.as_bytes()[5] == b'-'
            && suffix[6..8].chars().all(|c| c.is_ascii_digit())
            && suffix.as_bytes()[8] == b'-'
            && suffix[9..11].chars().all(|c| c.is_ascii_digit())
        {
            name.truncate(name.len() - 11);
        }
    }

    // Step 4: strip compact date suffix -YYYYMMDD (exactly 9 chars)
    if name.len() > 9 {
        let parts: Vec<&str> = name.rsplitn(2, '-').collect();
        if parts.len() == 2 {
            if let Some(suffix) = parts.first() {
                if suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_digit()) {
                    name = parts[1].to_string();
                }
            }
        }
    }

    name
}

/// Calculate delta between two cumulative values
fn compute_delta(prev: &Option<CumulativeTokens>, current: &CumulativeTokens) -> DeltaTokens {
    match prev {
        None => DeltaTokens {
            input: current.input as u32,
            cached_input: current.cached_input as u32,
            output: current.output as u32,
        },
        Some(p) => DeltaTokens {
            input: current.input.saturating_sub(p.input) as u32,
            cached_input: current.cached_input.saturating_sub(p.cached_input) as u32,
            output: current.output.saturating_sub(p.output) as u32,
        },
    }
}

/// Extract cumulative token usage from JSON Value
fn parse_cumulative_tokens(total_usage: &serde_json::Value) -> Option<CumulativeTokens> {
    if total_usage.is_null() || !total_usage.is_object() {
        return None;
    }
    Some(CumulativeTokens {
        input: total_usage
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        cached_input: total_usage
            .get("cached_input_tokens")
            .or_else(|| total_usage.get("cache_read_input_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
        output: total_usage
            .get("output_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
    })
}

/// Sync Codex usage data (from JSONL session logs)
pub fn sync_codex_usage(db: &Database) -> Result<SessionSyncResult, AppError> {
    let codex_dir = get_codex_config_dir();

    let files = collect_codex_session_files(&codex_dir);

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
        match sync_single_codex_file(db, file_path) {
            Ok((imported, skipped, errors)) => {
                result.imported += imported;
                result.skipped += skipped;
                result.errors.extend(errors);
            }
            Err(e) => {
                let msg = format!(
                    "Failed to parse Codex session file {}: {e}",
                    file_path.display()
                );
                log::warn!("[CODEX-SYNC] {msg}");
                result.errors.push(msg);
            }
        }
    }

    if result.imported > 0 {
        log::debug!(
            "[CODEX-SYNC] Completed: imported {}, skipped {}, scanned {} files",
            result.imported,
            result.skipped,
            result.files_scanned
        );
    }

    Ok(result)
}

/// Collect all Codex session JSONL files
fn collect_codex_session_files(codex_dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();

    // 1. Scan sessions/YYYY/MM/DD/*.jsonl (date-partitioned directories)
    let sessions_dir = codex_dir.join("sessions");
    if sessions_dir.is_dir() {
        collect_jsonl_recursive(&sessions_dir, &mut files, 0, 3);
    }

    // 2. Scan archived_sessions/*.jsonl (flat archive directory)
    let archived_dir = codex_dir.join("archived_sessions");
    if archived_dir.is_dir() {
        if let Ok(entries) = fs::read_dir(&archived_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    files.push(path);
                }
            }
        }
    }

    files
}

/// Recursively scan .jsonl files under directory (with max depth limit)
fn collect_jsonl_recursive(dir: &Path, files: &mut Vec<PathBuf>, depth: u32, max_depth: u32) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && depth < max_depth {
            collect_jsonl_recursive(&path, files, depth + 1, max_depth);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
}

/// Synchronize one Codex JSONL file and return (imported, skipped, errors).
fn sync_single_codex_file(
    db: &Database,
    file_path: &Path,
) -> Result<(u32, u32, Vec<String>), AppError> {
    let file_path_str = file_path.to_string_lossy().to_string();

    // Get file metadata
    let metadata = fs::metadata(file_path)
        .map_err(|e| AppError::Config(format!("Failed to read file metadata: {e}")))?;
    let file_marker = metadata_sync_marker(&metadata);

    // Check sync state
    let (last_modified, last_offset) = get_sync_state(db, &file_path_str)?;

    // Skip if file unchanged
    if file_marker == last_modified {
        return Ok((0, 0, Vec::new()));
    }

    // Open file and parse line by line
    let file = fs::File::open(file_path)
        .map_err(|e| AppError::Config(format!("Failed to open file: {e}")))?;
    let mut reader = BufReader::new(file);

    let mut state = FileParseState {
        session_id: None,
        current_model: "unknown".to_string(),
        prev_total: None,
        event_index: 0,
    };

    let mut line_offset: i64 = 0;
    let mut imported: u32 = 0;
    let mut skipped: u32 = 0;
    let mut errors = Vec::new();
    let mut retry_required = false;
    let mut retry_offset = last_offset;
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = match reader.read_line(&mut line) {
            Ok(bytes_read) => bytes_read,
            Err(error) => {
                let error = format!(
                    "{}: failed to read JSONL line {}: {error}",
                    file_path.display(),
                    line_offset + 1
                );
                log::warn!("[CODEX-SYNC] {error}");
                errors.push(error);
                retry_required = true;
                retry_offset = line_offset.max(last_offset);
                break;
            }
        };
        if bytes_read == 0 {
            break;
        }

        line_offset += 1;

        if line.trim().is_empty() {
            continue;
        }

        let unterminated_value = if line.ends_with('\n') {
            None
        } else {
            match serde_json::from_str(&line) {
                Ok(value) => Some(value),
                Err(error) if error.is_eof() => {
                    let error = format!(
                        "{}: incomplete JSONL line {line_offset}: {error}",
                        file_path.display()
                    );
                    log::warn!("[CODEX-SYNC] {error}");
                    errors.push(error);
                    retry_required = true;
                    retry_offset = (line_offset - 1).max(last_offset);
                    break;
                }
                Err(error) => {
                    if line_offset > last_offset {
                        let error = format!(
                            "{}: invalid JSONL line {line_offset}: {error}",
                            file_path.display()
                        );
                        log::warn!("[CODEX-SYNC] {error}");
                        errors.push(error);
                    }
                    continue;
                }
            }
        };

        // Quick filter: skip irrelevant lines before JSON deserialization
        let is_event_msg = line.contains("\"event_msg\"");
        let is_turn_context = line.contains("\"turn_context\"");
        let is_session_meta = line.contains("\"session_meta\"");

        if !is_event_msg && !is_turn_context && !is_session_meta {
            continue;
        }
        if is_event_msg && !line.contains("\"token_count\"") {
            continue;
        }

        let value: serde_json::Value = match unterminated_value {
            Some(value) => value,
            None => match serde_json::from_str(&line) {
                Ok(value) => value,
                Err(error) => {
                    if line_offset > last_offset {
                        let error = format!(
                            "{}: invalid JSONL line {line_offset}: {error}",
                            file_path.display()
                        );
                        log::warn!("[CODEX-SYNC] {error}");
                        errors.push(error);
                    }
                    continue;
                }
            },
        };

        let event_type = match value.get("type").and_then(|t| t.as_str()) {
            Some(t) => t,
            None => continue,
        };

        match event_type {
            "session_meta" if state.session_id.is_none() => {
                let payload = value.get("payload");
                state.session_id = payload
                    .and_then(|p| {
                        p.get("session_id")
                            .or_else(|| p.get("sessionId"))
                            .or_else(|| p.get("id"))
                    })
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
            }
            "turn_context" => {
                if let Some(payload) = value.get("payload") {
                    // model may be in payload.model or payload.info.model
                    if let Some(model) = payload
                        .get("model")
                        .or_else(|| payload.get("info").and_then(|info| info.get("model")))
                        .and_then(|v| v.as_str())
                    {
                        state.current_model = normalize_codex_model(model);
                    }
                }
            }
            "event_msg" => {
                let payload = match value.get("payload") {
                    Some(p) => p,
                    None => continue,
                };

                // Only process token_count type
                if payload.get("type").and_then(|t| t.as_str()) != Some("token_count") {
                    continue;
                }

                let info = match payload.get("info") {
                    Some(i) if !i.is_null() => i,
                    _ => continue, // Skip the first event where info is null
                };

                // Extract model (token_count events may also carry model)
                if let Some(model) = info
                    .get("model")
                    .or_else(|| info.get("model_name"))
                    .or_else(|| payload.get("model"))
                    .and_then(|v| v.as_str())
                {
                    state.current_model = normalize_codex_model(model);
                }

                // Prefer total_token_usage (cumulative), fallback to last_token_usage (incremental)
                let (cumulative, is_total) = if let Some(total) = info.get("total_token_usage") {
                    (parse_cumulative_tokens(total), true)
                } else if let Some(last) = info.get("last_token_usage") {
                    (parse_cumulative_tokens(last), false)
                } else {
                    continue;
                };

                let cumulative = match cumulative {
                    Some(c) => c,
                    None => continue,
                };

                let delta = if is_total {
                    // Cumulative mode: calculate delta from last value
                    let d = compute_delta(&state.prev_total, &cumulative);
                    state.prev_total = Some(cumulative);
                    d
                } else {
                    // Incremental mode: use last_token_usage values directly
                    DeltaTokens {
                        input: cumulative.input as u32,
                        cached_input: cumulative.cached_input as u32,
                        output: cumulative.output as u32,
                    }
                };

                // Clamp: cached should not exceed input (guard against abnormal data)
                let delta = DeltaTokens {
                    cached_input: delta.cached_input.min(delta.input),
                    ..delta
                };

                if delta.is_zero() {
                    continue; // Skip zero delta events at task boundaries
                }

                state.event_index += 1;

                // Skip already processed lines (but still need to parse to restore state)
                if line_offset <= last_offset {
                    continue;
                }

                // Generate unique request_id
                let session_id_str = state.session_id.as_deref().unwrap_or("unknown");
                let request_id = format!("codex_session:{}:{}", session_id_str, state.event_index);

                // Extract timestamp
                let timestamp = value
                    .get("timestamp")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                match insert_codex_session_entry(
                    db,
                    &request_id,
                    &delta,
                    &state.current_model,
                    state.session_id.as_deref(),
                    timestamp.as_deref(),
                ) {
                    Ok(true) => imported += 1,
                    Ok(false) => skipped += 1,
                    Err(e) => {
                        let error =
                            format!("{}: insert failed ({request_id}): {e}", file_path.display());
                        log::warn!("[CODEX-SYNC] {error}");
                        errors.push(error);
                        retry_required = true;
                        retry_offset = (line_offset - 1).max(last_offset);
                        skipped += 1;
                        break;
                    }
                }
            }
            _ => {}
        }
    }

    let state_result = if retry_required {
        // Preserve the old mtime so the file is retried, while checkpointing
        // the fully processed prefix so permanent diagnostics are not repeated.
        update_sync_state(db, &file_path_str, last_modified, retry_offset)
    } else {
        update_sync_state_after_records(db, &file_path_str, file_marker, line_offset, false)
    };
    if let Err(error) = state_result {
        let error = format!(
            "{}: failed to update Codex sync state: {error}",
            file_path.display()
        );
        log::warn!("[CODEX-SYNC] {error}");
        errors.push(error);
    }

    Ok((imported, skipped, errors))
}

/// Insert a single Codex session record into proxy_request_logs
fn insert_codex_session_entry(
    db: &Database,
    request_id: &str,
    delta: &DeltaTokens,
    model: &str,
    session_id: Option<&str>,
    timestamp: Option<&str>,
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

    let dedup_key = DedupKey {
        app_type: "codex",
        model,
        input_tokens: delta.input,
        output_tokens: delta.output,
        cache_read_tokens: delta.cached_input,
        cache_creation_tokens: 0,
        created_at,
    };
    if should_skip_session_insert(&conn, request_id, &dedup_key)? {
        return Ok(false);
    }

    // Calculate costs
    let usage = TokenUsage {
        input_tokens: delta.input,
        output_tokens: delta.output,
        cache_read_tokens: delta.cached_input,
        cache_creation_tokens: 0,
        model: Some(model.to_string()),
        message_id: None,
    };

    let pricing = find_codex_pricing(&conn, model);
    let multiplier = Decimal::from(1);
    let (input_cost, output_cost, cache_read_cost, cache_creation_cost, total_cost) = match pricing
    {
        Some(p) => {
            let cost = CostCalculator::calculate_for_app("codex", &usage, &p, multiplier);
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

    let inserted_rows = conn
        .execute(
            "INSERT OR IGNORE INTO proxy_request_logs (
            request_id, provider_id, app_type, model, request_model,
            input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
            input_cost_usd, output_cost_usd, cache_read_cost_usd, cache_creation_cost_usd, total_cost_usd,
            latency_ms, first_token_ms, status_code, error_message, session_id,
            provider_type, is_streaming, cost_multiplier, created_at, data_source
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)",
            rusqlite::params![
                request_id,
                "_codex_session",    // provider_id
                "codex",             // app_type
                model,
                model,               // request_model = model
                delta.input,
                delta.output,
                delta.cached_input,
                0i64,                // cache_creation_tokens: not available in Codex logs
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
                Some("codex_session"), // provider_type
                1i64,                // is_streaming
                "1.0",               // cost_multiplier
                created_at,
                "codex_session",     // data_source
            ],
        )
        .map_err(|e| AppError::Database(format!("Failed to insert Codex session log: {e}")))?;

    if inserted_rows > 0 {
        crate::usage_events::notify_log_recorded();
    }

    Ok(true)
}

/// Find Codex model pricing (with normalization)
fn find_codex_pricing(conn: &rusqlite::Connection, model_id: &str) -> Option<ModelPricing> {
    find_model_pricing(conn, &normalize_codex_model(model_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn failed_codex_insert_retries_before_advancing_cursor() -> Result<(), AppError> {
        let db = Database::memory()?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute_batch(
                "CREATE TRIGGER fail_codex_session_record
                 BEFORE INSERT ON proxy_request_logs
                 WHEN NEW.request_id = 'codex_session:retry-session:1'
                 BEGIN
                   SELECT RAISE(FAIL, 'forced insert failure');
                 END;",
            )?;
        }

        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("retry.jsonl");
        let lines = [
            r#"{"type":"session_meta","payload":{"session_id":"retry-session"}}"#,
            r#"{"type":"turn_context","payload":{"model":"glm-5.2"}}"#,
            r#"{"type":"event_msg","timestamp":"2026-07-18T00:00:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":2,"output_tokens":1}}}}"#,
        ];
        fs::write(&file, lines.join("\n")).unwrap();

        let (_, _, errors) = sync_single_codex_file(&db, &file)?;
        assert_eq!(errors.len(), 1);
        assert_eq!(
            get_sync_state(&db, &file.to_string_lossy())?,
            (0, 2),
            "the failed event must remain retryable after its completed prefix is checkpointed"
        );

        {
            let conn = lock_conn!(db.conn);
            conn.execute_batch("DROP TRIGGER fail_codex_session_record;")?;
        }

        let (imported, _, errors) = sync_single_codex_file(&db, &file)?;
        assert_eq!(imported, 1);
        assert!(errors.is_empty());
        assert_eq!(
            get_sync_state(&db, &file.to_string_lossy())?,
            (metadata_sync_marker(&fs::metadata(&file).unwrap()), 3)
        );
        assert_eq!(sync_single_codex_file(&db, &file)?.0, 0);

        Ok(())
    }

    #[test]
    fn codex_state_write_failure_preserves_durable_import_count() -> Result<(), AppError> {
        let db = Database::memory()?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute_batch(
                "CREATE TRIGGER fail_codex_sync_state
                 BEFORE INSERT ON session_log_sync
                 BEGIN
                   SELECT RAISE(FAIL, 'forced state write failure');
                 END;",
            )?;
        }

        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("state.jsonl");
        let lines = [
            r#"{"type":"session_meta","payload":{"session_id":"state-session"}}"#,
            r#"{"type":"turn_context","payload":{"model":"glm-5.2"}}"#,
            r#"{"type":"event_msg","timestamp":"2026-07-19T00:00:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":2,"output_tokens":1}}}}"#,
        ];
        fs::write(&file, lines.join("\n")).unwrap();

        let (imported, skipped, errors) = sync_single_codex_file(&db, &file)?;
        assert_eq!((imported, skipped), (1, 0));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("failed to update Codex sync state"));
        assert_eq!(get_sync_state(&db, &file.to_string_lossy())?, (0, 0));

        {
            let conn = lock_conn!(db.conn);
            let durable: i64 = conn.query_row(
                "SELECT COUNT(*) FROM proxy_request_logs
                 WHERE request_id = 'codex_session:state-session:1'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(durable, 1);
            conn.execute_batch("DROP TRIGGER fail_codex_sync_state;")?;
        }

        let (retry_imported, retry_skipped, retry_errors) = sync_single_codex_file(&db, &file)?;
        assert_eq!((retry_imported, retry_skipped), (0, 1));
        assert!(retry_errors.is_empty());
        assert_eq!(
            get_sync_state(&db, &file.to_string_lossy())?,
            (metadata_sync_marker(&fs::metadata(&file).unwrap()), 3)
        );

        Ok(())
    }

    #[test]
    fn incomplete_codex_eof_line_is_retried_after_completion() -> Result<(), AppError> {
        let db = Database::memory()?;
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("incomplete.jsonl");
        let session_meta =
            r#"{"type":"session_meta","payload":{"session_id":"incomplete-session"}}"#;
        let turn_context = r#"{"type":"turn_context","payload":{"model":"glm-5.2"}}"#;
        let first = r#"{"type":"event_msg","timestamp":"2026-07-18T00:00:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":2,"output_tokens":1}}}}"#;
        let second = r#"{"type":"event_msg","timestamp":"2026-07-18T00:01:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":4,"output_tokens":2}}}}"#;
        let third = r#"{"type":"event_msg","timestamp":"2026-07-18T00:02:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":6,"output_tokens":3}}}}"#;
        let split_at = second.len() - 2;
        fs::write(
            &file,
            format!(
                "{session_meta}\n{turn_context}\n{first}\n{}",
                &second[..split_at]
            ),
        )
        .unwrap();

        let (imported, _, errors) = sync_single_codex_file(&db, &file)?;
        assert_eq!(imported, 1);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("incomplete JSONL line 4"));
        assert_eq!(
            get_sync_state(&db, &file.to_string_lossy())?,
            (0, 3),
            "an unterminated final record must remain retryable after its completed prefix"
        );

        let mut writer = fs::OpenOptions::new().append(true).open(&file).unwrap();
        writeln!(writer, "{}", &second[split_at..]).unwrap();
        drop(writer);

        let (retry_imported, retry_skipped, retry_errors) = sync_single_codex_file(&db, &file)?;
        assert_eq!(retry_imported, 1);
        assert_eq!(retry_skipped, 0);
        assert!(retry_errors.is_empty());
        assert_eq!(
            get_sync_state(&db, &file.to_string_lossy())?,
            (metadata_sync_marker(&fs::metadata(&file).unwrap()), 4)
        );

        let mut writer = fs::OpenOptions::new().append(true).open(&file).unwrap();
        writer.write_all(b"{not json").unwrap();
        drop(writer);
        let (_, _, malformed_errors) = sync_single_codex_file(&db, &file)?;
        assert_eq!(malformed_errors.len(), 1);
        assert!(malformed_errors[0].contains("invalid JSONL line 5"));
        assert_eq!(
            get_sync_state(&db, &file.to_string_lossy())?,
            (metadata_sync_marker(&fs::metadata(&file).unwrap()), 5),
            "a permanent syntax error must not pin the file cursor"
        );
        assert_eq!(sync_single_codex_file(&db, &file)?, (0, 0, Vec::new()));

        let checkpoint_mtime = fs::metadata(&file).unwrap().modified().unwrap();
        let checkpoint_mtime_nanos =
            crate::services::session_usage::metadata_modified_nanos(&fs::metadata(&file).unwrap());
        let mut writer = fs::OpenOptions::new().append(true).open(&file).unwrap();
        writeln!(writer, "\n{third}").unwrap();
        drop(writer);
        let file_handle = fs::OpenOptions::new().write(true).open(&file).unwrap();
        file_handle
            .set_times(fs::FileTimes::new().set_modified(checkpoint_mtime))
            .unwrap();
        assert_eq!(
            crate::services::session_usage::metadata_modified_nanos(&fs::metadata(&file).unwrap()),
            checkpoint_mtime_nanos,
            "the fixture must reproduce an append within one filesystem mtime tick"
        );
        assert_eq!(sync_single_codex_file(&db, &file)?.0, 1);

        Ok(())
    }

    #[test]
    fn permanent_codex_parse_error_is_not_reported_again_after_cursor_advances(
    ) -> Result<(), AppError> {
        let db = Database::memory()?;
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("malformed.jsonl");
        let session_meta =
            r#"{"type":"session_meta","payload":{"session_id":"malformed-session"}}"#;
        let turn_context = r#"{"type":"turn_context","payload":{"model":"glm-5.2"}}"#;
        let first = r#"{"type":"event_msg","timestamp":"2026-07-18T00:00:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":2,"output_tokens":1}}}}"#;
        let malformed = r#"{"type":"event_msg","payload":{"type":"token_count","info":"#;
        let second = r#"{"type":"event_msg","timestamp":"2026-07-18T00:01:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":4,"output_tokens":2}}}}"#;
        fs::write(
            &file,
            format!("{session_meta}\n{turn_context}\n{first}\n{malformed}\n"),
        )
        .unwrap();

        let (imported, _, errors) = sync_single_codex_file(&db, &file)?;
        assert_eq!(imported, 1);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("invalid JSONL line 4"));
        assert_eq!(
            get_sync_state(&db, &file.to_string_lossy())?,
            (metadata_sync_marker(&fs::metadata(&file).unwrap()), 4)
        );

        let mut writer = fs::OpenOptions::new().append(true).open(&file).unwrap();
        writeln!(writer, "{second}").unwrap();
        drop(writer);

        let (retry_imported, _, retry_errors) = sync_single_codex_file(&db, &file)?;
        assert_eq!(retry_imported, 1);
        assert!(
            retry_errors.is_empty(),
            "an already-cursored malformed record must not be reported again"
        );

        let conn = lock_conn!(db.conn);
        let replayed: (String, String, String, u32, u32) = conn.query_row(
            "SELECT request_id, model, session_id, input_tokens, output_tokens
             FROM proxy_request_logs
             WHERE request_id = 'codex_session:malformed-session:2'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
        assert_eq!(
            replayed,
            (
                "codex_session:malformed-session:2".to_string(),
                "glm-5.2".to_string(),
                "malformed-session".to_string(),
                2,
                1,
            ),
            "replayed prefix state must drive the appended cumulative delta"
        );

        Ok(())
    }

    #[test]
    fn permanent_codex_parse_error_is_not_repeated_while_incomplete_tail_retries(
    ) -> Result<(), AppError> {
        let db = Database::memory()?;
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("mixed-errors.jsonl");
        let session_meta =
            r#"{"type":"session_meta","payload":{"session_id":"mixed-error-session"}}"#;
        let turn_context = r#"{"type":"turn_context","payload":{"model":"glm-5.2"}}"#;
        let first = r#"{"type":"event_msg","timestamp":"2026-07-18T00:00:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":2,"output_tokens":1}}}}"#;
        let malformed = r#"{"type":"event_msg","payload":{"type":"token_count","info":"#;
        let second = r#"{"type":"event_msg","timestamp":"2026-07-18T00:01:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":4,"output_tokens":2}}}}"#;
        let split_at = second.len() - 2;
        fs::write(
            &file,
            format!(
                "{session_meta}\n{turn_context}\n{first}\n{malformed}\n{}",
                &second[..split_at]
            ),
        )
        .unwrap();

        let (imported, _, first_errors) = sync_single_codex_file(&db, &file)?;
        assert_eq!(imported, 1);
        assert!(first_errors
            .iter()
            .any(|error| error.contains("invalid JSONL line 4")));
        assert!(first_errors
            .iter()
            .any(|error| error.contains("incomplete JSONL line 5")));
        assert_eq!(
            get_sync_state(&db, &file.to_string_lossy())?.1,
            4,
            "completed prefix lines should advance while the incomplete tail remains retryable"
        );

        let (_, _, retry_errors) = sync_single_codex_file(&db, &file)?;
        assert!(
            retry_errors
                .iter()
                .all(|error| !error.contains("invalid JSONL line 4")),
            "a permanent prefix error should be reported only once"
        );
        assert!(retry_errors
            .iter()
            .any(|error| error.contains("incomplete JSONL line 5")));

        let mut writer = fs::OpenOptions::new().append(true).open(&file).unwrap();
        writeln!(writer, "{}", &second[split_at..]).unwrap();
        drop(writer);

        let (completed_imports, _, completed_errors) = sync_single_codex_file(&db, &file)?;
        assert_eq!(completed_imports, 1);
        assert!(completed_errors.is_empty());

        Ok(())
    }

    #[test]
    fn test_delta_first_event() {
        let prev = None;
        let current = CumulativeTokens {
            input: 17934,
            cached_input: 9600,
            output: 454,
        };
        let delta = compute_delta(&prev, &current);
        assert_eq!(delta.input, 17934);
        assert_eq!(delta.cached_input, 9600);
        assert_eq!(delta.output, 454);
        assert!(!delta.is_zero());
    }

    #[test]
    fn test_delta_subsequent_event() {
        let prev = Some(CumulativeTokens {
            input: 17934,
            cached_input: 9600,
            output: 454,
        });
        let current = CumulativeTokens {
            input: 36722,
            cached_input: 27904,
            output: 804,
        };
        let delta = compute_delta(&prev, &current);
        assert_eq!(delta.input, 36722 - 17934);
        assert_eq!(delta.cached_input, 27904 - 9600);
        assert_eq!(delta.output, 804 - 454);
    }

    #[test]
    fn test_delta_zero_at_task_boundary() {
        let prev = Some(CumulativeTokens {
            input: 58346,
            cached_input: 46976,
            output: 1045,
        });
        // task boundary: identical cumulative values
        let current = CumulativeTokens {
            input: 58346,
            cached_input: 46976,
            output: 1045,
        };
        let delta = compute_delta(&prev, &current);
        assert!(delta.is_zero());
    }

    #[test]
    fn test_delta_saturating_sub() {
        // Abnormal case: current value less than previous (shouldn't happen, but needs protection)
        let prev = Some(CumulativeTokens {
            input: 100,
            cached_input: 50,
            output: 30,
        });
        let current = CumulativeTokens {
            input: 80,
            cached_input: 40,
            output: 20,
        };
        let delta = compute_delta(&prev, &current);
        assert_eq!(delta.input, 0);
        assert_eq!(delta.cached_input, 0);
        assert_eq!(delta.output, 0);
        assert!(delta.is_zero());
    }

    #[test]
    fn test_parse_cumulative_tokens_valid() {
        let json: serde_json::Value = serde_json::json!({
            "input_tokens": 17934,
            "cached_input_tokens": 9600,
            "output_tokens": 454,
            "reasoning_output_tokens": 233,
            "total_tokens": 18388
        });
        let tokens = parse_cumulative_tokens(&json).unwrap();
        assert_eq!(tokens.input, 17934);
        assert_eq!(tokens.cached_input, 9600);
        assert_eq!(tokens.output, 454);
    }

    #[test]
    fn test_parse_cumulative_tokens_null() {
        let json = serde_json::Value::Null;
        assert!(parse_cumulative_tokens(&json).is_none());
    }

    #[test]
    fn test_parse_cumulative_tokens_alt_field_names() {
        // Some versions may use cache_read_input_tokens instead of cached_input_tokens
        let json: serde_json::Value = serde_json::json!({
            "input_tokens": 1000,
            "cache_read_input_tokens": 500,
            "output_tokens": 200
        });
        let tokens = parse_cumulative_tokens(&json).unwrap();
        assert_eq!(tokens.cached_input, 500);
    }

    #[test]
    fn test_collect_codex_session_files_nonexistent() {
        let files = collect_codex_session_files(Path::new("/nonexistent/path"));
        assert!(files.is_empty());
    }

    #[test]
    fn test_insert_codex_session_skips_matching_proxy_log() -> Result<(), AppError> {
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
                    "codex-proxy",
                    "openai",
                    "codex",
                    "gpt-5.4",
                    "gpt-5.4",
                    10,
                    2,
                    1,
                    7,
                    "0.01",
                    100,
                    200,
                    1000,
                    "proxy"
                ],
            )?;
        }

        let delta = DeltaTokens {
            input: 10,
            cached_input: 1,
            output: 2,
        };
        let inserted = insert_codex_session_entry(
            &db,
            "codex-session-dup",
            &delta,
            "gpt-5.4",
            Some("session-1"),
            Some("1970-01-01T00:16:45Z"),
        )?;
        assert!(!inserted);

        let conn = lock_conn!(db.conn);
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM proxy_request_logs", [], |row| {
            row.get(0)
        })?;
        assert_eq!(count, 1);

        Ok(())
    }

    // ── Model name normalization tests ──

    #[test]
    fn test_normalize_codex_model_lowercase() {
        assert_eq!(normalize_codex_model("GLM-4.6"), "glm-4.6");
        assert_eq!(normalize_codex_model("DeepSeek-Chat"), "deepseek-chat");
        assert_eq!(normalize_codex_model("GPT-5.4"), "gpt-5.4");
    }

    #[test]
    fn test_normalize_codex_model_strip_prefix() {
        assert_eq!(normalize_codex_model("openai/gpt-5.4"), "gpt-5.4");
        assert_eq!(
            normalize_codex_model("azure/gpt-5.2-codex"),
            "gpt-5.2-codex"
        );
        assert_eq!(normalize_codex_model("OPENAI/GPT-5.4"), "gpt-5.4");
    }

    #[test]
    fn test_normalize_codex_model_strip_iso_date() {
        assert_eq!(normalize_codex_model("gpt-5.4-2026-03-05"), "gpt-5.4");
        assert_eq!(
            normalize_codex_model("gpt-5.4-pro-2026-03-05"),
            "gpt-5.4-pro"
        );
    }

    #[test]
    fn test_normalize_codex_model_strip_compact_date() {
        assert_eq!(normalize_codex_model("gpt-5.4-20260305"), "gpt-5.4");
        assert_eq!(
            normalize_codex_model("claude-opus-4-6-20260206"),
            "claude-opus-4-6"
        );
    }

    #[test]
    fn test_normalize_codex_model_no_change() {
        assert_eq!(normalize_codex_model("gpt-5.4"), "gpt-5.4");
        assert_eq!(normalize_codex_model("gpt-5.2-codex"), "gpt-5.2-codex");
        assert_eq!(normalize_codex_model("o3"), "o3");
        assert_eq!(normalize_codex_model("deepseek-chat"), "deepseek-chat");
    }

    #[test]
    fn test_normalize_codex_model_combined() {
        // prefix + uppercase + ISO date
        assert_eq!(
            normalize_codex_model("openai/GPT-5.4-2026-03-05"),
            "gpt-5.4"
        );
        // prefix + compact date
        assert_eq!(normalize_codex_model("openai/gpt-5.4-20260305"), "gpt-5.4");
    }

    #[test]
    fn test_cached_clamped_to_input() {
        // Abnormal scenario where cached > input should be clamped by min()
        let prev = Some(CumulativeTokens {
            input: 100,
            cached_input: 0,
            output: 50,
        });
        let current = CumulativeTokens {
            input: 110,       // delta = 10
            cached_input: 80, // delta = 80 (abnormal: greater than input delta)
            output: 60,
        };
        let delta = compute_delta(&prev, &current);
        // Before clamping: cached_input = 80, input = 10
        assert_eq!(delta.cached_input, 80);
        assert_eq!(delta.input, 10);
        // Actual clamping is at caller side: delta.cached_input.min(delta.input)
        let clamped = delta.cached_input.min(delta.input);
        assert_eq!(clamped, 10);
    }
}
