//! Codex 会话日志使用追踪
//!
//! 从 ~/.codex/sessions/ 下的 JSONL 会话文件中提取精确 token 使用数据，
//! 替代原有的 state_5.sqlite 估算方案。
//!
//! ## 数据流
//! ```text
//! ~/.codex/sessions/YYYY/MM/DD/*.jsonl → 增量解析 → delta 计算 → 费用计算 → proxy_request_logs 表
//! ```
//!
//! ## 解析的事件类型
//! - `session_meta` → 提取 session_id
//! - `turn_context` → 提取当前 model
//! - `event_msg` (type=token_count) → 提取累计 token 用量，计算 delta

use crate::codex_config::get_codex_config_dir;
use crate::database::{lock_conn, Database};
use crate::error::AppError;
use crate::proxy::usage::calculator::{CostCalculator, ModelPricing};
use crate::proxy::usage::parser::TokenUsage;
use crate::services::session_usage::{get_sync_state, metadata_modified_nanos, SessionSyncResult};
use crate::services::usage_stats::{find_model_pricing, should_skip_session_insert, DedupKey};
use rust_decimal::Decimal;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const CODEX_CURSOR_GUARD_BYTES: u64 = 64;
const CODEX_CURSOR_GUARD_VERSION: &str = "v2";

/// 累计 token 用量（跟踪 total_token_usage 字段）
#[derive(Debug, Clone, Default)]
struct CumulativeTokens {
    input: u64,
    cached_input: u64,
    output: u64,
}

/// 单次 API 调用的 token 增量
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

/// 单文件解析时的运行状态
#[derive(Clone)]
struct FileParseState {
    session_id: Option<String>,
    current_model: String,
    prev_total: Option<CumulativeTokens>,
    event_index: u32,
}

impl Default for FileParseState {
    fn default() -> Self {
        Self {
            session_id: None,
            current_model: "unknown".to_string(),
            prev_total: None,
            event_index: 0,
        }
    }
}

struct CodexSyncCheckpoint {
    last_modified: i64,
    byte_offset: i64,
    line_offset: i64,
    state: FileParseState,
    cursor_guard: String,
}

#[derive(Debug, Default)]
struct CodexFileSyncStats {
    imported: u32,
    skipped: u32,
    errors: Vec<String>,
    bytes_read: u64,
    lines_parsed: u64,
    start_byte: i64,
    reset: bool,
}

/// 归一化 Codex 模型名
///
/// 处理规则（按顺序）：
/// 1. 转小写：`GLM-4.6` → `glm-4.6`
/// 2. 剥离 provider 前缀：`openai/gpt-5.4` → `gpt-5.4`
/// 3. 剥离 ISO 日期后缀：`gpt-5.4-2026-03-05` → `gpt-5.4`
/// 4. 剥离紧凑日期后缀：`gpt-5.4-20260305` → `gpt-5.4`
fn normalize_codex_model(raw: &str) -> String {
    // Step 1: 小写
    let mut name = raw.to_lowercase();

    // Step 2: 剥离 "provider/" 前缀（如 openai/, azure/）
    if let Some(pos) = name.rfind('/') {
        name = name[pos + 1..].to_string();
    }

    // Step 3: 剥离 ISO 日期后缀 -YYYY-MM-DD（正好 11 字符）
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

    // Step 4: 剥离紧凑日期后缀 -YYYYMMDD（正好 9 字符）
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

/// 计算两次累计值之间的 delta
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

/// 从 JSON Value 中提取累计 token 用量
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

fn ensure_codex_sync_state_table(db: &Database) -> Result<(), AppError> {
    let conn = lock_conn!(db.conn);
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS codex_session_sync_state_v2 (
            file_path TEXT PRIMARY KEY,
            last_modified INTEGER NOT NULL,
            byte_offset INTEGER NOT NULL DEFAULT 0,
            line_offset INTEGER NOT NULL DEFAULT 0,
            session_id TEXT,
            current_model TEXT NOT NULL DEFAULT 'unknown',
            previous_input INTEGER,
            previous_cached_input INTEGER,
            previous_output INTEGER,
            event_index INTEGER NOT NULL DEFAULT 0,
            cursor_guard TEXT NOT NULL DEFAULT '',
            last_synced_at INTEGER NOT NULL
        );",
    )
    .map_err(|e| AppError::Database(format!("创建 Codex 增量同步状态表失败: {e}")))
}

