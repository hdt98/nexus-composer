use rquickjs::{Context, Function, Runtime};
use serde_json::Value;
use std::collections::HashMap;
use url::{Host, Url};

use crate::error::AppError;

/// Executes a usage-query script.
pub async fn execute_usage_script(
    script_code: &str,
    api_key: &str,
    base_url: &str,
    timeout_secs: u64,
    access_token: Option<&str>,
    user_id: Option<&str>,
    template_type: Option<&str>,
) -> Result<Value, AppError> {
    // Detect custom-template mode, preferring template_type supplied by the renderer.
    let is_custom_template = template_type.map(|t| t == "custom").unwrap_or(false);

    // 1. Substitute template variables without exposing sensitive data.
    let script_with_vars =
        build_script_with_vars(script_code, api_key, base_url, access_token, user_id);

    // 2. Validate base_url security when provided. In custom-template mode, the user may write a
    // complete URL directly in the script without template variables.
    if should_validate_base_url(base_url, is_custom_template) {
        validate_base_url(base_url)?;
    }

    // 3. Extract request configuration in a separate scope so Runtime and Context are released
    // before await.
    let request_config = {
        let runtime = Runtime::new().map_err(|e| {
            AppError::localized(
                "usage_script.runtime_create_failed",
                format!("Failed to create the JavaScript runtime: {e}"),
                format!("Failed to create JS runtime: {e}"),
            )
        })?;
        let context = Context::full(&runtime).map_err(|e| {
            AppError::localized(
                "usage_script.context_create_failed",
                format!("Failed to create the JavaScript context: {e}"),
                format!("Failed to create JS context: {e}"),
            )
        })?;

        context.with(|ctx| {
            // Execute user code and obtain the configuration object.
            let config: rquickjs::Object = ctx.eval(script_with_vars.clone()).map_err(|e| {
                AppError::localized(
                    "usage_script.config_parse_failed",
                    format!("Failed to evaluate configuration: {e}"),
                    format!("Failed to parse config: {e}"),
                )
            })?;

            // Extract request configuration.
            let request: rquickjs::Object = config.get("request").map_err(|e| {
                AppError::localized(
                    "usage_script.request_missing",
                    format!("Missing request configuration: {e}"),
                    format!("Missing request config: {e}"),
                )
            })?;

            // Convert request to a JSON string.
            let request_json: String = ctx
                .json_stringify(request)
                .map_err(|e| {
                    AppError::localized(
                        "usage_script.request_serialize_failed",
                        format!("Failed to serialize request: {e}"),
                        format!("Failed to serialize request: {e}"),
                    )
                })?
                .ok_or_else(|| {
                    AppError::localized(
                        "usage_script.serialize_none",
                        "Serialization returned None",
                        "Serialization returned None",
                    )
                })?
                .get()
                .map_err(|e| {
                    AppError::localized(
                        "usage_script.get_string_failed",
                        format!("Failed to read the string: {e}"),
                        format!("Failed to get string: {e}"),
                    )
                })?;

            Ok::<_, AppError>(request_json)
        })?
    }; // Runtime and Context are dropped here.

    // 4. Parse request configuration.
    let request: RequestConfig = serde_json::from_str(&request_config).map_err(|e| {
        AppError::localized(
            "usage_script.request_format_invalid",
            format!("Invalid request configuration format: {e}"),
            format!("Invalid request config format: {e}"),
        )
    })?;

    // 5. Validate the request URL by enforcing HTTPS and same-origin rules.
    validate_request_url(&request.url, base_url, is_custom_template)?;

    // 6. Send the HTTP request.
    let response_data = send_http_request(&request, timeout_secs).await?;

    // 7. Execute the extractor in a separate scope so Runtime and Context are released before return.
    let result: Value = {
        let runtime = Runtime::new().map_err(|e| {
            AppError::localized(
                "usage_script.runtime_create_failed",
                format!("Failed to create the JavaScript runtime: {e}"),
                format!("Failed to create JS runtime: {e}"),
            )
        })?;
        let context = Context::full(&runtime).map_err(|e| {
            AppError::localized(
                "usage_script.context_create_failed",
                format!("Failed to create the JavaScript context: {e}"),
                format!("Failed to create JS context: {e}"),
            )
        })?;

        context.with(|ctx| {
            // Evaluate the script again to obtain the configuration object.
            let config: rquickjs::Object = ctx.eval(script_with_vars.clone()).map_err(|e| {
                AppError::localized(
                    "usage_script.config_reparse_failed",
                    format!("Failed to evaluate configuration again: {e}"),
                    format!("Failed to re-parse config: {e}"),
                )
            })?;

            // Extract the extractor function.
            let extractor: Function = config.get("extractor").map_err(|e| {
                AppError::localized(
                    "usage_script.extractor_missing",
                    format!("Missing extractor function: {e}"),
                    format!("Missing extractor function: {e}"),
                )
            })?;

            // Convert response data to a JavaScript value.
            let response_js: rquickjs::Value =
                ctx.json_parse(response_data.as_str()).map_err(|e| {
                    AppError::localized(
                        "usage_script.response_parse_failed",
                        format!("Failed to parse response JSON: {e}"),
                        format!("Failed to parse response JSON: {e}"),
                    )
                })?;

            // Call extractor(response).
            let result_js: rquickjs::Value = extractor.call((response_js,)).map_err(|e| {
                AppError::localized(
                    "usage_script.extractor_exec_failed",
                    format!("Failed to execute extractor: {e}"),
                    format!("Failed to execute extractor: {e}"),
                )
            })?;

            // Convert the result to a JSON string.
            let result_json: String = ctx
                .json_stringify(result_js)
                .map_err(|e| {
                    AppError::localized(
                        "usage_script.result_serialize_failed",
                        format!("Failed to serialize the result: {e}"),
                        format!("Failed to serialize result: {e}"),
                    )
                })?
                .ok_or_else(|| {
                    AppError::localized(
                        "usage_script.serialize_none",
                        "Serialization returned None",
                        "Serialization returned None",
                    )
                })?
                .get()
                .map_err(|e| {
                    AppError::localized(
                        "usage_script.get_string_failed",
                        format!("Failed to read the string: {e}"),
                        format!("Failed to get string: {e}"),
                    )
                })?;

            // Parse as serde_json::Value.
            serde_json::from_str(&result_json).map_err(|e| {
                AppError::localized(
                    "usage_script.json_parse_failed",
                    format!("Failed to parse JSON: {e}"),
                    format!("JSON parse failed: {e}"),
                )
            })
        })?
    }; // Runtime and Context are dropped here.

    // 8. Validate the return-value format.
    validate_result(&result)?;

    Ok(result)
}

