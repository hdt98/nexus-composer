import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { exit } from "@tauri-apps/plugin-process";
import {
  Database,
  Download,
  RefreshCw,
  ExternalLink,
  FolderOpen,
  Loader2,
  AlertTriangle,
} from "lucide-react";
import { Button } from "@/components/ui/button";

const RELEASES_URL = "https://github.com/hdt98/nexus-composer/releases";

interface DatabaseUpgradeProps {
  payload: {
    path?: string;
    error?: string;
    kind?: string;
    db_version?: number;
    supported_version?: number;
  };
}

// checking: checking for an available update during startup
// upgradable: an update is available and upgrading the app should resolve the issue
// incompatible: the app is current but the database is still too new, possibly from a third-party client
// updating: downloading or installing the update
// error: the upgrade failed
type Phase = "checking" | "upgradable" | "incompatible" | "updating" | "error";

interface DownloadProgress {
  downloaded: number;
  total: number | null;
}

/**
 * In-app recovery UI for a database that is newer than the current app supports.
 *
 * Check for an available update during startup:
 * - If available, offer a one-click app upgrade that downloads, installs, and restarts with progress shown.
 * - If unavailable, explain that the current app is already up to date but the database remains incompatible,
 *   usually because a third-party client or newer build created it. Prompt the user to back up their data
 *   and use a compatible client or wait for official support.
 */
