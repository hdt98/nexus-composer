/**
 * Global outbound proxy API.
 *
 * Provides operations to get, set, and test the global proxy.
 */

import { invoke } from "@tauri-apps/api/core";

/**
 * Proxy test result.
 */
export interface ProxyTestResult {
  success: boolean;
  latencyMs: number;
  error: string | null;
}

/**
 * Outbound proxy status.
 */
export interface UpstreamProxyStatus {
  enabled: boolean;
  proxyUrl: string | null;
}

/**
 * Detected proxy.
 */
export interface DetectedProxy {
  url: string;
  proxyType: string;
  port: number;
}

/**
 * Get the global proxy URL.
 *
 * @returns The proxy URL, or null when direct connection is configured.
 */
export async function getGlobalProxyUrl(): Promise<string | null> {
  return invoke<string | null>("get_global_proxy_url");
}

/**
 * Set the global proxy URL.
 *
 * @param url - Proxy URL such as http://127.0.0.1:7890 or socks5://127.0.0.1:1080.
 *              An empty string clears the proxy and uses a direct connection.
 */
export async function setGlobalProxyUrl(url: string): Promise<void> {
  try {
    return await invoke("set_global_proxy_url", { url });
  } catch (error) {
    // Tauri invoke errors may be strings.
    throw new Error(typeof error === "string" ? error : String(error));
  }
}

/**
 * Test a proxy connection.
 *
 * @param url - Proxy URL to test.
 * @returns The result with success, latency, and error information.
 */
export async function testProxyUrl(url: string): Promise<ProxyTestResult> {
  return invoke<ProxyTestResult>("test_proxy_url", { url });
}

/**
 * Get current outbound proxy status.
 *
 * @returns Proxy status with enabled state and URL.
 */
export async function getUpstreamProxyStatus(): Promise<UpstreamProxyStatus> {
  return invoke<UpstreamProxyStatus>("get_upstream_proxy_status");
}

/**
 * Scan for local proxies.
 *
 * @returns Detected proxies.
 */
export async function scanLocalProxies(): Promise<DetectedProxy[]> {
  return invoke<DetectedProxy[]>("scan_local_proxies");
}
