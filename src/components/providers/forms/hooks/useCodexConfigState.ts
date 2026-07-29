import { useState, useCallback, useEffect, useRef } from "react";
import {
  extractCodexBaseUrl,
  extractCodexExperimentalBearerToken,
  setCodexBaseUrl as setCodexBaseUrlInConfig,
  updateCodexExperimentalBearerToken,
} from "@/utils/providerConfigUtils";
import { normalizeTomlText } from "@/utils/textNormalization";
import type { CodexCatalogModel } from "@/types";

interface UseCodexConfigStateProps {
  initialData?: {
    settingsConfig?: Record<string, unknown>;
  };
}

// Fall back to experimental_bearer_token in config.toml when auth.json has no OPENAI_API_KEY.
// This mobile-compatible form retains the ChatGPT login state while using a third-party token.
function pickCodexApiKey(
  authObj: { OPENAI_API_KEY?: unknown } | null | undefined,
  configText: string,
): string {
  if (authObj && typeof authObj.OPENAI_API_KEY === "string") {
    const key = authObj.OPENAI_API_KEY;
    if (key) return key;
  }
  return extractCodexExperimentalBearerToken(configText) || "";
}

/**
 * Manage Codex configuration state.
 * Codex configuration has two parts: auth.json as JSON and config.toml as a TOML string.
 */
