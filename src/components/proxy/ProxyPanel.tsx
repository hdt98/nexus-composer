import { useState, useEffect } from "react";
import {
  Activity,
  Clock,
  TrendingUp,
  Server,
  ListOrdered,
  Save,
  Loader2,
  Zap,
  Power,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/switch";
import { Label } from "@/components/ui/label";
import { Input } from "@/components/ui/input";
import { ToggleRow } from "@/components/ui/toggle-row";
import { useProxyStatus } from "@/hooks/useProxyStatus";
import { toast } from "sonner";
import { useFailoverQueue } from "@/lib/query/failover";
import { ProviderHealthBadge } from "@/components/providers/ProviderHealthBadge";
import { useProviderHealth } from "@/lib/query/failover";
import {
  useProxyTakeoverStatus,
  useSetProxyTakeoverForApp,
  useGlobalProxyConfig,
  useUpdateGlobalProxyConfig,
} from "@/lib/query/proxy";
import type { ProxyStatus } from "@/types/proxy";
import { useTranslation } from "react-i18next";
import { AnimatePresence, motion } from "framer-motion";
import { extractErrorMessage } from "@/utils/errorUtils";

interface ProxyPanelProps {
  enableLocalProxy: boolean;
  onEnableLocalProxyChange: (checked: boolean) => void;
  onToggleProxy: (checked: boolean) => Promise<void>;
  isProxyPending: boolean;
}

export function ProxyPanel({
  enableLocalProxy,
  onEnableLocalProxyChange,
  onToggleProxy,
  isProxyPending,
}: ProxyPanelProps) {
  const { t } = useTranslation();
  const { status, isRunning } = useProxyStatus();

  // Read per-application routing state.
  const { data: takeoverStatus } = useProxyTakeoverStatus();
  const setTakeoverForApp = useSetProxyTakeoverForApp();

  // Read global proxy configuration.
  const { data: globalConfig } = useGlobalProxyConfig();
  const updateGlobalConfig = useUpdateGlobalProxyConfig();

  // Keep address and port in local state; a string lets the port be cleared.
  const [listenAddress, setListenAddress] = useState("127.0.0.1");
  const [listenPort, setListenPort] = useState("15721");

  // Synchronize global configuration into local state.
  useEffect(() => {
    if (globalConfig) {
      setListenAddress(globalConfig.listenAddress);
      setListenPort(String(globalConfig.listenPort));
    }
  }, [globalConfig]);

  // Read failover queues for every supported application. Automatic failover
  // selects providers by queue priority (P1→P2→...).
  const { data: claudeQueue = [] } = useFailoverQueue("claude");
  const { data: codexQueue = [] } = useFailoverQueue("codex");
  const { data: geminiQueue = [] } = useFailoverQueue("gemini");

  const handleTakeoverChange = async (appType: string, enabled: boolean) => {
    try {
      await setTakeoverForApp.mutateAsync({ appType, enabled });
      toast.success(
        enabled
          ? t("proxy.takeover.enabled", {
              app: appType,
              defaultValue: `${appType} routing enabled`,
            })
          : t("proxy.takeover.disabled", {
              app: appType,
              defaultValue: `${appType} routing disabled`,
            }),
        { closeButton: true },
      );
    } catch (error) {
      const detail =
        extractErrorMessage(error) ||
        t("common.unknown", { defaultValue: "Unknown error" });
      toast.error(
        t("proxy.takeover.failed", {
          detail,
          defaultValue: "Failed to change routing state",
        }),
      );
    }
  };

  const handleLoggingChange = async (enabled: boolean) => {
    if (!globalConfig) return;
    try {
      await updateGlobalConfig.mutateAsync({
        ...globalConfig,
        enableLogging: enabled,
      });
      toast.success(
        enabled
          ? t("proxy.logging.enabled", {
              defaultValue: "Request logging enabled",
            })
          : t("proxy.logging.disabled", {
              defaultValue: "Request logging disabled",
            }),
        { closeButton: true },
      );
    } catch (error) {
      toast.error(
        t("proxy.logging.failed", {
          defaultValue: "Failed to change logging state",
        }),
      );
    }
  };

  const handleSaveBasicConfig = async () => {
    if (!globalConfig) return;

    // Validate an IPv4 address, IPv6 literal, or localhost.
    const addressTrimmed = listenAddress.trim();
    const ipv4Regex = /^(\d{1,3}\.){3}\d{1,3}$/;
    const isValidIpv4 = (addr: string): boolean =>
      ipv4Regex.test(addr) &&
      addr.split(".").every((n) => {
        const num = parseInt(n, 10);
        return num >= 0 && num <= 255;
      });
    // An IPv6 literal must contain ':' and remain valid when wrapped in URL
    // brackets. The backend rewrites `::` to `::1`, so accept `::` here too.
    const isValidIpv6 = (addr: string): boolean => {
      if (!addr.includes(":")) return false;
      try {
        new URL(`http://[${addr}]/`);
        return true;
      } catch {
        return false;
      }
    };
    const normalizedAddress =
      addressTrimmed === "localhost" ? "127.0.0.1" : addressTrimmed;
    const isValidAddress =
      addressTrimmed === "localhost" ||
      addressTrimmed === "0.0.0.0" ||
      isValidIpv4(addressTrimmed) ||
      isValidIpv6(addressTrimmed);
    if (!isValidAddress) {
      toast.error(
        t("proxy.settings.invalidAddress", {
          defaultValue:
            "Enter a valid IPv4 address (such as 127.0.0.1), IPv6 address (such as ::1), or localhost",
        }),
      );
      return;
    }

    // A port must contain decimal digits only.
    const portTrimmed = listenPort.trim();
    if (!/^\d+$/.test(portTrimmed)) {
      toast.error(
        t("proxy.settings.invalidPort", {
          defaultValue: "Enter a port from 1024 to 65535",
        }),
      );
      return;
    }
    const port = parseInt(portTrimmed);
    if (isNaN(port) || port < 1024 || port > 65535) {
      toast.error(
        t("proxy.settings.invalidPort", {
          defaultValue: "Enter a port from 1024 to 65535",
        }),
      );
      return;
    }
    try {
      await updateGlobalConfig.mutateAsync({
        ...globalConfig,
        listenAddress: normalizedAddress,
        listenPort: port,
      });
      toast.success(
        t("proxy.settings.configSaved", {
          defaultValue: "Proxy settings saved",
        }),
        { closeButton: true },
      );
    } catch (error) {
      toast.error(
        t("proxy.settings.configSaveFailed", {
          defaultValue: "Failed to save settings",
        }),
      );
    }
  };

  const formatUptime = (seconds: number): string => {
    const hours = Math.floor(seconds / 3600);
    const minutes = Math.floor((seconds % 3600) / 60);
    const secs = seconds % 60;

    if (hours > 0) {
      return `${hours}h ${minutes}m ${secs}s`;
    } else if (minutes > 0) {
      return `${minutes}m ${secs}s`;
    } else {
      return `${secs}s`;
    }
  };

  // URL-format the address, adding brackets around IPv6 literals.
  const formatAddressForUrl = (address: string, port: number): string => {
    const isIPv6 = address.includes(":");
    const host = isIPv6 ? `[${address}]` : address;
    return `http://${host}:${port}`;
  };

  return (
    <>
      <section className="space-y-4">
        {/* [1] Enable proxy button on main page — always visible */}
        <ToggleRow
          icon={<Zap className="h-4 w-4 text-green-500" />}
          title={t("settings.advanced.proxy.enableFeature")}
          description={t("settings.advanced.proxy.enableFeatureDescription")}
          checked={enableLocalProxy}
          onCheckedChange={onEnableLocalProxyChange}
        />

        {/* [2] Proxy service toggle — always visible */}
        <div className="flex items-center justify-between rounded-xl border border-border bg-card/50 p-4 transition-colors hover:bg-muted/50">
          <div className="flex items-center gap-3">
            <div className="flex h-8 w-8 items-center justify-center rounded-lg bg-background ring-1 ring-border">
              <Power className="h-4 w-4 text-green-500" />
            </div>
            <div className="space-y-1">
              <p className="text-sm font-medium leading-none">
                {t("proxyConfig.proxyEnabled", {
                  defaultValue: "Nexus",
                })}
              </p>
              <p className="text-xs text-muted-foreground">
                {isRunning
                  ? t("settings.advanced.proxy.running")
                  : t("settings.advanced.proxy.stopped")}
              </p>
            </div>
          </div>
          <Switch
            checked={isRunning}
            onCheckedChange={onToggleProxy}
            disabled={isProxyPending}
          />
        </div>

        {/* [3] App takeover switches — animated, visible only when proxy is running */}
        <AnimatePresence>
          {isRunning && (
            <motion.div
              initial={{ opacity: 0, height: 0 }}
              animate={{ opacity: 1, height: "auto" }}
              exit={{ opacity: 0, height: 0 }}
              transition={{ duration: 0.25, ease: "easeInOut" }}
              className="overflow-hidden"
            >
              <div className="rounded-xl border-2 border-primary/20 bg-primary/5 p-4 space-y-3">
                <p className="text-xs font-medium text-primary">
                  {t("proxyConfig.appTakeover", {
                    defaultValue: "Apps using Nexus",
                  })}
                </p>
                <div className="grid gap-2 sm:grid-cols-3">
                  {(["claude", "codex", "gemini"] as const).map((appType) => {
                    const isEnabled =
                      takeoverStatus?.[
                        appType as keyof typeof takeoverStatus
                      ] ?? false;
                    return (
                      <div
                        key={appType}
                        className="flex items-center justify-between rounded-md border border-primary/20 bg-background/60 px-3 py-2"
                      >
                        <span className="text-sm font-medium capitalize">
                          {appType}
                        </span>
                        <Switch
                          checked={isEnabled}
                          onCheckedChange={(checked) =>
                            handleTakeoverChange(appType, checked)
                          }
                          disabled={setTakeoverForApp.isPending}
                        />
                      </div>
                    );
                  })}
                </div>
                <p className="text-xs text-muted-foreground">
                  {t("proxy.takeover.hint", {
                    defaultValue: "Choose which applications use Nexus",
                  })}
                </p>
              </div>
            </motion.div>
          )}
        </AnimatePresence>

        {/* Running state: service info + stats */}
        {isRunning && status ? (
          <div className="space-y-6">
            {/* [4] Running info: address + current provider */}
            <div className="rounded-lg border border-border bg-muted/40 p-4 space-y-4">
              <div>
                <p className="text-xs text-muted-foreground mb-2">
                  {t("proxy.panel.serviceAddress", {
                    defaultValue: "Service Address",
                  })}
                </p>
                <div className="flex flex-col gap-2 sm:flex-row sm:items-center">
                  <code className="flex-1 text-sm bg-background px-3 py-2 rounded border border-border/60">
                    {formatAddressForUrl(status.address, status.port)}
                  </code>
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => {
                      navigator.clipboard.writeText(
                        formatAddressForUrl(status.address, status.port),
                      );
                      toast.success(
                        t("proxy.panel.addressCopied", {
                          defaultValue: "Address copied",
                        }),
                        { closeButton: true },
                      );
                    }}
                  >
                    {t("common.copy")}
                  </Button>
                </div>
                <p className="text-xs text-muted-foreground mt-2">
                  {t("proxy.settings.restartRequired", {
                    defaultValue:
                      "Stop the proxy before changing its listen address or port",
                  })}
                </p>
              </div>

              <div className="pt-3 border-t border-border space-y-2">
                <p className="text-xs text-muted-foreground">
                  {t("provider.inUse")}
                </p>
                {status.active_targets && status.active_targets.length > 0 ? (
                  <div className="grid gap-2 sm:grid-cols-2">
                    {status.active_targets.map((target) => (
                      <div
                        key={target.app_type}
                        className="flex items-center justify-between rounded-md border border-border bg-background/60 px-2 py-1.5 text-xs"
                      >
                        <span className="text-muted-foreground">
                          {target.app_type}
                        </span>
                        <span
                          className="ml-2 font-medium truncate text-foreground"
                          title={target.provider_name}
                        >
                          {target.provider_name}
                        </span>
                      </div>
                    ))}
                  </div>
                ) : status.current_provider ? (
                  <p className="text-sm text-muted-foreground">
                    {t("proxy.panel.currentProvider", {
                      defaultValue: "Current provider:",
                    })}{" "}
                    <span className="font-medium text-foreground">
                      {status.current_provider}
                    </span>
                  </p>
                ) : (
                  <p className="text-sm text-yellow-600 dark:text-yellow-400">
                    {t("proxy.panel.waitingFirstRequest", {
                      defaultValue:
                        "Current provider: waiting for the first request…",
                    })}
                  </p>
                )}
              </div>

              {/* [5] Logging toggles */}
              <div className="pt-3 border-t border-border">
                <div className="space-y-2">
                  <div className="flex items-center justify-between rounded-md border border-border bg-background/60 px-3 py-2">
                    <div className="space-y-0.5">
                      <Label className="text-sm font-medium">
                        {t("proxy.settings.fields.enableLogging.label", {
                          defaultValue: "Enable Request Logging",
                        })}
                      </Label>
                      <p className="text-xs text-muted-foreground">
                        {t("proxy.settings.fields.enableLogging.description", {
                          defaultValue:
                            "Record proxy requests for troubleshooting",
                        })}
                      </p>
                    </div>
                    <Switch
                      checked={globalConfig?.enableLogging ?? true}
                      onCheckedChange={handleLoggingChange}
                      disabled={updateGlobalConfig.isPending}
                    />
                  </div>
                </div>
              </div>

              {/* [6] Provider queues */}
              {(claudeQueue.length > 0 ||
                codexQueue.length > 0 ||
                geminiQueue.length > 0) && (
                <div className="pt-3 border-t border-border space-y-3">
                  <div className="flex items-center gap-2">
                    <ListOrdered className="h-3.5 w-3.5 text-muted-foreground" />
                    <p className="text-xs text-muted-foreground">
                      {t("proxy.failoverQueue.title")}
                    </p>
                  </div>

                  {claudeQueue.length > 0 && (
                    <ProviderQueueGroup
                      appType="claude"
                      appLabel="Claude"
                      targets={claudeQueue.map((item) => ({
                        id: item.providerId,
                        name: item.providerName,
                      }))}
                      status={status}
                    />
                  )}

                  {codexQueue.length > 0 && (
                    <ProviderQueueGroup
                      appType="codex"
                      appLabel="Codex"
                      targets={codexQueue.map((item) => ({
                        id: item.providerId,
                        name: item.providerName,
                      }))}
                      status={status}
                    />
                  )}

                  {geminiQueue.length > 0 && (
                    <ProviderQueueGroup
                      appType="gemini"
                      appLabel="Gemini"
                      targets={geminiQueue.map((item) => ({
                        id: item.providerId,
                        name: item.providerName,
                      }))}
                      status={status}
                    />
                  )}
                </div>
              )}
            </div>

            {/* [7] Stats cards */}
            <div className="grid gap-3 md:grid-cols-4">
              <StatCard
                icon={<Activity className="h-4 w-4" />}
                label={t("proxy.panel.stats.activeConnections", {
                  defaultValue: "Active Connections",
                })}
                value={status.active_connections}
              />
              <StatCard
                icon={<TrendingUp className="h-4 w-4" />}
                label={t("proxy.panel.stats.totalRequests", {
                  defaultValue: "Total Requests",
                })}
                value={status.total_requests}
              />
              <StatCard
                icon={<Clock className="h-4 w-4" />}
                label={t("proxy.panel.stats.successRate", {
                  defaultValue: "Success Rate",
                })}
                value={`${status.success_rate.toFixed(1)}%`}
                variant={status.success_rate > 90 ? "success" : "warning"}
              />
              <StatCard
                icon={<Clock className="h-4 w-4" />}
                label={t("proxy.panel.stats.uptime", {
                  defaultValue: "Uptime",
                })}
                value={formatUptime(status.uptime_seconds)}
              />
            </div>
          </div>
        ) : (
          <div className="space-y-6">
            {/* [8] Basic settings — address/port (only when stopped) */}
            <div className="rounded-lg border border-border bg-muted/40 p-4 space-y-4">
              <div>
                <h4 className="text-sm font-semibold">
                  {t("proxy.settings.basic.title", {
                    defaultValue: "Basic Settings",
                  })}
                </h4>
                <p className="text-xs text-muted-foreground">
                  {t("proxy.settings.basic.description", {
                    defaultValue:
                      "Configure the address and port used by the proxy service.",
                  })}
                </p>
              </div>

              <div className="grid gap-4 md:grid-cols-2">
                <div className="space-y-2">
                  <Label htmlFor="listen-address">
                    {t("proxy.settings.fields.listenAddress.label", {
                      defaultValue: "Listen Address",
                    })}
                  </Label>
                  <Input
                    id="listen-address"
                    value={listenAddress}
                    onChange={(e) => setListenAddress(e.target.value)}
                    placeholder={t(
                      "proxy.settings.fields.listenAddress.placeholder",
                      {
                        defaultValue: "127.0.0.1",
                      },
                    )}
                  />
                  <p className="text-xs text-muted-foreground">
                    {t("proxy.settings.fields.listenAddress.description", {
                      defaultValue:
                        "IP address on which the proxy listens (127.0.0.1 recommended)",
                    })}
                  </p>
                </div>

                <div className="space-y-2">
                  <Label htmlFor="listen-port">
                    {t("proxy.settings.fields.listenPort.label", {
                      defaultValue: "Listen Port",
                    })}
                  </Label>
                  <Input
                    id="listen-port"
                    type="number"
                    value={listenPort}
                    onChange={(e) => setListenPort(e.target.value)}
                    placeholder={t(
                      "proxy.settings.fields.listenPort.placeholder",
                      {
                        defaultValue: "15721",
                      },
                    )}
                  />
                  <p className="text-xs text-muted-foreground">
                    {t("proxy.settings.fields.listenPort.description", {
                      defaultValue:
                        "Port on which the proxy listens (1024–65535)",
                    })}
                  </p>
                </div>
              </div>

              <div className="flex justify-end">
                <Button
                  size="sm"
                  onClick={handleSaveBasicConfig}
                  disabled={updateGlobalConfig.isPending}
                >
                  {updateGlobalConfig.isPending ? (
                    <>
                      <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                      {t("common.saving", { defaultValue: "Saving..." })}
                    </>
                  ) : (
                    <>
                      <Save className="mr-2 h-4 w-4" />
                      {t("common.save", { defaultValue: "Save" })}
                    </>
                  )}
                </Button>
              </div>
            </div>

            {/* Stopped hint */}
            <div className="text-center py-6 text-muted-foreground">
              <div className="mx-auto w-16 h-16 rounded-full bg-muted flex items-center justify-center mb-4">
                <Server className="h-8 w-8" />
              </div>
              <p className="text-base font-medium text-foreground mb-1">
                {t("proxy.panel.stoppedTitle", {
                  defaultValue: "Proxy service is stopped",
                })}
              </p>
              <p className="text-sm text-muted-foreground">
                {t("proxy.panel.stoppedDescription", {
                  defaultValue: "Use the switch above to start it",
                })}
              </p>
            </div>
          </div>
        )}
      </section>
    </>
  );
}

