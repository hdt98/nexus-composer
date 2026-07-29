//! Global HTTP client module.
//!
//! Provides HTTP clients with global proxy support.
//! Modules that send HTTP requests should use clients from this module.

use once_cell::sync::OnceCell;
use reqwest::Client;
use std::env;
use std::net::IpAddr;
use std::sync::RwLock;
use std::time::Duration;

struct GlobalClients {
    general: Client,
    forwarding: Client,
}

/// Shared HTTP clients. Proxy forwarding has no client-level request deadline;
/// the forwarder applies the configured per-request timeout itself.
static GLOBAL_CLIENTS: OnceCell<RwLock<GlobalClients>> = OnceCell::new();

/// Current proxy URL for logging and status queries.
static CURRENT_PROXY_URL: OnceCell<RwLock<Option<String>>> = OnceCell::new();

/// Port currently used by the Nexus Composer proxy server.
static CC_SWITCH_PROXY_PORT: OnceCell<RwLock<u16>> = OnceCell::new();

/// Sets the Nexus Composer proxy server port.
///
/// Call when the proxy server starts so system proxy detection recognizes its port.
pub fn set_proxy_port(port: u16) {
    if let Some(lock) = CC_SWITCH_PROXY_PORT.get() {
        if let Ok(mut current_port) = lock.write() {
            *current_port = port;
            log::debug!("[GlobalProxy] Updated Nexus Composer proxy port to {port}");
        }
    } else {
        let _ = CC_SWITCH_PROXY_PORT.set(RwLock::new(port));
        log::debug!("[GlobalProxy] Initialized Nexus Composer proxy port to {port}");
    }
}

/// Returns the Nexus Composer proxy server port.
fn get_proxy_port() -> u16 {
    CC_SWITCH_PROXY_PORT
        .get()
        .and_then(|lock| lock.read().ok())
        .map(|port| *port)
        .unwrap_or(15721) // Fall back to the default port.
}

/// Initializes the global HTTP clients.
///
/// Call once during application startup.
///
/// # Arguments
/// * `proxy_url` - Proxy URL such as `http://127.0.0.1:7890` or
///   `socks5://127.0.0.1:1080`; `None` or an empty string selects a direct connection
pub fn init(proxy_url: Option<&str>) -> Result<(), String> {
    let effective_url = proxy_url.filter(|s| !s.trim().is_empty());
    let clients = build_clients(effective_url)?;

    // Initialize the global clients, or update them through `apply_proxy` if already set.
    if GLOBAL_CLIENTS.set(RwLock::new(clients)).is_err() {
        log::warn!(
            "[GlobalProxy] [GP-003] Already initialized, updating instead: {}",
            effective_url
                .map(mask_url)
                .unwrap_or_else(|| "direct connection".to_string())
        );
        // Already initialized; update through `apply_proxy`.
        return apply_proxy(proxy_url);
    }

    // Initialize the proxy URL record.
    let _ = CURRENT_PROXY_URL.set(RwLock::new(effective_url.map(|s| s.to_string())));

    log::info!(
        "[GlobalProxy] Initialized: {}",
        effective_url
            .map(mask_url)
            .unwrap_or_else(|| "direct connection".to_string())
    );

    Ok(())
}

/// Validates proxy configuration without applying it.
///
/// Validates the proxy URL without updating the global clients. Use before persisting
/// configuration.
///
/// # Arguments
/// * `proxy_url` - Proxy URL; `None` or an empty string selects a direct connection
///
/// # Returns
/// Returns `Ok(())` when valid, or an error message otherwise.
pub fn validate_proxy(proxy_url: Option<&str>) -> Result<(), String> {
    let effective_url = proxy_url.filter(|s| !s.trim().is_empty());
    // Build both clients to validate the complete runtime configuration.
    build_clients(effective_url)?;
    Ok(())
}