fn load_codex_checkpoint(
    db: &Database,
    file_path: &str,
) -> Result<Option<CodexSyncCheckpoint>, AppError> {
    let conn = lock_conn!(db.conn);
    let result = conn.query_row(
        "SELECT last_modified, byte_offset, line_offset, session_id, current_model,
                previous_input, previous_cached_input, previous_output, event_index, cursor_guard
         FROM codex_session_sync_state_v2
         WHERE file_path = ?1",
        rusqlite::params![file_path],
        |row| {
            let previous_input = row.get::<_, Option<i64>>(5)?;
            let previous_cached_input = row.get::<_, Option<i64>>(6)?;
            let previous_output = row.get::<_, Option<i64>>(7)?;
            let prev_total = match (previous_input, previous_cached_input, previous_output) {
                (Some(input), Some(cached_input), Some(output)) => Some(CumulativeTokens {
                    input: input.max(0) as u64,
                    cached_input: cached_input.max(0) as u64,
                    output: output.max(0) as u64,
                }),
                _ => None,
            };
            let event_index = row.get::<_, i64>(8)?.clamp(0, u32::MAX as i64) as u32;

            Ok(CodexSyncCheckpoint {
                last_modified: row.get(0)?,
                byte_offset: row.get::<_, i64>(1)?.max(0),
                line_offset: row.get::<_, i64>(2)?.max(0),
                state: FileParseState {
                    session_id: row.get(3)?,
                    current_model: row.get(4)?,
                    prev_total,
                    event_index,
                },
                cursor_guard: row.get(9)?,
            })
        },
    );

    match result {
        Ok(checkpoint) => Ok(Some(checkpoint)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(AppError::Database(format!(
            "读取 Codex 增量同步状态失败: {e}"
        ))),
    }
}

fn save_codex_checkpoint(
    db: &Database,
    file_path: &str,
    checkpoint: &CodexSyncCheckpoint,
) -> Result<(), AppError> {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let (previous_input, previous_cached_input, previous_output) =
        match checkpoint.state.prev_total.as_ref() {
            Some(total) => (
                Some(total.input.min(i64::MAX as u64) as i64),
                Some(total.cached_input.min(i64::MAX as u64) as i64),
                Some(total.output.min(i64::MAX as u64) as i64),
            ),
            None => (None, None, None),
        };

    let mut conn = lock_conn!(db.conn);
    let transaction = conn
        .transaction()
        .map_err(|e| AppError::Database(format!("开始 Codex 同步状态事务失败: {e}")))?;
    transaction
        .execute(
            "INSERT INTO codex_session_sync_state_v2 (
                file_path, last_modified, byte_offset, line_offset, session_id, current_model,
                previous_input, previous_cached_input, previous_output, event_index,
                cursor_guard, last_synced_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(file_path) DO UPDATE SET
                last_modified = excluded.last_modified,
                byte_offset = excluded.byte_offset,
                line_offset = excluded.line_offset,
                session_id = excluded.session_id,
                current_model = excluded.current_model,
                previous_input = excluded.previous_input,
                previous_cached_input = excluded.previous_cached_input,
                previous_output = excluded.previous_output,
                event_index = excluded.event_index,
                cursor_guard = excluded.cursor_guard,
                last_synced_at = excluded.last_synced_at",
            rusqlite::params![
                file_path,
                checkpoint.last_modified,
                checkpoint.byte_offset,
                checkpoint.line_offset,
                checkpoint.state.session_id.as_deref(),
                &checkpoint.state.current_model,
                previous_input,
                previous_cached_input,
                previous_output,
                checkpoint.state.event_index as i64,
                &checkpoint.cursor_guard,
                now,
            ],
        )
        .map_err(|e| AppError::Database(format!("更新 Codex 增量同步状态失败: {e}")))?;
    transaction
        .execute(
            "INSERT OR REPLACE INTO session_log_sync (
                file_path, last_modified, last_line_offset, last_synced_at
             ) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                file_path,
                checkpoint.last_modified,
                checkpoint.line_offset,
                now
            ],
        )
        .map_err(|e| AppError::Database(format!("更新兼容同步状态失败: {e}")))?;
    transaction
        .commit()
        .map_err(|e| AppError::Database(format!("提交 Codex 同步状态事务失败: {e}")))
}

#[cfg(unix)]
fn cursor_guard_metadata(metadata: &fs::Metadata) -> (String, String) {
    use std::os::unix::fs::MetadataExt;

    (
        format!("{}:{}", metadata.dev(), metadata.ino()),
        format!("{}:{}", metadata.ctime(), metadata.ctime_nsec()),
    )
}

#[cfg(not(unix))]
fn cursor_guard_metadata(metadata: &fs::Metadata) -> (String, String) {
    let created = metadata
        .created()
        .ok()
        .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    (
        created.to_string(),
        metadata_modified_nanos(metadata).to_string(),
    )
}

fn cursor_guard(file: &mut fs::File, byte_offset: i64) -> Result<String, AppError> {
    let byte_offset = byte_offset.max(0) as u64;
    let metadata = file
        .metadata()
        .map_err(|e| AppError::Config(format!("无法读取 Codex 同步校验元数据: {e}")))?;
    let (file_identity, change_marker) = cursor_guard_metadata(&metadata);
    let mut hasher = Sha256::new();
    hasher.update(byte_offset.to_le_bytes());

    if byte_offset > 0 {
        let guard_len = byte_offset.min(CODEX_CURSOR_GUARD_BYTES);
        let latest_start = byte_offset - guard_len;
        let middle_start = (byte_offset / 2)
            .saturating_sub(guard_len / 2)
            .min(latest_start);
        let mut sample_starts = vec![0, middle_start, latest_start];
        sample_starts.sort_unstable();
        sample_starts.dedup();

        let mut bytes = vec![0u8; guard_len as usize];
        for start in sample_starts {
            file.seek(SeekFrom::Start(start))
                .map_err(|e| AppError::Config(format!("无法定位 Codex 同步校验位置: {e}")))?;
            file.read_exact(&mut bytes)
                .map_err(|e| AppError::Config(format!("无法读取 Codex 同步校验数据: {e}")))?;
            hasher.update(start.to_le_bytes());
            hasher.update(&bytes);
        }
    }

    // Persist only metadata and a digest, never raw prompt or response bytes.
    Ok(format!(
        "{CODEX_CURSOR_GUARD_VERSION}|{file_identity}|{change_marker}|{:x}",
        hasher.finalize()
    ))
}

