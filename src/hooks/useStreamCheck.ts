import { useState, useCallback } from "react";
import { toast } from "sonner";
import { useTranslation } from "react-i18next";
import {
  streamCheckProvider,
  type StreamCheckResult,
} from "@/lib/api/model-test";
import type { AppId } from "@/lib/api";

/**
 * Provider connectivity check.
 *
 * Probes only whether base_url is reachable; any HTTP response counts, and no
 * real model request is sent. It intentionally does not reset failover circuit
 * breakers: reachable does not mean correctly configured, and a provider with
 * an open port but invalid authentication must not be treated as healthy and
 * restored to service. Only real forwarded traffic drives circuit breakers;
 * see proxy/forwarder.rs.
 */
export function useStreamCheck(appId: AppId) {
  const { t } = useTranslation();
  const [checkingIds, setCheckingIds] = useState<Set<string>>(new Set());

  const checkProvider = useCallback(
    async (
      providerId: string,
      providerName: string,
    ): Promise<StreamCheckResult | null> => {
      setCheckingIds((prev) => new Set(prev).add(providerId));

      try {
        const result = await streamCheckProvider(appId, providerId);

        if (result.status === "operational") {
          toast.success(
            t("streamCheck.reachable", {
              providerName: providerName,
              responseTimeMs: result.responseTimeMs,
              defaultValue:
                "{{providerName}} is reachable ({{responseTimeMs}}ms)",
            }),
            { closeButton: true },
          );
        } else if (result.status === "degraded") {
          toast.warning(
            t("streamCheck.reachableSlow", {
              providerName: providerName,
              responseTimeMs: result.responseTimeMs,
              defaultValue:
                "{{providerName}} reachable but slow ({{responseTimeMs}}ms)",
            }),
          );
        } else {
          // This path is reserved for connection failures such as DNS,
          // connection refusal, TLS, or timeout.
          toast.error(
            t("streamCheck.unreachable", {
              providerName: providerName,
              message: result.message,
              defaultValue: "{{providerName}} unreachable: {{message}}",
            }),
            {
              description: t("streamCheck.unreachableHint", {
                defaultValue:
                  "Could not establish a connection (DNS / connect / TLS / timeout). Check the base_url and your network.",
              }),
              duration: 8000,
              closeButton: true,
            },
          );
        }

        return result;
      } catch (e) {
        toast.error(
          t("streamCheck.error", {
            providerName: providerName,
            error: String(e),
            defaultValue: "{{providerName}} check error: {{error}}",
          }),
        );
        return null;
      } finally {
        setCheckingIds((prev) => {
          const next = new Set(prev);
          next.delete(providerId);
          return next;
        });
      }
    },
    [appId, t],
  );

  const isChecking = useCallback(
    (providerId: string) => checkingIds.has(providerId),
    [checkingIds],
  );

  return { checkProvider, isChecking };
}