/// Applies previously validated proxy configuration.
///
/// Applies proxy configuration to the global clients without extra validation.
/// Call after `validate_proxy` succeeds.
///
/// # Arguments
/// * `proxy_url` - Proxy URL; `None` or an empty string selects a direct connection
pub fn apply_proxy(proxy_url: Option<&str>) -> Result<(), String> {
    let effective_url = proxy_url.filter(|s| !s.trim().is_empty());
    let new_clients = build_clients(effective_url)?;

    // Update the clients.
    if let Some(lock) = GLOBAL_CLIENTS.get() {
        let mut clients = lock.write().map_err(|e| {
            log::error!("[GlobalProxy] [GP-001] Failed to acquire write lock: {e}");
            "Failed to update proxy: lock poisoned".to_string()
        })?;
        *clients = new_clients;
    } else {
        // Initialize if needed.
        return init(proxy_url);
    }

    // Update the proxy URL record.
    if let Some(lock) = CURRENT_PROXY_URL.get() {
        let mut url = lock.write().map_err(|e| {
            log::error!("[GlobalProxy] [GP-002] Failed to acquire URL write lock: {e}");
            "Failed to update proxy URL record: lock poisoned".to_string()
        })?;
        *url = effective_url.map(|s| s.to_string());
    }

    log::info!(
        "[GlobalProxy] Applied: {}",
        effective_url
            .map(mask_url)
            .unwrap_or_else(|| "direct connection".to_string())
    );

    Ok(())
}

/// Updates proxy configuration at runtime.
///
/// Changes proxy settings without restarting the application. This function both
/// validates and applies them. To validate before persisting, use `validate_proxy`
/// followed by `apply_proxy`.
///
/// # Arguments
/// * `proxy_url` - New proxy URL; `None` or an empty string selects a direct connection
#[allow(dead_code)]
pub fn update_proxy(proxy_url: Option<&str>) -> Result<(), String> {
    let effective_url = proxy_url.filter(|s| !s.trim().is_empty());
    let new_clients = build_clients(effective_url)?;

    // Update the clients.
    if let Some(lock) = GLOBAL_CLIENTS.get() {
        let mut clients = lock.write().map_err(|e| {
            log::error!("[GlobalProxy] [GP-001] Failed to acquire write lock: {e}");
            "Failed to update proxy: lock poisoned".to_string()
        })?;
        *clients = new_clients;
    } else {
        // Initialize if needed.
        return init(proxy_url);
    }

    // Update the proxy URL record.
    if let Some(lock) = CURRENT_PROXY_URL.get() {
        let mut url = lock.write().map_err(|e| {
            log::error!("[GlobalProxy] [GP-002] Failed to acquire URL write lock: {e}");
            "Failed to update proxy URL record: lock poisoned".to_string()
        })?;
        *url = effective_url.map(|s| s.to_string());
    }

    log::info!(
        "[GlobalProxy] Updated: {}",
        effective_url
            .map(mask_url)
            .unwrap_or_else(|| "direct connection".to_string())
    );

    Ok(())
}

/// Returns the global HTTP client.
///
/// Uses the configured proxy when present, otherwise follows the system proxy.
pub fn get() -> Client {
    GLOBAL_CLIENTS
        .get()
        .and_then(|lock| lock.read().ok())
        .map(|clients| clients.general.clone())
        .unwrap_or_else(|| {
            log::warn!("[GlobalProxy] [GP-004] Client not initialized, using fallback");
            build_client(None).unwrap_or_default()
        })
}

/// Return the pooled client used by the local proxy forwarding path.
///
/// Unlike the general-purpose client, it has no built-in total request timeout.
/// RequestForwarder owns response-header, body, and stream-idle deadlines so a
/// configured zero can actually disable those deadlines.
pub fn get_for_forwarding() -> Client {
    GLOBAL_CLIENTS
        .get()
        .and_then(|lock| lock.read().ok())
        .map(|clients| clients.forwarding.clone())
        .unwrap_or_else(|| {
            log::warn!("[GlobalProxy] [GP-004] Client not initialized, using forwarding fallback");
            build_forwarding_client(None).unwrap_or_default()
        })
}

