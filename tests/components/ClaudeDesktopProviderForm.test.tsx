import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClientProvider } from "@tanstack/react-query";
import type { ComponentProps } from "react";
import { describe, expect, it, vi } from "vitest";
import { ClaudeDesktopProviderForm } from "@/components/providers/forms/ClaudeDesktopProviderForm";
import { claudeDesktopProviderPresets } from "@/config/claudeDesktopProviderPresets";
import {
  NEXUS_ENDPOINT,
  NEXUS_MANAGED_PRESET_VERSION,
  NEXUS_MAX_OUTPUT_TOKENS,
  NEXUS_MODEL,
  NEXUS_TEXT_MODEL_CATALOG,
} from "@/config/nexus";
import { createTestQueryClient } from "../utils/testQueryClient";

vi.mock("@/lib/api/providers", () => ({
  providersApi: {
    getClaudeDesktopDefaultRoutes: () => Promise.resolve([]),
  },
}));

function renderForm(
  initialData: ComponentProps<typeof ClaudeDesktopProviderForm>["initialData"],
  onSubmit = vi.fn(),
) {
  const queryClient = createTestQueryClient();
  const view = render(
    <QueryClientProvider client={queryClient}>
      <ClaudeDesktopProviderForm
        submitLabel="保存"
        onSubmit={onSubmit}
        onCancel={vi.fn()}
        initialData={initialData}
      />
    </QueryClientProvider>,
  );
  return { ...view, onSubmit };
}

const managedDesktopData: ComponentProps<
  typeof ClaudeDesktopProviderForm
>["initialData"] = {
  name: "Nexus GLM-5.2",
  category: "third_party",
  settingsConfig: {
    env: {
      ANTHROPIC_BASE_URL: NEXUS_ENDPOINT,
      ANTHROPIC_AUTH_TOKEN: "old-key",
    },
    modelCatalog: {
      customMetadata: "keep-me",
      models: [
        ...NEXUS_TEXT_MODEL_CATALOG.models,
        { model: `${NEXUS_MODEL}[1m]`, inputModalities: ["text"] },
        { model: "glm-5.2[1m]", inputModalities: ["text"] },
        { model: NEXUS_MODEL, role: "custom", keep: true },
        { model: "custom-model", inputModalities: ["text", "image"] },
      ],
    },
  },
  meta: {
    claudeDesktopMode: "proxy",
    apiFormat: "openai_chat",
    providerType: "nexus",
    managedNexusPresetVersion: NEXUS_MANAGED_PRESET_VERSION,
    localProxyRequestOverrides: {
      headers: { "x-custom": "keep-me" },
      body: {
        max_tokens: NEXUS_MAX_OUTPUT_TOKENS,
        temperature: 0.2,
        chat_template_kwargs: { enable_thinking: true, custom: "keep-me" },
      },
    },
    claudeDesktopModelRoutes: {
      "claude-sonnet-5": { model: NEXUS_MODEL, supports1m: true },
      "claude-opus-4-8": { model: NEXUS_MODEL, supports1m: true },
      "claude-fable-5": { model: NEXUS_MODEL, supports1m: true },
      "claude-haiku-4-5": { model: NEXUS_MODEL, supports1m: true },
    },
  },
};

