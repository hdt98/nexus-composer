/**
 * Claude Desktop provider presets use a top-level base URL and map
 * Desktop-visible role IDs to upstream models.
 */
import { ProviderCategory, type LocalProxyRequestOverrides } from "../types";
import type { PresetTheme } from "./claudeProviderPresets";
import {
  NEXUS_ENDPOINT,
  NEXUS_CLAUDE_DESKTOP_MANAGED_PRESET_VERSION,
  NEXUS_MODEL,
  NEXUS_REQUEST_OVERRIDES,
  NEXUS_TEXT_MODEL_CATALOG,
} from "./nexus";

export type ClaudeDesktopApiFormat =
  | "anthropic"
  | "openai_chat"
  | "openai_responses"
  | "gemini_native";

export interface ClaudeDesktopRoutePreset {
  routeId: string;
  upstreamModel: string;
  labelOverride?: string;
  supports1m: boolean;
}

/**
 * Claude Desktop 1.12603.1+ accepts fable alongside the established
 * sonnet, opus, and haiku role IDs. Mythos is not publicly available.
 */
export const CLAUDE_DESKTOP_ROLE_ROUTE_IDS = {
  sonnet: "claude-sonnet-5",
  opus: "claude-opus-4-8",
  fable: "claude-fable-5",
  haiku: "claude-haiku-4-5",
} as const;

export type ClaudeDesktopRoleId = keyof typeof CLAUDE_DESKTOP_ROLE_ROUTE_IDS;

export interface ClaudeDesktopProviderPreset {
  name: string;
  nameKey?: string;
  websiteUrl: string;
  apiKeyUrl?: string;
  category?: ProviderCategory;
  isPartner?: boolean;
  primePartner?: boolean;
  partnerPromotionKey?: string;
  baseUrl: string;
  apiKeyField?: "ANTHROPIC_AUTH_TOKEN" | "ANTHROPIC_API_KEY";
  mode: "direct" | "proxy";
  apiFormat?: ClaudeDesktopApiFormat;
  modelRoutes?: ClaudeDesktopRoutePreset[];
  providerType?: "github_copilot" | "codex_oauth" | "nexus";
  modelCatalog?: Record<string, unknown>;
  localProxyRequestOverrides?: LocalProxyRequestOverrides;
  managedNexusPresetVersion?: number;
  requiresOAuth?: boolean;
  endpointCandidates?: string[];
  theme?: PresetTheme;
  icon?: string;
  iconColor?: string;
}

export const claudeDesktopProviderPresets: ClaudeDesktopProviderPreset[] = [
  {
    name: "Claude Desktop Official",
    websiteUrl: "https://claude.ai/download",
    category: "official",
    baseUrl: "",
    mode: "direct",
    apiFormat: "anthropic",
    theme: {
      icon: "claude",
      backgroundColor: "#D97757",
      textColor: "#FFFFFF",
    },
    icon: "anthropic",
    iconColor: "#D4915D",
  },
  {
    name: "Nexus GLM-5.2",
    nameKey: "providerForm.presets.nexus",
    websiteUrl: NEXUS_ENDPOINT,
    category: "third_party",
    baseUrl: NEXUS_ENDPOINT,
    mode: "proxy",
    apiFormat: "openai_chat",
    modelRoutes: [
      {
        routeId: CLAUDE_DESKTOP_ROLE_ROUTE_IDS.sonnet,
        upstreamModel: NEXUS_MODEL,
        labelOverride: NEXUS_MODEL,
        supports1m: true,
      },
    ],
    providerType: "nexus",
    modelCatalog: NEXUS_TEXT_MODEL_CATALOG,
    localProxyRequestOverrides: NEXUS_REQUEST_OVERRIDES,
    managedNexusPresetVersion: NEXUS_CLAUDE_DESKTOP_MANAGED_PRESET_VERSION,
    endpointCandidates: [NEXUS_ENDPOINT],
    icon: "nexus",
    iconColor: "#6366F1",
  },
];