/// Returns the current proxy URL.
///
/// `None` indicates a direct connection.
pub fn get_current_proxy_url() -> Option<String> {
    CURRENT_PROXY_URL
        .get()
        .and_then(|lock| lock.read().ok())
        .and_then(|url| url.clone())
}

/// Returns whether a proxy is configured.
#[allow(dead_code)]
pub fn is_proxy_enabled() -> bool {
    get_current_proxy_url().is_some()
}

/// Builds an HTTP client.
fn build_client(proxy_url: Option<&str>) -> Result<Client, String> {
    build_client_with_timeout(proxy_url, Some(Duration::from_secs(600)))
}

fn build_forwarding_client(proxy_url: Option<&str>) -> Result<Client, String> {
    build_client_with_timeout(proxy_url, None)
}

fn build_clients(proxy_url: Option<&str>) -> Result<GlobalClients, String> {
    Ok(GlobalClients {
        general: build_client(proxy_url)?,
        forwarding: build_forwarding_client(proxy_url)?,
    })
}

fn build_client_with_timeout(
    proxy_url: Option<&str>,
    request_timeout: Option<Duration>,
) -> Result<Client, String> {
    let mut builder = Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(10)
        .tcp_keepalive(Duration::from_secs(60))
        // Disable automatic decompression so reqwest does not overwrite the client's original
        // `accept-encoding` header. `response_processor` handles the response according to
        // `content-encoding`.
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd();

    if let Some(timeout) = request_timeout {
        builder = builder.timeout(timeout);
    }

    // Use the explicit proxy when present; otherwise follow the system proxy.
    if let Some(url) = proxy_url {
        // Validate the URL format and scheme first.
        let parsed = url::Url::parse(url)
            .map_err(|e| format!("Invalid proxy URL '{}': {}", mask_url(url), e))?;

        let scheme = parsed.scheme();
        if !["http", "https", "socks5", "socks5h"].contains(&scheme) {
            return Err(format!(
                "Invalid proxy scheme '{}' in URL '{}'. Supported: http, https, socks5, socks5h",
                scheme,
                mask_url(url)
            ));
        }

        let proxy = reqwest::Proxy::all(url)
            .map_err(|e| format!("Invalid proxy URL '{}': {}", mask_url(url), e))?;
        builder = builder.proxy(proxy);
        log::debug!("[GlobalProxy] Proxy configured: {}", mask_url(url));
    } else {
        // Without an explicit proxy, let reqwest detect proxy environment variables.
        // Bypass a system proxy that points back to this server to avoid recursion.
        if system_proxy_points_to_loopback() {
            builder = builder.no_proxy();
            log::warn!(
                "[GlobalProxy] System proxy points to localhost, bypassing to avoid recursion"
            );
        } else {
            log::debug!("[GlobalProxy] Following system proxy (no explicit proxy configured)");
        }
    }

    builder
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {e}"))
}

