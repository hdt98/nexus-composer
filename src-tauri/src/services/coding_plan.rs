//! Token Plan quota query service for supported domestic providers.
//!
//! Supports Token Plan quota queries for Kimi For Coding, Zhipu GLM, and MiniMax.
//! Reuses the `SubscriptionQuota` and `QuotaTier` types from the subscription module.

use super::subscription::{
    CredentialStatus, QuotaTier, SubscriptionQuota, TIER_FIVE_HOUR, TIER_MONTHLY, TIER_WEEKLY_LIMIT,
};
use std::time::{SystemTime, UNIX_EPOCH};

// ── Provider detection ─────────────────────────────────────

enum CodingPlanProvider {
    Kimi,
    ZhipuCn,
    ZhipuEn,
    MiniMaxCn,
    MiniMaxEn,
    ZenMux,
    /// Volcengine Ark Agent Plan / Coding Plan (`base_url` looks like
    /// `https://ark.cn-beijing.volces.com/api/coding[/v3]`).
    Volcengine,
}

fn detect_provider(base_url: &str) -> Option<CodingPlanProvider> {
    let url = base_url.to_lowercase();
    if url.contains("api.kimi.com/coding") {
        Some(CodingPlanProvider::Kimi)
    } else if url.contains("open.bigmodel.cn") || url.contains("bigmodel.cn") {
        Some(CodingPlanProvider::ZhipuCn)
    } else if url.contains("api.z.ai") {
        Some(CodingPlanProvider::ZhipuEn)
    } else if url.contains("api.minimaxi.com") {
        Some(CodingPlanProvider::MiniMaxCn)
    } else if url.contains("api.minimax.io") {
        Some(CodingPlanProvider::MiniMaxEn)
    } else if url.contains("zenmux") {
        Some(CodingPlanProvider::ZenMux)
    } else if url.contains("volces.com/api/coding") {
        // Only match the Coding/Agent Plan entrypoint. DouBaoSeed pay-as-you-go uses
        // `/api/v3` and `/api/compatible`, has no plan quota, and should not match here.
        Some(CodingPlanProvider::Volcengine)
    } else {
        None
    }
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn millis_to_iso8601(ms: i64) -> Option<String> {
    let secs = ms / 1000;
    let nsecs = ((ms % 1000) * 1_000_000) as u32;
    chrono::DateTime::from_timestamp(secs, nsecs).map(|dt| dt.to_rfc3339())
}

/// Extract a reset time from a JSON value, accepting both string and number forms.
/// - String: return as-is (ISO 8601)
/// - Number: auto-detect seconds vs milliseconds and convert to ISO 8601
fn extract_reset_time(value: &serde_json::Value) -> Option<String> {
    if let Some(s) = value.as_str() {
        return Some(s.to_string());
    }
    if let Some(n) = value.as_i64() {
        // Treat zero or negative timestamps as no reset time, for example when a
        // Volcengine session has no active window and returns `-1`.
        if n <= 0 {
            return None;
        }
        // Distinguish seconds from milliseconds: second timestamps are < 1e12,
        // millisecond timestamps are >= 1e12.
        let ms = if n < 1_000_000_000_000 { n * 1000 } else { n };
        return millis_to_iso8601(ms);
    }
    None
}

/// Parse a JSON value as `f64`, accepting both numeric and string forms such as
/// `100` and `"100"`.
fn parse_f64(value: &serde_json::Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

fn make_error(msg: String) -> SubscriptionQuota {
    SubscriptionQuota {
        tool: "coding_plan".to_string(),
        credential_status: CredentialStatus::Valid,
        credential_message: None,
        success: false,
        tiers: vec![],
        extra_usage: None,
        error: Some(msg),
        queried_at: Some(now_millis()),
    }
}

// ── Kimi For Coding ─────────────────────────────────────────

async fn query_kimi(api_key: &str) -> SubscriptionQuota {
    let client = crate::proxy::http_client::get();

    let resp = client
        .get("https://api.kimi.com/coding/v1/usages")
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Accept", "application/json")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await;

    let resp = match resp {
        Ok(r) => r,
        Err(e) => return make_error(format!("Network error: {e}")),
    };

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return SubscriptionQuota {
            tool: "coding_plan".to_string(),
            credential_status: CredentialStatus::Expired,
            credential_message: Some("Invalid API key".to_string()),
            success: false,
            tiers: vec![],
            extra_usage: None,
            error: Some(format!("Authentication failed (HTTP {status})")),
            queried_at: Some(now_millis()),
        };
    }

    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return make_error(format!("API error (HTTP {status}): {body}"));
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return make_error(format!("Failed to parse response: {e}")),
    };

    let mut tiers = Vec::new();

    // 5-hour window quota, shown first.
    if let Some(limits) = body.get("limits").and_then(|v| v.as_array()) {
        for limit_item in limits {
            if let Some(detail) = limit_item.get("detail") {
                let limit = detail.get("limit").and_then(parse_f64).unwrap_or(1.0);
                let remaining = detail.get("remaining").and_then(parse_f64).unwrap_or(0.0);
                let resets_at = detail.get("resetTime").and_then(extract_reset_time);

                let used = (limit - remaining).max(0.0);
                let utilization = if limit > 0.0 {
                    (used / limit) * 100.0
                } else {
                    0.0
                };
                tiers.push(QuotaTier {
                    name: "five_hour".to_string(),
                    utilization,
                    resets_at,
                    used_value_usd: None,
                    max_value_usd: None,
                });
            }
        }
    }

    // Overall usage (weekly quota).
    if let Some(usage) = body.get("usage") {
        let limit = usage.get("limit").and_then(parse_f64).unwrap_or(1.0);
        let remaining = usage.get("remaining").and_then(parse_f64).unwrap_or(0.0);
        let resets_at = usage.get("resetTime").and_then(extract_reset_time);

        let used = (limit - remaining).max(0.0);
        let utilization = if limit > 0.0 {
            (used / limit) * 100.0
        } else {
            0.0
        };
        tiers.push(QuotaTier {
            name: "weekly_limit".to_string(),
            utilization,
            resets_at,
            used_value_usd: None,
            max_value_usd: None,
        });
    }

    SubscriptionQuota {
        tool: "coding_plan".to_string(),
        credential_status: CredentialStatus::Valid,
        credential_message: None,
        success: true,
        tiers,
        extra_usage: None,
        error: None,
        queried_at: Some(now_millis()),
    }
}

// ── Zhipu GLM ───────────────────────────────────────────────

/// Classify Zhipu `TOKENS_LIMIT` entries by the explicit window encoded in `unit`.
enum ZhipuWindow {
    FiveHour,
    Weekly,
}

/// Determine which window a `TOKENS_LIMIT` entry belongs to from its `unit` field.
///
/// Observed shapes in production (`bigmodel.cn` and `z.ai` share the same backend
/// and field layout):
/// - `unit: 3, number: 5` => rolling 5-hour window (present in both old and new plans)
/// - `unit: 6, number: 7` and `unit: 6, number: 1` => weekly window (both values
///   have been observed, so classification keys only on `unit`, not `number`)
///
/// Return `None` when `unit` is missing or unknown so the caller can fall back to
/// reset-time heuristics.
fn classify_zhipu_window(item: &serde_json::Value) -> Option<ZhipuWindow> {
    match item.get("unit").and_then(|v| v.as_i64()) {
        Some(3) => Some(ZhipuWindow::FiveHour),
        Some(6) => Some(ZhipuWindow::Weekly),
        _ => None,
    }
}

