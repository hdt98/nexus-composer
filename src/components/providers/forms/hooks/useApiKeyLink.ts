import { useMemo } from "react";
import type { AppId } from "@/lib/api";
import type { ProviderCategory } from "@/types";
import type { ProviderPreset } from "@/config/claudeProviderPresets";
import type { CodexProviderPreset } from "@/config/codexProviderPresets";
import type { GeminiProviderPreset } from "@/config/geminiProviderPresets";
import type { OpenCodeProviderPreset } from "@/config/opencodeProviderPresets";
import type { ClaudeDesktopProviderPreset } from "@/config/claudeDesktopProviderPresets";

type PresetEntry = {
  id: string;
  preset:
    | ProviderPreset
    | CodexProviderPreset
    | GeminiProviderPreset
    | OpenCodeProviderPreset
    | ClaudeDesktopProviderPreset;
};

interface UseApiKeyLinkProps {
  appId: AppId;
  category?: ProviderCategory;
  selectedPresetId: string | null;
  presetEntries: PresetEntry[];
  formWebsiteUrl: string;
}

/**
 * Manages API key link visibility and URL.
 */
export function useApiKeyLink({
  appId,
  category,
  selectedPresetId,
  presetEntries,
  formWebsiteUrl,
}: UseApiKeyLinkProps) {
  // Determine whether to show the API key link.
  const shouldShowApiKeyLink = useMemo(() => {
    return (
      category !== "official" &&
      (category === "cn_official" ||
        category === "aggregator" ||
        category === "third_party")
    );
  }, [category]);

  // Resolve the current preset entry.
  const currentPresetEntry = useMemo(() => {
    if (selectedPresetId && selectedPresetId !== "custom") {
      return presetEntries.find((item) => item.id === selectedPresetId);
    }
    return undefined;
  }, [selectedPresetId, presetEntries]);

  // Resolve the current provider URL for the API key link.
  const getWebsiteUrl = useMemo(() => {
    if (currentPresetEntry) {
      const preset = currentPresetEntry.preset;
      // Prefer apiKeyUrl, which may include promotion parameters, for
      // cn_official, aggregator, and third_party presets.
      if (
        preset.category === "cn_official" ||
        preset.category === "aggregator" ||
        preset.category === "third_party"
      ) {
        return preset.apiKeyUrl || preset.websiteUrl || "";
      }
      return preset.websiteUrl || "";
    }
    return formWebsiteUrl || "";
  }, [currentPresetEntry, formWebsiteUrl]);

  // Extract partner information.
  const isPartner = useMemo(() => {
    return currentPresetEntry?.preset.isPartner ?? false;
  }, [currentPresetEntry]);

  const partnerPromotionKey = useMemo(() => {
    return currentPresetEntry?.preset.partnerPromotionKey;
  }, [currentPresetEntry]);

  return {
    shouldShowApiKeyLink:
      appId === "claude" ||
      appId === "claude-desktop" ||
      appId === "codex" ||
      appId === "gemini" ||
      appId === "opencode" ||
      appId === "openclaw" ||
      appId === "hermes"
        ? shouldShowApiKeyLink
        : false,
    websiteUrl: getWebsiteUrl,
    isPartner,
    partnerPromotionKey,
  };
}