fn cursor_guard_matches(stored: &str, current: &str, allow_changed_marker: bool) -> bool {
    let stored_parts: Vec<&str> = stored.split('|').collect();
    let current_parts: Vec<&str> = current.split('|').collect();
    if stored_parts.len() != 4
        || current_parts.len() != 4
        || stored_parts[0] != CODEX_CURSOR_GUARD_VERSION
        || current_parts[0] != CODEX_CURSOR_GUARD_VERSION
    {
        return false;
    }

    stored_parts[1] == current_parts[1]
        && stored_parts[3] == current_parts[3]
        && (allow_changed_marker || stored_parts[2] == current_parts[2])
}

/// 同步 Codex 使用数据（从 JSONL 会话日志）
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

    ensure_codex_sync_state_table(db)?;

    for file_path in &files {
        match sync_single_codex_file_with_stats(db, file_path) {
            Ok(stats) => {
                if stats.bytes_read > 0 || stats.reset {
                    log::debug!(
                        "[CODEX-SYNC] incremental file pass: start_byte={}, bytes_read={}, parsed_lines={}, reset={}",
                        stats.start_byte,
                        stats.bytes_read,
                        stats.lines_parsed,
                        stats.reset
                    );
                }
                result.imported += stats.imported;
                result.skipped += stats.skipped;
                result.errors.extend(stats.errors);
            }
            Err(e) => {
                let msg = format!("Codex 会话文件解析失败 {}: {e}", file_path.display());
                log::warn!("[CODEX-SYNC] {msg}");
                result.errors.push(msg);
            }
        }
    }

    if result.imported > 0 {
        log::info!(
            "[CODEX-SYNC] 同步完成: 导入 {} 条, 跳过 {} 条, 扫描 {} 个文件",
            result.imported,
            result.skipped,
            result.files_scanned
        );
    }

    Ok(result)
}

