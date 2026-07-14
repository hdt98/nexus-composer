import type { LocalProxyRequestOverrides } from "../types";

export const NEXUS_ENDPOINT =
  "https://my-tenant-2-glm52-sonle-tp4.onenexus-do.cloud/v1";
export const NEXUS_MODEL = "GLM-5.2-FP8";
export const NEXUS_CLAUDE_MODEL = `${NEXUS_MODEL}[1m]`;
export const NEXUS_CONTEXT_WINDOW = 1_048_576;
export const NEXUS_AUTO_COMPACT_TOKENS = 252_000;
export const NEXUS_MANAGED_PRESET_VERSION = 1;
export const NEXUS_TEXT_MODEL_CATALOG = {
  models: [
    { model: NEXUS_MODEL, inputModalities: ["text"] },
    { model: "glm-5.2", inputModalities: ["text"] },
  ],
};
export const NEXUS_REASONING_REQUEST_OVERRIDES = {
  body: { chat_template_kwargs: { enable_thinking: true } },
} satisfies LocalProxyRequestOverrides;
