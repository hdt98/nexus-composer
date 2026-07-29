import { renderHook, act } from "@testing-library/react";
import { describe, it, expect, beforeEach, vi } from "vitest";
import { useSettings } from "@/hooks/useSettings";
import type { Settings } from "@/types";

const mutateAsyncMock = vi.fn();
const useSettingsQueryMock = vi.fn();
const setAppConfigDirOverrideMock = vi.fn();
const applyClaudePluginConfigMock = vi.fn();
const applyClaudeOnboardingSkipMock = vi.fn();
const clearClaudeOnboardingSkipMock = vi.fn();
const syncCurrentProvidersLiveMock = vi.fn();
const updateTrayMenuMock = vi.fn();
const getCurrentMock = vi.fn();
const getAllMock = vi.fn();
const getQueryDataMock = vi.fn();
const toastErrorMock = vi.fn();
const toastSuccessMock = vi.fn();

let settingsFormMock: any;
let directorySettingsMock: any;
let metadataMock: any;
let serverSettings: Settings;

vi.mock("sonner", () => ({
  toast: {
    error: (...args: unknown[]) => toastErrorMock(...args),
    success: (...args: unknown[]) => toastSuccessMock(...args),
  },
}));

vi.mock("@/hooks/useSettingsForm", () => ({
  useSettingsForm: () => settingsFormMock,
}));

vi.mock("@/hooks/useDirectorySettings", () => ({
  useDirectorySettings: () => directorySettingsMock,
}));

vi.mock("@/hooks/useSettingsMetadata", () => ({
  useSettingsMetadata: () => metadataMock,
}));

vi.mock("@/lib/query", () => ({
  useSettingsQuery: (...args: unknown[]) => useSettingsQueryMock(...args),
  useSaveSettingsMutation: () => ({
    mutateAsync: mutateAsyncMock,
    isPending: false,
  }),
}));

vi.mock("@tanstack/react-query", async () => {
  const actual = await vi.importActual<typeof import("@tanstack/react-query")>(
    "@tanstack/react-query",
  );
  return {
    ...actual,
    useQueryClient: () => ({
      getQueryData: (...args: unknown[]) => getQueryDataMock(...args),
    }),
  };
});

vi.mock("@/lib/api", () => ({
  settingsApi: {
    setAppConfigDirOverride: (...args: unknown[]) =>
      setAppConfigDirOverrideMock(...args),
    applyClaudePluginConfig: (...args: unknown[]) =>
      applyClaudePluginConfigMock(...args),
    applyClaudeOnboardingSkip: (...args: unknown[]) =>
      applyClaudeOnboardingSkipMock(...args),
    clearClaudeOnboardingSkip: (...args: unknown[]) =>
      clearClaudeOnboardingSkipMock(...args),
    syncCurrentProvidersLive: (...args: unknown[]) =>
      syncCurrentProvidersLiveMock(...args),
  },
  providersApi: {
    updateTrayMenu: (...args: unknown[]) => updateTrayMenuMock(...args),
    getCurrent: (...args: unknown[]) => getCurrentMock(...args),
    getAll: (...args: unknown[]) => getAllMock(...args),
  },
}));

const createSettingsFormMock = (overrides: Record<string, unknown> = {}) => ({
  settings: {
    showInTray: true,
    minimizeToTrayOnClose: true,
    enableClaudePluginIntegration: false,
    skipClaudeOnboarding: true,
    claudeConfigDir: "/claude",
    codexConfigDir: "/codex",
    geminiConfigDir: "/gemini",
    opencodeConfigDir: "/opencode",
    openclawConfigDir: "/openclaw",
    language: "en",
  },
  isLoading: false,
  initialLanguage: "en",
  updateSettings: vi.fn(),
  resetSettings: vi.fn(),
  syncLanguage: vi.fn(),
  ...overrides,
});

const createDirectorySettingsMock = (
  overrides: Record<string, unknown> = {},
) => ({
  appConfigDir: undefined,
  resolvedDirs: {
    appConfig: "/home/mock/.cc-switch",
    claude: "/default/claude",
    codex: "/default/codex",
    gemini: "/default/gemini",
    opencode: "/default/opencode",
    openclaw: "/default/openclaw",
  },
  isLoading: false,
  initialAppConfigDir: undefined,
  updateDirectory: vi.fn(),
  updateAppConfigDir: vi.fn(),
  browseDirectory: vi.fn(),
  browseAppConfigDir: vi.fn(),
  resetDirectory: vi.fn(),
  resetAppConfigDir: vi.fn(),
  resetAllDirectories: vi.fn(),
  ...overrides,
});

