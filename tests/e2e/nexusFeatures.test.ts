/**
 * Focused checks for the Nexus Composer presets.
 */
import { describe, expect, it } from "vitest";
import { parse as parseToml } from "smol-toml";
import { claudeDesktopProviderPresets } from "@/config/claudeDesktopProviderPresets";
import { providerPresets } from "@/config/claudeProviderPresets";
import { codexProviderPresets } from "@/config/codexProviderPresets";
import {
  NEXUS_AUTO_COMPACT_TOKENS,
  NEXUS_CLAUDE_MODEL,
  NEXUS_CODEX_STREAM_IDLE_TIMEOUT_MS,
  NEXUS_CONTEXT_WINDOW,
  NEXUS_ENDPOINT,
  NEXUS_MAX_OUTPUT_TOKENS,
  NEXUS_MODEL,
  NEXUS_REASONING_REQUEST_OVERRIDES,
  NEXUS_TEXT_MODEL_CATALOG,
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
});

describe("Nexus GLM-5.2 Claude preset config", () => {
  const nexus = () => providerPresets.find((p) => p.name === "Nexus GLM-5.2")!;

  it("points to the hosted endpoint without an embedded key", () => {
    const env = (nexus().settingsConfig as any).env;
    expect(env.ANTHROPIC_BASE_URL).toBe(NEXUS_ENDPOINT);
    expect(env.ANTHROPIC_AUTH_TOKEN).toBe("");
  });

  it("uses the 1M model aliases and bounded compaction", () => {
    const env = (nexus().settingsConfig as any).env;
    expect(env.ANTHROPIC_MODEL).toBe(NEXUS_CLAUDE_MODEL);
    expect(env.ANTHROPIC_DEFAULT_SONNET_MODEL).toBe(NEXUS_CLAUDE_MODEL);
    expect(env.ANTHROPIC_DEFAULT_HAIKU_MODEL).toBe(NEXUS_CLAUDE_MODEL);
    expect(env.ANTHROPIC_DEFAULT_OPUS_MODEL).toBe(NEXUS_CLAUDE_MODEL);
    expect(env.ANTHROPIC_DEFAULT_FABLE_MODEL).toBe(NEXUS_CLAUDE_MODEL);
    expect(env.CLAUDE_CODE_AUTO_COMPACT_WINDOW).toBe(
      String(NEXUS_AUTO_COMPACT_TOKENS),
    );
    expect(env.CLAUDE_CODE_ATTRIBUTION_HEADER).toBe("0");
  });

  it("uses text-only capabilities and the final thinking override", () => {
    expect((nexus().settingsConfig as any).modelCatalog).toEqual(
      NEXUS_TEXT_MODEL_CATALOG,
    );
    expect(nexus().localProxyRequestOverrides).toEqual(
      NEXUS_REASONING_REQUEST_OVERRIDES,
    );
    expect(nexus().localProxyRequestOverrides?.body?.max_tokens).toBe(
      NEXUS_MAX_OUTPUT_TOKENS,
    );
  });

  it("uses openai_chat format for proxy conversion", () => {
    expect(nexus().apiFormat).toBe("openai_chat");
  });
});

describe("Nexus GLM-5.2 Codex preset config", () => {
  const nexus = () =>
    codexProviderPresets.find((p) => p.name === "Nexus GLM-5.2")!;

  it("points to the hosted endpoint and model", () => {
    expect(nexus().config).toContain(NEXUS_ENDPOINT);
    expect(nexus().config).toContain(`model = "${NEXUS_MODEL}"`);
  });

  it("sets context and compaction without forcing reasoning effort", () => {
    expect(nexus().config).toContain(
      `model_context_window = ${NEXUS_CONTEXT_WINDOW}`,
    );
    expect(nexus().config).toContain(
      `model_auto_compact_token_limit = ${NEXUS_AUTO_COMPACT_TOKENS}`,
    );
    expect(nexus().config).not.toContain("model_reasoning_effort");
  });

  it("allows extended stream idle gaps", () => {
    const config = parseToml(nexus().config) as any;
    expect(config.model_providers.custom.stream_idle_timeout_ms).toBe(
      NEXUS_CODEX_STREAM_IDLE_TIMEOUT_MS,
    );
  });

  it("has a text-only model catalog", () => {
    expect(nexus().modelCatalog).toHaveLength(1);
    expect(nexus().modelCatalog![0]).toMatchObject({
      model: NEXUS_MODEL,
      displayName: "GLM-5.2",
      contextWindow: NEXUS_CONTEXT_WINDOW,
      inputModalities: ["text"],
    });
  });
});

describe("Nexus GLM-5.2 Claude Desktop preset config", () => {
  it("maps every role to the hosted text-only model", () => {
    const nexus = claudeDesktopProviderPresets.find(
      (preset) => preset.providerType === "nexus",
    )!;
    expect(nexus.baseUrl).toBe(NEXUS_ENDPOINT);
    expect(nexus.modelCatalog).toEqual(NEXUS_TEXT_MODEL_CATALOG);
    expect(nexus.modelRoutes).toHaveLength(4);
    expect(
      nexus.modelRoutes?.every(
        (route) => route.upstreamModel === NEXUS_MODEL && route.supports1m,
      ),
    ).toBe(true);
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

describe("Nexus preset safety", () => {
  it("contains no loopback fallback or credential-shaped token", () => {
    const serialized = JSON.stringify({
      providerPresets,
      codexProviderPresets,
      claudeDesktopProviderPresets,
    });
    expect(serialized).not.toMatch(/127\.0\.0\.1|localhost/i);
    expect(serialized).not.toMatch(/onenx_[A-Za-z0-9_-]+/);
  });
});
