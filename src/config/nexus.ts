import type { LocalProxyRequestOverrides } from "../types";

export const NEXUS_ENDPOINT =
  "https://my-tenant-2-glm52-sonle-tp4.onenexus-do.cloud/v1";
export const NEXUS_MODEL = "GLM-5.2-FP8";
export const NEXUS_CLAUDE_MODEL = `${NEXUS_MODEL}[1m]`;
export const NEXUS_CONTEXT_WINDOW = 1_048_576;
export const NEXUS_AUTO_COMPACT_TOKENS = 252_000;
export const NEXUS_MAX_OUTPUT_TOKENS = 65_536;
export const NEXUS_MANAGED_PRESET_VERSION = 2;
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

const MANAGED_NEXUS_MODELS = new Set([
  NEXUS_MODEL,
  NEXUS_CLAUDE_MODEL,
  "glm-5.2",
  "glm-5.2[1m]",
]);

export const isManagedNexusEndpoint = (value: unknown): boolean =>
  typeof value === "string" &&
  value.trim().replace(/\/+$/, "") === NEXUS_ENDPOINT;

export const removeManagedNexusCatalog = (
  settings: Record<string, unknown>,
): void => {
  const catalog = settings.modelCatalog;
  if (!catalog || typeof catalog !== "object" || Array.isArray(catalog)) return;
  const catalogRecord = catalog as Record<string, unknown>;
  const models = catalogRecord.models;
  if (!Array.isArray(models)) return;

  const remaining = models.filter((entry) => {
    if (!entry || typeof entry !== "object" || Array.isArray(entry)) {
      return true;
    }
    const model = entry as Record<string, unknown>;
    return (
      Object.prototype.hasOwnProperty.call(model, "role") ||
      !MANAGED_NEXUS_MODELS.has(model.model as string)
    );
  });
  if (remaining.length === 0 && Object.keys(catalogRecord).length === 1) {
    delete settings.modelCatalog;
  } else {
    catalogRecord.models = remaining;
  }
};

export const removeManagedNexusReasoningOverride = (meta: {
  localProxyRequestOverrides?: unknown;
}): void => {
  const overrides = meta.localProxyRequestOverrides;
  if (!overrides || typeof overrides !== "object" || Array.isArray(overrides)) {
    return;
  }
  const overrideRecord = overrides as Record<string, unknown>;
  const body = overrideRecord.body;
  if (!body || typeof body !== "object" || Array.isArray(body)) return;
  const bodyRecord = body as Record<string, unknown>;
  if (bodyRecord.max_tokens === NEXUS_MAX_OUTPUT_TOKENS) {
    delete bodyRecord.max_tokens;
  }
  const template = bodyRecord.chat_template_kwargs;
  if (!template || typeof template !== "object" || Array.isArray(template)) {
    if (Object.keys(bodyRecord).length === 0) delete overrideRecord.body;
    if (Object.keys(overrideRecord).length === 0) {
      delete meta.localProxyRequestOverrides;
    }
    return;
  }
  const templateRecord = template as Record<string, unknown>;
  if (templateRecord.enable_thinking !== true) return;

  delete templateRecord.enable_thinking;
  if (Object.keys(templateRecord).length === 0) {
    delete bodyRecord.chat_template_kwargs;
  }
  if (Object.keys(bodyRecord).length === 0) delete overrideRecord.body;
  if (Object.keys(overrideRecord).length === 0) {
    delete meta.localProxyRequestOverrides;
  }
};
