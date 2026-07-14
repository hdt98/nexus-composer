import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";
import { describe, expect, it, vi } from "vitest";
import { ProviderForm } from "@/components/providers/forms/ProviderForm";
import { codexProviderPresets } from "@/config/codexProviderPresets";
import { NEXUS_CAPABILITIES } from "@/config/nexusCapabilities";
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

describe("ProviderForm Nexus capabilities", () => {
  it("writes the Codex preset capability into a new provider", async () => {
    const onSubmit = vi.fn();
    render(
      <QueryClientProvider client={createTestQueryClient()}>
        <ProviderForm
          appId="codex"
          submitLabel="Save"
          onSubmit={onSubmit}
          onCancel={vi.fn()}
        />
      </QueryClientProvider>,
    );

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
    const settings = JSON.parse(onSubmit.mock.calls[0][0].settingsConfig);
    expect(settings.nexusCapabilities).toEqual(NEXUS_CAPABILITIES);
  });

  it("preserves an existing Codex capability through edit reconstruction", async () => {
    const preset = codexProviderPresets.find(
      (candidate) => candidate.nexusCapabilities,
    )!;
    const onSubmit = vi.fn();
    render(
      <QueryClientProvider client={createTestQueryClient()}>
        <ProviderForm
          appId="codex"
          submitLabel="Save"
          onSubmit={onSubmit}
          onCancel={vi.fn()}
          initialData={{
            name: "Nexus GLM-5.2",
            websiteUrl: preset.websiteUrl,
            category: preset.category,
            settingsConfig: {
              auth: { OPENAI_API_KEY: "test-key" },
              config: preset.config,
              modelCatalog: { models: preset.modelCatalog },
              nexusCapabilities: NEXUS_CAPABILITIES,
            },
            meta: { apiFormat: "openai_chat" },
          }}
        />
      </QueryClientProvider>,
    );

    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
    const settings = JSON.parse(onSubmit.mock.calls[0][0].settingsConfig);
    expect(settings.nexusCapabilities).toEqual(NEXUS_CAPABILITIES);
  });
});
