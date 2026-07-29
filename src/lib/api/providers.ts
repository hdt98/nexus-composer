import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  Provider,
  UniversalProvider,
  UniversalProvidersMap,
} from "@/types";
import type { AppId } from "./types";

export interface ProviderSortUpdate {
  id: string;
  sortIndex: number;
}

export interface ProviderSwitchEvent {
  appType: AppId;
  providerId: string;
}

export interface SwitchResult {
  warnings: string[];
}

export interface OpenTerminalOptions {
  cwd?: string;
}

export interface SshHostEntry {
  alias: string;
  hostName?: string;
  user?: string;
  port?: string;
  source?: string;
}

export type SshConnectionTarget =
  | {
      type: "config";
      alias: string;
    }
  | {
      type: "manual";
      host: string;
      user?: string;
      port?: number;
      password?: string;
    };

export interface RemoteApplyResult {
  hostAlias: string;
  app: AppId;
  providerId: string;
  writtenFiles: string[];
  overwroteExistingConfig: boolean;
  warnings: string[];
}

export interface RemoteConfigFile {
  path: string;
  exists: boolean;
  bytes: number;
}

export interface RemoteProviderState {
  hostAlias: string;
  app: AppId;
  provider?: Provider | null;
  matchedProviderId?: string | null;
  files: RemoteConfigFile[];
  hasExistingConfig: boolean;
  hasUnmanagedConfig: boolean;
  overwriteWarning?: string | null;
  warnings: string[];
}

export interface RemoteImportResult {
  hostAlias: string;
  app: AppId;
  provider: Provider;
}

export interface RemoteUsageSyncResult {
  hostAlias: string;
  app: AppId;
  imported: number;
  skipped: number;
  unchangedFiles: number;
  filesScanned: number;
  errors: string[];
  warnings: string[];
}

export interface RemoteAgentStatus {
  hostAlias: string;
  installed: boolean;
  running: boolean;
  nodeVersion?: string | null;
  pid?: number | null;
  status?: any | null;
  error?: string | null;
}

export interface RemoteCodexReloadResult {
  attempted: boolean;
  stoppedProcesses: number;
  reconnectTriggered: boolean;
  error?: string | null;
}

export interface RemoteAgentDeployResult {
  hostAlias: string;
  installed: boolean;
  running: boolean;
  writtenFiles: string[];
  status?: any | null;
  historyPreparation: RemoteAgentHistoryMaintenanceResult;
  codexReload: RemoteCodexReloadResult;
}

export interface RemoteAgentSwitchResult {
  hostAlias: string;
  providerId: string;
  response: any;
  historyPreparation: RemoteAgentHistoryMaintenanceResult;
  codexReload: RemoteCodexReloadResult;
}

export interface RemoteAgentHistoryMaintenanceResult {
  ok: boolean;
  sourceProvider?: string;
  targetProvider?: string;
  targetModel?: string;
  jsonlFiles?: number;
  stateRows?: number;
  backupDir?: string;
  skipped?: string;
}

export interface RemoteAgentStopResult {
  ok: boolean;
  restored_backup?: boolean;
  history_maintenance?: RemoteAgentHistoryMaintenanceResult;
  codex_reload?: RemoteCodexReloadResult;
}

export type RemoteUsageSyncStage =
  | "starting"
  | "connecting"
  | "listed"
  | "analyzing"
  | "analyzed"
  | "importing"
  | "completed"
  | "failed";

export interface RemoteUsageSyncProgressEvent {
  syncId: string;
  hostAlias: string;
  app: AppId;
  stage: RemoteUsageSyncStage;
  currentStep: number;
  totalSteps: number;
  fileCount?: number;
  payloadBytes?: number;
  filesScanned?: number;
  imported?: number;
  skipped?: number;
  unchangedFiles?: number;
  error?: string;
}

export interface ClaudeDesktopStatus {
  supported: boolean;
  configured: boolean;
  appliedId?: string | null;
  profilePath?: string | null;
  configLibraryPath?: string | null;
  mode?: "direct" | "proxy" | null;
  expectedBaseUrl?: string | null;
  actualBaseUrl?: string | null;
  proxyRunning: boolean;
  staleRawModels: boolean;
  missingRouteMappings: boolean;
  gatewayTokenConfigured: boolean;
}