interface StatCardProps {
  icon: React.ReactNode;
  label: string;
  value: string | number;
  variant?: "default" | "success" | "warning";
}

function StatCard({ icon, label, value, variant = "default" }: StatCardProps) {
  const variantStyles = {
    default: "",
    success: "border-green-500/40 bg-green-500/5",
    warning: "border-yellow-500/40 bg-yellow-500/5",
  };

  return (
    <div
      className={`rounded-lg border border-border bg-card/60 p-4 text-sm text-muted-foreground ${variantStyles[variant]}`}
    >
      <div className="flex items-center gap-2 text-muted-foreground mb-2">
        {icon}
        <span className="text-xs">{label}</span>
      </div>
      <p className="text-xl font-semibold text-foreground">{value}</p>
    </div>
  );
}

interface ProviderQueueGroupProps {
  appType: string;
  appLabel: string;
  targets: Array<{
    id: string;
    name: string;
  }>;
  status: ProxyStatus;
}

function ProviderQueueGroup({
  appType,
  appLabel,
  targets,
  status,
}: ProviderQueueGroupProps) {
  // Find the active target for this application.
  const activeTarget = status.active_targets?.find(
    (t) => t.app_type === appType,
  );

  return (
    <div className="space-y-2">
      {/* Application heading */}
      <div className="flex items-center gap-2 px-2">
        <span className="text-xs font-semibold text-foreground/80">
          {appLabel}
        </span>
        <div className="flex-1 h-px bg-border/50" />
      </div>

      {/* Provider list */}
      <div className="space-y-1.5">
        {targets.map((target, index) => (
          <ProviderQueueItem
            key={target.id}
            provider={target}
            priority={index + 1}
            appType={appType}
            isCurrent={activeTarget?.provider_id === target.id}
          />
        ))}
      </div>
    </div>
  );
}