/// Parse Zhipu `limits[]` from `data` into a tier list.
///
/// Classification priority:
/// 1. Explicit field: `unit` identifies the window type (see
///    [`classify_zhipu_window`]). This cannot be replaced by sorting on
///    `nextResetTime`: near the end of a weekly cycle, the weekly window can reset
///    sooner than the 5-hour window (issue #3036), so time ordering would invert
///    the buckets in that scenario.
/// 2. Fallback heuristic (when `unit` is missing or unknown): entries without
///    `nextResetTime` are assigned to `five_hour` first, because the 5-hour bucket
///    may have no reset when usage is 0%. The remaining entries are assigned in
///    ascending reset order to the still-empty slots.
///
/// Old plans subscribed before February 12, 2026 return only one `TOKENS_LIMIT`,
/// so this naturally degrades to showing only `five_hour`; new plans return two.
fn parse_zhipu_token_tiers(data: &serde_json::Value) -> Vec<QuotaTier> {
    type Entry = (Option<i64>, f64, Option<String>);
    let mut five_hour: Option<Entry> = None;
    let mut weekly: Option<Entry> = None;
    let mut unclassified: Vec<Entry> = Vec::new();

    if let Some(limits) = data.get("limits").and_then(|v| v.as_array()) {
        for limit_item in limits {
            let limit_type = limit_item
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            // Case-insensitive comparison keeps working if upstream changes
            // "TOKENS_LIMIT" to lowercase or mixed case.
            if !limit_type.eq_ignore_ascii_case("TOKENS_LIMIT") {
                continue;
            }
            let percentage = limit_item
                .get("percentage")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let reset_ms = limit_item.get("nextResetTime").and_then(|v| v.as_i64());
            let reset_iso = reset_ms.and_then(millis_to_iso8601);
            let entry = (reset_ms, percentage, reset_iso);
            match classify_zhipu_window(limit_item) {
                Some(ZhipuWindow::FiveHour) if five_hour.is_none() => five_hour = Some(entry),
                Some(ZhipuWindow::Weekly) if weekly.is_none() => weekly = Some(entry),
                _ => unclassified.push(entry),
            }
        }
    }

    unclassified.sort_by_key(|(reset, _, _)| (reset.is_some(), reset.unwrap_or(i64::MIN)));
    for entry in unclassified {
        if five_hour.is_none() {
            five_hour = Some(entry);
        } else if weekly.is_none() {
            weekly = Some(entry);
        }
        // Zhipu currently returns at most two `TOKENS_LIMIT` entries. Ignore extras.
    }

    let mut tiers = Vec::new();
    for (name, slot) in [(TIER_FIVE_HOUR, five_hour), (TIER_WEEKLY_LIMIT, weekly)] {
        if let Some((_, percentage, resets_at)) = slot {
            tiers.push(QuotaTier {
                name: name.to_string(),
                utilization: percentage,
                resets_at,
                used_value_usd: None,
                max_value_usd: None,
            });
        }
    }
    tiers
}

/// Resolve the Zhipu quota endpoint from the user's configured `base_url`.
///
/// Zhipu ships as two distinct presets (Zhipu GLM = `open.bigmodel.cn`,
/// Zhipu GLM en = `api.z.ai`) that share the same quota path and JSON shape.
/// The quota endpoint lives on the same host as the user's coding endpoint,
/// so we route by `base_url` and let the caller's existing reachability
/// (they're already using this host to run coding) determine success — no
/// cross-host fallback, no auth-error heuristics.
fn zhipu_quota_base(base_url: &str) -> &'static str {
    if base_url.to_lowercase().contains("bigmodel.cn") {
        "https://open.bigmodel.cn"
    } else {
        "https://api.z.ai"
    }
}

async fn query_zhipu(base_url: &str, api_key: &str) -> SubscriptionQuota {
    let client = crate::proxy::http_client::get();
    let url = format!(
        "{}/api/monitor/usage/quota/limit",
        zhipu_quota_base(base_url)
    );

    let resp = client
        .get(&url)
        .header("Authorization", api_key) // Zhipu expects the raw key, without `Bearer`.
        .header("Content-Type", "application/json")
        .header("Accept-Language", "en-US,en")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await;

    let resp = match resp {
        Ok(r) => r,
        Err(e) => return make_error(format!("Network error: {e}")),
    };

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return SubscriptionQuota {
            tool: "coding_plan".to_string(),
            credential_status: CredentialStatus::Expired,
            credential_message: Some("Invalid API key".to_string()),
            success: false,
            tiers: vec![],
            extra_usage: None,
            error: Some(format!("Authentication failed (HTTP {status})")),
            queried_at: Some(now_millis()),
        };
    }

    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return make_error(format!("API error (HTTP {status}): {body}"));
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return make_error(format!("Failed to parse response: {e}")),
    };

    // Check for API-level business errors.
    if body.get("success").and_then(|v| v.as_bool()) == Some(false) {
        let msg = body
            .get("msg")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown error");
        return make_error(format!("API error: {msg}"));
    }

    let data = match body.get("data") {
        Some(d) => d,
        None => return make_error("Missing 'data' field in response".to_string()),
    };

    let tiers = parse_zhipu_token_tiers(data);

    // Store the plan level in `credential_message`.
    let level = data
        .get("level")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    SubscriptionQuota {
        tool: "coding_plan".to_string(),
        credential_status: CredentialStatus::Valid,
        credential_message: level,
        success: true,
        tiers,
        extra_usage: None,
        error: None,
        queried_at: Some(now_millis()),
    }
}

// ── MiniMax ─────────────────────────────────────────────────

async fn query_minimax(api_key: &str, is_cn: bool) -> SubscriptionQuota {
    let client = crate::proxy::http_client::get();

    let api_domain = if is_cn {
        "api.minimaxi.com"
    } else {
        "api.minimax.io"
    };
    let url = format!("https://{api_domain}/v1/api/openplatform/coding_plan/remains");

    let resp = client
        .get(&url)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await;

    let resp = match resp {
        Ok(r) => r,
        Err(e) => return make_error(format!("Network error: {e}")),
    };

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return SubscriptionQuota {
            tool: "coding_plan".to_string(),
            credential_status: CredentialStatus::Expired,
            credential_message: Some("Invalid API key".to_string()),
            success: false,
            tiers: vec![],
            extra_usage: None,
            error: Some(format!("Authentication failed (HTTP {status})")),
            queried_at: Some(now_millis()),
        };
    }

    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return make_error(format!("API error (HTTP {status}): {body}"));
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return make_error(format!("Failed to parse response: {e}")),
    };

    // Check for API-level business errors.
    if let Some(base_resp) = body.get("base_resp") {
        let status_code = base_resp
            .get("status_code")
            .and_then(|v| v.as_i64())
            .unwrap_or(-1);
        if status_code != 0 {
            let msg = base_resp
                .get("status_msg")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown error");
            return make_error(format!("API error (code {status_code}): {msg}"));
        }
    }

    // Keep extraction in a pure function so unit tests do not need mocks. The new
    // API returns remaining percent directly, so invert it to used percent.
    let tiers = parse_minimax_tiers(&body);

    SubscriptionQuota {
        tool: "coding_plan".to_string(),
        credential_status: CredentialStatus::Valid,
        credential_message: None,
        success: true,
        tiers,
        extra_usage: None,
        error: None,
        queried_at: Some(now_millis()),
    }
}

// ── ZenMux ──────────────────────────────────────────────────

