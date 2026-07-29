/**
 * Routing table for Coding Plan provider base URLs.
 *
 * Keep this aligned with
 * `src-tauri/src/services/coding_plan.rs::detect_provider`: the backend uses
 * `url.contains(...)` substring checks, while the frontend uses equivalent
 * regular expressions. Add providers here so UsageScriptModal options,
 * useProviderActions auto-injection, and tray detection can all reuse them.
 */
import { createUsageScript } from "@/types";
import { TEMPLATE_TYPES } from "@/config/constants";

export interface CodingPlanProviderEntry {
  /** Matches the backend QuotaTier `codingPlanProvider` value. */
  id: "kimi" | "zhipu" | "minimax" | "zenmux" | "volcengine";
  /** Display label for the UsageScriptModal dropdown. */
  label: string;
  /** Rule for matching a base URL. */
  pattern: RegExp;
}

export const CODING_PLAN_PROVIDERS: readonly CodingPlanProviderEntry[] = [
  { id: "kimi", label: "Kimi For Coding", pattern: /api\.kimi\.com\/coding/i },
  {
    id: "zhipu",
    label: "Zhipu GLM",
    pattern: /bigmodel\.cn|api\.z\.ai/i,
  },
  {
    id: "minimax",
    label: "MiniMax",
    pattern: /api\.minimaxi?\.com|api\.minimax\.io/i,
  },
  {
    id: "zenmux",
    label: "ZenMux",
    pattern: /zenmux\./i,
  },
  {
    // Volcengine Ark Agent Plan / Coding Plan. Its base URL resembles
    // ark.cn-beijing.volces.com/api/coding[/v3], which is equivalent to the
    // backend detect_provider substring check for `volces.com/api/coding`.
    id: "volcengine",
    label: "Volcengine Ark",
    pattern: /volces\.com\/api\/coding/i,
  },
] as const;

/** Detects a Coding Plan provider by base URL, or returns null if none match. */
export function detectCodingPlanProvider(
  baseUrl: string | undefined | null,
): CodingPlanProviderEntry["id"] | null {
  if (!baseUrl) return null;
  for (const cp of CODING_PLAN_PROVIDERS) {
    if (cp.pattern.test(baseUrl)) return cp.id;
  }
  return null;
}

/**
 * When creating a Claude provider whose `ANTHROPIC_BASE_URL` matches the
 * Coding Plan routing table, automatically enables `meta.usage_script` and
 * marks it as token_plan.
 *
 * - Inject only when `meta.usage_script` is entirely absent; never overwrite
 *   existing user or UsageScriptModal configuration.
 * - Apply only to the Claude app: the token_plan branch in backend
 *   `commands/provider.rs` handles only a Claude provider's
 *   `settings_config.env.ANTHROPIC_BASE_URL`.
 * - Leave code empty so Rust uses the dedicated
 *   `coding_plan::get_coding_plan_quota` path instead of executing JavaScript.
 */
export function injectCodingPlanUsageScript<
  T extends {
    settingsConfig?: Record<string, any>;
    meta?: Record<string, any>;
  },
>(appId: string, provider: T): T {
  if (appId !== "claude") return provider;
  if (provider.meta?.usage_script) return provider;

  const baseUrl = provider.settingsConfig?.env?.ANTHROPIC_BASE_URL;
  const codingPlanProvider = detectCodingPlanProvider(
    typeof baseUrl === "string" ? baseUrl : null,
  );
  if (!codingPlanProvider) return provider;

  return {
    ...provider,
    meta: {
      ...(provider.meta ?? {}),
      usage_script: createUsageScript({
        enabled: true,
        templateType: TEMPLATE_TYPES.TOKEN_PLAN,
        codingPlanProvider,
      }),
    },
  };
}