export function useCodexConfigState({ initialData }: UseCodexConfigStateProps) {
  const [codexAuth, setCodexAuthState] = useState("");
  const [codexConfig, setCodexConfigState] = useState("");
  const [codexApiKey, setCodexApiKey] = useState("");
  const [codexBaseUrl, setCodexBaseUrl] = useState("");
  const [codexCatalogModels, setCodexCatalogModels] = useState<
    CodexCatalogModel[]
  >([]);
  const [codexAuthError, setCodexAuthError] = useState("");

  const isUpdatingCodexBaseUrlRef = useRef(false);

  // Initialize Codex configuration in edit mode.
  useEffect(() => {
    if (!initialData) return;

    const config = initialData.settingsConfig;
    if (typeof config === "object" && config !== null) {
      // Set auth.json.
      const auth = (config as any).auth || {};
      setCodexAuthState(JSON.stringify(auth, null, 2));

      // Set config.toml.
      const configStr =
        typeof (config as any).config === "string"
          ? (config as any).config
          : "";
      setCodexConfigState(configStr);

      const modelCatalog = (config as any).modelCatalog;
      const rawCatalogModels = Array.isArray(modelCatalog?.models)
        ? modelCatalog.models
        : [];
      setCodexCatalogModels(
        rawCatalogModels
          .map((item: any) => {
            // Hidden fields for native Responses profiles are not exposed in the row UI but must survive load and save.
            // Preserve them verbatim so editing MiMo, MiniMax, and similar profiles does not discard official
            // base_instructions, parallel tool support, or image modalities. The database source of truth uses
            // camelCase, while live-config fallback parsing may use snake_case, so support both like displayName/contextWindow.
            const supportsParallelToolCalls =
              typeof item?.supportsParallelToolCalls === "boolean"
                ? item.supportsParallelToolCalls
                : typeof item?.supports_parallel_tool_calls === "boolean"
                  ? item.supports_parallel_tool_calls
                  : undefined;
            const inputModalities = Array.isArray(item?.inputModalities)
              ? item.inputModalities
              : Array.isArray(item?.input_modalities)
                ? item.input_modalities
                : undefined;
            const baseInstructions =
              typeof item?.baseInstructions === "string"
                ? item.baseInstructions
                : typeof item?.base_instructions === "string"
                  ? item.base_instructions
                  : undefined;
            return {
              model: typeof item?.model === "string" ? item.model : "",
              displayName:
                typeof item?.displayName === "string"
                  ? item.displayName
                  : typeof item?.display_name === "string"
                    ? item.display_name
                    : "",
              contextWindow:
                typeof item?.contextWindow === "string" ||
                typeof item?.contextWindow === "number"
                  ? item.contextWindow
                  : typeof item?.context_window === "string" ||
                      typeof item?.context_window === "number"
                    ? item.context_window
                    : "",
              ...(supportsParallelToolCalls !== undefined
                ? { supportsParallelToolCalls }
                : {}),
              ...(inputModalities ? { inputModalities } : {}),
              ...(baseInstructions ? { baseInstructions } : {}),
            };
          })
          .filter((item: CodexCatalogModel) => item.model.trim()),
      );

      // Extract the base URL.
      const initialBaseUrl = extractCodexBaseUrl(configStr);
      if (initialBaseUrl) {
        setCodexBaseUrl(initialBaseUrl);
      }

      setCodexApiKey(pickCodexApiKey(auth, configStr));
    }
  }, [initialData]);

  // Keep the base URL synchronized with the TOML configuration.
  useEffect(() => {
    if (isUpdatingCodexBaseUrlRef.current) {
      return;
    }
    const extracted = extractCodexBaseUrl(codexConfig) || "";
    setCodexBaseUrl((prev) => (prev === extracted ? prev : extracted));
  }, [codexConfig]);

  // Get the API key from auth JSON.
  const getCodexAuthApiKey = useCallback((authString: string): string => {
    try {
      const auth = JSON.parse(authString || "{}");
      return typeof auth.OPENAI_API_KEY === "string" ? auth.OPENAI_API_KEY : "";
    } catch {
      return "";
    }
  }, []);

  // Extract and synchronize the API key from codexAuth.
  useEffect(() => {
    let parsed: { OPENAI_API_KEY?: unknown } | null = null;
    try {
      parsed = JSON.parse(codexAuth || "{}");
    } catch {
      parsed = null;
    }
    const extractedKey = pickCodexApiKey(parsed, codexConfig);
    setCodexApiKey((prev) => (prev === extractedKey ? prev : extractedKey));
  }, [codexAuth, codexConfig]);

  // Validate Codex auth JSON.
  const validateCodexAuth = useCallback((value: string): string => {
    if (!value.trim()) return "";
    try {
      const parsed = JSON.parse(value);
      if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
        return "Auth JSON must be an object";
      }
      return "";
    } catch {
      return "Invalid JSON format";
    }
  }, []);

  // Set and validate auth JSON.
  const setCodexAuth = useCallback(
    (value: string) => {
      setCodexAuthState(value);
      setCodexAuthError(validateCodexAuth(value));
    },
    [validateCodexAuth],
  );

  // Set config, including functional updates.
  const setCodexConfig = useCallback(
    (value: string | ((prev: string) => string)) => {
      setCodexConfigState((prev) =>
        typeof value === "function"
          ? (value as (input: string) => string)(prev)
          : value,
      );
    },
    [],
  );

  // Write Codex API key input back to auth.json.
  // If config.toml contains experimental_bearer_token in its mobile-compatible form,
  // update or clear it too. Otherwise, pickCodexApiKey would restore a value the user cleared.
  const handleCodexApiKeyChange = useCallback(
    (key: string) => {
      const trimmed = key.trim();
      setCodexApiKey(trimmed);
      try {
        const auth = JSON.parse(codexAuth || "{}");
        auth.OPENAI_API_KEY = trimmed;
        setCodexAuth(JSON.stringify(auth, null, 2));
      } catch {
        // ignore
      }
      setCodexConfig((prev) =>
        updateCodexExperimentalBearerToken(prev, trimmed),
      );
    },
    [codexAuth, setCodexAuth, setCodexConfig],
  );

  // Handle Codex base URL changes.
  const handleCodexBaseUrlChange = useCallback(
    (url: string) => {
      const sanitized = url.trim();
      setCodexBaseUrl(sanitized);

      isUpdatingCodexBaseUrlRef.current = true;
      setCodexConfig((prev) => setCodexBaseUrlInConfig(prev, sanitized));
      setTimeout(() => {
        isUpdatingCodexBaseUrlRef.current = false;
      }, 0);
    },
    [setCodexConfig],
  );

  // Handle config changes and synchronize the base URL.
  const handleCodexConfigChange = useCallback(
    (value: string) => {
      // Normalize CJK, full-width, and curly quotation marks to prevent TOML parse errors.
      const normalized = normalizeTomlText(value);
      setCodexConfig(normalized);

      if (!isUpdatingCodexBaseUrlRef.current) {
        const extracted = extractCodexBaseUrl(normalized) || "";
        if (extracted !== codexBaseUrl) {
          setCodexBaseUrl(extracted);
        }
      }
    },
    [setCodexConfig, codexBaseUrl],
  );

  // Reset configuration when switching presets.
  const resetCodexConfig = useCallback(
    (
      auth: Record<string, unknown>,
      config: string,
      modelCatalogModels: CodexCatalogModel[] = [],
    ) => {
      const authString = JSON.stringify(auth, null, 2);
      setCodexAuth(authString);
      setCodexConfig(config);
      setCodexCatalogModels(modelCatalogModels);

      const baseUrl = extractCodexBaseUrl(config);
      setCodexBaseUrl(baseUrl || "");

      setCodexApiKey(pickCodexApiKey(auth, config));
    },
    [setCodexAuth, setCodexConfig, setCodexCatalogModels],
  );

  return {
    codexAuth,
    codexConfig,
    codexApiKey,
    codexBaseUrl,
    codexCatalogModels,
    codexAuthError,
    setCodexAuth,
    setCodexConfig,
    setCodexCatalogModels,
    handleCodexApiKeyChange,
    handleCodexBaseUrlChange,
    handleCodexConfigChange,
    resetCodexConfig,
    getCodexAuthApiKey,
    validateCodexAuth,
  };
}
