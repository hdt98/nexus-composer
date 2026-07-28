import { useEffect, useMemo, useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  AlertTriangle,
  BarChart3,
  CheckCircle2,
  ChevronDown,
  Download,
  FileText,
  Loader2,
  Plus,
  Power,
  RefreshCw,
  Server,
  Trash2,
  UploadCloud,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import type { Provider } from "@/types";
import type { AppId } from "@/lib/api";
import {
  providersApi,
  type RemoteProviderState,
  type RemoteUsageSyncProgressEvent,
  type SshConnectionTarget,
} from "@/lib/api/providers";
import { extractErrorMessage } from "@/utils/errorUtils";
import { cn } from "@/lib/utils";
import {
  getSavedSshConnectionKey,
  loadSavedSshConnections,
  removeSavedSshConnection,
  saveSshConnection,
  updateSavedSshConnectionSnapshot,
  type SavedSshConnection,
} from "@/components/remote/savedSshConnection";
import {
  resolveRemoteAgentProviderId,
  resolveRemoteModelLabel,
  resolveRemoteProviderLabel,
  savedSshConnectionToTarget,
} from "@/components/remote/remoteStatus";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ProviderIcon } from "@/components/ProviderIcon";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Label } from "@/components/ui/label";

interface RemoteProviderPageProps {
  appId: AppId;
  providers: Record<string, Provider>;
  currentProviderId: string;
  isLoading?: boolean;
}

const SUPPORTED_REMOTE_APPS: AppId[] = ["claude", "codex", "gemini"];

const formatTargetLabel = (target: SshConnectionTarget | null) => {
  if (!target) return "";
  if (target.type === "config") return target.alias;

  const userPrefix = target.user ? `${target.user}@` : "";
  const portSuffix = target.port ? `:${target.port}` : "";
  return `${userPrefix}${target.host}${portSuffix}`;
};

const getTargetKey = (target: SshConnectionTarget | null) => {
  if (!target) return "";
  if (target.type === "config") return `config:${target.alias}`;
  return `manual:${target.user ?? ""}@${target.host}:${target.port ?? ""}`;
};

const areTargetsEqual = (
  left: SshConnectionTarget | null,
  right: SshConnectionTarget | null,
) => {
  if (!left || !right || left.type !== right.type) return false;
  if (left.type === "config" && right.type === "config") {
    return left.alias === right.alias;
  }
  if (left.type === "manual" && right.type === "manual") {
    return (
      left.host === right.host &&
      (left.user ?? "") === (right.user ?? "") &&
      (left.port ?? undefined) === (right.port ?? undefined) &&
      (left.password ?? "") === (right.password ?? "")
    );
  }
  return false;
};

const formatBytes = (bytes?: number) => {
  if (!bytes || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  let value = bytes;
  let unitIndex = 0;
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex += 1;
  }
  const digits = value >= 10 || unitIndex === 0 ? 0 : 1;
  return `${value.toFixed(digits)} ${units[unitIndex]}`;
};

const createUsageSyncId = () =>
  `remote-usage-${Date.now()}-${Math.random().toString(36).slice(2)}`;

const getProviderSummary = (provider: Provider, appId: AppId) => {
  const config = provider.settingsConfig ?? {};
  if (appId === "claude") {
    return (
      config.env?.ANTHROPIC_BASE_URL ||
      config.env?.ANTHROPIC_MODEL ||
      provider.notes ||
      provider.websiteUrl ||
      ""
    );
  }

  if (appId === "codex") {
    const configText = typeof config.config === "string" ? config.config : "";
    const baseUrl = configText.match(/base_url\s*=\s*"([^"]+)"/)?.[1];
    return baseUrl || config.auth?.OPENAI_API_BASE || provider.notes || "";
  }

  if (appId === "gemini") {
    return (
      config.env?.GOOGLE_GEMINI_BASE_URL ||
      config.env?.GEMINI_MODEL ||
      provider.notes ||
      ""
    );
  }

  return provider.notes || "";
};

