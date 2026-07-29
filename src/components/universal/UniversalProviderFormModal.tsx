import { useState, useEffect, useCallback, useMemo } from "react";
import { useDarkMode } from "@/hooks/useDarkMode";
import { useTranslation } from "react-i18next";
import { Eye, EyeOff, RefreshCw } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { FullScreenPanel } from "@/components/common/FullScreenPanel";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ProviderIcon } from "@/components/ProviderIcon";
import JsonEditor from "@/components/JsonEditor";
import type { UniversalProvider, UniversalProviderModels } from "@/types";
import {
  universalProviderPresets,
  createUniversalProviderFromPreset,
  type UniversalProviderPreset,
} from "@/config/universalProviderPresets";
import { deepClone } from "@/utils/deepClone";

interface UniversalProviderFormModalProps {
  isOpen: boolean;
  onClose: () => void;
  onSave: (provider: UniversalProvider) => void;
  onSaveAndSync?: (provider: UniversalProvider) => void;
  editingProvider?: UniversalProvider | null;
  initialPreset?: UniversalProviderPreset | null;
}

export function UniversalProviderFormModal({
  isOpen,
  onClose,
  onSave,
  onSaveAndSync,
  editingProvider,
  initialPreset,
}: UniversalProviderFormModalProps) {
  const isDarkMode = useDarkMode();
  const { t } = useTranslation();
  const isEditMode = !!editingProvider;

  // Form state.
  const [selectedPreset, setSelectedPreset] =
    useState<UniversalProviderPreset | null>(null);
  const [name, setName] = useState("");
  const [baseUrl, setBaseUrl] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [showApiKey, setShowApiKey] = useState(false);
  const [websiteUrl, setWebsiteUrl] = useState("");
  const [notes, setNotes] = useState("");

  // Per-app enabled state.
  const [claudeEnabled, setClaudeEnabled] = useState(true);
  const [codexEnabled, setCodexEnabled] = useState(true);
  const [geminiEnabled, setGeminiEnabled] = useState(true);

  // Model configuration.
  const [models, setModels] = useState<UniversalProviderModels>({});

  // Save-and-sync confirmation dialog.
  const [syncConfirmOpen, setSyncConfirmOpen] = useState(false);
  const [pendingProvider, setPendingProvider] =
    useState<UniversalProvider | null>(null);

  // Initialize the form.
  useEffect(() => {
    if (editingProvider) {
      // Edit mode: load existing data.
      setName(editingProvider.name);
      setBaseUrl(editingProvider.baseUrl);
      setApiKey(editingProvider.apiKey);
      setWebsiteUrl(editingProvider.websiteUrl || "");
      setNotes(editingProvider.notes || "");
      setClaudeEnabled(editingProvider.apps.claude);
      setCodexEnabled(editingProvider.apps.codex);
      setGeminiEnabled(editingProvider.apps.gemini);
      setModels(editingProvider.models || {});

      // Try to match an existing preset.
      const preset = universalProviderPresets.find(
        (p) => p.providerType === editingProvider.providerType,
      );
      setSelectedPreset(preset || null);
    } else {
      // Create mode: use the supplied preset or default to the first preset.
      const defaultPreset = initialPreset || universalProviderPresets[0];
      setSelectedPreset(defaultPreset);
      setName(defaultPreset.name);
      setBaseUrl("");
      setApiKey("");
      setWebsiteUrl(defaultPreset.websiteUrl || "");
      setNotes("");
      setClaudeEnabled(defaultPreset.defaultApps.claude);
      setCodexEnabled(defaultPreset.defaultApps.codex);
      setGeminiEnabled(defaultPreset.defaultApps.gemini);
      setModels(deepClone(defaultPreset.defaultModels));
    }
  }, [editingProvider, initialPreset, isOpen]);

  // Select a preset.
  const handlePresetSelect = useCallback(
    (preset: UniversalProviderPreset) => {
      setSelectedPreset(preset);
      if (!isEditMode) {
        setName(preset.name);
        setClaudeEnabled(preset.defaultApps.claude);
        setCodexEnabled(preset.defaultApps.codex);
        setGeminiEnabled(preset.defaultApps.gemini);
        setModels(deepClone(preset.defaultModels));
      }
    },
    [isEditMode],
  );

  // Update model configuration.
  const updateModel = useCallback(
    (app: "claude" | "codex" | "gemini", field: string, value: string) => {
      setModels((prev) => ({
        ...prev,
        [app]: {
          ...(prev[app] || {}),
          [field]: value,
        },
      }));
    },
    [],
  );

  // Build the Claude configuration JSON preview.
  const claudeConfigJson = useMemo(() => {
    if (!claudeEnabled) return null;
    const model = models.claude?.model || "claude-sonnet-4-20250514";
    const haiku = models.claude?.haikuModel || "claude-haiku-4-20250514";
    const sonnet = models.claude?.sonnetModel || "claude-sonnet-4-20250514";
    const opus = models.claude?.opusModel || "claude-sonnet-4-20250514";
    return {
      env: {
        ANTHROPIC_BASE_URL: baseUrl,
        ANTHROPIC_AUTH_TOKEN: apiKey,
        ANTHROPIC_MODEL: model,
        ANTHROPIC_DEFAULT_HAIKU_MODEL: haiku,
        ANTHROPIC_DEFAULT_SONNET_MODEL: sonnet,
        ANTHROPIC_DEFAULT_OPUS_MODEL: opus,
      },
    };
  }, [claudeEnabled, baseUrl, apiKey, models.claude]);

  // Build the Codex configuration JSON preview.
  const codexConfigJson = useMemo(() => {
    if (!codexEnabled) return null;
    const model = models.codex?.model || "gpt-5.5";
    const reasoningEffort = models.codex?.reasoningEffort || "high";
    // Ensure base_url ends in /v1 because Codex uses the OpenAI-compatible API.
    const codexBaseUrl = baseUrl.endsWith("/v1")
      ? baseUrl
      : `${baseUrl.replace(/\/+$/, "")}/v1`;
    const configToml = `model_provider = "custom"
model = "${model}"
model_reasoning_effort = "${reasoningEffort}"
disable_response_storage = true

[model_providers.custom]
name = "NewAPI"
base_url = "${codexBaseUrl}"
wire_api = "responses"
requires_openai_auth = true`;
    return {
      auth: {
        OPENAI_API_KEY: apiKey,
      },
      config: configToml,
    };
  }, [codexEnabled, baseUrl, apiKey, models.codex]);

  // Build the Gemini configuration JSON preview.
  const geminiConfigJson = useMemo(() => {
    if (!geminiEnabled) return null;
    const model = models.gemini?.model || "gemini-2.5-pro";
    return {
      env: {
        GOOGLE_GEMINI_BASE_URL: baseUrl,
        GEMINI_API_KEY: apiKey,
        GEMINI_MODEL: model,
      },
    };
  }, [geminiEnabled, baseUrl, apiKey, models.gemini]);

  // Submit the form.
  const handleSubmit = useCallback(() => {
    if (!name.trim() || !baseUrl.trim() || !apiKey.trim()) {
      return;
    }

    const provider: UniversalProvider = editingProvider
      ? {
          ...editingProvider,
          name: name.trim(),
          baseUrl: baseUrl.trim(),
          apiKey: apiKey.trim(),
          websiteUrl: websiteUrl.trim() || undefined,
          notes: notes.trim() || undefined,
          apps: {
            claude: claudeEnabled,
            codex: codexEnabled,
            gemini: geminiEnabled,
          },
          models,
        }
      : createUniversalProviderFromPreset(
          selectedPreset || universalProviderPresets[0],
          crypto.randomUUID(),
          baseUrl.trim(),
          apiKey.trim(),
          name.trim(),
        );

    // In create mode, update enabled apps and models.
    if (!editingProvider) {
      provider.apps = {
        claude: claudeEnabled,
        codex: codexEnabled,
        gemini: geminiEnabled,
      };
      provider.models = models;
      provider.websiteUrl = websiteUrl.trim() || undefined;
      provider.notes = notes.trim() || undefined;
    }

    onSave(provider);
    onClose();
  }, [
    editingProvider,
    name,
    baseUrl,
    apiKey,
    websiteUrl,
    notes,
    claudeEnabled,
    codexEnabled,
    geminiEnabled,
    models,
    selectedPreset,
    onSave,
    onClose,
  ]);

  // Build the provider object.
  const buildProvider = useCallback((): UniversalProvider | null => {
    if (!name.trim() || !baseUrl.trim() || !apiKey.trim()) {
      return null;
    }

    const provider: UniversalProvider = editingProvider
      ? {
          ...editingProvider,
          name: name.trim(),
          baseUrl: baseUrl.trim(),
          apiKey: apiKey.trim(),
          websiteUrl: websiteUrl.trim() || undefined,
          notes: notes.trim() || undefined,
          apps: {
            claude: claudeEnabled,
            codex: codexEnabled,
            gemini: geminiEnabled,
          },
          models,
        }
      : createUniversalProviderFromPreset(
          selectedPreset || universalProviderPresets[0],
          crypto.randomUUID(),
          baseUrl.trim(),
          apiKey.trim(),
          name.trim(),
        );

    // In create mode, update enabled apps and models.
    if (!editingProvider) {
      provider.apps = {
        claude: claudeEnabled,
        codex: codexEnabled,
        gemini: geminiEnabled,
      };
      provider.models = models;
      provider.websiteUrl = websiteUrl.trim() || undefined;
      provider.notes = notes.trim() || undefined;
    }

    return provider;
  }, [
    editingProvider,
    name,
    baseUrl,
    apiKey,
    websiteUrl,
    notes,
    claudeEnabled,
    codexEnabled,
    geminiEnabled,
    models,
    selectedPreset,
  ]);

  // Open the save-and-sync confirmation dialog.
  const handleSaveAndSyncClick = useCallback(() => {
    const provider = buildProvider();
    if (!provider || !onSaveAndSync) return;

    setPendingProvider(provider);
    setSyncConfirmOpen(true);
  }, [buildProvider, onSaveAndSync]);

  // Confirm save and sync.
  const confirmSaveAndSync = useCallback(() => {
    if (!pendingProvider || !onSaveAndSync) return;

    onSaveAndSync(pendingProvider);
    setSyncConfirmOpen(false);
    setPendingProvider(null);
    onClose();
  }, [pendingProvider, onSaveAndSync, onClose]);

  const footer = (
    <>
      <Button variant="outline" onClick={onClose}>
        {t("common.cancel", { defaultValue: "Cancel" })}
      </Button>
      {isEditMode && onSaveAndSync ? (
        <Button
          onClick={handleSaveAndSyncClick}
          disabled={!name.trim() || !baseUrl.trim() || !apiKey.trim()}
        >
          <RefreshCw className="mr-1.5 h-4 w-4" />
          {t("universalProvider.saveAndSync", { defaultValue: "Save & Sync" })}
        </Button>
      ) : (
        <Button
          onClick={handleSubmit}
          disabled={!name.trim() || !baseUrl.trim() || !apiKey.trim()}
        >
          {t("common.add", { defaultValue: "Add" })}
        </Button>
      )}
    </>
  );

  return (
    <FullScreenPanel
      isOpen={isOpen}
      title={
        isEditMode
          ? t("universalProvider.edit", {
              defaultValue: "Edit Universal Provider",
            })
          : t("universalProvider.add", {
              defaultValue: "Add Universal Provider",
            })
      }
      onClose={onClose}
      footer={footer}
    >
      <div className="space-y-6">
        {/* Preset selection, available only in create mode. */}
        {!isEditMode && (
          <div className="space-y-3">
            <Label>
              {t("universalProvider.selectPreset", {
                defaultValue: "Select Preset Type",
              })}
            </Label>
            <div className="flex flex-wrap gap-2">
              {universalProviderPresets.map((preset) => (
                <button
                  key={preset.providerType}
                  type="button"
                  onClick={() => handlePresetSelect(preset)}
                  className={`inline-flex items-center gap-2 rounded-lg px-4 py-2 text-sm font-medium transition-colors ${
                    selectedPreset?.providerType === preset.providerType
                      ? "bg-primary text-primary-foreground"
                      : "bg-accent text-muted-foreground hover:bg-accent/80"
                  }`}
                >
                  <ProviderIcon
                    icon={preset.icon}
                    name={preset.name}
                    size={16}
                  />
                  {preset.name}
                </button>
              ))}
            </div>
            {selectedPreset?.description && (
              <p className="text-xs text-muted-foreground">
                {selectedPreset.description}
              </p>
            )}
          </div>
        )}

        {/* Basic information. */}
        <div className="space-y-4">
          <div className="space-y-2">
            <Label htmlFor="name">
              {t("universalProvider.name", { defaultValue: "Name" })}
            </Label>
            <Input
              id="name"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder={t("universalProvider.namePlaceholder", {
                defaultValue: "e.g., My NewAPI",
              })}
            />
          </div>

          <div className="space-y-2">
            <Label htmlFor="baseUrl">
              {t("universalProvider.baseUrl", { defaultValue: "API URL" })}
            </Label>
            <Input
              id="baseUrl"
              value={baseUrl}
              onChange={(e) => setBaseUrl(e.target.value)}
              placeholder="https://api.example.com"
            />
          </div>

          <div className="space-y-2">
            <Label htmlFor="apiKey">
              {t("universalProvider.apiKey", { defaultValue: "API Key" })}
            </Label>
            <div className="relative">
              <Input
                id="apiKey"
                type={showApiKey ? "text" : "password"}
                value={apiKey}
                onChange={(e) => setApiKey(e.target.value)}
                placeholder="sk-..."
                className="pr-10"
              />
              <Button
                type="button"
                variant="ghost"
                size="icon"
                className="absolute right-0 top-0 h-full px-3"
                onClick={() => setShowApiKey(!showApiKey)}
              >
                {showApiKey ? (
                  <EyeOff className="h-4 w-4" />
                ) : (
                  <Eye className="h-4 w-4" />
                )}
              </Button>
            </div>
          </div>

          <div className="space-y-2">
            <Label htmlFor="websiteUrl">
              {t("universalProvider.websiteUrl", {
                defaultValue: "Website URL",
              })}
            </Label>
            <Input
              id="websiteUrl"
              value={websiteUrl}
              onChange={(e) => setWebsiteUrl(e.target.value)}
              placeholder={t("universalProvider.websiteUrlPlaceholder", {
                defaultValue:
                  "https://example.com (optional, displayed in the list)",
              })}
            />
          </div>

          <div className="space-y-2">
            <Label htmlFor="notes">
              {t("universalProvider.notes", { defaultValue: "Notes" })}
            </Label>
            <Input
              id="notes"
              value={notes}
              onChange={(e) => setNotes(e.target.value)}
              placeholder={t("universalProvider.notesPlaceholder", {
                defaultValue: "Optional: Add notes",
              })}
            />
          </div>
        </div>

        {/* Enabled apps. */}
        <div className="space-y-3">
          <Label>
            {t("universalProvider.enabledApps", {
              defaultValue: "Enabled Apps",
            })}
          </Label>
          <div className="flex flex-col gap-3">
            <div className="flex items-center justify-between rounded-lg border p-3">
              <div className="flex items-center gap-2">
                <ProviderIcon icon="claude" name="Claude" size={20} />
                <span className="font-medium">Claude Code</span>
              </div>
              <Switch
                checked={claudeEnabled}
                onCheckedChange={setClaudeEnabled}
              />
            </div>
            <div className="flex items-center justify-between rounded-lg border p-3">
              <div className="flex items-center gap-2">
                <ProviderIcon icon="openai" name="Codex" size={20} />
                <span className="font-medium">OpenAI Codex</span>
              </div>
              <Switch
                checked={codexEnabled}
                onCheckedChange={setCodexEnabled}
              />
            </div>
            <div className="flex items-center justify-between rounded-lg border p-3">
              <div className="flex items-center gap-2">
                <ProviderIcon icon="gemini" name="Gemini" size={20} />
                <span className="font-medium">Gemini CLI</span>
              </div>
              <Switch
                checked={geminiEnabled}
                onCheckedChange={setGeminiEnabled}
              />
            </div>
          </div>
        </div>

        {/* Model configuration. */}
        <div className="space-y-4">
          <Label>
            {t("universalProvider.modelConfig", {
              defaultValue: "Model Configuration",
            })}
          </Label>

          {/* Claude models. */}
          {claudeEnabled && (
            <div className="space-y-3 rounded-lg border p-4">
              <div className="flex items-center gap-2 font-medium">
                <ProviderIcon icon="claude" name="Claude" size={16} />
                Claude
              </div>
              <div className="grid gap-3 sm:grid-cols-2">
                <div className="space-y-1">
                  <Label className="text-xs">
                    {t("universalProvider.model", { defaultValue: "Model" })}
                  </Label>
                  <Input
                    value={models.claude?.model || ""}
                    onChange={(e) =>
                      updateModel("claude", "model", e.target.value)
                    }
                    placeholder="claude-sonnet-4-20250514"
                  />
                </div>
                <div className="space-y-1">
                  <Label className="text-xs">Haiku</Label>
                  <Input
                    value={models.claude?.haikuModel || ""}
                    onChange={(e) =>
                      updateModel("claude", "haikuModel", e.target.value)
                    }
                    placeholder="claude-haiku-4-20250514"
                  />
                </div>
                <div className="space-y-1">
                  <Label className="text-xs">Sonnet</Label>
                  <Input
                    value={models.claude?.sonnetModel || ""}
                    onChange={(e) =>
                      updateModel("claude", "sonnetModel", e.target.value)
                    }
                    placeholder="claude-sonnet-4-20250514"
                  />
                </div>
                <div className="space-y-1">
                  <Label className="text-xs">Opus</Label>
                  <Input
                    value={models.claude?.opusModel || ""}
                    onChange={(e) =>
                      updateModel("claude", "opusModel", e.target.value)
                    }
                    placeholder="claude-sonnet-4-20250514"
                  />
                </div>
              </div>
            </div>
          )}

          {/* Codex model. */}
          {codexEnabled && (
            <div className="space-y-3 rounded-lg border p-4">
              <div className="flex items-center gap-2 font-medium">
                <ProviderIcon icon="openai" name="Codex" size={16} />
                Codex
              </div>
              <div className="grid gap-3 sm:grid-cols-2">
                <div className="space-y-1">
                  <Label className="text-xs">
                    {t("universalProvider.model", { defaultValue: "Model" })}
                  </Label>
                  <Input
                    value={models.codex?.model || ""}
                    onChange={(e) =>
                      updateModel("codex", "model", e.target.value)
                    }
                    placeholder="gpt-5.5"
                  />
                </div>
                <div className="space-y-1">
                  <Label className="text-xs">Reasoning Effort</Label>
                  <Input
                    value={models.codex?.reasoningEffort || ""}
                    onChange={(e) =>
                      updateModel("codex", "reasoningEffort", e.target.value)
                    }
                    placeholder="high"
                  />
                </div>
              </div>
            </div>
          )}

          {/* Gemini model. */}
          {geminiEnabled && (
            <div className="space-y-3 rounded-lg border p-4">
              <div className="flex items-center gap-2 font-medium">
                <ProviderIcon icon="gemini" name="Gemini" size={16} />
                Gemini
              </div>
              <div className="space-y-1">
                <Label className="text-xs">
                  {t("universalProvider.model", { defaultValue: "Model" })}
                </Label>
                <Input
                  value={models.gemini?.model || ""}
                  onChange={(e) =>
                    updateModel("gemini", "model", e.target.value)
                  }
                  placeholder="gemini-2.5-pro"
                />
              </div>
            </div>
          )}
        </div>

        {/* Configuration JSON preview. */}
        {isEditMode && (claudeEnabled || codexEnabled || geminiEnabled) && (
          <div className="space-y-4">
            <Label>
              {t("universalProvider.configJsonPreview", {
                defaultValue: "Config JSON Preview",
              })}
            </Label>
            <p className="text-xs text-muted-foreground">
              {t("universalProvider.configJsonPreviewHint", {
                defaultValue:
                  "The following configurations will be synced to each app (only the displayed fields will be overwritten, other custom settings will be preserved)",
              })}
            </p>

            {/* Claude JSON */}
            {claudeConfigJson && (
              <div className="space-y-2">
                <div className="flex items-center gap-2 text-sm font-medium">
                  <ProviderIcon icon="claude" name="Claude" size={16} />
                  Claude
                </div>
                <JsonEditor
                  value={JSON.stringify(claudeConfigJson, null, 2)}
                  onChange={() => {}}
                  height={180}
                  darkMode={isDarkMode}
                />
              </div>
            )}

            {/* Codex JSON */}
            {codexConfigJson && (
              <div className="space-y-2">
                <div className="flex items-center gap-2 text-sm font-medium">
                  <ProviderIcon icon="openai" name="Codex" size={16} />
                  Codex
                </div>
                <JsonEditor
                  value={JSON.stringify(codexConfigJson, null, 2)}
                  onChange={() => {}}
                  height={280}
                  darkMode={isDarkMode}
                />
              </div>
            )}

            {/* Gemini JSON */}
            {geminiConfigJson && (
              <div className="space-y-2">
                <div className="flex items-center gap-2 text-sm font-medium">
                  <ProviderIcon icon="gemini" name="Gemini" size={16} />
                  Gemini
                </div>
                <JsonEditor
                  value={JSON.stringify(geminiConfigJson, null, 2)}
                  onChange={() => {}}
                  height={140}
                  darkMode={isDarkMode}
                />
              </div>
            )}
          </div>
        )}
      </div>

      {/* Save-and-sync confirmation dialog. */}
      <ConfirmDialog
        isOpen={syncConfirmOpen}
        title={t("universalProvider.syncConfirmTitle", {
          defaultValue: "Sync Universal Provider",
        })}
        message={t("universalProvider.syncConfirmDescription", {
          defaultValue:
            'Syncing "{{name}}" will overwrite the associated provider configurations in Claude, Codex, and Gemini. Do you want to continue?',
          name: name,
        })}
        confirmText={t("universalProvider.saveAndSync", {
          defaultValue: "Save & Sync",
        })}
        onConfirm={confirmSaveAndSync}
        onCancel={() => {
          setSyncConfirmOpen(false);
          setPendingProvider(null);
        }}
      />
    </FullScreenPanel>
  );
}
