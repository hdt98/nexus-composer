import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";
import { describe, expect, it, vi } from "vitest";
import {
  ProviderForm,
  type ProviderFormProps,
} from "@/components/providers/forms/ProviderForm";
import {
  NEXUS_CLAUDE_BASE_URL,
  NEXUS_CLAUDE_MANAGED_PRESET_VERSION,
  NEXUS_MANAGED_PRESET_VERSION,
  NEXUS_REQUEST_OVERRIDES,
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
  initialData?: ProviderFormProps["initialData"],
) {
  const onSubmit = vi.fn();
  render(
    <QueryClientProvider client={createTestQueryClient()}>
      <ProviderForm
        appId={appId}
        providerId={initialData ? "managed-provider" : undefined}
        submitLabel="Save"
        onSubmit={onSubmit}
        onCancel={vi.fn()}
        initialData={initialData}
      />
    </QueryClientProvider>,
  );
  return onSubmit;
}

describe("ProviderForm Nexus presets", () => {
  it.each(["codex", "claude"] as const)(
    "persists managed metadata and request defaults for %s",
    async (appId) => {
      const onSubmit = renderForm(appId);

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
        managedNexusPresetVersion:
          appId === "claude"
            ? NEXUS_CLAUDE_MANAGED_PRESET_VERSION
            : NEXUS_MANAGED_PRESET_VERSION,
        localProxyRequestOverrides: NEXUS_REQUEST_OVERRIDES,
      });
    },
  );

  it("preserves managed metadata when saving a historical Claude model alias", async () => {
    const onSubmit = renderForm("claude", {
      name: "Nexus GLM-5.2",
      category: "third_party",
      settingsConfig: {
        env: {
          ANTHROPIC_BASE_URL: NEXUS_CLAUDE_BASE_URL,
          ANTHROPIC_AUTH_TOKEN: "test-key",
          ANTHROPIC_MODEL: "glm-5.2[1m]",
        },
      },
      meta: {
        providerType: "nexus",
        managedNexusPresetVersion: NEXUS_CLAUDE_MANAGED_PRESET_VERSION,
        apiFormat: "openai_chat",
        localProxyRequestOverrides: NEXUS_REQUEST_OVERRIDES,
      },
    });

    await waitFor(() =>
      expect(
        screen.getByDisplayValue(NEXUS_CLAUDE_BASE_URL),
      ).toBeInTheDocument(),
    );
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
    expect(onSubmit.mock.calls[0][0].meta).toMatchObject({
      providerType: "nexus",
      managedNexusPresetVersion: NEXUS_CLAUDE_MANAGED_PRESET_VERSION,
    });
  });
});
