/**
 * Provider preset configuration templates.
 */
import { ProviderCategory, type LocalProxyRequestOverrides } from "../types";
import {
  NEXUS_AUTO_COMPACT_TOKENS,
  NEXUS_CLAUDE_BASE_URL,
  NEXUS_CLAUDE_MANAGED_PRESET_VERSION,
  NEXUS_CLAUDE_MODEL,
  NEXUS_ENDPOINT,
  NEXUS_KIMI_MODEL,
  NEXUS_KIMI_ORIGIN,
  NEXUS_KIMI_TEXT_MODEL_CATALOG,
  NEXUS_REQUEST_OVERRIDES,
  NEXUS_TEXT_MODEL_CATALOG,
} from "./nexus";

export interface TemplateValueConfig {
  label: string;
  placeholder: string;
  defaultValue?: string;
  editorValue: string;
}

/**
 * Visual theme configuration for a provider preset.
 */
export interface PresetTheme {
  /** Icon type: 'claude' | 'codex' | 'gemini' | 'generic'. */
  icon?: "claude" | "codex" | "gemini" | "generic";
  /** Selected-state background color as a Tailwind class or hex value. */
  backgroundColor?: string;
  /** Selected-state text color as a Tailwind class or hex value. */
  textColor?: string;
}

export interface ProviderPreset {
  name: string;
  nameKey?: string; // i18n key for localized display name
  websiteUrl: string;
  // Optional API key URL for third-party or aggregator providers.
  apiKeyUrl?: string;
  settingsConfig: object;
  isOfficial?: boolean; // Whether this is an official preset.
  isPartner?: boolean; // Whether this is a commercial partner.
  primePartner?: boolean; // Featured top-tier partner whose badge uses a heart.
  partnerPromotionKey?: string; // i18n key for partner promotional text.
  category?: ProviderCategory; // Provider category.
  // API key field used by this preset; defaults to ANTHROPIC_AUTH_TOKEN.
  apiKeyField?: "ANTHROPIC_AUTH_TOKEN" | "ANTHROPIC_API_KEY";
  // Template variables for dynamically substituting configuration values.
  templateValues?: Record<string, TemplateValueConfig>; // editorValue stores the current editor input.
  // Candidate request endpoints used for endpoint management and speed tests.
  endpointCandidates?: string[];
  // Visual theme configuration.
  theme?: PresetTheme;
  // Icon configuration.
  icon?: string; // Icon name.
  iconColor?: string; // Icon color.

  // Claude API format, used only by Claude providers.
  // - "anthropic" (default): pass through Anthropic Messages API requests.
  // - "openai_chat": convert to the OpenAI Chat Completions format.
  // - "openai_responses": convert to the OpenAI Responses API format.
  // - "gemini_native": convert to the Gemini Native generateContent format.
  apiFormat?:
    | "anthropic"
    | "openai_chat"
    | "openai_responses"
    | "gemini_native";

  // Provider type identifier for providers requiring special handling.
  // - "github_copilot": GitHub Copilot provider requiring OAuth.
  // - "codex_oauth": OpenAI Codex proxy through ChatGPT Plus/Pro requiring OAuth.
  providerType?: "github_copilot" | "codex_oauth" | "nexus";
  localProxyRequestOverrides?: LocalProxyRequestOverrides;
  managedNexusPresetVersion?: number;
  apiKeyOptional?: boolean;

  // Whether this provider requires OAuth instead of an API key.
  requiresOAuth?: boolean;

  // Whether to hide this preset in the UI while retaining its definition.
  hidden?: boolean;

  // Full model-list URL overriding automatic candidate resolution.
  // By default, the backend derives candidates from baseURL using /v1/models, /models,
  // and variants with known compatibility subpaths removed.
  modelsUrl?: string;
}

export const providerPresets: ProviderPreset[] = [
  // Nexus routes Anthropic Messages through the built-in Chat adapter.
  {
    name: "Nexus GLM-5.2",
    nameKey: "providerForm.presets.nexus",
    websiteUrl: NEXUS_ENDPOINT,
    settingsConfig: {
      modelCatalog: NEXUS_TEXT_MODEL_CATALOG,
      env: {
        ANTHROPIC_BASE_URL: NEXUS_CLAUDE_BASE_URL,
        ANTHROPIC_AUTH_TOKEN: "",
        ANTHROPIC_MODEL: NEXUS_CLAUDE_MODEL,
        ANTHROPIC_DEFAULT_HAIKU_MODEL: NEXUS_CLAUDE_MODEL,
        ANTHROPIC_DEFAULT_SONNET_MODEL: NEXUS_CLAUDE_MODEL,
        ANTHROPIC_DEFAULT_OPUS_MODEL: NEXUS_CLAUDE_MODEL,
        ANTHROPIC_DEFAULT_FABLE_MODEL: NEXUS_CLAUDE_MODEL,
        API_TIMEOUT_MS: "3000000",
        CLAUDE_CODE_AUTO_COMPACT_WINDOW: String(NEXUS_AUTO_COMPACT_TOKENS),
        CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1",
        CLAUDE_CODE_ATTRIBUTION_HEADER: "0",
      },
    },
    endpointCandidates: [NEXUS_CLAUDE_BASE_URL],
    apiFormat: "openai_chat",
    providerType: "nexus",
    managedNexusPresetVersion: NEXUS_CLAUDE_MANAGED_PRESET_VERSION,
    localProxyRequestOverrides: NEXUS_REQUEST_OVERRIDES,
    category: "third_party",
    icon: "nexus",
    iconColor: "#6366F1",
  },
  {
    name: "Nexus Kimi-K3",
    websiteUrl: NEXUS_KIMI_ORIGIN,
    settingsConfig: {
      modelCatalog: NEXUS_KIMI_TEXT_MODEL_CATALOG,
      env: {
        ANTHROPIC_BASE_URL: NEXUS_KIMI_ORIGIN,
        ANTHROPIC_AUTH_TOKEN: "",
        ANTHROPIC_MODEL: NEXUS_KIMI_MODEL,
        ANTHROPIC_DEFAULT_HAIKU_MODEL: NEXUS_KIMI_MODEL,
        ANTHROPIC_DEFAULT_SONNET_MODEL: NEXUS_KIMI_MODEL,
        ANTHROPIC_DEFAULT_OPUS_MODEL: NEXUS_KIMI_MODEL,
        ANTHROPIC_DEFAULT_FABLE_MODEL: NEXUS_KIMI_MODEL,
      },
    },
    endpointCandidates: [NEXUS_KIMI_ORIGIN],
    apiFormat: "openai_chat",
    apiKeyOptional: true,
    category: "third_party",
    icon: "kimi",
    iconColor: "#6366F1",
  },
  {
    name: "Claude Official",
    websiteUrl: "https://www.anthropic.com/claude-code",
    settingsConfig: {
      env: {},
    },
    isOfficial: true,
    category: "official",
    theme: {
      icon: "claude",
      backgroundColor: "#D97757",
      textColor: "#FFFFFF",
    },
    icon: "anthropic",
    iconColor: "#D4915D",
  },
];
