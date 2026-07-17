import { useEffect, useMemo, useRef, useState } from "react";
import { useForm } from "react-hook-form";
import { zodResolver } from "@hookform/resolvers/zod";
import { useQuery } from "@tanstack/react-query";
import { useTranslation } from "react-i18next";
import {
  ChevronDown,
  ChevronRight,
  Download,
  Loader2,
  Plus,
  Trash2,
} from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import {
  Form,
  FormControl,
  FormField,
  FormItem,
  FormMessage,
} from "@/components/ui/form";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { BasicFormFields } from "./BasicFormFields";
import { CodexOAuthSection } from "./CodexOAuthSection";
import { CopilotAuthSection } from "./CopilotAuthSection";
import { ApiKeySection } from "./shared/ApiKeySection";
import { EndpointField } from "./shared/EndpointField";
import { ModelDropdown } from "./shared/ModelDropdown";
import { ProviderPresetSelector } from "./ProviderPresetSelector";
import { useApiKeyLink } from "./hooks/useApiKeyLink";
import { providerSchema, type ProviderFormData } from "@/lib/schemas/provider";
import type {
  ClaudeApiFormat,
  ClaudeDesktopModelRoute,
  ProviderCategory,
  ProviderMeta,
} from "@/types";
import type { OpenClawSuggestedDefaults } from "@/config/openclawProviderPresets";
import {
  CLAUDE_DESKTOP_ROLE_ROUTE_IDS,
  claudeDesktopProviderPresets,
  type ClaudeDesktopProviderPreset,
  type ClaudeDesktopRoleId,
} from "@/config/claudeDesktopProviderPresets";
import { NEXUS_ENDPOINT, NEXUS_MODEL } from "@/config/nexus";
import {
  fetchModelsForConfig,
  showFetchModelsError,
  type FetchedModel,
} from "@/lib/api/model-fetch";
import {
  providersApi,
  type ClaudeDesktopDefaultRoute,
} from "@/lib/api/providers";
import { resolveManagedAccountId } from "@/lib/authBinding";

export type ClaudeDesktopProviderFormValues = ProviderFormData & {
  presetId?: string;
  presetCategory?: ProviderCategory;
  isPartner?: boolean;
  partnerPromotionKey?: string;
  meta?: ProviderMeta;
  providerKey?: string;
  suggestedDefaults?: OpenClawSuggestedDefaults;
};

type ApiKeyField = "ANTHROPIC_AUTH_TOKEN" | "ANTHROPIC_API_KEY";

type PresetEntry = {
  id: string;
  preset: ClaudeDesktopProviderPreset;
};

export interface ClaudeDesktopProviderFormProps {
  submitLabel: string;
  onSubmit: (values: ClaudeDesktopProviderFormValues) => Promise<void> | void;
  onCancel: () => void;
  onSubmittingChange?: (isSubmitting: boolean) => void;
  initialData?: {
    name?: string;
    websiteUrl?: string;
    notes?: string;
    settingsConfig?: Record<string, unknown>;
    category?: ProviderCategory;
    meta?: ProviderMeta;
    icon?: string;
    iconColor?: string;
  };
  showButtons?: boolean;
}

type RouteRow = {
  rowId: string;
  route: string;
  model: string;
  labelOverride: string;
  supports1m: boolean;
};

type RouteRowValues = Omit<RouteRow, "rowId">;
type RouteRole = ClaudeDesktopRoleId;

const CLAUDE_ROUTE_PREFIX = "claude-";
const ANTHROPIC_CLAUDE_ROUTE_PREFIX = "anthropic/claude-";
const LEGACY_ONE_M_MARKER = "[1m]";
const ROLE_ROUTE_IDS = CLAUDE_DESKTOP_ROLE_ROUTE_IDS;
const ROLE_ORDER: RouteRole[] = ["sonnet", "opus", "fable", "haiku"];

function envString(
  settingsConfig: Record<string, unknown> | undefined,
  key: string,
) {
  const env = settingsConfig?.env;
  if (!env || typeof env !== "object") return "";
  const value = (env as Record<string, unknown>)[key];
  return typeof value === "string" ? value : "";
}

function clonePlainRecord(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return {};
  }
  return { ...(value as Record<string, unknown>) };
}

function normalizeEndpoint(value: string) {
  return value.trim().replace(/\/+$/, "");
}

function routeRoleFromId(route: string): RouteRole {
  const normalized = route.trim().toLowerCase();
  // Keep the same order as the backend claude_role_keyword helper.
  if (normalized.includes("opus")) return "opus";
  if (normalized.includes("haiku")) return "haiku";
  if (normalized.includes("fable")) return "fable";
  return "sonnet";
}

function routeIdForRole(role: RouteRole, usedRoutes: Set<string>) {
  const baseRoute = ROLE_ROUTE_IDS[role];
  if (!usedRoutes.has(baseRoute)) return baseRoute;

  let index = 2;
  while (usedRoutes.has(`${baseRoute}-r${index}`)) {
    index += 1;
  }
  return `${baseRoute}-r${index}`;
}

