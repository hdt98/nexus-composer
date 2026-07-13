//! Usage-statistics commands.

use crate::error::AppError;
use crate::services::usage_stats::*;
use crate::store::AppState;
use rust_decimal::Decimal;
use std::str::FromStr;
use tauri::State;

/// Get the usage summary without blocking the async command runtime on SQLite.
#[tauri::command]
pub async fn get_usage_summary(
    state: State<'_, AppState>,
    start_date: Option<i64>,
    end_date: Option<i64>,
    app_type: Option<String>,
    provider_name: Option<String>,
    model: Option<String>,
) -> Result<UsageSummary, String> {
    let db = state.inner().db.clone();
    tokio::task::spawn_blocking(move || {
        db.get_usage_summary(
            start_date,
            end_date,
            app_type.as_deref(),
            provider_name.as_deref(),
            model.as_deref(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

/// Get usage summaries grouped by `app_type`.
#[tauri::command]
pub async fn get_usage_summary_by_app(
    state: State<'_, AppState>,
    start_date: Option<i64>,
    end_date: Option<i64>,
    provider_name: Option<String>,
    model: Option<String>,
) -> Result<Vec<UsageSummaryByApp>, String> {
    let db = state.inner().db.clone();
    tokio::task::spawn_blocking(move || {
        db.get_usage_summary_by_app(
            start_date,
            end_date,
            provider_name.as_deref(),
            model.as_deref(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

/// Get daily trends.
#[tauri::command]
pub async fn get_usage_trends(
    state: State<'_, AppState>,
    start_date: Option<i64>,
    end_date: Option<i64>,
    app_type: Option<String>,
    provider_name: Option<String>,
    model: Option<String>,
) -> Result<Vec<DailyStats>, String> {
    let db = state.inner().db.clone();
    tokio::task::spawn_blocking(move || {
        db.get_daily_trends(
            start_date,
            end_date,
            app_type.as_deref(),
            provider_name.as_deref(),
            model.as_deref(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

/// Get provider statistics.
#[tauri::command]
pub async fn get_provider_stats(
    state: State<'_, AppState>,
    start_date: Option<i64>,
    end_date: Option<i64>,
    app_type: Option<String>,
    provider_name: Option<String>,
    model: Option<String>,
) -> Result<Vec<ProviderStats>, String> {
    let db = state.inner().db.clone();
    tokio::task::spawn_blocking(move || {
        db.get_provider_stats(
            start_date,
            end_date,
            app_type.as_deref(),
            provider_name.as_deref(),
            model.as_deref(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

/// Get model statistics.
#[tauri::command]
pub async fn get_model_stats(
    state: State<'_, AppState>,
    start_date: Option<i64>,
    end_date: Option<i64>,
    app_type: Option<String>,
    provider_name: Option<String>,
    model: Option<String>,
) -> Result<Vec<ModelStats>, String> {
    let db = state.inner().db.clone();
    tokio::task::spawn_blocking(move || {
        db.get_model_stats(
            start_date,
            end_date,
            app_type.as_deref(),
            provider_name.as_deref(),
            model.as_deref(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

/// Get the request-log list.
#[tauri::command]
pub fn get_request_logs(
    state: State<'_, AppState>,
    filters: LogFilters,
    page: u32,
    page_size: u32,
) -> Result<PaginatedLogs, AppError> {
    state.db.get_request_logs(&filters, page, page_size)
}

/// Get details for one request.
#[tauri::command]
pub fn get_request_detail(
    state: State<'_, AppState>,
    request_id: String,
) -> Result<Option<RequestLogDetail>, AppError> {
    state.db.get_request_detail(&request_id)
}

/// Get the model-pricing list.
#[tauri::command]
pub fn get_model_pricing(state: State<'_, AppState>) -> Result<Vec<ModelPricingInfo>, AppError> {
    log::info!("Getting model-pricing list");

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
            "The model_pricing table does not exist; restart the application to trigger database migration"
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

    log::info!("Retrieved {} model-pricing records", pricing.len());
    Ok(pricing)
}

/// Update model pricing.
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
        let mut conn = crate::database::lock_conn!(db.conn);
        let tx = conn.transaction().map_err(|e| {
            AppError::Database(format!("Failed to begin model pricing update: {e}"))
        })?;
        tx.execute(
            "DELETE FROM model_pricing_deletions WHERE model_id = ?1",
            [&model_id],
        )
        .map_err(|e| {
            AppError::Database(format!(
                "Failed to clear model pricing deletion marker: {e}"
            ))
        })?;
        tx.execute(
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
        tx.commit().map_err(|e| {
            AppError::Database(format!("Failed to commit model pricing update: {e}"))
        })?;
    }

    if let Err(e) = db.backfill_missing_usage_costs_for_model(&model_id) {
        log::warn!(
            "Failed to backfill historical usage costs after updating model pricing (model_id={model_id}): {e}"
        );
    }

    Ok(())
}

/// Check provider usage limits.
#[tauri::command]
pub fn check_provider_limits(
    state: State<'_, AppState>,
    provider_id: String,
    app_type: String,
) -> Result<crate::services::usage_stats::ProviderLimitStatus, AppError> {
    state.db.check_provider_limits(&provider_id, &app_type)
}

/// Delete model pricing.
#[tauri::command]
pub fn delete_model_pricing(state: State<'_, AppState>, model_id: String) -> Result<(), AppError> {
    state.db.delete_model_pricing_persistently(&model_id)?;

    log::info!("Deleted model pricing: {model_id}");
    Ok(())
}

/// Replace all configured model pricing with the built-in catalog.
#[tauri::command]
pub fn reset_model_pricing_to_defaults(state: State<'_, AppState>) -> Result<(), AppError> {
    state.db.reset_model_pricing_to_defaults()?;
    log::info!("Model pricing catalog reset to application defaults");
    Ok(())
}

/// Manually trigger session-log synchronization.
#[tauri::command]
pub fn sync_session_usage(
    state: State<'_, AppState>,
) -> Result<crate::services::session_usage::SessionSyncResult, AppError> {
    // Synchronize Claude session logs.
    let mut result = crate::services::session_usage::sync_claude_session_logs(&state.db)?;

    // Synchronize Codex usage data.
    match crate::services::session_usage_codex::sync_codex_usage(&state.db) {
        Ok(codex_result) => {
            result.imported += codex_result.imported;
            result.skipped += codex_result.skipped;
            result.files_scanned += codex_result.files_scanned;
            result.errors.extend(codex_result.errors);
        }
        Err(e) => {
            result
                .errors
                .push(format!("Codex synchronization failed: {e}"));
        }
    }

    // Synchronize Gemini usage data.
    match crate::services::session_usage_gemini::sync_gemini_usage(&state.db) {
        Ok(gemini_result) => {
            result.imported += gemini_result.imported;
            result.skipped += gemini_result.skipped;
            result.files_scanned += gemini_result.files_scanned;
            result.errors.extend(gemini_result.errors);
        }
        Err(e) => {
            result
                .errors
                .push(format!("Gemini synchronization failed: {e}"));
        }
    }

    // Synchronize OpenCode usage data.
    match crate::services::session_usage_opencode::sync_opencode_usage(&state.db) {
        Ok(opencode_result) => {
            result.imported += opencode_result.imported;
            result.skipped += opencode_result.skipped;
            result.files_scanned += opencode_result.files_scanned;
            result.errors.extend(opencode_result.errors);
        }
        Err(e) => {
            result
                .errors
                .push(format!("OpenCode synchronization failed: {e}"));
        }
    }

    Ok(result)
}

/// Get the data-source distribution.
#[tauri::command]
pub fn get_usage_data_sources(
    state: State<'_, AppState>,
) -> Result<Vec<crate::services::session_usage::DataSourceSummary>, AppError> {
    crate::services::session_usage::get_data_source_breakdown(&state.db)
}

/// Model-pricing information.
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
