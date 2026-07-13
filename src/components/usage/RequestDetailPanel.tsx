import { useTranslation } from "react-i18next";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { useRequestDetail } from "@/lib/query/usage";
import {
  CACHE_INCLUSIVE_APP_TYPES,
  getFreshInputTokens,
  isUnpricedUsage,
} from "@/types/usage";
import { UsageQueryState } from "./UsageQueryState";

interface RequestDetailPanelProps {
  requestId: string;
  onClose: () => void;
}

export function RequestDetailPanel({
  requestId,
  onClose,
}: RequestDetailPanelProps) {
  const { t, i18n } = useTranslation();
  const {
    data: request,
    isLoading,
    isError,
    refetch,
  } = useRequestDetail(requestId);
  const dateLocale = i18n.language === "vi" ? "vi-VN" : "en-US";

  if (isLoading) {
    return (
      <Dialog open onOpenChange={onClose}>
        <DialogContent className="max-w-2xl">
          <UsageQueryState state="loading" className="min-h-[400px]" />
        </DialogContent>
      </Dialog>
    );
  }

  if (isError) {
    return (
      <Dialog open onOpenChange={onClose}>
        <DialogContent className="max-w-2xl">
          <DialogHeader>
            <DialogTitle>
              {t("usage.requestDetail", "Request Detail")}
            </DialogTitle>
          </DialogHeader>
          <UsageQueryState state="error" onRetry={() => void refetch()} />
        </DialogContent>
      </Dialog>
    );
  }

  if (!request) {
    return (
      <Dialog open onOpenChange={onClose}>
        <DialogContent className="max-w-2xl">
          <DialogHeader>
            <DialogTitle>
              {t("usage.requestDetail", "Request Detail")}
            </DialogTitle>
          </DialogHeader>
          <div className="text-center text-muted-foreground">
            {t("usage.requestNotFound", "Request not found")}
          </div>
        </DialogContent>
      </Dialog>
    );
  }

  const freshInput = getFreshInputTokens(request);
  const isCacheInclusive = request.inputTokens !== freshInput;
  const cacheCreationUnavailable = CACHE_INCLUSIVE_APP_TYPES.has(
    request.appType,
  );
  const hasMeasuredOutcome = (request.dataSource || "proxy") === "proxy";
  const tokenUsageKnown = request.tokenUsageKnown !== false;
  const unpriced = isUnpricedUsage(request);
  // Explicit null is the v13 legacy-indeterminate state. An omitted property
  // can come from an older backend and must retain isUnpricedUsage's numeric
  // compatibility fallback, matching RequestLogTable.
  const pricingIndeterminate = request.pricingKnown === null;
  return (
    <Dialog open onOpenChange={onClose}>
      <DialogContent className="max-w-2xl max-h-[80vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>
            {t("usage.requestDetail", "Request Detail")}
          </DialogTitle>
        </DialogHeader>

        <div className="space-y-4">
          {/* Basic information */}
          <div className="rounded-lg border p-4">
            <h3 className="mb-3 font-semibold">
              {t("usage.basicInfo", "Basic Info")}
            </h3>
            <dl className="grid grid-cols-2 gap-3 text-sm">
              <div>
                <dt className="text-muted-foreground">
                  {t("usage.requestId", "Request ID")}
                </dt>
                <dd className="font-mono">{request.requestId}</dd>
              </div>
              <div>
                <dt className="text-muted-foreground">
                  {t("usage.time", "Time")}
                </dt>
                <dd>
                  {new Date(request.createdAt * 1000).toLocaleString(
                    dateLocale,
                  )}
                </dd>
              </div>
              <div>
                <dt className="text-muted-foreground">
                  {t("usage.provider", "Provider")}
                </dt>
                <dd className="text-sm">
                  <span className="font-medium">
                    {request.providerName ||
                      t("usage.unknownProvider", "Unknown Provider")}
                  </span>
                  <span className="ml-2 font-mono text-xs text-muted-foreground">
                    {request.providerId}
                  </span>
                </dd>
              </div>
              <div>
                <dt className="text-muted-foreground">
                  {t("usage.appType", "App Type")}
                </dt>
                <dd>{request.appType}</dd>
              </div>
              <div>
                <dt className="text-muted-foreground">
                  {t("usage.model", "Model")}
                </dt>
                <dd className="font-mono">{request.model}</dd>
                {request.requestModel &&
                  request.requestModel !== request.model && (
                    <>
                      <dt className="mt-1 text-muted-foreground">
                        {t("usage.requestModel", "Request Model")}
                      </dt>
                      <dd className="font-mono text-xs">
                        {request.requestModel}
                      </dd>
                    </>
                  )}
                <dt className="mt-1 text-muted-foreground">
                  {t("usage.pricingModel", "Pricing Model")}
                </dt>
                <dd
                  className="font-mono text-xs"
                  title={
                    request.pricingModel?.trim()
                      ? undefined
                      : t(
                          "usage.pricingModelUnavailable",
                          "The pricing model was not recorded for this request.",
                        )
                  }
                >
                  {request.pricingModel?.trim() ||
                    t("usage.notMeasured", "N/A")}
                </dd>
              </div>
              <div>
                <dt className="text-muted-foreground">
                  {t("usage.status", "Status")}
                </dt>
                <dd>
                  {hasMeasuredOutcome ? (
                    <span
                      className={`inline-flex rounded-full px-2 py-1 text-xs ${
                        request.statusCode >= 200 && request.statusCode < 300
                          ? "bg-green-100 text-green-800"
                          : "bg-red-100 text-red-800"
                      }`}
                    >
                      {request.statusCode}
                    </span>
                  ) : (
                    <span className="text-muted-foreground">
                      {t("usage.notMeasured", "N/A")}
                    </span>
                  )}
                </dd>
              </div>
              <div>
                <dt className="text-muted-foreground">
                  {t("usage.source", "Source")}
                </dt>
                <dd>{request.dataSource || "proxy"}</dd>
              </div>
              {!hasMeasuredOutcome && (
                <div
                  className="col-span-2 text-xs text-muted-foreground"
                  role="note"
                >
                  {t(
                    "usage.importedMetricsUnavailable",
                    "Imported sessions do not include measured status or timing.",
                  )}
                </div>
              )}
            </dl>
          </div>

          {/* Token usage */}
          <div className="rounded-lg border p-4">
            <h3 className="mb-3 font-semibold">
              {t("usage.tokenUsage", "Token Usage")}
            </h3>
            {tokenUsageKnown ? (
              <>
                <dl className="grid grid-cols-2 gap-3 text-sm">
                  <div>
                    <dt className="text-muted-foreground">
                      {t("usage.freshInput", "Fresh Input")}
                    </dt>
                    <dd className="font-mono">
                      {freshInput.toLocaleString()}
                      {isCacheInclusive && (
                        <span className="ml-2 text-xs text-muted-foreground/70 font-normal">
                          ({t("usage.rawInputLabel", "Raw")}:{" "}
                          {request.inputTokens.toLocaleString()})
                        </span>
                      )}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-muted-foreground">
                      {t("usage.outputTokens", "Output")}
                    </dt>
                    <dd className="font-mono">
                      {request.outputTokens.toLocaleString()}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-muted-foreground">
                      {t("usage.cacheReadTokens", "Cache Hit")}
                    </dt>
                    <dd className="font-mono">
                      {request.cacheReadTokens.toLocaleString()}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-muted-foreground">
                      {t("usage.cacheCreationTokens", "Cache Creation")}
                    </dt>
                    <dd
                      className="font-mono"
                      title={
                        cacheCreationUnavailable
                          ? t(
                              "usage.cacheWriteNotReported",
                              "OpenAI-style protocols do not report cache creation separately.",
                            )
                          : undefined
                      }
                    >
                      {cacheCreationUnavailable
                        ? "N/A"
                        : request.cacheCreationTokens.toLocaleString()}
                    </dd>
                  </div>
                  <div className="col-span-2">
                    <dt className="text-muted-foreground">
                      {t("usage.totalTokens", "New Tokens (Input + Output)")}
                    </dt>
                    <dd className="text-lg font-semibold">
                      {(freshInput + request.outputTokens).toLocaleString()}
                    </dd>
                  </div>
                </dl>
                {cacheCreationUnavailable && (
                  <p className="mt-3 text-xs text-muted-foreground" role="note">
                    {t(
                      "usage.cacheWriteNotReported",
                      "OpenAI-style protocols do not report cache creation separately.",
                    )}
                  </p>
                )}
                <p className="mt-3 text-xs text-muted-foreground" role="note">
                  {t(
                    "usage.outputIncludesReasoning",
                    "Output includes reasoning or thinking tokens when the source reports them; reasoning is not tracked separately.",
                  )}
                </p>
              </>
            ) : (
              <p className="text-sm text-muted-foreground" role="note">
                {t(
                  "usage.tokenUsageUnavailable",
                  "The upstream response did not report token usage. Token totals and estimated costs exclude this request.",
                )}
              </p>
            )}
          </div>

          {/* Cost breakdown */}
          <div className="rounded-lg border p-4">
            <h3 className="mb-3 font-semibold">
              {t("usage.costBreakdown", "Cost Breakdown")}
            </h3>
            {tokenUsageKnown && !unpriced && !pricingIndeterminate ? (
              <>
                <p className="mb-3 text-xs text-muted-foreground" role="note">
                  {t(
                    "usage.costEstimateSemantics",
                    "Costs are estimates from configured per-token list prices and multipliers. They are not infrastructure spend or provider invoices.",
                  )}
                </p>
                <dl className="grid grid-cols-2 gap-3 text-sm">
                  <div>
                    <dt className="text-muted-foreground">
                      {t("usage.inputCost", "Input Cost")}
                      <span className="ml-1 text-xs">
                        ({t("usage.baseCost", "Base")})
                      </span>
                    </dt>
                    <dd className="font-mono">
                      ${parseFloat(request.inputCostUsd).toFixed(6)}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-muted-foreground">
                      {t("usage.outputCost", "Output Cost")}
                      <span className="ml-1 text-xs">
                        ({t("usage.baseCost", "Base")})
                      </span>
                    </dt>
                    <dd className="font-mono">
                      ${parseFloat(request.outputCostUsd).toFixed(6)}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-muted-foreground">
                      {t("usage.cacheReadCost", "Cache Hit Cost")}
                      <span className="ml-1 text-xs">
                        ({t("usage.baseCost", "Base")})
                      </span>
                    </dt>
                    <dd className="font-mono">
                      ${parseFloat(request.cacheReadCostUsd).toFixed(6)}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-muted-foreground">
                      {t("usage.cacheCreationCost", "Cache Creation Cost")}
                      <span className="ml-1 text-xs">
                        ({t("usage.baseCost", "Base")})
                      </span>
                    </dt>
                    <dd className="font-mono">
                      {cacheCreationUnavailable
                        ? "N/A"
                        : `$${parseFloat(request.cacheCreationCostUsd).toFixed(6)}`}
                    </dd>
                  </div>
                  {/* Show a non-default cost multiplier. */}
                  {request.costMultiplier &&
                    parseFloat(request.costMultiplier) !== 1 && (
                      <div className="col-span-2 border-t pt-3">
                        <dt className="text-muted-foreground">
                          {t("usage.costMultiplier", "Cost Multiplier")}
                        </dt>
                        <dd className="font-mono">×{request.costMultiplier}</dd>
                      </div>
                    )}
                  <div
                    className={`col-span-2 ${request.costMultiplier && parseFloat(request.costMultiplier) !== 1 ? "" : "border-t"} pt-3`}
                  >
                    <dt className="text-muted-foreground">
                      {t("usage.estimatedCost", "Estimated Cost")}
                      {request.costMultiplier &&
                        parseFloat(request.costMultiplier) !== 1 && (
                          <span className="ml-1 text-xs">
                            ({t("usage.withMultiplier", "with multiplier")})
                          </span>
                        )}
                    </dt>
                    <dd
                      className={`text-lg font-semibold ${
                        unpriced ? "text-muted-foreground" : "text-primary"
                      }`}
                    >
                      {unpriced
                        ? t("usage.unpriced", "Unpriced")
                        : `$${parseFloat(request.totalCostUsd).toFixed(6)}`}
                    </dd>
                  </div>
                </dl>
              </>
            ) : tokenUsageKnown && pricingIndeterminate ? (
              <p className="text-sm text-muted-foreground" role="note">
                {t(
                  "usage.pricingUnknownLegacyRequest",
                  "Pricing is unknown for this legacy request; its stored zero cannot distinguish a free rule from missing pricing.",
                )}
              </p>
            ) : tokenUsageKnown ? (
              <p className="text-sm text-muted-foreground" role="note">
                {t(
                  "usage.pricingUnavailable",
                  "Unpriced: token usage was reported, but no configured pricing rule matched this request.",
                )}
              </p>
            ) : (
              <p className="text-sm text-muted-foreground" role="note">
                {t(
                  "usage.tokenCostUnavailable",
                  "N/A because the upstream response did not report token usage.",
                )}
              </p>
            )}
          </div>

          {/* Performance */}
          <div className="rounded-lg border p-4">
            <h3 className="mb-3 font-semibold">
              {t("usage.performance", "Performance")}
            </h3>
            <dl className="grid grid-cols-2 gap-3 text-sm">
              <div>
                <dt className="text-muted-foreground">
                  {t("usage.latency", "Latency")}
                </dt>
                <dd className="font-mono">
                  {hasMeasuredOutcome
                    ? `${request.latencyMs}ms`
                    : t("usage.notMeasured", "N/A")}
                </dd>
              </div>
              <div>
                <dt className="text-muted-foreground">
                  {t("usage.firstToken", "First meaningful token")}
                </dt>
                <dd className="font-mono">
                  {hasMeasuredOutcome && request.firstTokenMs != null
                    ? `${request.firstTokenMs}ms`
                    : t("usage.notMeasured", "N/A")}
                </dd>
              </div>
            </dl>
          </div>

          {/* Error information */}
          {request.errorMessage && (
            <div className="rounded-lg border border-red-200 bg-red-50 p-4">
              <h3 className="mb-2 font-semibold text-red-800">
                {t("usage.errorMessage", "Error Message")}
              </h3>
              <p className="text-sm text-red-700">{request.errorMessage}</p>
            </div>
          )}

        </div>
      </DialogContent>
    </Dialog>
  );
}
