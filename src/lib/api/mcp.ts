import { invoke } from "@tauri-apps/api/core";
import type {
  McpConfigResponse,
  McpServer,
  McpServerSpec,
  McpServersMap,
  McpStatus,
} from "@/types";
import type { AppId } from "./types";

export const mcpApi = {
  async getStatus(): Promise<McpStatus> {
    return await invoke("get_claude_mcp_status");
  },

  async readConfig(): Promise<string | null> {
    return await invoke("read_claude_mcp_config");
  },

  async upsertServer(
    id: string,
    spec: McpServerSpec | Record<string, any>,
  ): Promise<boolean> {
    return await invoke("upsert_claude_mcp_server", { id, spec });
  },

  async deleteServer(id: string): Promise<boolean> {
    return await invoke("delete_claude_mcp_server", { id });
  },

  async validateCommand(cmd: string): Promise<boolean> {
    return await invoke("validate_mcp_command", { cmd });
  },

  /**
   * @deprecated Use getAllServers() instead (v3.7.0+).
   */
  async getConfig(app: AppId = "claude"): Promise<McpConfigResponse> {
    return await invoke("get_mcp_config", { app });
  },

  /**
   * @deprecated Use upsertUnifiedServer() instead (v3.7.0+).
   */
  async upsertServerInConfig(
    app: AppId,
    id: string,
    spec: McpServer,
    options?: { syncOtherSide?: boolean },
  ): Promise<boolean> {
    const payload = {
      app,
      id,
      spec,
      ...(options?.syncOtherSide !== undefined
        ? { syncOtherSide: options.syncOtherSide }
        : {}),
    };
    return await invoke("upsert_mcp_server_in_config", payload);
  },

  /**
   * @deprecated Use deleteUnifiedServer() instead (v3.7.0+).
   */
  async deleteServerInConfig(
    app: AppId,
    id: string,
    options?: { syncOtherSide?: boolean },
  ): Promise<boolean> {
    const payload = {
      app,
      id,
      ...(options?.syncOtherSide !== undefined
        ? { syncOtherSide: options.syncOtherSide }
        : {}),
    };
    return await invoke("delete_mcp_server_in_config", payload);
  },

  /**
   * @deprecated Use toggleApp() instead (v3.7.0+).
   */
  async setEnabled(app: AppId, id: string, enabled: boolean): Promise<boolean> {
    return await invoke("set_mcp_enabled", { app, id, enabled });
  },

  // ========================================================================
  // Unified MCP management API added in v3.7.0.
  // ========================================================================

  /**
   * Gets all MCP servers in the unified structure.
   */
  async getAllServers(): Promise<McpServersMap> {
    return await invoke("get_mcp_servers");
  },

  /**
   * Adds or updates an MCP server in the unified structure.
   */
  async upsertUnifiedServer(server: McpServer): Promise<void> {
    return await invoke("upsert_mcp_server", { server });
  },

  /**
   * Deletes an MCP server.
   */
  async deleteUnifiedServer(id: string): Promise<boolean> {
    return await invoke("delete_mcp_server", { id });
  },

  /**
   * Toggles an MCP server for a specified application.
   */
  async toggleApp(
    serverId: string,
    app: AppId,
    enabled: boolean,
  ): Promise<void> {
    return await invoke("toggle_mcp_app", { serverId, app, enabled });
  },

  /**
   * Imports MCP servers from all applications.
   */
  async importFromApps(): Promise<number> {
    return await invoke("import_mcp_from_apps");
  },
};
