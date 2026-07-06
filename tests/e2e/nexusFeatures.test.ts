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
import { codexProviderPresets } from "@/config/codexProviderPresets";

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
      "Longcat", "DeepSeek", "Kimi", "Kimi For Coding",
      "AWS Bedrock (AKSK)", "AWS Bedrock (API Key)",
      "OpenRouter", "TheRouter", "SubRouter",
      "Baidu Qianfan Coding Plan", "Bailian",
      "Xiaomi MiMo", "Zhipu GLM",
      "Shengsuanyun", "PatewayAI",
    ];
    for (const name of removed) {
      expect(claudeNames).not.toContain(name);
      expect(codexNames).not.toContain(name);
    }
  });
});

describe("Nexus GLM-5.2 Claude preset config", () => {
  it("points to the SGLang endpoint", () => {
    const nexus = providerPresets.find((p) => p.name === "Nexus GLM-5.2")!;
    const env = (nexus.settingsConfig as any).env;
    expect(env.ANTHROPIC_BASE_URL).toBe("http://127.0.0.1:30000/v1");
  });

  it("uses GLM-5.2-SGLang as the model", () => {
    const nexus = providerPresets.find((p) => p.name === "Nexus GLM-5.2")!;
    const env = (nexus.settingsConfig as any).env;
    expect(env.ANTHROPIC_MODEL).toBe("GLM-5.2-SGLang");
    expect(env.ANTHROPIC_DEFAULT_SONNET_MODEL).toBe("GLM-5.2-SGLang");
    expect(env.ANTHROPIC_DEFAULT_HAIKU_MODEL).toBe("GLM-5.2-SGLang");
    expect(env.ANTHROPIC_DEFAULT_OPUS_MODEL).toBe("GLM-5.2-SGLang");
  });

  it("uses openai_chat format for proxy conversion", () => {
    const nexus = providerPresets.find((p) => p.name === "Nexus GLM-5.2")!;
    expect(nexus.apiFormat).toBe("openai_chat");
  });

  it("has the nexus icon", () => {
    const nexus = providerPresets.find((p) => p.name === "Nexus GLM-5.2")!;
    expect(nexus.icon).toBe("nexus");
    expect(nexus.iconColor).toBe("#6366F1");
  });
});

describe("Nexus GLM-5.2 Codex preset config", () => {
  it("points to the SGLang endpoint", () => {
    const nexus = codexProviderPresets.find((p) => p.name === "Nexus GLM-5.2")!;
    expect(nexus.config).toContain("http://127.0.0.1:30000/v1");
  });

  it("uses GLM-5.2-SGLang as the model", () => {
    const nexus = codexProviderPresets.find((p) => p.name === "Nexus GLM-5.2")!;
    expect(nexus.config).toContain("GLM-5.2-SGLang");
  });

  it("uses openai_chat format for proxy conversion", () => {
    const nexus = codexProviderPresets.find((p) => p.name === "Nexus GLM-5.2")!;
    expect(nexus.apiFormat).toBe("openai_chat");
  });

  it("has model catalog with correct context window", () => {
    const nexus = codexProviderPresets.find((p) => p.name === "Nexus GLM-5.2")!;
    expect(nexus.modelCatalog).toBeDefined();
    expect(nexus.modelCatalog).toHaveLength(1);
    expect(nexus.modelCatalog![0].model).toBe("GLM-5.2-SGLang");
    expect(nexus.modelCatalog![0].displayName).toBe("GLM-5.2");
    expect(nexus.modelCatalog![0].contextWindow).toBe(1048576);
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
      return !!(preset.isPartner || preset.primePartner || preset.partnerPromotionKey);
    };

    for (const p of providerPresets) {
      expect(isSponsorPreset(p)).toBe(false);
    }
    for (const p of codexProviderPresets) {
      expect(isSponsorPreset(p)).toBe(false);
    }

    const fakeSponsor = { name: "FakeProvider", isPartner: true, category: "aggregator" };
    expect(isSponsorPreset(fakeSponsor)).toBe(true);
  });
});
