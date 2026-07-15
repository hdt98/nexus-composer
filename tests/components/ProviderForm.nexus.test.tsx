import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";
import type { ComponentProps } from "react";
import { describe, expect, it, vi } from "vitest";
import { ProviderForm } from "@/components/providers/forms/ProviderForm";
import {
  NEXUS_CLAUDE_MODEL,
  NEXUS_ENDPOINT,
  NEXUS_MANAGED_PRESET_VERSION,
  NEXUS_MAX_OUTPUT_TOKENS,
  NEXUS_MODEL,
} from "@/config/nexus";
import { createTestQueryClient } from "../utils/testQueryClient";

vi.mock("@/components/providers/forms/CodexConfigEditor", () => ({
  default: () => null,
}));
vi.mock("@/components/providers/forms/ProviderAdvancedConfig", () => ({
  ProviderAdvancedConfig: () => null,
}));
vi.mock("@/components/providers/forms/CommonConfigEditor", () => ({
  CommonConfigEditor: () => null,
}));
vi.mock("@/components/JsonEditor", () => ({ default: () => null }));

function renderForm(
  appId: "codex" | "claude",
  onSubmit = vi.fn(),
  initialData?: ComponentProps<typeof ProviderForm>["initialData"],
) {
  render(
    <QueryClientProvider client={createTestQueryClient()}>
      <ProviderForm
        appId={appId}
        submitLabel="Save"
        onSubmit={onSubmit}
        onCancel={vi.fn()}
        initialData={initialData}
      />
    </QueryClientProvider>,
  );
  return onSubmit;
}

const managedMeta = {
  providerType: "nexus",
  managedNexusPresetVersion: NEXUS_MANAGED_PRESET_VERSION,
  apiFormat: "openai_chat" as const,
  localProxyRequestOverrides: {
    headers: { "x-custom": "keep-me" },
    body: {
      max_tokens: NEXUS_MAX_OUTPUT_TOKENS,
      temperature: 0.2,
      chat_template_kwargs: { enable_thinking: true, custom: "keep-me" },
    },
  },
};

const managedCatalog = {
  customMetadata: "keep-me",
  models: [
    { model: NEXUS_MODEL, inputModalities: ["text"] },
    { model: NEXUS_CLAUDE_MODEL, inputModalities: ["text"] },
    { model: "glm-5.2", inputModalities: ["text"] },
    { model: "glm-5.2[1m]", inputModalities: ["text"] },
    { model: NEXUS_MODEL, role: "custom", keep: true },
    { model: "custom-model", inputModalities: ["text", "image"] },
  ],
};

const managedCodexConfig = `model_provider = "custom"
model = "${NEXUS_MODEL}"
[model_providers.custom]
name = "Nexus GLM-5.2"
base_url = "${NEXUS_ENDPOINT}"
wire_api = "responses"
`;

const managedClaudeSettings = {
  modelCatalog: managedCatalog,
  env: {
    ANTHROPIC_BASE_URL: NEXUS_ENDPOINT,
    ANTHROPIC_AUTH_TOKEN: "old-key",
    ANTHROPIC_MODEL: NEXUS_CLAUDE_MODEL,
    ANTHROPIC_DEFAULT_HAIKU_MODEL: NEXUS_CLAUDE_MODEL,
    ANTHROPIC_DEFAULT_SONNET_MODEL: NEXUS_CLAUDE_MODEL,
    ANTHROPIC_DEFAULT_OPUS_MODEL: NEXUS_CLAUDE_MODEL,
    ANTHROPIC_DEFAULT_FABLE_MODEL: NEXUS_CLAUDE_MODEL,
    ANTHROPIC_CUSTOM_MODEL_OPTION: NEXUS_CLAUDE_MODEL,
  },
};

