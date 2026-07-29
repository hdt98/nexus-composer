import React from "react";
import { RefreshCw, AlertCircle, Clock } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { AppId } from "@/lib/api";
import { useSubscriptionQuota } from "@/lib/query/subscription";
import type { QuotaTier, SubscriptionQuota } from "@/types/subscription";

interface SubscriptionQuotaFooterProps {
  appId: AppId;
  inline?: boolean;
  isCurrent?: boolean;
  autoQueryInterval?: number;
}

interface SubscriptionQuotaViewProps {
  quota: SubscriptionQuota | undefined;
  loading: boolean;
  refetch: () => void;
  /** Supplies the {tool} interpolation for `subscription.expiredHint` without coupling the view to the hook's appId. */
  appIdForExpiredHint: string;
  inline?: boolean;
}

/** Display-key mapping for known tier names, shared by official subscriptions and token plans. */
export const TIER_I18N_KEYS: Record<string, string> = {
  five_hour: "subscription.fiveHour",
  seven_day: "subscription.sevenDay",
  seven_day_opus: "subscription.sevenDayOpus",
  seven_day_sonnet: "subscription.sevenDaySonnet",
  // The Codex free plan uses a 30-day secondary window; paid plans use 7 days.
  "30_day": "subscription.thirtyDay",
  // Gemini model categories.
  gemini_pro: "subscription.geminiPro",
  gemini_flash: "subscription.geminiFlash",
  gemini_flash_lite: "subscription.geminiFlashLite",
  // Token plans; five_hour already uses the official mapping above.
  weekly_limit: "subscription.sevenDay",
  // Monthly window for Volcano Ark Agent and Coding plans.
  monthly: "subscription.monthly",
  // GitHub Copilot
  premium: "subscription.copilotPremium",
};

/** Return a color class for the utilization percentage. */
export function utilizationColor(utilization: number): string {
  if (utilization >= 90) return "text-red-500 dark:text-red-400";
  if (utilization >= 70) return "text-orange-500 dark:text-orange-400";
  return "text-green-600 dark:text-green-400";
}

/** Return a plain countdown string such as "2h30m" or "3d12h". */
export function countdownStr(resetsAt: string | null): string | null {
  if (!resetsAt) return null;
  const diffMs = new Date(resetsAt).getTime() - Date.now();
  if (diffMs <= 0) return null;

  const hours = Math.floor(diffMs / (1000 * 60 * 60));
  const minutes = Math.floor((diffMs % (1000 * 60 * 60)) / (1000 * 60));

  if (hours > 24) {
    const days = Math.floor(hours / 24);
    return `${days}d${hours % 24}h`;
  }
  if (hours > 0) return `${hours}h${minutes}m`;
  return `${minutes}m`;
}

/** Format the reset time as a localized countdown. */
function formatResetTime(
  resetsAt: string | null,
  t: (key: string, options?: Record<string, string>) => string,
): string | null {
  const time = countdownStr(resetsAt);
  if (!time) return null;
  return t("subscription.resetsIn", { time });
}

/** Tiers hidden from the inline layout. */
const HIDDEN_INLINE_TIERS = new Set(["seven_day_sonnet"]);

/** Format relative time consistently with UsageFooter. */
function formatRelativeTime(
  timestamp: number,
  now: number,
  t: (key: string, options?: { count?: number }) => string,
): string {
  const diff = Math.floor((now - timestamp) / 1000);
  if (diff < 60) return t("usage.justNow");
  if (diff < 3600)
    return t("usage.minutesAgo", { count: Math.floor(diff / 60) });
  if (diff < 86400)
    return t("usage.hoursAgo", { count: Math.floor(diff / 3600) });
  return t("usage.daysAgo", { count: Math.floor(diff / 86400) });
}

/**
 * Presentation-only component for the five SubscriptionQuota states
 * (not_found, parse_error, expired, API failure, and success) in inline or
 * expanded layouts.
 *
 * The caller injects data from its hook so different quota backends can reuse
 * the same rendering logic:
 * - `SubscriptionQuotaFooter`: CLI credential path, keyed by appId.
 * - `CodexOauthQuotaFooter`: Nexus Composer-managed OAuth path, keyed by ChatGPT account.
 */