async fn query_zenmux(base_url: &str, api_key: &str) -> SubscriptionQuota {
    let client = crate::proxy::http_client::get();

    let resp = client
        .get(base_url)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Accept", "application/json")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await;

    let resp = match resp {
        Ok(r) => r,
        Err(e) => return make_error(format!("Network error: {e}")),
    };

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return SubscriptionQuota {
            tool: "coding_plan".to_string(),
            credential_status: CredentialStatus::Expired,
            credential_message: Some("Invalid API key".to_string()),
            success: false,
            tiers: vec![],
            extra_usage: None,
            error: Some(format!("Authentication failed (HTTP {status})")),
            queried_at: Some(now_millis()),
        };
    }

    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return make_error(format!("API error (HTTP {status}): {body}"));
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return make_error(format!("Failed to parse response: {e}")),
    };

    // Check for API-level business errors.
    if body.get("success").and_then(|v| v.as_bool()) != Some(true) {
        let msg = body
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown error");
        return make_error(format!("API error: {msg}"));
    }

    let data = match body.get("data") {
        Some(d) => d,
        None => return make_error("Missing 'data' field in response".to_string()),
    };

    let mut tiers = Vec::new();

    // 5-hour window quota.
    if let Some(q5h) = data.get("quota_5_hour") {
        let usage_pct = q5h
            .get("usage_percentage")
            .and_then(parse_f64)
            .unwrap_or(0.0);
        let resets_at = q5h
            .get("resets_at")
            .and_then(|v| v.as_str())
            .map(String::from);
        let used_usd = q5h.get("used_value_usd").and_then(parse_f64);
        let max_usd = q5h.get("max_value_usd").and_then(parse_f64);
        tiers.push(QuotaTier {
            name: "five_hour".to_string(),
            utilization: usage_pct * 100.0,
            resets_at,
            used_value_usd: used_usd,
            max_value_usd: max_usd,
        });
    }

    // 7-day window quota.
    if let Some(q7d) = data.get("quota_7_day") {
        let usage_pct = q7d
            .get("usage_percentage")
            .and_then(parse_f64)
            .unwrap_or(0.0);
        let resets_at = q7d
            .get("resets_at")
            .and_then(|v| v.as_str())
            .map(String::from);
        let used_usd = q7d.get("used_value_usd").and_then(parse_f64);
        let max_usd = q7d.get("max_value_usd").and_then(parse_f64);
        tiers.push(QuotaTier {
            name: "weekly_limit".to_string(),
            utilization: usage_pct * 100.0,
            resets_at,
            used_value_usd: used_usd,
            max_value_usd: max_usd,
        });
    }

    // Store the plan tier and account status in `credential_message`.
    let plan_tier = data
        .get("plan")
        .and_then(|p| p.get("tier"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let account_status = data
        .get("account_status")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let plan_info = if !plan_tier.is_empty() {
        format!("{plan_tier} ({account_status})")
    } else {
        String::new()
    };

    SubscriptionQuota {
        tool: "coding_plan".to_string(),
        credential_status: CredentialStatus::Valid,
        credential_message: if plan_info.is_empty() {
            None
        } else {
            Some(plan_info)
        },
        success: true,
        tiers,
        extra_usage: None,
        error: None,
        queried_at: Some(now_millis()),
    }
}

/// Parse MiniMax coding-plan quota tiers from the `/coding_plan/remains` response.
///
/// In the new API, `current_*_remaining_percent` means remaining percent (0-100).
/// The `model_remains` array includes `general` (the coding plan) and other models
/// such as `video`; only `general` is used here.
///
/// The 5-hour bucket always exists. The weekly bucket is not present on every plan,
/// so it is considered active only when `current_weekly_status == 1`. Plans without
/// a weekly quota report `3` there and keep `remaining_percent` at 100, so the
/// weekly tier should not be shown.
fn parse_minimax_tiers(body: &serde_json::Value) -> Vec<QuotaTier> {
    let mut tiers = Vec::new();

    let Some(model_remains) = body.get("model_remains").and_then(|v| v.as_array()) else {
        return tiers;
    };

    // Only use the `model_name == "general"` entry and skip non-coding models such
    // as `video`.
    let Some(item) = model_remains.iter().find(|item| {
        item.get("model_name")
            .and_then(|v| v.as_str())
            .map(|s| s == "general")
            .unwrap_or(false)
    }) else {
        return tiers;
    };

    // 5-hour bucket: remaining percent => used percent.
    if let Some(remain_pct) = item
        .get("current_interval_remaining_percent")
        .and_then(|v| v.as_f64())
    {
        let resets_at = item
            .get("end_time")
            .and_then(|v| v.as_i64())
            .and_then(millis_to_iso8601);
        tiers.push(QuotaTier {
            name: TIER_FIVE_HOUR.to_string(),
            utilization: 100.0 - remain_pct,
            resets_at,
            used_value_usd: None,
            max_value_usd: None,
        });
    }

    // Weekly bucket: active only when `status == 1`; values such as `status == 3`
    // indicate that the plan has no weekly quota, so skip it.
    if item.get("current_weekly_status").and_then(|v| v.as_i64()) == Some(1) {
        if let Some(remain_pct) = item
            .get("current_weekly_remaining_percent")
            .and_then(|v| v.as_f64())
        {
            let resets_at = item
                .get("weekly_end_time")
                .and_then(|v| v.as_i64())
                .and_then(millis_to_iso8601);
            tiers.push(QuotaTier {
                name: TIER_WEEKLY_LIMIT.to_string(),
                utilization: 100.0 - remain_pct,
                resets_at,
                used_value_usd: None,
                max_value_usd: None,
            });
        }
    }

    tiers
}

// ── Volcengine Ark Agent Plan / Coding Plan ─────────────────
//
// Unlike Kimi and MiniMax, which expose data-plane Bearer quota endpoints,
// Volcengine usage queries use a control-plane OpenAPI through the unified
// gateway `open.volcengineapi.com`, not the data-plane inference host
// `ark.cn-beijing.volces.com`, for example:
// `POST https://open.volcengineapi.com/?Action=...&Version=2024-01-01&Region=cn-beijing`.
// This endpoint requires Volcengine Signature V4 with AK/SK. Reusing the
// inference Bearer key has been observed to fail with `400 InvalidAuthorization`,
// which is a format-layer rejection rather than a permission error. Users
// therefore need to provide the account AccessKey ID and Secret separately for
// usage queries. Both plans share the same AK/SK, so auth-class errors stop
// probing immediately instead of trying the other plan.
//
// Auto-detection tries `GetAFPUsage` first (Agent Plan, returns absolute
// `Quota`/`Used` values), then falls back to `GetCodingPlanUsage` when the
// account is not subscribed.

/// Unified control-plane OpenAPI gateway, distinct from the data-plane inference
/// host `ark.cn-beijing.volces.com`.
const VOLCENGINE_OPENAPI_HOST: &str = "open.volcengineapi.com";
const VOLCENGINE_API_VERSION: &str = "2024-01-01";
/// Default Region for the Ark control-plane OpenAPI. Agent and Coding Plan are
/// currently hosted in `cn-beijing`.
const VOLCENGINE_DEFAULT_REGION: &str = "cn-beijing";

/// Classified result of a single OpenAPI call.
enum VolcCall {
    /// 2xx, JSON parses successfully, and there is no OpenAPI-level error.
    /// The business `Result` may still be empty when not subscribed.
    Body(serde_json::Value),
    /// Hard authentication failure such as HTTP 401/403 or AccessDenied/Signature
    /// error codes. Both plans share credentials, so stop immediately.
    Auth(String),
    /// Network error, non-auth HTTP error, or parse failure. Record it and allow
    /// trying the other plan.
    Soft(String),
}

/// Extract the Region required by the control-plane OpenAPI from the data-plane
/// `base_url`, for example `ark.cn-beijing.volces.com` => `cn-beijing`.
/// Fall back to `cn-beijing` when no recognizable region is present. The
/// control-plane host is fixed (`VOLCENGINE_OPENAPI_HOST`) and does not vary
/// with `base_url`.
fn volcengine_region(base_url: &str) -> String {
    let host = base_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(base_url)
        .split('/')
        .next()
        .unwrap_or("");
    host.split('.')
        .find(|p| p.starts_with("cn-") || p.starts_with("ap-"))
        .map(|p| p.to_string())
        .unwrap_or_else(|| VOLCENGINE_DEFAULT_REGION.to_string())
}

/// Return whether an OpenAPI error code should be treated as an auth error that
/// requires a hard stop and AK/SK guidance.
fn volcengine_is_auth_error_code(code: &str) -> bool {
    let c = code.to_lowercase();
    c.contains("auth")
        || c.contains("signature")
        || c.contains("accessdenied")
        || c.contains("denied")
        || c.contains("unauthorized")
        || c.contains("forbidden")
        || c.contains("credential")
        || c.contains("token")
}

/// Extract `ResponseMetadata.Error` or top-level `Error` from a Volcengine
/// OpenAPI response.
fn volcengine_response_error(body: &serde_json::Value) -> Option<(String, String)> {
    let err = body
        .get("ResponseMetadata")
        .and_then(|m| m.get("Error"))
        .or_else(|| body.get("Error"))?;
    let code = err
        .get("Code")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let msg = err
        .get("Message")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if code.is_empty() && msg.is_empty() {
        None
    } else {
        Some((code, msg))
    }
}

/// Guidance appended after authentication failures.
const VOLCENGINE_AKSK_HINT: &str =
    "Check the AccessKey ID / Secret are correct and the account has Ark usage-query (OpenAPI) permission.";

// ── Volcengine Signature V4 (AK/SK) ─────────────────────────
//
// The algorithm is a Volcengine-specific SigV4 variant, matching the official
// `volc-openapi-demos/signature/java/Sign.java`.
// Two differences are critical enough that copying the standard SigV4 logic from
// `s3.rs` will break signing:
//   1. canonical headers and `SignedHeaders` use the fixed order
//      `host;x-date;x-content-sha256;content-type` rather than alphabetical order;
//   2. the algorithm string is `HMAC-SHA256` with no `AWS4` prefix, the credential
//      scope ends in `request` rather than `aws4_request`, and the signing key uses
//      `kDate=HMAC(SK, date)` with no `AWS4` prefix on the secret.
// The canonical query is still sorted alphabetically by key, as in standard SigV4.
// Service is `ark`, the method is POST, and the body is empty.

const VOLCENGINE_SERVICE: &str = "ark";
const VOLCENGINE_CONTENT_TYPE: &str = "application/json; charset=utf-8";
const VOLCENGINE_SIGNED_HEADERS: &str = "host;x-date;x-content-sha256;content-type";

fn volc_hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    type HmacSha256 = Hmac<sha2::Sha256>;
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn volc_sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(data))
}