/// Request configuration structure.
#[derive(Debug, serde::Deserialize)]
struct RequestConfig {
    url: String,
    method: String,
    #[serde(default)]
    headers: HashMap<String, String>,
    #[serde(default)]
    body: Option<String>,
}

/// Sends an HTTP request.
async fn send_http_request(config: &RequestConfig, timeout_secs: u64) -> Result<String, AppError> {
    // Use the global HTTP client, which already includes proxy configuration.
    let client = crate::proxy::http_client::get();
    // Clamp timeout to 2–30 seconds so invalid configuration cannot block for too long.
    let request_timeout = std::time::Duration::from_secs(timeout_secs.clamp(2, 30));

    // Validate the HTTP method strictly; do not fall back to GET for invalid values.
    let method: reqwest::Method = config.method.parse().map_err(|_| {
        AppError::localized(
            "usage_script.invalid_http_method",
            format!("Unsupported HTTP method: {}", config.method),
            format!("Unsupported HTTP method: {}", config.method),
        )
    })?;

    let mut req = client
        .request(method.clone(), &config.url)
        .timeout(request_timeout);

    // Add request headers.
    for (k, v) in &config.headers {
        req = req.header(k, v);
    }

    // Add the request body.
    if let Some(body) = &config.body {
        req = req.body(body.clone());
    }

    // Send the request.
    let resp = req.send().await.map_err(|e| {
        AppError::localized(
            "usage_script.request_failed",
            format!("Request failed: {e}"),
            format!("Request failed: {e}"),
        )
    })?;

    let status = resp.status();
    let text = resp.text().await.map_err(|e| {
        AppError::localized(
            "usage_script.read_response_failed",
            format!("Failed to read the response: {e}"),
            format!("Failed to read response: {e}"),
        )
    })?;

    if !status.is_success() {
        let preview = if text.len() > 200 {
            let mut safe_cut = 200usize;
            while !text.is_char_boundary(safe_cut) {
                safe_cut = safe_cut.saturating_sub(1);
            }
            format!("{}...", &text[..safe_cut])
        } else {
            text.clone()
        };
        return Err(AppError::localized(
            "usage_script.http_error",
            format!("HTTP {status} : {preview}"),
            format!("HTTP {status} : {preview}"),
        ));
    }

    Ok(text)
}

