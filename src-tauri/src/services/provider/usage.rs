//! Usage script execution
//!
//! Handles executing and formatting usage query results.

use crate::app_config::AppType;
use crate::error::AppError;
use crate::provider::{UsageResult, UsageScript};
use crate::store::AppState;
use crate::usage_script;

fn user_facing_usage_error(error: AppError) -> String {
    match error {
        AppError::Localized { en, .. } => en,
        other => other.to_string(),
    }
}

fn format_usage_data(data: serde_json::Value) -> UsageResult {
    let parsed = if data.is_array() {
        serde_json::from_value(data)
    } else {
        serde_json::from_value(data).map(|single| vec![single])
    };

    match parsed {
        Ok(data) => UsageResult {
            success: true,
            data: Some(data),
            error: None,
        },
        Err(error) => UsageResult {
            success: false,
            data: None,
            error: Some(format!("Data format error: {error}")),
        },
    }
}

/// Execute usage script and format result (private helper method)
pub(crate) async fn execute_and_format_usage_result(
    script_code: &str,
    api_key: &str,
    base_url: &str,
    timeout: u64,
    access_token: Option<&str>,
    user_id: Option<&str>,
    template_type: Option<&str>,
) -> Result<UsageResult, AppError> {
    match usage_script::execute_usage_script(
        script_code,
        api_key,
        base_url,
        timeout,
        access_token,
        user_id,
        template_type,
    )
    .await
    {
        Ok(data) => Ok(format_usage_data(data)),
        Err(err) => Ok(UsageResult {
            success: false,
            data: None,
            error: Some(user_facing_usage_error(err)),
        }),
    }
}

/// Resolve `(api_key, base_url)` for the JS-script path: explicit non-empty
/// script values win, otherwise fall back to the provider's stored config via
/// `Provider::resolve_usage_credentials` — the same per-app resolver the
/// native balance/coding-plan path and the frontend `getProviderCredentials`
/// use, so `{{apiKey}}`/`{{baseUrl}}` match what the UI shows for them.
fn resolve_script_credentials(
    app_type: &AppType,
    provider: &crate::provider::Provider,
    api_key: Option<&str>,
    base_url: Option<&str>,
) -> (String, String) {
    let (provider_base_url, provider_api_key) = provider.resolve_usage_credentials(app_type);

    let api_key = api_key
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or(provider_api_key);

    let base_url = base_url
        .map(str::trim)
        .filter(|value| !value.is_empty())
        // Trim like the provider path so `{{baseUrl}}/path` never doubles the slash.
        .map(|value| value.trim_end_matches('/').to_owned())
        .unwrap_or(provider_base_url);

    (api_key, base_url)
}

/// Query provider usage (using saved script configuration)
pub async fn query_usage(
    state: &AppState,
    app_type: AppType,
    provider_id: &str,
) -> Result<UsageResult, AppError> {
    let (script_code, timeout, api_key, base_url, access_token, user_id, template_type) = {
        let providers = state.db.get_all_providers(app_type.as_str())?;
        let provider = providers.get(provider_id).ok_or_else(|| {
            AppError::localized(
                "provider.not_found",
                format!("供应商不存在: {provider_id}"),
                format!("Provider not found: {provider_id}"),
            )
        })?;

        let usage_script = provider
            .meta
            .as_ref()
            .and_then(|m| m.usage_script.as_ref())
            .ok_or_else(|| {
                AppError::localized(
                    "provider.usage.script.missing",
                    "未配置用量查询脚本",
                    "Usage script is not configured",
                )
            })?;
        if !usage_script.enabled {
            return Err(AppError::localized(
                "provider.usage.disabled",
                "用量查询未启用",
                "Usage query is disabled",
            ));
        }

        // Get credentials: prioritize UsageScript values, fallback to provider config
        let (api_key, base_url) = resolve_script_credentials(
            &app_type,
            provider,
            usage_script.api_key.as_deref(),
            usage_script.base_url.as_deref(),
        );

        (
            usage_script.code.clone(),
            usage_script.timeout.unwrap_or(10),
            api_key,
            base_url,
            usage_script.access_token.clone(),
            usage_script.user_id.clone(),
            usage_script.template_type.clone(),
        )
    };

    execute_and_format_usage_result(
        &script_code,
        &api_key,
        &base_url,
        timeout,
        access_token.as_deref(),
        user_id.as_deref(),
        template_type.as_deref(),
    )
    .await
}

