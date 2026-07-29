import { settingsApi } from "@/lib/api";

/**
 * Shared post-change synchronization utility that writes the current provider
 * back to the corresponding application's live configuration. It does not
 * throw; callers choose notification behavior from the returned result.
 */
export async function syncCurrentProvidersLiveSafe(): Promise<{
  ok: boolean;
  error?: Error;
}> {
  try {
    await settingsApi.syncCurrentProvidersLive();
    return { ok: true };
  } catch (err) {
    const error = err instanceof Error ? err : new Error(String(err ?? ""));
    return { ok: false, error };
  }
}