/// Validates a script return value, accepting one object or an array.
fn validate_result(result: &Value) -> Result<(), AppError> {
    // Validate every element when the result is an array.
    if let Some(arr) = result.as_array() {
        if arr.is_empty() {
            return Err(AppError::localized(
                "usage_script.empty_array",
                "The array returned by the script cannot be empty",
                "Script returned empty array",
            ));
        }
        for (idx, item) in arr.iter().enumerate() {
            validate_single_usage(item).map_err(|e| {
                AppError::localized(
                    "usage_script.array_validation_failed",
                    format!("Validation failed at array index [{idx}]: {e}"),
                    format!("Validation failed at index [{idx}]: {e}"),
                )
            })?;
        }
        return Ok(());
    }

    // Validate a single object directly for backward compatibility.
    validate_single_usage(result)
}

/// Validates one usage-data object.
fn validate_single_usage(result: &Value) -> Result<(), AppError> {
    let obj = result.as_object().ok_or_else(|| {
        AppError::localized(
            "usage_script.must_return_object",
            "The script must return an object or an array of objects",
            "Script must return object or array of objects",
        )
    })?;

    // Every field is optional, so check only its type when present.
    if obj.contains_key("isValid")
        && !result["isValid"].is_null()
        && !result["isValid"].is_boolean()
    {
        return Err(AppError::localized(
            "usage_script.isvalid_type_error",
            "isValid must be a boolean or null",
            "isValid must be boolean or null",
        ));
    }
    if obj.contains_key("invalidMessage")
        && !result["invalidMessage"].is_null()
        && !result["invalidMessage"].is_string()
    {
        return Err(AppError::localized(
            "usage_script.invalidmessage_type_error",
            "invalidMessage must be a string or null",
            "invalidMessage must be string or null",
        ));
    }
    if obj.contains_key("remaining")
        && !result["remaining"].is_null()
        && !result["remaining"].is_number()
    {
        return Err(AppError::localized(
            "usage_script.remaining_type_error",
            "remaining must be a number or null",
            "remaining must be number or null",
        ));
    }
    if obj.contains_key("unit") && !result["unit"].is_null() && !result["unit"].is_string() {
        return Err(AppError::localized(
            "usage_script.unit_type_error",
            "unit must be a string or null",
            "unit must be string or null",
        ));
    }
    if obj.contains_key("total") && !result["total"].is_null() && !result["total"].is_number() {
        return Err(AppError::localized(
            "usage_script.total_type_error",
            "total must be a number or null",
            "total must be number or null",
        ));
    }
    if obj.contains_key("used") && !result["used"].is_null() && !result["used"].is_number() {
        return Err(AppError::localized(
            "usage_script.used_type_error",
            "used must be a number or null",
            "used must be number or null",
        ));
    }
    if obj.contains_key("planName")
        && !result["planName"].is_null()
        && !result["planName"].is_string()
    {
        return Err(AppError::localized(
            "usage_script.planname_type_error",
            "planName must be a string or null",
            "planName must be string or null",
        ));
    }
    if obj.contains_key("extra") && !result["extra"].is_null() && !result["extra"].is_string() {
        return Err(AppError::localized(
            "usage_script.extra_type_error",
            "extra must be a string or null",
            "extra must be string or null",
        ));
    }

    Ok(())
}

