import { useEffect, useState, useCallback } from "react";
import type { ProviderCategory } from "@/types";
import {
  getApiKeyFromConfig,
  setApiKeyInConfig,
  hasApiKeyField,
} from "@/utils/providerConfigUtils";

interface UseApiKeyStateProps {
  initialConfig?: string;
  onConfigChange: (config: string) => void;
  selectedPresetId: string | null;
  category?: ProviderCategory;
  appType?: string;
  apiKeyField?: string;
}

/**
 * Manages API key input state and synchronizes it with JSON configuration.
 */
export function useApiKeyState({
  initialConfig,
  onConfigChange,
  selectedPresetId,
  category,
  appType,
  apiKeyField,
}: UseApiKeyStateProps) {
  const [apiKey, setApiKey] = useState(() => {
    if (initialConfig) {
      return getApiKeyFromConfig(initialConfig, appType);
    }
    return "";
  });

  // Synchronize external configuration changes, such as form.reset or a live
  // read, back to API key state. Only synchronize parseable JSON so temporary
  // invalid input while editing does not make the field flicker.
  useEffect(() => {
    if (!initialConfig) return;

    try {
      JSON.parse(initialConfig);
    } catch {
      return;
    }

    // Extract the API key, or an empty string if it is absent.
    const extracted = getApiKeyFromConfig(initialConfig, appType);
    if (extracted !== apiKey) {
      setApiKey(extracted);
    }
  }, [initialConfig, appType, apiKey]);

  const handleApiKeyChange = useCallback(
    (key: string) => {
      setApiKey(key);

      const configString = setApiKeyInConfig(
        initialConfig || "{}",
        key.trim(),
        {
          // Create a missing field only in add mode for a known, non-official
          // category. Official categories disable the field in the UI, while
          // an omitted category must not trigger unexpected field creation.
          createIfMissing:
            selectedPresetId !== null &&
            category !== undefined &&
            category !== "official",
          appType,
          apiKeyField,
        },
      );

      onConfigChange(configString);
    },
    [
      initialConfig,
      selectedPresetId,
      category,
      appType,
      apiKeyField,
      onConfigChange,
    ],
  );

  const showApiKey = useCallback(
    (config: string, isEditMode: boolean) => {
      return (
        selectedPresetId !== null ||
        (isEditMode && hasApiKeyField(config, appType))
      );
    },
    [selectedPresetId, appType],
  );

  return {
    apiKey,
    setApiKey,
    handleApiKeyChange,
    showApiKey,
  };
}
