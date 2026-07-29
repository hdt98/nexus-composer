import { McpServer, McpServerSpec } from "../types";
import { isWindows } from "@/lib/platform";

export type McpPreset = Omit<McpServer, "enabled" | "description">;

// Create cross-platform npx command configuration. Windows requires a cmd /c
// wrapper to execute npx.cmd, while macOS and Linux can invoke npx directly.
const createNpxCommand = (
  packageName: string,
  extraArgs: string[] = [],
): { command: string; args: string[] } => {
  if (isWindows()) {
    return {
      command: "cmd",
      args: ["/c", "npx", ...extraArgs, packageName],
    };
  } else {
    return {
      command: "npx",
      args: [...extraArgs, packageName],
    };
  }
};

// Simplified MCP presets:
// - Include only common, ready-to-use stdio examples.
// - Avoid category, template, and speed-test complexity; seed config.json with
//   these entries disabled by default.
// - Users can enable or edit them from the MCP panel.
// - The description field uses an i18n key resolved with t() when consumed.
export const mcpPresets: McpPreset[] = [
  {
    id: "fetch",
    name: "mcp-server-fetch",
    tags: ["stdio", "http", "web"],
    server: {
      type: "stdio",
      command: "uvx",
      args: ["mcp-server-fetch"],
    } as McpServerSpec,
    homepage: "https://github.com/modelcontextprotocol/servers",
    docs: "https://github.com/modelcontextprotocol/servers/tree/main/src/fetch",
  },
  {
    id: "time",
    name: "@modelcontextprotocol/server-time",
    tags: ["stdio", "time", "utility"],
    server: {
      type: "stdio",
      ...createNpxCommand("@modelcontextprotocol/server-time", ["-y"]),
    } as McpServerSpec,
    homepage: "https://github.com/modelcontextprotocol/servers",
    docs: "https://github.com/modelcontextprotocol/servers/tree/main/src/time",
  },
  {
    id: "memory",
    name: "@modelcontextprotocol/server-memory",
    tags: ["stdio", "memory", "graph"],
    server: {
      type: "stdio",
      ...createNpxCommand("@modelcontextprotocol/server-memory", ["-y"]),
    } as McpServerSpec,
    homepage: "https://github.com/modelcontextprotocol/servers",
    docs: "https://github.com/modelcontextprotocol/servers/tree/main/src/memory",
  },
  {
    id: "sequential-thinking",
    name: "@modelcontextprotocol/server-sequential-thinking",
    tags: ["stdio", "thinking", "reasoning"],
    server: {
      type: "stdio",
      ...createNpxCommand("@modelcontextprotocol/server-sequential-thinking", [
        "-y",
      ]),
    } as McpServerSpec,
    homepage: "https://github.com/modelcontextprotocol/servers",
    docs: "https://github.com/modelcontextprotocol/servers/tree/main/src/sequentialthinking",
  },
  {
    id: "context7",
    name: "@upstash/context7-mcp",
    tags: ["stdio", "docs", "search"],
    server: {
      type: "stdio",
      ...createNpxCommand("@upstash/context7-mcp", ["-y"]),
    } as McpServerSpec,
    homepage: "https://context7.com",
    docs: "https://github.com/upstash/context7/blob/master/README.md",
  },
];

// Resolve a preset with its localized description.
export const getMcpPresetWithDescription = (
  preset: McpPreset,
  t: (key: string) => string,
): McpServer => {
  const descriptionKey = `mcp.presets.${preset.id}.description`;
  return {
    ...preset,
    description: t(descriptionKey),
  } as McpServer;
};

export default mcpPresets;
