/**
 * Nexus Composer E2E feature tests.
 *
 * These tests verify the actual Nexus Composer customizations:
 * - Preset arrays contain only Nexus GLM-5.2 + Official
 * - Nexus GLM-5.2 preset config is correct (endpoint, model, format)
 * - Sponsor filter works (no sponsors visible even if added later)
 * - Claude Official has empty env (resets to native API)
 */
import { describe, expect, it } from "vitest";
import { providerPresets } from "@/config/claudeProviderPresets";
import {
  codexPresetSettingsConfig,
  codexProviderPresets,
} from "@/config/codexProviderPresets";
import {
  NEXUS_AUTH_TOKEN,
  NEXUS_AUTO_COMPACT_TOKENS,
  NEXUS_CAPABILITIES,
  NEXUS_CLAUDE_MODEL,
  NEXUS_CONTEXT_WINDOW,
  NEXUS_ENDPOINT,
} from "@/config/nexus";

describe("Nexus Composer preset arrays", () => {
  it("Claude presets contain exactly Nexus GLM-5.2 and Claude Official", () => {
    const names = providerPresets.map((p) => p.name);
    expect(names).toHaveLength(2);
    expect(names).toContain("Nexus GLM-5.2");
    expect(names).toContain("Claude Official");
  });

  it("Codex presets contain exactly Nexus GLM-5.2 and OpenAI Official", () => {
    const names = codexProviderPresets.map((p) => p.name);
    expect(names).toHaveLength(2);
    expect(names).toContain("Nexus GLM-5.2");
    expect(names).toContain("OpenAI Official");
  });

  it("does not contain any removed presets", () => {
    const claudeNames = providerPresets.map((p) => p.name);
    const codexNames = codexProviderPresets.map((p) => p.name);

    const removed = [
      "Longcat",
      "DeepSeek",
      "Kimi",
      "Kimi For Coding",
      "AWS Bedrock (AKSK)",
      "AWS Bedrock (API Key)",
      "OpenRouter",
      "TheRouter",
      "SubRouter",
      "Baidu Qianfan Coding Plan",
      "Bailian",
      "Xiaomi MiMo",
      "Zhipu GLM",
      "Shengsuanyun",
      "PatewayAI",
    ];
    for (const name of removed) {
      expect(claudeNames).not.toContain(name);
      expect(codexNames).not.toContain(name);
    }
  });
});

describe("Nexus GLM-5.2 Claude preset config", () => {
  it("points to the Nexus endpoint", () => {
    const nexus = providerPresets.find((p) => p.providerType === "nexus")!;
    const env = (nexus.settingsConfig as any).env;
    expect(NEXUS_ENDPOINT).toBe("http://127.0.0.1:30001/v1");
    expect(env.ANTHROPIC_BASE_URL).toBe(NEXUS_ENDPOINT);
    expect(env.ANTHROPIC_AUTH_TOKEN).toBe(NEXUS_AUTH_TOKEN);
  });

  it("advertises 1M context with bounded compaction and stable prefix caching", () => {
    const nexus = providerPresets.find((p) => p.providerType === "nexus")!;
    const env = (nexus.settingsConfig as any).env;
    expect(env.ANTHROPIC_MODEL).toBe(NEXUS_CLAUDE_MODEL);
    expect(env.ANTHROPIC_DEFAULT_SONNET_MODEL).toBe(NEXUS_CLAUDE_MODEL);
    expect(env.ANTHROPIC_DEFAULT_HAIKU_MODEL).toBe(NEXUS_CLAUDE_MODEL);
    expect(env.ANTHROPIC_DEFAULT_OPUS_MODEL).toBe(NEXUS_CLAUDE_MODEL);
    expect(env.API_TIMEOUT_MS).toBe("3000000");
    expect(env.CLAUDE_CODE_AUTO_COMPACT_WINDOW).toBe(
      String(NEXUS_AUTO_COMPACT_TOKENS),
    );
    expect(env.CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC).toBe("1");
    expect(env.CLAUDE_CODE_ATTRIBUTION_HEADER).toBe("0");
    expect((nexus.settingsConfig as any).nexusCapabilities).toEqual(
      NEXUS_CAPABILITIES,
    );
  });

  it("uses openai_chat format for proxy conversion", () => {
    const nexus = providerPresets.find((p) => p.providerType === "nexus")!;
    expect(nexus.apiFormat).toBe("openai_chat");
  });

  it("has the nexus icon", () => {
    const nexus = providerPresets.find((p) => p.providerType === "nexus")!;
    expect(nexus.icon).toBe("nexus");
    expect(nexus.iconColor).toBe("#6366F1");
  });
});

