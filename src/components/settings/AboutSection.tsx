import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Download,
  Copy,
  ExternalLink,
  Github,
  Globe,
  Info,
  Loader2,
  RefreshCw,
  Terminal,
  CheckCircle2,
  AlertCircle,
  ArrowUpCircle,
  ChevronDown,
  Stethoscope,
} from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import { getVersion } from "@tauri-apps/api/app";
import { settingsApi } from "@/lib/api";
import type {
  ToolInstallation,
  ToolInstallationReport,
} from "@/lib/api/settings";
import { useUpdate } from "@/contexts/UpdateContext";
import { Badge } from "@/components/ui/badge";
import { motion } from "framer-motion";
import appIcon from "@/assets/icons/app-icon.png";
import { APP_ICON_MAP } from "@/config/appConfig";
import type { AppId } from "@/lib/api/types";
import { extractErrorMessage } from "@/utils/errorUtils";
import { isWindows } from "@/lib/platform";
import { isUpdateAvailable } from "@/lib/version";
import { ToolUpgradeConfirmDialog } from "./ToolUpgradeConfirmDialog";
import { ToolInstallRow } from "./ToolInstallRow";

interface AboutSectionProps {
  isPortable: boolean;
}

interface ToolVersion {
  name: string;
  version: string | null;
  latest_version: string | null;
  error: string | null;
  // The backend found the executable, but `--version` failed after install.
  // Read this field directly instead of inferring from error text to avoid
  // brittle frontend-backend string coupling.
  installed_but_broken: boolean;
  env_type: "windows" | "wsl" | "macos" | "linux" | "unknown";
  wsl_distro: string | null;
}

const TOOL_NAMES = [
  "claude",
  "codex",
  "gemini",
  "opencode",
  "openclaw",
  "hermes",
] as const;
type ToolName = (typeof TOOL_NAMES)[number];
type ToolLifecycleAction = "install" | "update";

type WslShellPreference = {
  wslShell?: string | null;
  wslShellFlag?: string | null;
};

const WSL_SHELL_OPTIONS = ["sh", "bash", "zsh", "fish", "dash"] as const;
// UI-friendly order: login shell first.
const WSL_SHELL_FLAG_OPTIONS = ["-lic", "-lc", "-c"] as const;

const ENV_BADGE_CONFIG: Record<
  string,
  { labelKey: string; className: string }
> = {
  wsl: {
    labelKey: "settings.envBadge.wsl",
    className:
      "bg-orange-500/10 text-orange-600 dark:text-orange-400 border-orange-500/20",
  },
  windows: {
    labelKey: "settings.envBadge.windows",
    className:
      "bg-blue-500/10 text-blue-600 dark:text-blue-400 border-blue-500/20",
  },
  macos: {
    labelKey: "settings.envBadge.macos",
    className:
      "bg-gray-500/10 text-gray-600 dark:text-gray-400 border-gray-500/20",
  },
  linux: {
    labelKey: "settings.envBadge.linux",
    className:
      "bg-green-500/10 text-green-600 dark:text-green-400 border-green-500/20",
  },
};

const posixScriptInstallCommand = (url: string) =>
  `bash -c 'tmp=$(mktemp) && curl -fsSL ${url} -o $tmp && bash $tmp; status=$?; rm -f $tmp; exit $status'`;

const HERMES_WINDOWS_INSTALL_SCRIPT =
  "irm https://raw.githubusercontent.com/NousResearch/hermes-agent/main/scripts/install.ps1 | iex";

const powershellEncodedCommand = (script: string): string => {
  let binary = "";
  for (let i = 0; i < script.length; i += 1) {
    const code = script.charCodeAt(i);
    binary += String.fromCharCode(code & 0xff, code >> 8);
  }
  return btoa(binary);
};

const HERMES_WINDOWS_INSTALL_COMMAND = `powershell -NoProfile -ExecutionPolicy Bypass -EncodedCommand ${powershellEncodedCommand(
  HERMES_WINDOWS_INSTALL_SCRIPT,
)}`;

const POSIX_ONE_CLICK_INSTALL_COMMANDS = `# Claude Code
${posixScriptInstallCommand("https://claude.ai/install.sh")} || npm i -g @anthropic-ai/claude-code@latest
# Codex
npm i -g @openai/codex@latest
# Gemini CLI
npm i -g @google/gemini-cli@latest
# OpenCode
${posixScriptInstallCommand("https://opencode.ai/install")} || npm i -g opencode-ai@latest
# OpenClaw
npm i -g openclaw@latest
# Hermes
${posixScriptInstallCommand("https://raw.githubusercontent.com/NousResearch/hermes-agent/main/scripts/install.sh")}`;

const WINDOWS_ONE_CLICK_INSTALL_COMMANDS = `# Claude Code
npm i -g @anthropic-ai/claude-code@latest
# Codex
npm i -g @openai/codex@latest
# Gemini CLI
npm i -g @google/gemini-cli@latest
# OpenCode
npm i -g opencode-ai@latest
# OpenClaw
npm i -g openclaw@latest
# Hermes
${HERMES_WINDOWS_INSTALL_COMMAND}`;

const ONE_CLICK_INSTALL_COMMANDS = isWindows()
  ? WINDOWS_ONE_CLICK_INSTALL_COMMANDS
  : POSIX_ONE_CLICK_INSTALL_COMMANDS;

const TOOL_DISPLAY_NAMES: Record<ToolName, string> = {
  claude: "Claude Code",
  codex: "Codex",
  gemini: "Gemini CLI",
  opencode: "OpenCode",
  openclaw: "OpenClaw",
  hermes: "Hermes",
};

// The backend returns `tool` as string. Centralize the ToolName assertion and
// fallback here so the upgrade confirmation dialog can render stable labels
// without inline casts or per-render closures in JSX.
function toolDisplayName(tool: string): string {
  return TOOL_DISPLAY_NAMES[tool as ToolName] ?? tool;
}