function fallbackCatalogRouteId(usedRoutes: Set<string>) {
  const role = ROLE_ORDER.find((candidate) => {
    const route = ROLE_ROUTE_IDS[candidate];
    return !usedRoutes.has(route);
  });
  return routeIdForRole(role ?? "sonnet", usedRoutes);
}

function createRouteRow(row: RouteRowValues): RouteRow {
  return {
    rowId: crypto.randomUUID(),
    ...row,
  };
}

function initialRouteRows(
  routes: Record<string, ClaudeDesktopModelRoute> | undefined,
): RouteRow[] {
  const usedRoutes = new Set(
    Object.keys(routes ?? {}).filter((route) => isClaudeSafeRoute(route)),
  );

  return Object.entries(routes ?? {}).map(([route, value]) => {
    const routeId = isClaudeSafeRoute(route)
      ? route
      : fallbackCatalogRouteId(usedRoutes);
    usedRoutes.add(routeId);

    return createRouteRow({
      route: routeId,
      model: value.model ?? "",
      labelOverride:
        value.labelOverride ??
        (!isClaudeSafeRoute(route) ? value.model || route : ""),
      supports1m: value.supports1m ?? false,
    });
  });
}

// Proxy mode mirrors Claude Code with four fixed model roles. Normalize routes
// from any source into those slots so a missing role cannot break subagents.
// Claude Desktop 1.12603.1 and later accepts Fable as an independent role.
function normalizeProxyRows(rows: RouteRow[]): RouteRow[] {
  return ROLE_ORDER.map((role) => {
    const match = rows.find(
      (row) => row.route.trim() && routeRoleFromId(row.route) === role,
    );
    return createRouteRow({
      route: ROLE_ROUTE_IDS[role],
      model: match?.model ?? "",
      labelOverride: match?.labelOverride ?? "",
      supports1m: match?.supports1m ?? false,
    });
  });
}

function isClaudeSafeRoute(route: string) {
  const normalized = route.trim().toLowerCase();
  if (normalized.includes(LEGACY_ONE_M_MARKER)) return false;
  const routeTail = normalized.startsWith(ANTHROPIC_CLAUDE_ROUTE_PREFIX)
    ? normalized.slice(ANTHROPIC_CLAUDE_ROUTE_PREFIX.length)
    : normalized.startsWith(CLAUDE_ROUTE_PREFIX)
      ? normalized.slice(CLAUDE_ROUTE_PREFIX.length)
      : "";

  // Require an identifier after the role prefix. Degenerate values such as
  // claude-sonnet- make Claude Desktop reject the entire profile. Keep this in
  // sync with the backend is_claude_safe_model_id helper.
  return ["sonnet-", "opus-", "haiku-", "fable-"].some(
    (prefix) =>
      routeTail.startsWith(prefix) && routeTail.length > prefix.length,
  );
}

function defaultRouteRows(
  defaults: ClaudeDesktopDefaultRoute[],
  defaultModel: string,
): RouteRow[] {
  return defaults.map((route, index) =>
    createRouteRow({
      route: route.routeId,
      model: index === 0 ? defaultModel : "",
      labelOverride: "",
      supports1m: route.supports1m,
    }),
  );
}

function matchesNexusProtocolContract(
  mode: "direct" | "proxy",
  apiFormat: ClaudeApiFormat,
  routes: Record<string, ClaudeDesktopModelRoute>,
) {
  const expectedRoutes = Object.values(CLAUDE_DESKTOP_ROLE_ROUTE_IDS);
  return (
    mode === "proxy" &&
    apiFormat === "openai_chat" &&
    Object.keys(routes).length === expectedRoutes.length &&
    expectedRoutes.every((routeId) => {
      const route = routes[routeId];
      return (
        route?.model === NEXUS_MODEL &&
        route.labelOverride === NEXUS_MODEL &&
        route.supports1m === true
      );
    })
  );
}

