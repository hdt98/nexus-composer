import { useRef } from "react";
import {
  useQuery,
  type UseQueryResult,
  keepPreviousData,
} from "@tanstack/react-query";
import {
  providersApi,
  settingsApi,
  usageApi,
  sessionsApi,
  type AppId,
} from "@/lib/api";
import type {
  Provider,
  Settings,
  UsageResult,
  SessionMeta,
  SessionMessage,
} from "@/types";
import { usageKeys } from "@/lib/query/usage";

const sortProviders = (
  providers: Record<string, Provider>,
): Record<string, Provider> => {
  const sortedEntries = Object.values(providers)
    .sort((a, b) => {
      const indexA = a.sortIndex ?? Number.MAX_SAFE_INTEGER;
      const indexB = b.sortIndex ?? Number.MAX_SAFE_INTEGER;
      if (indexA !== indexB) {
        return indexA - indexB;
      }

      const timeA = a.createdAt ?? 0;
      const timeB = b.createdAt ?? 0;
      if (timeA === timeB) {
        return a.name.localeCompare(b.name, "en-US");
      }
      return timeA - timeB;
    })
    .map((provider) => [provider.id, provider] as const);

  return Object.fromEntries(sortedEntries);
};

export interface ProvidersQueryData {
  providers: Record<string, Provider>;
  currentProviderId: string;
}

export interface UseProvidersQueryOptions {
  isProxyRunning?: boolean; // Whether the proxy service is running
}

export const useProvidersQuery = (
  appId: AppId,
  options?: UseProvidersQueryOptions,
): UseQueryResult<ProvidersQueryData> => {
  const { isProxyRunning = false } = options || {};

  return useQuery({
    queryKey: ["providers", appId],
    placeholderData: keepPreviousData,
    // While the proxy is running, refresh providers every 10 seconds so
    // circuit-breaker changes to disabled proxy targets appear automatically.
    refetchInterval: isProxyRunning ? 10000 : false,
    queryFn: async () => {
      let providers: Record<string, Provider> = {};
      let currentProviderId = "";

      try {
        providers = await providersApi.getAll(appId);
      } catch (error) {
        console.error("Failed to get providers:", error);
      }

      try {
        currentProviderId = await providersApi.getCurrent(appId);
      } catch (error) {
        console.error("Failed to get the current provider:", error);
      }

      return {
        providers: sortProviders(providers),
        currentProviderId,
      };
    },
  });
};

export const useSettingsQuery = (): UseQueryResult<Settings> => {
  return useQuery({
    queryKey: ["settings"],
    queryFn: async () => settingsApi.get(),
  });
};

export interface UseUsageQueryOptions {
  enabled?: boolean;
  autoQueryInterval?: number; // Automatic query interval in minutes; 0 disables it
}

/** Most recent successful usage snapshot for keep-last-good behavior. */
export interface LastGoodUsage {
  data: UsageResult;
  at: number; // Time when the successful result was obtained, in milliseconds
}

/** How long failures may continue showing the last successful value. */
export const KEEP_LAST_GOOD_MS = 10 * 60 * 1000; // 10 minutes

/**
 * Determine whether a usage-query failure is transient or network-related and
 * may therefore be hidden briefly by keep-last-good.
 *
 * Only transient failures may preserve the last successful value. Deterministic
 * failures such as authentication errors, empty API keys, unknown providers,
 * 4xx responses, or script/parse errors must surface immediately so credential
 * changes do not leave stale quota values visible until the window expires.
 *
 * This uses an allowlist of stable backend network-error prefixes and HTTP 5xx
 * responses. Unrecognized errors fail safely as non-transient and surface
 * immediately. Keep these cases aligned with backend-owned English errors:
 * - Native balance/coding_plan/subscription request failures and timeouts use
 *   `"Network error: …"`; non-2xx upstream responses use
 *   `"API error (HTTP <code>…)"`.
 * - JavaScript usage_script `request_failed` uses "Request failed" and
 *   `read_response_failed` uses "Failed to read response"; non-2xx upstream
 *   responses use `"HTTP <code> …"`.
 *
 * HTTP 5xx server errors are usually transient. HTTP 4xx authentication or
 * client errors, including 401/403/404/429, remain deterministic and surface
 * immediately.
 */
export function isTransientUsageError(result: UsageResult): boolean {
  if (result.success) return false;
  const e = result.error?.toLowerCase() ?? "";
  if (!e) return false;

  // Network failures: send errors, timeouts, and response-read errors.
  if (
    e.includes("network error") || // Native path
    e.includes("request failed") || // JavaScript usage script
    e.includes("failed to read response") // JavaScript usage script
  ) {
    return true;
  }

  // Treat 5xx as transient and 4xx as deterministic. The first "HTTP <code>"
  // in the error is the upstream status, for example native
  // "API error (HTTP 500…)" or JavaScript "HTTP 500 …".
  const httpMatch = e.match(/http\s+(\d{3})/);
  if (httpMatch) {
    const status = Number(httpMatch[1]);
    return status >= 500 && status <= 599;
  }

  return false;
}