const TOOL_APP_IDS: Record<ToolName, AppId> = {
  claude: "claude",
  codex: "codex",
  gemini: "gemini",
  opencode: "opencode",
  openclaw: "openclaw",
  hermes: "hermes",
};

// Tool version probing is expensive: one `--version` subprocess plus one
// npm/GitHub/PyPI request per tool.
// The settings page uses Radix Tabs, and inactive tabs unmount. Re-mounting
// AboutSection every time the user returns to "About" should not trigger a full
// re-probe. Keep a module-level cache alive across re-mounts, reuse it while it
// is fresh (`last full load < TTL`), and only force a full re-check when it
// expires or the user clicks Refresh. `at` tracks the completion time of the
// last full load. Single-tool refreshes after a shell change or upgrade update
// the data but do not refresh `at`, so a partial refresh does not extend the
// full-cache TTL.
const TOOL_VERSIONS_CACHE_TTL_MS = 10 * 60 * 1000; // 10 minutes
let toolVersionsCache: { data: ToolVersion[]; at: number } | null = null;
// Cache the app version too. `getVersion` is local and fast, but caching avoids
// a loading flicker on re-mount.
let appVersionCache: string | null = null;

// Merge probe results by name: replace existing entries, append new ones, and
// use the new list directly when the previous list is empty. Component state
// and the module cache share the same merge semantics.
function mergeToolVersions(
  prev: ToolVersion[],
  updated: ToolVersion[],
): ToolVersion[] {
  if (prev.length === 0) return updated;
  const byName = new Map(updated.map((t) => [t.name, t]));
  const merged = prev.map((t) => byName.get(t.name) ?? t);
  const existing = new Set(prev.map((t) => t.name));
  for (const u of updated) {
    if (!existing.has(u.name)) merged.push(u);
  }
  return merged;
}

