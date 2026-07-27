/**
 * Nexus Composer E2E feature tests.
 *
 * These tests verify the actual Nexus Composer customizations:
 * - Claude Code contains Nexus GLM-5.2 + Nexus Kimi-K3 + Official
 * - Codex contains Nexus GLM-5.2 + Nexus Kimi-K3
 * - Claude Desktop preserves its existing catalog and adds both Nexus presets
 * - Nexus GLM-5.2 preset config is correct (endpoint, model, format)
 * - Claude Official has empty env (resets to native API)
 */
import { describe, expect, it } from "vitest";
import { providerPresets } from "@/config/claudeProviderPresets";
import { claudeDesktopProviderPresets } from "@/config/claudeDesktopProviderPresets";
import { codexProviderPresets } from "@/config/codexProviderPresets";
import {
  NEXUS_AUTO_COMPACT_TOKENS,
  NEXUS_CLAUDE_BASE_URL,
  NEXUS_CLAUDE_DESKTOP_MANAGED_PRESET_VERSION,
  NEXUS_CLAUDE_MANAGED_PRESET_VERSION,
  NEXUS_CLAUDE_MODEL,
  NEXUS_CONTEXT_WINDOW,
  NEXUS_ENDPOINT,
  NEXUS_MAX_OUTPUT_TOKENS,
  NEXUS_MANAGED_PRESET_VERSION,
  NEXUS_MODEL,
  NEXUS_REQUEST_OVERRIDES,
  NEXUS_KIMI_ENDPOINT,
  NEXUS_KIMI_MODEL,
  NEXUS_KIMI_ORIGIN,
  NEXUS_KIMI_TEXT_MODEL_CATALOG,
  NEXUS_TEXT_MODEL_CATALOG,
} from "@/config/nexus";

