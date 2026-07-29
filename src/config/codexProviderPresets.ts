/**
 * Codex provider preset configuration templates.
 */
import { ProviderCategory } from "../types";
import type {
  CodexApiFormat,
  CodexCatalogModel,
  CodexChatReasoning,
  LocalProxyRequestOverrides,
} from "../types";
import type { PresetTheme } from "./claudeProviderPresets";
import {
  NEXUS_AUTO_COMPACT_TOKENS,
  NEXUS_CONTEXT_WINDOW,
  NEXUS_ENDPOINT,
  NEXUS_MANAGED_PRESET_VERSION,
  NEXUS_MODEL,
  NEXUS_REQUEST_OVERRIDES,
  NEXUS_KIMI_ENDPOINT,
  NEXUS_KIMI_MODEL,
} from "./nexus";

export interface CodexProviderPreset {
  name: string;
  nameKey?: string; // i18n key for localized display name
  websiteUrl: string;
  // Third-party providers may expose a separate API key URL.
  apiKeyUrl?: string;
  auth: Record<string, any>; // Written to ~/.codex/auth.json.
  config: string; // TOML string written to ~/.codex/config.toml.
  isOfficial?: boolean; // Whether this is an official preset.
  isPartner?: boolean; // Whether this is a commercial partner.
  primePartner?: boolean; // Featured top-tier partner whose badge uses a heart.
  partnerPromotionKey?: string; // i18n key for partner promotional text.
  category?: ProviderCategory; // Provider category.
  isCustomTemplate?: boolean; // Whether this is a custom template.
  // Candidate request endpoints used for endpoint management and speed tests.
  endpointCandidates?: string[];
  // Visual theme configuration.
  theme?: PresetTheme;
  // Icon configuration.
  icon?: string; // Icon name.
  iconColor?: string; // Icon color.
  // Codex API format.
  apiFormat?: CodexApiFormat;
  // Model catalog for Codex Chat local routing mode.
  modelCatalog?: CodexCatalogModel[];
  // Codex Responses -> Chat Completions reasoning capability defaults
  codexChatReasoning?: CodexChatReasoning;
  localProxyRequestOverrides?: LocalProxyRequestOverrides;
  providerType?: "nexus";
  managedNexusPresetVersion?: number;
  apiKeyOptional?: boolean;
}

/**
 * Generate auth.json for a third-party provider.
 */
export function generateThirdPartyAuth(apiKey: string): Record<string, any> {
  return {
    OPENAI_API_KEY: apiKey || "",
  };
}

/**
 * Generate config.toml for a third-party provider.
 */
export function generateThirdPartyConfig(
  providerName: string,
  baseUrl: string,
  modelName = "gpt-5.5",
  managed?: {
    contextWindow: number;
    autoCompactTokenLimit: number;
    streamIdleTimeoutMs: number;
  },
  requiresOpenAiAuth = true,
): string {
  const tomlString = (value: string) => JSON.stringify(value);
  const modelSettings = managed
    ? `model_context_window = ${managed.contextWindow}\nmodel_auto_compact_token_limit = ${managed.autoCompactTokenLimit}\n`
    : `model_reasoning_effort = "high"\n`;
  const streamTimeout = managed
    ? `\nstream_idle_timeout_ms = ${managed.streamIdleTimeoutMs}`
    : "";

  return `model_provider = "custom"
model = ${tomlString(modelName)}
${modelSettings}disable_response_storage = true

[model_providers.custom]
name = ${tomlString(providerName)}
base_url = ${tomlString(baseUrl)}
wire_api = "responses"
requires_openai_auth = ${requiresOpenAiAuth}${streamTimeout}`;
}

function modelCatalog(
  models: Array<
    | string
    | {
        model: string;
        displayName?: string;
        contextWindow?: number;
        // Native Responses (direct) overrides for the generated
        // model-catalogs.json; omit to inherit the native template defaults
        // (supports_parallel_tool_calls=false, input_modalities=["text"]).
        supportsParallelToolCalls?: boolean;
        inputModalities?: string[];
        // Vendor's OFFICIAL base_instructions; omit to inherit the neutral
        // template default. Required by Codex, so the backend always emits one.
        baseInstructions?: string;
      }
  >,
): CodexCatalogModel[] {
  return models.map((entry) =>
    typeof entry === "string"
      ? { model: entry }
      : {
          model: entry.model,
          displayName: entry.displayName,
          contextWindow: entry.contextWindow,
          supportsParallelToolCalls: entry.supportsParallelToolCalls,
          inputModalities: entry.inputModalities,
          baseInstructions: entry.baseInstructions,
        },
  );
}

export const codexProviderPresets: CodexProviderPreset[] = [
  // Nexus routes Codex Responses through the built-in Chat adapter.
  {
    name: "Nexus GLM-5.2",
    nameKey: "providerForm.presets.nexus",
    websiteUrl: NEXUS_ENDPOINT,
    auth: generateThirdPartyAuth(""),
    config: generateThirdPartyConfig(
      "Nexus GLM-5.2",
      NEXUS_ENDPOINT,
      NEXUS_MODEL,
      {
        contextWindow: NEXUS_CONTEXT_WINDOW,
        autoCompactTokenLimit: NEXUS_AUTO_COMPACT_TOKENS,
        streamIdleTimeoutMs: 3_000_000,
      },
    ),
    endpointCandidates: [NEXUS_ENDPOINT],
    apiFormat: "openai_chat",
    providerType: "nexus",
    managedNexusPresetVersion: NEXUS_MANAGED_PRESET_VERSION,
    localProxyRequestOverrides: NEXUS_REQUEST_OVERRIDES,
    modelCatalog: modelCatalog([
      {
        model: NEXUS_MODEL,
        displayName: "GLM-5.2",
        contextWindow: NEXUS_CONTEXT_WINDOW,
        inputModalities: ["text"],
      },
    ]),
    category: "third_party",
    icon: "nexus",
    iconColor: "#6366F1",
  },
  {
    name: "Nexus Kimi-K3",
    websiteUrl: NEXUS_KIMI_ENDPOINT,
    auth: generateThirdPartyAuth(""),
    config: generateThirdPartyConfig(
      "Nexus Kimi-K3",
      NEXUS_KIMI_ENDPOINT,
      NEXUS_KIMI_MODEL,
      undefined,
      false,
    ),
    endpointCandidates: [NEXUS_KIMI_ENDPOINT],
    apiFormat: "openai_chat",
    apiKeyOptional: true,
    modelCatalog: modelCatalog([
      {
        model: NEXUS_KIMI_MODEL,
        displayName: "Kimi K3",
        inputModalities: ["text"],
      },
    ]),
    codexChatReasoning: {
      supportsThinking: true,
      supportsEffort: false,
      thinkingParam: "thinking",
      effortParam: "none",
      outputFormat: "reasoning_content",
    },
    category: "third_party",
    icon: "kimi",
    iconColor: "#6366F1",
  },
  {
    name: "OpenAI Official",
    websiteUrl: "https://chatgpt.com/codex",
    isOfficial: true,
    category: "official",
    auth: {},
    config: ``,
    theme: {
      icon: "codex",
      backgroundColor: "#1F2937", // gray-800
      textColor: "#FFFFFF",
    },
    icon: "openai",
    iconColor: "#00A67E",
  },
];