/// 收集所有 Codex 会话 JSONL 文件
fn collect_codex_session_files(codex_dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();

    // 1. 扫描 sessions/YYYY/MM/DD/*.jsonl（日期分区目录）
    let sessions_dir = codex_dir.join("sessions");
    if sessions_dir.is_dir() {
        collect_jsonl_recursive(&sessions_dir, &mut files, 0, 3);
    }

    // 2. 扫描 archived_sessions/*.jsonl（扁平归档目录）
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

/// 递归扫描目录下的 .jsonl 文件（限制最大深度）
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
#[cfg(test)]
fn sync_single_codex_file(
    db: &Database,
    file_path: &Path,
) -> Result<(u32, u32, Vec<String>), AppError> {
    ensure_codex_sync_state_table(db)?;
    let stats = sync_single_codex_file_with_stats(db, file_path)?;
    Ok((stats.imported, stats.skipped, stats.errors))
}

fn sync_single_codex_file_with_stats(
    db: &Database,
    file_path: &Path,
) -> Result<CodexFileSyncStats, AppError> {
    let file_path_str = file_path.to_string_lossy().to_string();

    let mut file =
        fs::File::open(file_path).map_err(|e| AppError::Config(format!("无法打开文件: {e}")))?;
    let metadata = file
        .metadata()
        .map_err(|e| AppError::Config(format!("无法读取文件元数据: {e}")))?;
    let file_modified = metadata_modified_nanos(&metadata);
    let file_len = metadata.len().min(i64::MAX as u64) as i64;
    let mut stats = CodexFileSyncStats::default();

    let checkpoint = load_codex_checkpoint(db, &file_path_str)?;
    let (mut state, mut line_offset, start_byte) = match checkpoint {
        Some(mut checkpoint) => {
            let file_grew = checkpoint.byte_offset < file_len;
            let can_resume = checkpoint.byte_offset <= file_len
                && cursor_guard_matches(
                    &checkpoint.cursor_guard,
                    &cursor_guard(&mut file, checkpoint.byte_offset)?,
                    file_grew,
                );
            if can_resume {
                if checkpoint.byte_offset == file_len {
                    if checkpoint.last_modified != file_modified {
                        checkpoint.last_modified = file_modified;
                        checkpoint.cursor_guard = cursor_guard(&mut file, checkpoint.byte_offset)?;
                        save_codex_checkpoint(db, &file_path_str, &checkpoint)?;
                    }
                    stats.start_byte = checkpoint.byte_offset;
                    return Ok(stats);
                }
                (
                    checkpoint.state,
                    checkpoint.line_offset,
                    checkpoint.byte_offset,
                )
            } else {
                stats.reset = true;
                (FileParseState::default(), 0, 0)
            }
        }
        None => {
            // Existing installations only have the legacy line cursor. Keep unchanged
            // files cold; the first changed pass safely rebuilds parser state from byte
            // zero and stable request IDs deduplicate already imported records.
            let (legacy_modified, _) = get_sync_state(db, &file_path_str)?;
            if legacy_modified > 0 && file_modified <= legacy_modified {
                return Ok(stats);
            }
            (FileParseState::default(), 0, 0)
        }
    };
    stats.start_byte = start_byte;

    file.seek(SeekFrom::Start(start_byte as u64))
        .map_err(|e| AppError::Config(format!("无法定位 Codex 增量同步位置: {e}")))?;
    let mut reader = BufReader::new(file);
    let mut current_byte = start_byte;
    let mut line_bytes = Vec::new();

    loop {
        line_bytes.clear();
        let bytes_read = reader
            .read_until(b'\n', &mut line_bytes)
            .map_err(|e| AppError::Config(format!("无法读取 Codex 会话文件: {e}")))?;
        if bytes_read == 0 {
            break;
        }
        stats.bytes_read += bytes_read as u64;

        let next_byte = current_byte.saturating_add(bytes_read as i64);
        let has_newline = line_bytes.last() == Some(&b'\n');
        let json_bytes = if has_newline {
            &line_bytes[..line_bytes.len() - 1]
        } else {
            &line_bytes[..]
        };
        let line = String::from_utf8_lossy(json_bytes);

        let is_event_msg = line.contains("\"event_msg\"");
        let is_turn_context = line.contains("\"turn_context\"");
        let is_session_meta = line.contains("\"session_meta\"");
        let is_relevant = is_event_msg || is_turn_context || is_session_meta;
        let is_token_event = !is_event_msg || line.contains("\"token_count\"");

        let value = if !has_newline || (is_relevant && is_token_event) {
            match serde_json::from_slice::<serde_json::Value>(json_bytes) {
                Ok(value) => {
                    stats.lines_parsed += 1;
                    Some(value)
                }
                Err(_) if !has_newline => {
                    // The writer may still be appending this JSON value. Leave both
                    // byte and line checkpoints before the fragment so it is retried.
                    break;
                }
                Err(_) => None,
            }
        } else {
            None
        };

        current_byte = next_byte;
        line_offset += 1;

        let value = match value {
            Some(value) if is_relevant && is_token_event => value,
            _ => continue,
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
                    // model 可能在 payload.model 或 payload.info.model
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

                // 只处理 token_count 类型
                if payload.get("type").and_then(|t| t.as_str()) != Some("token_count") {
                    continue;
                }

                let info = match payload.get("info") {
                    Some(i) if !i.is_null() => i,
                    _ => continue, // 跳过 info 为 null 的首个事件
                };

                // 提取模型（token_count 事件也可能携带 model）
                if let Some(model) = info
                    .get("model")
                    .or_else(|| info.get("model_name"))
                    .or_else(|| payload.get("model"))
                    .and_then(|v| v.as_str())
                {
                    state.current_model = normalize_codex_model(model);
                }

                // 优先用 total_token_usage（累计值），fallback 到 last_token_usage（增量值）
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
                    // 累计值模式：计算与上次的 delta
                    let d = compute_delta(&state.prev_total, &cumulative);
                    state.prev_total = Some(cumulative);
                    d
                } else {
                    // 增量值模式：直接使用 last_token_usage 的值
                    DeltaTokens {
                        input: cumulative.input as u32,
                        cached_input: cumulative.cached_input as u32,
                        output: cumulative.output as u32,
                    }
                };

                // 钳制：cached 不应超过 input（防护异常数据）
                let delta = DeltaTokens {
                    cached_input: delta.cached_input.min(delta.input),
                    ..delta
                };

                if delta.is_zero() {
                    continue; // 跳过 task 边界的零 delta 事件
                }

                state.event_index += 1;

                // 生成唯一 request_id
                let session_id_str = state.session_id.as_deref().unwrap_or("unknown");
                let request_id = format!("codex_session:{}:{}", session_id_str, state.event_index);

                // 提取时间戳
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
                    Ok(true) => stats.imported += 1,
                    Ok(false) => stats.skipped += 1,
                    Err(e) => {
                        let error =
                            format!("{}: insert failed ({request_id}): {e}", file_path.display());
                        log::warn!("[CODEX-SYNC] {error}");
                        stats.errors.push(error);
                        stats.skipped += 1;
                    }
                }
            }
            _ => {}
        }
    }

    if stats.errors.is_empty() {
        let final_metadata = reader
            .get_ref()
            .metadata()
            .map_err(|e| AppError::Config(format!("无法读取 Codex 文件最终状态: {e}")))?;
        let final_modified = metadata_modified_nanos(&final_metadata);
        let guard = cursor_guard(reader.get_mut(), current_byte)?;
        save_codex_checkpoint(
            db,
            &file_path_str,
            &CodexSyncCheckpoint {
                last_modified: final_modified,
                byte_offset: current_byte,
                line_offset,
                state,
                cursor_guard: guard,
            },
        )?;
    }

    Ok(stats)
}

