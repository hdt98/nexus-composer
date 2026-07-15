import type { LocalProxyRequestOverrides } from "../types";

export const NEXUS_ENDPOINT =
  "https://my-tenant-2-glm52-sonle-tp4.onenexus-do.cloud/v1";
export const NEXUS_MODEL = "GLM-5.2-FP8";
export const NEXUS_CLAUDE_MODEL = `${NEXUS_MODEL}[1m]`;
export const NEXUS_CONTEXT_WINDOW = 1_048_576;
export const NEXUS_AUTO_COMPACT_TOKENS = 252_000;
export const NEXUS_MAX_OUTPUT_TOKENS = 65_536;
export const NEXUS_CODEX_STREAM_IDLE_TIMEOUT_MS = 900_000;
export const NEXUS_MANAGED_PRESET_VERSION = 3;
export const NEXUS_TEXT_MODEL_CATALOG = {
  models: [
    { model: NEXUS_MODEL, inputModalities: ["text"] },
    { model: "glm-5.2", inputModalities: ["text"] },
  ],
};
export const NEXUS_REASONING_REQUEST_OVERRIDES = {
  body: {
    max_tokens: NEXUS_MAX_OUTPUT_TOKENS,
    chat_template_kwargs: { enable_thinking: true },
  },
} satisfies LocalProxyRequestOverrides;
