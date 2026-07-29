import { useMemo } from "react";
import type { AppId } from "@/lib/api";
import type { CustomEndpoint } from "@/types";
import type { ProviderPreset } from "@/config/claudeProviderPresets";
import type { CodexProviderPreset } from "@/config/codexProviderPresets";

type PresetEntry = {
  id: string;
  preset: ProviderPreset | CodexProviderPreset;
};

interface UseCustomEndpointsProps {
  appId: AppId;
  selectedPresetId: string | null;
  presetEntries: PresetEntry[];
  draftCustomEndpoints: string[];
  baseUrl: string;
  codexBaseUrl: string;
}

/**
 * Collects and manages custom endpoints.
 *
 * Sources:
 * 1. Custom endpoints added by the user in the speed-test dialog.
 * 2. endpointCandidates from the preset.
 * 3. The currently selected Base URL.
 */
export function useCustomEndpoints({
  appId,
  selectedPresetId,
  presetEntries,
  draftCustomEndpoints,
  baseUrl,
  codexBaseUrl,
}: UseCustomEndpointsProps) {
  const customEndpointsMap = useMemo(() => {
    const urlSet = new Set<string>();

    // Normalize and add a URL.
    const push = (raw?: string) => {
      const url = (raw || "").trim().replace(/\/+$/, "");
      if (url) urlSet.add(url);
    };

    // 1. Custom endpoints added by the user.
    for (const u of draftCustomEndpoints) push(u);

    // 2. Preset endpoint candidates.
    if (selectedPresetId && selectedPresetId !== "custom") {
      const entry = presetEntries.find((item) => item.id === selectedPresetId);
      if (entry) {
        const preset = entry.preset as any;
        if (Array.isArray(preset?.endpointCandidates)) {
          for (const u of preset.endpointCandidates as string[]) push(u);
        }
      }
    }

    // 3. Current Base URL.
    if (appId === "codex") {
      push(codexBaseUrl);
    } else {
      push(baseUrl);
    }

    // Build the CustomEndpoint map.
    const urls = Array.from(urlSet.values());
    if (urls.length === 0) {
      return null;
    }

    const now = Date.now();
    const customMap: Record<string, CustomEndpoint> = {};
    for (const url of urls) {
      if (!customMap[url]) {
        customMap[url] = { url, addedAt: now, lastUsed: undefined };
      }
    }

    return customMap;
  }, [
    appId,
    selectedPresetId,
    presetEntries,
    draftCustomEndpoints,
    baseUrl,
    codexBaseUrl,
  ]);

  return customEndpointsMap;
}
