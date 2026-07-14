/**
 * Codex 预设供应商配置模板
 */
import { ProviderCategory } from "../types";
import type {
  CodexApiFormat,
  CodexCatalogModel,
  CodexChatReasoning,
  LocalProxyRequestOverrides,
} from "../types";
import type { PresetTheme } from "./claudeProviderPresets";
import {
  NEXUS_AUTO_COMPACT_TOKENS,
  NEXUS_CODEX_STREAM_IDLE_TIMEOUT_MS,
  NEXUS_CONTEXT_WINDOW,
  NEXUS_ENDPOINT,
  NEXUS_MANAGED_PRESET_VERSION,
  NEXUS_MODEL,
  NEXUS_REASONING_REQUEST_OVERRIDES,
} from "./nexus";

export interface CodexProviderPreset {
  name: string;
  nameKey?: string; // i18n key for localized display name
  websiteUrl: string;
  // 第三方供应商可提供单独的获取 API Key 链接
  apiKeyUrl?: string;
  auth: Record<string, any>; // 将写入 ~/.codex/auth.json
  config: string; // 将写入 ~/.codex/config.toml（TOML 字符串）
  isOfficial?: boolean; // 标识是否为官方预设
  isPartner?: boolean; // 标识是否为商业合作伙伴
  primePartner?: boolean; // 置顶合作伙伴（顶级）：徽章显示为心形
  partnerPromotionKey?: string; // 合作伙伴促销信息的 i18n key
  category?: ProviderCategory; // 新增：分类
  isCustomTemplate?: boolean; // 标识是否为自定义模板
  // 新增：请求地址候选列表（用于地址管理/测速）
  endpointCandidates?: string[];
  // 新增：视觉主题配置
  theme?: PresetTheme;
  // 图标配置
  icon?: string; // 图标名称
  iconColor?: string; // 图标颜色
  // Codex API 格式
  apiFormat?: CodexApiFormat;
  // Codex Chat 本地路由模式下的模型目录
  modelCatalog?: CodexCatalogModel[];
  // Codex Responses -> Chat Completions reasoning capability defaults
  codexChatReasoning?: CodexChatReasoning;
  localProxyRequestOverrides?: LocalProxyRequestOverrides;
  providerType?: "nexus";
  managedNexusPresetVersion?: number;
}

/**
 * 生成第三方供应商的 auth.json
 */
export function generateThirdPartyAuth(apiKey: string): Record<string, any> {
  return {
    OPENAI_API_KEY: apiKey || "",
  };
}

/**
 * 生成第三方供应商的 config.toml
 */
export function generateThirdPartyConfig(
  providerName: string,
  baseUrl: string,
  modelName = "gpt-5.5",
  reasoningEffort: "high" | null = "high",
  contextWindow?: number,
  autoCompactTokenLimit?: number,
): string {
  const tomlString = (value: string) => JSON.stringify(value);
  const reasoning = reasoningEffort
    ? `model_reasoning_effort = ${tomlString(reasoningEffort)}\n`
    : "";
  const context = contextWindow
    ? `model_context_window = ${contextWindow}\n`
    : "";
  const compact = autoCompactTokenLimit
    ? `model_auto_compact_token_limit = ${autoCompactTokenLimit}\n`
    : "";

  return `model_provider = "custom"
model = ${tomlString(modelName)}
${reasoning}${context}${compact}disable_response_storage = true

[model_providers.custom]
name = ${tomlString(providerName)}
base_url = ${tomlString(baseUrl)}
wire_api = "responses"
requires_openai_auth = true`;
}

function modelCatalog(
  models: Array<
    | string
    | {
        model: string;
        displayName?: string;
        contextWindow?: number;
        // Overrides for generated model-catalogs.json entries. Native Responses
        // defaults to text-only and non-parallel tools when these are omitted.
        supportsParallelToolCalls?: boolean;
        inputModalities?: string[];
        // Vendor's OFFICIAL base_instructions; omit to inherit the neutral
        // template default. Required by Codex, so the backend always emits one.
        baseInstructions?: string;
      }
  >,
): CodexCatalogModel[] {
  return models.map((entry) =>
    typeof entry === "string"
      ? { model: entry }
      : {
          model: entry.model,
          displayName: entry.displayName,
          contextWindow: entry.contextWindow,
          supportsParallelToolCalls: entry.supportsParallelToolCalls,
          inputModalities: entry.inputModalities,
          baseInstructions: entry.baseInstructions,
        },
  );
}

export const codexProviderPresets: CodexProviderPreset[] = [
  // Nexus routes Codex Responses through the built-in Chat adapter.
  {
    name: "Nexus GLM-5.2",
    nameKey: "providerForm.presets.nexus",
    websiteUrl: NEXUS_ENDPOINT,
    auth: generateThirdPartyAuth(""),
    config: `${generateThirdPartyConfig(
      "Nexus GLM-5.2",
      NEXUS_ENDPOINT,
      NEXUS_MODEL,
      null,
      NEXUS_CONTEXT_WINDOW,
      NEXUS_AUTO_COMPACT_TOKENS,
    )}
stream_idle_timeout_ms = ${NEXUS_CODEX_STREAM_IDLE_TIMEOUT_MS}`,
    endpointCandidates: [NEXUS_ENDPOINT],
    apiFormat: "openai_chat",
    providerType: "nexus",
    managedNexusPresetVersion: NEXUS_MANAGED_PRESET_VERSION,
    localProxyRequestOverrides: NEXUS_REASONING_REQUEST_OVERRIDES,
    modelCatalog: modelCatalog([
      {
        model: NEXUS_MODEL,
        displayName: "GLM-5.2",
        contextWindow: NEXUS_CONTEXT_WINDOW,
        inputModalities: ["text"],
      },
    ]),
    category: "third_party",
    icon: "nexus",
    iconColor: "#6366F1",
  },
  {
    name: "OpenAI Official",
    websiteUrl: "https://chatgpt.com/codex",
    isOfficial: true,
    category: "official",
    auth: {},
    config: ``,
    theme: {
      icon: "codex",
      backgroundColor: "#1F2937", // gray-800
      textColor: "#FFFFFF",
    },
    icon: "openai",
    iconColor: "#00A67E",
  },
];
