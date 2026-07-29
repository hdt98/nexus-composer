import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Save } from "lucide-react";
import { Button } from "@/components/ui/button";
import { FullScreenPanel } from "@/components/common/FullScreenPanel";
import type { Provider } from "@/types";
import {
  ProviderForm,
  type ProviderFormValues,
} from "@/components/providers/forms/ProviderForm";
import { openclawApi, providersApi, vscodeApi, type AppId } from "@/lib/api";

interface EditProviderDialogProps {
  open: boolean;
  provider: Provider | null;
  onOpenChange: (open: boolean) => void;
  onSubmit: (payload: {
    provider: Provider;
    originalId?: string;
  }) => Promise<void> | void;
  appId: AppId;
  isProxyTakeover?: boolean; // Skip live config during proxy takeover to avoid showing the rewritten proxy values.
}

const NEXUS_MODEL_CATALOG_FILES = new Set([
  "nexus-model-catalog.json",
  "cc-switch-model-catalog.json",
]);

function withoutNexusModelCatalogPointer(config: string): string {
  let topLevel = true;
  return config
    .split("\n")
    .filter((line) => {
      const trimmed = line.trimStart();
      if (trimmed.startsWith("[")) topLevel = false;
      const path = topLevel
        ? trimmed.match(/^model_catalog_json\s*=\s*["']([^"']+)["']/)?.[1]
        : undefined;
      return !NEXUS_MODEL_CATALOG_FILES.has(path?.split(/[\\/]/).pop() ?? "");
    })
    .join("\n");
}

function hasModelCatalogEntries(catalog: unknown): boolean {
  const models = (catalog as { models?: unknown } | null)?.models;
  return Array.isArray(models) && models.length > 0;
}