/// Percent-encode everything outside the RFC3986 unreserved set for use in the
/// canonical query string.
fn volc_uri_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => {
                use std::fmt::Write;
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// Build the canonical query string by sorting keys alphabetically and URL-encoding
/// each segment. The exact same string is used for both signing and the final
/// request URL so they stay byte-for-byte identical.
fn volcengine_canonical_query(action: &str, region: &str) -> String {
    let mut pairs = [
        ("Action", action),
        ("Region", region),
        ("Version", VOLCENGINE_API_VERSION),
    ];
    pairs.sort_by(|a, b| a.0.cmp(b.0));
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", volc_uri_encode(k), volc_uri_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Generate the Volcengine Signature V4 auth headers and return
/// `(Authorization, X-Date, X-Content-Sha256)`. All three must be sent with the
/// request. `canonical_query` must exactly match the query string used in the
/// actual request URL. `now` is injected for deterministic unit tests.
fn volcengine_sign(
    access_key_id: &str,
    secret_access_key: &str,
    region: &str,
    canonical_query: &str,
    body: &[u8],
    now: chrono::DateTime<chrono::Utc>,
) -> (String, String, String) {
    let x_date = now.format("%Y%m%dT%H%M%SZ").to_string();
    let short_date = now.format("%Y%m%d").to_string();
    let x_content_sha256 = volc_sha256_hex(body);

    // Fixed-order canonical headers, specific to Volcengine. Do not sort them.
    let canonical_headers = format!(
        "host:{VOLCENGINE_OPENAPI_HOST}\nx-date:{x_date}\nx-content-sha256:{x_content_sha256}\ncontent-type:{VOLCENGINE_CONTENT_TYPE}\n"
    );
    let canonical_request = format!(
        "POST\n/\n{canonical_query}\n{canonical_headers}\n{VOLCENGINE_SIGNED_HEADERS}\n{x_content_sha256}"
    );

    let credential_scope = format!("{short_date}/{region}/{VOLCENGINE_SERVICE}/request");
    let string_to_sign = format!(
        "HMAC-SHA256\n{x_date}\n{credential_scope}\n{}",
        volc_sha256_hex(canonical_request.as_bytes())
    );

    // Derive the signing key as `kDate=HMAC(SK, date)` with no `AWS4` prefix on
    // the secret, and use `request` as the terminal scope component.
    let k_date = volc_hmac_sha256(secret_access_key.as_bytes(), short_date.as_bytes());
    let k_region = volc_hmac_sha256(&k_date, region.as_bytes());
    let k_service = volc_hmac_sha256(&k_region, VOLCENGINE_SERVICE.as_bytes());
    let k_signing = volc_hmac_sha256(&k_service, b"request");
    let signature: String = volc_hmac_sha256(&k_signing, string_to_sign.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();

    let authorization = format!(
        "HMAC-SHA256 Credential={access_key_id}/{credential_scope}, SignedHeaders={VOLCENGINE_SIGNED_HEADERS}, Signature={signature}"
    );
    (authorization, x_date, x_content_sha256)
}

async fn volcengine_openapi_call(
    region: &str,
    access_key_id: &str,
    secret_access_key: &str,
    action: &str,
) -> VolcCall {
    let client = crate::proxy::http_client::get();
    // Use the same canonical query for signing and the actual URL so both stay
    // byte-identical; otherwise the signature will not match.
    let canonical_query = volcengine_canonical_query(action, region);
    let url = format!("https://{VOLCENGINE_OPENAPI_HOST}/?{canonical_query}");
    let body: &[u8] = b"";
    let (authorization, x_date, x_content_sha256) = volcengine_sign(
        access_key_id,
        secret_access_key,
        region,
        &canonical_query,
        body,
        chrono::Utc::now(),
    );

    let resp = client
        .post(&url)
        .header("X-Date", x_date)
        .header("X-Content-Sha256", x_content_sha256)
        .header("Content-Type", VOLCENGINE_CONTENT_TYPE)
        .header("Authorization", authorization)
        .body(body.to_vec())
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await;

    let resp = match resp {
        Ok(r) => r,
        Err(e) => return VolcCall::Soft(format!("Network error: {e}")),
    };

    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return VolcCall::Auth(format!(
            "Authentication failed (HTTP {status}). {VOLCENGINE_AKSK_HINT}"
        ));
    }
    if !status.is_success() {
        // The Volcengine OpenAPI gateway often returns 4xx, usually HTTP 400, for
        // signature or credential errors and still includes the same
        // `ResponseMetadata.Error` envelope used by 200 responses rather than a
        // plain 401/403. Parse that envelope here too so rejected Bearer-style
        // credentials still surface AK/SK guidance and expired credentials instead
        // of a generic API error.
        let raw = resp.text().await.unwrap_or_default();
        if let Ok(body) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some((code, msg)) = volcengine_response_error(&body) {
                if volcengine_is_auth_error_code(&code) {
                    return VolcCall::Auth(format!(
                        "Authentication failed (HTTP {status}, {code}): {msg}. {VOLCENGINE_AKSK_HINT}"
                    ));
                }
                return VolcCall::Soft(format!("API error (HTTP {status}, {code}): {msg}"));
            }
        }
        return VolcCall::Soft(format!("API error (HTTP {status}): {raw}"));
    }

    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => return VolcCall::Soft(format!("Failed to parse response: {e}")),
    };

    // Volcengine OpenAPI business errors commonly arrive as HTTP 200 plus
    // `ResponseMetadata.Error`.
    if let Some((code, msg)) = volcengine_response_error(&body) {
        if volcengine_is_auth_error_code(&code) {
            return VolcCall::Auth(format!(
                "Authentication failed ({code}): {msg}. {VOLCENGINE_AKSK_HINT}"
            ));
        }
        return VolcCall::Soft(format!("API error ({code}): {msg}"));
    }

    VolcCall::Body(body)
}

/// Parse the `Result` payload of `GetAFPUsage` into a tier list.
///
/// Show the 5-hour, weekly, and monthly windows to match the console.
/// `AFPDaily` is hidden by the official console, likely because its quota is often
/// above the weekly cap and behaves as a historical default rather than an
/// enforced limit, so it is skipped here as well.
/// `Quota` and `Used` are absolute AFP values, so used percent is `Used/Quota*100`.
/// Treat `Quota <= 0` as an unsubscribed or disabled window and skip it. This also
/// lets the caller identify "authenticated but no Agent Plan" as an empty result
/// and fall back to Coding Plan detection.
fn parse_afp_tiers(result: &serde_json::Value) -> Vec<QuotaTier> {
    let mut tiers = Vec::new();
    for (key, name) in [
        ("AFPFiveHour", TIER_FIVE_HOUR),
        ("AFPWeekly", TIER_WEEKLY_LIMIT),
        ("AFPMonthly", TIER_MONTHLY),
    ] {
        let Some(win) = result.get(key) else { continue };
        let quota = win.get("Quota").and_then(parse_f64).unwrap_or(0.0);
        if quota <= 0.0 {
            continue;
        }
        let used = win.get("Used").and_then(parse_f64).unwrap_or(0.0);
        // Used percent. Do not clamp the range; keep behavior aligned with
        // `parse_zhipu_token_tiers` and `parse_minimax_tiers`, and leave display
        // policy to the rendering layer.
        let utilization = used / quota * 100.0;
        let resets_at = win.get("ResetTime").and_then(extract_reset_time);
        tiers.push(QuotaTier {
            name: name.to_string(),
            utilization,
            resets_at,
            used_value_usd: None,
            max_value_usd: None,
        });
    }
    tiers
}

/// Normalize `GetCodingPlanUsage` window labels to tier names.
fn volcengine_coding_window(label: &str) -> Option<&'static str> {
    match label.to_lowercase().as_str() {
        "session" | "5h" | "fivehour" | "five_hour" | "rolling_5h" => Some(TIER_FIVE_HOUR),
        "weekly" | "week" | "7d" => Some(TIER_WEEKLY_LIMIT),
        "monthly" | "month" => Some(TIER_MONTHLY),
        _ => None,
    }
}

/// Parse the `Result` payload of `GetCodingPlanUsage` into a tier list defensively.
///
/// The official documentation does not fully specify this response field by field.
/// Based on the official `ark-cli` description, it returns `session`, `weekly`,
/// and `monthly` windows, exposes only the used percentage, and reports reset
/// times in seconds. This parser therefore accepts `QuotaUsage`, `Usages`, or
/// `Details` arrays plus several possible field names, consuming recognized
/// values and skipping unknown ones.
fn parse_coding_plan_tiers(result: &serde_json::Value) -> Vec<QuotaTier> {
    let mut tiers = Vec::new();
    let arr = result
        .get("QuotaUsage")
        .and_then(|v| v.as_array())
        .or_else(|| result.get("Usages").and_then(|v| v.as_array()))
        .or_else(|| result.get("Details").and_then(|v| v.as_array()));
    let Some(arr) = arr else { return tiers };

    for item in arr {
        // The real field is `Level`, observed on June 21, 2026 with
        // `session`/`weekly`/`monthly`. The rest are defensive fallbacks.
        let label = item
            .get("Level")
            .and_then(|v| v.as_str())
            .or_else(|| item.get("Type").and_then(|v| v.as_str()))
            .or_else(|| item.get("Period").and_then(|v| v.as_str()))
            .or_else(|| item.get("Label").and_then(|v| v.as_str()))
            .or_else(|| item.get("Window").and_then(|v| v.as_str()))
            .unwrap_or("");
        let Some(name) = volcengine_coding_window(label) else {
            continue;
        };
        let utilization = item
            .get("Percent")
            .and_then(parse_f64)
            .or_else(|| item.get("UsedPercent").and_then(parse_f64))
            .or_else(|| item.get("UsagePercent").and_then(parse_f64))
            .unwrap_or(0.0);
        // Accept seconds, milliseconds, and strings. `extract_reset_time`
        // distinguishes seconds from milliseconds internally.
        let resets_at = item
            .get("ResetTime")
            .or_else(|| item.get("ResetTimestamp"))
            .and_then(extract_reset_time);
        tiers.push(QuotaTier {
            name: name.to_string(),
            utilization,
            resets_at,
            used_value_usd: None,
            max_value_usd: None,
        });
    }
    tiers
}