const createMetadataMock = (overrides: Record<string, unknown> = {}) => ({
  isPortable: false,
  requiresRestart: false,
  isLoading: false,
  acknowledgeRestart: vi.fn(),
  setRequiresRestart: vi.fn(),
  ...overrides,
});

describe("useSettings hook", () => {
  beforeEach(() => {
    mutateAsyncMock.mockReset();
    useSettingsQueryMock.mockReset();
    setAppConfigDirOverrideMock.mockReset();
    applyClaudePluginConfigMock.mockReset();
    applyClaudeOnboardingSkipMock.mockReset();
    clearClaudeOnboardingSkipMock.mockReset();
    syncCurrentProvidersLiveMock.mockReset();
    getCurrentMock.mockReset();
    getAllMock.mockReset();
    getQueryDataMock.mockReset();
    toastErrorMock.mockReset();
    toastSuccessMock.mockReset();
    window.localStorage.clear();

    serverSettings = {
      showInTray: true,
      minimizeToTrayOnClose: true,
      enableClaudePluginIntegration: false,
      skipClaudeOnboarding: true,
      claudeConfigDir: "/server/claude",
      codexConfigDir: "/server/codex",
      geminiConfigDir: "/server/gemini",
      opencodeConfigDir: "/server/opencode",
      openclawConfigDir: "/server/openclaw",
      language: "en",
    };

    useSettingsQueryMock.mockReturnValue({
      data: serverSettings,
      isLoading: false,
    });

    settingsFormMock = createSettingsFormMock({
      settings: {
        ...serverSettings,
        language: "en",
      },
    });
    directorySettingsMock = createDirectorySettingsMock();
    metadataMock = createMetadataMock();

    mutateAsyncMock.mockResolvedValue(true);
    setAppConfigDirOverrideMock.mockResolvedValue(true);
    applyClaudePluginConfigMock.mockResolvedValue(true);
    applyClaudeOnboardingSkipMock.mockResolvedValue(true);
    clearClaudeOnboardingSkipMock.mockResolvedValue(true);
    syncCurrentProvidersLiveMock.mockResolvedValue({ ok: true });
    getCurrentMock.mockResolvedValue(null);
    getAllMock.mockResolvedValue({});
    // Keep the query client cache aligned with serverSettings by default so existing "prev === data" assertions retain their meaning.
    getQueryDataMock.mockImplementation(() => serverSettings);
  });

  it("auto-saves and applies Claude onboarding skip when toggled on", async () => {
    serverSettings = {
      ...serverSettings,
      skipClaudeOnboarding: false,
    };
    useSettingsQueryMock.mockReturnValue({
      data: serverSettings,
      isLoading: false,
    });

    settingsFormMock = createSettingsFormMock({
      settings: {
        ...serverSettings,
        language: "en",
        skipClaudeOnboarding: false,
      },
    });

    const { result } = renderHook(() => useSettings());

    await act(async () => {
      await result.current.autoSaveSettings({ skipClaudeOnboarding: true });
    });

    expect(applyClaudeOnboardingSkipMock).toHaveBeenCalledTimes(1);
    expect(toastErrorMock).not.toHaveBeenCalled();
  });

  it("auto-saves and clears Claude onboarding skip when toggled off", async () => {
    serverSettings = {
      ...serverSettings,
      skipClaudeOnboarding: true,
    };
    useSettingsQueryMock.mockReturnValue({
      data: serverSettings,
      isLoading: false,
    });

    settingsFormMock = createSettingsFormMock({
      settings: {
        ...serverSettings,
        language: "en",
        skipClaudeOnboarding: true,
      },
    });

    const { result } = renderHook(() => useSettings());

    await act(async () => {
      await result.current.autoSaveSettings({ skipClaudeOnboarding: false });
    });

    expect(clearClaudeOnboardingSkipMock).toHaveBeenCalledTimes(1);
    expect(toastErrorMock).not.toHaveBeenCalled();
  });

  it("saves settings and flags restart when app config directory changes", async () => {
    serverSettings = {
      ...serverSettings,
      enableClaudePluginIntegration: false,
      claudeConfigDir: "/server/claude",
      codexConfigDir: undefined,
      geminiConfigDir: "/server/gemini",
      opencodeConfigDir: "/server/opencode",
      openclawConfigDir: "/server/openclaw",
      language: "en",
    };
    useSettingsQueryMock.mockReturnValue({
      data: serverSettings,
      isLoading: false,
    });

    settingsFormMock = createSettingsFormMock({
      settings: {
        ...serverSettings,
        claudeConfigDir: "  /custom/claude  ",
        codexConfigDir: "   ",
        openclawConfigDir: "  /custom/openclaw  ",
        language: "en",
        enableClaudePluginIntegration: true, // State changes from false to true.
      },
      initialLanguage: "en",
    });

    directorySettingsMock = createDirectorySettingsMock({
      appConfigDir: "  /override/app  ",
      initialAppConfigDir: "/previous/app",
    });

    const { result } = renderHook(() => useSettings());

    let saveResult: { requiresRestart: boolean } | null = null;
    await act(async () => {
      saveResult = await result.current.saveSettings();
    });

    expect(saveResult).toEqual({ requiresRestart: true });
    expect(mutateAsyncMock).toHaveBeenCalledTimes(1);
    const payload = mutateAsyncMock.mock.calls[0][0] as Settings;
    expect(payload.claudeConfigDir).toBe("/custom/claude");
    expect(payload.codexConfigDir).toBeUndefined();
    expect(payload.openclawConfigDir).toBe("/custom/openclaw");
    expect(payload.language).toBe("en");
    expect(setAppConfigDirOverrideMock).toHaveBeenCalledWith("/override/app");
    // The state changed, so the API should be called.
    expect(applyClaudePluginConfigMock).toHaveBeenCalledWith({
      official: false,
    });
    expect(metadataMock.setRequiresRestart).toHaveBeenCalledWith(true);
    expect(window.localStorage.getItem("language")).toBe("en");
    expect(toastErrorMock).not.toHaveBeenCalled();
    // Plugin synchronization already includes syncCurrentProvidersLiveSafe, so the directory change must not call it again.
    expect(syncCurrentProvidersLiveMock).toHaveBeenCalledTimes(1);
  });

  it("saves settings without restart when directory unchanged", async () => {
    // Keep server and local state aligned so no API call is triggered.
    serverSettings = {
      ...serverSettings,
      enableClaudePluginIntegration: false,
      launchOnStartup: false,
    };
    useSettingsQueryMock.mockReturnValue({
      data: serverSettings,
      isLoading: false,
    });

    settingsFormMock = createSettingsFormMock({
      settings: {
        ...serverSettings,
        enableClaudePluginIntegration: false, // State is unchanged.
        launchOnStartup: false, // State is unchanged.
        language: "en",
      },
      initialLanguage: "en",
    });

    directorySettingsMock = createDirectorySettingsMock({
      appConfigDir: undefined,
      initialAppConfigDir: undefined,
    });

    const { result } = renderHook(() => useSettings());

    let saveResult: { requiresRestart: boolean } | null = null;
    await act(async () => {
      saveResult = await result.current.saveSettings();
    });

    expect(saveResult).toEqual({ requiresRestart: false });
    expect(setAppConfigDirOverrideMock).toHaveBeenCalledWith(null);
    // The state did not change, so the API should not be called.
    expect(applyClaudePluginConfigMock).not.toHaveBeenCalled();
    expect(metadataMock.setRequiresRestart).toHaveBeenCalledWith(false);
    // The directory did not change, so synchronization should not run.
    expect(syncCurrentProvidersLiveMock).not.toHaveBeenCalled();
  });

  it("shows toast when Claude plugin sync fails but continues flow", async () => {
    // Set server state to false and local state to true to trigger a state change.
    serverSettings = {
      ...serverSettings,
      enableClaudePluginIntegration: false,
    };
    useSettingsQueryMock.mockReturnValue({
      data: serverSettings,
      isLoading: false,
    });

    settingsFormMock = createSettingsFormMock({
      settings: {
        ...serverSettings,
        enableClaudePluginIntegration: true, // State changed.
        language: "en",
      },
    });
    directorySettingsMock = createDirectorySettingsMock({
      appConfigDir: "/override/app",
      initialAppConfigDir: "/prior/app",
    });

    applyClaudePluginConfigMock.mockRejectedValueOnce(new Error("sync failed"));

    const { result } = renderHook(() => useSettings());

    await act(async () => {
      await result.current.saveSettings();
    });

    expect(toastErrorMock).toHaveBeenCalled();
    const message = toastErrorMock.mock.calls.at(-1)?.[0] as string;
    expect(message).toContain("Sync Claude plugin failed");
    expect(metadataMock.setRequiresRestart).toHaveBeenCalledWith(true);
  });

  it("detects plugin toggle via live cache even when closure data is stale", async () => {
    // Simulate a rapid-toggle race: useSettingsQueryMock data remains stale at false because the closure has not updated,
    // while the live query client cache is true after the previous enabled state was persisted.
    // The user wants to switch the form back to false. The old implementation skipped the side effect because data === form;
    // the current implementation must read prev=true and run it.
    serverSettings = {
      ...serverSettings,
      enableClaudePluginIntegration: false,
    };
    useSettingsQueryMock.mockReturnValue({
      data: serverSettings,
      isLoading: false,
    });

    settingsFormMock = createSettingsFormMock({
      settings: {
        ...serverSettings,
        enableClaudePluginIntegration: false,
        language: "en",
      },
    });
    directorySettingsMock = createDirectorySettingsMock();

    // The actual previous cached value is true, while closure data is still false.
    getQueryDataMock.mockImplementation(() => ({
      ...serverSettings,
      enableClaudePluginIntegration: true,
    }));

    const { result } = renderHook(() => useSettings());

    await act(async () => {
      await result.current.saveSettings(undefined, { silent: true });
    });

    // The live cached value is true and the payload is false, so the difference triggers clear_claude_config.
    expect(applyClaudePluginConfigMock).toHaveBeenCalledWith({
      official: true,
    });
    expect(syncCurrentProvidersLiveMock).toHaveBeenCalled();
  });

  it("resets form, language and directories using server data", () => {
    serverSettings = {
      ...serverSettings,
      claudeConfigDir: "  /server/claude  ",
      codexConfigDir: "   ",
      language: "en",
    };
    useSettingsQueryMock.mockReturnValue({
      data: serverSettings,
      isLoading: false,
    });

    settingsFormMock = createSettingsFormMock({
      settings: {
        ...serverSettings,
        language: "en",
      },
      initialLanguage: "en",
    });
    directorySettingsMock = createDirectorySettingsMock();

    const { result } = renderHook(() => useSettings());

    act(() => {
      result.current.resetSettings();
    });

    expect(settingsFormMock.resetSettings).toHaveBeenCalledWith(serverSettings);
    expect(settingsFormMock.syncLanguage).toHaveBeenCalledWith(
      settingsFormMock.initialLanguage,
    );
    expect(directorySettingsMock.resetAllDirectories).toHaveBeenCalledWith({
      claude: "/server/claude",
      codex: undefined,
      gemini: "/server/gemini",
      opencode: "/server/opencode",
      openclaw: "/server/openclaw",
      hermes: undefined,
    });
    expect(metadataMock.setRequiresRestart).toHaveBeenCalledWith(false);
  });

  it("returns null immediately when settings state is missing", async () => {
    settingsFormMock = createSettingsFormMock({
      settings: null,
    });

    const { result } = renderHook(() => useSettings());

    let resultValue: { requiresRestart: boolean } | null = null;
    await act(async () => {
      resultValue = await result.current.saveSettings();
    });

    expect(resultValue).toBeNull();
    expect(mutateAsyncMock).not.toHaveBeenCalled();
    expect(setAppConfigDirOverrideMock).not.toHaveBeenCalled();
  });

  it("throws when save mutation rejects and keeps restart flag untouched", async () => {
    settingsFormMock = createSettingsFormMock();
    directorySettingsMock = createDirectorySettingsMock({
      appConfigDir: "/override/app",
      initialAppConfigDir: "/override/app",
    });
    const rejection = new Error("save failed");
    mutateAsyncMock.mockRejectedValueOnce(rejection);

    const { result } = renderHook(() => useSettings());

    await expect(
      act(async () => {
        await result.current.saveSettings();
      }),
    ).rejects.toThrow("save failed");

    expect(setAppConfigDirOverrideMock).not.toHaveBeenCalled();
    expect(metadataMock.setRequiresRestart).not.toHaveBeenCalledWith(true);
  });
});
