import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useQueryClient } from "@tanstack/react-query";
import { Loader2, RefreshCw } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { usageApi } from "@/lib/api/usage";
import { usageKeys } from "@/lib/query/usage";

export function SessionSyncButton() {
  const { t } = useTranslation();
  const queryClient = useQueryClient();
  const [syncing, setSyncing] = useState(false);

  const handleSync = async () => {
    setSyncing(true);
    try {
      const result = await usageApi.syncSessionUsage();
      if (result.imported > 0) {
        queryClient.invalidateQueries({ queryKey: usageKeys.all });
      }

      if (result.errors.length > 0) {
        if (result.imported > 0) {
          toast.warning(
            t("usage.sessionSync.partial", {
              imported: result.imported,
              failed: result.errors.length,
            }),
          );
        } else {
          toast.error(
            t("usage.sessionSync.failedWithCount", {
              count: result.errors.length,
            }),
          );
        }
      } else if (result.imported > 0) {
        toast.success(
          t("usage.sessionSync.imported", {
            count: result.imported,
            defaultValue: "Imported {{count}} records from session logs",
          }),
        );
      } else {
        toast.info(
          t("usage.sessionSync.upToDate", {
            defaultValue: "Session logs are up to date",
          }),
        );
      }
    } catch {
      toast.error(
        t("usage.sessionSync.failed", {
          defaultValue: "Session sync failed",
        }),
      );
    } finally {
      setSyncing(false);
    }
  };

  const label = t("usage.sessionSync.trigger", {
    defaultValue: "Sync session logs",
  });

  return (
    <Button
      type="button"
      variant="outline"
      size="sm"
      className="h-9 gap-1.5"
      onClick={handleSync}
      disabled={syncing}
      title={label}
      aria-label={label}
    >
      {syncing ? (
        <Loader2 className="h-3.5 w-3.5 animate-spin" />
      ) : (
        <RefreshCw className="h-3.5 w-3.5" />
      )}
      {t("usage.sessionSync.resync", { defaultValue: "Sync" })}
    </Button>
  );
}