fn volcengine_success(tiers: Vec<QuotaTier>, plan: Option<String>) -> SubscriptionQuota {
    SubscriptionQuota {
        tool: "coding_plan".to_string(),
        credential_status: CredentialStatus::Valid,
        credential_message: plan,
        success: true,
        tiers,
        extra_usage: None,
        error: None,
        queried_at: Some(now_millis()),
    }
}

fn volcengine_auth_error(detail: String) -> SubscriptionQuota {
    SubscriptionQuota {
        tool: "coding_plan".to_string(),
        credential_status: CredentialStatus::Expired,
        credential_message: Some("Invalid API key".to_string()),
        success: false,
        tiers: vec![],
        extra_usage: None,
        error: Some(detail),
        queried_at: Some(now_millis()),
    }
}

async fn query_volcengine(
    base_url: &str,
    access_key_id: &str,
    secret_access_key: &str,
) -> SubscriptionQuota {
    let region = volcengine_region(base_url);
    let mut soft_errors: Vec<String> = Vec::new();
    // If the response is 2xx with no `Error` envelope but still yields no tiers,
    // keep a truncated raw response for diagnostics so it is possible to
    // distinguish "truly not subscribed" from "guessed the wrong field names or
    // wrapper layer". Signature failures do not reach this path because they
    // surface through the Auth or Soft branches.
    let mut empty_responses: Vec<String> = Vec::new();
    let summarize = |action: &str, body: &serde_json::Value| -> String {
        let raw: String = body.to_string().chars().take(700).collect();
        format!("{action}={raw}")
    };

    // 1) Agent Plan: `GetAFPUsage`
    match volcengine_openapi_call(&region, access_key_id, secret_access_key, "GetAFPUsage").await {
        VolcCall::Auth(detail) => return volcengine_auth_error(detail),
        VolcCall::Soft(detail) => soft_errors.push(format!("GetAFPUsage: {detail}")),
        VolcCall::Body(body) => {
            let result = body.get("Result").unwrap_or(&body);
            let tiers = parse_afp_tiers(result);
            if !tiers.is_empty() {
                let plan = result
                    .get("PlanType")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| format!("Agent Plan {s}"));
                return volcengine_success(tiers, plan);
            }
            empty_responses.push(summarize("GetAFPUsage", &body));
        }
    }

    // 2) Coding Plan: `GetCodingPlanUsage`
    match volcengine_openapi_call(
        &region,
        access_key_id,
        secret_access_key,
        "GetCodingPlanUsage",
    )
    .await
    {
        VolcCall::Auth(detail) => return volcengine_auth_error(detail),
        VolcCall::Soft(detail) => soft_errors.push(format!("GetCodingPlanUsage: {detail}")),
        VolcCall::Body(body) => {
            let result = body.get("Result").unwrap_or(&body);
            let tiers = parse_coding_plan_tiers(result);
            if !tiers.is_empty() {
                return volcengine_success(tiers, Some("Coding Plan".to_string()));
            }
            empty_responses.push(summarize("GetCodingPlanUsage", &body));
        }
    }

    if !soft_errors.is_empty() {
        make_error(soft_errors.join("; "))
    } else if !empty_responses.is_empty() {
        // The signature passed and the request reached the business layer, but no
        // parsable quota was found. Include the raw response to help verify the
        // actual field names and wrapper shape or confirm there is no subscription.
        make_error(format!(
            "No active subscription found (signature OK). Raw: {}",
            empty_responses.join(" || ")
        ))
    } else {
        make_error(
            "No active Agent Plan or Coding Plan subscription found for this credential"
                .to_string(),
        )
    }
}

// ── Public entrypoint ──────────────────────────────────────

/// Build a failure result for missing credentials or an unknown provider domain,
/// using `NotFound` plus an explicit error message.
fn coding_plan_not_found(error: &str) -> SubscriptionQuota {
    SubscriptionQuota {
        tool: "coding_plan".to_string(),
        credential_status: CredentialStatus::NotFound,
        credential_message: None,
        success: false,
        tiers: vec![],
        extra_usage: None,
        error: Some(error.to_string()),
        queried_at: None,
    }
}

pub async fn get_coding_plan_quota(
    base_url: &str,
    api_key: &str,
    access_key_id: Option<&str>,
    secret_access_key: Option<&str>,
) -> Result<SubscriptionQuota, String> {
    let provider = match detect_provider(base_url) {
        Some(p) => p,
        // The domain does not match a known plan provider, such as a third-party
        // relay. Return an explicit error instead of failing silently.
        None => return Ok(coding_plan_not_found("Unknown coding plan provider")),
    };

    // Volcengine Ark uses control-plane AK/SK signing instead of the data-plane
    // Bearer `api_key` used by other providers, so credentials and query flow are
    // different and need an early dedicated branch.
    if let CodingPlanProvider::Volcengine = provider {
        let ak = access_key_id.unwrap_or("").trim();
        let sk = secret_access_key.unwrap_or("").trim();
        if ak.is_empty() || sk.is_empty() {
            return Ok(coding_plan_not_found(
                "Volcengine usage query needs the account AccessKey ID + Secret (not the inference API key)",
            ));
        }
        return Ok(query_volcengine(base_url, ak, sk).await);
    }

    // All other providers use data-plane Bearer `api_key`.
    // Match `balance::get_balance` by returning an explicit error instead of a
    // blank failure in the footer.
    if api_key.trim().is_empty() {
        return Ok(coding_plan_not_found("API key is empty"));
    }

    let quota = match provider {
        CodingPlanProvider::Kimi => query_kimi(api_key).await,
        CodingPlanProvider::ZhipuCn | CodingPlanProvider::ZhipuEn => {
            query_zhipu(base_url, api_key).await
        }
        CodingPlanProvider::MiniMaxCn => query_minimax(api_key, true).await,
        CodingPlanProvider::MiniMaxEn => query_minimax(api_key, false).await,
        CodingPlanProvider::ZenMux => query_zenmux(base_url, api_key).await,
        // Volcengine already returns through the AK/SK branch above, so this path
        // is unreachable.
        CodingPlanProvider::Volcengine => {
            unreachable!("volcengine handled via AK/SK branch above")
        }
    };

    Ok(quota)
}

#[cfg(test)]
mod tests {
    use super::{
        parse_afp_tiers, parse_coding_plan_tiers, parse_minimax_tiers, parse_zhipu_token_tiers,
        volcengine_canonical_query, volcengine_is_auth_error_code, volcengine_region,
        volcengine_response_error, volcengine_sign, zhipu_quota_base, TIER_FIVE_HOUR, TIER_MONTHLY,
        TIER_WEEKLY_LIMIT,
    };
    use serde_json::json;

    #[test]
    fn zhipu_new_plan_two_tiers_sorted_by_reset_time() {
        // New plans return two `TOKENS_LIMIT` entries. The earlier `nextResetTime`
        // maps to `five_hour`, the later one to `weekly_limit`. Put the weekly
        // bucket first on purpose to verify order independence.
        let data = json!({
            "limits": [
                { "type": "TOKENS_LIMIT", "percentage": 53.0, "nextResetTime": 2_000_000_000_000_i64 },
                { "type": "TOKENS_LIMIT", "percentage": 44.0, "nextResetTime": 1_000_000_000_000_i64 },
                { "type": "TIME_LIMIT",   "percentage":  7.0 },
            ]
        });
        let tiers = parse_zhipu_token_tiers(&data);
        assert_eq!(tiers.len(), 2);
        assert_eq!(tiers[0].name, TIER_FIVE_HOUR);
        assert_eq!(tiers[0].utilization, 44.0);
        assert_eq!(tiers[1].name, TIER_WEEKLY_LIMIT);
        assert_eq!(tiers[1].utilization, 53.0);
    }

    #[test]
    fn zhipu_old_plan_single_tier_falls_back_to_five_hour() {
        // Old plans subscribed before February 12, 2026 return only one
        // `TOKENS_LIMIT` and have no weekly quota.
        let data = json!({
            "limits": [
                {
                    "type": "TOKENS_LIMIT",
                    "percentage": 2.0,
                    "nextResetTime": 1_774_967_594_803_i64
                },
                { "type": "TIME_LIMIT", "percentage": 0.0 }
            ]
        });
        let tiers = parse_zhipu_token_tiers(&data);
        assert_eq!(tiers.len(), 1);
        assert_eq!(tiers[0].name, TIER_FIVE_HOUR);
        assert_eq!(tiers[0].utilization, 2.0);
    }

