import { useState, useCallback, useEffect, useRef } from "react";
import TOML from "smol-toml";

/**
 * Validates Codex config.toml syntax in real time with smol-toml and
 * debouncing.
 */
export function useCodexTomlValidation() {
  const [configError, setConfigError] = useState("");
  const debounceTimerRef = useRef<NodeJS.Timeout | null>(null);

  /**
   * Validates TOML syntax.
   * @param tomlText TOML text to validate.
   * @returns Whether validation succeeded.
   */
  const validateToml = useCallback((tomlText: string): boolean => {
    // Treat an empty string as valid.
    if (!tomlText.trim()) {
      setConfigError("");
      return true;
    }

    try {
      TOML.parse(tomlText);
      setConfigError("");
      return true;
    } catch (error) {
      const errorMessage =
        error instanceof Error ? error.message : "Invalid TOML format";
      setConfigError(errorMessage);
      return false;
    }
  }, []);

  /**
   * Debounced validation with a 500 ms delay.
   * @param tomlText TOML text to validate.
   */
  const debouncedValidate = useCallback(
    (tomlText: string) => {
      // Clear the previous timer.
      if (debounceTimerRef.current) {
        clearTimeout(debounceTimerRef.current);
      }

      // Start a new timer.
      debounceTimerRef.current = setTimeout(() => {
        validateToml(tomlText);
      }, 500);
    },
    [validateToml],
  );

  /**
   * Clears the error message.
   */
  const clearError = useCallback(() => {
    setConfigError("");
  }, []);

  // Clean up the timer.
  useEffect(() => {
    return () => {
      if (debounceTimerRef.current) {
        clearTimeout(debounceTimerRef.current);
      }
    };
  }, []);

  return {
    configError,
    validateToml,
    debouncedValidate,
    clearError,
  };
}
