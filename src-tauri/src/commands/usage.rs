//! Usage statistics commands

use crate::database::Database;
use crate::error::AppError;
use crate::services::usage_stats::*;
use crate::store::AppState;
use rust_decimal::Decimal;
use serde::Serialize;
use std::str::FromStr;
use tauri::State;

/// Get usage summary
#[tauri::command]
pub fn get_usage_summary(
    state: State<'_, AppState>,
    start_date: Option<i64>,
    end_date: Option<i64>,
    app_type: Option<String>,
    source: Option<String>,
    provider_name: Option<String>,
    model: Option<String>,
) -> Result<UsageSummary, AppError> {
    let _ = source; // reserved for future data_source filtering
    state.db.get_usage_summary(
        start_date,
        end_date,
        app_type.as_deref(),
        provider_name.as_deref(),
        model.as_deref(),
    )
}

/// Get usage summary split by app_type
#[tauri::command]
pub fn get_usage_summary_by_app(
    state: State<'_, AppState>,
    start_date: Option<i64>,
    end_date: Option<i64>,
    source: Option<String>,
    provider_name: Option<String>,
    model: Option<String>,
) -> Result<Vec<UsageSummaryByApp>, AppError> {
    let _ = source;
    state.db.get_usage_summary_by_app(
        start_date,
        end_date,
        provider_name.as_deref(),
        model.as_deref(),
    )
}

/// Get daily trends
#[tauri::command]
pub fn get_usage_trends(
    state: State<'_, AppState>,
    start_date: Option<i64>,
    end_date: Option<i64>,
    app_type: Option<String>,
    source: Option<String>,
    provider_name: Option<String>,
    model: Option<String>,
) -> Result<Vec<DailyStats>, AppError> {
    let _ = source;
    state.db.get_daily_trends(
        start_date,
        end_date,
        app_type.as_deref(),
        provider_name.as_deref(),
        model.as_deref(),
    )
}

/// Get provider statistics
#[tauri::command]
pub fn get_provider_stats(
    state: State<'_, AppState>,
    start_date: Option<i64>,
    end_date: Option<i64>,
    app_type: Option<String>,
    source: Option<String>,
    provider_name: Option<String>,
    model: Option<String>,
) -> Result<Vec<ProviderStats>, AppError> {
    let _ = source;
    state.db.get_provider_stats(
        start_date,
        end_date,
        app_type.as_deref(),
        provider_name.as_deref(),
        model.as_deref(),
    )
}

/// Get model statistics
#[tauri::command]
pub fn get_model_stats(
    state: State<'_, AppState>,
    start_date: Option<i64>,
    end_date: Option<i64>,
    app_type: Option<String>,
    source: Option<String>,
    provider_name: Option<String>,
    model: Option<String>,
) -> Result<Vec<ModelStats>, AppError> {
    let _ = source;
    state.db.get_model_stats(
        start_date,
        end_date,
        app_type.as_deref(),
        provider_name.as_deref(),
        model.as_deref(),
    )
}

/// Returns the request log list.
#[tauri::command]
pub fn get_request_logs(
    state: State<'_, AppState>,
    filters: LogFilters,
    page: u32,
    page_size: u32,
) -> Result<PaginatedLogs, AppError> {
    state.db.get_request_logs(&filters, page, page_size)
}

/// Returns one request's details.
#[tauri::command]
pub fn get_request_detail(
    state: State<'_, AppState>,
    request_id: String,
) -> Result<Option<RequestLogDetail>, AppError> {
    state.db.get_request_detail(&request_id)
}

/// Returns the model pricing list.
#[tauri::command]
pub fn get_model_pricing(state: State<'_, AppState>) -> Result<Vec<ModelPricingInfo>, AppError> {
    log::info!("Fetching model pricing list");
    state.db.ensure_model_pricing_seeded()?;

    let db = state.db.clone();
    let conn = crate::database::lock_conn!(db.conn);

    // Check whether the table exists.
    let table_exists: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='model_pricing'",
            [],
            |row| row.get::<_, i64>(0).map(|count| count > 0),
        )
        .unwrap_or(false);

    if !table_exists {
        log::error!(
            "The model_pricing table is missing; restart the application to run database migrations"
        );
        return Ok(Vec::new());
    }

    let mut stmt = conn.prepare(
        "SELECT model_id, display_name, input_cost_per_million, output_cost_per_million,
                cache_read_cost_per_million, cache_creation_cost_per_million
         FROM model_pricing
         ORDER BY display_name",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(ModelPricingInfo {
            model_id: row.get(0)?,
            display_name: row.get(1)?,
            input_cost_per_million: row.get(2)?,
            output_cost_per_million: row.get(3)?,
            cache_read_cost_per_million: row.get(4)?,
            cache_creation_cost_per_million: row.get(5)?,
        })
    })?;

    let mut pricing = Vec::new();
    for row in rows {
        pricing.push(row?);
    }

    log::info!("Fetched {} model pricing records", pricing.len());
    Ok(pricing)
}