export function AboutSection({ isPortable }: AboutSectionProps) {
  // ... (use hooks as before) ...
  const { t } = useTranslation();
  // Lazily initialize from the module cache so a re-mount can render the last
  // value on the first frame without a loading flicker. Fall back to the
  // original initial state on the first mount when the cache is empty.
  const [version, setVersion] = useState<string | null>(() => appVersionCache);
  const [isLoadingVersion, setIsLoadingVersion] = useState(
    () => appVersionCache === null,
  );
  const [isDownloading, setIsDownloading] = useState(false);
  const [toolVersions, setToolVersions] = useState<ToolVersion[]>(
    () => toolVersionsCache?.data ?? [],
  );
  // If any cache exists, show the stale value first and skip the initial
  // loading state. When expired, let the mount effect revalidate in the
  // background. Only the first uncached mount starts in loading state.
  const [isLoadingTools, setIsLoadingTools] = useState(
    () => toolVersionsCache === null,
  );
  const [toolActions, setToolActions] = useState<
    Partial<Record<ToolName, ToolLifecycleAction>>
  >({});
  const [batchAction, setBatchAction] = useState<ToolLifecycleAction | null>(
    null,
  );
  const [showInstallCommands, setShowInstallCommands] = useState(false);

  const { hasUpdate, updateInfo, checkUpdate, resetDismiss, isChecking } =
    useUpdate();

  const [wslShellByTool, setWslShellByTool] = useState<
    Record<string, WslShellPreference>
  >({});
  const [loadingTools, setLoadingTools] = useState<Record<string, boolean>>({});
  // Installation conflict diagnostics keyed by tool. Conflicts render under
  // the matching card. Data comes either from the top-level "Diagnose install
  // conflicts" action or from silent re-diagnosis after an upgrade that did
  // not change the detected version.
  const [toolDiagnostics, setToolDiagnostics] = useState<
    Partial<Record<ToolName, ToolInstallation[]>>
  >({});
  const [isDiagnosingAll, setIsDiagnosingAll] = useState(false);
  // Stash multi-install confirmations before upgrade. `toolNames` contains the
  // full target set for this run, while `plans` contains only the tools that
  // require confirmation because two or more installs were found. After user
  // confirmation, upgrade the full `toolNames` set.
  const [pendingUpgrade, setPendingUpgrade] = useState<{
    toolNames: ToolName[];
    plans: ToolInstallationReport[];
  } | null>(null);
  // Tools currently in upgrade preflight (the probe stage).
  // `probeToolInstallations` is a 1-3 second cross-process probe that runs
  // `--version` plus canonicalization for each tool. Before it returns,
  // `toolActions` and `batchAction` are still unset, so buttons would remain
  // enabled and a fast double-click could start two concurrent probe rounds and
  // then two concurrent `executeRun` calls (`npm i -g` or vendor installers
  // racing on the same writes). Track probe-time tools here and include them in
  // `isAnyBusy` to close that window. Use a Set instead of a boolean because
  // single-card and batch upgrades can preflight different tools independently,
  // and each card's disabled state should stay precise.
  const [preflightTools, setPreflightTools] = useState<Set<ToolName>>(
    () => new Set(),
  );

  const toolVersionByName = useMemo(() => {
    return new Map(toolVersions.map((tool) => [tool.name, tool]));
  }, [toolVersions]);

  const updatableToolNames = useMemo(
    () =>
      TOOL_NAMES.filter((toolName) => {
        const tool = toolVersionByName.get(toolName);
        return isUpdateAvailable(tool?.version, tool?.latest_version);
      }),
    [toolVersionByName],
  );

  const refreshToolVersions = useCallback(
    async (
      toolNames: ToolName[],
      wslOverrides?: Record<string, WslShellPreference>,
    ): Promise<ToolVersion[]> => {
      if (toolNames.length === 0) return [];

      // Single-tool refreshes still use the shared backend entry point
      // (`get_tool_versions`) with a tool filter.
      setLoadingTools((prev) => {
        const next = { ...prev };
        for (const name of toolNames) next[name] = true;
        return next;
      });

      try {
        const updated = await settingsApi.getToolVersions(
          toolNames,
          wslOverrides,
        );

        setToolVersions((prev) => mergeToolVersions(prev, updated));
        // Mirror the result into the module cache so tab switches can reuse it.
        // Preserve the timestamp from the last full load because a single-tool
        // refresh does not reset the full-cache TTL. When no cache exists yet,
        // start with `at = 0`. That sentinel means "no full load has completed"
        // so an interrupted partial cache is treated as expired and reloaded
        // instead of being reused as if it were complete. Only the `finally`
        // block in `loadAllToolVersions` writes a real full-load timestamp.
        toolVersionsCache = {
          data: mergeToolVersions(toolVersionsCache?.data ?? [], updated),
          at: toolVersionsCache?.at ?? 0,
        };

        // Return the refreshed rows so callers can check whether a version was
        // actually detected without reading stale closure state.
        return updated;
      } catch (error) {
        console.error("[AboutSection] Failed to refresh tools", error);
        return [];
      } finally {
        setLoadingTools((prev) => {
          const next = { ...prev };
          for (const name of toolNames) next[name] = false;
          return next;
        });
      }
    },
    [],
  );

  const loadAllToolVersions = useCallback(
    async (options?: { force?: boolean }) => {
      const force = options?.force ?? false;
      // Reuse a fresh cache when the user returns to the About tab so we can
      // skip six `--version` subprocesses and six latest-version requests.
      // Manual refresh passes `force` to bypass the cache.
      if (
        !force &&
        toolVersionsCache &&
        Date.now() - toolVersionsCache.at < TOOL_VERSIONS_CACHE_TTL_MS
      ) {
        setToolVersions(toolVersionsCache.data);
        setIsLoadingTools(false);
        return;
      }
      setIsLoadingTools(true);
      try {
        // Probe tools concurrently. As each result arrives, merge it into
        // `toolVersions`, mirror it into the module cache, and clear that tool's
        // loading flag so the matching card updates immediately instead of
        // waiting for all probes to finish. The backend previously awaited six
        // tools serially; concurrency collapses that into the slowest tool.
        // `refreshToolVersions` already handles merge-by-name, per-tool loading,
        // and local try/catch fallback, so `Promise.all` will not reject here.
        // Respect the current shell and flag overrides.
        await Promise.all(
          TOOL_NAMES.map((toolName) =>
            refreshToolVersions([toolName], wslShellByTool),
          ),
        );
      } finally {
        // Full probe finished. Stamp the cache with the current time and reset
        // the TTL window.
        if (toolVersionsCache) {
          toolVersionsCache = { ...toolVersionsCache, at: Date.now() };
        }
        setIsLoadingTools(false);
      }
    },
    [wslShellByTool, refreshToolVersions],
  );

  const handleToolShellChange = async (toolName: ToolName, value: string) => {
    const wslShell = value === "auto" ? null : value;
    const nextPref: WslShellPreference = {
      ...(wslShellByTool[toolName] ?? {}),
      wslShell,
    };
    setWslShellByTool((prev) => ({ ...prev, [toolName]: nextPref }));
    await refreshToolVersions([toolName], { [toolName]: nextPref });
  };

  const handleToolShellFlagChange = async (
    toolName: ToolName,
    value: string,
  ) => {
    const wslShellFlag = value === "auto" ? null : value;
    const nextPref: WslShellPreference = {
      ...(wslShellByTool[toolName] ?? {}),
      wslShellFlag,
    };
    setWslShellByTool((prev) => ({ ...prev, [toolName]: nextPref }));
    await refreshToolVersions([toolName], { [toolName]: nextPref });
  };

  useEffect(() => {
    let active = true;

    // Load the app version through a local `getVersion` call. It is separate
    // from tool probing and does not need network access. These used to share
    // one Promise.all, which delayed `setVersion` and `setIsLoadingVersion`
    // until every tool check finished, so the version badge under the icon had
    // to wait for all six tool probes. Split them into independent paths so the
    // app version appears immediately and tool cards refresh progressively.
    const loadAppVersion = async () => {
      try {
        const appVersion = await getVersion();
        appVersionCache = appVersion;
        if (active) {
          setVersion(appVersion);
        }
      } catch (error) {
        console.error("[AboutSection] Failed to load app version", error);
        if (active) {
          setVersion(null);
        }
      } finally {
        if (active) {
          setIsLoadingVersion(false);
        }
      }
    };

    void loadAppVersion();
    void loadAllToolVersions();
    return () => {
      active = false;
    };
    // Mount-only: loadAllToolVersions is intentionally excluded to avoid
    // re-fetching all tools whenever wslShellByTool changes. Single-tool
    // refreshes are handled by refreshToolVersions in the shell/flag handlers.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // ... (handlers like handleOpenReleaseNotes, handleCheckUpdate) ...

  const handleOpenReleaseNotes = useCallback(async () => {
    try {
      const targetVersion = updateInfo?.availableVersion ?? version ?? "";
      const displayVersion = targetVersion.startsWith("v")
        ? targetVersion
        : targetVersion
          ? `v${targetVersion}`
          : "";

      if (!displayVersion) {
        await settingsApi.openExternal(
          "https://github.com/hdt98/nexus-composer/releases",
        );
        return;
      }

      await settingsApi.openExternal(
        `https://github.com/hdt98/nexus-composer/releases/tag/${displayVersion}`,
      );
    } catch (error) {
      console.error("[AboutSection] Failed to open release notes", error);
      toast.error(t("settings.openReleaseNotesFailed"));
    }
  }, [t, updateInfo?.availableVersion, version]);

  const handleCheckUpdate = useCallback(async () => {
    if (hasUpdate) {
      if (isPortable) {
        try {
          await settingsApi.checkUpdates();
        } catch (error) {
          console.error("[AboutSection] Portable update failed", error);
        }
        return;
      }

      setIsDownloading(true);
      try {
        resetDismiss();
        const installed = await settingsApi.installUpdateAndRestart();
        if (!installed) {
          toast.success(t("settings.upToDate"), { closeButton: true });
        }
      } catch (error) {
        console.error("[AboutSection] Update failed", error);
        toast.error(t("settings.updateFailed"), {
          description: extractErrorMessage(error) || undefined,
          closeButton: true,
        });
        try {
          await settingsApi.checkUpdates();
        } catch (fallbackError) {
          console.error(
            "[AboutSection] Failed to open fallback updater",
            fallbackError,
          );
        }
      } finally {
        setIsDownloading(false);
      }
      return;
    }

    try {
      const available = await checkUpdate();
      if (!available) {
        toast.success(t("settings.upToDate"), { closeButton: true });
      }
    } catch (error) {
      console.error("[AboutSection] Check update failed", error);
      toast.error(t("settings.checkUpdateFailed"));
    }
  }, [checkUpdate, hasUpdate, isPortable, resetDismiss, t]);

  const handleCopyInstallCommands = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(ONE_CLICK_INSTALL_COMMANDS);
      toast.success(t("settings.installCommandsCopied"), { closeButton: true });
    } catch (error) {
      console.error("[AboutSection] Failed to copy install commands", error);
      toast.error(t("settings.installCommandsCopyFailed"));
    }
  }, [t]);

  // After an upgrade, silently re-diagnose a single tool in the background.
  // Save conflicts when found, or clear any stale conflict UI for that tool
  // when the issue is gone after an external uninstall or repair. Do not toast
  // or surface errors here; this is intentionally quieter than the explicit
  // full diagnostic action.
  const diagnoseToolSilently = useCallback(async (toolName: ToolName) => {
    try {
      const [report] = await settingsApi.probeToolInstallations([toolName]);
      setToolDiagnostics((prev) => {
        if (report?.is_conflict) {
          return { ...prev, [toolName]: report.installs };
        }
        // No conflict: clear any stale entry. Return the same reference when no
        // previous result exists so React can skip an unnecessary re-render.
        if (!(toolName in prev)) return prev;
        const next = { ...prev };
        delete next[toolName];
        return next;
      });
    } catch (error) {
      console.error(
        `[AboutSection] Auto-diagnose failed for ${toolName}`,
        error,
      );
    }
  }, []);

  // Top-level action: diagnose all six tools at once. Conflicts are stored on
  // their matching cards, and a no-conflict run emits one info toast. The
  // backend enumerates all installs per tool and decides whether they diverge.
  const handleDiagnoseAll = useCallback(async () => {
    setIsDiagnosingAll(true);
    try {
      const reports = await settingsApi.probeToolInstallations([...TOOL_NAMES]);
      const next: Partial<Record<ToolName, ToolInstallation[]>> = {};
      let conflicts = 0;
      for (const report of reports) {
        if (report.is_conflict) {
          next[report.tool as ToolName] = report.installs;
          conflicts += 1;
        }
      }
      setToolDiagnostics(next);
      if (conflicts === 0) {
        toast.info(t("settings.toolDiagnoseNoConflict"), { closeButton: true });
      }
    } catch (error) {
      console.error("[AboutSection] Diagnose all failed", error);
      toast.error(t("settings.toolDiagnoseFailed"), {
        description: extractErrorMessage(error) || undefined,
        closeButton: true,
      });
    } finally {
      setIsDiagnosingAll(false);
    }
  }, [t]);

  // Run install or upgrade operations serially after any required confirmation.
  const executeRun = useCallback(
    async (toolNames: ToolName[], action: ToolLifecycleAction) => {
      const isBatch = toolNames.length > 1;
      if (isBatch) {
        setBatchAction(action);
      }

      // Execute tools one by one. Each tool succeeds or fails independently and
      // refreshes its own version, so one failure does not abort later tools.
      // The backend previously built one `set -e` script for the whole batch,
      // which stopped on the first failure. `soft=true` means the command ran
      // successfully but still needs user intervention, such as an unchanged
      // version or an installed-but-not-runnable tool. That is different from a
      // hard command failure (`soft=false`) and should downgrade the toast to a
      // warning.
      const failures: {
        toolName: ToolName;
        detail: string;
        soft: boolean;
        kind?: "notRunnable" | "versionUnchanged";
      }[] = [];
      let succeeded = 0;

      for (const toolName of toolNames) {
        setToolActions((prev) => ({ ...prev, [toolName]: action }));
        try {
          const previousTool = toolVersionByName.get(toolName);
          const previousVersion = previousTool?.version ?? null;
          const previousLatestVersion = previousTool?.latest_version ?? null;

          await settingsApi.runToolLifecycleAction(
            [toolName],
            action,
            wslShellByTool,
          );
          // Refresh the tool version only after the command has fully finished
          // so the card reflects the final result immediately.
          const refreshed = await refreshToolVersions(
            [toolName],
            wslShellByTool,
          );
          const tool = refreshed.find((t) => t.name === toolName);
          if (tool?.version) {
            const latestVersion = tool.latest_version ?? previousLatestVersion;
            const versionUnchangedAfterUpdate =
              action === "update" &&
              Boolean(previousVersion) &&
              tool.version === previousVersion &&
              isUpdateAvailable(tool.version, latestVersion);

            if (versionUnchangedAfterUpdate) {
              // Some upstream updaters still return exit code 0 when the version
              // did not actually change. Re-check the refreshed current version
              // against `latest_version` here to avoid a false success toast.
              failures.push({
                toolName,
                detail: t("settings.toolActionVersionUnchanged", {
                  version: tool.version,
                  latest: latestVersion ?? t("common.unknown"),
                }),
                soft: true,
                kind: "versionUnchanged",
              });
              void diagnoseToolSilently(toolName);
            } else {
              succeeded += 1;
              // Always re-diagnose after a successful upgrade. If the version
              // stayed the same, another install may still be shadowing it. If
              // the version changed, another install may still coexist. In both
              // cases, refresh the conflict UI. `diagnoseToolSilently` clears
              // stale results automatically when no conflict remains.
              if (action === "update") {
                void diagnoseToolSilently(toolName);
              }
            }
          } else {
            // Exit code 0 but still no version after refresh usually means the
            // tool installed but cannot run, for example because OpenClaw needs
            // a newer Node version. `refreshToolVersions` has already merged the
            // backend error and cleared `version`, so classify this as a soft
            // failure and surface the reason.
            const detail = tool?.error?.trim() || t("settings.toolNotRunnable");
            failures.push({
              toolName,
              detail,
              soft: true,
              kind: "notRunnable",
            });
            // Installed-but-not-runnable can also come from multiple installs,
            // so auto-diagnose to help the user locate the conflict.
            void diagnoseToolSilently(toolName);
          }
        } catch (error) {
          console.error(
            `[AboutSection] Failed to run tool action for ${toolName}`,
            error,
          );
          const detail = extractErrorMessage(error) || String(error);
          failures.push({ toolName, detail, soft: false });
        } finally {
          setToolActions((prev) => {
            const next = { ...prev };
            delete next[toolName];
            return next;
          });
        }
      }

      if (isBatch) {
        setBatchAction(null);
      }

      const actionLabel =
        action === "install"
          ? t("settings.toolInstall")
          : t("settings.toolUpdate");

      if (failures.length === 0) {
        toast.success(
          t("settings.toolActionDone", {
            count: succeeded,
            action: actionLabel,
          }),
          { closeButton: true },
        );
        return;
      }

      // In batch mode, keep only the last error line per tool. In single-tool
      // mode, show the full detail.
      const lastLine = (text: string) => {
        const lines = text.trim().split("\n").filter(Boolean);
        return lines[lines.length - 1] ?? text;
      };
      const failureDescription = isBatch
        ? failures
            .map(
              (f) => `${TOOL_DISPLAY_NAMES[f.toolName]}: ${lastLine(f.detail)}`,
            )
            .join("\n")
        : failures[0]?.detail;

      const hardFailures = failures.filter((f) => !f.soft);
      const allSoftVersionUnchanged =
        failures.length > 0 &&
        failures.every((f) => f.soft && f.kind === "versionUnchanged");

      if (succeeded === 0 && hardFailures.length === 0) {
        // Commands ran successfully, but the result still needs user action
        // such as an unchanged version or an installed-but-not-runnable tool.
        // Downgrade to a warning and explain why.
        toast.warning(
          allSoftVersionUnchanged
            ? t("settings.toolActionVersionUnchangedTitle")
            : t("settings.toolActionInstalledNotRunnable"),
          {
            description: failureDescription || undefined,
            closeButton: true,
          },
        );
      } else if (succeeded === 0) {
        toast.error(t("settings.toolActionFailed"), {
          description: failureDescription || undefined,
          closeButton: true,
        });
      } else {
        // Partial success: summarize counts in a warning and list failed tools.
        toast.warning(
          t("settings.toolActionPartial", {
            succeeded,
            failed: failures.length,
            action: actionLabel,
          }),
          { description: failureDescription || undefined, closeButton: true },
        );
      }
    },
    [
      t,
      wslShellByTool,
      toolVersionByName,
      refreshToolVersions,
      diagnoseToolSilently,
    ],
  );

  // Shared entry lock for installs and upgrades. Every action registers
  // preflight at entry and unlocks on exit. This covers three race windows:
  //   1. The `probeToolInstallations` phase before update execution.
  //   2. The microtasks inside `executeRun` before `setToolActions` commits.
  //   3. The same microtask window for installs that jump straight to
  //      `executeRun`.
  // Early return avoids queueing the same tool twice when `toolActions` or
  // preflight already contains it. In batch mode, if any tool is locked, do
  // not start a second round because the backend assumes one serial `set -e`
  // script, and concurrent IPC calls would break that model.
  const handleRunToolAction = useCallback(
    async (toolNames: ToolName[], action: ToolLifecycleAction) => {
      if (toolNames.length === 0) return;
      if (
        toolNames.some(
          (name) => preflightTools.has(name) || toolActions[name] !== undefined,
        )
      ) {
        return;
      }
      // Register preflight immediately so buttons disable at once. `new
      // Set(prev)` preserves immutable updates; mutating the original Set would
      // keep the same reference and let React skip the re-render. The `finally`
      // block handles unlocks on error paths too.
      setPreflightTools((prev) => {
        const next = new Set(prev);
        toolNames.forEach((name) => next.add(name));
        return next;
      });
      try {
        if (action === "install") {
          await executeRun(toolNames, action);
          return;
        }
        let reports: ToolInstallationReport[];
        try {
          reports = await settingsApi.probeToolInstallations(toolNames);
        } catch (error) {
          // Probe failure should not block the upgrade. Fall back to direct
          // execution to preserve the previous behavior.
          console.error("[AboutSection] probeToolInstallations failed", error);
          await executeRun(toolNames, action);
          return;
        }
        const needConfirm = reports.filter((r) => r.needs_confirmation);
        if (needConfirm.length === 0) {
          await executeRun(toolNames, action);
          return;
        }
        setPendingUpgrade({ toolNames, plans: needConfirm });
      } finally {
        setPreflightTools((prev) => {
          const next = new Set(prev);
          toolNames.forEach((name) => next.delete(name));
          return next;
        });
      }
    },
    [executeRun, preflightTools, toolActions],
  );

  const handleConfirmUpgrade = useCallback(() => {
    if (pendingUpgrade) {
      void executeRun(pendingUpgrade.toolNames, "update");
    }
    setPendingUpgrade(null);
  }, [pendingUpgrade, executeRun]);

  const handleCancelUpgrade = useCallback(() => setPendingUpgrade(null), []);

  const displayVersion = version ?? t("common.unknown");

  // Treat any install or upgrade, batch or single-tool, as a busy state so all
  // action buttons can be disabled and concurrent global npm/pip writes do not
  // collide. `preflightTools` also covers the probe stage before `executeRun`;
  // without it, there would be a 1-3 second concurrency window before
  // `toolActions` is set.
  const isAnyBusy =
    Boolean(batchAction) ||
    Object.keys(toolActions).length > 0 ||
    preflightTools.size > 0;

  return (
    <motion.section
      initial={{ opacity: 0, y: 10 }}
      animate={{ opacity: 1, y: 0 }}
      transition={{ duration: 0.3 }}
      className="space-y-6"
    >
      <header className="space-y-1">
        <h3 className="text-sm font-medium">{t("common.about")}</h3>
        <p className="text-xs text-muted-foreground">
          {t("settings.aboutHint")}
        </p>
      </header>

      <motion.div
        initial={{ opacity: 0, scale: 0.98 }}
        animate={{ opacity: 1, scale: 1 }}
        transition={{ duration: 0.3, delay: 0.1 }}
        className="rounded-xl border border-border bg-gradient-to-br from-card/80 to-card/40 p-6 space-y-5 shadow-sm"
      >
        <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between">
          <div className="flex items-center gap-8">
            <div className="flex flex-col items-center gap-2">
              <div className="flex items-center gap-2">
                <div className="h-20 w-20 rounded-2xl overflow-hidden border border-gray-300 dark:border-gray-600 shadow-sm">
                  <img
                    src={appIcon}
                    alt="Nexus Composer"
                    className="w-full h-full object-contain"
                  />
                </div>
                <h4 className="text-lg font-semibold text-foreground">
                  Nexus Composer
                </h4>
              </div>
              <div className="flex items-center gap-2">
                <Badge variant="outline" className="gap-1.5 bg-background/80">
                  <span className="text-muted-foreground">
                    {t("common.version")}
                  </span>
                  {isLoadingVersion ? (
                    <Loader2 className="h-3 w-3 animate-spin" />
                  ) : (
                    <span className="font-medium">{`v${displayVersion}`}</span>
                  )}
                </Badge>
                {isPortable && (
                  <Badge variant="secondary" className="gap-1.5">
                    <Info className="h-3 w-3" />
                    {t("settings.portableMode")}
                  </Badge>
                )}
              </div>
            </div>
          </div>

          <div className="flex items-center gap-2">
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() =>
                settingsApi.openExternal(
                  "https://github.com/hdt98/nexus-composer",
                )
              }
              className="h-8 gap-1.5 text-xs"
            >
              <Globe className="h-3.5 w-3.5" />
              {t("settings.officialWebsite")}
            </Button>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={() =>
                settingsApi.openExternal(
                  "https://github.com/hdt98/nexus-composer",
                )
              }
              className="h-8 gap-1.5 text-xs"
            >
              <Github className="h-3.5 w-3.5" />
              {t("settings.github")}
            </Button>
            <Button
              type="button"
              variant="outline"
              size="sm"
              onClick={handleOpenReleaseNotes}
              className="h-8 gap-1.5 text-xs"
            >
              <ExternalLink className="h-3.5 w-3.5" />
              {t("settings.releaseNotes")}
            </Button>
            <Button
              type="button"
              size="sm"
              onClick={handleCheckUpdate}
              disabled={isChecking || isDownloading}
              className="h-8 gap-1.5 text-xs"
            >
              {isDownloading ? (
                <>
                  <Loader2 className="h-3.5 w-3.5 animate-spin" />
                  {t("settings.updating")}
                </>
              ) : hasUpdate ? (
                <>
                  <Download className="h-3.5 w-3.5" />
                  {t("settings.updateTo", {
                    version: updateInfo?.availableVersion ?? "",
                  })}
                </>
              ) : isChecking ? (
                <>
                  <RefreshCw className="h-3.5 w-3.5 animate-spin" />
                  {t("settings.checking")}
                </>
              ) : (
                <>
                  <RefreshCw className="h-3.5 w-3.5" />
                  {t("settings.checkForUpdates")}
                </>
              )}
            </Button>
          </div>
        </div>

        {hasUpdate && updateInfo && (
          <motion.div
            initial={{ opacity: 0, height: 0 }}
            animate={{ opacity: 1, height: "auto" }}
            className="rounded-lg bg-primary/10 border border-primary/20 px-4 py-3 text-sm"
          >
            <p className="font-medium text-primary mb-1">
              {t("settings.updateAvailable", {
                version: updateInfo.availableVersion,
              })}
            </p>
            {updateInfo.notes && (
              <p className="text-muted-foreground line-clamp-3 leading-relaxed">
                {updateInfo.notes}
              </p>
            )}
          </motion.div>
        )}
      </motion.div>

      <div className="space-y-3">
        <div className="flex flex-col gap-2 px-1 sm:flex-row sm:items-center sm:justify-between">
          <h3 className="text-sm font-medium">{t("settings.localEnvCheck")}</h3>
          <div className="flex flex-wrap items-center gap-2">
            <Button
              size="sm"
              variant="outline"
              className="h-7 gap-1.5 text-xs"
              onClick={() => handleDiagnoseAll()}
              disabled={isLoadingTools || isAnyBusy || isDiagnosingAll}
            >
              {isDiagnosingAll ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <Stethoscope className="h-3.5 w-3.5" />
              )}
              {isDiagnosingAll
                ? t("settings.toolDiagnosing")
                : t("settings.toolDiagnose")}
            </Button>
            <Button
              size="sm"
              variant="outline"
              className="h-7 gap-1.5 text-xs"
              onClick={() => loadAllToolVersions({ force: true })}
              disabled={isLoadingTools || isAnyBusy}
            >
              <RefreshCw
                className={
                  isLoadingTools ? "h-3.5 w-3.5 animate-spin" : "h-3.5 w-3.5"
                }
              />
              {isLoadingTools ? t("common.refreshing") : t("common.refresh")}
            </Button>
            <Button
              size="sm"
              className="h-7 gap-1.5 text-xs"
              onClick={() => handleRunToolAction(updatableToolNames, "update")}
              disabled={
                isLoadingTools || isAnyBusy || updatableToolNames.length === 0
              }
            >
              {batchAction === "update" ? (
                <Loader2 className="h-3.5 w-3.5 animate-spin" />
              ) : (
                <ArrowUpCircle className="h-3.5 w-3.5" />
              )}
              {t("settings.updateAllTools", {
                count: updatableToolNames.length,
              })}
            </Button>
          </div>
        </div>

        <div className="grid gap-3 px-1 sm:grid-cols-2 xl:grid-cols-3">
          {TOOL_NAMES.map((toolName, index) => {
            const tool = toolVersionByName.get(toolName);
            const appConfig = APP_ICON_MAP[TOOL_APP_IDS[toolName]];
            const displayName = TOOL_DISPLAY_NAMES[toolName];
            // Drive per-card loading from result availability instead of waiting
            // for the whole batch to finish so cards can refresh progressively:
            //   - `loadingTools[t]`: this tool is currently probing.
            //   - `isLoadingTools && !has(t)`: the batch is still running and
            //     this tool has not returned yet, which preserves the loading
            //     appearance for unfinished cards on first render or refresh.
            // Once a tool lands in `toolVersions`, `has(t)` becomes true and
            // that card leaves loading immediately even if global
            // `isLoadingTools` is still true.
            const isToolVersionLoading =
              Boolean(loadingTools[toolName]) ||
              (isLoadingTools && !toolVersionByName.has(toolName));
            const isOutdated = isUpdateAvailable(
              tool?.version,
              tool?.latest_version,
            );
            // Distinguish installed-but-not-runnable tools, such as cases where
            // the Node version is too old. That lets the card show the right
            // copy and hide a useless install button instead of mislabeling the
            // tool as not installed.
            const installedButBroken = Boolean(tool?.installed_but_broken);
            // Neither loading nor broken states have an actionable button.
            // Otherwise choose by installed vs outdated state.
            const action: ToolLifecycleAction | null =
              isToolVersionLoading || installedButBroken
                ? null
                : !tool?.version
                  ? "install"
                  : isOutdated
                    ? "update"
                    : null;
            const runningAction = toolActions[toolName];
            const title = tool?.version || tool?.error || t("common.unknown");
            const conflicts = toolDiagnostics[toolName];

            return (
              <motion.div
                key={toolName}
                initial={{ opacity: 0, y: 10 }}
                animate={{ opacity: 1, y: 0 }}
                transition={{ duration: 0.3, delay: 0.15 + index * 0.04 }}
                className="flex min-h-[150px] flex-col gap-3 rounded-xl border border-border bg-gradient-to-br from-card/80 to-card/40 p-4 shadow-sm transition-colors hover:border-primary/30"
              >
                <div className="flex items-start justify-between gap-3">
                  <div className="flex min-w-0 items-center gap-2">
                    <span className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md bg-background/80 text-muted-foreground">
                      {appConfig?.icon ?? <Terminal className="h-4 w-4" />}
                    </span>
                    <div className="min-w-0">
                      <div className="truncate text-sm font-medium">
                        {displayName}
                      </div>
                      {tool?.env_type && ENV_BADGE_CONFIG[tool.env_type] && (
                        <span
                          className={`mt-1 inline-flex w-fit text-[9px] px-1.5 py-0.5 rounded-full border ${ENV_BADGE_CONFIG[tool.env_type].className}`}
                        >
                          {t(ENV_BADGE_CONFIG[tool.env_type].labelKey)}
                          {tool.wsl_distro
                            ? ` ${String.fromCharCode(0xb7)} ${tool.wsl_distro}`
                            : ""}
                        </span>
                      )}
                    </div>
                  </div>
                  {isToolVersionLoading ? (
                    <Loader2 className="mt-1 h-4 w-4 animate-spin text-muted-foreground" />
                  ) : tool?.version ? (
                    isOutdated ? (
                      <span className="mt-1 shrink-0 rounded-full border border-yellow-500/20 bg-yellow-500/10 px-1.5 py-0.5 text-[10px] text-yellow-600 dark:text-yellow-400">
                        {t("settings.updateAvailableShort")}
                      </span>
                    ) : (
                      <CheckCircle2 className="mt-1 h-4 w-4 shrink-0 text-green-500" />
                    )
                  ) : (
                    <AlertCircle className="mt-1 h-4 w-4 shrink-0 text-yellow-500" />
                  )}
                </div>

                <div className="space-y-1.5 text-xs">
                  <div className="flex items-center justify-between gap-3">
                    <span className="text-muted-foreground">
                      {t("settings.currentVersion")}
                    </span>
                    <span
                      className="min-w-0 truncate font-mono text-foreground"
                      title={title}
                    >
                      {isToolVersionLoading
                        ? t("common.loading")
                        : tool?.version
                          ? tool.version
                          : installedButBroken
                            ? t("settings.installedNotRunnable")
                            : t("common.notInstalled")}
                    </span>
                  </div>
                  <div className="flex items-center justify-between gap-3">
                    <span className="text-muted-foreground">
                      {t("settings.latestVersion")}
                    </span>
                    <span className="min-w-0 truncate font-mono text-foreground">
                      {isToolVersionLoading
                        ? t("common.loading")
                        : tool?.latest_version || t("common.unknown")}
                    </span>
                  </div>
                  {!isToolVersionLoading && !tool?.version && tool?.error && (
                    <div className="truncate text-[11px] text-muted-foreground">
                      {tool.error}
                    </div>
                  )}
                </div>

                {tool?.env_type === "wsl" && (
                  <div className="flex flex-wrap gap-2">
                    <Select
                      value={wslShellByTool[toolName]?.wslShell || "auto"}
                      onValueChange={(v) => handleToolShellChange(toolName, v)}
                      disabled={isToolVersionLoading || isAnyBusy}
                    >
                      <SelectTrigger className="h-7 w-[82px] text-xs">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="auto">{t("common.auto")}</SelectItem>
                        {WSL_SHELL_OPTIONS.map((shell) => (
                          <SelectItem key={shell} value={shell}>
                            {shell}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                    <Select
                      value={wslShellByTool[toolName]?.wslShellFlag || "auto"}
                      onValueChange={(v) =>
                        handleToolShellFlagChange(toolName, v)
                      }
                      disabled={isToolVersionLoading || isAnyBusy}
                    >
                      <SelectTrigger className="h-7 w-[82px] text-xs">
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="auto">{t("common.auto")}</SelectItem>
                        {WSL_SHELL_FLAG_OPTIONS.map((flag) => (
                          <SelectItem key={flag} value={flag}>
                            {flag}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </div>
                )}

                {/* Render multi-install conflict diagnostics only after lazy
                    data has been loaded. */}
                {conflicts && conflicts.length > 0 && (
                  <div className="space-y-1.5 rounded-lg border border-yellow-500/20 bg-yellow-500/5 p-2.5">
                    <div className="text-[11px] font-medium text-yellow-600 dark:text-yellow-400">
                      {t("settings.toolConflictTitle")}
                    </div>
                    <p className="text-[10px] leading-snug text-muted-foreground">
                      {t("settings.toolConflictHint")}
                    </p>
                    <ul className="space-y-1.5">
                      {conflicts.map((inst) => (
                        <li key={inst.path}>
                          <ToolInstallRow inst={inst} />
                        </li>
                      ))}
                    </ul>
                  </div>
                )}

                <div className="mt-auto flex items-center justify-end">
                  {isToolVersionLoading ? (
                    <span className="text-xs text-muted-foreground">
                      {t("common.loading")}
                    </span>
                  ) : installedButBroken ? (
                    // Installed but not runnable: reinstalling will not help, so
                    // show an environment hint instead of a button.
                    <span className="text-xs text-yellow-600 dark:text-yellow-400">
                      {t("settings.toolCheckEnv")}
                    </span>
                  ) : action ? (
                    <Button
                      size="sm"
                      variant={action === "install" ? "outline" : "default"}
                      className="h-7 gap-1.5 text-xs"
                      onClick={() => handleRunToolAction([toolName], action)}
                      disabled={isToolVersionLoading || isAnyBusy}
                    >
                      {runningAction ? (
                        <Loader2 className="h-3.5 w-3.5 animate-spin" />
                      ) : action === "install" ? (
                        <Download className="h-3.5 w-3.5" />
                      ) : (
                        <ArrowUpCircle className="h-3.5 w-3.5" />
                      )}
                      {/* Keep the label stable while loading and swap only the
                          icon to a spinner so the button width stays fixed and
                          does not jitter between states. */}
                      {action === "install"
                        ? t("settings.toolInstall")
                        : t("settings.toolUpdate")}
                    </Button>
                  ) : (
                    <span className="text-xs text-muted-foreground">
                      {t("settings.toolReady")}
                    </span>
                  )}
                </div>
              </motion.div>
            );
          })}
        </div>
      </div>

      <motion.div
        initial={{ opacity: 0, y: 10 }}
        animate={{ opacity: 1, y: 0 }}
        transition={{ duration: 0.3, delay: 0.3 }}
        className="space-y-3"
      >
        <button
          type="button"
          onClick={() => setShowInstallCommands((v) => !v)}
          aria-expanded={showInstallCommands}
          className="flex w-full items-center gap-1.5 px-1 text-sm font-medium text-foreground transition-colors hover:text-primary"
        >
          <ChevronDown
            className={`h-3.5 w-3.5 transition-transform ${
              showInstallCommands ? "" : "-rotate-90"
            }`}
          />
          {t("settings.manualInstallCommands")}
        </button>
        {showInstallCommands && (
          <div className="rounded-xl border border-border bg-gradient-to-br from-card/80 to-card/40 p-4 space-y-3 shadow-sm">
            <div className="flex items-center justify-between gap-2">
              <p className="text-xs text-muted-foreground">
                {t("settings.oneClickInstallHint")}
              </p>
              <Button
                size="sm"
                variant="outline"
                onClick={handleCopyInstallCommands}
                className="h-7 gap-1.5 text-xs"
              >
                <Copy className="h-3.5 w-3.5" />
                {t("common.copy")}
              </Button>
            </div>
            <pre className="text-xs font-mono bg-background/80 px-3 py-2.5 rounded-lg border border-border/60 overflow-x-auto">
              {ONE_CLICK_INSTALL_COMMANDS}
            </pre>
          </div>
        )}
      </motion.div>

      <ToolUpgradeConfirmDialog
        isOpen={pendingUpgrade !== null}
        plans={pendingUpgrade?.plans ?? []}
        displayName={toolDisplayName}
        onConfirm={handleConfirmUpgrade}
        onCancel={handleCancelUpgrade}
      />
    </motion.section>
  );
}
