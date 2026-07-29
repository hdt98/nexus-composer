import type { CustomEndpoint, ProviderMeta } from "@/types";

/**
 * Merges custom endpoints into provider metadata.
 * - An empty customEndpoints object explicitly removes custom endpoints while
 *   preserving other metadata.
 * - null or undefined leaves existing endpoints unchanged.
 * - A populated customEndpoints object replaces existing custom endpoints.
 * - An empty result returns undefined unless endpoints were explicitly
 *   cleared, preventing an empty meta object from being written.
 */
export function mergeProviderMeta(
  initialMeta: ProviderMeta | undefined,
  customEndpoints: Record<string, CustomEndpoint> | null | undefined,
): ProviderMeta | undefined {
  const hasCustomEndpoints =
    !!customEndpoints && Object.keys(customEndpoints).length > 0;

  // An empty object explicitly requests removal of all endpoints.
  const isExplicitClear =
    customEndpoints !== null &&
    customEndpoints !== undefined &&
    Object.keys(customEndpoints).length === 0;

  if (hasCustomEndpoints) {
    return {
      ...(initialMeta ? { ...initialMeta } : {}),
      custom_endpoints: customEndpoints!,
    };
  }

  // Explicitly clear endpoints.
  if (isExplicitClear) {
    if (!initialMeta) {
      // A new provider without endpoints should not normally reach this path.
      return undefined;
    }

    if ("custom_endpoints" in initialMeta) {
      const { custom_endpoints, ...rest } = initialMeta;
      // Preserve other fields such as usage_script. Return an empty object when
      // needed so the backend knows to clear meta.
      return Object.keys(rest).length > 0 ? rest : {};
    }

    // initialMeta did not contain custom_endpoints.
    return { ...initialMeta };
  }

  // null or undefined means the user did not modify endpoints.
  if (!initialMeta) {
    return undefined;
  }

  if ("custom_endpoints" in initialMeta) {
    const { custom_endpoints, ...rest } = initialMeta;
    return Object.keys(rest).length > 0 ? rest : undefined;
  }

  return { ...initialMeta };
}
