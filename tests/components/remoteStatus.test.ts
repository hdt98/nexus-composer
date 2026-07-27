import { describe, expect, it } from "vitest";
import type { Provider } from "@/types";
import type { RemoteAgentStatus } from "@/lib/api/providers";
import {
  resolveRemoteAgentProviderId,
  resolveRemoteModelLabel,
  resolveRemoteProviderLabel,
  savedSshConnectionToTarget,
} from "@/components/remote/remoteStatus";

const provider = (
  name: string,
  settingsConfig: Record<string, any>,
  id = name.toLowerCase().replace(/ /g, "-"),
) => ({ id, name, settingsConfig }) as Provider;

describe("remote status helpers", () => {
  it("creates an auto-probe target without a persisted password", () => {
    expect(
      savedSshConnectionToTarget({
        host: "192.0.2.10",
        user: "locmt",
        port: 22,
      }),
    ).toEqual({
      type: "manual",
      host: "192.0.2.10",
      user: "locmt",
      port: 22,
    });
  });

  it("shows the running agent model before the static remote config model", () => {
    const agentStatus = {
      running: true,
      status: {
        proxy: { upstream_model: "GLM-5.2-FP8" },
        codex: { current_provider: "nexus-agent" },
      },
    } as RemoteAgentStatus;

    expect(
      resolveRemoteModelLabel({
        appId: "codex",
        agentStatus,
        provider: provider("Old provider", { config: 'model = "old-model"' }),
      }),
    ).toBe("GLM-5.2-FP8");
    expect(
      resolveRemoteProviderLabel({
        appId: "codex",
        agentStatus,
        matchedProvider: provider("Nexus GLM-5.2", {}),
        remoteProvider: provider("Old provider", {}),
      }),
    ).toBe("Nexus GLM-5.2");
  });

  it("matches the running agent to the local provider by endpoint and model", () => {
    const glmProvider = provider(
      "Nexus GLM-5.2",
      {
        config: `model_provider = "custom"
model = "GLM-5.2-FP8"

[model_providers.custom]
base_url = "http://198.51.100.20:30000/v1/"`,
      },
      "glm-provider",
    );
    const agentStatus = {
      running: true,
      status: {
        proxy: {
          upstream_url: "http://198.51.100.20:30000/v1",
          upstream_model: "GLM-5.2-FP8",
        },
      },
    } as RemoteAgentStatus;

    expect(
      resolveRemoteAgentProviderId({
        agentStatus,
        providers: { [glmProvider.id]: glmProvider },
      }),
    ).toBe("glm-provider");
  });

  it("prefers an explicit provider id reported by newer agents", () => {
    const glmProvider = provider("Nexus GLM-5.2", {}, "glm-provider");
    expect(
      resolveRemoteAgentProviderId({
        agentStatus: {
          running: true,
          status: { agent: { provider_id: "glm-provider" } },
        } as RemoteAgentStatus,
        providers: { [glmProvider.id]: glmProvider },
      }),
    ).toBe("glm-provider");
  });

  it("reads only the top-level Codex model when the agent is stopped", () => {
    expect(
      resolveRemoteModelLabel({
        appId: "codex",
        provider: provider("Custom", {
          config: `model = "GLM-5.2-FP8"

[profiles.nested]
model = "nested-model"`,
        }),
      }),
    ).toBe("GLM-5.2-FP8");
  });

  it("detects an empty Codex config as OpenAI Official with a safe default label", () => {
    const remoteOfficial = provider("Remote current config", {
      auth: { tokens: { refresh_token: "never-render-this" } },
      config: "",
    });

    expect(
      resolveRemoteProviderLabel({
        appId: "codex",
        remoteProvider: remoteOfficial,
      }),
    ).toBe("OpenAI Official");
    expect(
      resolveRemoteModelLabel({
        appId: "codex",
        provider: remoteOfficial,
      }),
    ).toBe("GPT (default)");
  });

  it("uses an explicit Official model before the default label", () => {
    expect(
      resolveRemoteModelLabel({
        appId: "codex",
        provider: provider("Remote current config", {
          config: 'model = "gpt-5.6-sol"',
        }),
      }),
    ).toBe("gpt-5.6-sol");
  });

  it("falls back to Claude and Gemini environment models", () => {
    expect(
      resolveRemoteModelLabel({
        appId: "claude",
        provider: provider("Claude", {
          env: { ANTHROPIC_MODEL: "claude-sonnet-4-5" },
        }),
      }),
    ).toBe("claude-sonnet-4-5");
    expect(
      resolveRemoteModelLabel({
        appId: "gemini",
        provider: provider("Gemini", {
          env: { GEMINI_MODEL: "gemini-2.5-pro" },
        }),
      }),
    ).toBe("gemini-2.5-pro");
  });
});
