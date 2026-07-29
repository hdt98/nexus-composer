import { useMemo } from "react";
import type { AppId } from "@/lib/api";
import type { ProviderPreset } from "@/config/claudeProviderPresets";
import type { CodexProviderPreset } from "@/config/codexProviderPresets";
import type { ProviderMeta, EndpointCandidate } from "@/types";
import { extractCodexBaseUrl } from "@/utils/providerConfigUtils";

type PresetEntry = {
  id: string;
  preset: ProviderPreset | CodexProviderPreset;
};

interface UseSpeedTestEndpointsProps {
  appId: AppId;
  selectedPresetId: string | null;
  presetEntries: PresetEntry[];
  baseUrl: string;
  codexBaseUrl: string;
  initialData?: {
    settingsConfig?: Record<string, unknown>;
    meta?: ProviderMeta;
  };
}

/**
 * Collect initial endpoints for the endpoint speed-test dialog.
 *
 * Sources:
 * 1. The currently selected base URL.
 * 2. The initial data URL in edit mode.
 * 3. endpointCandidates from the selected preset.
 *
 * Saved custom endpoints are loaded by EndpointSpeedTest through the
 * getCustomEndpoints API, so do not load them here and import duplicates.
 */
export function useSpeedTestEndpoints({
  appId,
  selectedPresetId,
  presetEntries,
  baseUrl,
  codexBaseUrl,
  initialData,
}: UseSpeedTestEndpointsProps) {
  const claudeEndpoints = useMemo<EndpointCandidate[]>(() => {
    // Reuse this branch for Claude and Gemini (non-Codex)
    if (appId !== "claude" && appId !== "gemini") return [];

    const map = new Map<string, EndpointCandidate>();
    // Candidate endpoints use isCustom: false because they come from presets or configuration.
    // EndpointSpeedTest loads saved custom endpoints separately through the API.
    const add = (url?: string, isCustom = false) => {
      if (!url) return;
      const sanitized = url.trim().replace(/\/+$/, "");
      if (!sanitized || map.has(sanitized)) return;
      map.set(sanitized, { url: sanitized, isCustom });
    };

    // 1. Current base URL.
    if (baseUrl) {
      add(baseUrl);
    }

    // 2. URL from initial data in edit mode.
    if (initialData && typeof initialData.settingsConfig === "object") {
      const configEnv = initialData.settingsConfig as {
        env?: { ANTHROPIC_BASE_URL?: string; GOOGLE_GEMINI_BASE_URL?: string };
      };
      const envUrls = [
        configEnv.env?.ANTHROPIC_BASE_URL,
        configEnv.env?.GOOGLE_GEMINI_BASE_URL,
      ];
      envUrls.forEach((u) => {
        if (typeof u === "string") add(u);
      });
    }

    // 3. endpointCandidates from the selected preset.
    if (selectedPresetId && selectedPresetId !== "custom") {
      const entry = presetEntries.find((item) => item.id === selectedPresetId);
      if (entry) {
        const preset = entry.preset as ProviderPreset & {
          settingsConfig?: { env?: { GOOGLE_GEMINI_BASE_URL?: string } };
          endpointCandidates?: string[];
        };
        // Add the preset's own base URL for Claude and Gemini compatibility.
        const presetEnv = preset.settingsConfig as {
          env?: {
            ANTHROPIC_BASE_URL?: string;
            GOOGLE_GEMINI_BASE_URL?: string;
          };
        };
        const presetUrls = [
          presetEnv?.env?.ANTHROPIC_BASE_URL,
          presetEnv?.env?.GOOGLE_GEMINI_BASE_URL,
        ];
        presetUrls.forEach((u) => add(u));
        // Add candidate endpoints from the preset.
        if (preset.endpointCandidates) {
          preset.endpointCandidates.forEach((url) => add(url));
        }
      }
    }

    return Array.from(map.values());
  }, [appId, baseUrl, initialData, selectedPresetId, presetEntries]);

  const codexEndpoints = useMemo<EndpointCandidate[]>(() => {
    if (appId !== "codex") return [];

    const map = new Map<string, EndpointCandidate>();
    // Candidate endpoints use isCustom: false because they come from presets or configuration.
    // EndpointSpeedTest loads saved custom endpoints separately through the API.
    const add = (url?: string, isCustom = false) => {
      if (!url) return;
      const sanitized = url.trim().replace(/\/+$/, "");
      if (!sanitized || map.has(sanitized)) return;
      map.set(sanitized, { url: sanitized, isCustom });
    };

    // 1. Current Codex base URL.
    if (codexBaseUrl) {
      add(codexBaseUrl);
    }

    // 2. URL from initial data in edit mode.
    const initialCodexConfig = initialData?.settingsConfig as
      | {
          config?: string;
        }
      | undefined;
    const configStr = initialCodexConfig?.config ?? "";
    const extractedBaseUrl = extractCodexBaseUrl(configStr);
    if (extractedBaseUrl) {
      add(extractedBaseUrl);
    }

    // 3. endpointCandidates from the selected preset.
    if (selectedPresetId && selectedPresetId !== "custom") {
      const entry = presetEntries.find((item) => item.id === selectedPresetId);
      if (entry) {
        const preset = entry.preset as CodexProviderPreset;
        // Add the preset's own base URL.
        const presetConfig = preset.config || "";
        const presetBaseUrl = extractCodexBaseUrl(presetConfig);
        if (presetBaseUrl) {
          add(presetBaseUrl);
        }
        // Add candidate endpoints from the preset.
        if (preset.endpointCandidates) {
          preset.endpointCandidates.forEach((url) => add(url));
        }
      }
    }

    return Array.from(map.values());
  }, [appId, codexBaseUrl, initialData, selectedPresetId, presetEntries]);

  return appId === "codex" ? codexEndpoints : claudeEndpoints;
}