describe("ClaudeDesktopProviderForm", () => {
  it("persists the managed Nexus catalog and request override", async () => {
    const onSubmit = vi.fn();
    renderForm(undefined, onSubmit);
    const preset = claudeDesktopProviderPresets.find(
      (candidate) => candidate.providerType === "nexus",
    )!;

    fireEvent.click(
      screen.getByRole("button", { name: new RegExp(preset.name) }),
    );
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "test-key" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
    const submitted = onSubmit.mock.calls[0][0];
    expect(JSON.parse(submitted.settingsConfig).modelCatalog).toEqual(
      NEXUS_TEXT_MODEL_CATALOG,
    );
    expect(submitted.meta).toMatchObject({
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

  it("clears managed Nexus metadata when another preset replaces it", async () => {
    const onSubmit = vi.fn();
    renderForm(undefined, onSubmit);

    fireEvent.click(screen.getByRole("button", { name: /Nexus GLM-5\.2/i }));
    fireEvent.click(
      screen.getByRole("button", { name: /Claude Desktop Official/i }),
    );
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
    expect(onSubmit.mock.calls[0][0].meta).not.toHaveProperty(
      "managedNexusPresetVersion",
    );
    expect(onSubmit.mock.calls[0][0].meta).not.toHaveProperty(
      "localProxyRequestOverrides",
    );
    expect(onSubmit.mock.calls[0][0].meta).not.toHaveProperty("providerType");
    expect(
      JSON.parse(onSubmit.mock.calls[0][0].settingsConfig),
    ).not.toHaveProperty("modelCatalog");
  });

  it("keeps managed Nexus metadata when only the Desktop credential changes", async () => {
    const { onSubmit } = renderForm(managedDesktopData);

    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "new-key" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
    expect(onSubmit.mock.calls[0][0].meta).toMatchObject({
      providerType: "nexus",
      managedNexusPresetVersion: NEXUS_MANAGED_PRESET_VERSION,
      localProxyRequestOverrides:
        managedDesktopData?.meta?.localProxyRequestOverrides,
    });
  });

  it("detaches managed metadata and only its catalog on Desktop route edit", async () => {
    const { onSubmit } = renderForm(managedDesktopData);

    fireEvent.change(screen.getAllByPlaceholderText("deepseek-v4-pro")[0], {
      target: { value: "custom-model" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

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

  it("编辑模型映射的菜单显示名时保持输入框焦点", () => {
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

    // 固定四档（Sonnet / Opus / Fable / Haiku）下有四个菜单显示名输入，取 Sonnet（首个）。
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

  it("编辑直连模型列表的模型 ID 时保持输入框焦点", () => {
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

  it("代理模式始终渲染 Sonnet / Opus / Fable / Haiku 四档（即使只配了一档）", () => {
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

    // 固定四档：每档各一个「菜单显示名」输入框，无论初始只配了几档。
    // Haiku 档的占位示例是 "DeepSeek V4 Flash"、其余三档是 "DeepSeek V4 Pro"
    // （见组件的 role-consistent 占位逻辑），故用正则同时匹配两种占位、数满四档。
    expect(
      screen.getAllByPlaceholderText(/DeepSeek V4 (Pro|Flash)/),
    ).toHaveLength(4);
  });

  it("代理模式初始无路由且默认路由未就绪时不渲染空四档", () => {
    // mock 的 getClaudeDesktopDefaultRoutes 返回 []，模拟默认路由尚未就绪。
    // 修复前：normalizeProxyRows([]) 会渲染空行并把 routes.length 撑起来，
    // 永久挡住 seed effect 的默认路由回填。修复后应保持空、等待 seed。
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

  it("保存模型映射时补齐固定四档并把留空档回填为 Sonnet 模型", async () => {
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

    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalled());
    const submitted = onSubmit.mock.calls[0][0];
    // claude-old 迁移到 Sonnet；留空的 Opus / Fable / Haiku 回填为 Sonnet 的
    // 上游模型，保证落库四档齐全，子 agent 调用的各档始终可解析。
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

  it("回填空档时继承 Sonnet 的 1M 声明", async () => {
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

    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalled());
    const routes = onSubmit.mock.calls[0][0].meta.claudeDesktopModelRoutes;
    // 留空的 Opus / Haiku 回填同一上游模型，1M 声明应与 Sonnet 一致。
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

  it("保存直连模型列表时不会保留旧 route 作为隐藏映射目标", async () => {
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

    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(onSubmit).toHaveBeenCalled());
    const submitted = onSubmit.mock.calls[0][0];
    expect(submitted.meta.claudeDesktopModelRoutes).toMatchObject({
      "claude-sonnet-5": {
        model: "claude-sonnet-5",
      },
    });
  });
});