describe("ProviderForm Nexus presets", () => {
  it("persists the Codex managed marker and final thinking override", async () => {
    const onSubmit = renderForm("codex");

    fireEvent.click(
      screen.getByRole("button", {
        name: /providerForm\.presets\.nexus|Nexus GLM-5\.2/i,
      }),
    );
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "test-key" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
    const payload = onSubmit.mock.calls[0][0];
    expect(payload.meta).toMatchObject({
      providerType: "nexus",
      managedNexusPresetVersion: NEXUS_MANAGED_PRESET_VERSION,
      localProxyRequestOverrides: {
        body: {
          max_tokens: NEXUS_MAX_OUTPUT_TOKENS,
          chat_template_kwargs: { enable_thinking: true },
        },
      },
    });
    expect(JSON.parse(payload.settingsConfig).config).not.toContain(
      "model_reasoning_effort",
    );
  });

  it("persists the Claude managed marker and final thinking override", async () => {
    const onSubmit = renderForm("claude");

    fireEvent.click(
      screen.getByRole("button", {
        name: /providerForm\.presets\.nexus|Nexus GLM-5\.2/i,
      }),
    );
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "test-key" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
    expect(onSubmit.mock.calls[0][0].meta).toMatchObject({
      providerType: "nexus",
      managedNexusPresetVersion: NEXUS_MANAGED_PRESET_VERSION,
      localProxyRequestOverrides: {
        body: {
          max_tokens: NEXUS_MAX_OUTPUT_TOKENS,
          chat_template_kwargs: { enable_thinking: true },
        },
      },
    });
  });

  it("keeps managed Codex metadata when only its credential changes", async () => {
    const onSubmit = renderForm("codex", vi.fn(), {
      name: "Nexus GLM-5.2",
      category: "third_party",
      settingsConfig: {
        auth: { OPENAI_API_KEY: "old-key" },
        config: managedCodexConfig,
        modelCatalog: managedCatalog,
      },
      meta: managedMeta,
    });

    fireEvent.change(document.querySelector("#codexBaseUrl")!, {
      target: { value: NEXUS_ENDPOINT },
    });
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "new-key" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
    expect(onSubmit.mock.calls[0][0].meta).toMatchObject(managedMeta);
  });

  it("detaches managed Codex metadata and only its catalog on endpoint edit", async () => {
    const onSubmit = renderForm("codex", vi.fn(), {
      name: "Nexus GLM-5.2",
      category: "third_party",
      settingsConfig: {
        auth: { OPENAI_API_KEY: "old-key" },
        config: managedCodexConfig,
        modelCatalog: managedCatalog,
      },
      meta: managedMeta,
    });

    fireEvent.change(document.querySelector("#codexBaseUrl")!, {
      target: { value: "https://custom.example/v1" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
    const submitted = onSubmit.mock.calls[0][0];
    expect(submitted.meta).not.toHaveProperty("providerType");
    expect(submitted.meta).not.toHaveProperty("managedNexusPresetVersion");
    expect(submitted.meta.localProxyRequestOverrides).toEqual({
      headers: { "x-custom": "keep-me" },
      body: {
        temperature: 0.2,
        chat_template_kwargs: { custom: "keep-me" },
      },
    });
    expect(JSON.parse(submitted.settingsConfig).modelCatalog.models).toEqual([
      { model: NEXUS_MODEL, role: "custom", keep: true },
      { model: "custom-model", inputModalities: ["text", "image"] },
    ]);
    expect(
      JSON.parse(submitted.settingsConfig).modelCatalog.customMetadata,
    ).toBe("keep-me");
  });

  it("keeps managed Claude metadata when only its credential changes", async () => {
    const onSubmit = renderForm("claude", vi.fn(), {
      name: "Nexus GLM-5.2",
      category: "third_party",
      settingsConfig: managedClaudeSettings,
      meta: managedMeta,
    });

    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "new-key" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
    expect(onSubmit.mock.calls[0][0].meta).toMatchObject(managedMeta);
  });

  it("detaches managed Claude metadata and only its catalog on model edit", async () => {
    const onSubmit = renderForm("claude", vi.fn(), {
      name: "Nexus GLM-5.2",
      category: "third_party",
      settingsConfig: managedClaudeSettings,
      meta: managedMeta,
    });

    fireEvent.change(document.querySelector("#claudeModel")!, {
      target: { value: "custom-model" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
    const submitted = onSubmit.mock.calls[0][0];
    expect(submitted.meta).not.toHaveProperty("providerType");
    expect(submitted.meta).not.toHaveProperty("managedNexusPresetVersion");
    expect(submitted.meta.localProxyRequestOverrides).toEqual({
      headers: { "x-custom": "keep-me" },
      body: {
        temperature: 0.2,
        chat_template_kwargs: { custom: "keep-me" },
      },
    });
    expect(JSON.parse(submitted.settingsConfig).modelCatalog.models).toEqual([
      { model: NEXUS_MODEL, role: "custom", keep: true },
      { model: "custom-model", inputModalities: ["text", "image"] },
    ]);
  });
});
