import { useEffect } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useQueryClient } from "@tanstack/react-query";
import { usageKeys } from "@/lib/query/usage";

/**
 * Listens for the backend `usage-log-recorded` event and immediately
 * invalidates UsageDashboard queries so users do not wait for the 30-second
 * polling interval.
 *
 * The backend emits this event, debounced over 200 ms, when a row is written to
 * `proxy_request_logs`. Sources include routing logs, Claude/Codex/Gemini
 * session synchronization, and startup archiving.
 *
 * This hook is mounted only in UsageDashboard to avoid irrelevant refreshes
 * elsewhere in the main interface.
 */
export function useUsageEventBridge() {
  const queryClient = useQueryClient();

  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let disposed = false;

    (async () => {
      const off = await listen("usage-log-recorded", () => {
        // Invalidate the entire usage namespace so summary, trends,
        // providerStats, modelStats, and logs all refetch.
        queryClient.invalidateQueries({ queryKey: usageKeys.all });
      });

      if (disposed) {
        off();
      } else {
        unlisten = off;
      }
    })();

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [queryClient]);
}
