import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";
import { describe, expect, it, vi } from "vitest";
import {
  ProviderForm,
  type ProviderFormProps,
} from "@/components/providers/forms/ProviderForm";
import type { LocalProxyRequestOverrides } from "@/types";
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

const desktopTimeoutWarning =
  /providerForm\.claudeDesktopMaxOutputTokensWarning|Claude Desktop may stop a turn/i;

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

function managedClaudeProvider(
  localProxyRequestOverrides: LocalProxyRequestOverrides = NEXUS_REQUEST_OVERRIDES,
  env: Record<string, string> = {},
): NonNullable<ProviderFormProps["initialData"]> {
  return {
    name: "Nexus GLM-5.2",
    category: "third_party",
    settingsConfig: {
      env: {
        ANTHROPIC_BASE_URL: NEXUS_CLAUDE_BASE_URL,
        ANTHROPIC_AUTH_TOKEN: "test-key",
        ANTHROPIC_MODEL: "glm-5.2[1m]",
        CLAUDE_CODE_MAX_OUTPUT_TOKENS: "65536",
        ...env,
      },
    },
    meta: {
      providerType: "nexus",
      managedNexusPresetVersion: NEXUS_CLAUDE_MANAGED_PRESET_VERSION,
      apiFormat: "openai_chat",
      localProxyRequestOverrides,
    },
  };
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
      const submitted = onSubmit.mock.calls[0][0];
      expect(submitted.meta).toMatchObject({
        providerType: "nexus",
        managedNexusPresetVersion:
          appId === "claude"
            ? NEXUS_CLAUDE_MANAGED_PRESET_VERSION
            : NEXUS_MANAGED_PRESET_VERSION,
        localProxyRequestOverrides: NEXUS_REQUEST_OVERRIDES,
      });
      if (appId === "claude") {
        expect(
          JSON.parse(submitted.settingsConfig).env
            .CLAUDE_CODE_MAX_OUTPUT_TOKENS,
        ).toBe("65536");
      }
    },
  );

  it.each([
    { input: "32768", expectedMax: 32_768, expectedEnv: "32768" },
    { input: "", expectedMax: undefined, expectedEnv: undefined },
  ])(
    "syncs the managed Claude client and proxy max token setting for '$input'",
    async ({ input, expectedMax, expectedEnv }) => {
      const onSubmit = renderForm(
        "claude",
        managedClaudeProvider({
          ...NEXUS_REQUEST_OVERRIDES,
          headers: { "x-test": "keep" },
        }),
      );

      await waitFor(() =>
        expect(
          screen.getByDisplayValue(NEXUS_CLAUDE_BASE_URL),
        ).toBeInTheDocument(),
      );
      const inputElement = screen.getByLabelText(
        /providerForm\.maxOutputTokens|Maximum output tokens/i,
      );
      expect(screen.queryByText(desktopTimeoutWarning)).not.toBeInTheDocument();
      fireEvent.change(inputElement, { target: { value: input } });
      fireEvent.submit(inputElement.closest("form")!);

      await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
      const submitted = onSubmit.mock.calls[0][0];
      expect(submitted.meta).toMatchObject({
        providerType: "nexus",
        managedNexusPresetVersion: NEXUS_CLAUDE_MANAGED_PRESET_VERSION,
      });
      expect(submitted.meta.localProxyRequestOverrides).toMatchObject({
        headers: { "x-test": "keep" },
        body: {
          chat_template_kwargs:
            NEXUS_REQUEST_OVERRIDES.body.chat_template_kwargs,
          ...(expectedMax === undefined ? {} : { max_tokens: expectedMax }),
        },
      });
      expect(submitted.meta.localProxyRequestOverrides.body.max_tokens).toBe(
        expectedMax,
      );
      expect(
        JSON.parse(submitted.settingsConfig).env.CLAUDE_CODE_MAX_OUTPUT_TOKENS,
      ).toBe(expectedEnv);
    },
  );

  it("removes the derived Claude cap when managed ownership detaches", async () => {
    const onSubmit = renderForm(
      "claude",
      managedClaudeProvider(NEXUS_REQUEST_OVERRIDES, { KEEP_ME: "yes" }),
    );

    fireEvent.change(screen.getByDisplayValue(NEXUS_CLAUDE_BASE_URL), {
      target: { value: "https://custom.example.com/v1" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
    const submitted = onSubmit.mock.calls[0][0];
    expect(submitted.meta.providerType).toBeUndefined();
    expect(JSON.parse(submitted.settingsConfig).env).toMatchObject({
      KEEP_ME: "yes",
    });
    expect(JSON.parse(submitted.settingsConfig).env).not.toHaveProperty(
      "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
    );
  });

  it("keeps Codex raw max_tokens behavior unchanged", async () => {
    const onSubmit = renderForm("codex", {
      name: "Custom Codex",
      category: "third_party",
      settingsConfig: {
        auth: { OPENAI_API_KEY: "test-key" },
        config:
          'model_provider = "custom"\nmodel = "custom-model"\n[model_providers.custom]\nbase_url = "https://example.com/v1"\n',
      },
      meta: {
        apiFormat: "openai_responses",
        localProxyRequestOverrides: { body: { max_tokens: "passthrough" } },
      },
    });

    expect(screen.queryByText(desktopTimeoutWarning)).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
    expect(
      onSubmit.mock.calls[0][0].meta.localProxyRequestOverrides.body.max_tokens,
    ).toBe("passthrough");
  });
});