    #[test]
    fn zhipu_no_token_limits_returns_empty() {
        let data = json!({ "limits": [{ "type": "TIME_LIMIT", "percentage": 5.0 }] });
        assert!(parse_zhipu_token_tiers(&data).is_empty());
    }

    #[test]
    fn zhipu_missing_reset_time_is_five_hour_when_weekly_has_reset() {
        // In a real report, the 5-hour bucket at 0% had no `nextResetTime` while
        // the weekly bucket did. This shape must not let reset ordering misclassify
        // the weekly bucket as `five_hour`.
        let data = json!({
            "limits": [
                { "type": "TOKENS_LIMIT", "percentage": 25.0, "nextResetTime": 2_000_000_000_000_i64 },
                { "type": "TOKENS_LIMIT", "percentage": 0.0 }
            ]
        });
        let tiers = parse_zhipu_token_tiers(&data);
        assert_eq!(tiers.len(), 2);
        assert_eq!(tiers[0].name, TIER_FIVE_HOUR);
        assert_eq!(tiers[0].utilization, 0.0);
        assert!(tiers[0].resets_at.is_none());
        assert_eq!(tiers[1].name, TIER_WEEKLY_LIMIT);
        assert_eq!(tiers[1].utilization, 25.0);
        assert!(tiers[1].resets_at.is_some());
    }

    #[test]
    fn zhipu_type_is_case_insensitive() {
        // Defensive behavior: still recognize the field if upstream changes
        // `"TOKENS_LIMIT"` to `"tokens_limit"` with case-only differences.
        // Delimiter changes such as `"TokensLimit"` without the underscore are not
        // treated as compatible.
        let data = json!({
            "limits": [
                { "type": "tokens_limit", "percentage": 12.0, "nextResetTime": 1_000_000_000_000_i64 },
                { "type": "Tokens_Limit", "percentage": 34.0, "nextResetTime": 2_000_000_000_000_i64 }
            ]
        });
        let tiers = parse_zhipu_token_tiers(&data);
        assert_eq!(tiers.len(), 2);
        assert_eq!(tiers[0].name, TIER_FIVE_HOUR);
        assert_eq!(tiers[0].utilization, 12.0);
        assert_eq!(tiers[1].name, TIER_WEEKLY_LIMIT);
        assert_eq!(tiers[1].utilization, 34.0);
    }

    #[test]
    fn zhipu_invalid_percentage_falls_back_to_zero() {
        // If `percentage` is a string or `null`, do not crash. Treat it as 0 while
        // still keeping the tier visible.
        let data = json!({
            "limits": [
                { "type": "TOKENS_LIMIT", "percentage": "invalid", "nextResetTime": 1_000_000_000_000_i64 },
                { "type": "TOKENS_LIMIT", "percentage": null,      "nextResetTime": 2_000_000_000_000_i64 }
            ]
        });
        let tiers = parse_zhipu_token_tiers(&data);
        assert_eq!(tiers.len(), 2);
        assert_eq!(tiers[0].utilization, 0.0);
        assert_eq!(tiers[1].utilization, 0.0);
    }

    #[test]
    fn zhipu_extreme_percentage_values_pass_through() {
        // Do not clamp negative or >100 values. The parser should preserve the
        // upstream value and let the rendering layer decide display policy.
        let data = json!({
            "limits": [
                { "type": "TOKENS_LIMIT", "percentage": -5.0,  "nextResetTime": 1_000_000_000_000_i64 },
                { "type": "TOKENS_LIMIT", "percentage": 150.0, "nextResetTime": 2_000_000_000_000_i64 }
            ]
        });
        let tiers = parse_zhipu_token_tiers(&data);
        assert_eq!(tiers.len(), 2);
        assert_eq!(tiers[0].utilization, -5.0);
        assert_eq!(tiers[1].utilization, 150.0);
    }