export const SubscriptionQuotaView: React.FC<SubscriptionQuotaViewProps> = ({
  quota,
  loading,
  refetch,
  appIdForExpiredHint,
  inline = false,
}) => {
  const { t } = useTranslation();

  // Update relative timestamps periodically.
  const [now, setNow] = React.useState(Date.now());
  React.useEffect(() => {
    if (!quota?.queriedAt) return;
    const interval = setInterval(() => setNow(Date.now()), 30000);
    return () => clearInterval(interval);
  }, [quota?.queriedAt]);

  // Render nothing when credentials are unavailable.
  if (!quota || quota.credentialStatus === "not_found") return null;

  // Silently render nothing when credentials cannot be parsed.
  if (quota.credentialStatus === "parse_error") return null;

  // Expired credentials.
  if (quota.credentialStatus === "expired" && !quota.success) {
    if (inline) {
      return (
        <div className="inline-flex items-center gap-2 text-xs rounded-lg border border-amber-200 dark:border-amber-800 bg-amber-50 dark:bg-amber-900/20 px-3 py-2 shadow-sm">
          <div className="flex items-center gap-1.5 text-amber-600 dark:text-amber-400">
            <AlertCircle size={12} />
            <span>{t("subscription.expired")}</span>
          </div>
          <button
            onClick={() => refetch()}
            disabled={loading}
            className="p-1 rounded hover:bg-muted transition-colors disabled:opacity-50 flex-shrink-0"
            title={t("subscription.refresh")}
          >
            <RefreshCw size={12} className={loading ? "animate-spin" : ""} />
          </button>
        </div>
      );
    }
    return (
      <div className="mt-3 rounded-xl border border-amber-200 dark:border-amber-800 bg-amber-50 dark:bg-amber-900/20 px-4 py-3 shadow-sm">
        <div className="flex items-center justify-between gap-2 text-xs">
          <div className="flex items-center gap-2 text-amber-600 dark:text-amber-400">
            <AlertCircle size={14} />
            <div>
              <span className="font-medium">{t("subscription.expired")}</span>
              <span className="ml-2 text-amber-500/70 dark:text-amber-400/70">
                {t("subscription.expiredHint", { tool: appIdForExpiredHint })}
              </span>
            </div>
          </div>
          <button
            onClick={() => refetch()}
            disabled={loading}
            className="p-1 rounded hover:bg-amber-100 dark:hover:bg-amber-800/30 transition-colors disabled:opacity-50 flex-shrink-0"
            title={t("subscription.refresh")}
          >
            <RefreshCw size={12} className={loading ? "animate-spin" : ""} />
          </button>
        </div>
      </div>
    );
  }

  // API request failure.
  if (!quota.success) {
    if (inline) {
      return (
        <div className="inline-flex items-center gap-2 text-xs rounded-lg border border-border-default bg-card px-3 py-2 shadow-sm">
          <div className="flex items-center gap-1.5 text-red-500 dark:text-red-400">
            <AlertCircle size={12} />
            <span>{t("subscription.queryFailed")}</span>
          </div>
          <button
            onClick={() => refetch()}
            disabled={loading}
            className="p-1 rounded hover:bg-muted transition-colors disabled:opacity-50 flex-shrink-0"
            title={t("subscription.refresh")}
          >
            <RefreshCw size={12} className={loading ? "animate-spin" : ""} />
          </button>
        </div>
      );
    }
    return (
      <div className="mt-3 rounded-xl border border-border-default bg-card px-4 py-3 shadow-sm">
        <div className="flex items-center justify-between gap-2 text-xs">
          <div className="flex items-center gap-2 text-red-500 dark:text-red-400">
            <AlertCircle size={14} />
            <span>{quota.error || t("subscription.queryFailed")}</span>
          </div>
          <button
            onClick={() => refetch()}
            disabled={loading}
            className="p-1 rounded hover:bg-gray-100 dark:hover:bg-gray-800 transition-colors disabled:opacity-50 flex-shrink-0"
            title={t("subscription.refresh")}
          >
            <RefreshCw size={12} className={loading ? "animate-spin" : ""} />
          </button>
        </div>
      </div>
    );
  }

  // Successful response.
  const tiers = (quota.tiers || []).filter(
    (tier) => tier.name in TIER_I18N_KEYS,
  );
  if (tiers.length === 0) return null;

  // Compact two-row inline layout.
  if (inline) {
    return (
      <div className="flex flex-col items-end gap-1 text-xs whitespace-nowrap flex-shrink-0">
        {/* First row: query time and refresh */}
        <div className="flex items-center gap-2 justify-end">
          <span className="text-[10px] text-muted-foreground/70 flex items-center gap-1">
            <Clock size={10} />
            {quota.queriedAt
              ? formatRelativeTime(quota.queriedAt, now, t)
              : t("usage.never", { defaultValue: "Never" })}
          </span>
          <button
            onClick={(e) => {
              e.stopPropagation();
              refetch();
            }}
            disabled={loading}
            className="p-1 rounded hover:bg-muted transition-colors disabled:opacity-50 flex-shrink-0 text-muted-foreground"
            title={t("subscription.refresh")}
          >
            <RefreshCw size={12} className={loading ? "animate-spin" : ""} />
          </button>
        </div>

        {/* Second row: utilization percentage for each tier */}
        <div className="flex items-center gap-2">
          {tiers
            .filter((tier) => !HIDDEN_INLINE_TIERS.has(tier.name))
            .map((tier) => (
              <TierBadge key={tier.name} tier={tier} t={t} />
            ))}
        </div>
      </div>
    );
  }

  // Expanded layout with detailed information.
  return (
    <div className="mt-3 rounded-xl border border-border-default bg-card px-4 py-3 shadow-sm">
      <div className="flex items-center justify-between mb-2">
        <span className="text-xs text-gray-500 dark:text-gray-400 font-medium">
          {t("subscription.title", { defaultValue: "Subscription Quota" })}
        </span>
        <div className="flex items-center gap-2">
          {quota.queriedAt && (
            <span className="text-[10px] text-muted-foreground/70 flex items-center gap-1">
              <Clock size={10} />
              {formatRelativeTime(quota.queriedAt, now, t)}
            </span>
          )}
          <button
            onClick={() => refetch()}
            disabled={loading}
            className="p-1 rounded hover:bg-muted transition-colors disabled:opacity-50"
            title={t("subscription.refresh")}
          >
            <RefreshCw size={12} className={loading ? "animate-spin" : ""} />
          </button>
        </div>
      </div>

      <div className="flex flex-col gap-2">
        {tiers.map((tier) => (
          <TierBar key={tier.name} tier={tier} t={t} />
        ))}
      </div>

      {/* Extra usage */}
      {quota.extraUsage?.isEnabled && (
        <div className="mt-2 pt-2 border-t border-border-default text-xs text-gray-500 dark:text-gray-400">
          <span className="font-medium">{t("subscription.extraUsage")}: </span>
          <span className="tabular-nums">
            {quota.extraUsage.currency === "USD" ? "$" : ""}
            {(quota.extraUsage.usedCredits ?? 0).toFixed(2)}
            {quota.extraUsage.monthlyLimit != null && (
              <>
                {" "}
                / {quota.extraUsage.currency === "USD" ? "$" : ""}
                {quota.extraUsage.monthlyLimit.toFixed(2)}
              </>
            )}
          </span>
        </div>
      )}
    </div>
  );
};

