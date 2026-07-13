export const NEXUS_ENDPOINT = "http://127.0.0.1:30001/v1";
export const NEXUS_MODEL = "glm-5.2";
export const NEXUS_CLAUDE_MODEL = `${NEXUS_MODEL}[1m]`;
export const NEXUS_AUTH_TOKEN = "dummy";
export const NEXUS_CONTEXT_WINDOW = 1_048_576;
export const NEXUS_AUTO_COMPACT_TOKENS = 252_000;
export const NEXUS_CAPABILITIES = {
  reasoningBoundary: "think_close",
} as const;
