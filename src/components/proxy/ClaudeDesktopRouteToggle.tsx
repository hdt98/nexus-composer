import { Loader2, Radio } from "lucide-react";
import { toast } from "sonner";
import { useTranslation } from "react-i18next";
import { Switch } from "@/components/ui/switch";
import { useProxyStatus } from "@/hooks/useProxyStatus";
import { cn } from "@/lib/utils";

interface ClaudeDesktopRouteToggleProps {
  className?: string;
}

export function ClaudeDesktopRouteToggle({
  className,
}: ClaudeDesktopRouteToggleProps) {
  const { t } = useTranslation();
  const {
    isRunning,
    status,
    takeoverStatus,
    startProxyServer,
    stopProxyServer,
    isStarting,
    isStoppingServer,
  } = useProxyStatus();

  const isBusy = isStarting || isStoppingServer;
  const otherTakeoverActive = Boolean(
    takeoverStatus?.claude || takeoverStatus?.codex || takeoverStatus?.gemini,
  );
  const routeAddress = status?.address ?? "127.0.0.1";
  const routePort = status?.port ?? 15721;

  const handleToggle = async (checked: boolean) => {
    try {
      if (checked) {
        await startProxyServer();
        return;
      }

      if (otherTakeoverActive) {
        toast.warning(
          t("claudeDesktop.route.stopBlockedByTakeover", {
            defaultValue:
              "Another app is using local routing. Turn off routing for that app in Settings before stopping the service.",
          }),
          { duration: 5000 },
        );
        return;
      }

      await stopProxyServer();
    } catch (error) {
      console.error("[ClaudeDesktopRouteToggle] Toggle route failed:", error);
    }
  };

  const tooltipText = isRunning
    ? t("claudeDesktop.route.tooltip.active", {
        address: routeAddress,
        port: routePort,
        defaultValue:
          "Claude Desktop local routing is running - {{address}}:{{port}}",
      })
    : t("claudeDesktop.route.tooltip.inactive", {
        address: routeAddress,
        port: routePort,
        defaultValue:
          "Turn on Claude Desktop local routing for providers that need model mapping or format conversion. Configured address: {{address}}:{{port}}",
      });

  return (
    <div
      className={cn(
        "flex items-center gap-1 px-1.5 h-8 rounded-lg bg-muted/50 transition-all",
        className,
      )}
      title={tooltipText}
    >
      {isBusy ? (
        <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />
      ) : (
        <Radio
          className={cn(
            "h-4 w-4 transition-colors",
            isRunning
              ? "text-emerald-500 animate-pulse"
              : "text-muted-foreground",
          )}
        />
      )}
      <Switch
        checked={isRunning}
        onCheckedChange={handleToggle}
        disabled={isBusy}
      />
    </div>
  );
}
