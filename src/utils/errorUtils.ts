/**
 * Extract a message from supported error shapes.
 * @param error Error value.
 * @returns The extracted message.
 */
export const extractErrorMessage = (error: unknown): string => {
  if (!error) return "";
  if (typeof error === "string") {
    return error;
  }
  if (error instanceof Error && error.message.trim()) {
    return error.message;
  }

  if (typeof error === "object") {
    const errObject = error as Record<string, unknown>;

    const candidate = errObject.message ?? errObject.error ?? errObject.detail;
    if (typeof candidate === "string" && candidate.trim()) {
      return candidate;
    }

    const payload = errObject.payload;
    if (typeof payload === "string" && payload.trim()) {
      return payload;
    }
    if (payload && typeof payload === "object") {
      const payloadObj = payload as Record<string, unknown>;
      const payloadCandidate =
        payloadObj.message ?? payloadObj.error ?? payloadObj.detail;
      if (typeof payloadCandidate === "string" && payloadCandidate.trim()) {
        return payloadCandidate;
      }
    }
  }

  return "";
};

/**
 * Map known MCP backend errors to localized messages.
 * Substring matching covers equivalent messages in different contexts.
 * Return an empty string when no match exists so callers can use the original
 * detail or their default localized message.
 */
export const translateMcpBackendError = (
  message: string,
  t: (key: string, opts?: any) => string,
): string => {
  if (!message) return "";
  const msg = String(message).trim();

  // Basic field and structure validation.
  if (msg.includes("MCP server ID cannot be empty")) {
    return t("mcp.error.idRequired");
  }
  if (
    msg.includes("MCP server definition must be a JSON object") ||
    msg.includes("An MCP server entry must be a JSON object") ||
    msg.includes("An MCP server entry is missing the server field") ||
    msg.includes(
      "The server field of an MCP server entry must be a JSON object",
    ) ||
    msg.includes("An MCP server connection definition must be a JSON object") ||
    (msg.includes("MCP server '") && msg.includes("is not an object")) ||
    msg.includes("has a non-object `server` field") ||
    msg.includes("Server configuration must be an object") ||
    msg.includes("MCP server name must be a string") ||
    msg.includes("MCP server description must be a string") ||
    msg.includes("MCP server homepage must be a string") ||
    msg.includes("MCP server docs must be a string") ||
    msg.includes("MCP server tags must be an array of strings") ||
    msg.includes("MCP server enabled must be a boolean")
  ) {
    return t("mcp.error.jsonInvalid");
  }
  if (msg.includes("MCP server type must be")) {
    return t("mcp.error.jsonInvalid");
  }

  // Required fields.
  if (
    msg.includes("An stdio MCP server is missing the command field") ||
    msg.includes("stdio MCP server is missing the `command` field") ||
    msg.includes("stdio MCP servers must include the command field")
  ) {
    return t("mcp.error.commandRequired");
  }
  if (
    msg.includes("An HTTP MCP server is missing the url field") ||
    msg.includes("An SSE MCP server is missing the url field") ||
    msg.includes("http MCP server is missing the `url` field") ||
    msg.includes("sse MCP server is missing the `url` field") ||
    msg.includes("MCP servers must include the url field") ||
    msg === "URL cannot be empty"
  ) {
    return t("mcp.wizard.urlRequired");
  }

  // File parsing and serialization.
  if (
    msg.includes("Failed to parse ~/.claude.json") ||
    msg.includes("Failed to parse ~/.codex/config.toml") ||
    msg.includes("Failed to parse config.toml") ||
    msg.includes("Unrecognized TOML format") ||
    msg.includes("TOML content cannot be empty")
  ) {
    return t("mcp.error.tomlInvalid");
  }
  if (msg.includes("Failed to serialize config.toml")) {
    return t("mcp.error.tomlInvalid");
  }

  return "";
};
