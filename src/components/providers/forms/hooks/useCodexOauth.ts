import { useManagedAuth } from "./useManagedAuth";

/**
 * Codex OAuth authentication hook for ChatGPT Plus/Pro.
 *
 * Reuses useManagedAuth with provider set to "codex_oauth".
 */
export function useCodexOauth() {
  return useManagedAuth("codex_oauth");
}
