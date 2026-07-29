import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useQuery } from "@tanstack/react-query";
import { toast } from "sonner";
import { Check, Loader2, Search } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Alert, AlertDescription } from "@/components/ui/alert";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { useUpdateModelPricing } from "@/lib/query/usage";
import { isTextEditableTarget } from "@/utils/domUtils";

const MODELS_DEV_API_URL = "https://models.dev/api.json";
// The full dataset has about 5,000 entries. Show only the newest batch by default and search all entries on demand.
const DEFAULT_VISIBLE_ROWS = 50;
const MAX_VISIBLE_ROWS = 200;

interface ModelsDevCost {
  input?: number;
  output?: number;
  cache_read?: number;
  cache_write?: number;
}

interface ModelsDevModel {
  id?: string;
  name?: string;
  release_date?: string;
  cost?: ModelsDevCost;
}

interface ModelsDevProvider {
  id?: string;
  name?: string;
  models?: Record<string, ModelsDevModel>;
}

type ModelsDevResponse = Record<string, ModelsDevProvider>;

interface ModelsDevEntry {
  /** providerId/modelId; the same model may appear under multiple providers. */
  key: string;
  providerId: string;
  providerName: string;
  modelId: string;
  /** ID stored in the database, normalized with the backend's clean_model_id_for_pricing rules. */
  normalizedId: string;
  modelName: string;
  /** YYYY-MM-DD or YYYY-MM; an empty string when unavailable. */
  releaseDate: string;
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
}

/**
 * Match the backend clean_model_id_for_pricing normalization in usage_stats.rs:
 * keep the segment after the final '/', remove the ':' suffix, replace '@' with '-',
 * convert to lowercase, and remove the [1m] marker. Cost attribution queries use
 * this normalized form, so storing the original ID would never produce a match.
 */
export function normalizeModelIdForPricing(modelId: string): string {
  const afterSlash = modelId.slice(modelId.lastIndexOf("/") + 1);
  const beforeColon = afterSlash.split(":")[0] ?? "";
  let normalized = beforeColon.trim().replace(/@/g, "-").toLowerCase();
  if (normalized.endsWith("[1m]")) {
    normalized = normalized.slice(0, -"[1m]".length).trim();
  }
  return normalized;
}

/** Convert to a non-negative decimal string the backend can parse; String() may use scientific notation. */
export function formatPrice(value: number): string {
  if (!Number.isFinite(value) || value <= 0) return "0";
  // toFixed uses scientific notation for values >= 1e21. Prices this large are invalid data, so treat them as zero.
  if (value >= 1e12) return "0";
  const trimmed = value.toFixed(6).replace(/0+$/, "").replace(/\.$/, "");
  return trimmed || "0";
}

export function flattenModels(data: ModelsDevResponse): ModelsDevEntry[] {
  const entries: ModelsDevEntry[] = [];
  for (const [providerId, provider] of Object.entries(data)) {
    if (!provider || typeof provider !== "object") continue;
    const providerName = provider.name || providerId;
    for (const [modelId, model] of Object.entries(provider.models ?? {})) {
      const cost = model?.cost;
      const input = typeof cost?.input === "number" ? cost.input : null;
      const output = typeof cost?.output === "number" ? cost.output : null;
      if (input === null && output === null) continue;
      const normalizedId = normalizeModelIdForPricing(modelId);
      if (!normalizedId) continue;
      entries.push({
        key: `${providerId}/${modelId}`,
        providerId,
        providerName,
        modelId,
        normalizedId,
        modelName: model?.name || modelId,
        releaseDate:
          typeof model?.release_date === "string" ? model.release_date : "",
        input: input ?? 0,
        output: output ?? 0,
        cacheRead: typeof cost?.cache_read === "number" ? cost.cache_read : 0,
        cacheWrite:
          typeof cost?.cache_write === "number" ? cost.cache_write : 0,
      });
    }
  }
  // Sort the most recently released models first.
  entries.sort(
    (a, b) =>
      b.releaseDate.localeCompare(a.releaseDate) ||
      a.modelName.localeCompare(b.modelName),
  );
  return entries;
}

interface ModelsDevPickerDialogProps {
  open: boolean;
  onClose: () => void;
  /** Called after a successful import, once the pricing list has refreshed. */
  onImported: () => void;
}

