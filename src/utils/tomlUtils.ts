import { parse as parseToml, stringify as stringifyToml } from "smol-toml";
import { normalizeTomlText } from "@/utils/textNormalization";
import { McpServerSpec } from "../types";

/**
 * Validate TOML syntax and convert it to a JSON-compatible object.
 * @param text TOML text.
 * @returns An error message, or an empty string on success.
 */
export const validateToml = (text: string): string => {
  if (!text.trim()) return "";
  try {
    const normalized = normalizeTomlText(text);
    const parsed = parseToml(normalized);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      return "mustBeObject";
    }
    return "";
  } catch (e: any) {
    // Return the underlying error so the caller can localize it.
    return e?.message || "parseError";
  }
};

/**
 * Convert an McpServerSpec object to TOML.
 * The serializer handles escaping and nested tables while preserving every
 * field, including extension fields such as timeout_ms.
 */
export const mcpServerToToml = (server: McpServerSpec): string => {
  // Copy every field, including extension fields.
  const obj: any = { ...server };

  // Remove undefined values for cleaner output.
  for (const k of Object.keys(obj)) {
    if (obj[k] === undefined) delete obj[k];
  }

  // The serializer adds a trailing newline; trim it for text-area display.
  return stringifyToml(obj).trim();
};

/**
 * Convert TOML text to a single McpServerSpec.
 * Supported formats:
 * 1. A direct server configuration with fields such as type, command, or args.
 * 2. [mcp_servers.<id>] (recommended), using the first server.
 * 3. [mcp.servers.<id>] (invalid but tolerated), also using the first server.
 * @param tomlText TOML text.
 * @returns An MCP server specification.
 * @throws When parsing or conversion fails.
 */
export const tomlToMcpServer = (tomlText: string): McpServerSpec => {
  if (!tomlText.trim()) {
    throw new Error("TOML content cannot be empty");
  }

  const parsed = parseToml(normalizeTomlText(tomlText));

  // Case 1: a direct server configuration with type, command, url, or related fields.
  if (
    parsed.type ||
    parsed.command ||
    parsed.url ||
    parsed.args ||
    parsed.env
  ) {
    return normalizeServerConfig(parsed);
  }

  // Case 2: recommended [mcp_servers.<id>] format.
  if (parsed.mcp_servers && typeof parsed.mcp_servers === "object") {
    const serverIds = Object.keys(parsed.mcp_servers);
    if (serverIds.length > 0) {
      const firstServer = (parsed.mcp_servers as any)[serverIds[0]];
      return normalizeServerConfig(firstServer);
    }
  }

  // Case 3: tolerate the invalid [mcp.servers.<id>] format.
  if (parsed.mcp && typeof parsed.mcp === "object") {
    const mcpObj = parsed.mcp as any;
    if (mcpObj.servers && typeof mcpObj.servers === "object") {
      const serverIds = Object.keys(mcpObj.servers);
      if (serverIds.length > 0) {
        const firstServer = mcpObj.servers[serverIds[0]];
        return normalizeServerConfig(firstServer);
      }
    }
  }

  throw new Error(
    "Unrecognized TOML format. Provide a single MCP server configuration or use [mcp_servers.<id>]",
  );
};

/**
 * Normalize a server configuration as McpServerSpec while preserving every
 * field, including extension fields such as timeout_ms.
 */
function normalizeServerConfig(config: any): McpServerSpec {
  if (!config || typeof config !== "object") {
    throw new Error("Server configuration must be an object");
  }

  const type = (config.type as string) || "stdio";

  // Track known fields so extension fields can be preserved later.
  const knownFields = new Set<string>();

  if (type === "stdio") {
    if (!config.command || typeof config.command !== "string") {
      throw new Error("stdio MCP servers must include the command field");
    }

    const server: McpServerSpec = {
      type: "stdio",
      command: config.command,
    };
    knownFields.add("type");
    knownFields.add("command");

    // Optional fields.
    if (config.args && Array.isArray(config.args)) {
      server.args = config.args.map((arg: any) => String(arg));
      knownFields.add("args");
    }
    if (config.env && typeof config.env === "object") {
      const env: Record<string, string> = {};
      for (const [k, v] of Object.entries(config.env)) {
        env[k] = String(v);
      }
      server.env = env;
      knownFields.add("env");
    }
    if (config.cwd && typeof config.cwd === "string") {
      server.cwd = config.cwd;
      knownFields.add("cwd");
    }

    // Preserve unknown extension fields such as timeout_ms.
    for (const key of Object.keys(config)) {
      if (!knownFields.has(key)) {
        server[key] = config[key];
      }
    }

    return server;
  } else if (type === "http" || type === "sse") {
    if (!config.url || typeof config.url !== "string") {
      throw new Error(`${type} MCP servers must include the url field`);
    }

    const server: McpServerSpec = {
      type: type as "http" | "sse",
      url: config.url,
    };
    knownFields.add("type");
    knownFields.add("url");

    // Optional fields.
    if (config.headers && typeof config.headers === "object") {
      const headers: Record<string, string> = {};
      for (const [k, v] of Object.entries(config.headers)) {
        headers[k] = String(v);
      }
      server.headers = headers;
      knownFields.add("headers");
    }

    // Preserve all unknown fields.
    for (const key of Object.keys(config)) {
      if (!knownFields.has(key)) {
        server[key] = config[key];
      }
    }

    return server;
  } else {
    throw new Error(`Unsupported MCP server type: ${type}`);
  }
}

/**
 * Try to derive a suitable server ID or title from TOML.
 * @param tomlText TOML text.
 * @returns A suggested ID, or an empty string on failure.
 */
export const extractIdFromToml = (tomlText: string): string => {
  try {
    const parsed = parseToml(normalizeTomlText(tomlText));

    // Try to extract an ID from [mcp_servers.<id>] or [mcp.servers.<id>].
    if (parsed.mcp_servers && typeof parsed.mcp_servers === "object") {
      const serverIds = Object.keys(parsed.mcp_servers);
      if (serverIds.length > 0) {
        return serverIds[0];
      }
    }

    if (parsed.mcp && typeof parsed.mcp === "object") {
      const mcpObj = parsed.mcp as any;
      if (mcpObj.servers && typeof mcpObj.servers === "object") {
        const serverIds = Object.keys(mcpObj.servers);
        if (serverIds.length > 0) {
          return serverIds[0];
        }
      }
    }

    // Fall back to deriving the ID from command.
    if (parsed.command && typeof parsed.command === "string") {
      const cmd = parsed.command.split(/[\\/]/).pop() || "";
      return cmd.replace(/\.(exe|bat|sh|js|py)$/i, "");
    }
  } catch {
    // Return an empty string when parsing fails.
  }

  return "";
};
