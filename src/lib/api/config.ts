// Configuration API
import { invoke } from "@tauri-apps/api/core";

export type AppType = "claude" | "codex" | "gemini" | "omo" | "omo_slim";

/**
 * Get the Claude common configuration snippet. Deprecated in favor of getCommonConfigSnippet.
 * @returns The common configuration snippet as JSON, or null when absent.
 * @deprecated Use getCommonConfigSnippet('claude') instead.
 */
export async function getClaudeCommonConfigSnippet(): Promise<string | null> {
  return invoke<string | null>("get_claude_common_config_snippet");
}

/**
 * Set the Claude common configuration snippet. Deprecated in favor of setCommonConfigSnippet.
 * @param snippet - Common configuration snippet as JSON.
 * @throws If the JSON is invalid.
 * @deprecated Use setCommonConfigSnippet('claude', snippet) instead.
 */
export async function setClaudeCommonConfigSnippet(
  snippet: string,
): Promise<void> {
  return invoke("set_claude_common_config_snippet", { snippet });
}

/**
 * Get a common configuration snippet through the unified API.
 * @param appType - App type: claude, codex, or gemini.
 * @returns The common configuration snippet as a raw string, or null when absent.
 */
export async function getCommonConfigSnippet(
  appType: AppType,
): Promise<string | null> {
  return invoke<string | null>("get_common_config_snippet", { appType });
}

/**
 * Set a common configuration snippet through the unified API.
 * @param appType - App type: claude, codex, or gemini.
 * @param snippet - Common configuration snippet as a raw string.
 * @throws If the format is invalid. Claude and Gemini validate JSON; Codex currently does not validate.
 */
export async function setCommonConfigSnippet(
  appType: AppType,
  snippet: string,
): Promise<void> {
  return invoke("set_common_config_snippet", { appType, snippet });
}

/**
 * Extract a common configuration snippet.
 *
 * By default, read the active provider's configuration. When `options.settingsConfig`
 * is supplied, extract from the editor's current content. Provider-specific fields
 * such as API keys, model configuration, and endpoints are excluded automatically.
 *
 * @param appType - App type: claude, codex, or gemini.
 * @param options - Optional extraction source.
 * @returns The extracted reusable snippet as a JSON or TOML string.
 */
export type ExtractCommonConfigSnippetOptions = {
  settingsConfig?: string;
};

export async function extractCommonConfigSnippet(
  appType: Exclude<AppType, "omo">,
  options?: ExtractCommonConfigSnippetOptions,
): Promise<string> {
  const args: Record<string, unknown> = { appType };
  const settingsConfig = options?.settingsConfig;

  if (typeof settingsConfig === "string" && settingsConfig.trim()) {
    args.settingsConfig = settingsConfig;
  }

  return invoke<string>("extract_common_config_snippet", args);
}
