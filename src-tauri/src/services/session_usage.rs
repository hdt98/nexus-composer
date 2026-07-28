//! Claude Code session log usage tracking
//!
//! Extracts token usage data from JSONL session files under ~/.claude/projects/,
//! implementing usage statistics without proxy mode.
//!
//! ## Data flow
//! ```text
//! ~/.claude/projects/*/*.jsonl → incremental parsing → deduplication → cost calculation → proxy_request_logs table
//! ```

use crate::config::get_claude_config_dir;
use crate::database::{billable_usage_sql, lock_conn, validate_cost_multiplier, Database};
use crate::error::AppError;
use crate::proxy::usage::calculator::{CostCalculator, ModelPricing};
use crate::proxy::usage::parser::TokenUsage;
use crate::services::usage_stats::{
    effective_usage_log_filter, find_model_pricing, is_placeholder_pricing_model,
    should_skip_correlated_session_insert, DedupKey,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

/// Sync result
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSyncResult {
    pub imported: u32,
    pub skipped: u32,
    pub files_scanned: u32,
    pub errors: Vec<String>,
}

/// Data source distribution
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataSourceSummary {
    pub data_source: String,
    pub request_count: u32,
    pub total_cost_usd: String,
}

static SESSION_USAGE_SYNC_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Options for importing session usage data from a remote host.
#[derive(Debug, Clone)]
pub struct SessionUsageImportOptions {
    /// Provider ID stored in proxy_request_logs.provider_id.
    pub provider_id: String,
    /// Provider type stored in proxy_request_logs.provider_type.
    pub provider_type: String,
    /// data_source value written to the database.
    pub data_source: String,
    /// Prefix for sync state keys in session_log_sync table.
    pub sync_key_prefix: Option<String>,
    /// Whether to deduplicate against existing proxy logs.
    pub dedup_with_proxy: bool,
}

impl SessionUsageImportOptions {
    pub fn local(
        data_source: impl Into<String>,
        provider_id: impl Into<String>,
        provider_type: impl Into<String>,
    ) -> Self {
        Self {
            provider_id: provider_id.into(),
            provider_type: provider_type.into(),
            data_source: data_source.into(),
            sync_key_prefix: None,
            dedup_with_proxy: true,
        }
    }

    /// Create options for a remote usage sync operation.
    pub fn remote(host_alias: &str, app: &str) -> Self {
        Self {
            provider_id: format!("_remote:{app}:{host_alias}"),
            provider_type: "remote_session".to_string(),
            data_source: format!("remote:{host_alias}"),
            sync_key_prefix: Some(format!("remote://{host_alias}/{app}")),
            dedup_with_proxy: true,
        }
    }

    /// Transform a raw request ID into a namespaced one for remote imports.
    pub fn request_id(&self, raw_id: &str) -> String {
        format!("{}:{raw_id}", self.data_source)
    }
}

pub(crate) fn session_sync_key(
    file_path: &Path,
    sync_root: &Path,
    options: &SessionUsageImportOptions,
) -> String {
    let Some(prefix) = options.sync_key_prefix.as_deref() else {
        return file_path.to_string_lossy().to_string();
    };

    let relative = file_path.strip_prefix(sync_root).unwrap_or(file_path);
    let relative = relative.to_string_lossy().replace('\\', "/");
    format!("{prefix}/{relative}")
}

/// Re-tag a legacy remote session log entry if it exists with a different provider/source.
/// Returns Some(true) if re-tagged successfully, Some(false) if it already exists with
/// correct tags, or None if no matching record was found (caller should proceed with insert).
pub fn retag_legacy_remote_session_log(
    conn: &rusqlite::Connection,
    request_id: &str,
    app: &str,
    model: &str,
    options: &SessionUsageImportOptions,
) -> Result<Option<bool>, AppError> {
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM proxy_request_logs WHERE request_id = ?1)",
            rusqlite::params![request_id],
            |row| row.get(0),
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

    if !exists {
        return Ok(None);
    }

    // Record exists — check if it needs re-tagging
    let updated = conn
        .execute(
            "UPDATE proxy_request_logs SET
                provider_id = ?2, provider_type = ?3, data_source = ?4, model = ?5
             WHERE request_id = ?1
               AND (provider_id != ?2 OR COALESCE(provider_type, '') != ?3 OR COALESCE(data_source, 'proxy') != ?4)",
            rusqlite::params![
                request_id,
                options.provider_id,
                options.provider_type,
                options.data_source,
                model,
            ],
        )
        .map_err(|e| AppError::Database(e.to_string()))?;

    let _ = app; // used for context in callers, not needed for the update
    Ok(Some(updated > 0))
}

fn merge_session_sync_result(
    total: &mut SessionSyncResult,
    error_prefix: &str,
    next: Result<SessionSyncResult, AppError>,
) {
    match next {
        Ok(next) => {
            total.imported += next.imported;
            total.skipped += next.skipped;
            total.files_scanned += next.files_scanned;
            total.errors.extend(next.errors);
        }
        Err(error) => total.errors.push(format!("{error_prefix}: {error}")),
    }
}

fn sync_all_session_usage_from_db(db: &Database) -> SessionSyncResult {
    let mut result = SessionSyncResult {
        imported: 0,
        skipped: 0,
        files_scanned: 0,
        errors: Vec::new(),
    };

    merge_session_sync_result(
        &mut result,
        "Claude sync failed",
        sync_claude_session_logs(db),
    );
    merge_session_sync_result(
        &mut result,
        "Codex sync failed",
        crate::services::session_usage_codex::sync_codex_usage(db),
    );
    merge_session_sync_result(
        &mut result,
        "Gemini sync failed",
        crate::services::session_usage_gemini::sync_gemini_usage(db),
    );
    merge_session_sync_result(
        &mut result,
        "OpenCode sync failed",
        crate::services::session_usage_opencode::sync_opencode_usage(db),
    );

    result
}

/// Synchronize every supported harness session source off the async runtime.
pub async fn sync_all_session_usage(db: Arc<Database>) -> Result<SessionSyncResult, AppError> {
    let guard = SESSION_USAGE_SYNC_LOCK.lock().await;
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        sync_all_session_usage_from_db(&db)
    })
    .await
    .map_err(|error| AppError::Message(format!("Session sync task failed: {error}")))
}

/// Assistant message usage data parsed from JSONL
#[derive(Debug)]
struct ParsedAssistantUsage {
    message_id: String,
    model: String,
    input_tokens: u32,
    output_tokens: u32,
    cache_read_tokens: u32,
    cache_creation_tokens: u32,
    stop_reason: Option<String>,
    timestamp: Option<String>,
    session_id: Option<String>,
}

struct SessionPricingEvidence {
    model: String,
    multiplier: Decimal,
}

enum SessionPricingEvidenceState {
    Absent,
    Canonical,
    Rejected,
    Valid(SessionPricingEvidence),
}

struct ExactProxyCandidate {
    request_id: String,
    status_code: i64,
    error_message: Option<String>,
    input_tokens: u32,
    output_tokens: u32,
    cache_read_tokens: u32,
    cache_creation_tokens: u32,
    input_cost_usd: String,
    output_cost_usd: String,
    cache_read_cost_usd: String,
    cache_creation_cost_usd: String,
    pricing_model: Option<String>,
    cost_multiplier: String,
}

impl ExactProxyCandidate {
    fn has_billable_tokens(&self) -> bool {
        self.input_tokens > 0
            || self.output_tokens > 0
            || self.cache_read_tokens > 0
            || self.cache_creation_tokens > 0
    }

    fn is_success(&self) -> bool {
        (200..300).contains(&self.status_code) && self.error_message.is_none()
    }
}

fn reconciled_component_cost(
    dimension: &str,
    persisted_tokens: u32,
    reconciled_tokens: u32,
    persisted_cost: &str,
    recalculated_cost: Option<Decimal>,
    preserve_unchanged: bool,
) -> (String, Decimal) {
    if preserve_unchanged && persisted_tokens == reconciled_tokens {
        return match persisted_cost.parse::<Decimal>() {
            Ok(cost) => (persisted_cost.to_string(), cost),
            Err(error) => {
                log::warn!(
                    "[SESSION-SYNC] Invalid persisted {dimension} cost during exact reconciliation; using zero: {error}"
                );
                ("0".to_string(), Decimal::ZERO)
            }
        };
    }

    let cost = recalculated_cost.unwrap_or(Decimal::ZERO);
    (cost.to_string(), cost)
}

/// Sync Claude Code session logs to usage statistics database
pub fn sync_claude_session_logs(db: &Database) -> Result<SessionSyncResult, AppError> {
    let projects_dir = get_claude_config_dir().join("projects");
    sync_claude_session_logs_from_projects_dir(db, &projects_dir)
}

fn sync_claude_session_logs_from_projects_dir(
    db: &Database,
    projects_dir: &Path,
) -> Result<SessionSyncResult, AppError> {
    repair_session_pricing_from_persisted_evidence(db)?;

    if !projects_dir.exists() {
        return Ok(SessionSyncResult {
            imported: 0,
            skipped: 0,
            files_scanned: 0,
            errors: vec![],
        });
    }

    let mut result = SessionSyncResult {
        imported: 0,
        skipped: 0,
        files_scanned: 0,
        errors: vec![],
    };

    // Collect all .jsonl files
    let jsonl_files = collect_jsonl_files(projects_dir);

    for file_path in &jsonl_files {
        result.files_scanned += 1;

        match sync_single_file(db, file_path) {
            Ok((imported, skipped, errors)) => {
                result.imported += imported;
                result.skipped += skipped;
                result.errors.extend(errors);
            }
            Err(e) => {
                let msg = format!("{}: {e}", file_path.display());
                log::warn!("[SESSION-SYNC] Failed to parse session file: {msg}");
                result.errors.push(msg);
            }
        }
    }

    if result.imported > 0 {
        log::debug!(
            "[SESSION-SYNC] Completed: imported {}, skipped {}, scanned {} files",
            result.imported,
            result.skipped,
            result.files_scanned
        );
    }

    Ok(result)
}

/// Repair session rows from proxy outcomes that arrived after transcript import.
/// Candidates and evidence come entirely from persisted exact message IDs, so
/// unchanged or unavailable transcript files do not prevent reconciliation.
fn repair_session_pricing_from_persisted_evidence(db: &Database) -> Result<(), AppError> {
    let conn = lock_conn!(db.conn);
    let billable_evidence = billable_usage_sql(Some("evidence"));
    let sql = format!(
        "SELECT session.request_id, session.message_id
         FROM proxy_request_logs session
         WHERE session.app_type = 'claude'
           AND session.data_source = 'session_log'
           AND session.message_id IS NOT NULL
           AND EXISTS (
               SELECT 1
               FROM proxy_request_logs evidence
               WHERE COALESCE(evidence.data_source, 'proxy') = 'proxy'
                 AND evidence.app_type IN ('claude', 'claude-desktop')
                 AND evidence.message_id = session.message_id
                 AND (
                     NOT ({billable_evidence})
                     OR (evidence.input_tokens = 0 AND session.input_tokens > 0)
                     OR (evidence.output_tokens = 0 AND session.output_tokens > 0)
                     OR (evidence.cache_read_tokens = 0 AND session.cache_read_tokens > 0)
                     OR (
                         evidence.cache_creation_tokens = 0
                         AND session.cache_creation_tokens > 0
                     )
                     OR (
                         (evidence.status_code < 200
                          OR evidence.status_code >= 300
                          OR evidence.error_message IS NOT NULL)
                         AND (
                             evidence.input_tokens != session.input_tokens
                             OR evidence.output_tokens != session.output_tokens
                             OR evidence.cache_read_tokens != session.cache_read_tokens
                             OR evidence.cache_creation_tokens != session.cache_creation_tokens
                         )
                     )
                 )
           )
         ORDER BY session.request_id"
    );
    let candidates = {
        let mut stmt = conn.prepare(&sql).map_err(|e| {
            AppError::Database(format!(
                "Failed to prepare session pricing repair query: {e}"
            ))
        })?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|e| {
                AppError::Database(format!(
                    "Failed to query session pricing repair candidates: {e}"
                ))
            })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| {
            AppError::Database(format!(
                "Failed to read session pricing repair candidates: {e}"
            ))
        })?
    };

    for (request_id, message_id) in candidates {
        if message_id.trim().is_empty() {
            continue;
        }
        reprice_existing_session_from_exact_proxy_evidence(&conn, &request_id, &message_id)?;
    }

    Ok(())
}

