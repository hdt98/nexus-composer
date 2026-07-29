import { useQueryClient } from "@tanstack/react-query";
import type { AppId } from "@/lib/api/types";
import type { UsageResult } from "@/types";
import type { SubscriptionQuota } from "@/types/subscription";
import { usageKeys } from "@/lib/query/usage";
import { subscriptionKeys } from "@/lib/query/subscription";
import { useTauriEvent } from "./useTauriEvent";

type UsageCacheUpdatedPayload =
  | {
      kind: "script";
      appType: AppId;
      providerId: string;
      data: UsageResult;
    }
  | {
      kind: "subscription";
      appType: AppId;
      data: SubscriptionQuota;
    };

/**
 * Backend UsageCache emits `usage-cache-updated` after writes. This hook
 * synchronizes the payload into React Query so tray-triggered refreshes that
 * bypass the frontend appear immediately, keeping React Query and Rust caches
 * consistent.
 */
export function useUsageCacheBridge() {
  const queryClient = useQueryClient();

  useTauriEvent<UsageCacheUpdatedPayload>("usage-cache-updated", (payload) => {
    if (payload.kind === "script") {
      queryClient.setQueryData<UsageResult>(
        usageKeys.script(payload.providerId, payload.appType),
        payload.data,
      );
    } else if (payload.kind === "subscription") {
      queryClient.setQueryData<SubscriptionQuota>(
        subscriptionKeys.quota(payload.appType),
        payload.data,
      );
    }
  });
}
