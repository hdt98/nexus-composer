/**
 * 预设供应商配置模板
 */
import { ProviderCategory, type LocalProxyRequestOverrides } from "../types";
import {
  NEXUS_AUTO_COMPACT_TOKENS,
  NEXUS_CLAUDE_MODEL,
  NEXUS_ENDPOINT,
  NEXUS_MANAGED_PRESET_VERSION,
  NEXUS_REQUEST_OVERRIDES,
  NEXUS_TEXT_MODEL_CATALOG,
} from "./nexus";

export interface TemplateValueConfig {
  label: string;
  placeholder: string;
  defaultValue?: string;
  editorValue: string;
}

/**
 * 预设供应商的视觉主题配置
 */
export interface PresetTheme {
  /** 图标类型：'claude' | 'codex' | 'gemini' | 'generic' */
  icon?: "claude" | "codex" | "gemini" | "generic";
  /** 背景色（选中状态），支持 Tailwind 类名或 hex 颜色 */
  backgroundColor?: string;
  /** 文字色（选中状态），支持 Tailwind 类名或 hex 颜色 */
  textColor?: string;
}

export interface ProviderPreset {
  name: string;
  nameKey?: string; // i18n key for localized display name
  websiteUrl: string;
  // 新增：第三方/聚合等可单独配置获取 API Key 的链接
  apiKeyUrl?: string;
  settingsConfig: object;
  isOfficial?: boolean; // 标识是否为官方预设
  isPartner?: boolean; // 标识是否为商业合作伙伴
  primePartner?: boolean; // 置顶合作伙伴（顶级）：徽章显示为心形
  partnerPromotionKey?: string; // 合作伙伴促销信息的 i18n key
  category?: ProviderCategory; // 新增：分类
  // 新增：指定该预设所使用的 API Key 字段名（默认 ANTHROPIC_AUTH_TOKEN）
  apiKeyField?: "ANTHROPIC_AUTH_TOKEN" | "ANTHROPIC_API_KEY";
  // 新增：模板变量定义，用于动态替换配置中的值
  templateValues?: Record<string, TemplateValueConfig>; // editorValue 存储编辑器中的实时输入值
  // 新增：请求地址候选列表（用于地址管理/测速）
  endpointCandidates?: string[];
  // 新增：视觉主题配置
  theme?: PresetTheme;
  // 图标配置
  icon?: string; // 图标名称
  iconColor?: string; // 图标颜色

  // Claude API 格式（仅 Claude 供应商使用）
  // - "anthropic" (默认): Anthropic Messages API 格式，直接透传
  // - "openai_chat": OpenAI Chat Completions 格式，需要格式转换
  // - "openai_responses": OpenAI Responses API 格式，需要格式转换
  // - "gemini_native": Gemini Native generateContent API 格式，需要格式转换
  apiFormat?:
    | "anthropic"
    | "openai_chat"
    | "openai_responses"
    | "gemini_native";

  // 供应商类型标识（用于特殊供应商检测）
  // - "github_copilot": GitHub Copilot 供应商（需要 OAuth 认证）
  // - "codex_oauth": OpenAI Codex via ChatGPT Plus/Pro 反代（需要 OAuth 认证）
  providerType?: "github_copilot" | "codex_oauth" | "nexus";
  localProxyRequestOverrides?: LocalProxyRequestOverrides;
  managedNexusPresetVersion?: number;

  // 是否需要 OAuth 认证（而非 API Key）
  requiresOAuth?: boolean;

  // 是否在 UI 中隐藏该预设（预设仍存在，仅不在列表中显示）
  hidden?: boolean;

  // 获取模型列表使用的完整 URL（覆写自动候选逻辑）
  // 缺省时后端基于 baseURL 自动尝试 /v1/models、/models 以及剥离已知兼容子路径后的变体。
  modelsUrl?: string;
}

export const providerPresets: ProviderPreset[] = [
  // Nexus routes Anthropic Messages through the built-in Chat adapter.
  {
    name: "Nexus GLM-5.2",
    nameKey: "providerForm.presets.nexus",
    websiteUrl: NEXUS_ENDPOINT,
    settingsConfig: {
      modelCatalog: NEXUS_TEXT_MODEL_CATALOG,
      env: {
        ANTHROPIC_BASE_URL: NEXUS_ENDPOINT,
        ANTHROPIC_AUTH_TOKEN: "",
        ANTHROPIC_MODEL: NEXUS_CLAUDE_MODEL,
        ANTHROPIC_DEFAULT_HAIKU_MODEL: NEXUS_CLAUDE_MODEL,
        ANTHROPIC_DEFAULT_SONNET_MODEL: NEXUS_CLAUDE_MODEL,
        ANTHROPIC_DEFAULT_OPUS_MODEL: NEXUS_CLAUDE_MODEL,
        ANTHROPIC_DEFAULT_FABLE_MODEL: NEXUS_CLAUDE_MODEL,
        API_TIMEOUT_MS: "3000000",
        CLAUDE_CODE_AUTO_COMPACT_WINDOW: String(NEXUS_AUTO_COMPACT_TOKENS),
        CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC: "1",
        CLAUDE_CODE_ATTRIBUTION_HEADER: "0",
      },
    },
    endpointCandidates: [NEXUS_ENDPOINT],
    apiFormat: "openai_chat",
    providerType: "nexus",
    managedNexusPresetVersion: NEXUS_MANAGED_PRESET_VERSION,
    localProxyRequestOverrides: NEXUS_REQUEST_OVERRIDES,
    category: "third_party",
    icon: "nexus",
    iconColor: "#6366F1",
  },
  {
    name: "Claude Official",
    websiteUrl: "https://www.anthropic.com/claude-code",
    settingsConfig: {
      env: {},
    },
    isOfficial: true,
    category: "official",
    theme: {
      icon: "claude",
      backgroundColor: "#D97757",
      textColor: "#FFFFFF",
    },
    icon: "anthropic",
    iconColor: "#D4915D",
  },
];