export function RemoteProviderPage({
  appId,
  providers,
  currentProviderId,
  isLoading = false,
}: RemoteProviderPageProps) {
  const { t, i18n } = useTranslation();
  const queryClient = useQueryClient();
  const isSupported = SUPPORTED_REMOTE_APPS.includes(appId);
  const [savedSshConnections, setSavedSshConnections] = useState(
    loadSavedSshConnections,
  );
  const initialSavedSshConnection = savedSshConnections[0];
  const [selectedSavedConnectionKey, setSelectedSavedConnectionKey] = useState(
    initialSavedSshConnection
      ? getSavedSshConnectionKey(initialSavedSshConnection)
      : "",
  );
  const [manualHost, setManualHost] = useState(
    initialSavedSshConnection?.host ?? "",
  );
  const [manualUser, setManualUser] = useState(
    initialSavedSshConnection?.user ?? "",
  );
  const [manualPort, setManualPort] = useState(
    String(initialSavedSshConnection?.port ?? 22),
  );
  const [manualPassword, setManualPassword] = useState("");
  const [connectionEditorOpen, setConnectionEditorOpen] = useState(false);
  const [expandedConnectionKey, setExpandedConnectionKey] = useState("");
  const [connectedTarget, setConnectedTarget] =
    useState<SshConnectionTarget | null>(() =>
      initialSavedSshConnection
        ? savedSshConnectionToTarget(initialSavedSshConnection)
        : null,
    );
  const [connectionVersion, setConnectionVersion] = useState(0);
  const [confirmApplyProvider, setConfirmApplyProvider] =
    useState<Provider | null>(null);
  const activeUsageSyncIdRef = useRef<string | null>(null);
  const autoProbeStartedRef = useRef(new Set<string>());
  const [usageSyncProgress, setUsageSyncProgress] =
    useState<RemoteUsageSyncProgressEvent | null>(null);
  const [agentLogs, setAgentLogs] = useState("");

  const populateConnectionForm = (
    connection: SavedSshConnection | undefined,
    probeConnection = false,
  ) => {
    setManualHost(connection?.host ?? "");
    setManualUser(connection?.user ?? "");
    setManualPort(String(connection?.port ?? 22));
    setManualPassword("");
    setConnectedTarget(
      connection && probeConnection
        ? savedSshConnectionToTarget(connection)
        : null,
    );
    if (connection && probeConnection) {
      setConnectionVersion((value) => value + 1);
    }
  };

  const toggleSavedConnection = (connectionKey: string) => {
    if (expandedConnectionKey === connectionKey) {
      setExpandedConnectionKey("");
      return;
    }

    const connection = savedSshConnections.find(
      (item) => getSavedSshConnectionKey(item) === connectionKey,
    );
    if (!connection) return;

    setSelectedSavedConnectionKey(connectionKey);
    populateConnectionForm(connection, true);
    setConnectionEditorOpen(false);
    setExpandedConnectionKey(connectionKey);
  };

  const startNewConnection = () => {
    setSelectedSavedConnectionKey("");
    populateConnectionForm(undefined);
    setExpandedConnectionKey("");
    setConnectionEditorOpen(true);
  };

  const removeSelectedConnection = () => {
    if (!selectedSavedConnectionKey) return;

    const remainingConnections = removeSavedSshConnection(
      selectedSavedConnectionKey,
    );
    setSavedSshConnections(remainingConnections);
    const nextConnection = remainingConnections[0];
    setSelectedSavedConnectionKey(
      nextConnection ? getSavedSshConnectionKey(nextConnection) : "",
    );
    populateConnectionForm(nextConnection, Boolean(nextConnection));
    setExpandedConnectionKey("");
  };

  const manualPortNumber = useMemo(() => {
    const trimmed = manualPort.trim();
    if (!trimmed) return undefined;
    const value = Number(trimmed);
    return Number.isInteger(value) && value > 0 && value <= 65535
      ? value
      : null;
  }, [manualPort]);

  const selectedTarget = useMemo<SshConnectionTarget | null>(() => {
    const host = manualHost.trim();
    if (!host || manualPortNumber === null) return null;
    return {
      type: "manual",
      host,
      user: manualUser.trim() || undefined,
      port: manualPortNumber,
      password: manualPassword || undefined,
    };
  }, [manualHost, manualPassword, manualPortNumber, manualUser]);

  const selectedTargetKey = useMemo(
    () => getTargetKey(selectedTarget),
    [selectedTarget],
  );
  const connectedTargetKey = useMemo(
    () => getTargetKey(connectedTarget),
    [connectedTarget],
  );
  const connectedHost = useMemo(
    () => formatTargetLabel(connectedTarget),
    [connectedTarget],
  );

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;

    void providersApi
      .onRemoteUsageSyncProgress((event) => {
        if (event.syncId === activeUsageSyncIdRef.current) {
          setUsageSyncProgress((current) => {
            if (event.stage !== "failed" || current?.syncId !== event.syncId) {
              return event;
            }

            return {
              ...current,
              ...event,
              currentStep: current.currentStep,
              totalSteps: current.totalSteps,
              fileCount: event.fileCount ?? current.fileCount,
              payloadBytes: event.payloadBytes ?? current.payloadBytes,
              filesScanned: event.filesScanned ?? current.filesScanned,
              imported: event.imported ?? current.imported,
              skipped: event.skipped ?? current.skipped,
              unchangedFiles: event.unchangedFiles ?? current.unchangedFiles,
            };
          });
        }
      })
      .then((nextUnlisten) => {
        if (disposed) {
          nextUnlisten();
          return;
        }
        unlisten = nextUnlisten;
      });

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  const remoteQuery = useQuery<RemoteProviderState>({
    queryKey: [
      "remoteProviderState",
      appId,
      connectedTargetKey,
      connectionVersion,
    ],
    queryFn: () => {
      if (!connectedTarget) {
        throw new Error("SSH target is not connected");
      }
      return providersApi.inspectRemote(appId, connectedTarget);
    },
    enabled: isSupported && Boolean(connectedTarget),
    retry: false,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
    staleTime: Infinity,
  });

  useEffect(() => {
    if (
      !remoteQuery.isSuccess ||
      !connectedTarget ||
      connectedTarget.type !== "manual"
    ) {
      return;
    }

    const connection = {
      host: connectedTarget.host,
      user: connectedTarget.user,
      port: connectedTarget.port ?? 22,
    };
    const connections = saveSshConnection(
      connection,
      selectedSavedConnectionKey,
    );
    const savedConnectionKey = getSavedSshConnectionKey(connection);
    setSavedSshConnections(connections);
    setSelectedSavedConnectionKey(savedConnectionKey);
    if (connectionEditorOpen) {
      setConnectionEditorOpen(false);
      setExpandedConnectionKey("");
    }
  }, [
    connectedTarget,
    connectionEditorOpen,
    remoteQuery.isSuccess,
    selectedSavedConnectionKey,
  ]);

  const remoteAgentQuery = useQuery({
    queryKey: ["remoteAgentStatus", connectedTargetKey, connectionVersion],
    queryFn: () => {
      if (!connectedTarget) {
        throw new Error("SSH target is not connected");
      }
      return providersApi.getRemoteAgentStatus(connectedTarget);
    },
    enabled: appId === "codex" && Boolean(connectedTarget),
    retry: false,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
    staleTime: Infinity,
  });

  const localProvidersQuery = useQuery({
    queryKey: ["remoteLocalProviders", appId],
    queryFn: () => providersApi.getAll(appId),
    enabled: isSupported,
    initialData: providers,
  });

  const localProviderMap = localProvidersQuery.data ?? providers;
  const localProviders = useMemo(
    () => Object.values(localProviderMap),
    [localProviderMap],
  );

  const directlyMatchedProvider = useMemo(() => {
    const matchedId = remoteQuery.data?.matchedProviderId;
    return matchedId ? localProviderMap[matchedId] : undefined;
  }, [localProviderMap, remoteQuery.data?.matchedProviderId]);
  const agentCurrentProviderId = useMemo(
    () =>
      resolveRemoteAgentProviderId({
        agentStatus: remoteAgentQuery.data,
        providers: localProviderMap,
      }),
    [localProviderMap, remoteAgentQuery.data],
  );
  const remoteCurrentProviderId = remoteAgentQuery.data?.running
    ? agentCurrentProviderId
    : (remoteQuery.data?.matchedProviderId ?? "");
  const remoteCurrentProvider = remoteCurrentProviderId
    ? localProviderMap[remoteCurrentProviderId]
    : undefined;

  const remoteModelLabel = useMemo(
    () =>
      resolveRemoteModelLabel({
        appId,
        agentStatus: remoteAgentQuery.data,
        provider: remoteQuery.data?.provider,
        matchedProvider: remoteCurrentProvider ?? directlyMatchedProvider,
      }),
    [
      appId,
      directlyMatchedProvider,
      remoteAgentQuery.data,
      remoteCurrentProvider,
      remoteQuery.data?.provider,
    ],
  );
  const remoteProviderLabel = useMemo(
    () =>
      resolveRemoteProviderLabel({
        appId,
        agentStatus: remoteAgentQuery.data,
        matchedProvider: remoteCurrentProvider ?? directlyMatchedProvider,
        remoteProvider: remoteQuery.data?.provider,
      }),
    [
      appId,
      directlyMatchedProvider,
      remoteAgentQuery.data,
      remoteCurrentProvider,
      remoteQuery.data?.provider,
    ],
  );

  // Persist the result of each successful inspection. Saved connection cards
  // can then keep showing their last detected status/model when another host
  // becomes active, instead of falling back to "Saved / Not detected".
  useEffect(() => {
    if (
      !remoteQuery.isSuccess ||
      !connectedTarget ||
      connectedTarget.type !== "manual"
    ) {
      return;
    }
    const connectionKey = getSavedSshConnectionKey({
      host: connectedTarget.host,
      user: connectedTarget.user,
      port: connectedTarget.port ?? 22,
    });
    const lastCheckedAt = Math.max(
      remoteQuery.dataUpdatedAt,
      remoteAgentQuery.dataUpdatedAt,
    );
    setSavedSshConnections(
      updateSavedSshConnectionSnapshot(connectionKey, {
        lastStatus: remoteAgentQuery.data?.running ? "running" : "connected",
        lastProvider: remoteProviderLabel || undefined,
        lastModel: remoteModelLabel || undefined,
        lastCheckedAt: lastCheckedAt || Date.now(),
      }),
    );
  }, [
    connectedTarget,
    remoteAgentQuery.data?.running,
    remoteAgentQuery.dataUpdatedAt,
    remoteModelLabel,
    remoteProviderLabel,
    remoteQuery.dataUpdatedAt,
    remoteQuery.isSuccess,
  ]);

  useEffect(() => {
    if (
      !remoteQuery.isError ||
      !connectedTarget ||
      connectedTarget.type !== "manual"
    ) {
      return;
    }
    const connectionKey = getSavedSshConnectionKey({
      host: connectedTarget.host,
      user: connectedTarget.user,
      port: connectedTarget.port ?? 22,
    });
    setSavedSshConnections(
      updateSavedSshConnectionSnapshot(connectionKey, {
        lastStatus: "unreachable",
        lastCheckedAt: remoteQuery.errorUpdatedAt || Date.now(),
      }),
    );
  }, [connectedTarget, remoteQuery.errorUpdatedAt, remoteQuery.isError]);

  // Backfill legacy saved entries once per page load. This is a single SSH
  // inspection, not polling; it makes connections saved by older builds show
  // their real current config without forcing the user to open and switch them.
  useEffect(() => {
    let disposed = false;
    const activeKey = connectedTargetKey;
    const pending = savedSshConnections.filter((connection) => {
      const connectionKey = getSavedSshConnectionKey(connection);
      return (
        !connection.lastStatus &&
        getTargetKey(savedSshConnectionToTarget(connection)) !== activeKey &&
        !autoProbeStartedRef.current.has(connectionKey)
      );
    });
    if (pending.length === 0) return;

    void (async () => {
      for (const connection of pending) {
        if (disposed) return;
        const connectionKey = getSavedSshConnectionKey(connection);
        autoProbeStartedRef.current.add(connectionKey);
        try {
          const state = await providersApi.inspectRemote(
            appId,
            savedSshConnectionToTarget(connection),
          );
          if (disposed) return;
          const matchedProvider = state.matchedProviderId
            ? localProviderMap[state.matchedProviderId]
            : undefined;
          const model = resolveRemoteModelLabel({
            appId,
            provider: state.provider,
            matchedProvider,
          });
          const providerLabel = resolveRemoteProviderLabel({
            appId,
            matchedProvider,
            remoteProvider: state.provider,
          });
          setSavedSshConnections(
            updateSavedSshConnectionSnapshot(connectionKey, {
              lastStatus: "connected",
              lastProvider: providerLabel || undefined,
              lastModel: model || undefined,
              lastCheckedAt: Date.now(),
            }),
          );
        } catch {
          if (disposed) return;
          setSavedSshConnections(
            updateSavedSshConnectionSnapshot(connectionKey, {
              lastStatus: "unreachable",
              lastCheckedAt: Date.now(),
            }),
          );
        }
      }
    })();

    return () => {
      disposed = true;
    };
  }, [appId, connectedTargetKey, localProviderMap, savedSshConnections]);
  const statusTarget = connectedTarget ?? selectedTarget;
  const connectionStatus = remoteQuery.isFetching
    ? "checking"
    : remoteQuery.isSuccess
      ? "connected"
      : remoteQuery.isError
        ? "unreachable"
        : "notChecked";
  const agentStatus = remoteAgentQuery.isFetching
    ? "checking"
    : remoteAgentQuery.data?.running
      ? "running"
      : remoteAgentQuery.data?.installed
        ? "stopped"
        : remoteAgentQuery.isSuccess
          ? "notInstalled"
          : remoteAgentQuery.isError
            ? "unavailable"
            : "notChecked";
  const lastCheckedAt = Math.max(
    remoteQuery.dataUpdatedAt,
    remoteAgentQuery.dataUpdatedAt,
  );
  const lastCheckedLabel = lastCheckedAt
    ? new Date(lastCheckedAt).toLocaleTimeString(i18n.language)
    : "";

  const remoteWarnings = remoteQuery.data?.warnings ?? [];
  const remoteFiles = remoteQuery.data?.files ?? [];
  const hasExistingRemoteConfig =
    remoteQuery.data?.hasExistingConfig ??
    remoteFiles.some((file) => file.exists);
  const hasUnmanagedRemoteConfig =
    !remoteCurrentProvider &&
    (remoteQuery.data?.hasUnmanagedConfig ??
      (hasExistingRemoteConfig && !remoteQuery.data?.matchedProviderId));
  const overwriteWarning =
    remoteQuery.data?.overwriteWarning ??
    t("remote.overwriteWarning", {
      defaultValue: "远端已有配置。切换会覆盖这些文件，建议先同步到本地。",
    });
  const usageSyncStage = usageSyncProgress?.stage ?? "starting";
  const usageSyncStageLabel = usageSyncProgress
    ? t(`remote.usageSyncProgress.stages.${usageSyncStage}`, {
        defaultValue: usageSyncStage,
        count: usageSyncProgress.fileCount ?? 0,
        imported: usageSyncProgress.imported ?? 0,
        skipped: usageSyncProgress.skipped ?? 0,
        unchangedFiles: usageSyncProgress.unchangedFiles ?? 0,
        filesScanned: usageSyncProgress.filesScanned ?? 0,
      })
    : "";

  const applyMutation = useMutation({
    mutationFn: async ({
      provider,
      forceOverwrite,
      stopAgentFirst = false,
    }: {
      provider: Provider;
      forceOverwrite: boolean;
      stopAgentFirst?: boolean;
    }) => {
      if (!connectedTarget) {
        throw new Error("SSH target is not connected");
      }
      if (stopAgentFirst) {
        await providersApi.stopRemoteAgent(connectedTarget);
      }
      return await providersApi.applyToRemote(
        provider.id,
        appId,
        connectedTarget,
        forceOverwrite,
      );
    },
    onSuccess: async (result) => {
      setConfirmApplyProvider(null);
      toast.success(
        t("remote.applySuccess", {
          defaultValue: "远端已切换到选中的供应商",
        }),
        {
          description: `${result.hostAlias} - ${
            result.writtenFiles?.length ?? 0
          } files`,
        },
      );
      await queryClient.invalidateQueries({
        queryKey: ["remoteProviderState", appId, connectedTargetKey],
      });
      if (appId === "codex") {
        await queryClient.invalidateQueries({
          queryKey: ["remoteAgentStatus", connectedTargetKey],
        });
      }
    },
    onError: async (error: unknown, variables) => {
      if (variables.stopAgentFirst) {
        setConfirmApplyProvider(null);
        await queryClient.invalidateQueries({
          queryKey: ["remoteProviderState", appId, connectedTargetKey],
        });
        await queryClient.invalidateQueries({
          queryKey: ["remoteAgentStatus", connectedTargetKey],
        });
      }
      toast.error(
        t("remote.applyFailed", {
          defaultValue: "远端切换失败: {{error}}",
          error: extractErrorMessage(error),
        }),
        { duration: 7000 },
      );
    },
  });

  const deployAgentMutation = useMutation({
    mutationFn: ({ provider }: { provider: Provider }) => {
      if (!connectedTarget) {
        throw new Error("SSH target is not connected");
      }
      return providersApi.deployRemoteAgent(connectedTarget, provider.id);
    },
    onSuccess: async (result) => {
      const migratedSessions = result.historyPreparation?.jsonlFiles ?? 0;
      const reloadMessage = result.codexReload?.reconnectTriggered
        ? t("remote.codexReloadTriggered")
        : result.codexReload?.error
          ? t("remote.codexReloadFallback")
          : t("remote.codexReloadOnNextConnection");
      toast.success(
        result.running
          ? "Remote agent deployed and running"
          : "Remote agent deployed",
        {
          description: `${result.hostAlias}. ${reloadMessage} ${t(
            "remote.newCodexSessionRequired",
            {
              defaultValue:
                "Open a new session or reopen an existing one to load this provider. {{count}} existing sessions are available in GLM history.",
              count: migratedSessions,
            },
          )}`,
          duration: 7000,
        },
      );
      await remoteAgentQuery.refetch();
      await remoteQuery.refetch();
    },
    onError: (error: unknown) => {
      toast.error("Remote agent deploy failed", {
        description: extractErrorMessage(error),
        duration: 7000,
      });
    },
  });

  const switchAgentMutation = useMutation({
    mutationFn: ({ provider }: { provider: Provider }) => {
      if (!connectedTarget) {
        throw new Error("SSH target is not connected");
      }
      return providersApi.switchRemoteAgentProvider(
        connectedTarget,
        provider.id,
      );
    },
    onSuccess: async (result) => {
      const migratedSessions = result.historyPreparation?.jsonlFiles ?? 0;
      const reloadMessage = result.codexReload?.reconnectTriggered
        ? t("remote.codexReloadTriggered")
        : result.codexReload?.error
          ? t("remote.codexReloadFallback")
          : t("remote.codexReloadOnNextConnection");
      toast.success("Remote agent provider switched", {
        description: `${result.hostAlias}. ${reloadMessage} ${t(
          "remote.newCodexSessionRequired",
          {
            defaultValue:
              "Open a new session or reopen an existing one to load this provider. {{count}} existing sessions are available in GLM history.",
            count: migratedSessions,
          },
        )}`,
        duration: 7000,
      });
      await remoteAgentQuery.refetch();
      await remoteQuery.refetch();
    },
    onError: (error: unknown) => {
      toast.error("Remote agent switch failed", {
        description: extractErrorMessage(error),
        duration: 7000,
      });
    },
  });

  const stopAgentMutation = useMutation({
    mutationFn: () => {
      if (!connectedTarget) {
        throw new Error("SSH target is not connected");
      }
      return providersApi.stopRemoteAgent(connectedTarget);
    },
    onSuccess: async (result) => {
      const maintenance = result.history_maintenance;
      const reload = result.codex_reload;
      const maintainedThreads = Math.max(
        maintenance?.jsonlFiles ?? 0,
        maintenance?.stateRows ?? 0,
      );
      const descriptions: string[] = [];
      if (maintainedThreads > 0) {
        descriptions.push(
          t("remote.historyMaintained", {
            defaultValue:
              "Kept {{count}} existing Codex threads resumable with {{provider}} / {{model}}.",
            count: maintainedThreads,
            provider: maintenance?.targetProvider ?? "OpenAI",
            model: maintenance?.targetModel ?? "current model",
          }),
        );
      }
      descriptions.push(
        reload?.reconnectTriggered
          ? t("remote.codexReloadTriggered")
          : reload?.error
            ? t("remote.codexReloadFallback")
            : t("remote.codexReloadOnNextConnection"),
      );
      toast.success(
        t("remote.agentStopSuccess", {
          defaultValue: "Remote Agent stopped",
        }),
        { description: descriptions.join(" ") },
      );
      await remoteAgentQuery.refetch();
      await remoteQuery.refetch();
    },
    onError: (error: unknown) => {
      toast.error("Remote agent stop failed", {
        description: extractErrorMessage(error),
        duration: 7000,
      });
    },
  });

  const logsAgentMutation = useMutation({
    mutationFn: () => {
      if (!connectedTarget) {
        throw new Error("SSH target is not connected");
      }
      return providersApi.getRemoteAgentLogs(connectedTarget, 120);
    },
    onSuccess: setAgentLogs,
    onError: (error: unknown) => {
      toast.error("Remote agent logs failed", {
        description: extractErrorMessage(error),
      });
    },
  });

  const requestRemoteAgentProvider = (provider: Provider) => {
    if (provider.category === "official") return;
    if (remoteAgentQuery.data?.running) {
      switchAgentMutation.mutate({ provider });
      return;
    }
    deployAgentMutation.mutate({ provider });
  };

  const requestApplyProvider = (
    provider: Provider,
    isRemoteCurrent: boolean,
  ) => {
    if (isRemoteCurrent || applyMutation.isPending) return;

    if (remoteAgentQuery.data?.running || hasUnmanagedRemoteConfig) {
      setConfirmApplyProvider(provider);
      return;
    }

    applyMutation.mutate({ provider, forceOverwrite: false });
  };

  const importMutation = useMutation({
    mutationFn: () => {
      if (!connectedTarget) {
        throw new Error("SSH target is not connected");
      }
      return providersApi.importRemote(appId, connectedTarget);
    },
    onSuccess: async (result) => {
      toast.success(
        t("remote.importSuccess", {
          defaultValue: "已同步远端配置到本地",
        }),
        { description: result.provider.name },
      );
      await queryClient.invalidateQueries({ queryKey: ["providers", appId] });
      await queryClient.invalidateQueries({
        queryKey: ["remoteLocalProviders", appId],
      });
      await queryClient.invalidateQueries({
        queryKey: ["remoteProviderState", appId, connectedTargetKey],
      });
    },
    onError: (error: unknown) => {
      toast.error(
        t("remote.importFailed", {
          defaultValue: "同步远端配置失败: {{error}}",
          error: extractErrorMessage(error),
        }),
        { duration: 7000 },
      );
    },
  });

  const syncUsageMutation = useMutation({
    mutationFn: ({ syncId }: { syncId: string }) => {
      if (!connectedTarget) {
        throw new Error("SSH target is not connected");
      }
      return providersApi.syncRemoteUsage(appId, connectedTarget, syncId);
    },
    onSuccess: async (result, variables) => {
      setUsageSyncProgress((current) =>
        current?.syncId === variables.syncId
          ? {
              ...current,
              stage: "completed",
              currentStep: current.totalSteps || 5,
              filesScanned: result.filesScanned,
              imported: result.imported,
              skipped: result.skipped,
              unchangedFiles: result.unchangedFiles,
            }
          : current,
      );
      toast.success(
        t("remote.usageSyncSuccess", {
          defaultValue: "已同步远端用量",
        }),
        {
          description: t("remote.usageSyncDescription", {
            defaultValue:
              "导入 {{imported}} 条，跳过 {{skipped}} 条，未变化文件 {{unchangedFiles}} 个",
            imported: result.imported,
            skipped: result.skipped,
            unchangedFiles: result.unchangedFiles,
          }),
        },
      );
      if (result.errors?.length) {
        toast.warning(
          t("remote.usageSyncPartial", {
            defaultValue: "部分远端日志解析失败",
          }),
          {
            description: result.errors.slice(0, 2).join("\n"),
            duration: 8000,
          },
        );
      }
      await queryClient.invalidateQueries({ queryKey: ["usage"] });
    },
    onError: (error: unknown, variables) => {
      const errorMessage = extractErrorMessage(error);
      setUsageSyncProgress((current) => {
        const fallback: RemoteUsageSyncProgressEvent = {
          syncId: variables.syncId,
          hostAlias: connectedHost,
          app: appId,
          stage: "failed",
          currentStep: current?.currentStep ?? 0,
          totalSteps: current?.totalSteps ?? 5,
          fileCount: current?.fileCount,
          payloadBytes: current?.payloadBytes,
          filesScanned: current?.filesScanned,
          imported: current?.imported,
          skipped: current?.skipped,
          unchangedFiles: current?.unchangedFiles,
          error: errorMessage,
        };
        return current?.syncId === variables.syncId ? fallback : current;
      });
      toast.error(
        t("remote.usageSyncFailed", {
          defaultValue: "同步远端用量失败: {{error}}",
          error: errorMessage,
        }),
        { duration: 7000 },
      );
    },
  });

  const usageSyncCanClose =
    Boolean(usageSyncProgress) &&
    (!syncUsageMutation.isPending || usageSyncStage === "failed");

  const startRemoteUsageSync = () => {
    if (!connectedTarget || syncUsageMutation.isPending) return;

    const syncId = createUsageSyncId();
    activeUsageSyncIdRef.current = syncId;
    setUsageSyncProgress({
      syncId,
      hostAlias: formatTargetLabel(connectedTarget),
      app: appId,
      stage: "starting",
      currentStep: 0,
      totalSteps: 5,
    });
    syncUsageMutation.mutate({ syncId });
  };

  const isSelectedHostConnected =
    Boolean(selectedTarget) &&
    areTargetsEqual(selectedTarget, connectedTarget) &&
    remoteQuery.isSuccess;
  const isInitialConnectingSelectedHost =
    Boolean(selectedTargetKey) &&
    connectedTargetKey === selectedTargetKey &&
    remoteQuery.isFetching &&
    !remoteQuery.data;
  const isConnectionDetailsExpanded =
    Boolean(expandedConnectionKey) &&
    expandedConnectionKey === selectedSavedConnectionKey;
  const confirmStopsRemoteAgent =
    appId === "codex" &&
    confirmApplyProvider?.category === "official" &&
    Boolean(remoteAgentQuery.data?.running);

  const connectSelectedTarget = () => {
    if (!selectedTarget) return;
    setConnectedTarget(selectedTarget);
    setConnectionVersion((value) => value + 1);
  };

  if (!isSupported) {
    return (
      <div className="px-6 pt-4">
        <div className="rounded-lg border border-dashed border-border px-4 py-8 text-center text-sm text-muted-foreground">
          {t("remote.unsupported", {
            defaultValue: "远端配置暂时只支持 Claude、Codex 和 Gemini。",
          })}
        </div>
      </div>
    );
  }

  return (
    <div className="px-6 pt-4 pb-10 space-y-4">
      <section className="rounded-lg border border-border bg-card p-4">
        <div className="mb-3 flex items-center justify-between gap-3">
          <h2 className="text-sm font-semibold">
            {t("remote.savedConnections", {
              defaultValue: "Saved connections",
            })}
          </h2>
          <Button type="button" variant="outline" onClick={startNewConnection}>
            <Plus className="h-4 w-4" />
            {t("remote.addConnection", {
              defaultValue: "Add connection",
            })}
          </Button>
        </div>
        {savedSshConnections.length === 0 ? (
          <div className="rounded-lg border border-dashed border-border px-4 py-8 text-center text-sm text-muted-foreground">
            {t("remote.savedConnectionEmpty", {
              defaultValue: "No SSH connections saved yet.",
            })}
          </div>
        ) : (
          <div className="space-y-2">
            {savedSshConnections.map((connection) => {
              const connectionKey = getSavedSshConnectionKey(connection);
              const target = savedSshConnectionToTarget(connection);
              const isActive = connectedTargetKey === getTargetKey(target);
              const isExpanded = expandedConnectionKey === connectionKey;
              const isChecking =
                isActive &&
                (remoteQuery.isFetching ||
                  (appId === "codex" && remoteAgentQuery.isFetching));
              const summaryStatus = isChecking
                ? "checking"
                : isActive && remoteAgentQuery.data?.running
                  ? "running"
                  : isActive && remoteQuery.isSuccess
                    ? "connected"
                    : isActive && remoteQuery.isError
                      ? "unreachable"
                      : (connection.lastStatus ?? "saved");
              const summaryStatusLabel =
                summaryStatus === "running"
                  ? t("remote.agentStates.running", {
                      defaultValue: "Running",
                    })
                  : t(`remote.connectionStates.${summaryStatus}`, {
                      defaultValue: summaryStatus,
                    });

              return (
                <div
                  key={connectionKey}
                  className={cn(
                    "overflow-hidden rounded-lg border border-border transition-colors",
                    isExpanded && "border-primary/40",
                  )}
                >
                  <button
                    type="button"
                    className="grid w-full items-center gap-3 px-3 py-3 text-left transition-colors hover:bg-muted/50 sm:grid-cols-[auto_minmax(0,1.3fr)_minmax(0,0.8fr)_minmax(0,1fr)_auto]"
                    onClick={() => toggleSavedConnection(connectionKey)}
                    aria-expanded={isExpanded}
                  >
                    <Badge
                      variant="secondary"
                      className={cn(
                        "w-fit",
                        (summaryStatus === "running" ||
                          summaryStatus === "connected") &&
                          "bg-emerald-100 text-emerald-700 dark:bg-emerald-900/40 dark:text-emerald-300",
                        summaryStatus === "unreachable" &&
                          "bg-red-100 text-red-700 dark:bg-red-900/40 dark:text-red-300",
                      )}
                    >
                      {isChecking && (
                        <Loader2 className="mr-1 h-3 w-3 animate-spin" />
                      )}
                      {summaryStatusLabel}
                    </Badge>
                    <div className="min-w-0">
                      <p className="text-xs text-muted-foreground">
                        {t("remote.hostStatusLabel", { defaultValue: "Host" })}
                      </p>
                      <p className="truncate text-sm font-medium">
                        {connection.host}:{connection.port}
                      </p>
                    </div>
                    <div className="min-w-0">
                      <p className="text-xs text-muted-foreground">
                        {t("remote.userStatusLabel", { defaultValue: "User" })}
                      </p>
                      <p className="truncate text-sm font-medium">
                        {connection.user || "—"}
                      </p>
                    </div>
                    <div className="min-w-0">
                      <p className="text-xs text-muted-foreground">
                        {t("remote.modelStatus", { defaultValue: "Model" })}
                      </p>
                      <p className="truncate text-sm font-medium">
                        {(isActive && remoteModelLabel) ||
                          connection.lastModel ||
                          t("remote.notDetected", {
                            defaultValue: "Not detected",
                          })}
                      </p>
                    </div>
                    <ChevronDown
                      className={cn(
                        "h-4 w-4 text-muted-foreground transition-transform",
                        isExpanded && "rotate-180",
                      )}
                    />
                  </button>

                  {isExpanded && isActive && statusTarget && (
                    <div className="border-t border-border bg-muted/20 p-3">
                      <div className="flex flex-wrap items-center justify-between gap-2">
                        <p className="text-sm font-medium">
                          {formatTargetLabel(statusTarget)}
                        </p>
                        {lastCheckedLabel && (
                          <span className="text-xs text-muted-foreground">
                            {t("remote.lastChecked", {
                              defaultValue: "Last checked {{time}}",
                              time: lastCheckedLabel,
                            })}
                          </span>
                        )}
                      </div>

                      <div className="mt-3 grid gap-2 sm:grid-cols-2 lg:grid-cols-4">
                        <div className="rounded-md border border-border/70 bg-background/70 px-3 py-2">
                          <p className="text-xs text-muted-foreground">
                            {t("remote.sshStatus", { defaultValue: "SSH" })}
                          </p>
                          <Badge
                            variant="secondary"
                            className={cn(
                              "mt-1",
                              connectionStatus === "connected" &&
                                "bg-emerald-100 text-emerald-700 dark:bg-emerald-900/40 dark:text-emerald-300",
                              connectionStatus === "unreachable" &&
                                "bg-red-100 text-red-700 dark:bg-red-900/40 dark:text-red-300",
                            )}
                          >
                            {connectionStatus === "checking" && (
                              <Loader2 className="mr-1 h-3 w-3 animate-spin" />
                            )}
                            {t(`remote.connectionStates.${connectionStatus}`, {
                              defaultValue: connectionStatus,
                            })}
                          </Badge>
                        </div>

                        {appId === "codex" && (
                          <div className="rounded-md border border-border/70 bg-background/70 px-3 py-2">
                            <p className="text-xs text-muted-foreground">
                              {t("remote.agentStatus", {
                                defaultValue: "Remote Agent",
                              })}
                            </p>
                            <Badge
                              variant="secondary"
                              className={cn(
                                "mt-1",
                                agentStatus === "running" &&
                                  "bg-emerald-100 text-emerald-700 dark:bg-emerald-900/40 dark:text-emerald-300",
                                (agentStatus === "unavailable" ||
                                  agentStatus === "stopped") &&
                                  "bg-amber-100 text-amber-700 dark:bg-amber-900/40 dark:text-amber-300",
                              )}
                            >
                              {agentStatus === "checking" && (
                                <Loader2 className="mr-1 h-3 w-3 animate-spin" />
                              )}
                              {t(`remote.agentStates.${agentStatus}`, {
                                defaultValue: agentStatus,
                              })}
                            </Badge>
                          </div>
                        )}

                        <div className="rounded-md border border-border/70 bg-background/70 px-3 py-2">
                          <p className="text-xs text-muted-foreground">
                            {t("remote.providerStatus", {
                              defaultValue: "Provider",
                            })}
                          </p>
                          <p className="mt-1 truncate text-sm font-medium">
                            {remoteProviderLabel ||
                              t("remote.notDetected", {
                                defaultValue: "Not detected",
                              })}
                          </p>
                        </div>

                        <div className="rounded-md border border-border/70 bg-background/70 px-3 py-2">
                          <p className="text-xs text-muted-foreground">
                            {t("remote.modelStatus", { defaultValue: "Model" })}
                          </p>
                          <p className="mt-1 truncate text-sm font-medium">
                            {remoteModelLabel ||
                              t("remote.notDetected", {
                                defaultValue: "Not detected",
                              })}
                          </p>
                        </div>
                      </div>

                      <div className="mt-3 flex flex-wrap items-center gap-2">
                        <Button
                          type="button"
                          variant="outline"
                          size="sm"
                          onClick={connectSelectedTarget}
                          disabled={remoteQuery.isFetching}
                        >
                          <RefreshCw
                            className={cn(
                              "h-4 w-4",
                              remoteQuery.isFetching && "animate-spin",
                            )}
                          />
                          {t("remote.refreshStatus", {
                            defaultValue: "Refresh status",
                          })}
                        </Button>
                        <Button
                          type="button"
                          variant="outline"
                          size="sm"
                          onClick={startRemoteUsageSync}
                          disabled={
                            !remoteQuery.isSuccess ||
                            syncUsageMutation.isPending
                          }
                        >
                          {syncUsageMutation.isPending ? (
                            <Loader2 className="h-4 w-4 animate-spin" />
                          ) : (
                            <BarChart3 className="h-4 w-4" />
                          )}
                          {t("remote.syncUsage", {
                            defaultValue: "Sync usage",
                          })}
                        </Button>
                        <Button
                          type="button"
                          variant="outline"
                          size="sm"
                          onClick={removeSelectedConnection}
                        >
                          <Trash2 className="h-4 w-4" />
                          {t("remote.removeConnection", {
                            defaultValue: "Remove",
                          })}
                        </Button>
                      </div>
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </section>

      {isConnectionDetailsExpanded && remoteQuery.isError && (
        <div className="rounded-lg border border-destructive/30 bg-destructive/10 px-4 py-3 text-sm text-destructive">
          {extractErrorMessage(remoteQuery.error)}
        </div>
      )}

      {isConnectionDetailsExpanded &&
        connectedHost &&
        remoteQuery.isLoading && (
          <div className="rounded-lg border border-dashed border-border px-4 py-12 text-center text-sm text-muted-foreground">
            <Loader2 className="mx-auto mb-3 h-5 w-5 animate-spin" />
            {t("remote.inspecting", {
              defaultValue: "正在读取远端配置...",
            })}
          </div>
        )}

      {isConnectionDetailsExpanded && remoteQuery.data && (
        <>
          {appId === "codex" && (
            <section className="rounded-lg border border-border bg-card p-4">
              <div className="flex flex-wrap items-center justify-between gap-3">
                <div>
                  <h2 className="text-base font-semibold">Remote Agent</h2>
                  <p className="mt-1 text-xs text-muted-foreground">
                    Runs a localhost Responses-to-Chat proxy on the SSH host.
                  </p>
                </div>
                <div className="flex items-center gap-2">
                  <Badge
                    variant="secondary"
                    className={cn(
                      remoteAgentQuery.data?.running
                        ? "bg-emerald-100 text-emerald-700 dark:bg-emerald-900/40 dark:text-emerald-300"
                        : "bg-slate-100 text-slate-700 dark:bg-slate-800 dark:text-slate-300",
                    )}
                  >
                    {remoteAgentQuery.isFetching
                      ? "Checking"
                      : remoteAgentQuery.data?.running
                        ? "Running"
                        : remoteAgentQuery.data?.installed
                          ? "Installed"
                          : "Not installed"}
                  </Badge>
                  {remoteAgentQuery.data?.nodeVersion && (
                    <Badge variant="outline">
                      Node {remoteAgentQuery.data.nodeVersion}
                    </Badge>
                  )}
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => remoteAgentQuery.refetch()}
                    disabled={remoteAgentQuery.isFetching}
                  >
                    <RefreshCw
                      className={cn(
                        "h-4 w-4",
                        remoteAgentQuery.isFetching && "animate-spin",
                      )}
                    />
                    Status
                  </Button>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => logsAgentMutation.mutate()}
                    disabled={!connectedTarget || logsAgentMutation.isPending}
                  >
                    {logsAgentMutation.isPending ? (
                      <Loader2 className="h-4 w-4 animate-spin" />
                    ) : (
                      <FileText className="h-4 w-4" />
                    )}
                    Logs
                  </Button>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => stopAgentMutation.mutate()}
                    disabled={
                      !remoteAgentQuery.data?.running ||
                      stopAgentMutation.isPending
                    }
                  >
                    {stopAgentMutation.isPending ? (
                      <Loader2 className="h-4 w-4 animate-spin" />
                    ) : (
                      <Power className="h-4 w-4" />
                    )}
                    Stop
                  </Button>
                </div>
              </div>
              {remoteAgentQuery.data?.status?.proxy && (
                <div className="mt-3 space-y-2 text-xs text-muted-foreground">
                  <div className="grid gap-2 sm:grid-cols-3">
                    <div>
                      Proxy: 127.0.0.1:
                      {remoteAgentQuery.data.status.proxy.port}
                    </div>
                    <div>
                      Model: {remoteAgentQuery.data.status.proxy.upstream_model}
                    </div>
                    <div>
                      Requests:{" "}
                      {remoteAgentQuery.data.status.proxy.requests_total}
                    </div>
                  </div>
                  <p>{t("remote.sessionReloadRequired")}</p>
                  <p>{t("remote.customModelHint")}</p>
                </div>
              )}
              {remoteAgentQuery.data?.error &&
                !remoteAgentQuery.data.running && (
                  <p className="mt-2 text-xs text-muted-foreground">
                    {remoteAgentQuery.data.error}
                  </p>
                )}
              {agentLogs && (
                <pre className="mt-3 max-h-48 overflow-auto rounded-lg bg-muted p-3 text-xs leading-relaxed">
                  {agentLogs}
                </pre>
              )}
            </section>
          )}

          <div className="grid gap-4 lg:grid-cols-[minmax(0,1fr)_minmax(22rem,0.9fr)]">
            <section className="rounded-lg border border-border bg-card p-4">
              <div className="flex flex-wrap items-start justify-between gap-3">
                <div>
                  <h2 className="text-base font-semibold">
                    {t("remote.currentConfig", {
                      defaultValue: "远端当前配置",
                    })}
                  </h2>
                  <p className="mt-1 text-xs text-muted-foreground">
                    {connectedHost} / {t(`apps.${appId}`)}
                  </p>
                </div>
                {remoteCurrentProvider ? (
                  <Badge
                    variant="secondary"
                    className="gap-1 bg-emerald-100 text-emerald-700 dark:bg-emerald-900/40 dark:text-emerald-300"
                  >
                    <CheckCircle2 className="h-3.5 w-3.5" />
                    {remoteCurrentProvider.name}
                  </Badge>
                ) : remoteQuery.data.provider ? (
                  <Button
                    size="sm"
                    onClick={() => importMutation.mutate()}
                    disabled={importMutation.isPending}
                  >
                    {importMutation.isPending ? (
                      <Loader2 className="h-4 w-4 animate-spin" />
                    ) : (
                      <Download className="h-4 w-4" />
                    )}
                    {t("remote.downloadLocal", {
                      defaultValue: "同步到本地",
                    })}
                  </Button>
                ) : null}
              </div>

              {hasUnmanagedRemoteConfig && (
                <div className="mt-4 rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-sm text-amber-900 dark:text-amber-200">
                  <div className="flex items-center gap-2 font-medium">
                    <AlertTriangle className="h-4 w-4" />
                    {t("remote.overwriteRiskTitle", {
                      defaultValue: "远端已有未同步配置",
                    })}
                  </div>
                  <p className="mt-1 text-xs">{overwriteWarning}</p>
                  {remoteQuery.data.provider && !remoteCurrentProvider && (
                    <Button
                      className="mt-3"
                      size="sm"
                      variant="outline"
                      onClick={() => importMutation.mutate()}
                      disabled={importMutation.isPending}
                    >
                      {importMutation.isPending ? (
                        <Loader2 className="h-4 w-4 animate-spin" />
                      ) : (
                        <Download className="h-4 w-4" />
                      )}
                      {t("remote.syncToLocalFirst", {
                        defaultValue: "先同步到本地",
                      })}
                    </Button>
                  )}
                </div>
              )}

              {remoteWarnings.length > 0 && (
                <div className="mt-4 rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-sm text-amber-900 dark:text-amber-200">
                  <div className="flex items-center gap-2 font-medium">
                    <AlertTriangle className="h-4 w-4" />
                    {t("remote.warningTitle", {
                      defaultValue: "远端配置不完整",
                    })}
                  </div>
                  <div className="mt-1 space-y-1 text-xs">
                    {remoteWarnings.map((warning) => (
                      <p key={warning}>{warning}</p>
                    ))}
                  </div>
                </div>
              )}

              {hasExistingRemoteConfig || remoteAgentQuery.data?.running ? (
                <div className="mt-4 grid gap-3 sm:grid-cols-2">
                  <div className="rounded-lg border border-border bg-muted/20 px-3 py-3">
                    <p className="text-xs text-muted-foreground">
                      {t("remote.providerStatus", {
                        defaultValue: "Provider",
                      })}
                    </p>
                    <p className="mt-1 truncate text-sm font-medium">
                      {remoteProviderLabel ||
                        t("remote.notDetected", {
                          defaultValue: "Not detected",
                        })}
                    </p>
                  </div>
                  <div className="rounded-lg border border-border bg-muted/20 px-3 py-3">
                    <p className="text-xs text-muted-foreground">
                      {t("remote.modelStatus", { defaultValue: "Model" })}
                    </p>
                    <p className="mt-1 truncate text-sm font-medium">
                      {remoteModelLabel ||
                        t("remote.notDetected", {
                          defaultValue: "Not detected",
                        })}
                    </p>
                  </div>
                </div>
              ) : (
                <div className="mt-4 rounded-lg border border-dashed border-border px-4 py-8 text-center text-sm text-muted-foreground">
                  {t("remote.noConfig", {
                    defaultValue: "这台服务器上还没有可识别的当前配置。",
                  })}
                </div>
              )}
            </section>

            <section className="rounded-lg border border-border bg-card p-4">
              <div className="flex items-center justify-between gap-3">
                <div>
                  <h2 className="text-base font-semibold">
                    {t("remote.localProviders", {
                      defaultValue: "切换远端供应商",
                    })}
                  </h2>
                  <p className="mt-1 text-xs text-muted-foreground">
                    {t("remote.localProvidersHint", {
                      defaultValue: "选择一个本地供应商写入当前 SSH 服务器。",
                    })}
                  </p>
                </div>
              </div>

              <div className="mt-4 space-y-2">
                {isLoading ? (
                  <div className="rounded-lg border border-dashed border-border px-4 py-8 text-center text-sm text-muted-foreground">
                    {t("common.loading")}
                  </div>
                ) : localProviders.length === 0 ? (
                  <div className="rounded-lg border border-dashed border-border px-4 py-8 text-center text-sm text-muted-foreground">
                    {t("provider.noProviders")}
                  </div>
                ) : (
                  localProviders.map((provider) => {
                    const isRemoteCurrent =
                      remoteCurrentProviderId === provider.id;
                    const isApplying =
                      applyMutation.variables?.provider.id === provider.id;
                    const isAgentProviderAction =
                      appId === "codex" && provider.category !== "official";
                    const isAgentWorking =
                      (deployAgentMutation.variables?.provider.id ===
                        provider.id &&
                        deployAgentMutation.isPending) ||
                      (switchAgentMutation.variables?.provider.id ===
                        provider.id &&
                        switchAgentMutation.isPending);
                    const summary = getProviderSummary(provider, appId);
                    const willOverwriteUnmanaged =
                      !isAgentProviderAction &&
                      hasUnmanagedRemoteConfig &&
                      !isRemoteCurrent;

                    return (
                      <div
                        key={provider.id}
                        className={cn(
                          "rounded-lg border border-border p-3 transition-colors",
                          isRemoteCurrent &&
                            "border-emerald-500/60 bg-emerald-500/10",
                        )}
                      >
                        <div className="flex items-center gap-3">
                          <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg border border-border bg-muted">
                            <ProviderIcon
                              icon={provider.icon}
                              name={provider.name}
                              color={provider.iconColor}
                              size={20}
                            />
                          </div>
                          <div className="min-w-0 flex-1">
                            <div className="flex flex-wrap items-center gap-2">
                              <h3 className="truncate text-sm font-medium">
                                {provider.name}
                              </h3>
                              {provider.id === currentProviderId && (
                                <Badge variant="outline" className="rounded-md">
                                  {t("remote.localCurrent", {
                                    defaultValue: "本地使用中",
                                  })}
                                </Badge>
                              )}
                              {isRemoteCurrent && (
                                <Badge
                                  variant="secondary"
                                  className="rounded-md bg-emerald-100 text-emerald-700 dark:bg-emerald-900/40 dark:text-emerald-300"
                                >
                                  {t("remote.remoteCurrent", {
                                    defaultValue: "远端当前",
                                  })}
                                </Badge>
                              )}
                            </div>
                            {summary && (
                              <p className="mt-1 truncate text-xs text-muted-foreground">
                                {summary}
                              </p>
                            )}
                          </div>
                          <Button
                            size="sm"
                            variant={isRemoteCurrent ? "secondary" : "default"}
                            disabled={
                              isRemoteCurrent ||
                              applyMutation.isPending ||
                              deployAgentMutation.isPending ||
                              switchAgentMutation.isPending
                            }
                            onClick={() => {
                              if (isAgentProviderAction) {
                                requestRemoteAgentProvider(provider);
                                return;
                              }
                              requestApplyProvider(provider, isRemoteCurrent);
                            }}
                          >
                            {isRemoteCurrent ? (
                              <CheckCircle2 className="h-4 w-4" />
                            ) : isAgentWorking ||
                              (isApplying && applyMutation.isPending) ? (
                              <Loader2 className="h-4 w-4 animate-spin" />
                            ) : willOverwriteUnmanaged ? (
                              <AlertTriangle className="h-4 w-4" />
                            ) : (
                              <UploadCloud className="h-4 w-4" />
                            )}
                            {isRemoteCurrent
                              ? t("remote.current", { defaultValue: "Current" })
                              : isAgentProviderAction
                                ? remoteAgentQuery.data?.running
                                  ? t("remote.apply", {
                                      defaultValue: "Switch",
                                    })
                                  : t("remote.enable", {
                                      defaultValue: "Enable",
                                    })
                                : t("remote.apply", { defaultValue: "切换" })}
                          </Button>
                        </div>
                      </div>
                    );
                  })
                )}
              </div>
            </section>
          </div>
        </>
      )}

      <Dialog
        open={connectionEditorOpen}
        onOpenChange={(open) => setConnectionEditorOpen(open)}
      >
        <DialogContent className="max-w-2xl">
          <form
            onSubmit={(event) => {
              event.preventDefault();
              connectSelectedTarget();
            }}
          >
            <DialogHeader>
              <DialogTitle>
                {t("remote.addConnectionTitle", {
                  defaultValue: "Add SSH connection",
                })}
              </DialogTitle>
              <DialogDescription>
                {t("remote.addConnectionDescription", {
                  defaultValue:
                    "Connect once to verify the server. Host, username, and port will be saved after a successful connection.",
                })}
              </DialogDescription>
            </DialogHeader>

            <div className="grid gap-4 px-6 py-5 sm:grid-cols-2">
              <div className="space-y-2">
                <Label htmlFor="remote-manual-host">
                  {t("remote.manualHost", {
                    defaultValue: "IP / Domain",
                  })}
                </Label>
                <Input
                  id="remote-manual-host"
                  value={manualHost}
                  onChange={(event) => {
                    setManualHost(event.target.value);
                    setConnectedTarget(null);
                  }}
                  placeholder={t("remote.manualHostPlaceholder", {
                    defaultValue: "192.168.1.10",
                  })}
                  autoFocus
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="remote-manual-user">
                  {t("remote.manualUser", {
                    defaultValue: "Username",
                  })}
                </Label>
                <Input
                  id="remote-manual-user"
                  value={manualUser}
                  onChange={(event) => {
                    setManualUser(event.target.value);
                    setConnectedTarget(null);
                  }}
                  placeholder={t("remote.manualUserPlaceholder", {
                    defaultValue: "root",
                  })}
                />
              </div>
              <div className="space-y-2">
                <Label htmlFor="remote-manual-port">
                  {t("remote.manualPort", {
                    defaultValue: "Port",
                  })}
                </Label>
                <Input
                  id="remote-manual-port"
                  type="text"
                  inputMode="numeric"
                  pattern="[0-9]*"
                  value={manualPort}
                  onChange={(event) => {
                    setManualPort(event.target.value);
                    setConnectedTarget(null);
                  }}
                  placeholder="22"
                />
                {manualPortNumber === null && (
                  <p className="text-xs text-destructive">
                    {t("remote.manualPortInvalid", {
                      defaultValue: "Port must be between 1 and 65535.",
                    })}
                  </p>
                )}
              </div>
              <div className="space-y-2">
                <Label htmlFor="remote-manual-password">
                  {t("remote.manualPassword", {
                    defaultValue: "Password",
                  })}
                </Label>
                <Input
                  id="remote-manual-password"
                  type="password"
                  value={manualPassword}
                  onChange={(event) => {
                    setManualPassword(event.target.value);
                    setConnectedTarget(null);
                  }}
                  autoComplete="off"
                  placeholder={t("remote.manualPasswordPlaceholder", {
                    defaultValue: "Optional",
                  })}
                />
              </div>

              <p className="text-xs text-muted-foreground sm:col-span-2">
                {t("remote.manualPasswordHint", {
                  defaultValue:
                    "The password is used only for this SSH connection and is never saved.",
                })}
              </p>

              {connectionEditorOpen &&
                remoteQuery.isError &&
                connectedTargetKey === selectedTargetKey && (
                  <div className="rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2 text-sm text-destructive sm:col-span-2">
                    {extractErrorMessage(remoteQuery.error)}
                  </div>
                )}
            </div>

            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                onClick={() => setConnectionEditorOpen(false)}
              >
                {t("common.cancel")}
              </Button>
              <Button
                type="submit"
                disabled={
                  !selectedTarget ||
                  isInitialConnectingSelectedHost ||
                  isSelectedHostConnected
                }
              >
                {isInitialConnectingSelectedHost ? (
                  <Loader2 className="h-4 w-4 animate-spin" />
                ) : isSelectedHostConnected ? (
                  <CheckCircle2 className="h-4 w-4" />
                ) : (
                  <Server className="h-4 w-4" />
                )}
                {isInitialConnectingSelectedHost
                  ? t("remote.connecting", { defaultValue: "Connecting" })
                  : isSelectedHostConnected
                    ? t("remote.connected", { defaultValue: "Connected" })
                    : t("remote.connect", { defaultValue: "Connect" })}
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>

      {usageSyncProgress && (
        <Dialog
          modal={false}
          open={Boolean(usageSyncProgress)}
          onOpenChange={(open) => {
            if (!open && usageSyncCanClose) {
              activeUsageSyncIdRef.current = null;
              setUsageSyncProgress(null);
            }
          }}
        >
          <DialogContent
            className="max-w-md"
            overlayClassName="pointer-events-none"
          >
            <DialogHeader>
              <DialogTitle>
                {t("remote.usageSyncProgress.title", {
                  defaultValue: "同步远端用量",
                })}
              </DialogTitle>
              <DialogDescription>
                {usageSyncProgress.hostAlias} /{" "}
                {t(`apps.${usageSyncProgress.app}`)}
              </DialogDescription>
            </DialogHeader>

            <div className="space-y-4 px-6 py-5">
              <div className="flex items-start gap-3">
                <div className="mt-0.5 flex h-9 w-9 shrink-0 items-center justify-center rounded-full border border-border bg-muted">
                  {usageSyncStage === "failed" ? (
                    <AlertTriangle className="h-4 w-4 text-destructive" />
                  ) : usageSyncStage === "completed" ? (
                    <CheckCircle2 className="h-4 w-4 text-emerald-600" />
                  ) : (
                    <Loader2 className="h-4 w-4 animate-spin text-primary" />
                  )}
                </div>
                <div className="min-w-0 flex-1">
                  <div className="text-sm font-medium">
                    {usageSyncStageLabel}
                  </div>
                  <div className="mt-1 text-xs text-muted-foreground">
                    {t("remote.usageSyncProgress.step", {
                      defaultValue: "阶段 {{current}} / {{total}}",
                      current: usageSyncProgress.currentStep,
                      total: usageSyncProgress.totalSteps,
                    })}
                  </div>
                </div>
              </div>

              <div
                className="grid gap-1"
                style={{
                  gridTemplateColumns: `repeat(${Math.max(
                    usageSyncProgress.totalSteps,
                    1,
                  )}, minmax(0, 1fr))`,
                }}
                aria-hidden="true"
              >
                {Array.from({
                  length: Math.max(usageSyncProgress.totalSteps, 1),
                }).map((_, index) => {
                  const isReached =
                    usageSyncStage === "completed" ||
                    index < usageSyncProgress.currentStep;
                  return (
                    <div
                      key={index}
                      className={cn(
                        "h-2 rounded-full bg-muted transition-colors",
                        isReached &&
                          (usageSyncStage === "failed"
                            ? "bg-destructive"
                            : "bg-primary"),
                      )}
                    />
                  );
                })}
              </div>

              <div className="grid gap-2 text-xs text-muted-foreground sm:grid-cols-2">
                {usageSyncProgress.fileCount !== undefined && (
                  <div className="rounded-md border border-border px-3 py-2">
                    <div className="font-medium text-foreground">
                      {usageSyncProgress.fileCount}
                    </div>
                    <div>
                      {t("remote.usageSyncProgress.files", {
                        defaultValue: "日志文件",
                      })}
                    </div>
                  </div>
                )}
                {usageSyncProgress.payloadBytes !== undefined && (
                  <div className="rounded-md border border-border px-3 py-2">
                    <div className="font-medium text-foreground">
                      {formatBytes(usageSyncProgress.payloadBytes)}
                    </div>
                    <div>
                      {t("remote.usageSyncProgress.payload", {
                        defaultValue: "远端摘要",
                      })}
                    </div>
                  </div>
                )}
                {usageSyncProgress.imported !== undefined && (
                  <div className="rounded-md border border-border px-3 py-2">
                    <div className="font-medium text-foreground">
                      {usageSyncProgress.imported}
                    </div>
                    <div>
                      {t("remote.usageSyncProgress.imported", {
                        defaultValue: "导入记录",
                      })}
                    </div>
                  </div>
                )}
                {usageSyncProgress.skipped !== undefined && (
                  <div className="rounded-md border border-border px-3 py-2">
                    <div className="font-medium text-foreground">
                      {usageSyncProgress.skipped}
                    </div>
                    <div>
                      {t("remote.usageSyncProgress.skipped", {
                        defaultValue: "跳过记录",
                      })}
                    </div>
                  </div>
                )}
                {usageSyncProgress.unchangedFiles !== undefined && (
                  <div className="rounded-md border border-border px-3 py-2">
                    <div className="font-medium text-foreground">
                      {usageSyncProgress.unchangedFiles}
                    </div>
                    <div>
                      {t("remote.usageSyncProgress.unchangedFiles", {
                        defaultValue: "未变化文件",
                      })}
                    </div>
                  </div>
                )}
              </div>

              {usageSyncProgress.error && (
                <div className="rounded-md border border-destructive/30 bg-destructive/10 px-3 py-2 text-sm text-destructive">
                  {usageSyncProgress.error}
                </div>
              )}
            </div>

            {usageSyncCanClose && (
              <DialogFooter>
                <Button
                  onClick={() => {
                    activeUsageSyncIdRef.current = null;
                    setUsageSyncProgress(null);
                  }}
                >
                  {t("remote.usageSyncProgress.close", {
                    defaultValue: "关闭",
                  })}
                </Button>
              </DialogFooter>
            )}
          </DialogContent>
        </Dialog>
      )}

      {confirmApplyProvider && (
        <ConfirmDialog
          isOpen={Boolean(confirmApplyProvider)}
          title={t("remote.confirmOverwriteTitle", {
            defaultValue: "确认覆盖远端配置？",
          })}
          message={
            confirmStopsRemoteAgent
              ? t("remote.confirmStopAgentMessage", {
                  defaultValue:
                    "The Remote Agent will stop and restore the previous Codex profile before OpenAI Official is applied. Existing files will be backed up.",
                })
              : `${overwriteWarning}\n\n${t("remote.confirmOverwriteMessage", {
                  defaultValue:
                    "继续后会把选中的本地供应商写入远端。当前远端文件会先自动备份，但远端正在使用的配置会被替换。",
                })}`
          }
          confirmText={t("remote.confirmOverwrite", {
            defaultValue: "确认切换",
          })}
          isLoading={applyMutation.isPending}
          loadingText={t("remote.switching", {
            defaultValue: "Switching remote provider...",
          })}
          cancelText={t("common.cancel")}
          onConfirm={() =>
            applyMutation.mutate({
              provider: confirmApplyProvider,
              forceOverwrite: true,
              stopAgentFirst: confirmStopsRemoteAgent,
            })
          }
          onCancel={() => setConfirmApplyProvider(null)}
        />
      )}
    </div>
  );
}