export function DatabaseUpgrade({ payload }: DatabaseUpgradeProps) {
  const { t } = useTranslation();
  const [phase, setPhase] = useState<Phase>("checking");
  const [availableVersion, setAvailableVersion] = useState<string | null>(null);
  const [progress, setProgress] = useState<DownloadProgress | null>(null);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const unlistenRef = useRef<(() => void) | null>(null);

  const dbVersion = payload.db_version;
  const supportedVersion = payload.supported_version;

  // Check for an available update during startup to choose the upgradable or incompatible phase.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const version = await invoke<string | null>(
          "check_app_update_available",
        );
        if (cancelled) return;
        if (version) {
          setAvailableVersion(version);
          setPhase("upgradable");
        } else {
          setPhase("incompatible");
        }
      } catch {
        // If the check fails, for example while offline, still allow an upgrade attempt to avoid a dead end.
        if (!cancelled) setPhase("upgradable");
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    return () => {
      unlistenRef.current?.();
    };
  }, []);

  const startUpgrade = useCallback(async () => {
    setPhase("updating");
    setProgress(null);
    setErrorMsg(null);
    try {
      unlistenRef.current?.();
      unlistenRef.current = await listen<DownloadProgress>(
        "update-download-progress",
        (e) => setProgress(e.payload),
      );
      // On success, the backend downloads, installs, and restarts without returning. False means no update is available.
      const updating = await invoke<boolean>("install_update_and_restart");
      unlistenRef.current?.();
      unlistenRef.current = null;
      if (!updating) {
        // Race: an update existed during the check but not during installation. Treat it as incompatible.
        setPhase("incompatible");
      }
      // If updating is true, the app is about to restart. Remain in the updating phase until the process exits.
    } catch (e) {
      unlistenRef.current?.();
      unlistenRef.current = null;
      setErrorMsg(e instanceof Error ? e.message : String(e));
      setPhase("error");
    }
  }, []);

  const percent =
    progress && progress.total && progress.total > 0
      ? Math.min(100, Math.round((progress.downloaded / progress.total) * 100))
      : null;
  const fmtMB = (n: number) => (n / 1024 / 1024).toFixed(1);

  const isIncompatible = phase === "incompatible";
  const accent = isIncompatible
    ? {
        chip: "bg-red-100 text-red-600 dark:bg-red-950/50 dark:text-red-400",
        Icon: AlertTriangle,
      }
    : {
        chip: "bg-amber-100 text-amber-600 dark:bg-amber-950/50 dark:text-amber-400",
        Icon: Database,
      };
  const AccentIcon = accent.Icon;

  return (
    <div className="flex min-h-screen items-center justify-center bg-background p-6 text-foreground">
      <div className="w-full max-w-lg space-y-5 rounded-2xl border border-border/60 bg-card/80 p-7 shadow-xl">
        <div className="flex items-start gap-4">
          <div
            className={`flex h-12 w-12 shrink-0 items-center justify-center rounded-xl ${accent.chip}`}
          >
            <AccentIcon className="h-6 w-6" />
          </div>
          <div className="space-y-1">
            <h1 className="text-lg font-semibold">
              {t("dbUpgrade.title", "Database version is too new")}
            </h1>
            <p className="text-sm text-muted-foreground">
              {t(
                "dbUpgrade.description",
                "This database was created by a newer version of Nexus Composer. Upgrade the app to continue. Upgrading will not delete your data.",
              )}
            </p>
            {dbVersion != null && supportedVersion != null && (
              <p className="pt-0.5 text-xs text-muted-foreground tabular-nums">
                {t("dbUpgrade.versionInfo", {
                  db: dbVersion,
                  supported: supportedVersion,
                  defaultValue:
                    "Database v{{db}} · App supports v{{supported}}",
                })}
              </p>
            )}
          </div>
        </div>

        {/* Error details and database path. */}
        <div className="space-y-1 rounded-lg border border-border/50 bg-muted/40 p-3 text-xs text-muted-foreground">
          {payload.error && (
            <p className="break-words font-mono">{payload.error}</p>
          )}
          {payload.path && (
            <p className="break-all">
              {t("dbUpgrade.dbPath", "Database file")}: {payload.path}
            </p>
          )}
        </div>

        {phase === "checking" && (
          <p className="flex items-center gap-2 text-sm text-muted-foreground">
            <Loader2 className="h-4 w-4 animate-spin" />
            {t("dbUpgrade.checking", "Checking for available updates…")}
          </p>
        )}

        {phase === "upgradable" && availableVersion && (
          <p className="text-sm text-muted-foreground">
            {t("dbUpgrade.updateAvailable", {
              version: availableVersion,
              defaultValue:
                "Version v{{version}} is available; upgrading will let you continue.",
            })}
          </p>
        )}

        {phase === "incompatible" && (
          <div className="space-y-2 rounded-lg border border-red-300/60 bg-red-50 p-3 text-sm text-red-700 dark:border-red-500/40 dark:bg-red-950/40 dark:text-red-300">
            <p className="font-medium">
              {t("dbUpgrade.incompatibleTitle", "Upgrading won't fix this")}
            </p>
            <p className="leading-relaxed">
              {t("dbUpgrade.incompatibleDescription", {
                db: dbVersion,
                supported: supportedVersion,
                defaultValue:
                  "You are already on the latest version, but the database (v{{db}}) is still newer than this app supports (v{{supported}}). It was likely created by a third-party client or a newer build, so upgrading the official app cannot make it compatible.",
              })}
            </p>
          </div>
        )}

        {phase === "updating" && (
          <div className="space-y-2">
            <div className="flex items-center justify-between text-sm">
              <span className="flex items-center gap-2 text-muted-foreground">
                <Loader2 className="h-4 w-4 animate-spin" />
                {percent === null
                  ? t("dbUpgrade.preparing", "Preparing update…")
                  : t("dbUpgrade.downloading", "Downloading update…")}
              </span>
              {percent !== null && (
                <span className="tabular-nums text-muted-foreground">
                  {percent}%
                </span>
              )}
            </div>
            <div className="h-2 w-full overflow-hidden rounded-full bg-muted">
              <div
                className={`h-full rounded-full bg-amber-500 transition-all duration-200 ${
                  percent === null ? "w-1/3 animate-pulse" : ""
                }`}
                style={percent === null ? undefined : { width: `${percent}%` }}
              />
            </div>
            {progress && (
              <p className="text-right text-xs tabular-nums text-muted-foreground">
                {fmtMB(progress.downloaded)} MB
                {progress.total ? ` / ${fmtMB(progress.total)} MB` : ""}
              </p>
            )}
          </div>
        )}

        {phase === "error" && errorMsg && (
          <p className="rounded-lg border border-red-300/60 bg-red-50 p-3 text-sm text-red-700 dark:border-red-500/40 dark:bg-red-950/40 dark:text-red-300">
            {errorMsg}
          </p>
        )}

        <div className="flex flex-wrap items-center gap-2">
          {(phase === "upgradable" || phase === "error") && (
            <Button
              onClick={startUpgrade}
              className="gap-2 bg-amber-500 text-white hover:bg-amber-600"
            >
              {phase === "error" ? (
                <RefreshCw className="h-4 w-4" />
              ) : (
                <Download className="h-4 w-4" />
              )}
              {phase === "error"
                ? t("dbUpgrade.retry", "Retry upgrade")
                : t("dbUpgrade.upgradeNow", "Upgrade app")}
            </Button>
          )}

          {(phase === "incompatible" || phase === "error") && (
            <Button
              variant="outline"
              className="gap-2"
              onClick={() =>
                void invoke("open_external", { url: RELEASES_URL })
              }
            >
              <ExternalLink className="h-4 w-4" />
              {t("dbUpgrade.openReleases", "Open releases page")}
            </Button>
          )}

          <Button
            variant="outline"
            className="gap-2"
            onClick={() => void invoke("open_app_config_folder")}
            disabled={phase === "updating"}
          >
            <FolderOpen className="h-4 w-4" />
            {t("dbUpgrade.openConfigDir", "Open config folder")}
          </Button>

          <Button
            variant="ghost"
            className="ml-auto text-muted-foreground"
            onClick={() => void exit(0)}
            disabled={phase === "updating"}
          >
            {t("dbUpgrade.quit", "Quit")}
          </Button>
        </div>
      </div>
    </div>
  );
}

export default DatabaseUpgrade;