    #[test]
    fn zhipu_unit_field_overrides_reset_order_when_weekly_resets_sooner() {
        // Real case, reproduced again on June 10, 2026 for issue #3036: near the
        // end of a weekly cycle, the weekly bucket reset earlier than the 5-hour
        // bucket. The real values were roughly 1% used for 5h with reset in about
        // 5 hours and 42% used for weekly with reset in about 1 hour. The old
        // reset-order logic necessarily inverted them, so `unit` must win.
        let data = json!({
            "limits": [
                { "type": "TOKENS_LIMIT", "unit": 6, "number": 7, "percentage": 42.0, "nextResetTime": 1_000_003_600_000_i64 },
                { "type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 1.0,  "nextResetTime": 1_000_018_000_000_i64 }
            ]
        });
        let tiers = parse_zhipu_token_tiers(&data);
        assert_eq!(tiers.len(), 2);
        assert_eq!(tiers[0].name, TIER_FIVE_HOUR);
        assert_eq!(tiers[0].utilization, 1.0);
        assert_eq!(tiers[1].name, TIER_WEEKLY_LIMIT);
        assert_eq!(tiers[1].utilization, 42.0);
    }

    #[test]
    fn zhipu_weekly_unit_six_number_one_variant() {
        // `z.ai` has also been observed returning `(unit: 6, number: 1)` for a
        // weekly window, meaning classification depends only on `unit`.
        let data = json!({
            "limits": [
                { "type": "TOKENS_LIMIT", "unit": 6, "number": 1, "percentage": 30.0, "nextResetTime": 1_000_000_000_000_i64 },
                { "type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 10.0, "nextResetTime": 2_000_000_000_000_i64 }
            ]
        });
        let tiers = parse_zhipu_token_tiers(&data);
        assert_eq!(tiers.len(), 2);
        assert_eq!(tiers[0].name, TIER_FIVE_HOUR);
        assert_eq!(tiers[0].utilization, 10.0);
        assert_eq!(tiers[1].name, TIER_WEEKLY_LIMIT);
        assert_eq!(tiers[1].utilization, 30.0);
    }

    #[test]
    fn zhipu_partial_unit_fields_fill_remaining_slot() {
        // If only the weekly bucket carries `unit`, the other entry with no `unit`
        // should fill the remaining `five_hour` slot even if it resets later.
        // Explicit classification must not be disturbed by time ordering.
        let data = json!({
            "limits": [
                { "type": "TOKENS_LIMIT", "unit": 6, "number": 7, "percentage": 42.0, "nextResetTime": 1_000_000_000_000_i64 },
                { "type": "TOKENS_LIMIT", "percentage": 1.0, "nextResetTime": 2_000_000_000_000_i64 }
            ]
        });
        let tiers = parse_zhipu_token_tiers(&data);
        assert_eq!(tiers.len(), 2);
        assert_eq!(tiers[0].name, TIER_FIVE_HOUR);
        assert_eq!(tiers[0].utilization, 1.0);
        assert_eq!(tiers[1].name, TIER_WEEKLY_LIMIT);
        assert_eq!(tiers[1].utilization, 42.0);
    }

    #[test]
    fn zhipu_unknown_unit_values_fall_back_to_reset_order() {
        // Unknown `unit` enum values should not be guessed. Fall back to the older
        // reset-time heuristic for the whole set.
        let data = json!({
            "limits": [
                { "type": "TOKENS_LIMIT", "unit": 9, "percentage": 44.0, "nextResetTime": 1_000_000_000_000_i64 },
                { "type": "TOKENS_LIMIT", "unit": 9, "percentage": 53.0, "nextResetTime": 2_000_000_000_000_i64 }
            ]
        });
        let tiers = parse_zhipu_token_tiers(&data);
        assert_eq!(tiers.len(), 2);
        assert_eq!(tiers[0].name, TIER_FIVE_HOUR);
        assert_eq!(tiers[0].utilization, 44.0);
        assert_eq!(tiers[1].name, TIER_WEEKLY_LIMIT);
        assert_eq!(tiers[1].utilization, 53.0);
    }

    #[test]
    fn zhipu_duplicate_unit_classification_fills_other_slot() {
        // Defensive behavior: if upstream incorrectly labels both entries as the
        // 5-hour window, the first occupies `five_hour` and the second degrades
        // into the weekly slot via fallback, preserving data without panicking.
        let data = json!({
            "limits": [
                { "type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 10.0, "nextResetTime": 1_000_000_000_000_i64 },
                { "type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 20.0, "nextResetTime": 2_000_000_000_000_i64 }
            ]
        });
        let tiers = parse_zhipu_token_tiers(&data);
        assert_eq!(tiers.len(), 2);
        assert_eq!(tiers[0].name, TIER_FIVE_HOUR);
        assert_eq!(tiers[0].utilization, 10.0);
        assert_eq!(tiers[1].name, TIER_WEEKLY_LIMIT);
        assert_eq!(tiers[1].utilization, 20.0);
    }

    #[test]
    fn zhipu_more_than_two_token_limits_keeps_first_two() {
        // Defensive behavior: Zhipu currently returns at most two `TOKENS_LIMIT`
        // entries. If upstream adds a third unexpectedly, drop it to avoid leaving
        // naming gaps.
        let data = json!({
            "limits": [
                { "type": "TOKENS_LIMIT", "percentage": 1.0, "nextResetTime": 1_000_000_000_000_i64 },
                { "type": "TOKENS_LIMIT", "percentage": 2.0, "nextResetTime": 2_000_000_000_000_i64 },
                { "type": "TOKENS_LIMIT", "percentage": 3.0, "nextResetTime": 3_000_000_000_000_i64 }
            ]
        });
        let tiers = parse_zhipu_token_tiers(&data);
        assert_eq!(tiers.len(), 2);
        assert_eq!(tiers[0].name, TIER_FIVE_HOUR);
        assert_eq!(tiers[1].name, TIER_WEEKLY_LIMIT);
    }

    // ── MiniMax ──

    #[test]
    fn minimax_general_two_tiers_from_remaining_percent() {
        // Main path: the `general` bucket reports 98% left for 5h and 95% left for
        // weekly, which should invert to 2% and 5% used.
        let body = json!({
            "model_remains": [
                {
                    "model_name": "general",
                    "current_interval_remaining_percent": 98.0,
                    "current_weekly_remaining_percent": 95.0,
                    "current_interval_status": 1,
                    "current_weekly_status": 1,
                    "end_time": 1_780_329_600_000_i64,
                    "weekly_end_time": 1_780_848_000_000_i64
                },
                {
                    "model_name": "video",
                    "current_interval_remaining_percent": 100.0,
                    "current_weekly_remaining_percent": 100.0
                }
            ],
            "base_resp": { "status_code": 0, "status_msg": "success" }
        });
        let tiers = parse_minimax_tiers(&body);
        assert_eq!(tiers.len(), 2);
        assert_eq!(tiers[0].name, TIER_FIVE_HOUR);
        assert_eq!(tiers[0].utilization, 2.0);
        assert!(tiers[0].resets_at.is_some());
        assert_eq!(tiers[1].name, TIER_WEEKLY_LIMIT);
        assert_eq!(tiers[1].utilization, 5.0);
        assert!(tiers[1].resets_at.is_some());
    }

    #[test]
    fn minimax_skips_video_and_finds_general_in_any_position() {
        // Defensive behavior: find `general` even if `video` appears first in the
        // array.
        let body = json!({
            "model_remains": [
                {
                    "model_name": "video",
                    "current_interval_remaining_percent": 50.0,
                    "current_weekly_remaining_percent": 50.0
                },
                {
                    "model_name": "general",
                    "current_interval_remaining_percent": 80.0,
                    "current_weekly_remaining_percent": 70.0,
                    "current_interval_status": 1,
                    "current_weekly_status": 1
                }
            ]
        });
        let tiers = parse_minimax_tiers(&body);
        assert_eq!(tiers.len(), 2);
        // These values come from the `general` bucket, not `video`: 20%/30% rather
        // than 50%/50%.
        assert_eq!(tiers[0].utilization, 20.0);
        assert_eq!(tiers[1].utilization, 30.0);
    }

    #[test]
    fn minimax_missing_general_returns_empty() {
        // If `model_remains` contains only `video`, is empty, or is missing, the
        // parser should not crash and should return no tiers.
        let body = json!({
            "model_remains": [
                {
                    "model_name": "video",
                    "current_interval_remaining_percent": 100.0,
                    "current_weekly_remaining_percent": 100.0
                }
            ]
        });
        assert!(parse_minimax_tiers(&body).is_empty());

        let body_empty: serde_json::Value = json!({ "model_remains": [] });
        assert!(parse_minimax_tiers(&body_empty).is_empty());

        let body_no_field = json!({});
        assert!(parse_minimax_tiers(&body_no_field).is_empty());
    }

    #[test]
    fn minimax_missing_percent_fields_skips_tier() {
        // If a field is missing, skip only the affected bucket and still keep the
        // other one visible.
        let body = json!({
            "model_remains": [{
                "model_name": "general",
                "current_interval_remaining_percent": 60.0,
                "current_weekly_status": 1
                // Missing `current_weekly_remaining_percent`
            }]
        });
        let tiers = parse_minimax_tiers(&body);
        assert_eq!(tiers.len(), 1);
        assert_eq!(tiers[0].name, TIER_FIVE_HOUR);
        assert_eq!(tiers[0].utilization, 40.0);
    }

    #[test]
    fn minimax_negative_percent_passes_through() {
        // Defensive behavior: match the contract of `parse_zhipu_token_tiers` and
        // do not clamp negative or >100 values.
        let body = json!({
            "model_remains": [{
                "model_name": "general",
                "current_interval_remaining_percent": -5.0,
                "current_weekly_remaining_percent": 150.0,
                "current_interval_status": 1,
                "current_weekly_status": 1
            }]
        });
        let tiers = parse_minimax_tiers(&body);
        assert_eq!(tiers.len(), 2);
        assert_eq!(tiers[0].utilization, 105.0); // 100 - (-5)
        assert_eq!(tiers[1].utilization, -50.0); // 100 - 150
    }

    #[test]
    fn minimax_weekly_status_3_skips_weekly_tier() {
        // Plans without a weekly quota report `current_weekly_status = 3` and keep
        // `remaining_percent` at 100, so `weekly_limit` must not be emitted or the
        // UI would show a fake "0% used" weekly bucket.
        let body = json!({
            "model_remains": [
                {
                    "model_name": "general",
                    "start_time": 1_780_347_600_000_i64,
                    "end_time": 1_780_365_600_000_i64,
                    "remains_time": 4_161_372_i64,
                    "current_interval_remaining_percent": 99,
                    "current_interval_status": 1,
                    "current_weekly_total_count": 0,
                    "current_weekly_usage_count": 0,
                    "weekly_start_time": 1_780_243_200_000_i64,
                    "weekly_end_time": 1_780_848_000_000_i64,
                    "weekly_remains_time": 486_561_372_i64,
                    "current_weekly_status": 3,
                    "current_weekly_remaining_percent": 100
                },
                {
                    "model_name": "video",
                    "current_interval_remaining_percent": 100,
                    "current_weekly_status": 3,
                    "current_weekly_remaining_percent": 100
                }
            ],
            "base_resp": { "status_code": 0, "status_msg": "success" }
        });
        let tiers = parse_minimax_tiers(&body);
        assert_eq!(tiers.len(), 1);
        assert_eq!(tiers[0].name, TIER_FIVE_HOUR);
        assert_eq!(tiers[0].utilization, 1.0);
        assert!(tiers[0].resets_at.is_some());
    }

    #[test]
    fn minimax_weekly_status_2_also_skips_weekly_tier() {
        // Defensive behavior: treat any status other than `1` as an inactive weekly
        // bucket and skip it.
        let body = json!({
            "model_remains": [{
                "model_name": "general",
                "current_interval_remaining_percent": 80.0,
                "current_weekly_remaining_percent": 50.0,
                "current_weekly_status": 2
            }]
        });
        let tiers = parse_minimax_tiers(&body);
        assert_eq!(tiers.len(), 1);
        assert_eq!(tiers[0].name, TIER_FIVE_HOUR);
        assert_eq!(tiers[0].utilization, 20.0);
    }

    #[test]
    fn zhipu_quota_base_routes_bigmodel_url_to_cn_endpoint() {
        assert_eq!(
            zhipu_quota_base("https://open.bigmodel.cn/api/paas/v4"),
            "https://open.bigmodel.cn"
        );
    }

    #[test]
    fn zhipu_quota_base_routes_z_ai_url_to_en_endpoint() {
        assert_eq!(
            zhipu_quota_base("https://api.z.ai/api/paas/v4"),
            "https://api.z.ai"
        );
    }

    #[test]
    fn zhipu_quota_base_defaults_to_en_for_unknown_url() {
        // When the URL has no obvious Zhipu domain markers, default to the
        // international endpoint because it is the more general entrypoint.
        assert_eq!(
            zhipu_quota_base("https://example.com/zhipu"),
            "https://api.z.ai"
        );
    }

    #[test]
    fn zhipu_quota_base_routes_uppercase_cn_url_to_cn_endpoint() {
        // Keep routing case-insensitive to match `detect_provider` and avoid
        // silently routing an uppercase preset URL to the international endpoint.
        assert_eq!(
            zhipu_quota_base("HTTPS://OPEN.BIGMODEL.CN/api/paas/v4"),
            "https://open.bigmodel.cn"
        );
        assert_eq!(
            zhipu_quota_base("https://Open.BigModel.cn/api/paas/v4"),
            "https://open.bigmodel.cn"
        );
    }

    // ── Volcengine Ark Agent Plan / Coding Plan ──

    #[test]
    fn volcengine_afp_three_windows_from_official_example() {
        // Literal example from the official `GetAFPUsage` docs: 5h 25%, weekly 30%,
        // monthly 42.525%. `AFPDaily` is hidden by the console and should be skipped.
        let result = json!({
            "PlanType": "Large",
            "AFPFiveHour": { "Quota": 50.0,   "Used": 12.5,  "SubscribeTime": 1778788800000_i64, "ResetTime": 1778806800000_i64 },
            "AFPDaily":    { "Quota": 100.0,  "Used": 22.5,  "SubscribeTime": 1778716800000_i64, "ResetTime": 1778803200000_i64 },
            "AFPWeekly":   { "Quota": 500.0,  "Used": 150.0, "SubscribeTime": 1778457600000_i64, "ResetTime": 1779062400000_i64 },
            "AFPMonthly":  { "Quota": 2000.0, "Used": 850.5, "SubscribeTime": 1777939200000_i64, "ResetTime": 1780531200000_i64 }
        });
        let tiers = parse_afp_tiers(&result);
        assert_eq!(
            tiers.len(),
            3,
            "daily should be skipped, leaving only 5h/weekly/monthly"
        );
        assert_eq!(tiers[0].name, TIER_FIVE_HOUR);
        assert!((tiers[0].utilization - 25.0).abs() < 1e-9);
        assert!(tiers[0].resets_at.is_some());
        assert_eq!(tiers[1].name, TIER_WEEKLY_LIMIT);
        assert!((tiers[1].utilization - 30.0).abs() < 1e-9);
        assert_eq!(tiers[2].name, TIER_MONTHLY);
        assert!((tiers[2].utilization - 42.525).abs() < 1e-9);
        assert!(tiers[2].resets_at.is_some());
    }

    #[test]
    fn volcengine_afp_zero_quota_windows_treated_as_unbound() {
        // Auth succeeded but there is no Agent Plan: windows with `Quota = 0`
        // produce an empty result, which lets the caller fall back to Coding Plan.
        let result = json!({
            "PlanType": "",
            "AFPFiveHour": { "Quota": 0.0, "Used": 0.0 },
            "AFPWeekly":   { "Quota": 0.0, "Used": 0.0 },
            "AFPMonthly":  { "Quota": 0.0, "Used": 0.0 }
        });
        assert!(parse_afp_tiers(&result).is_empty());
    }

    #[test]
    fn volcengine_afp_partial_windows_only_subscribed_ones() {
        // Only the 5-hour window has quota, with weekly and monthly missing, so
        // only one tier should be emitted.
        let result = json!({
            "AFPFiveHour": { "Quota": 40.0, "Used": 10.0, "ResetTime": 1778806800000_i64 },
            "AFPWeekly":   { "Quota": 0.0,  "Used": 0.0 }
        });
        let tiers = parse_afp_tiers(&result);
        assert_eq!(tiers.len(), 1);
        assert_eq!(tiers[0].name, TIER_FIVE_HOUR);
        assert!((tiers[0].utilization - 25.0).abs() < 1e-9);
    }

    #[test]
    fn volcengine_coding_plan_real_response_levels() {
        // Real `GetCodingPlanUsage` response observed by a user on June 21, 2026:
        // the field name is `Level`, not `Type`; only percentages are returned;
        // `ResetTimestamp` is in seconds; and an inactive `session` window reports
        // `-1`, meaning there is no reset time.
        let result = json!({
            "Status": "Running",
            "UpdateTimestamp": 1782053286_i64,
            "QuotaUsage": [
                { "Level": "session", "Percent": 0.0,      "ResetTimestamp": -1_i64 },
                { "Level": "weekly",  "Percent": 1.672568, "ResetTimestamp": 1782057600_i64 },
                { "Level": "monthly", "Percent": 0.836284, "ResetTimestamp": 1784303999_i64 }
            ]
        });
        let tiers = parse_coding_plan_tiers(&result);
        assert_eq!(tiers.len(), 3);
        assert_eq!(tiers[0].name, TIER_FIVE_HOUR);
        assert!((tiers[0].utilization - 0.0).abs() < 1e-9);
        assert!(
            tiers[0].resets_at.is_none(),
            "session ResetTimestamp = -1 should mean there is no reset time"
        );
        assert_eq!(tiers[1].name, TIER_WEEKLY_LIMIT);
        assert!((tiers[1].utilization - 1.672568).abs() < 1e-6);
        assert!(tiers[1].resets_at.is_some());
        assert_eq!(tiers[2].name, TIER_MONTHLY);
        assert!((tiers[2].utilization - 0.836284).abs() < 1e-6);
    }

    #[test]
    fn volcengine_coding_plan_unknown_window_skipped_and_missing_array_empty() {
        let result = json!({
            "QuotaUsage": [
                { "Level": "daily", "Percent": 9.0 },
                { "Level": "weekly", "Percent": 20.0 }
            ]
        });
        let tiers = parse_coding_plan_tiers(&result);
        assert_eq!(tiers.len(), 1, "unknown daily window should be skipped");
        assert_eq!(tiers[0].name, TIER_WEEKLY_LIMIT);

        assert!(parse_coding_plan_tiers(&json!({})).is_empty());
    }

    #[test]
    fn volcengine_region_derivation() {
        assert_eq!(
            volcengine_region("https://ark.cn-beijing.volces.com/api/coding"),
            "cn-beijing"
        );
        // Extract other regions from the data-plane domain by segment.
        assert_eq!(
            volcengine_region("https://ark.cn-shanghai.volces.com/api/coding/v3"),
            "cn-shanghai"
        );
        // Fall back to the default `cn-beijing` when no recognizable region segment exists.
        assert_eq!(
            volcengine_region("https://example.com/api/coding"),
            "cn-beijing"
        );
    }

    #[test]
    fn volcengine_canonical_query_is_sorted_and_encoded() {
        // Keys are sorted alphabetically: `Action < Region < Version`. The `-`
        // character is unreserved and should not be encoded.
        assert_eq!(
            volcengine_canonical_query("GetAFPUsage", "cn-beijing"),
            "Action=GetAFPUsage&Region=cn-beijing&Version=2024-01-01"
        );
    }

    #[test]
    fn volcengine_sign_structure_and_determinism() {
        // Without a server-side golden vector, lock down the signature structure
        // contract and determinism. That is enough to catch implementation errors
        // such as header order, scope suffix, algorithm prefix, or empty-body hash.
        // Real correctness is still validated by live user observations.
        let now = chrono::DateTime::parse_from_rfc3339("2024-06-21T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let region = "cn-beijing";
        let query = volcengine_canonical_query("GetAFPUsage", region);
        let (auth, x_date, x_content) =
            volcengine_sign("AKLTtest", "secretkey", region, &query, b"", now);

        // SHA-256 of an empty body, proving that the empty body path is used.
        assert_eq!(
            x_content,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        // `X-Date` should look like `20240621T000000Z`.
        assert_eq!(x_date, "20240621T000000Z");
        // `Authorization` structure: no `AWS4` prefix in the algorithm, the scope
        // ends in `ark/request`, and `SignedHeaders` is fixed.
        assert!(
            auth.starts_with("HMAC-SHA256 Credential=AKLTtest/20240621/cn-beijing/ark/request,"),
            "unexpected credential/scope: {auth}"
        );
        assert!(
            auth.contains("SignedHeaders=host;x-date;x-content-sha256;content-type,"),
            "unexpected signed headers: {auth}"
        );
        // The signature should be a 64-character hexadecimal string.
        let sig = auth.rsplit("Signature=").next().unwrap();
        assert_eq!(sig.len(), 64);
        assert!(sig.bytes().all(|b| b.is_ascii_hexdigit()));

        // Determinism: identical input must produce identical output.
        let (auth2, _, _) = volcengine_sign("AKLTtest", "secretkey", region, &query, b"", now);
        assert_eq!(auth, auth2);
    }

    #[test]
    fn volcengine_auth_error_code_detection_and_extraction() {
        assert!(volcengine_is_auth_error_code("AccessDenied"));
        assert!(volcengine_is_auth_error_code("SignatureDoesNotMatch"));
        assert!(volcengine_is_auth_error_code("InvalidAuthorization"));
        assert!(volcengine_is_auth_error_code("Unauthorized"));
        assert!(!volcengine_is_auth_error_code("InvalidParameter.Action"));
        assert!(!volcengine_is_auth_error_code("InternalError"));

        // Extract `ResponseMetadata.Error`
        let body = json!({
            "ResponseMetadata": { "RequestId": "x", "Error": { "Code": "AccessDenied", "Message": "no permission" } }
        });
        let (code, msg) = volcengine_response_error(&body).expect("expected to extract Error");
        assert_eq!(code, "AccessDenied");
        assert_eq!(msg, "no permission");

        // Return `None` when there is no `Error`.
        let ok_body = json!({ "ResponseMetadata": { "RequestId": "x" }, "Result": {} });
        assert!(volcengine_response_error(&ok_body).is_none());
    }
}