export function ModelsDevPickerDialog({
  open,
  onClose,
  onImported,
}: ModelsDevPickerDialogProps) {
  const { t } = useTranslation();
  const updatePricing = useUpdateModelPricing();

  const [search, setSearch] = useState("");
  const [providerFilter, setProviderFilter] = useState("all");
  const [selected, setSelected] = useState<ModelsDevEntry | null>(null);

  // Reset the selection and filters each time the dialog opens.
  useEffect(() => {
    if (open) {
      setSearch("");
      setProviderFilter("all");
      setSelected(null);
    }
  }, [open]);

  const { data, isLoading, error, refetch } = useQuery({
    queryKey: ["models-dev-pricing"],
    queryFn: async (): Promise<ModelsDevResponse> => {
      const res = await fetch(MODELS_DEV_API_URL);
      if (!res.ok) {
        throw new Error(`HTTP ${res.status}`);
      }
      return res.json();
    },
    enabled: open,
    staleTime: 60 * 60 * 1000,
    retry: 1,
  });

  const entries = useMemo(() => (data ? flattenModels(data) : []), [data]);

  const providers = useMemo(() => {
    const map = new Map<string, string>();
    for (const entry of entries) {
      if (!map.has(entry.providerId)) {
        map.set(entry.providerId, entry.providerName);
      }
    }
    return Array.from(map, ([id, name]) => ({ id, name })).sort((a, b) =>
      a.name.localeCompare(b.name),
    );
  }, [entries]);

  const isFiltering = search.trim() !== "" || providerFilter !== "all";

  const filtered = useMemo(() => {
    const query = search.trim().toLowerCase();
    return entries.filter(
      (entry) =>
        (providerFilter === "all" || entry.providerId === providerFilter) &&
        (!query ||
          entry.modelId.toLowerCase().includes(query) ||
          entry.normalizedId.includes(query) ||
          entry.modelName.toLowerCase().includes(query) ||
          entry.providerName.toLowerCase().includes(query)),
    );
  }, [entries, search, providerFilter]);

  // Show only the newest batch by default. Search and filtering match the full dataset, capped to prevent UI stalls.
  const visible = useMemo(
    () =>
      filtered.slice(0, isFiltering ? MAX_VISIBLE_ROWS : DEFAULT_VISIBLE_ROWS),
    [filtered, isFiltering],
  );

  // Single selection: clicking an unselected row replaces the selection; clicking the selected row clears it.
  // This avoids triggering a full zero-cost backfill scan for every item in a bulk import; see update_model_pricing.
  const toggleEntry = (entry: ModelsDevEntry) => {
    setSelected((prev) => (prev?.key === entry.key ? null : entry));
  };

  const handleImport = async () => {
    if (!selected) return;

    try {
      await updatePricing.mutateAsync({
        modelId: selected.normalizedId,
        displayName: selected.modelName,
        inputCost: formatPrice(selected.input),
        outputCost: formatPrice(selected.output),
        cacheReadCost: formatPrice(selected.cacheRead),
        cacheCreationCost: formatPrice(selected.cacheWrite),
      });

      toast.success(
        t("usage.modelsDevImported", {
          name: selected.modelName,
          defaultValue: "Imported pricing for {{name}}",
        }),
        { closeButton: true },
      );
      onImported();
    } catch (error) {
      toast.error(String(error));
    }
  };

  const priceColumns = (entry: ModelsDevEntry) =>
    [
      { label: t("usage.inputCost", "Input Cost"), value: entry.input },
      { label: t("usage.outputCost", "Output Cost"), value: entry.output },
      { label: t("usage.cacheReadCost", "Cache Hit"), value: entry.cacheRead },
      {
        label: t("usage.cacheWriteCost", "Cache Creation"),
        value: entry.cacheWrite,
      },
    ] as const;

  return (
    <Dialog
      open={open}
      onOpenChange={(nextOpen) => {
        if (!nextOpen && !updatePricing.isPending) {
          onClose();
        }
      }}
    >
      <DialogContent
        zIndex="top"
        className="max-w-3xl h-[80vh]"
        onEscapeKeyDown={(e) => {
          // Pressing Escape in the search field must not close the dialog and discard the selected model, matching FullScreenPanel behavior.
          if (isTextEditableTarget(e.target)) {
            e.preventDefault();
          }
        }}
      >
        <DialogHeader>
          <DialogTitle>
            {t("usage.modelsDevPickerTitle", "Import Pricing from models.dev")}
          </DialogTitle>
          <DialogDescription>
            {t(
              "usage.modelsDevPickerDesc",
              "Select a model to import (prices in USD per million tokens). One model per import.",
            )}
          </DialogDescription>
        </DialogHeader>

        <div className="flex flex-1 min-h-0 flex-col gap-3 px-6 py-4">
          {isLoading ? (
            <div className="flex flex-1 items-center justify-center">
              <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
            </div>
          ) : error ? (
            <Alert variant="destructive">
              <AlertDescription className="flex items-center justify-between gap-3">
                <span>
                  {t(
                    "usage.modelsDevLoadError",
                    "Failed to load models.dev data",
                  )}
                  : {error instanceof Error ? error.message : String(error)}
                </span>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => refetch()}
                  className="shrink-0"
                >
                  {t("usage.modelsDevRetry", "Retry")}
                </Button>
              </AlertDescription>
            </Alert>
          ) : (
            <>
              <div className="flex items-center gap-2">
                <Select
                  value={providerFilter}
                  onValueChange={setProviderFilter}
                >
                  <SelectTrigger className="w-44 shrink-0">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent className="z-[120] max-h-[min(24rem,var(--radix-select-content-available-height))]">
                    <SelectItem value="all">
                      {t("usage.modelsDevAllProviders", "All providers")}
                    </SelectItem>
                    {providers.map((provider) => (
                      <SelectItem key={provider.id} value={provider.id}>
                        {provider.name}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <div className="relative flex-1">
                  <Search className="absolute left-2.5 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
                  <Input
                    value={search}
                    onChange={(e) => setSearch(e.target.value)}
                    placeholder={t(
                      "usage.modelsDevSearchPlaceholder",
                      "Search models or providers (full search)...",
                    )}
                    className="pl-8"
                  />
                </div>
              </div>

              <div className="flex-1 min-h-0 overflow-y-auto rounded-md border border-border/50">
                {filtered.length === 0 ? (
                  <div className="flex h-full items-center justify-center py-8 text-sm text-muted-foreground">
                    {t("usage.modelsDevNoResults", "No matching models")}
                  </div>
                ) : (
                  <div className="divide-y divide-border/30">
                    {visible.map((entry) => (
                      <div
                        key={entry.key}
                        role="button"
                        aria-pressed={selected?.key === entry.key}
                        onClick={() => toggleEntry(entry)}
                        className={`flex cursor-pointer items-center gap-3 px-3 py-2 ${
                          selected?.key === entry.key
                            ? "bg-accent/50"
                            : "hover:bg-muted/40"
                        }`}
                      >
                        <Check
                          className={`h-4 w-4 shrink-0 text-primary ${
                            selected?.key === entry.key
                              ? "visible"
                              : "invisible"
                          }`}
                        />
                        <div className="min-w-0 flex-1">
                          <div className="flex items-center gap-2">
                            <span className="truncate text-sm font-medium">
                              {entry.modelName}
                            </span>
                            <span className="shrink-0 text-xs text-muted-foreground">
                              {entry.providerName}
                            </span>
                            {entry.releaseDate && (
                              <span className="shrink-0 text-[10px] text-muted-foreground/70">
                                {entry.releaseDate}
                              </span>
                            )}
                          </div>
                          <div
                            className="truncate font-mono text-xs text-muted-foreground"
                            title={entry.modelId}
                          >
                            {entry.normalizedId}
                          </div>
                        </div>
                        <div className="flex shrink-0 gap-3 text-right">
                          {priceColumns(entry).map((column) => (
                            <div key={column.label} className="w-16">
                              <div className="text-[10px] text-muted-foreground">
                                {column.label}
                              </div>
                              <div className="font-mono text-xs">
                                ${formatPrice(column.value)}
                              </div>
                            </div>
                          ))}
                        </div>
                      </div>
                    ))}
                    {filtered.length > visible.length && (
                      <div className="px-3 py-2 text-center text-xs text-muted-foreground">
                        {isFiltering
                          ? t("usage.modelsDevTruncated", {
                              shown: visible.length,
                              total: filtered.length,
                              defaultValue:
                                "Showing first {{shown}} of {{total}} results — refine your search",
                            })
                          : t("usage.modelsDevDefaultHint", {
                              shown: visible.length,
                              total: filtered.length,
                              defaultValue:
                                "Showing the {{shown}} most recently released models (of {{total}}) — type to search all",
                            })}
                      </div>
                    )}
                  </div>
                )}
              </div>
            </>
          )}
        </div>

        <DialogFooter>
          <Button
            variant="outline"
            onClick={onClose}
            disabled={updatePricing.isPending}
          >
            {t("common.cancel", "Cancel")}
          </Button>
          <Button
            onClick={handleImport}
            disabled={!selected || updatePricing.isPending}
          >
            {updatePricing.isPending ? (
              <>
                <Loader2 className="mr-1.5 h-4 w-4 animate-spin" />
                {t("usage.modelsDevImporting", "Importing...")}
              </>
            ) : (
              t("usage.modelsDevImportButton", "Import")
            )}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
