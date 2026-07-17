import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";
import type { ComponentProps } from "react";
import userEvent from "@testing-library/user-event";
import { beforeAll, describe, expect, it, vi } from "vitest";
import { ClaudeDesktopProviderForm } from "@/components/providers/forms/ClaudeDesktopProviderForm";
import {
  NEXUS_ENDPOINT,
  NEXUS_CLAUDE_DESKTOP_MANAGED_PRESET_VERSION,
  NEXUS_MODEL,
  NEXUS_REQUEST_OVERRIDES,
  NEXUS_TEXT_MODEL_CATALOG,
} from "@/config/nexus";
import { createTestQueryClient } from "../utils/testQueryClient";

vi.mock("@/lib/api/providers", () => ({
  providersApi: {
    getClaudeDesktopDefaultRoutes: () => Promise.resolve([]),
  },
}));

beforeAll(() => {
  Element.prototype.scrollIntoView = vi.fn();
});

function renderForm(
  initialData: ComponentProps<typeof ClaudeDesktopProviderForm>["initialData"],
  onSubmit = vi.fn(),
) {
  const queryClient = createTestQueryClient();
  const view = render(
    <QueryClientProvider client={queryClient}>
      <ClaudeDesktopProviderForm
        submitLabel="Save"
        onSubmit={onSubmit}
        onCancel={vi.fn()}
        initialData={initialData}
      />
    </QueryClientProvider>,
  );
  return { ...view, onSubmit };
}

function managedNexusProvider() {
  return {
    name: "Nexus GLM-5.2",
    settingsConfig: {
      env: {
        ANTHROPIC_BASE_URL: NEXUS_ENDPOINT,
        ANTHROPIC_AUTH_TOKEN: "old-key",
      },
      modelCatalog: NEXUS_TEXT_MODEL_CATALOG,
    },
    meta: {
      providerType: "nexus",
      managedNexusPresetVersion: NEXUS_CLAUDE_DESKTOP_MANAGED_PRESET_VERSION,
      localProxyRequestOverrides: NEXUS_REQUEST_OVERRIDES,
      claudeDesktopMode: "proxy" as const,
      apiFormat: "openai_chat" as const,
      claudeDesktopModelRoutes: Object.fromEntries(
        [
          "claude-sonnet-5",
          "claude-opus-4-8",
          "claude-fable-5",
          "claude-haiku-4-5",
        ].map((route) => [
          route,
          {
            model: NEXUS_MODEL,
            labelOverride: NEXUS_MODEL,
            supports1m: true,
          },
        ]),
      ),
    },
  };
}

function expectManagedNexusDetached(submitted: {
  settingsConfig: string;
  meta: Record<string, unknown>;
}) {
  expect(JSON.parse(submitted.settingsConfig)).not.toHaveProperty(
    "modelCatalog",
  );
  expect(submitted.meta).not.toHaveProperty("providerType");
  expect(submitted.meta).not.toHaveProperty("managedNexusPresetVersion");
  expect(submitted.meta).not.toHaveProperty("localProxyRequestOverrides");
}

