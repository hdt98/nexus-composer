/**
 * Codex 预设供应商配置模板
 */
import { ProviderCategory } from "../types";
import type {
  CodexApiFormat,
  CodexCatalogModel,
  CodexChatReasoning,
} from "../types";
import type { PresetTheme } from "./claudeProviderPresets";
import {
  NEXUS_AUTH_TOKEN,
  NEXUS_AUTO_COMPACT_TOKENS,
  NEXUS_CAPABILITIES,
  NEXUS_CONTEXT_WINDOW,
  NEXUS_ENDPOINT,
  NEXUS_MODEL,
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
  providerType?: "nexus";
  nexusCapabilities?: typeof NEXUS_CAPABILITIES;
}

export function codexPresetSettingsConfig(preset: CodexProviderPreset) {
  return {
    auth: preset.auth ?? {},
    config: preset.config ?? "",
    ...(preset.nexusCapabilities
      ? { nexusCapabilities: preset.nexusCapabilities }
      : {}),
  };
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
  contextWindow: number | null = null,
  autoCompactTokenLimit: number | null = null,
): string {
  const tomlString = (value: string) => JSON.stringify(value);
  const reasoningConfig = reasoningEffort
    ? `model_reasoning_effort = ${tomlString(reasoningEffort)}\n`
    : "";
  const contextConfig = contextWindow
    ? `model_context_window = ${contextWindow}\n`
    : "";
  const compactConfig = autoCompactTokenLimit
    ? `model_auto_compact_token_limit = ${autoCompactTokenLimit}\n`
    : "";

  return `model_provider = "custom"
model = ${tomlString(modelName)}
${reasoningConfig}${contextConfig}${compactConfig}disable_response_storage = true

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
        // Native Responses (direct) overrides for the generated
        // model-catalogs.json; omit to inherit the native template defaults
        // (supports_parallel_tool_calls=false, input_modalities=["text"]).
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
    auth: generateThirdPartyAuth(NEXUS_AUTH_TOKEN),
    config: generateThirdPartyConfig(
      "Nexus GLM-5.2",
      NEXUS_ENDPOINT,
      NEXUS_MODEL,
      null,
      NEXUS_CONTEXT_WINDOW,
      NEXUS_AUTO_COMPACT_TOKENS,
    ),
    endpointCandidates: [NEXUS_ENDPOINT],
    apiFormat: "openai_chat",
    providerType: "nexus",
    nexusCapabilities: NEXUS_CAPABILITIES,
    codexChatReasoning: {
      supportsThinking: true,
      supportsEffort: false,
      thinkingParam: "chat_template_kwargs.enable_thinking",
      effortParam: "none",
      outputFormat: "reasoning_content",
    },
    modelCatalog: modelCatalog([
      {
        model: NEXUS_MODEL,
        displayName: "GLM-5.2",
        contextWindow: NEXUS_CONTEXT_WINDOW,
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