/// Test usage script (using temporary script content, not saved)
#[allow(clippy::too_many_arguments)]
pub async fn test_usage_script(
    state: &AppState,
    app_type: AppType,
    provider_id: &str,
    script_code: &str,
    timeout: u64,
    api_key: Option<&str>,
    base_url: Option<&str>,
    access_token: Option<&str>,
    user_id: Option<&str>,
    template_type: Option<&str>,
) -> Result<UsageResult, AppError> {
    let providers = state.db.get_all_providers(app_type.as_str())?;
    let provider = providers.get(provider_id).ok_or_else(|| {
        AppError::localized(
            "provider.not_found",
            format!("供应商不存在: {provider_id}"),
            format!("Provider not found: {provider_id}"),
        )
    })?;

    // Resolve like the real query so testing matches what a saved script does:
    // explicit values win, empty ones fall back to the provider config.
    let (api_key, base_url) = resolve_script_credentials(&app_type, provider, api_key, base_url);

    execute_and_format_usage_result(
        script_code,
        &api_key,
        &base_url,
        timeout,
        access_token,
        user_id,
        template_type,
    )
    .await
}

/// Validate UsageScript configuration (boundary checks)
pub(crate) fn validate_usage_script(script: &UsageScript) -> Result<(), AppError> {
    // Validate auto query interval (0-1440 minutes, max 24 hours)
    if let Some(interval) = script.auto_query_interval {
        if interval > 1440 {
            return Err(AppError::localized(
                "usage_script.interval_too_large",
                format!("自动查询间隔不能超过 1440 分钟（24小时），当前值: {interval}"),
                format!(
                    "Auto query interval cannot exceed 1440 minutes (24 hours), current: {interval}"
                ),
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{format_usage_data, resolve_script_credentials, user_facing_usage_error};
    use crate::app_config::AppType;
    use crate::error::AppError;
    use crate::provider::Provider;
    use serde_json::json;

    fn provider_with_settings(settings_config: serde_json::Value) -> Provider {
        Provider::with_id(
            "provider-1".to_string(),
            "Provider".to_string(),
            settings_config,
            None,
        )
    }

    #[test]
    fn localized_usage_errors_use_the_supported_english_fallback() {
        let error = AppError::localized(
            "usage_script.test",
            "Chinese internal fallback",
            "English user-facing error",
        );

        assert_eq!(user_facing_usage_error(error), "English user-facing error");
    }

    #[test]
    fn malformed_usage_data_returns_a_supported_english_error() {
        let result = format_usage_data(json!("not an object"));

        assert!(!result.success);
        assert!(result.data.is_none());
        let error = result.error.expect("format error");
        assert!(error.starts_with("Data format error:"));
        assert!(!error
            .chars()
            .any(|character| ('\u{4e00}'..='\u{9fff}').contains(&character)));
    }

    #[test]
    fn script_values_override_provider_credentials() {
        let provider = provider_with_settings(json!({
            "env": {
                "ANTHROPIC_AUTH_TOKEN": "provider-key",
                "ANTHROPIC_BASE_URL": "https://provider.example.com/"
            }
        }));

        let (api_key, base_url) = resolve_script_credentials(
            &AppType::Claude,
            &provider,
            Some(" script-key "),
            Some(" https://script.example.com/ "),
        );
        assert_eq!(api_key, "script-key");
        assert_eq!(base_url, "https://script.example.com");
    }

    #[test]
    fn empty_script_values_fall_back_to_provider_credentials() {
        let provider = provider_with_settings(json!({
            "env": {
                "ANTHROPIC_AUTH_TOKEN": "provider-key",
                "ANTHROPIC_BASE_URL": "https://provider.example.com/"
            }
        }));

        let (api_key, base_url) =
            resolve_script_credentials(&AppType::Claude, &provider, Some(""), None);
        assert_eq!(api_key, "provider-key");
        assert_eq!(base_url, "https://provider.example.com");
    }

    #[test]
    fn codex_fallback_reads_auth_and_config_toml() {
        let provider = provider_with_settings(json!({
            "auth": {
                "OPENAI_API_KEY": "openai-key"
            },
            "config": r#"model_provider = "azure"

[model_providers.azure]
base_url = "https://azure.example.com/v1/"

[model_providers.other]
base_url = "https://other.example.com/v1"
"#
        }));

        let (api_key, base_url) =
            resolve_script_credentials(&AppType::Codex, &provider, None, None);
        assert_eq!(api_key, "openai-key");
        assert_eq!(base_url, "https://azure.example.com/v1");
    }
}