describe("Nexus Composer preset arrays", () => {
  it("Claude presets contain both Nexus routes and Claude Official", () => {
    const names = providerPresets.map((p) => p.name);
    expect(names).toHaveLength(3);
    expect(names).toContain("Nexus GLM-5.2");
    expect(names).toContain("Nexus Kimi-K3");
    expect(names).toContain("Claude Official");
  });

  it("Codex presets contain Nexus GLM-5.2, Nexus Kimi-K3, and OpenAI Official", () => {
    const names = codexProviderPresets.map((p) => p.name);
    expect(names).toHaveLength(3);
    expect(names).toContain("Nexus GLM-5.2");
    expect(names).toContain("Nexus Kimi-K3");
    expect(names).toContain("OpenAI Official");
  });

  it("adds Nexus without replacing the Claude Desktop preset catalog", () => {
    const names = claudeDesktopProviderPresets.map((preset) => preset.name);
    expect(names).toHaveLength(69);
    expect(names).toContain("Nexus GLM-5.2");
    expect(names).toContain("Nexus Kimi-K3");
    expect(names).toContain("Claude Desktop Official");
    expect(names).toContain("Shengsuanyun");
    expect(new Set(names).size).toBe(names.length);
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

  it("ships Nexus presets without hosted credentials", () => {
    const claude = providerPresets.find(
      (preset) => preset.name === "Nexus GLM-5.2",
    )!;
    const codex = codexProviderPresets.find(
      (preset) => preset.name === "Nexus GLM-5.2",
    )!;
    const desktop = claudeDesktopProviderPresets.find(
      (preset) => preset.name === "Nexus GLM-5.2",
    )!;

    expect((claude.settingsConfig as any).env.ANTHROPIC_AUTH_TOKEN).toBe("");
    expect((codex.auth as any).OPENAI_API_KEY).toBe("");
    expect(JSON.stringify({ claude, codex, desktop })).not.toContain("onenx_");
  });
});

describe("Nexus GLM-5.2 Claude preset config", () => {
  it("uses the hosted Nexus endpoint without a bundled credential", () => {
    const nexus = providerPresets.find((p) => p.name === "Nexus GLM-5.2")!;
    const env = (nexus.settingsConfig as any).env;
    expect(env.ANTHROPIC_BASE_URL).toBe(NEXUS_CLAUDE_BASE_URL);
    expect(nexus.endpointCandidates).toEqual([NEXUS_CLAUDE_BASE_URL]);
    expect(`${env.ANTHROPIC_BASE_URL}/v1/messages`).toBe(
      `${NEXUS_ENDPOINT}/messages`,
    );
    expect(env.ANTHROPIC_AUTH_TOKEN).toBe("");
  });

  it("configures the 1M model aliases and long-running client settings", () => {
    const nexus = providerPresets.find((p) => p.name === "Nexus GLM-5.2")!;
    const env = (nexus.settingsConfig as any).env;
    expect(env.ANTHROPIC_MODEL).toBe(NEXUS_CLAUDE_MODEL);
    expect(env.ANTHROPIC_DEFAULT_SONNET_MODEL).toBe(NEXUS_CLAUDE_MODEL);
    expect(env.ANTHROPIC_DEFAULT_HAIKU_MODEL).toBe(NEXUS_CLAUDE_MODEL);
    expect(env.ANTHROPIC_DEFAULT_OPUS_MODEL).toBe(NEXUS_CLAUDE_MODEL);
    expect(env.ANTHROPIC_DEFAULT_FABLE_MODEL).toBe(NEXUS_CLAUDE_MODEL);
    expect(env.API_TIMEOUT_MS).toBe("3000000");
    expect(env.CLAUDE_CODE_AUTO_COMPACT_WINDOW).toBe(
      String(NEXUS_AUTO_COMPACT_TOKENS),
    );
    expect(env.CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC).toBe("1");
    expect(env.CLAUDE_CODE_ATTRIBUTION_HEADER).toBe("0");
  });

  it("uses the Chat adapter with text-only continuity defaults", () => {
    const nexus = providerPresets.find((p) => p.name === "Nexus GLM-5.2")!;
    expect(nexus.managedNexusPresetVersion).toBe(
      NEXUS_CLAUDE_MANAGED_PRESET_VERSION,
    );
    expect(nexus.apiFormat).toBe("openai_chat");
    expect((nexus.settingsConfig as any).modelCatalog).toEqual(
      NEXUS_TEXT_MODEL_CATALOG,
    );
    expect(nexus.localProxyRequestOverrides).toEqual(NEXUS_REQUEST_OVERRIDES);
  });

  it("has the nexus icon", () => {
    const nexus = providerPresets.find((p) => p.name === "Nexus GLM-5.2")!;
    expect(nexus.icon).toBe("nexus");
    expect(nexus.iconColor).toBe("#6366F1");
  });
});

describe("Nexus GLM-5.2 Codex preset config", () => {
  it("uses the hosted endpoint and managed long-context settings", () => {
    const nexus = codexProviderPresets.find((p) => p.name === "Nexus GLM-5.2")!;
    expect(nexus.config).toContain(NEXUS_ENDPOINT);
    expect(nexus.config).toContain(`model = "${NEXUS_MODEL}"`);
    expect(nexus.config).toContain(
      `model_context_window = ${NEXUS_CONTEXT_WINDOW}`,
    );
    expect(nexus.config).toContain(
      `model_auto_compact_token_limit = ${NEXUS_AUTO_COMPACT_TOKENS}`,
    );
    expect(nexus.config).toContain("stream_idle_timeout_ms = 3000000");
    expect(nexus.config).not.toContain("model_reasoning_effort");
  });

  it("uses the Chat adapter with continuity defaults", () => {
    const nexus = codexProviderPresets.find((p) => p.name === "Nexus GLM-5.2")!;
    expect(nexus.managedNexusPresetVersion).toBe(NEXUS_MANAGED_PRESET_VERSION);
    expect(nexus.apiFormat).toBe("openai_chat");
    expect(nexus.localProxyRequestOverrides).toEqual(NEXUS_REQUEST_OVERRIDES);
    expect(NEXUS_REQUEST_OVERRIDES.body).toEqual({
      max_tokens: NEXUS_MAX_OUTPUT_TOKENS,
      chat_template_kwargs: {
        enable_thinking: true,
        clear_thinking: false,
      },
    });
  });

  it("has model catalog with correct context window", () => {
    const nexus = codexProviderPresets.find((p) => p.name === "Nexus GLM-5.2")!;
    expect(nexus.modelCatalog).toBeDefined();
    expect(nexus.modelCatalog).toHaveLength(1);
    expect(nexus.modelCatalog![0].model).toBe(NEXUS_MODEL);
    expect(nexus.modelCatalog![0].displayName).toBe("GLM-5.2");
    expect(nexus.modelCatalog![0].contextWindow).toBe(NEXUS_CONTEXT_WINDOW);
    expect(nexus.modelCatalog![0].inputModalities).toEqual(["text"]);
  });
});

describe("Nexus Kimi-K3 Codex preset config", () => {
  const nexusKimi = codexProviderPresets.find(
    (preset) => preset.name === "Nexus Kimi-K3",
  )!;

  it("uses the hosted Chat endpoint without requiring a credential", () => {
    expect(nexusKimi).toBeDefined();
    expect(nexusKimi.websiteUrl).toBe(NEXUS_KIMI_ENDPOINT);
    expect(nexusKimi.endpointCandidates).toEqual([NEXUS_KIMI_ENDPOINT]);
    expect(nexusKimi.auth).toEqual({ OPENAI_API_KEY: "" });
    expect(nexusKimi.apiKeyOptional).toBe(true);
    expect(nexusKimi.config).toContain(`model = "${NEXUS_KIMI_MODEL}"`);
    expect(nexusKimi.config).toContain(`base_url = "${NEXUS_KIMI_ENDPOINT}"`);
    expect(nexusKimi.config).toContain("requires_openai_auth = false");
  });

  it("uses Kimi reasoning defaults without GLM-specific overrides", () => {
    expect(nexusKimi.apiFormat).toBe("openai_chat");
    expect(nexusKimi.codexChatReasoning).toEqual({
      supportsThinking: true,
      supportsEffort: false,
      thinkingParam: "thinking",
      effortParam: "none",
      outputFormat: "reasoning_content",
    });
    expect(nexusKimi.localProxyRequestOverrides).toBeUndefined();
    expect(nexusKimi.providerType).toBeUndefined();
    expect(nexusKimi.managedNexusPresetVersion).toBeUndefined();
    expect(nexusKimi.modelCatalog).toEqual([
      expect.objectContaining({
        model: NEXUS_KIMI_MODEL,
        displayName: "Kimi K3",
        inputModalities: ["text"],
      }),
    ]);
  });
});

describe("Nexus Kimi-K3 Claude preset config", () => {
  const nexusKimi = providerPresets.find(
    (preset) => preset.name === "Nexus Kimi-K3",
  )!;

  it("routes the no-key model through the local Chat adapter", () => {
    expect(nexusKimi).toBeDefined();
    expect(nexusKimi.apiFormat).toBe("openai_chat");
    expect(nexusKimi.apiKeyOptional).toBe(true);
    expect(nexusKimi.endpointCandidates).toEqual([NEXUS_KIMI_ORIGIN]);
    expect((nexusKimi.settingsConfig as any).modelCatalog).toEqual(
      NEXUS_KIMI_TEXT_MODEL_CATALOG,
    );
    expect((nexusKimi.settingsConfig as any).env).toMatchObject({
      ANTHROPIC_BASE_URL: NEXUS_KIMI_ORIGIN,
      ANTHROPIC_AUTH_TOKEN: "",
      ANTHROPIC_MODEL: NEXUS_KIMI_MODEL,
      ANTHROPIC_DEFAULT_HAIKU_MODEL: NEXUS_KIMI_MODEL,
      ANTHROPIC_DEFAULT_SONNET_MODEL: NEXUS_KIMI_MODEL,
      ANTHROPIC_DEFAULT_OPUS_MODEL: NEXUS_KIMI_MODEL,
      ANTHROPIC_DEFAULT_FABLE_MODEL: NEXUS_KIMI_MODEL,
    });
    expect(nexusKimi.localProxyRequestOverrides).toBeUndefined();
    expect(nexusKimi.providerType).toBeUndefined();
  });
});

describe("Nexus GLM-5.2 Claude Desktop preset config", () => {
  const nexus = claudeDesktopProviderPresets.find(
    (preset) => preset.name === "Nexus GLM-5.2",
  )!;

  it("routes the text-only hosted model through the local Chat adapter", () => {
    expect(nexus).toBeDefined();
    expect(nexus.baseUrl).toBe(NEXUS_ENDPOINT);
    expect(nexus.mode).toBe("proxy");
    expect(nexus.apiFormat).toBe("openai_chat");
    expect(nexus.modelCatalog).toEqual(NEXUS_TEXT_MODEL_CATALOG);
    expect(nexus.localProxyRequestOverrides).toEqual(NEXUS_REQUEST_OVERRIDES);
  });

  it("declares one 1M GLM route that the form expands across Desktop roles", () => {
    expect(nexus.modelRoutes).toEqual([
      expect.objectContaining({
        upstreamModel: NEXUS_MODEL,
        supports1m: true,
      }),
    ]);
    expect(nexus.providerType).toBe("nexus");
    expect(nexus.managedNexusPresetVersion).toBe(
      NEXUS_CLAUDE_DESKTOP_MANAGED_PRESET_VERSION,
    );
  });
});

describe("Nexus Kimi-K3 Claude Desktop preset config", () => {
  const nexusKimi = claudeDesktopProviderPresets.find(
    (preset) => preset.name === "Nexus Kimi-K3",
  )!;

  it("routes the no-key model through the local Chat adapter", () => {
    expect(nexusKimi).toBeDefined();
    expect(nexusKimi.baseUrl).toBe(NEXUS_KIMI_ENDPOINT);
    expect(nexusKimi.mode).toBe("proxy");
    expect(nexusKimi.apiFormat).toBe("openai_chat");
    expect(nexusKimi.apiKeyOptional).toBe(true);
    expect(nexusKimi.modelCatalog).toEqual(NEXUS_KIMI_TEXT_MODEL_CATALOG);
    expect(nexusKimi.modelRoutes).toEqual([
      expect.objectContaining({
        upstreamModel: NEXUS_KIMI_MODEL,
        supports1m: false,
      }),
    ]);
    expect(nexusKimi.localProxyRequestOverrides).toBeUndefined();
    expect(nexusKimi.providerType).toBeUndefined();
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
    const desktopNexus = claudeDesktopProviderPresets.find(
      (preset) => preset.name === "Nexus GLM-5.2",
    );
    expect(isSponsorPreset(desktopNexus!)).toBe(false);

    const fakeSponsor = {
      name: "FakeProvider",
      isPartner: true,
      category: "aggregator",
    };
    expect(isSponsorPreset(fakeSponsor)).toBe(true);
  });

  it("does not ship a credential, loopback route, or serving-stack copy", () => {
    const serialized = JSON.stringify({
      providerPresets,
      codexProviderPresets,
      claudeDesktopProviderPresets,
    });
    expect(serialized).not.toMatch(/onenx_[A-Za-z0-9_-]+/);
    expect(serialized).not.toMatch(/127\.0\.0\.1|localhost/i);
    expect(serialized).not.toMatch(/SGLang|SSH/i);
  });
});
