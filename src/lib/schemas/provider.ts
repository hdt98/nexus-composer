import { z } from "zod";

/**
 * Parses JSON syntax errors and extracts location details.
 */
function parseJsonError(error: unknown): string {
  if (!(error instanceof SyntaxError)) {
    return "Invalid configuration JSON";
  }

  const message = error.message;

  // Extract a Chrome/V8 position from messages such as
  // "Unexpected token ... in JSON at position 123".
  const positionMatch = message.match(/at position (\d+)/i);
  if (positionMatch) {
    const position = parseInt(positionMatch[1], 10);
    return `Invalid JSON: ${message.split(" in JSON")[0]} (position ${position})`;
  }

  // Firefox: "JSON.parse: unexpected character at line 1 column 23"
  const lineColumnMatch = message.match(/line (\d+) column (\d+)/i);
  if (lineColumnMatch) {
    const line = lineColumnMatch[1];
    const column = lineColumnMatch[2];
    return `Invalid JSON: line ${line}, column ${column}`;
  }

  // Extract the relevant details for other error formats.
  const cleanMessage = message
    .replace(/^JSON\.parse:\s*/i, "")
    .replace(/^Unexpected\s+/i, "Unexpected ")
    .replace(/token/gi, "token")
    .replace(/Expected/gi, "Expected");

  return `Invalid JSON: ${cleanMessage}`;
}

export const providerSchema = z.object({
  name: z.string(), // Required-field validation is handled by a toast in handleSubmit.
  websiteUrl: z.string().url("Enter a valid URL").optional().or(z.literal("")),
  notes: z.string().optional(),
  settingsConfig: z
    .string()
    .min(1, "Configuration is required")
    .superRefine((value, ctx) => {
      try {
        JSON.parse(value);
      } catch (error) {
        ctx.addIssue({
          code: z.ZodIssueCode.custom,
          message: parseJsonError(error),
        });
      }
    }),
  // Icon configuration.
  icon: z.string().optional(),
  iconColor: z.string().optional(),
});

export type ProviderFormData = z.infer<typeof providerSchema>;
