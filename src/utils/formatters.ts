/**
 * Format a JSON string.
 * @param value - Raw JSON string.
 * @returns Formatted JSON using two-space indentation.
 * @throws If the JSON is invalid.
 */
export function formatJSON(value: string): string {
  const trimmed = value.trim();
  if (!trimmed) {
    return "";
  }
  const parsed = JSON.parse(trimmed);
  return JSON.stringify(parsed, null, 2);
}

/**
 * Parse MCP JSON configuration from either supported shape:
 * 1. A plain config object: { "command": "npx", "args": [...], ... }
 * 2. A named wrapper: "server-name": { "command": "npx", ... } or { "server-name": {...} }
 *
 * @param jsonText - JSON string.
 * @returns { id?: string, config: object, formattedConfig: string }
 * @throws If the JSON is invalid.
 */
export function parseSmartMcpJson(jsonText: string): {
  id?: string;
  config: any;
  formattedConfig: string;
} {
  let trimmed = jsonText.trim();
  if (!trimmed) {
    return { config: {}, formattedConfig: "" };
  }

  // Wrap a key-value fragment such as "key": {...} in a complete object.
  if (trimmed.startsWith('"') && !trimmed.startsWith("{")) {
    trimmed = `{${trimmed}}`;
  }

  const parsed = JSON.parse(trimmed);

  // Extract the name and config from a single-key object whose value is an object.
  const keys = Object.keys(parsed);
  if (
    keys.length === 1 &&
    parsed[keys[0]] &&
    typeof parsed[keys[0]] === "object" &&
    !Array.isArray(parsed[keys[0]])
  ) {
    const id = keys[0];
    const config = parsed[id];
    return {
      id,
      config,
      formattedConfig: JSON.stringify(config, null, 2),
    };
  }

  // Otherwise use the parsed value directly.
  return {
    config: parsed,
    formattedConfig: JSON.stringify(parsed, null, 2),
  };
}

/**
 * TOML formatting is disabled.
 *
 * smol-toml parse/stringify discards all comments and original formatting.
 * TOML is commonly used for configuration files, where comments are important
 * documentation, so losing them would significantly degrade the user experience.
 *
 * Possible future approaches:
 * - Use @ltd/j-toml, which preserves comments but adds a dependency and complex API.
 * - Implement a lightweight formatter that changes only indentation and whitespace.
 * - Use toml-eslint-parser with a custom generator.
 *
 * For now, rely on existing TOML syntax validation in useCodexTomlValidation
 * and do not provide formatting.
 */
