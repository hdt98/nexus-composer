/**
 * Failover toggle.
 *
 * Displayed in the main header to enable or disable automatic failover.
 */

import { Shuffle, Loader2 } from "lucide-react";
import { Switch } from "@/components/ui/switch";
import {
  useAutoFailoverEnabled,
  useSetAutoFailoverEnabled,
} from "@/lib/query/failover";
import { useProxyStatus } from "@/hooks/useProxyStatus";
import { cn } from "@/lib/utils";
import { useTranslation } from "react-i18next";
import type { AppId } from "@/lib/api";

interface FailoverToggleProps {
  className?: string;
  activeApp: AppId;
}

export function FailoverToggle({ className, activeApp }: FailoverToggleProps) {
  const { t } = useTranslation();
  const { data: isEnabled = false, isLoading } =
    useAutoFailoverEnabled(activeApp);
  const setEnabled = useSetAutoFailoverEnabled();
  const { takeoverStatus } = useProxyStatus();
  const takeoverEnabled = takeoverStatus?.[activeApp] ?? false;

  const handleToggle = (checked: boolean) => {
    if (checked && !takeoverEnabled) return;
    setEnabled.mutate({ appType: activeApp, enabled: checked });
  };

  const appLabel =
    activeApp === "claude"
      ? "Claude"
      : activeApp === "codex"
        ? "Codex"
        : "Gemini";

  const tooltipText = !takeoverEnabled
    ? t("failover.tooltip.takeoverRequired", {
        app: appLabel,
        defaultValue: "Enable routing for {{app}} before enabling failover",
      })
    : isEnabled
      ? t("failover.tooltip.enabled", {
          app: appLabel,
          defaultValue:
            "{{app}} failover enabled\nRequests follow queue priority (P1→P2→...)",
        })
      : t("failover.tooltip.disabled", {
          app: appLabel,
          defaultValue:
            "Enable {{app}} failover\nSwitches to queue P1 immediately, then falls back on failures",
        });

  return (
    <div
      className={cn(
        "flex items-center gap-1 px-1.5 h-8 rounded-lg bg-muted/50 transition-all",
        className,
      )}
      title={tooltipText}
    >
      {setEnabled.isPending || isLoading ? (
        <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />
      ) : (
        <Shuffle
          className={cn(
            "h-4 w-4 transition-colors",
            isEnabled
              ? "text-emerald-500 animate-pulse"
              : "text-muted-foreground",
          )}
        />
      )}
      <Switch
        checked={isEnabled}
        onCheckedChange={handleToggle}
        disabled={setEnabled.isPending || isLoading || !takeoverEnabled}
      />
    </div>
  );
}