/// 插入单条 Codex 会话记录到 proxy_request_logs
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

    // 计算费用
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
                0i64,                // cache_creation_tokens: Codex 日志无此数据
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
        .map_err(|e| AppError::Database(format!("插入 Codex 会话日志失败: {e}")))?;

    if inserted_rows > 0 {
        crate::usage_events::notify_log_recorded();
    }

    Ok(true)
}

/// 查找 Codex 模型定价（带归一化）
fn find_codex_pricing(conn: &rusqlite::Connection, model_id: &str) -> Option<ModelPricing> {
    find_model_pricing(conn, &normalize_codex_model(model_id))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // task 边界：相同的累计值
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
        // 异常情况：当前值小于前值（不应发生，但需防护）
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
        // 某些版本可能使用 cache_read_input_tokens 而非 cached_input_tokens
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

        let tmp =
            std::env::temp_dir().join(format!("nexus-codex-sync-retry-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let file = tmp.join("retry.jsonl");
        let lines = [
            r#"{"type":"session_meta","payload":{"session_id":"retry-session"}}"#,
            r#"{"type":"turn_context","payload":{"model":"glm-5.2"}}"#,
            r#"{"type":"event_msg","timestamp":"2026-07-18T00:00:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":2,"output_tokens":1}}}}"#,
        ];
        fs::write(&file, lines.join("\n")).unwrap();

        let (_, _, errors) = sync_single_codex_file(&db, &file)?;
        assert_eq!(errors.len(), 1);
        assert_eq!(get_sync_state(&db, &file.to_string_lossy())?, (0, 0));
        assert!(
            load_codex_checkpoint(&db, &file.to_string_lossy())?.is_none(),
            "a failed insert must not advance the byte/parser checkpoint"
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
            (metadata_modified_nanos(&fs::metadata(&file).unwrap()), 3)
        );
        assert_eq!(
            load_codex_checkpoint(&db, &file.to_string_lossy())?
                .expect("checkpoint after successful retry")
                .byte_offset,
            fs::metadata(&file).unwrap().len() as i64
        );
        assert_eq!(sync_single_codex_file(&db, &file)?.0, 0);

        fs::remove_dir_all(&tmp).ok();
        Ok(())
    }

    #[test]
    fn appended_codex_sync_reads_only_the_new_tail() -> Result<(), AppError> {
        use std::io::Write;

        let db = Database::memory()?;
        ensure_codex_sync_state_table(&db)?;
        let tmp =
            std::env::temp_dir().join(format!("nexus-codex-sync-append-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let file = tmp.join("append.jsonl");
        let initial = [
            r#"{"type":"session_meta","payload":{"session_id":"append-session"}}"#,
            r#"{"type":"turn_context","payload":{"model":"glm-5.2"}}"#,
            r#"{"type":"event_msg","timestamp":"2026-07-18T00:00:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":2,"output_tokens":1}}}}"#,
        ]
        .join("\n")
            + "\n";
        fs::write(&file, &initial).unwrap();

        let first = sync_single_codex_file_with_stats(&db, &file)?;
        assert_eq!(first.start_byte, 0);
        assert_eq!(first.bytes_read, initial.len() as u64);
        assert_eq!(first.imported, 1);

        let old_len = fs::metadata(&file).unwrap().len();
        let appended = [
            r#"{"type":"turn_context","payload":{"model":"glm-5.2"}}"#,
            r#"{"type":"event_msg","timestamp":"2026-07-18T00:01:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":5,"output_tokens":3}}}}"#,
        ]
        .join("\n")
            + "\n";
        fs::OpenOptions::new()
            .append(true)
            .open(&file)
            .unwrap()
            .write_all(appended.as_bytes())
            .unwrap();
        {
            // Simulate a filesystem whose timestamp did not advance: growth must
            // still be authoritative and force a tail read.
            let appended_mtime = metadata_modified_nanos(&fs::metadata(&file).unwrap());
            let conn = lock_conn!(db.conn);
            conn.execute(
                "UPDATE codex_session_sync_state_v2
                 SET last_modified = ?1
                 WHERE file_path = ?2",
                rusqlite::params![appended_mtime, file.to_string_lossy()],
            )?;
        }

        let second = sync_single_codex_file_with_stats(&db, &file)?;
        assert_eq!(second.start_byte, old_len as i64);
        assert_eq!(second.bytes_read, appended.len() as u64);
        assert_eq!(second.imported, 1);
        assert!(!second.reset);

        let conn = lock_conn!(db.conn);
        let second_delta: (i64, i64) = conn.query_row(
            "SELECT input_tokens, output_tokens
             FROM proxy_request_logs
             WHERE request_id = 'codex_session:append-session:2'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(second_delta, (3, 2));
        drop(conn);

        let unchanged = sync_single_codex_file_with_stats(&db, &file)?;
        assert_eq!(
            unchanged.start_byte,
            fs::metadata(&file).unwrap().len() as i64
        );
        assert_eq!(unchanged.bytes_read, 0);
        assert_eq!(unchanged.lines_parsed, 0);
        assert_eq!(unchanged.imported, 0);

        fs::remove_dir_all(&tmp).ok();
        Ok(())
    }

    #[test]
    fn unchanged_legacy_cursor_does_not_trigger_upgrade_rescan() -> Result<(), AppError> {
        let db = Database::memory()?;
        ensure_codex_sync_state_table(&db)?;
        let tmp =
            std::env::temp_dir().join(format!("nexus-codex-sync-legacy-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let file = tmp.join("legacy.jsonl");
        let content = [
            r#"{"type":"session_meta","payload":{"session_id":"legacy"}}"#,
            r#"{"type":"event_msg","payload":{"type":"token_count","info":{"model":"glm-5.2","total_token_usage":{"input_tokens":3,"output_tokens":1}}}}"#,
        ]
        .join("\n")
            + "\n";
        fs::write(&file, content).unwrap();
        let modified = metadata_modified_nanos(&fs::metadata(&file).unwrap());
        {
            let conn = lock_conn!(db.conn);
            conn.execute(
                "INSERT INTO session_log_sync (
                    file_path, last_modified, last_line_offset, last_synced_at
                 ) VALUES (?1, ?2, 2, 0)",
                rusqlite::params![file.to_string_lossy(), modified],
            )?;
        }

        let stats = sync_single_codex_file_with_stats(&db, &file)?;
        assert_eq!(stats.bytes_read, 0);
        assert_eq!(stats.lines_parsed, 0);
        assert_eq!(stats.imported, 0);
        assert!(
            load_codex_checkpoint(&db, &file.to_string_lossy())?.is_none(),
            "unchanged legacy files should remain cold until their first append"
        );

        fs::remove_dir_all(&tmp).ok();
        Ok(())
    }

    #[test]
    fn truncated_codex_file_resets_parser_checkpoint() -> Result<(), AppError> {
        let db = Database::memory()?;
        ensure_codex_sync_state_table(&db)?;
        let tmp = std::env::temp_dir().join(format!(
            "nexus-codex-sync-truncate-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&tmp).unwrap();
        let file = tmp.join("truncate.jsonl");
        let initial = [
            r#"{"type":"session_meta","payload":{"session_id":"old-session-with-a-long-name"}}"#,
            r#"{"type":"turn_context","payload":{"model":"glm-5.2"}}"#,
            r#"{"type":"event_msg","timestamp":"2026-07-18T00:00:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":20,"output_tokens":10}}}}"#,
            r#"{"type":"irrelevant","padding":"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"}"#,
        ]
        .join("\n")
            + "\n";
        fs::write(&file, initial).unwrap();
        assert_eq!(sync_single_codex_file_with_stats(&db, &file)?.imported, 1);

        let replacement = [
            r#"{"type":"session_meta","payload":{"session_id":"new"}}"#,
            r#"{"type":"event_msg","timestamp":"2026-07-18T00:02:00Z","payload":{"type":"token_count","info":{"model":"glm-5.2","total_token_usage":{"input_tokens":4,"output_tokens":2}}}}"#,
        ]
        .join("\n")
            + "\n";
        fs::write(&file, replacement.as_bytes()).unwrap();

        let reset = sync_single_codex_file_with_stats(&db, &file)?;
        assert!(reset.reset);
        assert_eq!(reset.start_byte, 0);
        assert_eq!(reset.bytes_read, replacement.len() as u64);
        assert_eq!(reset.imported, 1);

        let conn = lock_conn!(db.conn);
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM proxy_request_logs
             WHERE request_id = 'codex_session:new:1'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(count, 1);

        fs::remove_dir_all(&tmp).ok();
        Ok(())
    }

    #[test]
    fn replaced_codex_file_with_larger_tail_resets_on_guard_mismatch() -> Result<(), AppError> {
        let db = Database::memory()?;
        ensure_codex_sync_state_table(&db)?;
        let tmp =
            std::env::temp_dir().join(format!("nexus-codex-sync-rotate-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let file = tmp.join("rotate.jsonl");
        let initial = [
            r#"{"type":"session_meta","payload":{"session_id":"old"}}"#,
            r#"{"type":"event_msg","timestamp":"2026-07-18T00:00:00Z","payload":{"type":"token_count","info":{"model":"glm-5.2","total_token_usage":{"input_tokens":2,"output_tokens":1}}}}"#,
        ]
        .join("\n")
            + "\n";
        fs::write(&file, initial).unwrap();
        assert_eq!(sync_single_codex_file_with_stats(&db, &file)?.imported, 1);

        let replacement = [
            r#"{"type":"session_meta","payload":{"session_id":"rotated-session"}}"#,
            r#"{"type":"turn_context","payload":{"model":"glm-5.2"}}"#,
            r#"{"type":"event_msg","timestamp":"2026-07-18T00:03:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":8,"output_tokens":4}}}}"#,
            r#"{"type":"irrelevant","padding":"make-the-replacement-longer-than-the-original-checkpoint"}"#,
        ]
        .join("\n")
            + "\n";
        assert!(replacement.len() > fs::metadata(&file).unwrap().len() as usize);
        fs::write(&file, replacement.as_bytes()).unwrap();

        let reset = sync_single_codex_file_with_stats(&db, &file)?;
        assert!(reset.reset);
        assert_eq!(reset.start_byte, 0);
        assert_eq!(reset.imported, 1);

        let conn = lock_conn!(db.conn);
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM proxy_request_logs
             WHERE request_id = 'codex_session:rotated-session:1'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(count, 1);

        fs::remove_dir_all(&tmp).ok();
        Ok(())
    }

    #[test]
    fn equal_size_equal_mtime_codex_replacement_resets_checkpoint() -> Result<(), AppError> {
        let db = Database::memory()?;
        ensure_codex_sync_state_table(&db)?;
        let tmp = std::env::temp_dir().join(format!(
            "nexus-codex-sync-equal-metadata-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&tmp).unwrap();
        let file = tmp.join("equal-metadata.jsonl");
        let initial = [
            r#"{"type":"session_meta","payload":{"session_id":"old-a"}}"#,
            r#"{"type":"event_msg","timestamp":"2026-07-18T00:00:00Z","payload":{"type":"token_count","info":{"model":"glm-5.2","total_token_usage":{"input_tokens":2,"output_tokens":1}}}}"#,
            r#"{"type":"irrelevant","padding":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#,
        ]
        .join("\n")
            + "\n";
        fs::write(&file, initial.as_bytes()).unwrap();
        assert_eq!(sync_single_codex_file_with_stats(&db, &file)?.imported, 1);

        let replacement = [
            r#"{"type":"session_meta","payload":{"session_id":"new-b"}}"#,
            r#"{"type":"event_msg","timestamp":"2026-07-18T00:00:00Z","payload":{"type":"token_count","info":{"model":"glm-5.2","total_token_usage":{"input_tokens":8,"output_tokens":4}}}}"#,
            r#"{"type":"irrelevant","padding":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}"#,
        ]
        .join("\n")
            + "\n";
        assert_eq!(replacement.len(), initial.len());
        fs::write(&file, replacement.as_bytes()).unwrap();
        {
            // Model a replacement whose mtime was preserved: from the syncer's
            // perspective, the current metadata exactly matches the checkpoint.
            let replacement_mtime = metadata_modified_nanos(&fs::metadata(&file).unwrap());
            let conn = lock_conn!(db.conn);
            conn.execute(
                "UPDATE codex_session_sync_state_v2
                 SET last_modified = ?1
                 WHERE file_path = ?2",
                rusqlite::params![replacement_mtime, file.to_string_lossy()],
            )?;
        }

        let reset = sync_single_codex_file_with_stats(&db, &file)?;
        assert!(reset.reset);
        assert_eq!(reset.start_byte, 0);
        assert_eq!(reset.imported, 1);

        let conn = lock_conn!(db.conn);
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM proxy_request_logs
             WHERE request_id = 'codex_session:new-b:1'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(count, 1);

        fs::remove_dir_all(&tmp).ok();
        Ok(())
    }

    #[test]
    fn equal_metadata_codex_replacement_with_shared_suffix_resets_checkpoint(
    ) -> Result<(), AppError> {
        let db = Database::memory()?;
        ensure_codex_sync_state_table(&db)?;
        let tmp = std::env::temp_dir().join(format!(
            "nexus-codex-sync-shared-suffix-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&tmp).unwrap();
        let file = tmp.join("shared-suffix.jsonl");
        let shared_suffix = r#"{"type":"irrelevant","padding":"shared-shared-shared-shared-shared-shared-shared-shared-shared-shared-shared-shared"}"#;
        let initial = [
            r#"{"type":"session_meta","payload":{"session_id":"old-c"}}"#,
            r#"{"type":"event_msg","timestamp":"2026-07-18T00:00:00Z","payload":{"type":"token_count","info":{"model":"glm-5.2","total_token_usage":{"input_tokens":3,"output_tokens":1}}}}"#,
            shared_suffix,
        ]
        .join("\n")
            + "\n";
        fs::write(&file, initial.as_bytes()).unwrap();
        assert_eq!(sync_single_codex_file_with_stats(&db, &file)?.imported, 1);
        let original_guard = load_codex_checkpoint(&db, &file.to_string_lossy())?
            .expect("checkpoint after initial shared-suffix import")
            .cursor_guard;

        let replacement = [
            r#"{"type":"session_meta","payload":{"session_id":"new-d"}}"#,
            r#"{"type":"event_msg","timestamp":"2026-07-18T00:00:00Z","payload":{"type":"token_count","info":{"model":"glm-5.2","total_token_usage":{"input_tokens":9,"output_tokens":5}}}}"#,
            shared_suffix,
        ]
        .join("\n")
            + "\n";
        assert_eq!(replacement.len(), initial.len());
        assert_eq!(
            &replacement.as_bytes()[replacement.len() - CODEX_CURSOR_GUARD_BYTES as usize..],
            &initial.as_bytes()[initial.len() - CODEX_CURSOR_GUARD_BYTES as usize..],
            "the adversarial replacement must preserve the old suffix guard"
        );
        fs::write(&file, replacement.as_bytes()).unwrap();
        {
            let replacement_mtime = metadata_modified_nanos(&fs::metadata(&file).unwrap());
            let mut replacement_file = fs::File::open(&file).unwrap();
            let replacement_guard = cursor_guard(&mut replacement_file, replacement.len() as i64)?;
            let original_digest = original_guard
                .rsplit('|')
                .next()
                .expect("versioned original guard digest");
            let replacement_parts: Vec<&str> = replacement_guard.split('|').collect();
            assert_eq!(replacement_parts.len(), 4);
            assert_ne!(
                replacement_parts[3], original_digest,
                "start/middle anchors must distinguish content with a shared suffix"
            );
            let metadata_colliding_guard = format!(
                "{}|{}|{}|{}",
                replacement_parts[0], replacement_parts[1], replacement_parts[2], original_digest
            );
            let conn = lock_conn!(db.conn);
            conn.execute(
                "UPDATE codex_session_sync_state_v2
                 SET last_modified = ?1, cursor_guard = ?2
                 WHERE file_path = ?3",
                rusqlite::params![
                    replacement_mtime,
                    metadata_colliding_guard,
                    file.to_string_lossy()
                ],
            )?;
        }

        let reset = sync_single_codex_file_with_stats(&db, &file)?;
        assert!(reset.reset);
        assert_eq!(reset.start_byte, 0);
        assert_eq!(reset.imported, 1);

        let conn = lock_conn!(db.conn);
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM proxy_request_logs
             WHERE request_id = 'codex_session:new-d:1'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(count, 1);

        fs::remove_dir_all(&tmp).ok();
        Ok(())
    }

    #[test]
    fn valid_unterminated_codex_json_is_checkpointed_once() -> Result<(), AppError> {
        let db = Database::memory()?;
        ensure_codex_sync_state_table(&db)?;
        let tmp =
            std::env::temp_dir().join(format!("nexus-codex-sync-no-eol-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let file = tmp.join("no-eol.jsonl");
        let content = [
            r#"{"type":"session_meta","payload":{"session_id":"no-eol"}}"#,
            r#"{"type":"event_msg","timestamp":"2026-07-18T00:04:00Z","payload":{"type":"token_count","info":{"model":"glm-5.2","total_token_usage":{"input_tokens":3,"output_tokens":1}}}}"#,
        ]
        .join("\n");
        fs::write(&file, content.as_bytes()).unwrap();

        let first = sync_single_codex_file_with_stats(&db, &file)?;
        assert_eq!(first.imported, 1);
        assert_eq!(first.bytes_read, content.len() as u64);
        assert_eq!(
            load_codex_checkpoint(&db, &file.to_string_lossy())?
                .expect("valid EOF record should be checkpointed")
                .byte_offset,
            content.len() as i64
        );

        let second = sync_single_codex_file_with_stats(&db, &file)?;
        assert_eq!(second.bytes_read, 0);
        assert_eq!(second.imported, 0);

        fs::remove_dir_all(&tmp).ok();
        Ok(())
    }

    #[test]
    fn incomplete_codex_tail_is_retried_after_append() -> Result<(), AppError> {
        let db = Database::memory()?;
        let tmp =
            std::env::temp_dir().join(format!("nexus-codex-sync-tail-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&tmp).unwrap();
        let file = tmp.join("partial.jsonl");
        let complete_lines = [
            r#"{"type":"session_meta","payload":{"session_id":"partial-session"}}"#,
            r#"{"type":"turn_context","payload":{"model":"glm-5.2"}}"#,
            r#"{"type":"event_msg","timestamp":"2026-07-18T00:00:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":2,"output_tokens":1}}}}"#,
        ]
        .join("\n");
        let partial = r#"{"type":"event_msg","timestamp":"2026-07-18T00:01:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":5"#;
        fs::write(&file, format!("{complete_lines}\n{partial}")).unwrap();

        assert_eq!(sync_single_codex_file(&db, &file)?.0, 1);
        assert_eq!(
            get_sync_state(&db, &file.to_string_lossy())?.1,
            3,
            "the cursor must stay before an unterminated JSON fragment"
        );

        use std::io::Write;
        let mut output = fs::OpenOptions::new().append(true).open(&file).unwrap();
        output.write_all(br#","output_tokens":2}}}}"#).unwrap();
        output.write_all(b"\n").unwrap();

        assert_eq!(sync_single_codex_file(&db, &file)?.0, 1);

        let conn = lock_conn!(db.conn);
        let second_delta: (i64, i64) = conn.query_row(
            "SELECT input_tokens, output_tokens
             FROM proxy_request_logs
             WHERE request_id = 'codex_session:partial-session:2'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(second_delta, (3, 1));

        fs::remove_dir_all(&tmp).ok();
        Ok(())
    }

    // ── 模型名归一化测试 ──

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
        // cached > input 的异常场景应被 min() 钳制
        let prev = Some(CumulativeTokens {
            input: 100,
            cached_input: 0,
            output: 50,
        });
        let current = CumulativeTokens {
            input: 110,       // delta = 10
            cached_input: 80, // delta = 80（异常：大于 input delta）
            output: 60,
        };
        let delta = compute_delta(&prev, &current);
        // 钳制前：cached_input = 80, input = 10
        assert_eq!(delta.cached_input, 80);
        assert_eq!(delta.input, 10);
        // 实际钳制在调用侧：delta.cached_input.min(delta.input)
        let clamped = delta.cached_input.min(delta.input);
        assert_eq!(clamped, 10);
    }
}
