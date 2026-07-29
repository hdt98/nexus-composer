import { useCallback, useEffect, useState } from "react";
import { settingsApi } from "@/lib/api";

export interface UseSettingsMetadataResult {
  isPortable: boolean;
  requiresRestart: boolean;
  isLoading: boolean;
  acknowledgeRestart: () => void;
  setRequiresRestart: (value: boolean) => void;
}

/**
 * useSettingsMetadata manages:
 * - isPortable mode.
 * - The requiresRestart flag.
 */
export function useSettingsMetadata(): UseSettingsMetadataResult {
  const [isPortable, setIsPortable] = useState(false);
  const [requiresRestart, setRequiresRestart] = useState(false);
  const [isLoading, setIsLoading] = useState(true);

  // Load metadata.
  useEffect(() => {
    let active = true;
    setIsLoading(true);

    const load = async () => {
      try {
        const portable = await settingsApi.isPortable();

        if (!active) return;

        setIsPortable(portable);
      } catch (error) {
        console.error("[useSettingsMetadata] Failed to load metadata", error);
      } finally {
        if (active) {
          setIsLoading(false);
        }
      }
    };

    void load();
    return () => {
      active = false;
    };
  }, []);

  const acknowledgeRestart = useCallback(() => {
    setRequiresRestart(false);
  }, []);

  return {
    isPortable,
    requiresRestart,
    isLoading,
    acknowledgeRestart,
    setRequiresRestart,
  };
}
