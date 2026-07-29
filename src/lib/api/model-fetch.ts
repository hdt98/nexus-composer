import { invoke } from "@tauri-apps/api/core";
import type { TFunction } from "i18next";
import { toast } from "sonner";

export interface FetchedModel {
  id: string;
  ownedBy: string | null;
}

/**
 * Fetches available models from a provider.
 *
 * Uses an OpenAI-compatible GET /v1/models endpoint. `modelsUrl` takes
 * precedence as an exact override; otherwise the backend generates candidates
 * from baseURL and tries them in order, including a fallback that strips
 * compatibility subpaths such as /anthropic.
 */
export async function fetchModelsForConfig(
  baseUrl: string,
  apiKey: string,
  isFullUrl?: boolean,
  modelsUrl?: string,
  customUserAgent?: string,
): Promise<FetchedModel[]> {
  return invoke("fetch_models_for_config", {
    baseUrl,
    apiKey,
    isFullUrl,
    modelsUrl,
    customUserAgent,
  });
}

/**
 * Fetches models available through Codex OAuth for ChatGPT Plus/Pro routing.
 *
 * Codex OAuth uses ChatGPT's backend-api/codex endpoint and is not compatible
 * with a standard /v1/models endpoint.
 */
export async function fetchCodexOauthModels(
  accountId?: string | null,
): Promise<FetchedModel[]> {
  return invoke("get_codex_oauth_models", {
    accountId: accountId || null,
  });
}

/**
 * Shows the appropriate toast for a model-fetch error.
 */
export function showFetchModelsError(
  err: unknown,
  t: TFunction,
  opts?: { hasApiKey: boolean; hasBaseUrl: boolean },
): void {
  // Frontend preflight for missing required fields.
  if (opts && !opts.hasBaseUrl && !opts.hasApiKey) {
    toast.error(t("providerForm.fetchModelsNeedConfig"));
    return;
  }
  if (opts && !opts.hasApiKey) {
    toast.error(t("providerForm.fetchModelsNeedApiKey"));
    return;
  }
  if (opts && !opts.hasBaseUrl) {
    toast.error(t("providerForm.fetchModelsNeedEndpoint"));
    return;
  }

  // Parse the backend error string.
  const msg = String(err);

  if (msg.includes("HTTP 401") || msg.includes("HTTP 403")) {
    toast.error(t("providerForm.fetchModelsAuthFailed"));
    return;
  }
  // All candidate endpoints returned 404/405, indicating an unavailable
  // /models endpoint or an incorrect Base URL.
  if (msg.includes("All candidates failed")) {
    toast.error(t("providerForm.fetchModelsEndpointNotFound"));
    return;
  }
  if (msg.includes("HTTP 404") || msg.includes("HTTP 405")) {
    toast.error(t("providerForm.fetchModelsEndpointNotFound"));
    return;
  }
  if (msg.includes("timeout") || msg.includes("timed out")) {
    toast.error(t("providerForm.fetchModelsTimeout"));
    return;
  }
  if (msg.includes("Failed to parse")) {
    toast.error(t("providerForm.fetchModelsNotSupported"));
    return;
  }

  // Generic fallback.
  toast.error(t("providerForm.fetchModelsFailed"));
}
