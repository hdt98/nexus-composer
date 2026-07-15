import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";
import { describe, expect, it, vi } from "vitest";
import { ProviderForm } from "@/components/providers/forms/ProviderForm";
import { NEXUS_REQUEST_OVERRIDES } from "@/config/nexus";
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

function renderForm(appId: "codex" | "claude") {
  const onSubmit = vi.fn();
  render(
    <QueryClientProvider client={createTestQueryClient()}>
      <ProviderForm
        appId={appId}
        submitLabel="Save"
        onSubmit={onSubmit}
        onCancel={vi.fn()}
      />
    </QueryClientProvider>,
  );
  return onSubmit;
}

describe("ProviderForm Nexus presets", () => {
  it.each(["codex", "claude"] as const)(
    "persists the managed Nexus metadata for %s",
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
        managedNexusPresetVersion: 1,
        localProxyRequestOverrides: NEXUS_REQUEST_OVERRIDES,
      });
    },
  );
});