fn system_proxy_points_to_loopback() -> bool {
    const KEYS: [&str; 6] = [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ];

    KEYS.iter()
        .filter_map(|key| env::var(key).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .any(|value| proxy_points_to_loopback(&value))
}

fn proxy_points_to_loopback(value: &str) -> bool {
    fn host_is_loopback(host: &str) -> bool {
        if host.eq_ignore_ascii_case("localhost") {
            return true;
        }
        host.parse::<IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
    }

    // Bypass only the Nexus Composer proxy port to avoid recursive forwarding.
    fn is_cc_switch_proxy_port(port: Option<u16>) -> bool {
        let cc_switch_port = get_proxy_port();
        port == Some(cc_switch_port)
    }

    if let Ok(parsed) = url::Url::parse(value) {
        if let Some(host) = parsed.host_str() {
            // Match only a loopback host using the Nexus Composer proxy port.
            return host_is_loopback(host) && is_cc_switch_proxy_port(parsed.port());
        }
        return false;
    }

    let with_scheme = format!("http://{value}");
    if let Ok(parsed) = url::Url::parse(&with_scheme) {
        if let Some(host) = parsed.host_str() {
            return host_is_loopback(host) && is_cc_switch_proxy_port(parsed.port());
        }
    }

    false
}

/// Masks sensitive URL data for logging.
pub fn mask_url(url: &str) -> String {
    if let Ok(parsed) = url::Url::parse(url) {
        // Hide credentials while retaining the scheme, host, and port.
        let host = parsed.host_str().unwrap_or("?");
        match parsed.port() {
            Some(port) => format!("{}://{}:{}", parsed.scheme(), host, port),
            None => format!("{}://{}", parsed.scheme(), host),
        }
    } else {
        // Return a truncated value when URL parsing fails.
        if url.len() > 20 {
            format!("{}...", &url[..20])
        } else {
            url.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn test_mask_url() {
        assert_eq!(mask_url("http://127.0.0.1:7890"), "http://127.0.0.1:7890");
        assert_eq!(
            mask_url("http://user:pass@127.0.0.1:7890"),
            "http://127.0.0.1:7890"
        );
        assert_eq!(
            mask_url("socks5://admin:secret@proxy.example.com:1080"),
            "socks5://proxy.example.com:1080"
        );
        // A URL without a port must not display ":?".
        assert_eq!(
            mask_url("http://proxy.example.com"),
            "http://proxy.example.com"
        );
        assert_eq!(
            mask_url("https://user:pass@proxy.example.com"),
            "https://proxy.example.com"
        );
    }

    #[test]
    fn test_build_client_direct() {
        let result = build_client(None);
        assert!(result.is_ok());
    }

    #[test]
    fn test_build_client_with_http_proxy() {
        let result = build_client(Some("http://127.0.0.1:7890"));
        assert!(result.is_ok());
    }

    #[test]
    fn test_build_client_with_socks5_proxy() {
        let result = build_client(Some("socks5://127.0.0.1:1080"));
        assert!(result.is_ok());
    }

    #[test]
    fn test_build_client_invalid_url() {
        // `reqwest::Proxy::all` does not reject every invalid URL immediately, so use an
        // explicitly invalid scheme.
        let result = build_client(Some("invalid-scheme://127.0.0.1:7890"));
        assert!(result.is_err(), "Should reject invalid proxy scheme");
    }

    #[test]
    fn test_proxy_points_to_loopback() {
        // Set the Nexus Composer proxy port to its default, 15721.
        set_proxy_port(15721);

        // Match loopback addresses only when they use the Nexus Composer port.
        assert!(proxy_points_to_loopback("http://127.0.0.1:15721"));
        assert!(proxy_points_to_loopback("socks5://localhost:15721"));
        assert!(proxy_points_to_loopback("127.0.0.1:15721"));

        // Allow other loopback ports for other local proxy tools.
        assert!(!proxy_points_to_loopback("http://127.0.0.1:7890"));
        assert!(!proxy_points_to_loopback("socks5://localhost:1080"));

        // Do not bypass non-loopback addresses.
        assert!(!proxy_points_to_loopback("http://192.168.1.10:7890"));
        assert!(!proxy_points_to_loopback("http://192.168.1.10:15721"));
    }

    #[test]
    fn test_system_proxy_points_to_loopback() {
        let _guard = env_lock().lock().unwrap();

        // Set the Nexus Composer proxy port.
        set_proxy_port(15721);

        let keys = [
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
        ];

        for key in &keys {
            std::env::remove_var(key);
        }

        // Bypass a proxy that points to the Nexus Composer port.
        std::env::set_var("HTTP_PROXY", "http://127.0.0.1:15721");
        assert!(system_proxy_points_to_loopback());

        // Do not bypass a local proxy on another port.
        std::env::set_var("HTTP_PROXY", "http://127.0.0.1:7890");
        assert!(!system_proxy_points_to_loopback());

        // Do not bypass non-loopback addresses.
        std::env::set_var("HTTP_PROXY", "http://10.0.0.2:7890");
        assert!(!system_proxy_points_to_loopback());

        for key in &keys {
            std::env::remove_var(key);
        }
    }
}
