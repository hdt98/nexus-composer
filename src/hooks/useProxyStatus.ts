/**
 * Proxy service status hook.
 */

import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { toast } from "sonner";
import { useTranslation } from "react-i18next";
import type {
  ProxyStatus,
  ProxyServerInfo,
  ProxyTakeoverStatus,
} from "@/types/proxy";
import { extractErrorMessage } from "@/utils/errorUtils";

/**
 * Manage proxy service status.
 */
export function useProxyStatus() {
  const queryClient = useQueryClient();
  const { t } = useTranslation();

  // Query status with automatic polling.
  const { data: status, isLoading } = useQuery({
    queryKey: ["proxyStatus"],
    queryFn: () => invoke<ProxyStatus>("get_proxy_status"),
    // Poll only while the service is running.
    refetchInterval: (query) => (query.state.data?.running ? 2000 : false),
    // Retain previous data to prevent flicker.
    placeholderData: (previousData) => previousData,
  });

  // Query routing takeover status for each app.
  const { data: takeoverStatus } = useQuery({
    queryKey: ["proxyTakeoverStatus"],
    queryFn: () => invoke<ProxyTakeoverStatus>("get_proxy_takeover_status"),
    placeholderData: (previousData) => previousData,
  });

  // Start the server. The master switch starts only the service, without taking over app routing.
  const startProxyServerMutation = useMutation({
    mutationFn: () => invoke<ProxyServerInfo>("start_proxy_server"),
    onSuccess: (info) => {
      toast.success(
        t("proxy.server.started", {
          address: info.address,
          port: info.port,
          defaultValue: `Routing service started - ${info.address}:${info.port}`,
        }),
        { closeButton: true },
      );
      queryClient.invalidateQueries({ queryKey: ["proxyStatus"] });
    },
    onError: (error: Error) => {
      const detail =
        extractErrorMessage(error) ||
        t("common.unknown", { defaultValue: "Unknown" });
      toast.error(
        t("proxy.server.startFailed", {
          detail,
          defaultValue: `Failed to start routing service: ${detail}`,
        }),
      );
    },
  });

  // Stop only the server without rewriting or restoring takeover state for other apps.
  const stopProxyServerMutation = useMutation({
    mutationFn: () => invoke("stop_proxy_server"),
    onSuccess: () => {
      toast.success(
        t("proxy.server.stopped", {
          defaultValue: "Routing service stopped",
        }),
        { closeButton: true },
      );
      queryClient.invalidateQueries({ queryKey: ["proxyStatus"] });
    },
    onError: (error: Error) => {
      const detail =
        extractErrorMessage(error) ||
        t("common.unknown", { defaultValue: "Unknown" });
      toast.error(
        t("proxy.server.stopFailed", {
          detail,
          defaultValue: `Failed to stop routing service: ${detail}`,
        }),
      );
    },
  });

  // Stop the server through the master switch and restore every live config under takeover.
  const stopWithRestoreMutation = useMutation({
    mutationFn: () => invoke("stop_proxy_with_restore"),
    onSuccess: () => {
      toast.success(
        t("proxy.stoppedWithRestore", {
          defaultValue: "Routing service stopped, all routing configs restored",
        }),
        { closeButton: true },
      );
      queryClient.invalidateQueries({ queryKey: ["proxyStatus"] });
      queryClient.invalidateQueries({ queryKey: ["proxyTakeoverStatus"] });
      // Remove all provider health caches after the backend clears their database records.
      queryClient.removeQueries({ queryKey: ["providerHealth"] });
      // Remove all circuit breaker statistics caches because stopping the proxy resets their state.
      queryClient.removeQueries({ queryKey: ["circuitBreakerStats"] });
      // Failover queues and toggle states persist and do not need to be refreshed.
    },
    onError: (error: Error) => {
      const detail =
        extractErrorMessage(error) ||
        t("common.unknown", { defaultValue: "Unknown" });
      toast.error(
        t("proxy.stopWithRestoreFailed", {
          detail,
          defaultValue: `Stop failed: ${detail}`,
        }),
      );
    },
  });

  // Enable or disable routing takeover per app.
  const setTakeoverForAppMutation = useMutation({
    mutationFn: ({ appType, enabled }: { appType: string; enabled: boolean }) =>
      invoke("set_proxy_takeover_for_app", { appType, enabled }),
    onSuccess: (_data, variables) => {
      const appLabel =
        variables.appType === "claude"
          ? "Claude"
          : variables.appType === "codex"
            ? "Codex"
            : variables.appType === "gemini"
              ? "Gemini"
              : "OpenCode";

      toast.success(
        variables.enabled
          ? t("proxy.takeover.enabled", {
              app: appLabel,
              defaultValue: `${appLabel} routing enabled`,
            })
          : t("proxy.takeover.disabled", {
              app: appLabel,
              defaultValue: `${appLabel} routing disabled`,
            }),
        { closeButton: true },
      );

      queryClient.invalidateQueries({ queryKey: ["proxyStatus"] });
      queryClient.invalidateQueries({ queryKey: ["proxyTakeoverStatus"] });
    },
    onError: (error: Error) => {
      const detail =
        extractErrorMessage(error) ||
        t("common.unknown", { defaultValue: "Unknown" });
      toast.error(
        t("proxy.takeover.failed", {
          detail,
          defaultValue: `Failed to toggle routing: ${detail}`,
        }),
      );
    },
  });

  // Hot-switch providers while proxy routing is active.
  const switchProxyProviderMutation = useMutation({
    mutationFn: ({
      appType,
      providerId,
    }: {
      appType: string;
      providerId: string;
    }) => invoke("switch_proxy_provider", { appType, providerId }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["proxyStatus"] });
    },
    onError: (error: Error) => {
      const detail =
        extractErrorMessage(error) ||
        t("common.unknown", { defaultValue: "Unknown" });
      toast.error(
        t("proxy.switchFailed", {
          error: detail,
          defaultValue: `Switch failed: ${detail}`,
        }),
      );
    },
  });

  // Check whether the proxy is running.
  const checkRunning = async () => {
    try {
      return await invoke<boolean>("is_proxy_running");
    } catch {
      return false;
    }
  };

  // Check whether routing takeover is active.
  const checkTakeoverActive = async () => {
    try {
      return await invoke<boolean>("is_live_takeover_active");
    } catch {
      return false;
    }
  };

  return {
    status,
    isLoading,
    isRunning: status?.running || false,
    takeoverStatus,
    isTakeoverActive:
      takeoverStatus?.claude ||
      takeoverStatus?.codex ||
      takeoverStatus?.gemini ||
      false,

    // Start or stop through the master switch.
    startProxyServer: startProxyServerMutation.mutateAsync,
    stopProxyServer: stopProxyServerMutation.mutateAsync,
    stopWithRestore: stopWithRestoreMutation.mutateAsync,

    // Per-app routing takeover toggle.
    setTakeoverForApp: setTakeoverForAppMutation.mutateAsync,

    // Switch providers while proxy routing is active.
    switchProxyProvider: switchProxyProviderMutation.mutateAsync,

    // Status checks.
    checkRunning,
    checkTakeoverActive,

    // Loading states.
    isStarting: startProxyServerMutation.isPending,
    isStoppingServer: stopProxyServerMutation.isPending,
    isStopping: stopWithRestoreMutation.isPending,
    isPending:
      startProxyServerMutation.isPending ||
      stopProxyServerMutation.isPending ||
      stopWithRestoreMutation.isPending ||
      setTakeoverForAppMutation.isPending,
  };
}