/// Builds a script with substituted variables while preserving legacy-script compatibility.
fn build_script_with_vars(
    script_code: &str,
    api_key: &str,
    base_url: &str,
    access_token: Option<&str>,
    user_id: Option<&str>,
) -> String {
    let mut replaced = script_code
        .replace("{{apiKey}}", api_key)
        .replace("{{baseUrl}}", base_url);

    if let Some(token) = access_token {
        replaced = replaced.replace("{{accessToken}}", token);
    }
    if let Some(uid) = user_id {
        replaced = replaced.replace("{{userId}}", uid);
    }

    replaced
}

/// Performs basic security validation for base_url.
fn validate_base_url(base_url: &str) -> Result<(), AppError> {
    if base_url.is_empty() {
        return Err(AppError::localized(
            "usage_script.base_url_empty",
            "base_url cannot be empty",
            "base_url cannot be empty",
        ));
    }

    // Parse the URL.
    let parsed_url = Url::parse(base_url).map_err(|e| {
        AppError::localized(
            "usage_script.base_url_invalid",
            format!("Invalid base_url: {e}"),
            format!("Invalid base_url: {e}"),
        )
    })?;

    let is_loopback = is_loopback_host(&parsed_url);

    // Require HTTPS, allowing localhost for development.
    if parsed_url.scheme() != "https" && !is_loopback {
        return Err(AppError::localized(
            "usage_script.base_url_https_required",
            "base_url must use HTTPS, except for localhost",
            "base_url must use HTTPS (localhost allowed)",
        ));
    }

    // Validate the hostname format.
    let hostname = parsed_url.host_str().ok_or_else(|| {
        AppError::localized(
            "usage_script.base_url_hostname_missing",
            "base_url must contain a valid hostname",
            "base_url must include a valid hostname",
        )
    })?;

    // Perform a basic hostname-format check.
    if hostname.is_empty() {
        return Err(AppError::localized(
            "usage_script.base_url_hostname_empty",
            "The base_url hostname cannot be empty",
            "base_url hostname cannot be empty",
        ));
    }

    Ok(())
}

fn should_validate_base_url(base_url: &str, is_custom_template: bool) -> bool {
    !base_url.is_empty() && !is_custom_template
}