describe("Nexus GLM-5.2 Codex preset config", () => {
  it("points to the Nexus endpoint without forcing reasoning effort", () => {
    const nexus = codexProviderPresets.find((p) => p.providerType === "nexus")!;
    expect(nexus.config).toContain(`base_url = "${NEXUS_ENDPOINT}"`);
    expect(nexus.config).not.toContain("model_reasoning_effort");
    expect(nexus.config).toContain(
      `model_context_window = ${NEXUS_CONTEXT_WINDOW}`,
    );
    expect(nexus.config).toContain(
      `model_auto_compact_token_limit = ${NEXUS_AUTO_COMPACT_TOKENS}`,
    );
    expect(nexus.auth).toEqual({ OPENAI_API_KEY: NEXUS_AUTH_TOKEN });
  });

  it("declares hybrid thinking without forwarding an effort override", () => {
    const nexus = codexProviderPresets.find((p) => p.providerType === "nexus")!;
    expect(nexus.config).toContain("glm-5.2");
    expect(nexus.codexChatReasoning).toMatchObject({
      supportsThinking: true,
      supportsEffort: false,
      thinkingParam: "none",
      effortParam: "none",
    });
    expect(nexus.nexusCapabilities).toEqual(NEXUS_CAPABILITIES);
    expect(codexPresetSettingsConfig(nexus).nexusCapabilities).toEqual(
      NEXUS_CAPABILITIES,
    );
  });

  it("uses openai_chat format for proxy conversion", () => {
    const nexus = codexProviderPresets.find((p) => p.providerType === "nexus")!;
    expect(nexus.apiFormat).toBe("openai_chat");
  });

  it("has model catalog with correct context window", () => {
    const nexus = codexProviderPresets.find((p) => p.providerType === "nexus")!;
    expect(nexus.modelCatalog).toBeDefined();
    expect(nexus.modelCatalog).toHaveLength(1);
    expect(nexus.modelCatalog![0].model).toBe("glm-5.2");
    expect(nexus.modelCatalog![0].displayName).toBe("GLM-5.2");
    expect(nexus.modelCatalog![0].contextWindow).toBe(NEXUS_CONTEXT_WINDOW);
  });
});

describe("Nexus preset safety", () => {
  it("contains no hosted-test endpoint or credential-shaped token", () => {
    const serialized = JSON.stringify({
      providerPresets,
      codexProviderPresets,
    });
    expect(serialized).not.toContain("onenexus-do.cloud");
    expect(serialized).not.toMatch(/onenx_[A-Za-z0-9_-]+/);
  });
});

describe("Claude Official preset", () => {
  it("has empty env (resets to native Anthropic API)", () => {
    const official = providerPresets.find((p) => p.name === "Claude Official")!;
    expect((official.settingsConfig as any).env).toEqual({});
  });

  it("is marked as official category", () => {
    const official = providerPresets.find((p) => p.name === "Claude Official")!;
    expect(official.isOfficial).toBe(true);
    expect(official.category).toBe("official");
  });
});

describe("Sponsor filter", () => {
  it("would hide partner presets if any existed", () => {
    const isSponsorPreset = (preset: any): boolean => {
      if (preset.isOfficial || preset.category === "official") return false;
      if (preset.name === "Nexus GLM-5.2") return false;
      return !!(
        preset.isPartner ||
        preset.primePartner ||
        preset.partnerPromotionKey
      );
    };

    for (const p of providerPresets) {
      expect(isSponsorPreset(p)).toBe(false);
    }
    for (const p of codexProviderPresets) {
      expect(isSponsorPreset(p)).toBe(false);
    }

    const fakeSponsor = {
      name: "FakeProvider",
      isPartner: true,
      category: "aggregator",
    };
    expect(isSponsorPreset(fakeSponsor)).toBe(true);
  });
});