export function ClaudeDesktopProviderForm({
  submitLabel,
  onSubmit,
  onCancel,
  onSubmittingChange,
  initialData,
  showButtons = true,
}: ClaudeDesktopProviderFormProps) {
  const { t } = useTranslation();
  const initialMode = initialData?.meta?.claudeDesktopMode ?? "direct";
  const [mode, setMode] = useState<"direct" | "proxy">(initialMode);
  const needsModelMapping = mode === "proxy";
  const [apiFormat, setApiFormat] = useState<ClaudeApiFormat>(
    initialData?.meta?.apiFormat ?? "anthropic",
  );
  const [baseUrl, setBaseUrl] = useState(
    envString(initialData?.settingsConfig, "ANTHROPIC_BASE_URL"),
  );
  const [apiKey, setApiKey] = useState(
    envString(initialData?.settingsConfig, "ANTHROPIC_AUTH_TOKEN") ||
      envString(initialData?.settingsConfig, "ANTHROPIC_API_KEY"),
  );
  const [apiKeyField, setApiKeyField] = useState<ApiKeyField>(() =>
    envString(initialData?.settingsConfig, "ANTHROPIC_API_KEY")
      ? "ANTHROPIC_API_KEY"
      : "ANTHROPIC_AUTH_TOKEN",
  );
  const [selectedGitHubAccountId, setSelectedGitHubAccountId] = useState<
    string | null
  >(() => resolveManagedAccountId(initialData?.meta, "github_copilot"));
  const [selectedCodexAccountId, setSelectedCodexAccountId] = useState<
    string | null
  >(() => resolveManagedAccountId(initialData?.meta, "codex_oauth"));
  const [codexFastMode, setCodexFastMode] = useState<boolean>(
    () => initialData?.meta?.codexFastMode ?? false,
  );
  const [selectedPresetId, setSelectedPresetId] = useState<string | null>(
    "custom",
  );
  const [activePreset, setActivePreset] = useState<
    (ClaudeDesktopProviderPreset & { id: string }) | null
  >(null);
  const [routes, setRoutes] = useState<RouteRow[]>(() => {
    const rows = initialRouteRows(initialData?.meta?.claudeDesktopModelRoutes);
    // Normalize populated proxy routes into the four roles. Leave an empty
    // initial list for the seed effect so async defaults can still populate it.
    return initialMode === "proxy" && rows.length > 0
      ? normalizeProxyRows(rows)
      : rows;
  });
  const didSeedDefaultRoutes = useRef(
    Object.keys(initialData?.meta?.claudeDesktopModelRoutes ?? {}).length > 0,
  );
  const [fetchedModels, setFetchedModels] = useState<FetchedModel[]>([]);
  const [isFetchingModels, setIsFetchingModels] = useState(false);
  const [directModelsExpanded, setDirectModelsExpanded] = useState(
    initialMode === "direct" &&
      Object.keys(initialData?.meta?.claudeDesktopModelRoutes ?? {}).length > 0,
  );
  const { data: defaultRoutes = [] } = useQuery({
    queryKey: ["claudeDesktopDefaultRoutes"],
    queryFn: () => providersApi.getClaudeDesktopDefaultRoutes(),
  });
  const defaultProxyRouteRows = useMemo(
    () =>
      defaultRouteRows(
        defaultRoutes,
        envString(initialData?.settingsConfig, "ANTHROPIC_MODEL"),
      ),
    [defaultRoutes, initialData?.settingsConfig],
  );

  const defaultValues: ProviderFormData = useMemo(
    () => ({
      name: initialData?.name ?? "",
      websiteUrl: initialData?.websiteUrl ?? "",
      notes: initialData?.notes ?? "",
      settingsConfig: JSON.stringify(
        initialData?.settingsConfig ?? { env: {} },
        null,
        2,
      ),
      icon: initialData?.icon ?? "",
      iconColor: initialData?.iconColor ?? "",
    }),
    [initialData],
  );

  const form = useForm<ProviderFormData>({
    resolver: zodResolver(providerSchema),
    defaultValues,
    mode: "onSubmit",
  });

  useEffect(() => {
    onSubmittingChange?.(form.formState.isSubmitting || isFetchingModels);
  }, [form.formState.isSubmitting, isFetchingModels, onSubmittingChange]);

  const presetEntries = useMemo<PresetEntry[]>(
    () =>
      claudeDesktopProviderPresets.map((preset, index) => ({
        id: `claude-desktop-${index}`,
        preset,
      })),
    [],
  );

  const presetCategoryLabels: Record<string, string> = useMemo(
    () => ({
      official: t("providerForm.categoryOfficial", {
        defaultValue: "Official",
      }),
      cn_official: t("providerForm.categoryCnOfficial", {
        defaultValue: "Regional official",
      }),
      aggregator: t("providerForm.categoryAggregation", {
        defaultValue: "Aggregator",
      }),
      third_party: t("providerForm.categoryThirdParty", {
        defaultValue: "Third party",
      }),
    }),
    [t],
  );
  const activeProviderType = activePreset
    ? activePreset.providerType
    : initialData?.meta?.providerType;
  const isOfficial = activePreset
    ? activePreset.category === "official"
    : initialData?.category === "official";
  const usesManagedOAuth =
    activePreset?.requiresOAuth === true ||
    activeProviderType === "github_copilot" ||
    activeProviderType === "codex_oauth";

  // Reuse the Claude Code API-key and invitation-link behavior.
  const apiKeyLinkCategory = activePreset?.category ?? initialData?.category;
  const {
    shouldShowApiKeyLink,
    websiteUrl: apiKeyLinkWebsiteUrl,
    isPartner: apiKeyLinkIsPartner,
    partnerPromotionKey: apiKeyLinkPromotionKey,
  } = useApiKeyLink({
    appId: "claude-desktop",
    category: apiKeyLinkCategory,
    selectedPresetId,
    presetEntries,
    formWebsiteUrl: form.watch("websiteUrl") || "",
  });

  const applyDesktopPreset = (preset: ClaudeDesktopProviderPreset) => {
    form.setValue("name", preset.nameKey ? t(preset.nameKey) : preset.name);
    form.setValue("websiteUrl", preset.websiteUrl);
    form.setValue("notes", "");
    form.setValue("icon", preset.icon ?? "");
    form.setValue("iconColor", preset.iconColor ?? "");

    setBaseUrl(preset.baseUrl);
    setApiKey("");
    setApiKeyField(preset.apiKeyField ?? "ANTHROPIC_AUTH_TOKEN");
    setApiFormat(preset.apiFormat ?? "anthropic");

    didSeedDefaultRoutes.current = true;
    setMode(preset.mode);
    if (preset.mode === "proxy" && preset.modelRoutes) {
      setRoutes(
        normalizeProxyRows(
          preset.modelRoutes.map((r) =>
            createRouteRow({
              route: r.routeId,
              model: r.upstreamModel,
              labelOverride: r.labelOverride ?? "",
              supports1m: r.supports1m,
            }),
          ),
        ),
      );
    } else {
      setRoutes([]);
    }
  };

  const handlePresetChange = (value: string) => {
    setSelectedPresetId(value);

    if (value === "custom") {
      setActivePreset(null);
      form.reset(defaultValues);
      setBaseUrl("");
      setApiKey("");
      setApiKeyField("ANTHROPIC_AUTH_TOKEN");
      setApiFormat("anthropic");
      didSeedDefaultRoutes.current = false;
      setMode("direct");
      setRoutes([]);
      return;
    }

    const entry = presetEntries.find((item) => item.id === value);
    if (!entry) return;

    setActivePreset({ ...entry.preset, id: value });
    applyDesktopPreset(entry.preset);
  };

  const updateRoute = (index: number, patch: Partial<RouteRowValues>) => {
    setRoutes((current) =>
      current.map((row, i) => (i === index ? { ...row, ...patch } : row)),
    );
  };

  const handleModelMappingChange = (checked: boolean) => {
    setMode(checked ? "proxy" : "direct");
    if (checked) {
      // Normalize proxy mode into the four fixed roles. If no routes exist,
      // seed them from backend defaults, including the default Sonnet model.
      setRoutes((current) => {
        // Keep an empty list while async defaults load; normalizing it early
        // would create four blank rows and permanently block the seed effect.
        if (current.length === 0 && defaultProxyRouteRows.length === 0) {
          return current;
        }
        const useDefaults =
          current.length === 0 && defaultProxyRouteRows.length > 0;
        if (useDefaults) {
          didSeedDefaultRoutes.current = true;
        }
        return normalizeProxyRows(
          useDefaults ? defaultProxyRouteRows : current,
        );
      });
    }
  };

  useEffect(() => {
    if (
      didSeedDefaultRoutes.current ||
      mode !== "proxy" ||
      routes.length > 0 ||
      defaultProxyRouteRows.length === 0
    ) {
      return;
    }

    didSeedDefaultRoutes.current = true;
    setRoutes(normalizeProxyRows(defaultProxyRouteRows));
  }, [defaultProxyRouteRows, mode, routes.length]);

  const handleFetchModels = async () => {
    if (!baseUrl.trim() || !apiKey.trim()) {
      showFetchModelsError(null, t, {
        hasBaseUrl: Boolean(baseUrl.trim()),
        hasApiKey: Boolean(apiKey.trim()),
      });
      return;
    }

    setIsFetchingModels(true);
    try {
      const models = await fetchModelsForConfig(baseUrl.trim(), apiKey.trim());
      setFetchedModels(models);
      toast.success(
        t("providerForm.fetchModelsSuccess", {
          count: models.length,
          defaultValue: `Fetched ${models.length} models`,
        }),
      );
    } catch (error) {
      showFetchModelsError(error, t, {
        hasBaseUrl: Boolean(baseUrl.trim()),
        hasApiKey: Boolean(apiKey.trim()),
      });
    } finally {
      setIsFetchingModels(false);
    }
  };

  const handleSubmit = async (values: ProviderFormData) => {
    if (!values.name.trim()) {
      toast.error(
        t("providerForm.fillSupplierName", {
          defaultValue: "Enter a provider name",
        }),
      );
      return;
    }
    if (isOfficial) {
      // Official providers use Claude Desktop's built-in first-party mode.
      // Preserve the empty environment placeholder used by OFFICIAL_SEEDS.
      const settingsConfig = clonePlainRecord(initialData?.settingsConfig);
      settingsConfig.env = {};
      delete settingsConfig.modelCatalog;
      const meta: ProviderMeta = { ...(initialData?.meta ?? {}) };
      delete meta.claudeDesktopMode;
      delete meta.claudeDesktopModelRoutes;
      delete meta.apiFormat;
      delete meta.endpointAutoSelect;
      delete meta.isFullUrl;
      delete meta.providerType;
      delete meta.localProxyRequestOverrides;
      delete meta.managedNexusPresetVersion;
      await onSubmit({
        ...values,
        name: values.name.trim(),
        websiteUrl: values.websiteUrl?.trim() ?? "",
        notes: values.notes?.trim() ?? "",
        settingsConfig: JSON.stringify(settingsConfig, null, 2),
        meta,
        presetId: activePreset?.id,
        presetCategory: "official",
      });
      return;
    }
    if (!baseUrl.trim()) {
      toast.error(
        t("providerForm.fetchModelsNeedEndpoint", {
          defaultValue: "Enter the API endpoint first",
        }),
      );
      return;
    }
    if (!usesManagedOAuth && !apiKey.trim()) {
      toast.error(
        t("providerForm.fetchModelsNeedApiKey", {
          defaultValue: "Enter an API key first",
        }),
      );
      return;
    }

    const routeEntries = routes
      .map((route) => ({
        ...route,
        route: route.route.trim(),
        model: route.model.trim(),
        labelOverride: route.labelOverride.trim(),
      }))
      .filter((route) => route.route || route.model);

    if (mode === "proxy") {
      // Route IDs are generated for the four fixed roles. Require one upstream
      // model and let blank roles inherit it so every subagent role remains usable.
      const primary = routeEntries.find((route) => route.model);
      if (!primary) {
        toast.error(
          t("claudeDesktop.routesRequired", {
            defaultValue: "Configure at least one model mapping",
          }),
        );
        return;
      }
      for (const route of routeEntries) {
        if (!route.model) {
          route.model = primary.model;
          if (!route.labelOverride) {
            route.labelOverride = primary.labelOverride || primary.model;
          }
          // Inherited roles use the same upstream model and 1M capability unless
          // the user explicitly enabled 1M for that role.
          if (!route.supports1m) {
            route.supports1m = primary.supports1m;
          }
        }
      }
    } else {
      const invalid = routeEntries.find(
        (route) => !route.route || !isClaudeSafeRoute(route.route),
      );
      if (invalid) {
        toast.error(
          t("claudeDesktop.directModelInvalid", {
            defaultValue:
              "Direct mode requires a Sonnet, Opus, Fable, or Haiku model ID recognized by Claude Desktop",
          }),
        );
        return;
      }
    }

    const settingsConfig = clonePlainRecord(initialData?.settingsConfig);
    const env = clonePlainRecord(settingsConfig.env);
    delete env.ANTHROPIC_AUTH_TOKEN;
    delete env.ANTHROPIC_API_KEY;
    settingsConfig.env = usesManagedOAuth
      ? {
          ...env,
          ANTHROPIC_BASE_URL: baseUrl.trim().replace(/\/+$/, ""),
        }
      : {
          ...env,
          ANTHROPIC_BASE_URL: baseUrl.trim().replace(/\/+$/, ""),
          [apiKeyField]: apiKey.trim(),
        };
    if (activePreset) {
      if (activePreset.modelCatalog) {
        settingsConfig.modelCatalog = activePreset.modelCatalog;
      } else {
        delete settingsConfig.modelCatalog;
      }
    }

    const routeMap = routeEntries.reduce<
      Record<string, ClaudeDesktopModelRoute>
    >((acc, route) => {
      acc[route.route] = {
        model: mode === "direct" ? route.route : route.model || route.route,
        labelOverride:
          route.labelOverride || (mode === "proxy" ? route.model : undefined),
        supports1m: route.supports1m || undefined,
      };
      return acc;
    }, {});
    const managedNexusSelected =
      activePreset?.providerType === "nexus" ||
      (!activePreset && initialData?.meta?.providerType === "nexus");
    const managedNexusProtocolChanged =
      managedNexusSelected &&
      !matchesNexusProtocolContract(mode, apiFormat, routeMap);
    const managedNexusEndpointChanged =
      managedNexusSelected &&
      normalizeEndpoint(baseUrl) !== normalizeEndpoint(NEXUS_ENDPOINT);

    const meta: ProviderMeta = {
      ...(initialData?.meta ?? {}),
      claudeDesktopMode: mode,
      apiFormat: mode === "proxy" ? apiFormat : "anthropic",
    };

    meta.claudeDesktopModelRoutes = routeMap;
    meta.providerType = activeProviderType;
    if (activePreset) {
      meta.localProxyRequestOverrides = activePreset.localProxyRequestOverrides;
      meta.managedNexusPresetVersion = activePreset.managedNexusPresetVersion;
    }
    if (managedNexusProtocolChanged) {
      delete settingsConfig.modelCatalog;
      delete meta.providerType;
      delete meta.managedNexusPresetVersion;
      delete meta.localProxyRequestOverrides;
    } else if (managedNexusEndpointChanged) {
      delete meta.managedNexusPresetVersion;
    }
    meta.authBinding =
      activeProviderType === "github_copilot"
        ? {
            source: "managed_account",
            authProvider: "github_copilot",
            accountId: selectedGitHubAccountId ?? undefined,
          }
        : activeProviderType === "codex_oauth"
          ? {
              source: "managed_account",
              authProvider: "codex_oauth",
              accountId: selectedCodexAccountId ?? undefined,
            }
          : undefined;
    meta.codexFastMode =
      activeProviderType === "codex_oauth" ? codexFastMode : undefined;

    delete meta.endpointAutoSelect;
    delete meta.isFullUrl;

    await onSubmit({
      ...values,
      name: values.name.trim(),
      websiteUrl: values.websiteUrl?.trim() ?? "",
      notes: values.notes?.trim() ?? "",
      settingsConfig: JSON.stringify(settingsConfig, null, 2),
      meta,
      presetId: activePreset?.id,
      presetCategory: activePreset?.category,
      isPartner: activePreset?.isPartner,
      partnerPromotionKey: activePreset?.partnerPromotionKey,
    });
  };

  const renderActionButtons = (onAdd: () => void, addLabel: string) => (
    <div className="flex gap-1">
      {!usesManagedOAuth && (
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={handleFetchModels}
          disabled={isFetchingModels}
          className="h-7 gap-1"
        >
          {isFetchingModels ? (
            <Loader2 className="h-3.5 w-3.5 animate-spin" />
          ) : (
            <Download className="h-3.5 w-3.5" />
          )}
          {t("providerForm.fetchModels", { defaultValue: "Fetch models" })}
        </Button>
      )}
      <Button
        type="button"
        variant="outline"
        size="sm"
        onClick={onAdd}
        className="h-7 gap-1"
      >
        <Plus className="h-3.5 w-3.5" />
        {addLabel}
      </Button>
    </div>
  );

  return (
    <Form {...form}>
      <form
        id="provider-form"
        onSubmit={form.handleSubmit(handleSubmit)}
        className="space-y-6"
      >
        {!initialData && (
          <ProviderPresetSelector
            selectedPresetId={selectedPresetId}
            presetEntries={presetEntries}
            presetCategoryLabels={presetCategoryLabels}
            onPresetChange={handlePresetChange}
            category={activePreset?.category}
          />
        )}

        <BasicFormFields form={form} />

        {isOfficial && (
          <div className="rounded-lg border border-border-default bg-muted/20 p-3 text-sm text-muted-foreground">
            {t("claudeDesktop.officialNotice", {
              defaultValue:
                "Official Claude Desktop providers use the app's built-in sign-in and need no API key or endpoint.",
            })}
          </div>
        )}

        {!isOfficial && (
          <>
            {usesManagedOAuth ? (
              <div className="rounded-lg border border-border-default bg-muted/20 p-3">
                {activeProviderType === "github_copilot" ? (
                  <CopilotAuthSection
                    selectedAccountId={selectedGitHubAccountId}
                    onAccountSelect={setSelectedGitHubAccountId}
                  />
                ) : (
                  <CodexOAuthSection
                    selectedAccountId={selectedCodexAccountId}
                    onAccountSelect={setSelectedCodexAccountId}
                    fastModeEnabled={codexFastMode}
                    onFastModeChange={setCodexFastMode}
                  />
                )}
              </div>
            ) : (
              <ApiKeySection
                value={apiKey}
                onChange={setApiKey}
                category={apiKeyLinkCategory}
                shouldShowLink={
                  shouldShowApiKeyLink && activeProviderType !== "nexus"
                }
                websiteUrl={apiKeyLinkWebsiteUrl}
                isPartner={apiKeyLinkIsPartner}
                partnerPromotionKey={apiKeyLinkPromotionKey}
              />
            )}

            <EndpointField
              id="baseUrl"
              label={t("providerForm.apiEndpoint")}
              value={baseUrl}
              onChange={(v) => setBaseUrl(v)}
              placeholder={t("providerForm.apiEndpointPlaceholder")}
              hint={
                needsModelMapping && apiFormat === "openai_responses"
                  ? t("providerForm.apiHintResponses")
                  : needsModelMapping && apiFormat === "openai_chat"
                    ? t("providerForm.apiHintOAI")
                    : needsModelMapping && apiFormat === "gemini_native"
                      ? t("providerForm.apiHintGeminiNative")
                      : t("providerForm.apiHint")
              }
              showManageButton={false}
            />

            <div className="space-y-3 rounded-lg border border-border-default bg-muted/20 p-4">
              <div className="flex items-center justify-between gap-4">
                <div className="space-y-1">
                  <Label>
                    {t("claudeDesktop.modelMappingToggle", {
                      defaultValue: "Needs model mapping",
                    })}
                  </Label>
                  <p className="text-xs leading-relaxed text-muted-foreground">
                    {needsModelMapping
                      ? t("claudeDesktop.modelMappingOnHint", {
                          defaultValue:
                            "Claude Desktop accepts four role IDs: claude-sonnet-*, claude-opus-*, claude-fable-*, and claude-haiku-*. Nexus maps them to the provider's models and keeps routing active while they are in use.",
                        })
                      : t("claudeDesktop.modelMappingOffHint", {
                          defaultValue:
                            "Use direct mode only when the provider accepts Claude Desktop's Sonnet, Opus, Fable, and Haiku role IDs. Enable mapping for all other model IDs.",
                        })}
                  </p>
                </div>
                <Switch
                  checked={needsModelMapping}
                  onCheckedChange={handleModelMappingChange}
                  aria-label={t("claudeDesktop.modelMappingToggle", {
                    defaultValue: "Needs model mapping",
                  })}
                />
              </div>
            </div>

            {needsModelMapping && (
              <div className="space-y-4 rounded-lg border border-border-default p-4">
                <div className="space-y-2">
                  <Label>
                    {t("providerForm.apiFormat", {
                      defaultValue: "API format",
                    })}
                  </Label>
                  <Select
                    value={apiFormat}
                    onValueChange={(value) =>
                      setApiFormat(value as ClaudeApiFormat)
                    }
                  >
                    <SelectTrigger className="w-full">
                      <SelectValue />
                    </SelectTrigger>
                    <SelectContent>
                      <SelectItem value="anthropic">
                        {t("providerForm.apiFormatAnthropic", {
                          defaultValue: "Anthropic Messages (native)",
                        })}
                      </SelectItem>
                      <SelectItem value="openai_chat">
                        {t("providerForm.apiFormatOpenAIChat", {
                          defaultValue:
                            "OpenAI Chat Completions (Needs Routing)",
                        })}
                      </SelectItem>
                      <SelectItem value="openai_responses">
                        {t("providerForm.apiFormatOpenAIResponses", {
                          defaultValue: "OpenAI Responses API (Needs Routing)",
                        })}
                      </SelectItem>
                      <SelectItem value="gemini_native">
                        {t("providerForm.apiFormatGeminiNative", {
                          defaultValue:
                            "Gemini Native generateContent (Needs Routing)",
                        })}
                      </SelectItem>
                    </SelectContent>
                  </Select>
                </div>

                <div className="space-y-3">
                  <div className="space-y-1 border-t border-border-default pt-4">
                    <div className="flex items-center justify-between">
                      <Label>
                        {t("claudeDesktop.routeMapTitle", {
                          defaultValue: "Model mapping",
                        })}
                      </Label>
                      {!usesManagedOAuth && (
                        <Button
                          type="button"
                          variant="outline"
                          size="sm"
                          onClick={handleFetchModels}
                          disabled={isFetchingModels}
                          className="h-7 gap-1"
                        >
                          {isFetchingModels ? (
                            <Loader2 className="h-3.5 w-3.5 animate-spin" />
                          ) : (
                            <Download className="h-3.5 w-3.5" />
                          )}
                          {t("providerForm.fetchModels", {
                            defaultValue: "Fetch models",
                          })}
                        </Button>
                      )}
                    </div>
                    <p className="text-xs leading-relaxed text-muted-foreground">
                      {t("claudeDesktop.routeMapHint", {
                        defaultValue:
                          "Map the Sonnet, Opus, Fable, and Haiku roles to upstream models. Blank roles inherit the first configured model so subagent calls remain available.",
                      })}
                    </p>
                  </div>

                  <div className="hidden grid-cols-[140px_1fr_1fr_116px] gap-2 px-1 text-xs font-medium text-muted-foreground md:grid">
                    <span>
                      {t("claudeDesktop.routeModelLabel", {
                        defaultValue: "Model role",
                      })}
                    </span>
                    <span>
                      {t("claudeDesktop.labelOverrideLabel", {
                        defaultValue: "Menu label",
                      })}
                    </span>
                    <span>
                      {t("claudeDesktop.upstreamModelLabel", {
                        defaultValue: "Upstream model",
                      })}
                    </span>
                    <span>
                      {t("claudeDesktop.supports1mLabel", {
                        defaultValue: "Advertise 1M",
                      })}
                    </span>
                  </div>
                  {routes.map((route, index) => {
                    const role = routeRoleFromId(route.route);
                    const roleLabel =
                      role === "opus"
                        ? t("claudeDesktop.routeRoleOpus", {
                            defaultValue: "Opus",
                          })
                        : role === "haiku"
                          ? t("claudeDesktop.routeRoleHaiku", {
                              defaultValue: "Haiku",
                            })
                          : role === "fable"
                            ? t("claudeDesktop.routeRoleFable", {
                                defaultValue: "Fable",
                              })
                            : t("claudeDesktop.routeRoleSonnet", {
                                defaultValue: "Sonnet",
                              });
                    // Show a lightweight placeholder for Haiku and a larger
                    // model for other roles, keeping labels and IDs aligned.
                    const isHaikuRole = role === "haiku";
                    const labelPlaceholder = isHaikuRole
                      ? "DeepSeek V4 Flash"
                      : "DeepSeek V4 Pro";
                    const modelPlaceholder = isHaikuRole
                      ? "deepseek-v4-flash"
                      : "deepseek-v4-pro";
                    return (
                      <div
                        key={route.rowId}
                        className="grid grid-cols-1 gap-2 md:grid-cols-[140px_1fr_1fr_116px]"
                      >
                        <div className="flex h-9 items-center rounded-md border border-input bg-muted px-3 text-sm font-medium text-muted-foreground">
                          {roleLabel}
                        </div>
                        <Input
                          value={route.labelOverride}
                          onChange={(event) =>
                            updateRoute(index, {
                              labelOverride: event.target.value,
                            })
                          }
                          placeholder={labelPlaceholder}
                        />
                        <div className="flex gap-1">
                          <Input
                            value={route.model}
                            onChange={(event) =>
                              updateRoute(index, { model: event.target.value })
                            }
                            placeholder={modelPlaceholder}
                            className="flex-1"
                          />
                          {fetchedModels.length > 0 && (
                            <ModelDropdown
                              models={fetchedModels}
                              onSelect={(id) =>
                                updateRoute(index, {
                                  model: id,
                                  labelOverride: route.labelOverride || id,
                                })
                              }
                            />
                          )}
                        </div>
                        <label className="flex h-9 items-center gap-2 text-sm text-muted-foreground">
                          <Checkbox
                            checked={route.supports1m}
                            onCheckedChange={(checked) =>
                              updateRoute(index, {
                                supports1m: checked === true,
                              })
                            }
                          />
                          {t("claudeDesktop.supports1mShort", {
                            defaultValue: "1M",
                          })}
                        </label>
                      </div>
                    );
                  })}
                </div>
              </div>
            )}

            {!needsModelMapping && (
              <Collapsible
                open={directModelsExpanded}
                onOpenChange={setDirectModelsExpanded}
              >
                <CollapsibleTrigger asChild>
                  <Button
                    type="button"
                    variant={null}
                    size="sm"
                    className="h-8 gap-1.5 px-0 text-sm font-medium text-foreground hover:opacity-70"
                  >
                    {directModelsExpanded ? (
                      <ChevronDown className="h-4 w-4" />
                    ) : (
                      <ChevronRight className="h-4 w-4" />
                    )}
                    {t("claudeDesktop.directModelListTitle", {
                      defaultValue:
                        "Manual Claude Desktop model list (advanced, optional)",
                    })}
                  </Button>
                </CollapsibleTrigger>
                {!directModelsExpanded && (
                  <p className="ml-1 mt-1 text-xs text-muted-foreground">
                    {t("claudeDesktop.directModelListCollapsedHint", {
                      defaultValue:
                        "Native Claude providers usually need no entries because Claude Desktop reads /v1/models automatically.",
                    })}
                  </p>
                )}
                <CollapsibleContent className="space-y-4 pt-2">
                  <div className="space-y-4 rounded-lg border border-border-default p-4">
                    <div className="flex flex-wrap items-start justify-between gap-3">
                      <p className="flex-1 text-xs leading-relaxed text-muted-foreground">
                        {t("claudeDesktop.directModelListHint", {
                          defaultValue:
                            "Use this only when /v1/models is unavailable or lacks Claude Desktop-compatible Sonnet, Opus, Fable, or Haiku IDs. Enable 1M to advertise 1M context support.",
                        })}
                      </p>
                      {renderActionButtons(
                        () =>
                          setRoutes((current) => [
                            ...current,
                            createRouteRow({
                              route: "",
                              model: "",
                              labelOverride: "",
                              supports1m: false,
                            }),
                          ]),
                        t("claudeDesktop.addModel", {
                          defaultValue: "Add model",
                        }),
                      )}
                    </div>

                    {routes.length > 0 ? (
                      <div className="space-y-2">
                        {routes.map((route, index) => (
                          <div
                            key={route.rowId}
                            className="grid grid-cols-1 gap-2 md:grid-cols-[1fr_116px_36px]"
                          >
                            <div className="flex gap-1">
                              <Input
                                value={route.route}
                                onChange={(event) =>
                                  updateRoute(index, {
                                    route: event.target.value,
                                  })
                                }
                                placeholder="claude-sonnet-4-6"
                                className="flex-1"
                              />
                              {fetchedModels.length > 0 && (
                                <ModelDropdown
                                  models={fetchedModels}
                                  onSelect={(id) =>
                                    updateRoute(index, { route: id })
                                  }
                                />
                              )}
                            </div>
                            <label className="flex h-9 items-center gap-2 text-sm text-muted-foreground">
                              <Checkbox
                                checked={route.supports1m}
                                onCheckedChange={(checked) =>
                                  updateRoute(index, {
                                    supports1m: checked === true,
                                  })
                                }
                              />
                              {t("claudeDesktop.supports1mShort", {
                                defaultValue: "1M",
                              })}
                            </label>
                            <Button
                              type="button"
                              variant="ghost"
                              size="icon"
                              onClick={() =>
                                setRoutes((current) =>
                                  current.filter((_, i) => i !== index),
                                )
                              }
                            >
                              <Trash2 className="h-4 w-4" />
                            </Button>
                          </div>
                        ))}
                      </div>
                    ) : null}
                  </div>
                </CollapsibleContent>
              </Collapsible>
            )}

            <FormField
              control={form.control}
              name="settingsConfig"
              render={() => (
                <FormItem className="space-y-0">
                  <FormControl>
                    <input type="hidden" />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />
          </>
        )}

        {showButtons && (
          <div className="flex justify-end gap-2">
            <Button variant="outline" type="button" onClick={onCancel}>
              {t("common.cancel")}
            </Button>
            <Button type="submit" disabled={form.formState.isSubmitting}>
              {submitLabel}
            </Button>
          </div>
        )}
      </form>
    </Form>
  );
}