export interface ClaudeDesktopDefaultRoute {
  routeId: string;
  envKey: string;
  supports1m: boolean;
}

export const providersApi = {
  async getAll(appId: AppId): Promise<Record<string, Provider>> {
    return await invoke("get_providers", { app: appId });
  },

  async getCurrent(appId: AppId): Promise<string> {
    return await invoke("get_current_provider", { app: appId });
  },

  async add(
    provider: Provider,
    appId: AppId,
    addToLive?: boolean,
  ): Promise<boolean> {
    return await invoke("add_provider", { provider, app: appId, addToLive });
  },

  async update(
    provider: Provider,
    appId: AppId,
    originalId?: string,
  ): Promise<boolean> {
    return await invoke("update_provider", {
      provider,
      app: appId,
      originalId,
    });
  },

  async delete(id: string, appId: AppId): Promise<boolean> {
    return await invoke("delete_provider", { id, app: appId });
  },

  /**
   * Remove provider from live config only (for additive mode apps like OpenCode)
   * Does NOT delete from database - provider remains in the list
   */
  async removeFromLiveConfig(id: string, appId: AppId): Promise<boolean> {
    return await invoke("remove_provider_from_live_config", { id, app: appId });
  },

  async switch(id: string, appId: AppId): Promise<SwitchResult> {
    return await invoke("switch_provider", { id, app: appId });
  },

  async getSshHosts(): Promise<SshHostEntry[]> {
    return await invoke("get_ssh_config_hosts");
  },

  async applyToRemote(
    id: string,
    appId: AppId,
    target: SshConnectionTarget,
    forceOverwrite = false,
  ): Promise<RemoteApplyResult> {
    return await invoke("apply_provider_to_remote", {
      id,
      app: appId,
      target,
      forceOverwrite,
    });
  },

  async inspectRemote(
    appId: AppId,
    target: SshConnectionTarget,
  ): Promise<RemoteProviderState> {
    return await invoke("inspect_remote_provider", { app: appId, target });
  },

  async importRemote(
    appId: AppId,
    target: SshConnectionTarget,
  ): Promise<RemoteImportResult> {
    return await invoke("import_remote_provider", { app: appId, target });
  },

  async syncRemoteUsage(
    appId: AppId,
    target: SshConnectionTarget,
    syncId?: string,
  ): Promise<RemoteUsageSyncResult> {
    return await invoke("sync_remote_session_usage", {
      app: appId,
      target,
      syncId,
    });
  },

  async getRemoteAgentStatus(
    target: SshConnectionTarget,
  ): Promise<RemoteAgentStatus> {
    return await invoke("get_remote_agent_status", { target });
  },

  async deployRemoteAgent(
    target: SshConnectionTarget,
    providerId: string,
  ): Promise<RemoteAgentDeployResult> {
    return await invoke("deploy_remote_agent", { target, providerId });
  },

  async switchRemoteAgentProvider(
    target: SshConnectionTarget,
    providerId: string,
  ): Promise<RemoteAgentSwitchResult> {
    return await invoke("switch_remote_agent_provider", {
      target,
      providerId,
    });
  },

  async stopRemoteAgent(
    target: SshConnectionTarget,
  ): Promise<RemoteAgentStopResult> {
    return await invoke("stop_remote_agent", { target });
  },

  async getRemoteAgentLogs(
    target: SshConnectionTarget,
    lines = 120,
  ): Promise<string> {
    return await invoke("get_remote_agent_logs", { target, lines });
  },

  async importDefault(appId: AppId): Promise<boolean> {
    return await invoke("import_default_config", { app: appId });
  },

  async importClaudeDesktopFromClaude(): Promise<number> {
    return await invoke("import_claude_desktop_providers_from_claude");
  },

  async ensureClaudeDesktopOfficialProvider(): Promise<boolean> {
    return await invoke("ensure_claude_desktop_official_provider");
  },

  async getClaudeDesktopStatus(): Promise<ClaudeDesktopStatus> {
    return await invoke("get_claude_desktop_status");
  },

  async getClaudeDesktopDefaultRoutes(): Promise<ClaudeDesktopDefaultRoute[]> {
    return await invoke("get_claude_desktop_default_routes");
  },

  async updateTrayMenu(): Promise<boolean> {
    return await invoke("update_tray_menu");
  },

  async updateSortOrder(
    updates: ProviderSortUpdate[],
    appId: AppId,
  ): Promise<boolean> {
    return await invoke("update_providers_sort_order", { updates, app: appId });
  },

  async onSwitched(
    handler: (event: ProviderSwitchEvent) => void,
  ): Promise<UnlistenFn> {
    return await listen("provider-switched", (event) => {
      const payload = event.payload as ProviderSwitchEvent;
      handler(payload);
    });
  },

  async onRemoteUsageSyncProgress(
    handler: (event: RemoteUsageSyncProgressEvent) => void,
  ): Promise<UnlistenFn> {
    return await listen("remote-usage-sync-progress", (event) => {
      const payload = event.payload as RemoteUsageSyncProgressEvent;
      handler(payload);
    });
  },

  /**
   * Open a terminal for a specific provider.
   * Any provider can open a terminal, whether or not it is currently active.
   * The terminal uses that provider's API configuration without affecting global settings.
   */
  async openTerminal(
    providerId: string,
    appId: AppId,
    options?: OpenTerminalOptions,
  ): Promise<boolean> {
    const { cwd } = options ?? {};
    return await invoke("open_provider_terminal", {
      providerId,
      app: appId,
      cwd,
    });
  },

  /**
   * Import providers from the OpenCode live config into the database.
   * OpenCode-specific: additive mode allows users to configure providers directly in opencode.json.
   */
  async importOpenCodeFromLive(): Promise<number> {
    return await invoke("import_opencode_providers_from_live");
  },

  /**
   * Get provider IDs from the OpenCode live config.
   * Used by the frontend to determine whether a provider is already in opencode.json.
   */
  async getOpenCodeLiveProviderIds(): Promise<string[]> {
    return await invoke("get_opencode_live_provider_ids");
  },

  /**
   * Get provider IDs from the OpenClaw live config.
   * Used by the frontend to determine whether a provider is already in openclaw.json.
   */
  async getOpenClawLiveProviderIds(): Promise<string[]> {
    return await invoke("get_openclaw_live_provider_ids");
  },

  /**
   * Get provider IDs from the Hermes live config.
   * Used by the frontend to determine whether a provider is already in the Hermes config.
   */
  async getHermesLiveProviderIds(): Promise<string[]> {
    return await invoke("get_hermes_live_provider_ids");
  },

  /**
   * Import providers from the OpenClaw live config into the database.
   * OpenClaw-specific: additive mode allows users to configure providers directly in openclaw.json.
   */
  async importOpenClawFromLive(): Promise<number> {
    return await invoke("import_openclaw_providers_from_live");
  },

  /**
   * Import providers from the Hermes live config into the database.
   * Hermes-specific: additive mode allows users to configure providers directly in the Hermes config.
   */
  async importHermesFromLive(): Promise<number> {
    return await invoke("import_hermes_providers_from_live");
  },
};

// ============================================================================
// Universal Provider API
// ============================================================================

export const universalProvidersApi = {
  /**
   * Get all universal providers.
   */
  async getAll(): Promise<UniversalProvidersMap> {
    return await invoke("get_universal_providers");
  },

  /**
   * Get one universal provider.
   */
  async get(id: string): Promise<UniversalProvider | null> {
    return await invoke("get_universal_provider", { id });
  },

  /**
   * Add or update a universal provider.
   */
  async upsert(provider: UniversalProvider): Promise<boolean> {
    return await invoke("upsert_universal_provider", { provider });
  },

  /**
   * Delete a universal provider.
   */
  async delete(id: string): Promise<boolean> {
    return await invoke("delete_universal_provider", { id });
  },

  /**
   * Manually synchronize a universal provider to each app.
   */
  async sync(id: string): Promise<boolean> {
    return await invoke("sync_universal_provider", { id });
  },
};
