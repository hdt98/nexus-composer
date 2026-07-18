import type { LocalProxyRequestOverrides } from "../types";

export const NEXUS_ORIGIN =
  "https://my-tenant-2-glm52-sonle-tp4.onenexus-do.cloud";
export const NEXUS_ENDPOINT = `${NEXUS_ORIGIN}/v1`;
export const NEXUS_CLAUDE_BASE_URL = NEXUS_ORIGIN;
export const NEXUS_MODEL = "GLM-5.2-FP8";
export const NEXUS_CLAUDE_MODEL = `${NEXUS_MODEL}[1m]`;
export const isNexusModel = (model: string): boolean => {
  const baseModel = model.replace(/(?:\[1m\])+$/, "");
  return [NEXUS_MODEL, "glm-5.2", "GLM-5.2-SGLang"].some(
    (alias) => alias === baseModel,
  );
};
export const NEXUS_CONTEXT_WINDOW = 1_048_576;
export const NEXUS_AUTO_COMPACT_TOKENS = 252_000;
export const NEXUS_MAX_OUTPUT_TOKENS = 65_536;
export const NEXUS_MANAGED_PRESET_VERSION = 7;
export const NEXUS_CLAUDE_MANAGED_PRESET_VERSION = 8;
export const NEXUS_CLAUDE_DESKTOP_MANAGED_PRESET_VERSION = 8;
export const NEXUS_TEXT_MODEL_CATALOG = {
  models: [{ model: NEXUS_MODEL, inputModalities: ["text"] }],
};
export const NEXUS_REQUEST_OVERRIDES = {
  body: {
    max_tokens: NEXUS_MAX_OUTPUT_TOKENS,
    chat_template_kwargs: { enable_thinking: true, clear_thinking: false },
  },
} satisfies LocalProxyRequestOverrides;