interface ProviderQueueItemProps {
  provider: {
    id: string;
    name: string;
  };
  priority: number;
  appType: string;
  isCurrent: boolean;
}

function ProviderQueueItem({
  provider,
  priority,
  appType,
  isCurrent,
}: ProviderQueueItemProps) {
  const { t } = useTranslation();
  const { data: health } = useProviderHealth(provider.id, appType);

  return (
    <div
      className={`flex items-center justify-between rounded-md border px-3 py-2 text-sm transition-colors ${
        isCurrent
          ? "border-primary/40 bg-primary/10 text-primary font-medium"
          : "border-border bg-background/60"
      }`}
    >
      <div className="flex items-center gap-2">
        <span
          className={`flex-shrink-0 flex items-center justify-center w-5 h-5 rounded-full text-xs font-bold ${
            isCurrent
              ? "bg-primary text-primary-foreground"
              : "bg-muted text-muted-foreground"
          }`}
        >
          {priority}
        </span>
        <span className={isCurrent ? "" : "text-foreground"}>
          {provider.name}
        </span>
        {isCurrent && (
          <span className="text-xs px-1.5 py-0.5 rounded bg-primary/20 text-primary">
            {t("provider.inUse")}
          </span>
        )}
      </div>
      {/* Health badge */}
      <ProviderHealthBadge
        consecutiveFailures={health?.consecutive_failures ?? 0}
        isHealthy={health?.is_healthy}
      />
    </div>
  );
}
