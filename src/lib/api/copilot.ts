/**
 * GitHub Copilot OAuth API
 *
 * API functions for the GitHub Copilot OAuth device-code flow.
 * Supports multiple accounts.
 */

import { invoke } from "@tauri-apps/api/core";

/**
 * GitHub device-code response
 */
export interface CopilotDeviceCodeResponse {
  device_code: string;
  user_code: string;
  verification_uri: string;
  expires_in: number;
  interval: number;
}

/**
 * Public GitHub account information
 */
export interface GitHubAccount {
  /** Unique GitHub user ID */
  id: string;
  /** GitHub username */
  login: string;
  /** Avatar URL */
  avatar_url: string | null;
  /** Authentication timestamp in Unix seconds */
  authenticated_at: number;
  /** GitHub domain: github.com or a GHES domain */
  github_domain: string;
}

/**
 * Multi-account Copilot authentication status
 */
export interface CopilotAuthStatus {
  /** Whether any account is authenticated; retained for backward compatibility */
  authenticated: boolean;
  /** Default account ID */
  default_account_id: string | null;
  /** Status message when legacy authentication-data migration fails */
  migration_error?: string | null;
  /** First account's username; retained for backward compatibility */
  username: string | null;
  /** Copilot token expiry; retained for backward compatibility */
  expires_at: number | null;
  /** All authenticated accounts */
  accounts: GitHubAccount[];
}

/**
 * Start the GitHub OAuth device-code flow.
 *
 * @returns A device-code response containing the user code and verification URL.
 */
export async function copilotStartDeviceFlow(): Promise<CopilotDeviceCodeResponse> {
  return invoke<CopilotDeviceCodeResponse>("copilot_start_device_flow");
}

/**
 * Poll for an OAuth token.
 *
 * Poll GitHub with the device code until the user completes authorization.
 *
 * @param deviceCode - Device code
 * @returns true when authenticated; false while awaiting user authorization.
 */
export async function copilotPollForAuth(deviceCode: string): Promise<boolean> {
  return invoke<boolean>("copilot_poll_for_auth", {
    deviceCode,
  });
}

/**
 * Get Copilot authentication status.
 *
 * @returns Status containing authentication state, username, and expiry.
 */
export async function copilotGetAuthStatus(): Promise<CopilotAuthStatus> {
  return invoke<CopilotAuthStatus>("copilot_get_auth_status");
}

/**
 * Sign out of Copilot.
 */
export async function copilotLogout(): Promise<void> {
  return invoke("copilot_logout");
}

/**
 * Check whether Copilot is authenticated.
 *
 * @returns true when authenticated.
 */
export async function copilotIsAuthenticated(): Promise<boolean> {
  return invoke<boolean>("copilot_is_authenticated");
}

/**
 * Available Copilot model
 */
export interface CopilotModel {
  id: string;
  name: string;
  vendor: string;
  model_picker_enabled: boolean;
}

/**
 * Get a valid Copilot token.
 *
 * Used internally for proxy requests.
 *
 * @returns Copilot Token
 */
export async function copilotGetToken(): Promise<string> {
  return invoke<string>("copilot_get_token");
}

/**
 * Get the available Copilot models.
 *
 * @returns Available models.
 */
export async function copilotGetModels(): Promise<CopilotModel[]> {
  return invoke<CopilotModel[]>("copilot_get_models");
}

/**
 * Quota details
 */
export interface QuotaDetail {
  entitlement: number;
  remaining: number;
  percent_remaining: number;
  unlimited: boolean;
}

/**
 * Quota snapshot
 */
export interface QuotaSnapshots {
  chat: QuotaDetail;
  completions: QuotaDetail;
  premium_interactions: QuotaDetail;
}

/**
 * Copilot usage response
 */
export interface CopilotUsageResponse {
  copilot_plan: string;
  quota_reset_date: string;
  quota_snapshots: QuotaSnapshots;
}

/**
 * Get Copilot usage information.
 *
 * @returns Usage details including plan type, reset date, and quota snapshots.
 */
export async function copilotGetUsage(): Promise<CopilotUsageResponse> {
  return invoke<CopilotUsageResponse>("copilot_get_usage");
}

// ==================== Multi-account management API ====================

/**
 * List all authenticated GitHub accounts.
 *
 * @returns Account list.
 */
export async function copilotListAccounts(): Promise<GitHubAccount[]> {
  return invoke<GitHubAccount[]>("copilot_list_accounts");
}

/**
 * Poll for an OAuth token in multi-account mode.
 *
 * Poll GitHub with the device code until the user completes authorization.
 * Return the newly added account after successful authorization.
 *
 * @param deviceCode - Device code
 * @returns The newly added account, or null while authorization is pending.
 */
export async function copilotPollForAccount(
  deviceCode: string,
): Promise<GitHubAccount | null> {
  return invoke<GitHubAccount | null>("copilot_poll_for_account", {
    deviceCode,
  });
}

/**
 * Remove a GitHub account.
 *
 * @param accountId - GitHub user ID
 */
export async function copilotRemoveAccount(accountId: string): Promise<void> {
  return invoke("copilot_remove_account", { accountId });
}

/**
 * Set the default GitHub account.
 *
 * @param accountId - GitHub user ID
 */
export async function copilotSetDefaultAccount(
  accountId: string,
): Promise<void> {
  return invoke("copilot_set_default_account", { accountId });
}

/**
 * Get a valid Copilot token for an account.
 *
 * Used internally for proxy requests.
 *
 * @param accountId - GitHub user ID
 * @returns Copilot Token
 */
export async function copilotGetTokenForAccount(
  accountId: string,
): Promise<string> {
  return invoke<string>("copilot_get_token_for_account", { accountId });
}

/**
 * Get available Copilot models for an account.
 *
 * @param accountId - GitHub user ID
 * @returns Available models.
 */
export async function copilotGetModelsForAccount(
  accountId: string,
): Promise<CopilotModel[]> {
  return invoke<CopilotModel[]>("copilot_get_models_for_account", {
    accountId,
  });
}

/**
 * Get Copilot usage information for an account.
 *
 * @param accountId - GitHub user ID
 * @returns Usage information.
 */
export async function copilotGetUsageForAccount(
  accountId: string,
): Promise<CopilotUsageResponse> {
  return invoke<CopilotUsageResponse>("copilot_get_usage_for_account", {
    accountId,
  });
}
