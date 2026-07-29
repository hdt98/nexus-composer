import { useState, useEffect, useCallback, useRef } from "react";
import { useTranslation } from "react-i18next";
import {
  updateCommonConfigSnippet,
  hasCommonConfigSnippet,
  validateJsonConfig,
} from "@/utils/providerConfigUtils";
import { configApi } from "@/lib/api";

const LEGACY_STORAGE_KEY = "nexus-composer:common-config-snippet";
const DEFAULT_COMMON_CONFIG_SNIPPET = `{
  "includeCoAuthoredBy": false
}`;

interface UseCommonConfigSnippetProps {
  settingsConfig: string;
  onConfigChange: (config: string) => void;
  initialData?: {
    settingsConfig?: Record<string, unknown>;
  };
  initialEnabled?: boolean;
  selectedPresetId?: string;
  /** When false, the hook skips all logic and returns disabled state. Default: true */
  enabled?: boolean;
}

/**
 * Manages the shared Claude configuration snippet.
 * Loads and saves it through config.json, with a seamless migration from localStorage.
 */
export function useCommonConfigSnippet({
  settingsConfig,
  onConfigChange,
  initialData,
  initialEnabled,
  selectedPresetId,
  enabled = true,
}: UseCommonConfigSnippetProps) {
  const { t } = useTranslation();
  const [useCommonConfig, setUseCommonConfig] = useState(false);
  const [commonConfigSnippet, setCommonConfigSnippetState] = useState<string>(
    DEFAULT_COMMON_CONFIG_SNIPPET,
  );
  const [commonConfigError, setCommonConfigError] = useState("");
  const [isLoading, setIsLoading] = useState(true);
  const [isExtracting, setIsExtracting] = useState(false);

  // Track updates originating from the common configuration.
  const isUpdatingFromCommonConfig = useRef(false);
  // Track whether create mode initialized its default selection.
  const hasInitializedNewMode = useRef(false);
  // Track whether edit mode initialized the explicit toggle or preview.
  const hasInitializedEditMode = useRef(false);

  // Reset initialization flags when the preset changes.
  useEffect(() => {
    if (!enabled) return;
    hasInitializedNewMode.current = false;
    hasInitializedEditMode.current = false;
  }, [selectedPresetId, enabled, initialEnabled]);

  // Load from config.json and migrate from localStorage when needed.
  useEffect(() => {
    if (!enabled) {
      setIsLoading(false);
      return;
    }
    let mounted = true;

    const loadSnippet = async () => {
      try {
        // Load through the unified API.
        const snippet = await configApi.getCommonConfigSnippet("claude");

        if (snippet && snippet.trim()) {
          if (mounted) {
            setCommonConfigSnippetState(snippet);
          }
        } else {
          // If config.json has no snippet, try to migrate one from localStorage.
          if (typeof window !== "undefined") {
            try {
              const legacySnippet =
                window.localStorage.getItem(LEGACY_STORAGE_KEY);
              if (legacySnippet && legacySnippet.trim()) {
                // Migrate the snippet to config.json.
                await configApi.setCommonConfigSnippet("claude", legacySnippet);
                if (mounted) {
                  setCommonConfigSnippetState(legacySnippet);
                }
                // Remove the migrated localStorage entry.
                window.localStorage.removeItem(LEGACY_STORAGE_KEY);
                console.log(
                  "[Migration] Moved the Claude common configuration from localStorage to config.json",
                );
              }
            } catch (e) {
              console.warn(
                "[Migration] Failed to migrate from localStorage:",
                e,
              );
            }
          }
        }
      } catch (error) {
        console.error("Failed to load common configuration:", error);
      } finally {
        if (mounted) {
          setIsLoading(false);
        }
      }
    };

    loadSnippet();

    return () => {
      mounted = false;
    };
  }, [enabled]);

  // Initialize the common configuration state in edit mode.
  useEffect(() => {
    if (!enabled) return;
    if (initialData && !isLoading && !hasInitializedEditMode.current) {
      hasInitializedEditMode.current = true;

      const configString = JSON.stringify(initialData.settingsConfig, null, 2);
      const inferredHasCommon = hasCommonConfigSnippet(
        configString,
        commonConfigSnippet,
      );

      // An explicit initialEnabled value takes precedence over the inferred value.
      // Use the inferred value when initialEnabled is undefined.
      const hasCommon =
        initialEnabled !== undefined ? initialEnabled : inferredHasCommon;
      setUseCommonConfig(hasCommon);

      // Add the snippet when it should be enabled but is not yet present.
      if (hasCommon && !inferredHasCommon) {
        const { updatedConfig, error } = updateCommonConfigSnippet(
          settingsConfig,
          commonConfigSnippet,
          true,
        );
        if (!error) {
          isUpdatingFromCommonConfig.current = true;
          onConfigChange(updatedConfig);
          setTimeout(() => {
            isUpdatingFromCommonConfig.current = false;
          }, 0);
        }
      }
    }
  }, [
    enabled,
    initialData,
    initialEnabled,
    commonConfigSnippet,
    isLoading,
    onConfigChange,
    settingsConfig,
  ]);

  // Enable a valid, non-empty common snippet by default in create mode.
  useEffect(() => {
    if (!enabled) return;
    // Run only once after loading in create mode.
    if (!initialData && !isLoading && !hasInitializedNewMode.current) {
      hasInitializedNewMode.current = true;

      // Check whether the snippet has meaningful content.
      try {
        const snippetObj = JSON.parse(commonConfigSnippet);
        const hasContent = Object.keys(snippetObj).length > 0;
        if (hasContent) {
          setUseCommonConfig(true);
          // Merge the common snippet into the current configuration.
          const { updatedConfig, error } = updateCommonConfigSnippet(
            settingsConfig,
            commonConfigSnippet,
            true,
          );
          if (!error) {
            isUpdatingFromCommonConfig.current = true;
            onConfigChange(updatedConfig);
            setTimeout(() => {
              isUpdatingFromCommonConfig.current = false;
            }, 0);
          }
        }
      } catch {
        // ignore parse error
      }
    }
  }, [
    enabled,
    initialData,
    commonConfigSnippet,
    isLoading,
    settingsConfig,
    onConfigChange,
  ]);

  // Handle the common-configuration toggle.
  const handleCommonConfigToggle = useCallback(
    (checked: boolean) => {
      const { updatedConfig, error: snippetError } = updateCommonConfigSnippet(
        settingsConfig,
        commonConfigSnippet,
        checked,
      );

      if (snippetError) {
        setCommonConfigError(snippetError);
        setUseCommonConfig(false);
        return;
      }

      setCommonConfigError("");
      setUseCommonConfig(checked);
      // Mark this update as originating from the common configuration.
      isUpdatingFromCommonConfig.current = true;
      onConfigChange(updatedConfig);
      // Reset the marker on the next event-loop turn.
      setTimeout(() => {
        isUpdatingFromCommonConfig.current = false;
      }, 0);
    },
    [settingsConfig, commonConfigSnippet, onConfigChange],
  );

  // Handle changes to the common configuration snippet.
  const handleCommonConfigSnippetChange = useCallback(
    (value: string) => {
      const previousSnippet = commonConfigSnippet;
      setCommonConfigSnippetState(value);

      if (!value.trim()) {
        setCommonConfigError("");
        // Persist the empty value to config.json.
        configApi
          .setCommonConfigSnippet("claude", "")
          .catch((error: unknown) => {
            console.error("Failed to save common configuration:", error);
            setCommonConfigError(
              t("claudeConfig.saveFailed", { error: String(error) }),
            );
          });

        if (useCommonConfig) {
          const { updatedConfig } = updateCommonConfigSnippet(
            settingsConfig,
            previousSnippet,
            false,
          );
          onConfigChange(updatedConfig);
          setUseCommonConfig(false);
        }
        return;
      }

      // Validate the JSON format.
      const validationError = validateJsonConfig(
        value,
        "Common configuration snippet",
      );
      if (validationError) {
        setCommonConfigError(validationError);
      } else {
        setCommonConfigError("");
        // Persist the snippet to config.json.
        configApi
          .setCommonConfigSnippet("claude", value)
          .catch((error: unknown) => {
            console.error("Failed to save common configuration:", error);
            setCommonConfigError(
              t("claudeConfig.saveFailed", { error: String(error) }),
            );
          });
      }

      // Replace the applied snippet when it is enabled and valid.
      if (useCommonConfig && !validationError) {
        const removeResult = updateCommonConfigSnippet(
          settingsConfig,
          previousSnippet,
          false,
        );
        if (removeResult.error) {
          setCommonConfigError(removeResult.error);
          return;
        }
        const addResult = updateCommonConfigSnippet(
          removeResult.updatedConfig,
          value,
          true,
        );

        if (addResult.error) {
          setCommonConfigError(addResult.error);
          return;
        }

        // Mark the update to avoid triggering the state check.
        isUpdatingFromCommonConfig.current = true;
        onConfigChange(addResult.updatedConfig);
        // Reset the marker on the next event-loop turn.
        setTimeout(() => {
          isUpdatingFromCommonConfig.current = false;
        }, 0);
      }
    },
    [commonConfigSnippet, settingsConfig, useCommonConfig, onConfigChange],
  );

  // Check whether changed config contains the common snippet, except during common-config updates.
  useEffect(() => {
    if (!enabled) return;
    if (isUpdatingFromCommonConfig.current || isLoading) {
      return;
    }
    const hasCommon = hasCommonConfigSnippet(
      settingsConfig,
      commonConfigSnippet,
    );
    setUseCommonConfig(hasCommon);
  }, [enabled, settingsConfig, commonConfigSnippet, isLoading]);

  // Extract the common snippet from the editor's current content.
  const handleExtract = useCallback(async () => {
    setIsExtracting(true);
    setCommonConfigError("");

    try {
      const extracted = await configApi.extractCommonConfigSnippet("claude", {
        settingsConfig,
      });

      if (!extracted || extracted === "{}") {
        setCommonConfigError(t("claudeConfig.extractNoCommonConfig"));
        return;
      }

      // Validate the extracted JSON.
      const validationError = validateJsonConfig(
        extracted,
        "Extracted configuration",
      );
      if (validationError) {
        setCommonConfigError(validationError);
        return;
      }

      // Update the snippet state.
      setCommonConfigSnippetState(extracted);

      // Persist it through the backend.
      await configApi.setCommonConfigSnippet("claude", extracted);
    } catch (error) {
      console.error("Failed to extract common configuration:", error);
      setCommonConfigError(
        t("claudeConfig.extractFailed", { error: String(error) }),
      );
    } finally {
      setIsExtracting(false);
    }
  }, [settingsConfig, t]);

  return {
    useCommonConfig,
    commonConfigSnippet,
    commonConfigError,
    isLoading,
    isExtracting,
    handleCommonConfigToggle,
    handleCommonConfigSnippetChange,
    handleExtract,
  };
}
