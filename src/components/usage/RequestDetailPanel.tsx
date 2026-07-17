import { useTranslation } from "react-i18next";
import { Copy, X } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { copyText } from "@/lib/clipboard";
import { useRequestDetail } from "@/lib/query/usage";
import { getFreshInputTokens, isUnpricedUsage } from "@/types/usage";

interface RequestDetailPanelProps {
  requestId: string;
  onClose: () => void;
}

function RequestDetailCloseButton({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation();

  return (
    <Button
      type="button"
      variant="ghost"
      size="icon"
      className="absolute right-4 top-4"
      aria-label={t("common.close")}
      onClick={onClose}
    >
      <X className="size-4" />
    </Button>
  );
}

function CopyableId({
  label,
  value,
  copyLabel,
}: {
  label: string;
  value: string;
  copyLabel: string;
}) {
  return (
    <div>
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="flex items-center gap-1 font-mono">
        <span className="min-w-0 break-all">{value}</span>
        <Button
          type="button"
          variant="ghost"
          size="icon"
          className="size-7 shrink-0"
          aria-label={copyLabel}
          onClick={() => void copyText(value)}
        >
          <Copy className="size-3.5" />
        </Button>
      </dd>
    </div>
  );
}

export function RequestDetailPanel({
  requestId,
  onClose,
}: RequestDetailPanelProps) {
  const { t, i18n } = useTranslation();
  const { data: request, isLoading } = useRequestDetail(requestId);
  const dateLocale = i18n.language === "vi" ? "vi-VN" : "en-US";
  const handleOpenChange = (open: boolean) => {
    if (!open) onClose();
  };

  if (isLoading) {
    return (
      <Dialog open onOpenChange={handleOpenChange}>
        <DialogContent className="max-w-2xl">
          <RequestDetailCloseButton onClose={onClose} />
          <div className="h-[400px] animate-pulse rounded bg-gray-100" />
        </DialogContent>
      </Dialog>
    );
  }

  if (!request) {
    return (
      <Dialog open onOpenChange={handleOpenChange}>
        <DialogContent className="max-w-2xl">
          <RequestDetailCloseButton onClose={onClose} />
          <DialogHeader>
            <DialogTitle>{t("usage.requestDetail")}</DialogTitle>
          </DialogHeader>
          <div className="text-center text-muted-foreground">
            {t("usage.requestNotFound")}
          </div>
        </DialogContent>
      </Dialog>
    );
  }

  const freshInput = getFreshInputTokens(request);
  const isCacheInclusive = request.inputTokens !== freshInput;
  const unpriced = isUnpricedUsage(request);
  const correlationId = request.correlationId;
  const sessionId = request.sessionId;

  return (
    <Dialog open onOpenChange={handleOpenChange}>
      <DialogContent className="max-w-2xl max-h-[80vh] overflow-y-auto">
        <RequestDetailCloseButton onClose={onClose} />
        <DialogHeader>
          <DialogTitle>{t("usage.requestDetail")}</DialogTitle>
        </DialogHeader>

        <div className="space-y-4">
          {/* Basic information */}
          <div className="rounded-lg border p-4">
            <h3 className="mb-3 font-semibold">{t("usage.basicInfo")}</h3>
            <dl className="grid grid-cols-2 gap-3 text-sm">
              <CopyableId
                label={t("usage.requestId")}
                value={request.requestId}
                copyLabel={t("usage.copyRequestId")}
              />
              {correlationId && (
                <CopyableId
                  label={t("usage.correlationId")}
                  value={correlationId}
                  copyLabel={t("usage.copyCorrelationId")}
                />
              )}
              {sessionId && (
                <CopyableId
                  label={t("usage.sessionId")}
                  value={sessionId}
                  copyLabel={t("usage.copySessionId")}
                />
              )}
              <div>
                <dt className="text-muted-foreground">{t("usage.time")}</dt>
                <dd>
                  {new Date(request.createdAt * 1000).toLocaleString(
                    dateLocale,
                  )}
                </dd>
              </div>
              <div>
                <dt className="text-muted-foreground">{t("usage.provider")}</dt>
                <dd className="text-sm">
                  <span className="font-medium">
                    {request.providerName || t("usage.unknownProvider")}
                  </span>
                  <span className="ml-2 font-mono text-xs text-muted-foreground">
                    {request.providerId}
                  </span>
                </dd>
              </div>
              <div>
                <dt className="text-muted-foreground">{t("usage.appType")}</dt>
                <dd>{request.appType}</dd>
              </div>
              <div>
                <dt className="text-muted-foreground">{t("usage.model")}</dt>
                <dd className="font-mono">{request.model}</dd>
                {request.requestModel &&
                  request.requestModel !== request.model && (
                    <>
                      <dt className="mt-1 text-muted-foreground">
                        {t("usage.requestModel")}
                      </dt>
                      <dd className="font-mono text-xs">
                        {request.requestModel}
                      </dd>
                    </>
                  )}
                {request.pricingModel &&
                  request.pricingModel !== request.model && (
                    <>
                      <dt className="mt-1 text-muted-foreground">
                        {t("usage.pricingModel")}
                      </dt>
                      <dd className="font-mono text-xs">
                        {request.pricingModel}
                      </dd>
                    </>
                  )}
              </div>
              <div>
                <dt className="text-muted-foreground">{t("usage.status")}</dt>
                <dd>
                  <span
                    className={`inline-flex rounded-full px-2 py-1 text-xs ${
                      request.statusCode >= 200 && request.statusCode < 300
                        ? "bg-green-100 text-green-800"
                        : "bg-red-100 text-red-800"
                    }`}
                  >
                    {request.statusCode}
                  </span>
                </dd>
              </div>
            </dl>
          </div>

          {/* Token usage */}
          <div className="rounded-lg border p-4">
            <h3 className="mb-3 font-semibold">{t("usage.tokenUsage")}</h3>
            <dl className="grid grid-cols-2 gap-3 text-sm">
              <div>
                <dt className="text-muted-foreground">
                  {t("usage.inputTokens")}
                </dt>
                <dd className="font-mono">
                  {freshInput.toLocaleString()}
                  {isCacheInclusive && (
                    <span className="ml-2 text-xs text-muted-foreground/70 font-normal">
                      ({t("usage.rawInputLabel")}:{" "}
                      {request.inputTokens.toLocaleString()})
                    </span>
                  )}
                </dd>
              </div>
              <div>
                <dt className="text-muted-foreground">
                  {t("usage.outputTokens")}
                </dt>
                <dd className="font-mono">
                  {request.outputTokens.toLocaleString()}
                </dd>
              </div>
              <div>
                <dt className="text-muted-foreground">
                  {t("usage.cacheReadTokens")}
                </dt>
                <dd className="font-mono">
                  {request.cacheReadTokens.toLocaleString()}
                </dd>
              </div>
              <div>
                <dt className="text-muted-foreground">
                  {t("usage.cacheCreationTokens")}
                </dt>
                <dd className="font-mono">
                  {request.cacheCreationTokens.toLocaleString()}
                </dd>
              </div>
              <div className="col-span-2">
                <dt className="text-muted-foreground">
                  {t("usage.totalTokens")}
                </dt>
                <dd className="text-lg font-semibold">
                  {(freshInput + request.outputTokens).toLocaleString()}
                </dd>
              </div>
            </dl>
          </div>

          {/* Cost breakdown */}
          <div className="rounded-lg border p-4">
            <h3 className="mb-3 font-semibold">{t("usage.costBreakdown")}</h3>
            <dl className="grid grid-cols-2 gap-3 text-sm">
              <div>
                <dt className="text-muted-foreground">
                  {t("usage.inputCost")}
                  <span className="ml-1 text-xs">({t("usage.baseCost")})</span>
                </dt>
                <dd className="font-mono">
                  ${parseFloat(request.inputCostUsd).toFixed(6)}
                </dd>
              </div>
              <div>
                <dt className="text-muted-foreground">
                  {t("usage.outputCost")}
                  <span className="ml-1 text-xs">({t("usage.baseCost")})</span>
                </dt>
                <dd className="font-mono">
                  ${parseFloat(request.outputCostUsd).toFixed(6)}
                </dd>
              </div>
              <div>
                <dt className="text-muted-foreground">
                  {t("usage.cacheReadCost")}
                  <span className="ml-1 text-xs">({t("usage.baseCost")})</span>
                </dt>
                <dd className="font-mono">
                  ${parseFloat(request.cacheReadCostUsd).toFixed(6)}
                </dd>
              </div>
              <div>
                <dt className="text-muted-foreground">
                  {t("usage.cacheCreationCost")}
                  <span className="ml-1 text-xs">({t("usage.baseCost")})</span>
                </dt>
                <dd className="font-mono">
                  ${parseFloat(request.cacheCreationCostUsd).toFixed(6)}
                </dd>
              </div>
              {/* Show a non-default cost multiplier. */}
              {request.costMultiplier &&
                parseFloat(request.costMultiplier) !== 1 && (
                  <div className="col-span-2 border-t pt-3">
                    <dt className="text-muted-foreground">
                      {t("usage.costMultiplier")}
                    </dt>
                    <dd className="font-mono">×{request.costMultiplier}</dd>
                  </div>
                )}
              <div
                className={`col-span-2 ${request.costMultiplier && parseFloat(request.costMultiplier) !== 1 ? "" : "border-t"} pt-3`}
              >
                <dt className="text-muted-foreground">
                  {t("usage.totalCost")}
                  {request.costMultiplier &&
                    parseFloat(request.costMultiplier) !== 1 && (
                      <span className="ml-1 text-xs">
                        ({t("usage.withMultiplier")})
                      </span>
                    )}
                </dt>
                <dd
                  className={`text-lg font-semibold ${
                    unpriced ? "text-muted-foreground" : "text-primary"
                  }`}
                >
                  {unpriced
                    ? t("usage.unpriced")
                    : `$${parseFloat(request.totalCostUsd).toFixed(6)}`}
                </dd>
              </div>
            </dl>
          </div>

          {/* Performance */}
          <div className="rounded-lg border p-4">
            <h3 className="mb-3 font-semibold">{t("usage.performance")}</h3>
            <dl className="grid grid-cols-2 gap-3 text-sm">
              <div>
                <dt className="text-muted-foreground">{t("usage.latency")}</dt>
                <dd className="font-mono">{request.latencyMs}ms</dd>
              </div>
            </dl>
          </div>

          {/* Error information */}
          {request.errorMessage && (
            <div className="rounded-lg border border-red-200 bg-red-50 p-4">
              <h3 className="mb-2 font-semibold text-red-800">
                {t("usage.errorMessage")}
              </h3>
              <p className="text-sm text-red-700">{request.errorMessage}</p>
            </div>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