/// Updates model pricing.
#[tauri::command]
pub fn update_model_pricing(
    state: State<'_, AppState>,
    model_id: String,
    display_name: String,
    input_cost: String,
    output_cost: String,
    cache_read_cost: String,
    cache_creation_cost: String,
) -> Result<(), AppError> {
    let db = state.db.clone();
    let model_id = model_id.trim().to_string();
    let display_name = display_name.trim().to_string();
    if model_id.is_empty() {
        return Err(AppError::localized(
            "usage.modelIdRequired",
            "Model ID is required",
            "Model ID is required",
        ));
    }
    if display_name.is_empty() {
        return Err(AppError::localized(
            "usage.displayNameRequired",
            "Display name is required",
            "Display name is required",
        ));
    }

    for (label, value) in [
        ("input_cost", &input_cost),
        ("output_cost", &output_cost),
        ("cache_read_cost", &cache_read_cost),
        ("cache_creation_cost", &cache_creation_cost),
    ] {
        let parsed = Decimal::from_str(value.trim()).map_err(|e| {
            AppError::localized(
                "usage.invalidPrice",
                format!("{label} price is invalid: {value} - {e}"),
                format!("{label} price is invalid: {value} - {e}"),
            )
        })?;
        if parsed < Decimal::ZERO {
            return Err(AppError::localized(
                "usage.invalidPrice",
                format!("{label} price must be non-negative: {value}"),
                format!("{label} price must be non-negative: {value}"),
            ));
        }
    }

    {
        let conn = crate::database::lock_conn!(db.conn);
        conn.execute(
            "INSERT OR REPLACE INTO model_pricing (
                model_id, display_name, input_cost_per_million, output_cost_per_million,
                cache_read_cost_per_million, cache_creation_cost_per_million
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                model_id,
                display_name,
                input_cost.trim(),
                output_cost.trim(),
                cache_read_cost.trim(),
                cache_creation_cost.trim()
            ],
        )
        .map_err(|e| AppError::Database(format!("Failed to update model pricing: {e}")))?;
    }

    if let Err(e) = db.backfill_missing_usage_costs_for_model(&model_id) {
        log::warn!(
            "Failed to backfill historical usage costs after updating model pricing (model_id={model_id}): {e}"
        );
    }

    Ok(())
}

/// Checks provider usage limits.
#[tauri::command]
pub fn check_provider_limits(
    state: State<'_, AppState>,
    provider_id: String,
    app_type: String,
) -> Result<crate::services::usage_stats::ProviderLimitStatus, AppError> {
    state.db.check_provider_limits(&provider_id, &app_type)
}

/// Deletes model pricing.
#[tauri::command]
pub fn delete_model_pricing(state: State<'_, AppState>, model_id: String) -> Result<(), AppError> {
    let db = state.db.clone();
    let conn = crate::database::lock_conn!(db.conn);

    conn.execute(
        "DELETE FROM model_pricing WHERE model_id = ?1",
        rusqlite::params![model_id],
    )
    .map_err(|e| AppError::Database(format!("Failed to delete model pricing: {e}")))?;

    log::info!("Deleted model pricing: {model_id}");
    Ok(())
}

/// Manually synchronize session logs.
#[tauri::command]
pub async fn sync_session_usage(
    state: State<'_, AppState>,
) -> Result<crate::services::session_usage::SessionSyncResult, AppError> {
    crate::services::session_usage::sync_all_session_usage(state.db.clone()).await
}

