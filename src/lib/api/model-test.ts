import { invoke } from "@tauri-apps/api/core";
import type { AppId } from "./types";

// ===== Connectivity Check Types =====
// This check only probes whether base_url is reachable. It does not send a real
// model request or affect failover circuit breakers.

export type HealthStatus = "operational" | "degraded" | "failed";

export interface StreamCheckConfig {
  /** Timeout for each probe, in seconds. */
  timeoutSecs: number;
  /** Maximum retries for timeout failures. */
  maxRetries: number;
  /** Degraded threshold in milliseconds; reachable responses with a longer TTFB are slow. */
  degradedThresholdMs: number;
}

export interface StreamCheckResult {
  status: HealthStatus;
  success: boolean;
  message: string;
  responseTimeMs?: number;
  httpStatus?: number;
  testedAt: number;
  retryCount: number;
}

// ===== Connectivity Check API =====

/**
 * Checks connectivity for one provider.
 */
export async function streamCheckProvider(
  appType: AppId,
  providerId: string,
): Promise<StreamCheckResult> {
  return invoke("stream_check_provider", { appType, providerId });
}

/**
 * Checks provider health in a streaming batch.
 */
export async function streamCheckAllProviders(
  appType: AppId,
  proxyTargetsOnly: boolean = false,
): Promise<Array<[string, StreamCheckResult]>> {
  return invoke("stream_check_all_providers", { appType, proxyTargetsOnly });
}

/**
 * Gets stream-check configuration.
 */
export async function getStreamCheckConfig(): Promise<StreamCheckConfig> {
  return invoke("get_stream_check_config");
}

/**
 * Saves stream-check configuration.
 */
export async function saveStreamCheckConfig(
  config: StreamCheckConfig,
): Promise<void> {
  return invoke("save_stream_check_config", { config });
}