describe("ClaudeDesktopProviderForm", () => {
  it("persists the managed Nexus preset metadata and request defaults", async () => {
    const onSubmit = vi.fn();
    renderForm(undefined, onSubmit);

    fireEvent.click(
      screen.getByRole("button", {
        name: /providerForm\.presets\.nexus|Nexus GLM-5\.2/i,
      }),
    );
    expect(
      screen.queryByText("providerForm.getApiKey"),
    ).not.toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "test-key" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
    const submitted = onSubmit.mock.calls[0][0];
    expect(JSON.parse(submitted.settingsConfig).modelCatalog).toEqual(
      NEXUS_TEXT_MODEL_CATALOG,
    );
    expect(submitted.meta).toMatchObject({
      providerType: "nexus",
      managedNexusPresetVersion: NEXUS_CLAUDE_DESKTOP_MANAGED_PRESET_VERSION,
      localProxyRequestOverrides: NEXUS_REQUEST_OVERRIDES,
    });
    const routes = Object.values(submitted.meta.claudeDesktopModelRoutes) as {
      model: string;
      supports1m?: boolean;
    }[];
    expect(routes).toHaveLength(4);
    expect(
      routes.every(
        (route) => route.model === "GLM-5.2-FP8" && route.supports1m,
      ),
    ).toBe(true);
  });

  it("detaches a newly selected Nexus preset when its endpoint is customized", async () => {
    const onSubmit = vi.fn();
    renderForm(undefined, onSubmit);

    fireEvent.click(
      screen.getByRole("button", {
        name: /providerForm\.presets\.nexus|Nexus GLM-5\.2/i,
      }),
    );
    fireEvent.change(screen.getByLabelText("providerForm.apiEndpoint"), {
      target: { value: "https://custom.example.com/v1" },
    });
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "test-key" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
    expectManagedNexusDetached(onSubmit.mock.calls[0][0]);
  });

  it("preserves managed Nexus ownership for name and API-key edits", async () => {
    const { onSubmit } = renderForm(managedNexusProvider());

    fireEvent.change(screen.getByLabelText("provider.name"), {
      target: { value: "My Nexus" },
    });
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "new-key" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
    const submitted = onSubmit.mock.calls[0][0];
    expect(submitted.name).toBe("My Nexus");
    expect(JSON.parse(submitted.settingsConfig)).toMatchObject({
      env: { ANTHROPIC_AUTH_TOKEN: "new-key" },
      modelCatalog: NEXUS_TEXT_MODEL_CATALOG,
    });
    expect(submitted.meta).toMatchObject({
      providerType: "nexus",
      managedNexusPresetVersion: NEXUS_CLAUDE_DESKTOP_MANAGED_PRESET_VERSION,
      localProxyRequestOverrides: NEXUS_REQUEST_OVERRIDES,
    });
  });

  it("detaches managed Nexus ownership when the endpoint changes", async () => {
    const { onSubmit } = renderForm(managedNexusProvider());

    fireEvent.change(screen.getByLabelText("providerForm.apiEndpoint"), {
      target: { value: "https://custom.example.com/v1" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
    expectManagedNexusDetached(onSubmit.mock.calls[0][0]);
  });

  it("detaches managed Nexus ownership when the routing mode changes", async () => {
    const { onSubmit } = renderForm(managedNexusProvider());

    fireEvent.click(screen.getByRole("switch"));
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
    expectManagedNexusDetached(onSubmit.mock.calls[0][0]);
  });

  it("detaches managed Nexus ownership when the API format changes", async () => {
    const user = userEvent.setup();
    const { onSubmit } = renderForm(managedNexusProvider());

    await user.click(screen.getByRole("combobox"));
    await user.click(
      await screen.findByRole("option", {
        name: "Anthropic Messages (native)",
      }),
    );
    await user.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
    expectManagedNexusDetached(onSubmit.mock.calls[0][0]);
  });

  it("detaches managed Nexus ownership when a model route changes", async () => {
    const { onSubmit } = renderForm(managedNexusProvider());

    fireEvent.change(screen.getAllByPlaceholderText("deepseek-v4-pro")[0], {
      target: { value: "custom-model" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
    expectManagedNexusDetached(onSubmit.mock.calls[0][0]);
  });

  it("keeps focus while editing a mapped model label", () => {
    renderForm({
      name: "Proxy Provider",
      settingsConfig: {
        env: {
          ANTHROPIC_BASE_URL: "https://api.example.com",
          ANTHROPIC_AUTH_TOKEN: "sk-test",
        },
      },
      meta: {
        claudeDesktopMode: "proxy",
        claudeDesktopModelRoutes: {
          "claude-old": {
            model: "upstream-old",
          },
        },
      },
    });

    // The first of the four menu-label inputs belongs to Sonnet.
    const input = screen.getAllByPlaceholderText(
      "DeepSeek V4 Pro",
    )[0] as HTMLInputElement;
    input.focus();

    fireEvent.change(input, { target: { value: "DeepSeek V4 Pro" } });

    const currentInput = screen.getAllByPlaceholderText(
      "DeepSeek V4 Pro",
    )[0] as HTMLInputElement;
    expect(currentInput).toHaveValue("DeepSeek V4 Pro");
    expect(document.activeElement).toBe(currentInput);
  });

  it("keeps focus while editing a direct model ID", () => {
    renderForm({
      name: "Direct Provider",
      settingsConfig: {
        env: {
          ANTHROPIC_BASE_URL: "https://api.example.com",
          ANTHROPIC_AUTH_TOKEN: "sk-test",
        },
      },
      meta: {
        claudeDesktopMode: "direct",
        claudeDesktopModelRoutes: {
          "claude-old": {
            model: "claude-old",
          },
        },
      },
    });

    const input = screen.getByPlaceholderText(
      "claude-sonnet-4-6",
    ) as HTMLInputElement;
    input.focus();

    fireEvent.change(input, { target: { value: "claude-12345" } });

    const currentInput = screen.getByPlaceholderText(
      "claude-sonnet-4-6",
    ) as HTMLInputElement;
    expect(currentInput).toHaveValue("claude-12345");
    expect(document.activeElement).toBe(currentInput);
  });

  it("renders all four proxy roles when only one role is configured", () => {
    renderForm({
      name: "Proxy Provider",
      settingsConfig: {
        env: {
          ANTHROPIC_BASE_URL: "https://api.example.com",
          ANTHROPIC_AUTH_TOKEN: "sk-test",
        },
      },
      meta: {
        claudeDesktopMode: "proxy",
        claudeDesktopModelRoutes: {
          "claude-sonnet-4-6": { model: "upstream-sonnet" },
        },
      },
    });

    // Haiku uses the Flash placeholder and the other roles use Pro, yielding
    // one menu-label input for each of the four fixed roles.
    expect(
      screen.getAllByPlaceholderText(/DeepSeek V4 (Pro|Flash)/),
    ).toHaveLength(4);
    expect(
      screen.getByText(/Map the Sonnet, Opus, Fable, and Haiku roles/),
    ).toBeVisible();
  });

  it("keeps proxy routes empty until default routes are available", () => {
    // The mock returns no defaults. Rendering four blank rows here would
    // prevent the seed effect from filling them after defaults arrive.
    renderForm({
      name: "Proxy Provider",
      settingsConfig: {
        env: {
          ANTHROPIC_BASE_URL: "https://api.example.com",
          ANTHROPIC_AUTH_TOKEN: "sk-test",
        },
      },
      meta: {
        claudeDesktopMode: "proxy",
        claudeDesktopModelRoutes: {},
      },
    });

    expect(screen.queryAllByPlaceholderText("DeepSeek V4 Pro")).toHaveLength(0);
  });

  it("fills blank proxy roles with the Sonnet model when saving", async () => {
    const onSubmit = vi.fn();
    renderForm(
      {
        name: "Proxy Provider",
        settingsConfig: {
          env: {
            ANTHROPIC_BASE_URL: "https://api.example.com",
            ANTHROPIC_AUTH_TOKEN: "sk-test",
          },
        },
        meta: {
          claudeDesktopMode: "proxy",
          claudeDesktopModelRoutes: {
            "claude-old": {
              model: "upstream-old",
            },
          },
        },
      },
      onSubmit,
    );

    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalled());
    const submitted = onSubmit.mock.calls[0][0];
    // The legacy route becomes Sonnet; blank roles inherit its upstream model.
    expect(submitted.meta.claudeDesktopModelRoutes).toMatchObject({
      "claude-sonnet-5": {
        model: "upstream-old",
        labelOverride: "upstream-old",
      },
      "claude-opus-4-8": { model: "upstream-old" },
      "claude-fable-5": { model: "upstream-old" },
      "claude-haiku-4-5": { model: "upstream-old" },
    });
    expect(Object.keys(submitted.meta.claudeDesktopModelRoutes).sort()).toEqual(
      [
        "claude-fable-5",
        "claude-haiku-4-5",
        "claude-opus-4-8",
        "claude-sonnet-5",
      ],
    );
  });

  it("inherits Sonnet 1M support when filling blank roles", async () => {
    const onSubmit = vi.fn();
    renderForm(
      {
        name: "Proxy Provider",
        settingsConfig: {
          env: {
            ANTHROPIC_BASE_URL: "https://api.example.com",
            ANTHROPIC_AUTH_TOKEN: "sk-test",
          },
        },
        meta: {
          claudeDesktopMode: "proxy",
          claudeDesktopModelRoutes: {
            "claude-sonnet-4-6": { model: "deepseek-v4-pro", supports1m: true },
          },
        },
      },
      onSubmit,
    );

    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalled());
    const routes = onSubmit.mock.calls[0][0].meta.claudeDesktopModelRoutes;
    // Blank roles inherit both the upstream model and its 1M capability.
    expect(routes["claude-sonnet-5"]).toMatchObject({
      model: "deepseek-v4-pro",
      supports1m: true,
    });
    expect(routes["claude-opus-4-8"]).toMatchObject({
      model: "deepseek-v4-pro",
      supports1m: true,
    });
    expect(routes["claude-haiku-4-5"]).toMatchObject({
      model: "deepseek-v4-pro",
      supports1m: true,
    });
  });

  it("does not preserve a stale direct route as a hidden mapping target", async () => {
    const onSubmit = vi.fn();
    renderForm(
      {
        name: "Direct Provider",
        settingsConfig: {
          env: {
            ANTHROPIC_BASE_URL: "https://api.example.com",
            ANTHROPIC_AUTH_TOKEN: "sk-test",
          },
        },
        meta: {
          claudeDesktopMode: "direct",
          claudeDesktopModelRoutes: {
            "claude-old": {
              model: "claude-old",
            },
          },
        },
      },
      onSubmit,
    );

    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalled());
    const submitted = onSubmit.mock.calls[0][0];
    expect(submitted.meta.claudeDesktopModelRoutes).toMatchObject({
      "claude-sonnet-5": {
        model: "claude-sonnet-5",
      },
    });
  });
});