/// Returns the usage data-source distribution.
#[tauri::command]
pub fn get_usage_data_sources(
    state: State<'_, AppState>,
) -> Result<Vec<crate::services::session_usage::DataSourceSummary>, AppError> {
    crate::services::session_usage::get_data_source_breakdown(&state.db)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteRemoteUsageDataResult {
    pub data_source: String,
    pub deleted_request_logs: usize,
    pub deleted_rollups: usize,
    pub deleted_sync_states: usize,
}

/// Deletes locally stored usage data for one remote source.
#[tauri::command]
pub fn delete_remote_usage_data(
    state: State<'_, AppState>,
    data_source: String,
) -> Result<DeleteRemoteUsageDataResult, AppError> {
    delete_remote_usage_data_for_source(&state.db, data_source)
}

fn delete_remote_usage_data_for_source(
    db: &Database,
    data_source: String,
) -> Result<DeleteRemoteUsageDataResult, AppError> {
    let data_source = data_source.trim().to_string();
    let host_alias = data_source
        .strip_prefix("remote:")
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .ok_or_else(|| {
            AppError::localized(
                "usage.sourceFilter.deleteInvalidSource",
                "Only remote usage data can be deleted",
                "Only remote usage data can be deleted",
            )
        })?;

    let sync_prefix = format!("remote://{host_alias}/");
    let sync_prefix_len = sync_prefix.len() as i64;
    let conn = crate::database::lock_conn!(db.conn);

    let deleted_request_logs = conn
        .execute(
            "DELETE FROM proxy_request_logs WHERE COALESCE(data_source, 'proxy') = ?1",
            rusqlite::params![data_source],
        )
        .map_err(|e| AppError::Database(format!("Failed to delete remote usage details: {e}")))?;

    let deleted_rollups = conn
        .execute(
            "DELETE FROM usage_daily_rollups WHERE data_source = ?1",
            rusqlite::params![data_source],
        )
        .map_err(|e| AppError::Database(format!("Failed to delete remote usage rollups: {e}")))?;

    let deleted_sync_states = conn
        .execute(
            "DELETE FROM session_log_sync WHERE substr(file_path, 1, ?1) = ?2",
            rusqlite::params![sync_prefix_len, sync_prefix],
        )
        .map_err(|e| AppError::Database(format!("Failed to delete remote sync state: {e}")))?;

    Ok(DeleteRemoteUsageDataResult {
        data_source,
        deleted_request_logs,
        deleted_rollups,
        deleted_sync_states,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::lock_conn;

    #[test]
    fn delete_remote_usage_data_removes_only_selected_host() -> Result<(), AppError> {
        let db = Database::memory()?;
        {
            let conn = lock_conn!(db.conn);
            for (request_id, source) in [
                ("remote-pjlab-1", "remote:pjlab"),
                ("remote-jd-1", "remote:jd"),
            ] {
                conn.execute(
                    "INSERT INTO proxy_request_logs (
                        request_id, provider_id, app_type, model,
                        latency_ms, status_code, created_at, data_source
                    ) VALUES (?1, ?2, 'codex', 'gpt-5.4', 0, 200, 1000, ?3)",
                    rusqlite::params![request_id, "_remote:codex:test", source],
                )?;
            }
            for source in ["remote:pjlab", "remote:jd"] {
                conn.execute(
                    "INSERT INTO usage_daily_rollups (
                        date, app_type, provider_id, data_source, model
                    ) VALUES ('2026-05-22', 'codex', '_remote:codex:test', ?1, 'gpt-5.4')",
                    rusqlite::params![source],
                )?;
            }
            for path in [
                "remote://pjlab/codex/session.jsonl",
                "remote://jd/codex/session.jsonl",
            ] {
                conn.execute(
                    "INSERT INTO session_log_sync (
                        file_path, last_modified, last_line_offset, last_synced_at
                    ) VALUES (?1, 1, 2, 3)",
                    rusqlite::params![path],
                )?;
            }
        }

        let result = delete_remote_usage_data_for_source(&db, "remote:pjlab".to_string())?;
        assert_eq!(result.deleted_request_logs, 1);
        assert_eq!(result.deleted_rollups, 1);
        assert_eq!(result.deleted_sync_states, 1);

        let conn = lock_conn!(db.conn);
        let pjlab_logs: i64 = conn.query_row(
            "SELECT COUNT(*) FROM proxy_request_logs WHERE data_source = 'remote:pjlab'",
            [],
            |row| row.get(0),
        )?;
        let jd_logs: i64 = conn.query_row(
            "SELECT COUNT(*) FROM proxy_request_logs WHERE data_source = 'remote:jd'",
            [],
            |row| row.get(0),
        )?;
        let pjlab_sync: i64 = conn.query_row(
            "SELECT COUNT(*) FROM session_log_sync WHERE file_path LIKE 'remote://pjlab/%'",
            [],
            |row| row.get(0),
        )?;
        let jd_sync: i64 = conn.query_row(
            "SELECT COUNT(*) FROM session_log_sync WHERE file_path LIKE 'remote://jd/%'",
            [],
            |row| row.get(0),
        )?;

        assert_eq!(pjlab_logs, 0);
        assert_eq!(jd_logs, 1);
        assert_eq!(pjlab_sync, 0);
        assert_eq!(jd_sync, 1);
        Ok(())
    }
}

/// Model pricing information.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelPricingInfo {
    pub model_id: String,
    pub display_name: String,
    pub input_cost_per_million: String,
    pub output_cost_per_million: String,
    pub cache_read_cost_per_million: String,
    pub cache_creation_cost_per_million: String,
}