/** Render a single tier in the inline layout. */
export const TierBadge: React.FC<{
  tier: QuotaTier;
  t: (key: string, options?: Record<string, unknown>) => string;
}> = ({ tier, t }) => {
  const label = TIER_I18N_KEYS[tier.name]
    ? t(TIER_I18N_KEYS[tier.name])
    : tier.name;
  const countdown = countdownStr(tier.resetsAt);

  const hasUsd = tier.usedValueUsd != null && tier.maxValueUsd != null;

  return (
    <div className="flex items-center gap-0.5">
      <span className="text-gray-500 dark:text-gray-400">{label}:</span>
      <span
        className={`font-semibold tabular-nums ${utilizationColor(tier.utilization)}`}
      >
        {t("subscription.utilization", { value: Math.round(tier.utilization) })}
      </span>
      {hasUsd && (
        <span className="text-muted-foreground/60">
          (${tier.usedValueUsd!.toFixed(2)}/${tier.maxValueUsd!.toFixed(2)})
        </span>
      )}
      {countdown && (
        <span className="text-muted-foreground/60 ml-0.5 flex items-center gap-px">
          <Clock size={10} />
          {countdown}
        </span>
      )}
    </div>
  );
};

/** Render a single tier's progress bar in the expanded layout. */
const TierBar: React.FC<{
  tier: QuotaTier;
  t: (key: string, options?: Record<string, unknown>) => string;
}> = ({ tier, t }) => {
  const label = TIER_I18N_KEYS[tier.name]
    ? t(TIER_I18N_KEYS[tier.name])
    : tier.name;
  const resetText = formatResetTime(tier.resetsAt, t);

  return (
    <div className="flex items-center gap-3 text-xs">
      <span
        className="text-gray-500 dark:text-gray-400 min-w-0 font-medium"
        style={{ width: "25%" }}
      >
        {label}
      </span>

      {/* Progress bar */}
      <div className="flex-1 h-2 bg-gray-100 dark:bg-gray-800 rounded-full overflow-hidden">
        <div
          className={`h-full rounded-full transition-all ${
            tier.utilization >= 90
              ? "bg-red-500"
              : tier.utilization >= 70
                ? "bg-orange-500"
                : "bg-green-500"
          }`}
          style={{ width: `${Math.min(tier.utilization, 100)}%` }}
        />
      </div>

      <div
        className="flex items-center gap-2 flex-shrink-0"
        style={{ width: "30%" }}
      >
        <span
          className={`font-semibold tabular-nums ${utilizationColor(tier.utilization)}`}
        >
          {Math.round(tier.utilization)}%
        </span>
        {resetText && (
          <span
            className="text-[10px] text-muted-foreground/70 truncate"
            title={resetText}
          >
            {resetText}
          </span>
        )}
      </div>
    </div>
  );
};

/**
 * Thin wrapper for the CLI credential path. It loads data through
 * useSubscriptionQuota(appId) and forwards it to SubscriptionQuotaView while
 * preserving the public props and behavior.
 */
const SubscriptionQuotaFooter: React.FC<SubscriptionQuotaFooterProps> = ({
  appId,
  inline = false,
  isCurrent = false,
  autoQueryInterval = 5,
}) => {
  const {
    data: quota,
    isFetching: loading,
    refetch,
  } = useSubscriptionQuota(
    appId,
    isCurrent,
    isCurrent && autoQueryInterval > 0,
    autoQueryInterval,
  );

  if (!isCurrent) return null;

  return (
    <SubscriptionQuotaView
      quota={quota}
      loading={loading}
      refetch={refetch}
      appIdForExpiredHint={appId}
      inline={inline}
    />
  );
};

export default SubscriptionQuotaFooter;
