import React from "react";
import type { ProviderMeta } from "@/types";
import { useCodexOauthQuota } from "@/lib/query/subscription";
import { SubscriptionQuotaView } from "@/components/SubscriptionQuotaFooter";

interface CodexOauthQuotaFooterProps {
  meta?: ProviderMeta;
  inline?: boolean;
  /** Whether this is the active provider. */
  isCurrent?: boolean;
}

/**
 * Subscription quota footer for Codex OAuth ChatGPT Plus/Pro routing.
 *
 * Reuses all SubscriptionQuotaView rendering states in inline and expanded
 * modes, using an OAuth token managed by nexus-composer instead of Codex CLI
 * credentials.
 */
const CodexOauthQuotaFooter: React.FC<CodexOauthQuotaFooterProps> = ({
  meta,
  inline = false,
  isCurrent = false,
}) => {
  const {
    data: quota,
    isFetching: loading,
    refetch,
  } = useCodexOauthQuota(meta, { enabled: true, autoQuery: isCurrent });

  return (
    <SubscriptionQuotaView
      quota={quota}
      loading={loading}
      refetch={refetch}
      appIdForExpiredHint="codex_oauth"
      inline={inline}
    />
  );
};

export default CodexOauthQuotaFooter;
