import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { mcpApi } from "@/lib/api/mcp";
import type { McpServer } from "@/types";
import type { AppId } from "@/lib/api/types";

/**
 * Queries all MCP servers through unified management.
 */
export function useAllMcpServers() {
  return useQuery({
    queryKey: ["mcp", "all"],
    queryFn: () => mcpApi.getAllServers(),
  });
}

/**
 * Adds or updates an MCP server.
 */
export function useUpsertMcpServer() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (server: McpServer) => mcpApi.upsertUnifiedServer(server),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["mcp", "all"] });
    },
  });
}

/**
 * Toggles an MCP server for a specific application.
 */
export function useToggleMcpApp() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({
      serverId,
      app,
      enabled,
    }: {
      serverId: string;
      app: AppId;
      enabled: boolean;
    }) => mcpApi.toggleApp(serverId, app, enabled),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["mcp", "all"] });
    },
  });
}

/**
 * Deletes an MCP server.
 */
export function useDeleteMcpServer() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => mcpApi.deleteUnifiedServer(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["mcp", "all"] });
    },
  });
}

/**
 * Imports MCP servers from all applications.
 */
export function useImportMcpFromApps() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => mcpApi.importFromApps(),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["mcp", "all"] });
    },
  });
}
