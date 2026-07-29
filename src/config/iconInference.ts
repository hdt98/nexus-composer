/**
 * Infers icon configuration from a provider name.
 */

const iconMappings = {
  // AI providers.
  claude: { icon: "claude", iconColor: "#D4915D" },
  anthropic: { icon: "anthropic", iconColor: "#D4915D" },
  deepseek: { icon: "deepseek", iconColor: "#1E88E5" },
  zhipu: { icon: "zhipu", iconColor: "#0F62FE" },
  glm: { icon: "zhipu", iconColor: "#0F62FE" },
  qwen: { icon: "qwen", iconColor: "#FF6A00" },
  bailian: { icon: "bailian", iconColor: "#624AFF" },
  alibaba: { icon: "alibaba", iconColor: "#FF6A00" },
  aliyun: { icon: "alibaba", iconColor: "#FF6A00" },
  kimi: { icon: "kimi", iconColor: "#6366F1" },
  moonshot: { icon: "moonshot", iconColor: "#6366F1" },
  stepfun: { icon: "stepfun", iconColor: "#005AFF" },
  step: { icon: "stepfun", iconColor: "#005AFF" },
  baidu: { icon: "baidu", iconColor: "#2932E1" },
  tencent: { icon: "tencent", iconColor: "#00A4FF" },
  hunyuan: { icon: "hunyuan", iconColor: "#00A4FF" },
  minimax: { icon: "minimax", iconColor: "#FF6B6B" },
  google: { icon: "google", iconColor: "#4285F4" },
  meta: { icon: "meta", iconColor: "#0081FB" },
  mistral: { icon: "mistral", iconColor: "#FF7000" },
  cohere: { icon: "cohere", iconColor: "#39594D" },
  perplexity: { icon: "perplexity", iconColor: "#20808D" },
  huggingface: { icon: "huggingface", iconColor: "#FFD21E" },
  novita: { icon: "novita", iconColor: "#000000" },

  // Cloud platforms.
  aws: { icon: "aws", iconColor: "#FF9900" },
  azure: { icon: "azure", iconColor: "#0078D4" },
  huawei: { icon: "huawei", iconColor: "#FF0000" },
  cloudflare: { icon: "cloudflare", iconColor: "#F38020" },
};

/**
 * Infers an icon from a preset name.
 */
export function inferIconForPreset(presetName: string): {
  icon?: string;
  iconColor?: string;
} {
  const nameLower = presetName.toLowerCase();

  // Match exact names or substrings.
  for (const [key, config] of Object.entries(iconMappings)) {
    if (nameLower.includes(key)) {
      return config;
    }
  }

  return {};
}

/**
 * Adds inferred icon configuration to presets in bulk.
 */
export function addIconsToPresets<
  T extends { name: string; icon?: string; iconColor?: string },
>(presets: T[]): T[] {
  return presets.map((preset) => {
    // Preserve an explicitly configured icon.
    if (preset.icon) {
      return preset;
    }

    // Otherwise infer an icon from the name.
    const inferred = inferIconForPreset(preset.name);
    return {
      ...preset,
      ...inferred,
    };
  });
}
