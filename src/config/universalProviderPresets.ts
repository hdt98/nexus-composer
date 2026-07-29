/**
 * Universal Provider preset configuration.
 *
 * Universal Providers share configuration across applications. Changes are
 * automatically synchronized to Claude, Codex, and Gemini.
 * Intended for multi-protocol API gateways such as NewAPI.
 */

import type {
  UniversalProvider,
  UniversalProviderApps,
  UniversalProviderModels,
} from "@/types";
import { deepClone } from "@/utils/deepClone";

/**
 * Universal Provider preset definition.
 */
export interface UniversalProviderPreset {
  /** Preset name. */
  name: string;
  /** Provider type identifier. */
  providerType: string;
  /** Applications enabled by default. */
  defaultApps: UniversalProviderApps;
  /** Default model configuration. */
  defaultModels: UniversalProviderModels;
  /** Website URL. */
  websiteUrl?: string;
  /** Icon name. */
  icon?: string;
  /** Icon color. */
  iconColor?: string;
  /** Description. */
  description?: string;
  /** Whether this is a custom template that users can fully customize. */
  isCustomTemplate?: boolean;
}

/**
 * Default NewAPI model configuration.
 */
const NEWAPI_DEFAULT_MODELS: UniversalProviderModels = {
  claude: {
    model: "claude-sonnet-5",
    haikuModel: "claude-haiku-4-5-20251001",
    sonnetModel: "claude-sonnet-5",
    opusModel: "claude-opus-4-8",
  },
  codex: {
    model: "gpt-5.5",
    reasoningEffort: "high",
  },
  gemini: {
    model: "gemini-3.5-flash",
  },
};

/**
 * Universal Provider presets.
 */
export const universalProviderPresets: UniversalProviderPreset[] = [
  {
    name: "NewAPI",
    providerType: "newapi",
    defaultApps: {
      claude: true,
      codex: true,
      gemini: true,
    },
    defaultModels: NEWAPI_DEFAULT_MODELS,
    websiteUrl: "https://www.newapi.pro",
    icon: "newapi",
    iconColor: "#00A67E",
    description:
      "A self-hostable API gateway supporting Anthropic, OpenAI, and Gemini protocols.",
  },
  {
    name: "Custom Gateway",
    providerType: "custom_gateway",
    defaultApps: {
      claude: true,
      codex: true,
      gemini: true,
    },
    defaultModels: NEWAPI_DEFAULT_MODELS,
    icon: "openai",
    iconColor: "#6366F1",
    description: "A custom API gateway configuration.",
    isCustomTemplate: true,
  },
];

/**
 * Creates a Universal Provider from a preset.
 */
export function createUniversalProviderFromPreset(
  preset: UniversalProviderPreset,
  id: string,
  baseUrl: string,
  apiKey: string,
  customName?: string,
): UniversalProvider {
  return {
    id,
    name: customName || preset.name,
    providerType: preset.providerType,
    apps: { ...preset.defaultApps },
    baseUrl,
    apiKey,
    models: deepClone(preset.defaultModels),
    websiteUrl: preset.websiteUrl,
    icon: preset.icon,
    iconColor: preset.iconColor,
    createdAt: Date.now(),
  };
}

/**
 * Returns the preset name for display in the UI.
 */
export function getPresetDisplayName(preset: UniversalProviderPreset): string {
  return preset.name;
}

/**
 * Finds a preset by provider type.
 */
export function findPresetByType(
  providerType: string,
): UniversalProviderPreset | undefined {
  return universalProviderPresets.find((p) => p.providerType === providerType);
}