/**
 * Pure keep-last-good decision function. It has no ref or clock; `now` is
 * injected for tests.
 *
 * Cross-region and third-party endpoints can return `Ok(success:false)` after
 * a brief network interruption. Treating that as new data immediately replaces
 * a successful value and makes the card flicker between success and failure.
 * When a failure is transient (see [`isTransientUsageError`]) and the latest
 * success is within `keepMs`, keep displaying that success and point
 * `lastQueriedAt` to its timestamp. Outside the window, or without a previous
 * success, display the failure normally.
 *
 * Deterministic failures such as authentication errors, empty keys, unknown
 * providers, and 4xx responses also clear `lastGood`. The old snapshot is no
 * longer trustworthy and must not reappear after a later network interruption.
 *
 * React Query already retains previous data for rejected transport failures
 * such as Copilot or database errors. This primarily handles the
 * `Ok(success:false)` overwrite path.
 */
export function resolveDisplayUsage(
  raw: UsageResult | undefined,
  dataUpdatedAt: number,
  prevLastGood: LastGoodUsage | null,
  now: number,
  keepMs: number = KEEP_LAST_GOOD_MS,
): {
  data: UsageResult | undefined;
  lastQueriedAt: number | null;
  lastGood: LastGoodUsage | null;
} {
  let lastGood = prevLastGood;
  if (raw?.success) {
    // Refresh the snapshot after success.
    lastGood = { data: raw, at: dataUpdatedAt || now };
  } else if (raw && !isTransientUsageError(raw)) {
    // A deterministic failure makes the previous snapshot untrustworthy.
    // Discard it so a later network interruption cannot revive invalid quota.
    lastGood = null;
  }

  let data = raw;
  let lastQueriedAt = dataUpdatedAt || null;
  if (
    raw &&
    !raw.success &&
    isTransientUsageError(raw) && // Hide only transient failures; deterministic ones surface immediately.
    lastGood &&
    now - lastGood.at < keepMs
  ) {
    data = lastGood.data;
    lastQueriedAt = lastGood.at;
  }

  return { data, lastQueriedAt, lastGood };
}

export const useUsageQuery = (
  providerId: string,
  appId: AppId,
  options?: UseUsageQueryOptions,
) => {
  const { enabled = true, autoQueryInterval = 0 } = options || {};

  // Match staleTime to the automatic refresh interval, or default to five
  // minutes, to avoid duplicate queries when switching application pages.
  const staleTime =
    autoQueryInterval > 0
      ? autoQueryInterval * 60 * 1000 // Match the refresh interval.
      : 5 * 60 * 1000; // Default to five minutes.

  const query = useQuery<UsageResult>({
    queryKey: usageKeys.script(providerId, appId),
    queryFn: async () => usageApi.query(providerId, appId),
    enabled: enabled && !!providerId,
    refetchInterval:
      autoQueryInterval > 0
        ? Math.max(autoQueryInterval, 1) * 60 * 1000 // Minimum one minute
        : false,
    refetchIntervalInBackground: true, // Continue scheduled queries in the background.
    refetchOnWindowFocus: false,
    // Cross-region and third-party usage endpoints should tolerate one brief
    // network interruption or transient 5xx. Retry once, matching
    // useSubscriptionQuota. Native balance/coding_plan paths fold network
    // errors into Ok(success:false), so this primarily covers rejected
    // transport failures such as Copilot or database errors.
    retry: 1,
    retryDelay: 1500,
    staleTime, // Dynamically calculated cache freshness
    gcTime: 10 * 60 * 1000, // Retain cache for 10 minutes after unmount.
  });

  // Keep displaying the latest success for failures within the 10-minute
  // keep-last-good window (see resolveDisplayUsage). Each hook instance owns a
  // card-scoped ref, and repeated writes of the same success are idempotent.
  const lastGoodRef = useRef<LastGoodUsage | null>(null);
  const { data, lastQueriedAt, lastGood } = resolveDisplayUsage(
    query.data,
    query.dataUpdatedAt,
    lastGoodRef.current,
    Date.now(),
  );
  lastGoodRef.current = lastGood;

  return {
    ...query,
    data,
    lastQueriedAt,
  };
};

export const useSessionsQuery = () => {
  return useQuery<SessionMeta[]>({
    queryKey: ["sessions"],
    queryFn: async () => sessionsApi.list(),
    staleTime: 30 * 1000,
  });
};

export const useSessionMessagesQuery = (
  providerId?: string,
  sourcePath?: string,
) => {
  return useQuery<SessionMessage[]>({
    queryKey: ["sessionMessages", providerId, sourcePath],
    queryFn: async () => sessionsApi.getMessages(providerId!, sourcePath!),
    enabled: Boolean(providerId && sourcePath),
    staleTime: 30 * 1000,
  });
};