/// Collect all .jsonl files under directory (including sub-agent files)
///
/// Scan at fixed depth without recursion to avoid infinite loops:
///   projects_dir/<project_dir>/*.jsonl                                      (main session)
///   projects_dir/<project_dir>/SESSION_ID/subagents/*.jsonl                  (Task/Agent sub-agent)
///   projects_dir/<project_dir>/SESSION_ID/subagents/workflows/wf_*/*.jsonl   (Workflow sub-agent)
///
/// The last layer is the sub-agent transcript produced by the Claude Code Workflow feature, nested one level
/// deeper than regular sub-agents at `workflows/wf_<ID>/`. Missing this layer would cause Workflow token
/// usage to be completely excluded from statistics; `journal.jsonl` doesn't contain `type=="assistant"` lines, during parsing
/// it is naturally skipped by `sync_single_file`, so no filename filtering is needed here.
fn collect_jsonl_files(projects_dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();

    let entries = match fs::read_dir(projects_dir) {
        Ok(e) => e,
        Err(_) => return files,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // .jsonl files under each project directory
        if let Ok(sub_entries) = fs::read_dir(&path) {
            for sub_entry in sub_entries.flatten() {
                let sub_path = sub_entry.path();
                if sub_path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    // Main session JSONL file
                    files.push(sub_path);
                } else if sub_path.is_dir() {
                    // Scan sub-agent directory: project/SESSION_ID/subagents/*.jsonl
                    let subagents_dir = sub_path.join("subagents");
                    if subagents_dir.is_dir() {
                        push_jsonl_children(&subagents_dir, &mut files);

                        // Additionally scan Workflow sub-agents:
                        // project/SESSION_ID/subagents/workflows/wf_<ID>/*.jsonl
                        let workflows_dir = subagents_dir.join("workflows");
                        if workflows_dir.is_dir() {
                            if let Ok(wf_entries) = fs::read_dir(&workflows_dir) {
                                for wf_entry in wf_entries.flatten() {
                                    let wf_path = wf_entry.path();
                                    if wf_path.is_dir() {
                                        push_jsonl_children(&wf_path, &mut files);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    files
}

/// Append all `.jsonl` files directly under `dir` to `files` (non-recursive).
fn push_jsonl_children(dir: &Path, files: &mut Vec<PathBuf>) {
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                files.push(path);
            }
        }
    }
}

/// Synchronize one JSONL file and return (imported, skipped, errors).
fn sync_single_file(db: &Database, file_path: &Path) -> Result<(u32, u32, Vec<String>), AppError> {
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

    // Start incremental parsing from last offset
    let file = fs::File::open(file_path)
        .map_err(|e| AppError::Config(format!("Failed to open file: {e}")))?;
    let mut reader = BufReader::new(file);

    let mut line_offset: i64 = 0;
    let mut messages: HashMap<String, (ParsedAssistantUsage, i64)> = HashMap::new();
    let mut current_session_id: Option<String> = None;
    let mut errors = Vec::new();
    let mut permanent_errors = Vec::new();
    let mut retry_offset: Option<i64> = None;
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
                log::warn!("[SESSION-SYNC] {error}");
                errors.push(error);
                retry_offset = Some(line_offset);
                break;
            }
        };
        if bytes_read == 0 {
            break;
        }

        line_offset += 1;

        // Skip already processed lines
        if line_offset <= last_offset {
            continue;
        }

        if line.trim().is_empty() {
            continue;
        }

        let value: serde_json::Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(error) => {
                let is_incomplete = !line.ends_with('\n') && error.is_eof();
                let kind = if is_incomplete {
                    "incomplete"
                } else {
                    "invalid"
                };
                let error = format!(
                    "{}: {kind} JSONL line {line_offset}: {error}",
                    file_path.display()
                );
                if is_incomplete {
                    log::warn!("[SESSION-SYNC] {error}");
                    errors.push(error);
                    retry_offset = Some(line_offset - 1);
                    break;
                }
                permanent_errors.push((line_offset, error));
                continue;
            }
        };

        // Extract session ID (from system or first message)
        if current_session_id.is_none() {
            if let Some(sid) = value.get("sessionId").and_then(|v| v.as_str()) {
                current_session_id = Some(sid.to_string());
            }
        }

        // Only process assistant type messages
        if value.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }

        let message = match value.get("message") {
            Some(m) => m,
            None => continue,
        };

        let msg_id = match message.get("id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };

        let usage = match message.get("usage") {
            Some(u) => u,
            None => continue,
        };

        let parsed = ParsedAssistantUsage {
            message_id: msg_id.clone(),
            model: message
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string(),
            input_tokens: usage
                .get("input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            output_tokens: usage
                .get("output_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            cache_read_tokens: usage
                .get("cache_read_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            cache_creation_tokens: usage
                .get("cache_creation_input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            stop_reason: message
                .get("stop_reason")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            timestamp: value
                .get("timestamp")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            session_id: current_session_id.clone(),
        };

        // Deduplicate by message.id: prefer entries with stop_reason, otherwise keep the latest
        let should_replace = match messages.get(&msg_id).map(|(message, _)| message) {
            None => true,
            Some(existing) => {
                // New entry has stop_reason but old one doesn't → replace
                if parsed.stop_reason.is_some() && existing.stop_reason.is_none() {
                    true
                }
                // Both have or both lack stop_reason → take the one with larger output_tokens
                else if parsed.stop_reason.is_some() == existing.stop_reason.is_some() {
                    parsed.output_tokens > existing.output_tokens
                } else {
                    false
                }
            }
        };

        if should_replace {
            messages.insert(msg_id, (parsed, line_offset));
        }
    }

    // Write to database
    let mut imported: u32 = 0;
    let mut skipped: u32 = 0;

    let mut messages = messages.values().collect::<Vec<_>>();
    messages.sort_by_key(|(_, line_offset)| *line_offset);

    for (msg, message_line_offset) in messages {
        // Import as long as real billable tokens were produced, no longer requiring stop_reason or output>0.
        //
        // Anthropic charges for input + cache_read + cache_creation when the request is accepted
        // (these are determined at request start), output is charged by actual generation. Workflow/sub-agent
        // parallel short-lived requests often only write the message_start snapshot (output=1, stop_reason=None)
        // without writing the final chunk, but their cache/input costs have been actually billed. Old logic used stop_reason
        // non-empty + output>0 double filter, which would discard these requests entirely, measured ~4.1% systematic underestimate,
        // with 92% concentrated in workflow/subagent. Changed here to "import if any billable dimension > 0".
        //
        // Dedup selection logic (above: prefer stop_reason / largest output by message.id) remains
        // unchanged: its selected representative row's input/cache are already accurate; request_id = session:msg_id
        // primary key + INSERT OR IGNORE ensures a message is still stored only once, relaxing the gate won't double-count.
        let has_billable_tokens = msg.input_tokens > 0
            || msg.output_tokens > 0
            || msg.cache_read_tokens > 0
            || msg.cache_creation_tokens > 0;
        if !has_billable_tokens {
            continue;
        }

        let request_id = format!(
            "{}{}",
            crate::proxy::usage::parser::SESSION_REQUEST_ID_PREFIX,
            msg.message_id
        );

        match insert_session_log_entry(db, &request_id, msg) {
            Ok(true) => imported += 1,
            Ok(false) => skipped += 1,
            Err(e) => {
                let error = format!(
                    "{}: insert failed ({}): {e}",
                    file_path.display(),
                    msg.message_id
                );
                log::warn!("[SESSION-SYNC] {error}");
                errors.push(error);
                let failed_offset = *message_line_offset - 1;
                retry_offset =
                    Some(retry_offset.map_or(failed_offset, |offset| offset.min(failed_offset)));
                skipped += 1;
            }
        }
    }

    let durable_offset = retry_offset.unwrap_or(line_offset);
    for (error_line, error) in permanent_errors {
        if error_line <= durable_offset {
            log::warn!("[SESSION-SYNC] {error}");
            errors.push(error);
        }
    }

    let state_result = if let Some(retry_offset) = retry_offset {
        update_sync_state(db, &file_path_str, last_modified, retry_offset)
    } else {
        update_sync_state_after_records(db, &file_path_str, file_marker, line_offset, false)
    };
    if let Err(error) = state_result {
        let error = format!(
            "{}: failed to update session sync state: {error}",
            file_path.display()
        );
        log::warn!("[SESSION-SYNC] {error}");
        errors.push(error);
    }

    Ok((imported, skipped, errors))
}

/// Get sync progress for an entry in the session_log_sync table.
///
/// Shared by all session_usage_* parsers.
pub(crate) fn get_sync_state(db: &Database, file_path: &str) -> Result<(i64, i64), AppError> {
    let conn = lock_conn!(db.conn);
    let result = conn.query_row(
        "SELECT last_modified, last_line_offset FROM session_log_sync WHERE file_path = ?1",
        rusqlite::params![file_path],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    );
    Ok(result.unwrap_or((0, 0)))
}

/// Return the nanosecond-resolution mtime component used by sync markers.
pub(crate) fn metadata_modified_nanos(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

/// Return a stable marker for an imported file's observable metadata state.
///
/// Modification time alone can stay unchanged when an append lands within one
/// filesystem timestamp tick. Mixing in the file length keeps those appends
/// visible without rescanning every unchanged transcript. Existing rows store
/// a plain timestamp, so they differ once and are upgraded by a safe deduped
/// rescan on the next synchronization.
pub(crate) fn metadata_sync_marker(metadata: &fs::Metadata) -> i64 {
    metadata_set_sync_marker(&[metadata])
}

/// Return one marker for an ordered set of files that form a single source.
pub(crate) fn metadata_set_sync_marker(metadata: &[&fs::Metadata]) -> i64 {
    let mut marker = 0xcbf29ce484222325_u64;
    for byte in (metadata.len() as u64).to_le_bytes() {
        marker ^= u64::from(byte);
        marker = marker.wrapping_mul(0x100000001b3);
    }
    for item in metadata {
        for byte in (metadata_modified_nanos(item) as u64)
            .to_le_bytes()
            .into_iter()
            .chain(item.len().to_le_bytes())
        {
            marker ^= u64::from(byte);
            marker = marker.wrapping_mul(0x100000001b3);
        }
    }

    // Filesystem mtimes stored by older versions are non-negative. Reserving
    // the sign bit guarantees that every legacy row is upgraded once.
    (marker | (1_u64 << 63)) as i64
}

/// Update sync progress for an entry in the session_log_sync table.
///
/// Shared by all session_usage_* parsers.
pub(crate) fn update_sync_state(
    db: &Database,
    file_path: &str,
    last_modified: i64,
    last_offset: i64,
) -> Result<(), AppError> {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let conn = lock_conn!(db.conn);
    conn.execute(
        "INSERT OR REPLACE INTO session_log_sync (file_path, last_modified, last_line_offset, last_synced_at)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![file_path, last_modified, last_offset, now],
    )
    .map_err(|e| AppError::Database(format!("Failed to update sync state: {e}")))?;
    Ok(())
}

pub(crate) fn update_sync_state_after_records(
    db: &Database,
    file_path: &str,
    last_modified: i64,
    last_offset: i64,
    retry_required: bool,
) -> Result<(), AppError> {
    if !retry_required {
        update_sync_state(db, file_path, last_modified, last_offset)?;
    }
    Ok(())
}

/// Insert a single session log entry into proxy_request_logs, returns whether insertion succeeded (true=new insert, false=already exists)
fn insert_session_log_entry(
    db: &Database,
    request_id: &str,
    msg: &ParsedAssistantUsage,
) -> Result<bool, AppError> {
    let conn = lock_conn!(db.conn);

    let created_at = msg
        .timestamp
        .as_ref()
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
        app_type: "claude",
        model: &msg.model,
        input_tokens: msg.input_tokens,
        output_tokens: msg.output_tokens,
        cache_read_tokens: msg.cache_read_tokens,
        cache_creation_tokens: msg.cache_creation_tokens,
        created_at,
    };

    let message_id = Some(msg.message_id.as_str()).filter(|value| !value.trim().is_empty());
    let transcript_usage = TokenUsage {
        input_tokens: msg.input_tokens,
        output_tokens: msg.output_tokens,
        cache_read_tokens: msg.cache_read_tokens,
        cache_creation_tokens: msg.cache_creation_tokens,
        model: Some(msg.model.clone()),
        message_id: message_id.map(str::to_owned),
    };

    if let Some(message_id) = message_id {
        if reconcile_exact_proxy_from_session(&conn, message_id, &transcript_usage)? {
            return Ok(false);
        }
    }

    if should_skip_correlated_session_insert(&conn, request_id, &msg.message_id, &dedup_key)? {
        if let Some(message_id) = message_id {
            reprice_existing_session_from_exact_proxy_evidence(&conn, request_id, message_id)?;
        }
        return Ok(false);
    }

    let pricing_evidence = match find_session_pricing_evidence(&conn, message_id)? {
        SessionPricingEvidenceState::Valid(evidence) => evidence,
        SessionPricingEvidenceState::Absent
        | SessionPricingEvidenceState::Canonical
        | SessionPricingEvidenceState::Rejected => SessionPricingEvidence {
            model: msg.model.clone(),
            multiplier: Decimal::from(1),
        },
    };

    // Calculate costs
    let usage = TokenUsage {
        input_tokens: msg.input_tokens,
        output_tokens: msg.output_tokens,
        cache_read_tokens: msg.cache_read_tokens,
        cache_creation_tokens: msg.cache_creation_tokens,
        model: Some(pricing_evidence.model.clone()),
        message_id: message_id.map(str::to_owned),
    };

    let multiplier = pricing_evidence.multiplier;
    let (input_cost, output_cost, cache_read_cost, cache_creation_cost, total_cost) =
        calculate_session_costs(&conn, &usage, &pricing_evidence);

    let inserted_rows = conn
        .execute(
            "INSERT OR IGNORE INTO proxy_request_logs (
            request_id, message_id, provider_id, app_type, model, request_model,
            input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
            input_cost_usd, output_cost_usd, cache_read_cost_usd, cache_creation_cost_usd, total_cost_usd,
            latency_ms, first_token_ms, status_code, error_message, session_id,
            provider_type, is_streaming, cost_multiplier, created_at, data_source, pricing_model
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26)",
            rusqlite::params![
                request_id,
                message_id,
                "_session",         // provider_id: marked as session source
                "claude",           // app_type
                msg.model,
                msg.model,          // request_model = model
                msg.input_tokens,
                msg.output_tokens,
                msg.cache_read_tokens,
                msg.cache_creation_tokens,
                input_cost,
                output_cost,
                cache_read_cost,
                cache_creation_cost,
                total_cost,
                0i64,               // latency_ms: session logs don't have this data
                Option::<i64>::None, // first_token_ms
                200i64,             // status_code: Requests in session logs are considered successful as long as they produce billable tokens
                Option::<String>::None, // error_message
                msg.session_id,
                Some("session_log"), // provider_type
                1i64,               // is_streaming: Claude Code typically uses streaming
                multiplier.to_string(), // cost_multiplier: correlated proxy evidence when available
                created_at,
                "session_log",      // data_source
                pricing_evidence.model,
            ],
        )
        .map_err(|e| AppError::Database(format!("Failed to insert session log: {e}")))?;

    // Only notify frontend when a new row is actually written, avoid empty refreshes when INSERT OR IGNORE skips
    if inserted_rows > 0 {
        crate::usage_events::notify_log_recorded();
    }

    Ok(true)
}

fn calculate_session_costs(
    conn: &rusqlite::Connection,
    usage: &TokenUsage,
    evidence: &SessionPricingEvidence,
) -> (String, String, String, String, String) {
    let Some(pricing) = find_model_pricing_for_session(conn, &evidence.model) else {
        return (
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
            "0".to_string(),
        );
    };
    let cost = CostCalculator::calculate(usage, &pricing, evidence.multiplier);
    (
        cost.input_cost.to_string(),
        cost.output_cost.to_string(),
        cost.cache_read_cost.to_string(),
        cost.cache_creation_cost.to_string(),
        cost.total_cost.to_string(),
    )
}

/// Reconcile a transcript with one unambiguous exact-ID proxy representative.
/// A failed partial proxy receives the complete transcript vector because its
/// transport outcome is not a complete usage snapshot. A successful proxy keeps
/// each positive persisted dimension and fills only dimensions it omitted. The
/// proxy's trace, status, route, and pricing identity remain authoritative.
/// Multiple billable representatives remain unchanged because selecting one is
/// ambiguous. Invalid pricing on a partial billable row uses the transcript
/// model at 1x; an invalid all-zero row keeps the separate session fallback.
fn reconcile_exact_proxy_from_session(
    conn: &rusqlite::Connection,
    message_id: &str,
    usage: &TokenUsage,
) -> Result<bool, AppError> {
    let sql = "SELECT l.request_id, l.status_code,
                l.input_tokens, l.output_tokens,
                l.cache_read_tokens, l.cache_creation_tokens,
                l.input_cost_usd, l.output_cost_usd,
                l.cache_read_cost_usd, l.cache_creation_cost_usd,
                l.pricing_model, l.cost_multiplier, l.error_message
         FROM proxy_request_logs l
         WHERE COALESCE(l.data_source, 'proxy') = 'proxy'
           AND l.app_type IN ('claude', 'claude-desktop')
           AND l.message_id = ?1
         ORDER BY l.request_id";
    let candidates = {
        let mut stmt = conn.prepare(sql).map_err(|e| {
            AppError::Database(format!(
                "Failed to prepare exact proxy usage repair query: {e}"
            ))
        })?;
        let rows = stmt
            .query_map([message_id], |row| {
                Ok(ExactProxyCandidate {
                    request_id: row.get(0)?,
                    status_code: row.get(1)?,
                    input_tokens: row.get(2)?,
                    output_tokens: row.get(3)?,
                    cache_read_tokens: row.get(4)?,
                    cache_creation_tokens: row.get(5)?,
                    input_cost_usd: row.get(6)?,
                    output_cost_usd: row.get(7)?,
                    cache_read_cost_usd: row.get(8)?,
                    cache_creation_cost_usd: row.get(9)?,
                    pricing_model: row.get(10)?,
                    cost_multiplier: row.get(11)?,
                    error_message: row.get(12)?,
                })
            })
            .map_err(|e| {
                AppError::Database(format!(
                    "Failed to query exact proxy usage repair candidates: {e}"
                ))
            })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| {
            AppError::Database(format!(
                "Failed to read exact proxy usage repair candidates: {e}"
            ))
        })?
    };

    let billable_candidates = candidates
        .iter()
        .filter(|candidate| candidate.has_billable_tokens())
        .collect::<Vec<_>>();
    let candidate = match billable_candidates.as_slice() {
        [candidate] => *candidate,
        [] => {
            let successful_zero_usage_candidates = candidates
                .iter()
                .filter(|candidate| candidate.is_success())
                .collect::<Vec<_>>();
            let [candidate] = successful_zero_usage_candidates.as_slice() else {
                if successful_zero_usage_candidates.len() > 1 {
                    log::warn!(
                        "[SESSION-SYNC] Multiple successful zero-usage proxy rows share a message ID; using the session fallback"
                    );
                }
                return Ok(false);
            };
            *candidate
        }
        _ => {
            log::warn!(
                "[SESSION-SYNC] Multiple billable proxy rows share a message ID; leaving duplicates unchanged"
            );
            return Ok(false);
        }
    };

    let mut reconciled_usage = if candidate.is_success() {
        TokenUsage {
            input_tokens: if candidate.input_tokens == 0 {
                usage.input_tokens
            } else {
                candidate.input_tokens
            },
            output_tokens: if candidate.output_tokens == 0 {
                usage.output_tokens
            } else {
                candidate.output_tokens
            },
            cache_read_tokens: if candidate.cache_read_tokens == 0 {
                usage.cache_read_tokens
            } else {
                candidate.cache_read_tokens
            },
            cache_creation_tokens: if candidate.cache_creation_tokens == 0 {
                usage.cache_creation_tokens
            } else {
                candidate.cache_creation_tokens
            },
            model: None,
            message_id: Some(message_id.to_string()),
        }
    } else {
        TokenUsage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_creation_tokens: usage.cache_creation_tokens,
            model: None,
            message_id: Some(message_id.to_string()),
        }
    };

    let usage_is_unchanged = candidate.input_tokens == reconciled_usage.input_tokens
        && candidate.output_tokens == reconciled_usage.output_tokens
        && candidate.cache_read_tokens == reconciled_usage.cache_read_tokens
        && candidate.cache_creation_tokens == reconciled_usage.cache_creation_tokens;
    if usage_is_unchanged {
        return Ok(true);
    }

    let persisted_evidence = candidate
        .pricing_model
        .as_deref()
        .map(str::trim)
        .filter(|model| !is_placeholder_pricing_model(model))
        .and_then(|model| {
            validate_cost_multiplier(&candidate.cost_multiplier)
                .ok()
                .map(|multiplier| SessionPricingEvidence {
                    model: model.to_string(),
                    multiplier,
                })
        });
    let (evidence, preserve_unchanged_costs) = if let Some(evidence) = persisted_evidence {
        (evidence, true)
    } else if candidate.has_billable_tokens() {
        let transcript_model = usage
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .unwrap_or("unknown");
        log::warn!(
            "[SESSION-SYNC] Partial exact proxy row has invalid pricing evidence; using transcript model at 1x"
        );
        (
            SessionPricingEvidence {
                model: transcript_model.to_string(),
                multiplier: Decimal::ONE,
            },
            false,
        )
    } else {
        return Ok(false);
    };
    reconciled_usage.model = Some(evidence.model.clone());
    let recalculated_costs = find_model_pricing_for_session(conn, &evidence.model)
        .map(|pricing| CostCalculator::calculate(&reconciled_usage, &pricing, evidence.multiplier));
    if recalculated_costs.is_none() {
        log::warn!(
            "[SESSION-SYNC] Current pricing is unavailable during exact reconciliation; preserving unchanged component costs"
        );
    }
    let (input_cost, input_cost_value) = reconciled_component_cost(
        "input",
        candidate.input_tokens,
        reconciled_usage.input_tokens,
        &candidate.input_cost_usd,
        recalculated_costs.as_ref().map(|cost| cost.input_cost),
        preserve_unchanged_costs,
    );
    let (output_cost, output_cost_value) = reconciled_component_cost(
        "output",
        candidate.output_tokens,
        reconciled_usage.output_tokens,
        &candidate.output_cost_usd,
        recalculated_costs.as_ref().map(|cost| cost.output_cost),
        preserve_unchanged_costs,
    );
    let (cache_read_cost, cache_read_cost_value) = reconciled_component_cost(
        "cache read",
        candidate.cache_read_tokens,
        reconciled_usage.cache_read_tokens,
        &candidate.cache_read_cost_usd,
        recalculated_costs.as_ref().map(|cost| cost.cache_read_cost),
        preserve_unchanged_costs,
    );
    let (cache_creation_cost, cache_creation_cost_value) = reconciled_component_cost(
        "cache creation",
        candidate.cache_creation_tokens,
        reconciled_usage.cache_creation_tokens,
        &candidate.cache_creation_cost_usd,
        recalculated_costs
            .as_ref()
            .map(|cost| cost.cache_creation_cost),
        preserve_unchanged_costs,
    );
    let total_cost = ((input_cost_value
        + output_cost_value
        + cache_read_cost_value
        + cache_creation_cost_value)
        * evidence.multiplier)
        .to_string();
    let multiplier = evidence.multiplier.to_string();
    let updated = conn
        .execute(
            "UPDATE proxy_request_logs
         SET input_tokens = ?1, output_tokens = ?2,
             cache_read_tokens = ?3, cache_creation_tokens = ?4,
             input_cost_usd = ?5, output_cost_usd = ?6,
             cache_read_cost_usd = ?7, cache_creation_cost_usd = ?8,
             total_cost_usd = ?9, pricing_model = ?10, cost_multiplier = ?11
         WHERE request_id = ?12
           AND message_id = ?13
           AND COALESCE(data_source, 'proxy') = 'proxy'
           AND app_type IN ('claude', 'claude-desktop')
           AND (input_tokens != ?1 OR output_tokens != ?2
                OR cache_read_tokens != ?3 OR cache_creation_tokens != ?4
                OR input_cost_usd != ?5 OR output_cost_usd != ?6
                OR cache_read_cost_usd != ?7 OR cache_creation_cost_usd != ?8
                OR total_cost_usd != ?9 OR pricing_model IS NOT ?10
                OR cost_multiplier != ?11)",
            rusqlite::params![
                reconciled_usage.input_tokens,
                reconciled_usage.output_tokens,
                reconciled_usage.cache_read_tokens,
                reconciled_usage.cache_creation_tokens,
                input_cost,
                output_cost,
                cache_read_cost,
                cache_creation_cost,
                total_cost,
                evidence.model,
                multiplier,
                candidate.request_id,
                message_id,
            ],
        )
        .map_err(|e| AppError::Database(format!("Failed to repair exact proxy usage: {e}")))?;
    if updated > 0 {
        crate::usage_events::notify_log_recorded();
    }
    Ok(true)
}

/// A repeated sync may observe a session row that was inserted just before its
/// asynchronous proxy outcome. Repair only that exact session row from later
/// persisted, unambiguous evidence; canonical proxy usage remains authoritative.
fn reprice_existing_session_from_exact_proxy_evidence(
    conn: &rusqlite::Connection,
    request_id: &str,
    message_id: &str,
) -> Result<(), AppError> {
    let (existing_usage, transcript_model) = match conn.query_row(
        "SELECT input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
                model, request_model
         FROM proxy_request_logs
         WHERE request_id = ?1
           AND message_id = ?2
           AND app_type = 'claude'
           AND data_source = 'session_log'",
        rusqlite::params![request_id, message_id],
        |row| {
            let model = row.get::<_, String>(4)?;
            let request_model = row.get::<_, Option<String>>(5)?;
            let transcript_model = request_model
                .filter(|value| !value.trim().is_empty())
                .unwrap_or(model);
            Ok((
                TokenUsage {
                    input_tokens: row.get(0)?,
                    output_tokens: row.get(1)?,
                    cache_read_tokens: row.get(2)?,
                    cache_creation_tokens: row.get(3)?,
                    model: Some(transcript_model.clone()),
                    message_id: Some(message_id.to_string()),
                },
                transcript_model,
            ))
        },
    ) {
        Ok(existing) => existing,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(()),
        Err(e) => {
            return Err(AppError::Database(format!(
                "Failed to query session pricing row for repair: {e}"
            )))
        }
    };

    if reconcile_exact_proxy_from_session(conn, message_id, &existing_usage)? {
        return Ok(());
    }

    let evidence = match find_session_pricing_evidence(conn, Some(message_id))? {
        SessionPricingEvidenceState::Valid(evidence) => evidence,
        SessionPricingEvidenceState::Rejected => SessionPricingEvidence {
            model: transcript_model,
            multiplier: Decimal::from(1),
        },
        SessionPricingEvidenceState::Absent | SessionPricingEvidenceState::Canonical => {
            return Ok(())
        }
    };
    let (input_cost, output_cost, cache_read_cost, cache_creation_cost, total_cost) =
        calculate_session_costs(conn, &existing_usage, &evidence);
    let updated = conn
        .execute(
            "UPDATE proxy_request_logs
             SET pricing_model = ?1, cost_multiplier = ?2,
                 input_cost_usd = ?3, output_cost_usd = ?4,
                 cache_read_cost_usd = ?5, cache_creation_cost_usd = ?6,
                 total_cost_usd = ?7
             WHERE request_id = ?8
               AND message_id = ?9
               AND app_type = 'claude'
               AND data_source = 'session_log'
               AND (pricing_model IS NOT ?1 OR cost_multiplier != ?2
                    OR input_cost_usd != ?3 OR output_cost_usd != ?4
                    OR cache_read_cost_usd != ?5 OR cache_creation_cost_usd != ?6
                    OR total_cost_usd != ?7)",
            rusqlite::params![
                evidence.model,
                evidence.multiplier.to_string(),
                input_cost,
                output_cost,
                cache_read_cost,
                cache_creation_cost,
                total_cost,
                request_id,
                message_id,
            ],
        )
        .map_err(|e| {
            AppError::Database(format!("Failed to repair session correlated pricing: {e}"))
        })?;
    if updated > 0 {
        crate::usage_events::notify_log_recorded();
    }
    Ok(())
}

/// Use only persisted exact-ID proxy evidence to price a correlated
/// session fallback. Current provider state cannot safely describe a historical
/// request, so unmatched transcripts retain their own model and a 1x multiplier.
fn find_session_pricing_evidence(
    conn: &rusqlite::Connection,
    message_id: Option<&str>,
) -> Result<SessionPricingEvidenceState, AppError> {
    let Some(message_id) = message_id.filter(|value| !value.trim().is_empty()) else {
        return Ok(SessionPricingEvidenceState::Absent);
    };

    let canonical_usage = billable_usage_sql(Some("canonical"));
    let canonical_sql = format!(
        "SELECT EXISTS (
             SELECT 1
             FROM proxy_request_logs canonical
             WHERE COALESCE(canonical.data_source, 'proxy') = 'proxy'
               AND canonical.app_type IN ('claude', 'claude-desktop')
               AND canonical.message_id = ?1
               AND {canonical_usage}
         )"
    );
    let has_canonical = conn
        .query_row(&canonical_sql, [message_id], |row| row.get::<_, bool>(0))
        .map_err(|e| {
            AppError::Database(format!(
                "Failed to query session correlated canonical evidence: {e}"
            ))
        })?;
    if has_canonical {
        return Ok(SessionPricingEvidenceState::Canonical);
    }

    let billable_evidence = billable_usage_sql(Some("l"));
    let sql = format!(
        "SELECT l.pricing_model, l.cost_multiplier
         FROM proxy_request_logs l
         WHERE COALESCE(l.data_source, 'proxy') = 'proxy'
           AND l.app_type IN ('claude', 'claude-desktop')
           AND l.message_id = ?1
           AND NOT ({billable_evidence})
         ORDER BY l.request_id"
    );
    let mut stmt = conn.prepare(&sql).map_err(|e| {
        AppError::Database(format!(
            "Failed to prepare session correlated pricing evidence query: {e}"
        ))
    })?;
    let rows = stmt
        .query_map([message_id], |row| {
            Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| {
            AppError::Database(format!(
                "Failed to query session correlated pricing evidence: {e}"
            ))
        })?;

    let mut evidence: Option<(String, String, Decimal)> = None;
    for row in rows {
        let (raw_model, raw_multiplier) = row.map_err(|e| {
            AppError::Database(format!(
                "Failed to read session correlated pricing evidence: {e}"
            ))
        })?;
        let Some(model) = raw_model.map(|value| value.trim().to_string()) else {
            log::warn!(
                "[SESSION-SYNC] Correlated pricing evidence has no model; using session model"
            );
            return Ok(SessionPricingEvidenceState::Rejected);
        };
        if is_placeholder_pricing_model(&model) {
            log::warn!(
                "[SESSION-SYNC] Correlated pricing evidence has a placeholder model; using session model"
            );
            return Ok(SessionPricingEvidenceState::Rejected);
        }
        let multiplier = match validate_cost_multiplier(&raw_multiplier) {
            Ok(value) => value,
            Err(e) => {
                log::warn!(
                    "[SESSION-SYNC] Correlated pricing evidence has an invalid multiplier; using session model: {e}"
                );
                return Ok(SessionPricingEvidenceState::Rejected);
            }
        };
        let normalized_model = model.to_ascii_lowercase();

        if let Some((_, existing_normalized, existing_multiplier)) = &evidence {
            if *existing_normalized != normalized_model || *existing_multiplier != multiplier {
                log::warn!(
                    "[SESSION-SYNC] Conflicting correlated pricing evidence for message ID; using session model"
                );
                return Ok(SessionPricingEvidenceState::Rejected);
            }
        } else {
            evidence = Some((model, normalized_model, multiplier));
        }
    }

    Ok(match evidence {
        Some((model, _, multiplier)) => {
            SessionPricingEvidenceState::Valid(SessionPricingEvidence { model, multiplier })
        }
        None => SessionPricingEvidenceState::Absent,
    })
}

/// Find model pricing from model_pricing table (supports fuzzy matching)
fn find_model_pricing_for_session(
    conn: &rusqlite::Connection,
    model_id: &str,
) -> Option<ModelPricing> {
    find_model_pricing(conn, model_id)
}

/// Query data source distribution statistics
pub fn get_data_source_breakdown(db: &Database) -> Result<Vec<DataSourceSummary>, AppError> {
    let conn = lock_conn!(db.conn);

    let effective_filter = effective_usage_log_filter("l");
    let sql = format!(
        "SELECT COALESCE(l.data_source, 'proxy') as ds, COUNT(*) as cnt,
                COALESCE(SUM(CAST(l.total_cost_usd AS REAL)), 0) as cost
         FROM proxy_request_logs l
         WHERE {effective_filter}
         GROUP BY ds
         ORDER BY cnt DESC"
    );

    let mut stmt = conn.prepare(&sql)?;

    let rows = stmt.query_map([], |row| {
        Ok(DataSourceSummary {
            data_source: row.get(0)?,
            request_count: row.get::<_, i64>(1)? as u32,
            total_cost_usd: format!("{:.6}", row.get::<_, f64>(2)?),
        })
    })?;

    let mut summaries = Vec::new();
    for row in rows {
        summaries.push(row.map_err(|e| AppError::Database(e.to_string()))?);
    }

    Ok(summaries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn session_sync_merge_preserves_success_and_source_errors() {
        let mut total = SessionSyncResult {
            imported: 1,
            skipped: 2,
            files_scanned: 3,
            errors: vec!["record error".to_string()],
        };

        merge_session_sync_result(
            &mut total,
            "Codex sync failed",
            Ok(SessionSyncResult {
                imported: 4,
                skipped: 5,
                files_scanned: 6,
                errors: vec!["second record error".to_string()],
            }),
        );
        merge_session_sync_result(
            &mut total,
            "Gemini sync failed",
            Err(AppError::Message("unavailable".to_string())),
        );

        assert_eq!(total.imported, 5);
        assert_eq!(total.skipped, 7);
        assert_eq!(total.files_scanned, 9);
        assert_eq!(
            total.errors,
            vec![
                "record error",
                "second record error",
                "Gemini sync failed: unavailable",
            ]
        );
    }

    #[test]
    fn state_write_failure_preserves_durable_import_count() -> Result<(), AppError> {
        let db = Database::memory()?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute_batch(
                "CREATE TRIGGER fail_session_sync_state
                 BEFORE INSERT ON session_log_sync
                 BEGIN
                   SELECT RAISE(FAIL, 'forced state write failure');
                 END;",
            )?;
        }

        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(
            project.join("state.jsonl"),
            r#"{"type":"assistant","message":{"id":"msg_state_fail","model":"claude-opus-4-8","usage":{"input_tokens":2,"output_tokens":1},"stop_reason":"end_turn"},"timestamp":"2026-07-19T00:00:00Z","sessionId":"session-state-fail"}"#,
        )
        .unwrap();

        let result = sync_claude_session_logs_from_projects_dir(&db, tmp.path())?;
        assert_eq!(result.files_scanned, 1);
        assert_eq!(result.errors.len(), 1);
        assert_eq!(
            result.imported, 1,
            "durable imports must survive a later state-write failure"
        );

        let conn = lock_conn!(db.conn);
        let durable: i64 = conn.query_row(
            "SELECT COUNT(*) FROM proxy_request_logs
             WHERE request_id = 'session:msg_state_fail'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(durable, 1);
        conn.execute_batch("DROP TRIGGER fail_session_sync_state;")?;
        drop(conn);

        let retry = sync_claude_session_logs_from_projects_dir(&db, tmp.path())?;
        assert_eq!((retry.imported, retry.skipped), (0, 1));
        assert!(retry.errors.is_empty());
        let file = project.join("state.jsonl");
        assert_eq!(
            get_sync_state(&db, &file.to_string_lossy())?,
            (metadata_sync_marker(&fs::metadata(file).unwrap()), 1)
        );

        Ok(())
    }

    #[test]
    fn partial_record_failure_keeps_file_retryable() -> Result<(), AppError> {
        let db = Database::memory()?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute_batch(
                "CREATE TRIGGER fail_one_session_record
                 BEFORE INSERT ON proxy_request_logs
                 WHEN NEW.request_id = 'session:msg_fail'
                 BEGIN
                   SELECT RAISE(FAIL, 'forced insert failure');
                 END;",
            )?;
        }

        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("partial.jsonl");
        let success = r#"{"type":"assistant","message":{"id":"msg_ok","model":"claude-opus-4-8","usage":{"input_tokens":2,"output_tokens":1},"stop_reason":"end_turn"},"timestamp":"2026-06-07T13:01:23Z","sessionId":"session-partial"}"#;
        let failure = r#"{"type":"assistant","message":{"id":"msg_fail","model":"claude-opus-4-8","usage":{"input_tokens":2,"output_tokens":1},"stop_reason":"end_turn"},"timestamp":"2026-06-07T13:01:24Z","sessionId":"session-partial"}"#;
        fs::write(&file, format!("{success}\n{failure}\n")).unwrap();

        let (imported, _skipped, errors) = sync_single_file(&db, &file)?;
        assert_eq!(imported, 1);
        assert_eq!(errors.len(), 1);
        assert_eq!(
            get_sync_state(&db, &file.to_string_lossy())?,
            (0, 1),
            "a partial insert failure must checkpoint only its durable prefix"
        );

        {
            let conn = lock_conn!(db.conn);
            conn.execute_batch("DROP TRIGGER fail_one_session_record;")?;
        }

        let (retry_imported, retry_skipped, retry_errors) = sync_single_file(&db, &file)?;
        assert_eq!(
            retry_imported, 1,
            "the failed record should import on retry"
        );
        assert_eq!(
            retry_skipped, 0,
            "the durable prefix should not be replayed"
        );
        assert!(retry_errors.is_empty());

        let marker = metadata_sync_marker(&fs::metadata(&file).unwrap());
        assert_eq!(
            get_sync_state(&db, &file.to_string_lossy())?,
            (marker, 2),
            "the cursor should advance after every record is durable"
        );

        let conn = lock_conn!(db.conn);
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM proxy_request_logs WHERE session_id = 'session-partial'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(count, 2);
        drop(conn);

        let (unchanged_imported, unchanged_skipped, unchanged_errors) =
            sync_single_file(&db, &file)?;
        assert_eq!((unchanged_imported, unchanged_skipped), (0, 0));
        assert!(unchanged_errors.is_empty());

        Ok(())
    }

    #[test]
    fn incomplete_claude_eof_line_is_retried_after_completion() -> Result<(), AppError> {
        let db = Database::memory()?;
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("incomplete.jsonl");
        let first = r#"{"type":"assistant","message":{"id":"msg_first","model":"claude-opus-4-8","usage":{"input_tokens":2,"output_tokens":1},"stop_reason":"end_turn"},"timestamp":"2026-06-07T13:01:23Z","sessionId":"session-incomplete"}"#;
        let second = r#"{"type":"assistant","message":{"id":"msg_completed","model":"claude-opus-4-8","usage":{"input_tokens":4,"output_tokens":2},"stop_reason":"end_turn"},"timestamp":"2026-06-07T13:01:24Z","sessionId":"session-incomplete"}"#;
        let third = r#"{"type":"assistant","message":{"id":"msg_after_invalid","model":"claude-opus-4-8","usage":{"input_tokens":6,"output_tokens":3},"stop_reason":"end_turn"},"timestamp":"2026-06-07T13:01:25Z","sessionId":"session-incomplete"}"#;
        let split_at = second.len() - 2;
        fs::write(&file, format!("{first}\n{}", &second[..split_at])).unwrap();

        let (imported, _, errors) = sync_single_file(&db, &file)?;
        assert_eq!(imported, 1);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("incomplete JSONL line 2"));
        assert_eq!(
            get_sync_state(&db, &file.to_string_lossy())?,
            (0, 1),
            "an unterminated final record must remain pending after its durable prefix"
        );

        let mut writer = fs::OpenOptions::new().append(true).open(&file).unwrap();
        writeln!(writer, "{}", &second[split_at..]).unwrap();
        drop(writer);

        let (retry_imported, retry_skipped, retry_errors) = sync_single_file(&db, &file)?;
        assert_eq!(retry_imported, 1);
        assert_eq!(retry_skipped, 0);
        assert!(retry_errors.is_empty());
        assert_eq!(
            get_sync_state(&db, &file.to_string_lossy())?,
            (metadata_sync_marker(&fs::metadata(&file).unwrap()), 2)
        );

        let mut writer = fs::OpenOptions::new().append(true).open(&file).unwrap();
        writer.write_all(b"{not json").unwrap();
        drop(writer);
        let (_, _, malformed_errors) = sync_single_file(&db, &file)?;
        assert_eq!(malformed_errors.len(), 1);
        assert!(malformed_errors[0].contains("invalid JSONL line 3"));
        assert_eq!(
            get_sync_state(&db, &file.to_string_lossy())?,
            (metadata_sync_marker(&fs::metadata(&file).unwrap()), 3),
            "a permanent syntax error must not pin the file cursor"
        );
        assert_eq!(sync_single_file(&db, &file)?, (0, 0, Vec::new()));

        let mut writer = fs::OpenOptions::new().append(true).open(&file).unwrap();
        writeln!(writer, "\n{third}").unwrap();
        drop(writer);
        assert_eq!(sync_single_file(&db, &file)?.0, 1);

        Ok(())
    }

    #[test]
    fn permanent_claude_parse_error_is_not_repeated_while_incomplete_tail_retries(
    ) -> Result<(), AppError> {
        let db = Database::memory()?;
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("mixed-errors.jsonl");
        let first = r#"{"type":"assistant","message":{"id":"msg_first","model":"claude-opus-4-8","usage":{"input_tokens":2,"output_tokens":1},"stop_reason":"end_turn"},"timestamp":"2026-06-07T13:01:23Z","sessionId":"session-mixed"}"#;
        let malformed = r#"{"type":"assistant","message":{"id":"broken""#;
        let second = r#"{"type":"assistant","message":{"id":"msg_completed","model":"claude-opus-4-8","usage":{"input_tokens":4,"output_tokens":2},"stop_reason":"end_turn"},"timestamp":"2026-06-07T13:01:24Z","sessionId":"session-mixed"}"#;
        let split_at = second.len() - 2;
        fs::write(
            &file,
            format!("{first}\n{malformed}\n{}", &second[..split_at]),
        )
        .unwrap();

        let (imported, _, first_errors) = sync_single_file(&db, &file)?;
        assert_eq!(imported, 1);
        assert!(first_errors
            .iter()
            .any(|error| error.contains("invalid JSONL line 2")));
        assert!(first_errors
            .iter()
            .any(|error| error.contains("incomplete JSONL line 3")));
        assert_eq!(
            get_sync_state(&db, &file.to_string_lossy())?,
            (0, 2),
            "completed prefix lines should advance while the incomplete tail remains retryable"
        );

        let (retry_imported, _, retry_errors) = sync_single_file(&db, &file)?;
        assert_eq!(retry_imported, 0);
        assert!(
            retry_errors
                .iter()
                .all(|error| !error.contains("invalid JSONL line 2")),
            "a permanent prefix error should be reported only once"
        );
        assert!(retry_errors
            .iter()
            .any(|error| error.contains("incomplete JSONL line 3")));

        let mut writer = fs::OpenOptions::new().append(true).open(&file).unwrap();
        writeln!(writer, "{}", &second[split_at..]).unwrap();
        drop(writer);

        let (completed_imports, _, completed_errors) = sync_single_file(&db, &file)?;
        assert_eq!(completed_imports, 1);
        assert!(completed_errors.is_empty());

        Ok(())
    }

    #[test]
    fn permanent_claude_parse_error_is_not_repeated_while_insert_retries() -> Result<(), AppError> {
        let db = Database::memory()?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute_batch(
                "CREATE TRIGGER fail_claude_session_record
                 BEFORE INSERT ON proxy_request_logs
                 WHEN NEW.request_id = 'session:msg_retry'
                 BEGIN
                   SELECT RAISE(FAIL, 'forced insert failure');
                 END;",
            )?;
        }

        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("mixed-insert-error.jsonl");
        let malformed = r#"{"type":"assistant","message":{"id":"broken""#;
        let retry = r#"{"type":"assistant","message":{"id":"msg_retry","model":"claude-opus-4-8","usage":{"input_tokens":2,"output_tokens":1},"stop_reason":"end_turn"},"timestamp":"2026-06-07T13:01:24Z","sessionId":"session-mixed-insert"}"#;
        fs::write(&file, format!("{malformed}\n{retry}\n")).unwrap();

        let (_, _, first_errors) = sync_single_file(&db, &file)?;
        assert!(first_errors
            .iter()
            .any(|error| error.contains("invalid JSONL line 1")));
        assert!(first_errors
            .iter()
            .any(|error| error.contains("insert failed (msg_retry)")));
        assert_eq!(
            get_sync_state(&db, &file.to_string_lossy())?,
            (0, 1),
            "the failed record should remain pending after its permanent-error prefix"
        );

        let (_, _, retry_errors) = sync_single_file(&db, &file)?;
        assert!(
            retry_errors
                .iter()
                .all(|error| !error.contains("invalid JSONL line 1")),
            "a permanent prefix error should not repeat during a database retry"
        );
        assert!(retry_errors
            .iter()
            .any(|error| error.contains("insert failed (msg_retry)")));

        {
            let conn = lock_conn!(db.conn);
            conn.execute_batch("DROP TRIGGER fail_claude_session_record;")?;
        }

        let (imported, _, errors) = sync_single_file(&db, &file)?;
        assert_eq!(imported, 1);
        assert!(errors.is_empty());

        Ok(())
    }

    #[test]
    fn permanent_claude_suffix_error_waits_for_earlier_insert_retry() -> Result<(), AppError> {
        let db = Database::memory()?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute_batch(
                "CREATE TRIGGER fail_claude_suffix_record
                 BEFORE INSERT ON proxy_request_logs
                 WHEN NEW.request_id = 'session:msg_before_suffix'
                 BEGIN
                   SELECT RAISE(FAIL, 'forced insert failure');
                 END;",
            )?;
        }

        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("retry-before-malformed.jsonl");
        let prefix = r#"{"type":"system","sessionId":"session-suffix"}"#;
        let retry = r#"{"type":"assistant","message":{"id":"msg_before_suffix","model":"claude-opus-4-8","usage":{"input_tokens":2,"output_tokens":1},"stop_reason":"end_turn"},"timestamp":"2026-06-07T13:01:24Z","sessionId":"session-suffix"}"#;
        let malformed = r#"{"type":"assistant","message":{"id":"broken""#;
        fs::write(&file, format!("{prefix}\n{retry}\n{malformed}\n")).unwrap();

        let (_, _, first_errors) = sync_single_file(&db, &file)?;
        assert!(first_errors
            .iter()
            .any(|error| error.contains("insert failed (msg_before_suffix)")));
        assert!(
            first_errors
                .iter()
                .all(|error| !error.contains("invalid JSONL line 3")),
            "a suffix diagnostic must wait until the earlier retryable record is durable"
        );
        assert_eq!(get_sync_state(&db, &file.to_string_lossy())?, (0, 1));

        let (_, _, retry_errors) = sync_single_file(&db, &file)?;
        assert!(retry_errors
            .iter()
            .all(|error| !error.contains("invalid JSONL line 3")));

        {
            let conn = lock_conn!(db.conn);
            conn.execute_batch("DROP TRIGGER fail_claude_suffix_record;")?;
        }

        let (imported, _, completed_errors) = sync_single_file(&db, &file)?;
        assert_eq!(imported, 1);
        assert_eq!(completed_errors.len(), 1);
        assert!(completed_errors[0].contains("invalid JSONL line 3"));
        assert_eq!(
            get_sync_state(&db, &file.to_string_lossy())?,
            (metadata_sync_marker(&fs::metadata(&file).unwrap()), 3)
        );
        assert_eq!(sync_single_file(&db, &file)?, (0, 0, Vec::new()));

        Ok(())
    }

    fn parsed_usage(message_id: &str) -> ParsedAssistantUsage {
        ParsedAssistantUsage {
            message_id: message_id.to_string(),
            model: "claude-sonnet-4-5".to_string(),
            input_tokens: 100,
            output_tokens: 20,
            cache_read_tokens: 10,
            cache_creation_tokens: 5,
            stop_reason: Some("end_turn".to_string()),
            timestamp: Some("1970-01-01T00:16:45Z".to_string()),
            session_id: Some("session-1".to_string()),
        }
    }

    fn insert_fingerprint_matching_proxy(
        db: &Database,
        app_type: &str,
        message_id: Option<&str>,
        status_code: i64,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(db.conn);
        conn.execute(
            "INSERT INTO proxy_request_logs (
                request_id, message_id, provider_id, app_type, model, request_model,
                input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
                total_cost_usd, latency_ms, status_code, created_at, data_source
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            rusqlite::params![
                format!("proxy-{app_type}-{}", message_id.unwrap_or("legacy")),
                message_id,
                "upstream-provider",
                app_type,
                "claude-sonnet-4-5",
                "claude-sonnet-4-5",
                100,
                20,
                10,
                5,
                "0.10",
                100,
                status_code,
                1000,
                "proxy"
            ],
        )?;
        Ok(())
    }

    fn insert_zero_usage_pricing_evidence(
        db: &Database,
        request_id: &str,
        app_type: &str,
        message_id: &str,
        pricing_model: Option<&str>,
        multiplier: &str,
    ) -> Result<(), AppError> {
        let conn = lock_conn!(db.conn);
        conn.execute(
            "INSERT INTO proxy_request_logs (
                request_id, message_id, provider_id, app_type, model, request_model,
                pricing_model, latency_ms, status_code, cost_multiplier, created_at, data_source
             ) VALUES (?1, ?2, 'upstream-provider', ?3, 'response-model',
                       'request-model', ?4, 100, 502, ?5, 1000, 'proxy')",
            rusqlite::params![request_id, message_id, app_type, pricing_model, multiplier],
        )?;
        Ok(())
    }

    #[test]
    fn test_parse_usage_from_jsonl_line() {
        let line = r#"{"type":"assistant","message":{"id":"msg_test123","model":"claude-opus-4-6","usage":{"input_tokens":3,"output_tokens":150,"cache_read_input_tokens":5000,"cache_creation_input_tokens":10000},"stop_reason":"end_turn"},"timestamp":"2026-04-05T12:00:00Z","sessionId":"session-abc"}"#;

        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(
            value.get("type").and_then(|t| t.as_str()),
            Some("assistant")
        );

        let message = value.get("message").unwrap();
        let usage = message.get("usage").unwrap();

        assert_eq!(usage.get("input_tokens").unwrap().as_u64().unwrap(), 3);
        assert_eq!(usage.get("output_tokens").unwrap().as_u64().unwrap(), 150);
        assert_eq!(
            usage
                .get("cache_read_input_tokens")
                .unwrap()
                .as_u64()
                .unwrap(),
            5000
        );
        assert_eq!(
            usage
                .get("cache_creation_input_tokens")
                .unwrap()
                .as_u64()
                .unwrap(),
            10000
        );
        assert_eq!(
            message.get("stop_reason").unwrap().as_str().unwrap(),
            "end_turn"
        );
    }

    #[test]
    fn test_dedup_by_message_id() {
        // Same message.id has multiple entries, should take the one with stop_reason value
        let mut messages: HashMap<String, ParsedAssistantUsage> = HashMap::new();

        // Intermediate entry (no stop_reason)
        let intermediate = ParsedAssistantUsage {
            message_id: "msg_1".to_string(),
            model: "claude-opus-4-6".to_string(),
            input_tokens: 3,
            output_tokens: 26,
            cache_read_tokens: 5000,
            cache_creation_tokens: 10000,
            stop_reason: None,
            timestamp: Some("2026-04-05T12:00:00Z".to_string()),
            session_id: None,
        };
        messages.insert("msg_1".to_string(), intermediate);

        // Final entry (with stop_reason)
        let final_entry = ParsedAssistantUsage {
            message_id: "msg_1".to_string(),
            model: "claude-opus-4-6".to_string(),
            input_tokens: 3,
            output_tokens: 1349,
            cache_read_tokens: 5000,
            cache_creation_tokens: 10000,
            stop_reason: Some("end_turn".to_string()),
            timestamp: Some("2026-04-05T12:00:00Z".to_string()),
            session_id: None,
        };

        // Should replace
        let should_replace = final_entry.stop_reason.is_some()
            && messages.get("msg_1").unwrap().stop_reason.is_none();
        assert!(should_replace);

        messages.insert("msg_1".to_string(), final_entry);
        assert_eq!(messages.get("msg_1").unwrap().output_tokens, 1349);
    }

    #[test]
    fn test_insert_claude_session_skips_proxy_with_same_message_id() -> Result<(), AppError> {
        let db = Database::memory()?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute(
                "INSERT INTO proxy_request_logs (
                    request_id, message_id, provider_id, app_type, model, request_model,
                    input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens,
                    total_cost_usd, latency_ms, status_code, created_at, data_source
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                rusqlite::params![
                    "proxy-different-id",
                    "msg_1",
                    "openai-compatible",
                    "claude",
                    "routed-upstream-model",
                    "claude-sonnet-4-5",
                    999,
                    888,
                    777,
                    666,
                    "0.10",
                    100,
                    200,
                    1,
                    "proxy"
                ],
            )?;
        }

        let msg = parsed_usage("msg_1");

        let inserted = insert_session_log_entry(&db, "session:msg_1", &msg)?;
        assert!(!inserted);

        let conn = lock_conn!(db.conn);
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM proxy_request_logs", [], |row| {
            row.get(0)
        })?;
        assert_eq!(count, 1);

        Ok(())
    }

    #[test]
    fn test_insert_claude_session_keeps_different_message_id() -> Result<(), AppError> {
        let db = Database::memory()?;
        insert_fingerprint_matching_proxy(&db, "claude", Some("msg_proxy"), 200)?;

        let msg = parsed_usage("msg_session");
        assert!(insert_session_log_entry(&db, "session:msg_session", &msg)?);

        let conn = lock_conn!(db.conn);
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM proxy_request_logs", [], |row| {
            row.get(0)
        })?;
        assert_eq!(count, 2);

        Ok(())
    }

    #[test]
    fn test_insert_claude_session_falls_back_for_legacy_proxy() -> Result<(), AppError> {
        for proxy_message_id in [None, Some("   "), Some("\t\n")] {
            let db = Database::memory()?;
            insert_fingerprint_matching_proxy(&db, "claude", proxy_message_id, 200)?;

            let msg = parsed_usage("msg_session");
            assert!(!insert_session_log_entry(&db, "session:msg_session", &msg)?);
        }

        Ok(())
    }

    #[test]
    fn test_insert_claude_session_keeps_legacy_semantic_failure() -> Result<(), AppError> {
        let db = Database::memory()?;
        insert_fingerprint_matching_proxy(&db, "claude", None, 200)?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute(
                "UPDATE proxy_request_logs
                 SET error_message = 'upstream_response_incomplete:length'
                 WHERE request_id = 'proxy-claude-legacy'",
                [],
            )?;
        }

        let msg = parsed_usage("msg_session");
        assert!(insert_session_log_entry(&db, "session:msg_session", &msg)?);

        let conn = lock_conn!(db.conn);
        let effective_filter = effective_usage_log_filter("l");
        let effective_count: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM proxy_request_logs l WHERE {effective_filter}"),
            [],
            |row| row.get(0),
        )?;
        assert_eq!(effective_count, 2);

        Ok(())
    }

    #[test]
    fn test_insert_claude_session_keeps_same_message_id_from_other_app() -> Result<(), AppError> {
        let db = Database::memory()?;
        insert_fingerprint_matching_proxy(&db, "codex", Some("msg_same"), 200)?;

        let msg = parsed_usage("msg_same");
        assert!(insert_session_log_entry(&db, "session:msg_same", &msg)?);

        Ok(())
    }

    #[test]
    fn test_insert_claude_session_skips_desktop_proxy_with_same_message_id() -> Result<(), AppError>
    {
        let db = Database::memory()?;
        insert_fingerprint_matching_proxy(&db, "claude-desktop", Some("msg_same"), 200)?;

        let msg = parsed_usage("msg_same");
        assert!(!insert_session_log_entry(&db, "session:msg_same", &msg)?);

        Ok(())
    }

    #[test]
    fn test_insert_claude_session_skips_billable_failed_proxy() -> Result<(), AppError> {
        let db = Database::memory()?;
        insert_fingerprint_matching_proxy(&db, "claude", Some("msg_same"), 502)?;

        let msg = parsed_usage("msg_same");
        assert!(!insert_session_log_entry(&db, "session:msg_same", &msg)?);

        Ok(())
    }

    #[test]
    fn test_insert_claude_session_keeps_historical_costs_for_complete_proxy() -> Result<(), AppError>
    {
        let db = Database::memory()?;
        insert_fingerprint_matching_proxy(&db, "claude", Some("msg_complete"), 200)?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute(
                "UPDATE proxy_request_logs
                 SET pricing_model = 'historical-model', cost_multiplier = '2',
                     input_cost_usd = '1', output_cost_usd = '2',
                     cache_read_cost_usd = '3', cache_creation_cost_usd = '4',
                     total_cost_usd = '10'
                 WHERE request_id = 'proxy-claude-msg_complete'",
                [],
            )?;
            conn.execute(
                "INSERT OR REPLACE INTO model_pricing (
                    model_id, display_name, input_cost_per_million,
                    output_cost_per_million, cache_read_cost_per_million,
                    cache_creation_cost_per_million
                 ) VALUES ('historical-model', 'Changed current pricing', '9', '9', '9', '9')",
                [],
            )?;
        }

        let msg = parsed_usage("msg_complete");
        assert!(!insert_session_log_entry(
            &db,
            "session:msg_complete",
            &msg
        )?);

        let conn = lock_conn!(db.conn);
        let persisted: (String, String, String, String, String, String, String) = conn.query_row(
            "SELECT pricing_model, cost_multiplier, input_cost_usd, output_cost_usd,
                    cache_read_cost_usd, cache_creation_cost_usd, total_cost_usd
             FROM proxy_request_logs WHERE request_id = 'proxy-claude-msg_complete'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )?;
        assert_eq!(
            persisted,
            (
                "historical-model".to_string(),
                "2".to_string(),
                "1".to_string(),
                "2".to_string(),
                "3".to_string(),
                "4".to_string(),
                "10".to_string(),
            )
        );

        Ok(())
    }

    #[test]
    fn test_insert_claude_session_hydrates_zero_usage_success_proxy() -> Result<(), AppError> {
        let db = Database::memory()?;
        insert_fingerprint_matching_proxy(&db, "claude", Some("msg_same"), 200)?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute(
                "UPDATE proxy_request_logs
                 SET input_tokens = 0, output_tokens = 0,
                     cache_read_tokens = 0, cache_creation_tokens = 0,
                     pricing_model = 'routed-model', cost_multiplier = '1.5'
                 WHERE request_id = 'proxy-claude-msg_same'",
                [],
            )?;
            conn.execute(
                "INSERT OR REPLACE INTO model_pricing (
                    model_id, display_name, input_cost_per_million,
                    output_cost_per_million, cache_read_cost_per_million,
                    cache_creation_cost_per_million
                 ) VALUES ('routed-model', 'Routed model', '1', '2', '3', '4')",
                [],
            )?;
        }

        let msg = parsed_usage("msg_same");
        assert!(!insert_session_log_entry(&db, "session:msg_same", &msg)?);

        let conn = lock_conn!(db.conn);
        let effective_filter = effective_usage_log_filter("l");
        let sql = format!(
            "SELECT request_id, input_tokens, output_tokens,
                    cache_read_tokens, cache_creation_tokens
             FROM proxy_request_logs l WHERE {effective_filter}"
        );
        let effective: (String, i64, i64, i64, i64) = conn.query_row(&sql, [], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?;
        assert_eq!(
            effective,
            ("proxy-claude-msg_same".to_string(), 100, 20, 10, 5)
        );
        let persisted_costs: (String, String, String, String, String, String, String) = conn
            .query_row(
                "SELECT pricing_model, cost_multiplier, input_cost_usd,
                        output_cost_usd, cache_read_cost_usd,
                        cache_creation_cost_usd, total_cost_usd
                 FROM proxy_request_logs
                 WHERE request_id = 'proxy-claude-msg_same'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )?;
        assert_eq!(persisted_costs.0, "routed-model");
        assert_eq!(persisted_costs.1, "1.5");
        for (actual, expected) in [
            (persisted_costs.2, Decimal::new(1, 4)),
            (persisted_costs.3, Decimal::new(4, 5)),
            (persisted_costs.4, Decimal::new(3, 5)),
            (persisted_costs.5, Decimal::new(2, 5)),
            (persisted_costs.6, Decimal::new(285, 6)),
        ] {
            assert_eq!(actual.parse::<Decimal>().unwrap(), expected);
        }

        Ok(())
    }

    #[test]
    fn test_insert_claude_session_enriches_partial_failed_proxy() -> Result<(), AppError> {
        let db = Database::memory()?;
        insert_fingerprint_matching_proxy(&db, "claude", Some("msg_partial_failed"), 502)?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute(
                "UPDATE proxy_request_logs
                 SET input_tokens = 120, output_tokens = 0,
                     pricing_model = 'routed-model', error_message = 'upstream failure',
                     cost_multiplier = '1.5', input_cost_usd = '0.00012',
                     output_cost_usd = '0', cache_read_cost_usd = '0.00003',
                     cache_creation_cost_usd = '0.00002', total_cost_usd = '0.000255'
                 WHERE request_id = 'proxy-claude-msg_partial_failed'",
                [],
            )?;
            conn.execute(
                "INSERT OR REPLACE INTO model_pricing (
                    model_id, display_name, input_cost_per_million,
                    output_cost_per_million, cache_read_cost_per_million,
                    cache_creation_cost_per_million
                 ) VALUES ('routed-model', 'Routed model', '1', '2', '3', '4')",
                [],
            )?;
        }

        let msg = parsed_usage("msg_partial_failed");
        assert!(!insert_session_log_entry(
            &db,
            "session:msg_partial_failed",
            &msg,
        )?);

        let conn = lock_conn!(db.conn);
        let persisted: (
            i64,
            String,
            i64,
            i64,
            i64,
            i64,
            String,
            String,
            String,
            String,
            String,
        ) = conn.query_row(
            "SELECT status_code, error_message, input_tokens, output_tokens,
                        cache_read_tokens, cache_creation_tokens,
                        input_cost_usd, output_cost_usd, cache_read_cost_usd,
                        cache_creation_cost_usd, total_cost_usd
                 FROM proxy_request_logs
                 WHERE request_id = 'proxy-claude-msg_partial_failed'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                ))
            },
        )?;
        assert_eq!(persisted.1, "upstream failure");
        assert_eq!(
            (
                persisted.0,
                persisted.2,
                persisted.3,
                persisted.4,
                persisted.5
            ),
            (502, 100, 20, 10, 5)
        );
        for (actual, expected) in [
            (persisted.6, Decimal::new(1, 4)),
            (persisted.7, Decimal::new(4, 5)),
            (persisted.8, Decimal::new(3, 5)),
            (persisted.9, Decimal::new(2, 5)),
            (persisted.10, Decimal::new(285, 6)),
        ] {
            assert_eq!(actual.parse::<Decimal>().unwrap(), expected);
        }
        let session_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM proxy_request_logs WHERE data_source = 'session_log'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(session_count, 0);

        Ok(())
    }

    #[test]
    fn test_insert_claude_session_fills_missing_success_proxy_output() -> Result<(), AppError> {
        let db = Database::memory()?;
        insert_fingerprint_matching_proxy(&db, "claude", Some("msg_partial_success"), 200)?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute(
                "UPDATE proxy_request_logs
                 SET input_tokens = 120, output_tokens = 0, pricing_model = 'routed-model',
                     cost_multiplier = '1.5', input_cost_usd = '0.00012',
                     output_cost_usd = '0', cache_read_cost_usd = '0.00003',
                     cache_creation_cost_usd = '0.00002', total_cost_usd = '0.000255'
                 WHERE request_id = 'proxy-claude-msg_partial_success'",
                [],
            )?;
            conn.execute(
                "INSERT OR REPLACE INTO model_pricing (
                    model_id, display_name, input_cost_per_million,
                    output_cost_per_million, cache_read_cost_per_million,
                    cache_creation_cost_per_million
                 ) VALUES ('routed-model', 'Routed model', '1', '2', '3', '4')",
                [],
            )?;
        }

        let msg = parsed_usage("msg_partial_success");
        assert!(!insert_session_log_entry(
            &db,
            "session:msg_partial_success",
            &msg,
        )?);

        let conn = lock_conn!(db.conn);
        let persisted: (
            i64,
            i64,
            i64,
            i64,
            i64,
            String,
            String,
            String,
            String,
            String,
        ) = conn.query_row(
            "SELECT status_code, input_tokens, output_tokens,
                    cache_read_tokens, cache_creation_tokens,
                    input_cost_usd, output_cost_usd, cache_read_cost_usd,
                    cache_creation_cost_usd, total_cost_usd
             FROM proxy_request_logs
             WHERE request_id = 'proxy-claude-msg_partial_success'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ))
            },
        )?;
        assert_eq!(
            (
                persisted.0,
                persisted.1,
                persisted.2,
                persisted.3,
                persisted.4
            ),
            (200, 120, 20, 10, 5)
        );
        for (actual, expected) in [
            (persisted.5, Decimal::new(12, 5)),
            (persisted.6, Decimal::new(4, 5)),
            (persisted.7, Decimal::new(3, 5)),
            (persisted.8, Decimal::new(2, 5)),
            (persisted.9, Decimal::new(315, 6)),
        ] {
            assert_eq!(actual.parse::<Decimal>().unwrap(), expected);
        }

        Ok(())
    }

    #[test]
    fn test_insert_claude_session_preserves_partial_costs_without_current_pricing(
    ) -> Result<(), AppError> {
        let db = Database::memory()?;
        insert_fingerprint_matching_proxy(&db, "claude", Some("msg_partial_historical"), 200)?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute(
                "UPDATE proxy_request_logs
                 SET output_tokens = 0, pricing_model = 'removed-model',
                     cost_multiplier = '2', input_cost_usd = '0.5',
                     output_cost_usd = '0', cache_read_cost_usd = '0.2',
                     cache_creation_cost_usd = '0.1', total_cost_usd = '1.6'
                 WHERE request_id = 'proxy-claude-msg_partial_historical'",
                [],
            )?;
            conn.execute(
                "DELETE FROM model_pricing WHERE model_id = 'removed-model'",
                [],
            )?;
        }

        let msg = parsed_usage("msg_partial_historical");
        assert!(!insert_session_log_entry(
            &db,
            "session:msg_partial_historical",
            &msg,
        )?);

        let conn = lock_conn!(db.conn);
        let persisted: (i64, String, String, String, String, String) = conn.query_row(
            "SELECT output_tokens, input_cost_usd, output_cost_usd,
                    cache_read_cost_usd, cache_creation_cost_usd, total_cost_usd
             FROM proxy_request_logs
             WHERE request_id = 'proxy-claude-msg_partial_historical'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )?;
        assert_eq!(persisted.0, 20);
        assert_eq!(persisted.1, "0.5");
        assert_eq!(persisted.2, "0");
        assert_eq!(persisted.3, "0.2");
        assert_eq!(persisted.4, "0.1");
        assert_eq!(persisted.5.parse::<Decimal>().unwrap(), Decimal::new(16, 1));

        Ok(())
    }

    #[test]
    fn test_insert_claude_session_uses_transcript_pricing_for_unpriced_partial_proxy(
    ) -> Result<(), AppError> {
        let db = Database::memory()?;
        insert_fingerprint_matching_proxy(&db, "claude", Some("msg_partial_unpriced"), 200)?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute(
                "UPDATE proxy_request_logs
                 SET output_tokens = 0, pricing_model = 'unknown'
                 WHERE request_id = 'proxy-claude-msg_partial_unpriced'",
                [],
            )?;
            conn.execute(
                "INSERT OR REPLACE INTO model_pricing (
                    model_id, display_name, input_cost_per_million,
                    output_cost_per_million, cache_read_cost_per_million,
                    cache_creation_cost_per_million
                 ) VALUES ('claude-sonnet-4-5', 'Transcript model', '1', '2', '3', '4')",
                [],
            )?;
        }

        let msg = parsed_usage("msg_partial_unpriced");
        assert!(!insert_session_log_entry(
            &db,
            "session:msg_partial_unpriced",
            &msg,
        )?);

        let conn = lock_conn!(db.conn);
        let persisted: (String, String, i64, String) = conn.query_row(
            "SELECT pricing_model, cost_multiplier, output_tokens, total_cost_usd
             FROM proxy_request_logs
             WHERE request_id = 'proxy-claude-msg_partial_unpriced'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        assert_eq!(persisted.0, "claude-sonnet-4-5");
        assert_eq!(persisted.1, "1");
        assert_eq!(persisted.2, 20);
        assert_eq!(persisted.3.parse::<Decimal>().unwrap(), Decimal::new(19, 5));
        let session_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM proxy_request_logs WHERE data_source = 'session_log'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(session_count, 0);

        Ok(())
    }

    #[test]
    fn test_insert_claude_session_does_not_hydrate_when_billable_proxy_exists(
    ) -> Result<(), AppError> {
        let db = Database::memory()?;
        insert_fingerprint_matching_proxy(&db, "claude", Some("msg_same"), 200)?;
        insert_zero_usage_pricing_evidence(
            &db,
            "proxy-claude-empty",
            "claude",
            "msg_same",
            Some("routed-model"),
            "1.5",
        )?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute(
                "UPDATE proxy_request_logs
                 SET status_code = 200
                 WHERE request_id = 'proxy-claude-empty'",
                [],
            )?;
        }

        let msg = parsed_usage("msg_same");
        assert!(!insert_session_log_entry(&db, "session:msg_same", &msg)?);

        let conn = lock_conn!(db.conn);
        let totals: (i64, i64, i64, i64) = conn.query_row(
            "SELECT SUM(input_tokens), SUM(output_tokens),
                    SUM(cache_read_tokens), SUM(cache_creation_tokens)
             FROM proxy_request_logs
             WHERE message_id = 'msg_same'
               AND COALESCE(data_source, 'proxy') = 'proxy'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        assert_eq!(totals, (100, 20, 10, 5));
        let empty_tokens: (i64, i64, i64, i64) = conn.query_row(
            "SELECT input_tokens, output_tokens,
                    cache_read_tokens, cache_creation_tokens
             FROM proxy_request_logs
             WHERE request_id = 'proxy-claude-empty'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        assert_eq!(empty_tokens, (0, 0, 0, 0));
        let session_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM proxy_request_logs WHERE data_source = 'session_log'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(session_count, 0);
        let effective_filter = effective_usage_log_filter("l");
        let effective_count_sql = format!(
            "SELECT COUNT(*) FROM proxy_request_logs l
             WHERE message_id = 'msg_same' AND {effective_filter}"
        );
        let effective_count: i64 = conn.query_row(&effective_count_sql, [], |row| row.get(0))?;
        assert_eq!(effective_count, 1);

        Ok(())
    }

    #[test]
    fn test_insert_claude_session_leaves_ambiguous_billable_proxies_unchanged(
    ) -> Result<(), AppError> {
        let db = Database::memory()?;
        insert_fingerprint_matching_proxy(&db, "claude", Some("msg_ambiguous"), 200)?;
        insert_fingerprint_matching_proxy(&db, "claude-desktop", Some("msg_ambiguous"), 200)?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute(
                "UPDATE proxy_request_logs SET output_tokens = 7
                 WHERE request_id = 'proxy-claude-msg_ambiguous'",
                [],
            )?;
            conn.execute(
                "UPDATE proxy_request_logs SET output_tokens = 9
                 WHERE request_id = 'proxy-claude-desktop-msg_ambiguous'",
                [],
            )?;
        }

        let msg = parsed_usage("msg_ambiguous");
        assert!(!insert_session_log_entry(
            &db,
            "session:msg_ambiguous",
            &msg,
        )?);

        let conn = lock_conn!(db.conn);
        let mut stmt = conn.prepare(
            "SELECT request_id, output_tokens FROM proxy_request_logs
             WHERE message_id = 'msg_ambiguous' ORDER BY request_id",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(
            rows,
            vec![
                ("proxy-claude-desktop-msg_ambiguous".to_string(), 9),
                ("proxy-claude-msg_ambiguous".to_string(), 7),
            ]
        );

        Ok(())
    }

    #[test]
    fn test_insert_claude_session_keeps_zero_usage_failed_proxy() -> Result<(), AppError> {
        let db = Database::memory()?;
        insert_fingerprint_matching_proxy(&db, "claude-desktop", Some("msg_same"), 200)?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute(
                "UPDATE proxy_request_logs
                 SET input_tokens = 0, output_tokens = 0,
                     cache_read_tokens = 0, cache_creation_tokens = 0,
                     error_message = 'upstream_response_incomplete:length'
                 WHERE request_id = 'proxy-claude-desktop-msg_same'",
                [],
            )?;
        }

        let msg = parsed_usage("msg_same");
        assert!(insert_session_log_entry(&db, "session:msg_same", &msg)?);

        let conn = lock_conn!(db.conn);
        let effective_filter = effective_usage_log_filter("l");
        let effective_count: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM proxy_request_logs l WHERE {effective_filter}"),
            [],
            |row| row.get(0),
        )?;
        assert_eq!(effective_count, 2);
        drop(conn);

        let by_app = db.get_usage_summary_by_app(None, None, None, None)?;
        assert_eq!(by_app.len(), 1);
        assert_eq!(by_app[0].app_type, "claude");
        assert_eq!(by_app[0].summary.total_requests, 2);
        assert_eq!(by_app[0].summary.success_rate, 50.0);

        Ok(())
    }

    #[test]
    fn test_session_fallback_uses_exact_proxy_pricing_evidence() -> Result<(), AppError> {
        let db = Database::memory()?;
        insert_fingerprint_matching_proxy(&db, "claude", Some("msg_evidence"), 502)?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute(
                "UPDATE proxy_request_logs
                 SET input_tokens = 0, output_tokens = 0,
                     cache_read_tokens = 0, cache_creation_tokens = 0,
                     pricing_model = 'routed-evidence-model', cost_multiplier = '1.5'
                 WHERE request_id = 'proxy-claude-msg_evidence'",
                [],
            )?;
            conn.execute(
                "INSERT OR REPLACE INTO model_pricing (
                    model_id, display_name, input_cost_per_million,
                    output_cost_per_million, cache_read_cost_per_million,
                    cache_creation_cost_per_million
                 ) VALUES (
                    'routed-evidence-model', 'Routed evidence model',
                    '1000000', '2000000', '3000000', '4000000'
                 )",
                [],
            )?;
        }

        let msg = parsed_usage("msg_evidence");
        assert!(insert_session_log_entry(&db, "session:msg_evidence", &msg)?);

        let conn = lock_conn!(db.conn);
        let persisted: (
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            String,
        ) = conn.query_row(
            "SELECT model, request_model, pricing_model, cost_multiplier,
                        input_cost_usd, output_cost_usd, cache_read_cost_usd,
                        cache_creation_cost_usd, total_cost_usd
                 FROM proxy_request_logs WHERE request_id = 'session:msg_evidence'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                ))
            },
        )?;
        let (
            model,
            request_model,
            pricing_model,
            multiplier,
            input,
            output,
            cache_read,
            cache_write,
            total,
        ) = persisted;
        assert_eq!(model, "claude-sonnet-4-5");
        assert_eq!(request_model, "claude-sonnet-4-5");
        assert_eq!(pricing_model, "routed-evidence-model");
        assert_eq!(multiplier, "1.5");
        for (actual, expected) in [
            (input, 100),
            (output, 40),
            (cache_read, 30),
            (cache_write, 20),
            (total, 285),
        ] {
            assert_eq!(actual.parse::<Decimal>().unwrap(), Decimal::from(expected));
        }

        Ok(())
    }

    #[test]
    fn test_resync_reprices_session_after_late_exact_proxy_evidence() -> Result<(), AppError> {
        let db = Database::memory()?;
        let projects_dir =
            std::env::temp_dir().join(format!("nexus-composer-test-{}", uuid::Uuid::new_v4()));
        let project_dir = projects_dir.join("project");
        fs::create_dir_all(&project_dir).unwrap();
        let transcript = project_dir.join("session.jsonl");
        fs::write(
            &transcript,
            concat!(
                r#"{"type":"assistant","message":{"id":"msg_late_evidence","model":"claude-sonnet-4-5","usage":{"input_tokens":100,"output_tokens":20,"cache_read_input_tokens":10,"cache_creation_input_tokens":5},"stop_reason":"end_turn"},"timestamp":"1970-01-01T00:16:45Z","sessionId":"session-1"}"#,
                "\n"
            ),
        )
        .unwrap();

        let first_sync = sync_claude_session_logs_from_projects_dir(&db, &projects_dir)?;
        assert_eq!(first_sync.imported, 1);

        insert_zero_usage_pricing_evidence(
            &db,
            "late-proxy-evidence",
            "claude",
            "msg_late_evidence",
            Some("late-routed-model"),
            "1.5",
        )?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute(
                "INSERT OR REPLACE INTO model_pricing (
                    model_id, display_name, input_cost_per_million,
                    output_cost_per_million, cache_read_cost_per_million,
                    cache_creation_cost_per_million
                 ) VALUES (
                    'late-routed-model', 'Late routed model',
                    '1000000', '2000000', '3000000', '4000000'
                 ), (
                    'claude-sonnet-4-5', 'Transcript model',
                    '5000000', '6000000', '7000000', '8000000'
                 )",
                [],
            )?;
        }

        let second_sync = sync_claude_session_logs_from_projects_dir(&db, &projects_dir)?;
        assert_eq!(second_sync.files_scanned, 1);
        assert_eq!(second_sync.imported, 0);
        assert_eq!(second_sync.skipped, 0);

        let conn = lock_conn!(db.conn);
        let (pricing_model, multiplier, total_cost): (String, String, String) = conn.query_row(
            "SELECT pricing_model, cost_multiplier, total_cost_usd
             FROM proxy_request_logs WHERE request_id = 'session:msg_late_evidence'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(pricing_model, "late-routed-model");
        assert_eq!(multiplier, "1.5");
        assert_eq!(total_cost.parse::<Decimal>().unwrap(), Decimal::from(285));
        drop(conn);

        insert_zero_usage_pricing_evidence(
            &db,
            "conflicting-proxy-evidence",
            "claude",
            "msg_late_evidence",
            Some("conflicting-routed-model"),
            "1.5",
        )?;
        let third_sync = sync_claude_session_logs_from_projects_dir(&db, &projects_dir)?;
        assert_eq!(third_sync.imported, 0);
        assert_eq!(third_sync.skipped, 0);
        let conn = lock_conn!(db.conn);
        let reset: (String, String, String, String, String, String, String) = conn.query_row(
            "SELECT pricing_model, cost_multiplier, input_cost_usd, output_cost_usd,
                    cache_read_cost_usd, cache_creation_cost_usd, total_cost_usd
             FROM proxy_request_logs WHERE request_id = 'session:msg_late_evidence'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )?;
        assert_eq!(reset.0, "claude-sonnet-4-5");
        assert_eq!(reset.1, "1");
        for (actual, expected) in [
            (reset.2, 500),
            (reset.3, 120),
            (reset.4, 70),
            (reset.5, 40),
            (reset.6, 730),
        ] {
            assert_eq!(actual.parse::<Decimal>().unwrap(), Decimal::from(expected));
        }
        drop(conn);

        let fourth_sync = sync_claude_session_logs_from_projects_dir(&db, &projects_dir)?;
        assert_eq!(fourth_sync.imported, 0);
        assert_eq!(fourth_sync.skipped, 0);
        let conn = lock_conn!(db.conn);
        assert_eq!(conn.changes(), 0);
        drop(conn);

        fs::remove_dir_all(&projects_dir).ok();
        Ok(())
    }

    #[test]
    fn test_sync_repairs_late_evidence_without_projects_directory() -> Result<(), AppError> {
        let db = Database::memory()?;
        let msg = parsed_usage("msg_missing_projects");
        assert!(insert_session_log_entry(
            &db,
            "session:msg_missing_projects",
            &msg,
        )?);
        insert_zero_usage_pricing_evidence(
            &db,
            "missing-projects-evidence",
            "claude",
            "msg_missing_projects",
            Some("missing-projects-model"),
            "2",
        )?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute(
                "INSERT OR REPLACE INTO model_pricing (
                    model_id, display_name, input_cost_per_million,
                    output_cost_per_million, cache_read_cost_per_million,
                    cache_creation_cost_per_million
                 ) VALUES (
                    'missing-projects-model', 'Missing projects model',
                    '1000000', '2000000', '3000000', '4000000'
                 )",
                [],
            )?;
        }

        let missing_projects_dir =
            std::env::temp_dir().join(format!("nexus-composer-missing-{}", uuid::Uuid::new_v4()));
        let result = sync_claude_session_logs_from_projects_dir(&db, &missing_projects_dir)?;
        assert_eq!(result.files_scanned, 0);
        assert_eq!(result.imported, 0);
        assert_eq!(result.skipped, 0);

        let conn = lock_conn!(db.conn);
        let (pricing_model, multiplier, total_cost): (String, String, String) = conn.query_row(
            "SELECT pricing_model, cost_multiplier, total_cost_usd
             FROM proxy_request_logs WHERE request_id = 'session:msg_missing_projects'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(pricing_model, "missing-projects-model");
        assert_eq!(multiplier, "2");
        assert_eq!(total_cost.parse::<Decimal>().unwrap(), Decimal::from(380));

        Ok(())
    }

    #[test]
    fn test_sync_reconciles_late_partial_proxy_without_projects_directory() -> Result<(), AppError>
    {
        let db = Database::memory()?;
        let msg = parsed_usage("msg_late_partial");
        assert!(insert_session_log_entry(
            &db,
            "session:msg_late_partial",
            &msg,
        )?);
        insert_fingerprint_matching_proxy(&db, "claude", Some("msg_late_partial"), 200)?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute(
                "UPDATE proxy_request_logs
                 SET input_tokens = 120, output_tokens = 10,
                     cache_read_tokens = 9, cache_creation_tokens = 4,
                     pricing_model = 'late-partial-model',
                     cost_multiplier = '1.5', input_cost_usd = '0.0001',
                     output_cost_usd = '0', cache_read_cost_usd = '0.00003',
                     cache_creation_cost_usd = '0.00002', total_cost_usd = '0.000225',
                     error_message = 'upstream_response_incomplete:length'
                 WHERE request_id = 'proxy-claude-msg_late_partial'",
                [],
            )?;
            conn.execute(
                "INSERT OR REPLACE INTO model_pricing (
                    model_id, display_name, input_cost_per_million,
                    output_cost_per_million, cache_read_cost_per_million,
                    cache_creation_cost_per_million
                 ) VALUES ('late-partial-model', 'Late partial model', '1', '2', '3', '4')",
                [],
            )?;
        }

        let missing_projects_dir =
            std::env::temp_dir().join(format!("nexus-composer-missing-{}", uuid::Uuid::new_v4()));
        let result = sync_claude_session_logs_from_projects_dir(&db, &missing_projects_dir)?;
        assert_eq!(result.files_scanned, 0);

        let conn = lock_conn!(db.conn);
        let proxy: (i64, String, i64, i64, i64, i64, String) = conn.query_row(
            "SELECT status_code, input_tokens, output_tokens,
                    cache_read_tokens, cache_creation_tokens, total_cost_usd,
                    error_message
             FROM proxy_request_logs
             WHERE request_id = 'proxy-claude-msg_late_partial'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(6)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )?;
        assert_eq!(
            (proxy.0, proxy.2, proxy.3, proxy.4, proxy.5),
            (200, 100, 20, 10, 5)
        );
        assert_eq!(proxy.1, "upstream_response_incomplete:length");
        assert_eq!(proxy.6.parse::<Decimal>().unwrap(), Decimal::new(285, 6));

        let effective_filter = effective_usage_log_filter("l");
        let effective_sql = format!(
            "SELECT request_id FROM proxy_request_logs l
             WHERE message_id = 'msg_late_partial' AND {effective_filter}"
        );
        let effective_id: String = conn.query_row(&effective_sql, [], |row| row.get(0))?;
        assert_eq!(effective_id, "proxy-claude-msg_late_partial");

        Ok(())
    }

    #[test]
    fn test_resync_hydrates_late_zero_usage_success_proxy() -> Result<(), AppError> {
        let db = Database::memory()?;
        let msg = parsed_usage("msg_canonical_late");

        assert!(insert_session_log_entry(
            &db,
            "session:msg_canonical_late",
            &msg,
        )?);
        insert_zero_usage_pricing_evidence(
            &db,
            "failed-pricing-evidence",
            "claude",
            "msg_canonical_late",
            Some("noncanonical-routed-model"),
            "1.5",
        )?;
        insert_zero_usage_pricing_evidence(
            &db,
            "canonical-success",
            "claude",
            "msg_canonical_late",
            Some("canonical-routed-model"),
            "2",
        )?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute(
                "UPDATE proxy_request_logs
                 SET status_code = 200
                 WHERE request_id = 'canonical-success'",
                [],
            )?;
        }

        assert!(!insert_session_log_entry(
            &db,
            "session:msg_canonical_late",
            &msg,
        )?);

        let conn = lock_conn!(db.conn);
        let effective_filter = effective_usage_log_filter("l");
        let sql = format!(
            "SELECT request_id, pricing_model, cost_multiplier,
                    input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens
             FROM proxy_request_logs l
             WHERE request_id = 'canonical-success'
               AND {effective_filter}"
        );
        let effective: (String, String, String, i64, i64, i64, i64) =
            conn.query_row(&sql, [], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            })?;
        assert_eq!(effective.0, "canonical-success");
        assert_eq!(effective.1, "canonical-routed-model");
        assert_eq!(effective.2, "2");
        assert_eq!(
            (effective.3, effective.4, effective.5, effective.6),
            (100, 20, 10, 5)
        );

        let effective_ids_sql = format!(
            "SELECT request_id FROM proxy_request_logs l
             WHERE {effective_filter}
             ORDER BY request_id"
        );
        let mut stmt = conn.prepare(&effective_ids_sql)?;
        let effective_ids = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(
            effective_ids,
            vec![
                "canonical-success".to_string(),
                "failed-pricing-evidence".to_string(),
            ]
        );

        Ok(())
    }

    #[test]
    fn test_session_fallback_ignores_other_app_pricing_evidence() -> Result<(), AppError> {
        let db = Database::memory()?;
        insert_fingerprint_matching_proxy(&db, "codex", Some("msg_evidence"), 502)?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute(
                "UPDATE proxy_request_logs
                 SET input_tokens = 0, output_tokens = 0,
                     cache_read_tokens = 0, cache_creation_tokens = 0,
                     pricing_model = 'wrong-app-model', cost_multiplier = '9'
                 WHERE request_id = 'proxy-codex-msg_evidence'",
                [],
            )?;
        }

        let msg = parsed_usage("msg_evidence");
        assert!(insert_session_log_entry(&db, "session:msg_evidence", &msg)?);

        let conn = lock_conn!(db.conn);
        let (pricing_model, multiplier): (String, String) = conn.query_row(
            "SELECT pricing_model, cost_multiplier
             FROM proxy_request_logs WHERE request_id = 'session:msg_evidence'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(pricing_model, "claude-sonnet-4-5");
        assert_eq!(multiplier, "1");

        Ok(())
    }

    #[test]
    fn test_session_fallback_accepts_desktop_pricing_evidence() -> Result<(), AppError> {
        let db = Database::memory()?;
        insert_zero_usage_pricing_evidence(
            &db,
            "desktop-evidence",
            "claude-desktop",
            "msg_evidence",
            Some("desktop-routed-model"),
            "0",
        )?;
        insert_zero_usage_pricing_evidence(
            &db,
            "claude-evidence",
            "claude",
            "msg_evidence",
            Some("desktop-routed-model"),
            "0.0",
        )?;

        let msg = parsed_usage("msg_evidence");
        assert!(insert_session_log_entry(&db, "session:msg_evidence", &msg)?);

        let conn = lock_conn!(db.conn);
        let (pricing_model, multiplier, total_cost): (String, String, String) = conn.query_row(
            "SELECT pricing_model, cost_multiplier, total_cost_usd
             FROM proxy_request_logs WHERE request_id = 'session:msg_evidence'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(pricing_model, "desktop-routed-model");
        assert_eq!(multiplier.parse::<Decimal>().unwrap(), Decimal::ZERO);
        assert_eq!(total_cost.parse::<Decimal>().unwrap(), Decimal::ZERO);

        Ok(())
    }

    #[test]
    fn test_session_fallback_rejects_ambiguous_pricing_evidence() -> Result<(), AppError> {
        let db = Database::memory()?;
        insert_zero_usage_pricing_evidence(
            &db,
            "claude-evidence",
            "claude",
            "msg_evidence",
            Some("routed-model-a"),
            "1.5",
        )?;
        insert_zero_usage_pricing_evidence(
            &db,
            "desktop-evidence",
            "claude-desktop",
            "msg_evidence",
            Some("routed-model-b"),
            "1.5",
        )?;

        let msg = parsed_usage("msg_evidence");
        assert!(insert_session_log_entry(&db, "session:msg_evidence", &msg)?);

        let conn = lock_conn!(db.conn);
        let (pricing_model, multiplier): (String, String) = conn.query_row(
            "SELECT pricing_model, cost_multiplier
             FROM proxy_request_logs WHERE request_id = 'session:msg_evidence'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(pricing_model, "claude-sonnet-4-5");
        assert_eq!(multiplier, "1");

        Ok(())
    }

    #[test]
    fn test_session_fallback_hides_ambiguous_zero_usage_success_proxies() -> Result<(), AppError> {
        let db = Database::memory()?;
        insert_zero_usage_pricing_evidence(
            &db,
            "claude-evidence",
            "claude",
            "msg_evidence",
            Some("routed-model-a"),
            "1.5",
        )?;
        insert_zero_usage_pricing_evidence(
            &db,
            "desktop-evidence",
            "claude-desktop",
            "msg_evidence",
            Some("routed-model-b"),
            "1.5",
        )?;
        {
            let conn = lock_conn!(db.conn);
            conn.execute("UPDATE proxy_request_logs SET status_code = 200", [])?;
        }

        let msg = parsed_usage("msg_evidence");
        assert!(insert_session_log_entry(&db, "session:msg_evidence", &msg)?);

        let conn = lock_conn!(db.conn);
        let effective_filter = effective_usage_log_filter("l");
        let sql = format!(
            "SELECT request_id FROM proxy_request_logs l
             WHERE {effective_filter}
             ORDER BY request_id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let effective_ids = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(effective_ids, vec!["session:msg_evidence".to_string()]);
        let (pricing_model, multiplier): (String, String) = conn.query_row(
            "SELECT pricing_model, cost_multiplier
             FROM proxy_request_logs WHERE request_id = 'session:msg_evidence'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(pricing_model, "claude-sonnet-4-5");
        assert_eq!(multiplier, "1");

        Ok(())
    }

    #[test]
    fn test_session_fallback_rejects_invalid_pricing_evidence() -> Result<(), AppError> {
        for status_code in [502, 200] {
            for (pricing_model, multiplier) in [
                (None, "1"),
                (Some("unknown"), "1"),
                (Some("routed-model"), "not-a-number"),
                (Some("routed-model"), "-1"),
            ] {
                let db = Database::memory()?;
                insert_zero_usage_pricing_evidence(
                    &db,
                    "invalid-evidence",
                    "claude",
                    "msg_evidence",
                    pricing_model,
                    multiplier,
                )?;
                {
                    let conn = lock_conn!(db.conn);
                    conn.execute(
                        "UPDATE proxy_request_logs SET status_code = ?1",
                        [status_code],
                    )?;
                }

                let msg = parsed_usage("msg_evidence");
                assert!(insert_session_log_entry(&db, "session:msg_evidence", &msg)?);

                let conn = lock_conn!(db.conn);
                let persisted: (String, String) = conn.query_row(
                    "SELECT pricing_model, cost_multiplier
                     FROM proxy_request_logs WHERE request_id = 'session:msg_evidence'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                assert_eq!(persisted.0, "claude-sonnet-4-5");
                assert_eq!(persisted.1, "1");
                if status_code == 200 {
                    let effective_filter = effective_usage_log_filter("l");
                    let sql = format!(
                        "SELECT request_id FROM proxy_request_logs l WHERE {effective_filter}"
                    );
                    let effective_id: String = conn.query_row(&sql, [], |row| row.get(0))?;
                    assert_eq!(effective_id, "session:msg_evidence");
                }
            }
        }

        Ok(())
    }

    #[test]
    fn test_insert_claude_session_stores_blank_message_id_as_null() -> Result<(), AppError> {
        let db = Database::memory()?;
        let msg = parsed_usage("   ");
        assert!(insert_session_log_entry(&db, "session:blank", &msg)?);

        let conn = lock_conn!(db.conn);
        let message_id: Option<String> = conn.query_row(
            "SELECT message_id FROM proxy_request_logs WHERE request_id = 'session:blank'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(message_id, None);

        Ok(())
    }

    #[test]
    fn test_collect_jsonl_files_includes_subagents() {
        let tmp =
            std::env::temp_dir().join(format!("nexus-composer-test-{}", uuid::Uuid::new_v4()));
        let project = tmp.join("project");
        let session_dir = project.join("test-session");
        let subagents_dir = session_dir.join("subagents");
        fs::create_dir_all(&subagents_dir).unwrap();

        fs::write(project.join("main.jsonl"), "{}").unwrap();
        fs::write(subagents_dir.join("agent-abc.jsonl"), "{}").unwrap();

        let files = collect_jsonl_files(&tmp);
        assert_eq!(files.len(), 2);
        let paths: Vec<String> = files
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        assert!(paths.iter().any(|p| p.contains("main.jsonl")));
        assert!(paths.iter().any(|p| p.contains("agent-abc.jsonl")));

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_collect_jsonl_files_includes_workflow_subagents() {
        // Claude Code Workflow nests sub-agent transcripts in
        // project/SESSION_ID/subagents/workflows/wf_<ID>/, one level deeper than regular sub-agents.
        let tmp =
            std::env::temp_dir().join(format!("nexus-composer-test-{}", uuid::Uuid::new_v4()));
        let project = tmp.join("project");
        let session_dir = project.join("test-session");
        let subagents_dir = session_dir.join("subagents");
        let wf_dir = subagents_dir.join("workflows").join("wf_test123");
        fs::create_dir_all(&wf_dir).unwrap();

        fs::write(project.join("main.jsonl"), "{}").unwrap();
        fs::write(subagents_dir.join("agent-plain.jsonl"), "{}").unwrap();
        fs::write(wf_dir.join("agent-wf.jsonl"), "{}").unwrap();
        // journal.jsonl will also be collected, but yields 0 entries during parsing since it has no assistant lines
        fs::write(wf_dir.join("journal.jsonl"), "{}").unwrap();

        let files = collect_jsonl_files(&tmp);
        let paths: Vec<String> = files
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();

        // Main session + regular sub-agent + Workflow sub-agent(agent-wf + journal) = 4
        assert_eq!(files.len(), 4);
        assert!(paths.iter().any(|p| p.contains("main.jsonl")));
        assert!(paths.iter().any(|p| p.contains("agent-plain.jsonl")));
        assert!(
            paths.iter().any(|p| p.contains("agent-wf.jsonl")),
            "Workflow sub-agent transcript must be collected"
        );

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn test_sync_imports_billable_message_without_stop_reason() -> Result<(), AppError> {
        // Regression: messages with missing stop_reason but real cache/input costs (Workflow /
        // sub-agent common pattern of "only message_start snapshot written, no final chunk") must be counted,
        // must not discard entirely due to missing stop_reason or output==0; placeholder rows with all 0 tokens should still be skipped.
        let db = Database::memory()?;
        let tmp =
            std::env::temp_dir().join(format!("nexus-composer-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let file = tmp.join("agent-wf.jsonl");

        // First line: no stop_reason, output=1, but cache_read/cache_creation are large -> should import
        // Second line: all tokens are 0 -> should skip (no billing significance)
        let billable = r#"{"type":"assistant","message":{"id":"msg_nostop","model":"claude-opus-4-8","usage":{"input_tokens":2,"output_tokens":1,"cache_read_input_tokens":48719,"cache_creation_input_tokens":2061}},"timestamp":"2026-06-07T13:01:23Z","sessionId":"session-wf"}"#;
        let empty = r#"{"type":"assistant","message":{"id":"msg_empty","model":"claude-opus-4-8","usage":{"input_tokens":0,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}},"timestamp":"2026-06-07T13:01:24Z","sessionId":"session-wf"}"#;
        fs::write(&file, format!("{billable}\n{empty}\n")).unwrap();

        let (imported, _skipped, _errors) = sync_single_file(&db, &file)?;
        assert_eq!(
            imported, 1,
            "messages with cache costs but no stop_reason must be imported"
        );

        let conn = lock_conn!(db.conn);
        let (cache_read, message_id): (i64, Option<String>) = conn.query_row(
            "SELECT cache_read_tokens, message_id FROM proxy_request_logs
             WHERE request_id = 'session:msg_nostop'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(cache_read, 48719, "cache_read must be fully recorded");
        assert_eq!(message_id.as_deref(), Some("msg_nostop"));
        let empty_exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM proxy_request_logs WHERE request_id = 'session:msg_empty')",
            [],
            |row| row.get(0),
        )?;
        assert!(
            !empty_exists,
            "messages with all 0 tokens should be skipped"
        );
        drop(conn);

        fs::remove_dir_all(&tmp).ok();
        Ok(())
    }
}
