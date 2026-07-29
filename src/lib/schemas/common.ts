import { z } from "zod";
import { validateToml, tomlToMcpServer } from "@/utils/tomlUtils";

/**
 * Parse a JSON syntax error and return clearer location information.
 */
function parseJsonError(error: unknown): string {
  if (!(error instanceof SyntaxError)) {
    return "Invalid JSON";
  }

  const message = error.message || "Failed to parse JSON";

  // Chrome/V8: "Unexpected token ... in JSON at position 123"
  const positionMatch = message.match(/at position (\d+)/i);
  if (positionMatch) {
    const position = parseInt(positionMatch[1], 10);
    return `Invalid JSON (position: ${position})`;
  }

  // Firefox: "JSON.parse: unexpected character at line 1 column 23"
  const lineColumnMatch = message.match(/line (\d+) column (\d+)/i);
  if (lineColumnMatch) {
    const line = lineColumnMatch[1];
    const column = lineColumnMatch[2];
    return `Invalid JSON: line ${line}, column ${column}`;
  }

  return `Invalid JSON: ${message}`;
}

/**
 * Shared JSON configuration text validation:
 * - Must not be empty.
 * - Must parse as an object rather than an array.
 */
export const jsonConfigSchema = z
  .string()
  .min(1, "Configuration cannot be empty")
  .superRefine((value, ctx) => {
    try {
      const obj = JSON.parse(value);
      if (!obj || typeof obj !== "object" || Array.isArray(obj)) {
        ctx.addIssue({
          code: z.ZodIssueCode.custom,
          message: "Configuration must be a single object",
        });
      }
    } catch (e) {
      ctx.addIssue({
        code: z.ZodIssueCode.custom,
        message: parseJsonError(e),
      });
    }
  });

/**
 * Shared TOML configuration text validation:
 * - May be empty when the calling feature does not require a value.
 * - Must have valid syntax and structure.
 * - Reports required command or url fields for stdio, http, and sse servers.
 */
export const tomlConfigSchema = z.string().superRefine((value, ctx) => {
  const err = validateToml(value);
  if (err) {
    ctx.addIssue({
      code: z.ZodIssueCode.custom,
      message: `Invalid TOML: ${err}`,
    });
    return;
  }

  if (!value.trim()) return;

  try {
    const server = tomlToMcpServer(value);
    if (server.type === "stdio" && !server.command?.trim()) {
      ctx.addIssue({
        code: z.ZodIssueCode.custom,
        message: "stdio servers require command",
      });
    }
    if (
      (server.type === "http" || server.type === "sse") &&
      !server.url?.trim()
    ) {
      ctx.addIssue({
        code: z.ZodIssueCode.custom,
        message: `${server.type} servers require url`,
      });
    }
  } catch (e: any) {
    ctx.addIssue({
      code: z.ZodIssueCode.custom,
      message: e?.message || "Failed to parse TOML",
    });
  }
});