/// Validates request-URL security by enforcing HTTPS and same-origin rules.
fn validate_request_url(
    request_url: &str,
    base_url: &str,
    is_custom_template: bool,
) -> Result<(), AppError> {
    // Parse the request URL.
    let parsed_request = Url::parse(request_url).map_err(|e| {
        AppError::localized(
            "usage_script.request_url_invalid",
            format!("Invalid request URL: {e}"),
            format!("Invalid request URL: {e}"),
        )
    })?;

    let is_request_loopback = is_loopback_host(&parsed_request);

    // Require HTTPS, allowing localhost for development. Custom-template mode lets users choose
    // HTTP at their own security risk.
    if !is_custom_template && parsed_request.scheme() != "https" && !is_request_loopback {
        return Err(AppError::localized(
            "usage_script.request_https_required",
            "The request URL must use HTTPS, except for localhost",
            "Request URL must use HTTPS (localhost allowed)",
        ));
    }

    // Perform a same-origin check when a non-empty base_url is provided. Custom-template mode may
    // access any HTTPS domain and skips this check.
    if !base_url.is_empty() && !is_custom_template {
        // Parse the base URL.
        let parsed_base = Url::parse(base_url).map_err(|e| {
            AppError::localized(
                "usage_script.base_url_invalid",
                format!("Invalid base_url: {e}"),
                format!("Invalid base_url: {e}"),
            )
        })?;

        // Core security check: require the same host and port as base_url.
        if parsed_request.host_str() != parsed_base.host_str() {
            return Err(AppError::localized(
                "usage_script.request_host_mismatch",
                format!(
                    "Request host {} does not match base_url host {}; requests must be same-origin",
                    parsed_request.host_str().unwrap_or("unknown"),
                    parsed_base.host_str().unwrap_or("unknown")
                ),
                format!(
                    "Request host {} must match base_url host {} (same-origin required)",
                    parsed_request.host_str().unwrap_or("unknown"),
                    parsed_base.host_str().unwrap_or("unknown")
                ),
            ));
        }

        // Check ports while accounting for defaults. port_or_known_default() resolves HTTP to 80
        // and HTTPS to 443 automatically.
        match (
            parsed_request.port_or_known_default(),
            parsed_base.port_or_known_default(),
        ) {
            (Some(request_port), Some(base_port)) if request_port == base_port => {
                // Ports match; continue.
            }
            (Some(request_port), Some(base_port)) => {
                return Err(AppError::localized(
                    "usage_script.request_port_mismatch",
                    format!("Request port {request_port} must match base_url port {base_port}"),
                    format!("Request port {request_port} must match base_url port {base_port}"),
                ));
            }
            _ => {
                // This should be unreachable because port_or_known_default() should always return Some.
                return Err(AppError::localized(
                    "usage_script.request_port_unknown",
                    "Cannot determine the port number",
                    "Unable to determine port number",
                ));
            }
        }
    }

    Ok(())
}

/// Returns whether a URL points to localhost or a loopback address.
fn is_loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_https_bypass_prevention() {
        // Reject HTTP for non-local hosts.
        let result = validate_base_url("http://127.0.0.1.evil.com/api");
        assert!(
            result.is_err(),
            "Should reject HTTP for non-localhost domains"
        );
    }

    #[test]
    fn test_custom_template_allows_http_lan_request_with_different_base_url() {
        assert!(
            !should_validate_base_url("http://10.37.192.156:8090/anthropic", true),
            "Custom scripts should not validate an unused provider base_url fallback"
        );

        let result = validate_request_url(
            "http://10.37.192.156:18344/user/balance",
            "http://10.37.192.156:8090/anthropic",
            true,
        );
        assert!(
            result.is_ok(),
            "Custom usage scripts should be able to call an explicit HTTP quota endpoint"
        );
    }

    #[test]
    fn test_port_comparison() {
        // Verify that port comparison handles default and explicit ports correctly.

        // Cases: (base_url, request_url, should_match).
        let test_cases = vec![
            // HTTPS default-port cases.
            (
                "https://api.example.com",
                "https://api.example.com/v1/test",
                true,
            ),
            (
                "https://api.example.com",
                "https://api.example.com:443/v1/test",
                true,
            ),
            (
                "https://api.example.com:443",
                "https://api.example.com/v1/test",
                true,
            ),
            (
                "https://api.example.com:443",
                "https://api.example.com:443/v1/test",
                true,
            ),
            // Port-mismatch cases.
            (
                "https://api.example.com",
                "https://api.example.com:8443/v1/test",
                false,
            ),
            (
                "https://api.example.com:443",
                "https://api.example.com:8443/v1/test",
                false,
            ),
        ];

        for (base_url, request_url, should_match) in test_cases {
            let result = validate_request_url(request_url, base_url, false);

            if should_match {
                assert!(
                    result.is_ok(),
                    "A matching URL was rejected: base_url={}, request_url={}, error={}",
                    base_url,
                    request_url,
                    result.unwrap_err()
                );
            } else {
                assert!(
                    result.is_err(),
                    "A mismatched URL was accepted: base_url={}, request_url={}",
                    base_url,
                    request_url
                );
            }
        }
    }
}