export function EditProviderDialog({
  open,
  provider,
  onOpenChange,
  onSubmit,
  appId,
  isProxyTakeover = false,
}: EditProviderDialogProps) {
  const { t } = useTranslation();
  const [isFormSubmitting, setIsFormSubmitting] = useState(false);

  // Default to provider.settingsConfig. For the active provider, try to replace the initial value with live config.
  const [liveSettings, setLiveSettings] = useState<Record<
    string,
    unknown
  > | null>(null);

  // Track whether live config has loaded so repeated reads cannot overwrite user edits.
  const [hasLoadedLive, setHasLoadedLive] = useState(false);

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      if (!open || !provider) {
        setLiveSettings(null);
        setHasLoadedLive(false);
        return;
      }

      // Load only once per open session.
      if (hasLoadedLive) {
        return;
      }

      // During proxy takeover, the proxy has rewritten live config with its address and placeholders.
      // Fall back directly to the database source of truth to avoid confusing displays and accidental saves.
      if (isProxyTakeover) {
        if (!cancelled) {
          setLiveSettings(null);
          setHasLoadedLive(true);
        }
        return;
      }

      // OpenCode uses additive mode - each provider's config is stored independently in DB
      // Reading live config would return the full opencode.json (with $schema, provider, mcp etc.)
      // instead of just the provider fragment, causing incorrect nested structure on save
      if (appId === "opencode") {
        if (!cancelled) {
          setLiveSettings(null);
          setHasLoadedLive(true);
        }
        return;
      }

      if (appId === "openclaw") {
        try {
          const live = await openclawApi.getLiveProvider(provider.id);
          if (!cancelled && live && typeof live === "object") {
            setLiveSettings(live);
          } else if (!cancelled) {
            setLiveSettings(null);
          }
        } catch {
          if (!cancelled) {
            setLiveSettings(null);
          }
        } finally {
          if (!cancelled) {
            setHasLoadedLive(true);
          }
        }
        return;
      }

      try {
        const currentId = await providersApi.getCurrent(appId);
        if (currentId && provider.id === currentId) {
          try {
            const live = (await vscodeApi.getLiveProviderSettings(
              appId,
            )) as Record<string, unknown>;
            if (!cancelled && live && typeof live === "object") {
              setLiveSettings(live);
              setHasLoadedLive(true);
            }
          } catch {
            // Fall back to the database source of truth if reading live config fails, without interrupting editing.
            if (!cancelled) {
              setLiveSettings(null);
              setHasLoadedLive(true);
            }
          }
        } else {
          if (!cancelled) {
            setLiveSettings(null);
            setHasLoadedLive(true);
          }
        }
      } finally {
        // no-op
      }
    };
    void load();
    return () => {
      cancelled = true;
    };
  }, [open, provider?.id, appId, hasLoadedLive, isProxyTakeover]); // Depend on provider.id rather than the entire provider object.

  const initialSettingsConfig = useMemo(() => {
    const base = (liveSettings ?? provider?.settingsConfig ?? {}) as Record<
      string,
      unknown
    >;

    // Codex modelCatalog is a Nexus Composer-specific field whose source of truth is the database.
    // Live config.toml contains only a projected model_catalog_json pointer when written. Codex.app rewrites,
    // proxy takeover and restore cycles, or provider switching may remove that projection, causing
    // read_live_settings to reconstruct an empty value. Allowing live config to overwrite it would show an
    // empty mapping table and erase the database mapping on save. Always prefer the database modelCatalog,
    // falling back to the reconstructed live value only when the database has no catalog.
    if (
      appId === "codex" &&
      liveSettings &&
      provider?.settingsConfig &&
      typeof provider.settingsConfig === "object"
    ) {
      const dbCatalog = (provider.settingsConfig as Record<string, unknown>)
        .modelCatalog;
      if (dbCatalog !== undefined) {
        const merged: Record<string, unknown> = {
          ...base,
          modelCatalog: dbCatalog,
        };
        if (
          hasModelCatalogEntries(dbCatalog) &&
          typeof merged.config === "string"
        ) {
          merged.config = withoutNexusModelCatalogPointer(merged.config);
        }
        return merged;
      }
    }

    return base;
  }, [liveSettings, provider?.settingsConfig, appId]); // Depend on settingsConfig rather than the entire provider.

  // Stabilize initialData so provider object updates do not reset the form.
  const initialData = useMemo(() => {
    if (!provider) return null;
    return {
      name: provider.name,
      notes: provider.notes,
      websiteUrl: provider.websiteUrl,
      settingsConfig: initialSettingsConfig,
      category: provider.category,
      meta: provider.meta,
      icon: provider.icon,
      iconColor: provider.iconColor,
    };
  }, [
    open, // Re-read the latest provider data on every open instead of showing stale data after a save.
    provider?.id, // Depend only on the ID so provider object updates do not trigger recalculation.
    provider?.meta, // Include meta so testConfig initializes correctly.
    initialSettingsConfig,
  ]);

  const handleSubmit = useCallback(
    async (values: ProviderFormValues) => {
      if (!provider) return;

      // values.settingsConfig is already the final configuration string.
      // ProviderForm has assembled the correct configuration for Claude, Codex, or Gemini.
      const parsedConfig = JSON.parse(values.settingsConfig) as Record<
        string,
        unknown
      >;
      const nextProviderId =
        (appId === "opencode" || appId === "openclaw") &&
        values.providerKey?.trim()
          ? values.providerKey.trim()
          : provider.id;

      const updatedProvider: Provider = {
        ...provider,
        id: nextProviderId,
        name: values.name.trim(),
        notes: values.notes?.trim() || undefined,
        websiteUrl: values.websiteUrl?.trim() || undefined,
        settingsConfig: parsedConfig,
        icon: values.icon?.trim() || undefined,
        iconColor: values.iconColor?.trim() || undefined,
        ...(values.presetCategory ? { category: values.presetCategory } : {}),
        // Preserve or update the meta field.
        ...(values.meta ? { meta: values.meta } : {}),
      };

      await onSubmit({
        provider: updatedProvider,
        originalId: provider.id,
      });
      onOpenChange(false);
    },
    [appId, onSubmit, onOpenChange, provider],
  );

  if (!provider || !initialData) {
    return null;
  }

  return (
    <FullScreenPanel
      isOpen={open}
      title={t("provider.editProvider")}
      onClose={() => onOpenChange(false)}
      footer={
        <Button
          type="submit"
          form="provider-form"
          disabled={isFormSubmitting}
          className="bg-primary text-primary-foreground hover:bg-primary/90"
        >
          <Save className="h-4 w-4 mr-2" />
          {t("common.save")}
        </Button>
      }
    >
      <ProviderForm
        appId={appId}
        providerId={provider.id}
        submitLabel={t("common.save")}
        onSubmit={handleSubmit}
        onCancel={() => onOpenChange(false)}
        onSubmittingChange={setIsFormSubmitting}
        initialData={initialData}
        showButtons={false}
        isProxyTakeover={isProxyTakeover}
      />
    </FullScreenPanel>
  );
}
