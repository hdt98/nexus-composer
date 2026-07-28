import type { Provider } from "@/types";
import type { AppId } from "@/lib/api";
import type {
  RemoteAgentStatus,
  SshConnectionTarget,
} from "@/lib/api/providers";
import type { SavedSshConnection } from "@/components/remote/savedSshConnection";
import { parse as parseToml } from "smol-toml";

export const savedSshConnectionToTarget = (
  connection: SavedSshConnection,
): SshConnectionTarget => ({
  type: "manual",
  host: connection.host,
  user: connection.user,
  port: connection.port,
});

const firstNonEmptyString = (...values: unknown[]): string => {
  for (const value of values) {
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return "";
};

const extractCodexModel = (configText: unknown): string => {
  if (typeof configText !== "string") return "";
  try {
    const parsed = parseToml(configText) as Record<string, unknown>;
    return firstNonEmptyString(parsed.model);
  } catch {
    return "";
  }
};

const isCodexOfficialProvider = (provider?: Provider | null): boolean => {
  if (!provider) return false;
  if (provider.category === "official") return true;

  const configText = provider.settingsConfig?.config;
  if (typeof configText !== "string") return false;
  try {
    const parsed = parseToml(configText) as Record<string, unknown>;
    const providerId = firstNonEmptyString(parsed.model_provider);
    // An omitted provider id (including an otherwise empty config.toml) and
    // the built-in `openai` id both route Codex through OpenAI Official.
    if (!providerId || providerId === "openai") return true;

    const modelProviders = parsed.model_providers;
    if (
      !modelProviders ||
      typeof modelProviders !== "object" ||
      Array.isArray(modelProviders)
    ) {
      return false;
    }
    const providerConfig = (modelProviders as Record<string, unknown>)[
      providerId
    ];
    if (
      !providerConfig ||
      typeof providerConfig !== "object" ||
      Array.isArray(providerConfig)
    ) {
      return false;
    }
    const table = providerConfig as Record<string, unknown>;
    return (
      firstNonEmptyString(table.name).toLowerCase() === "openai" &&
      table.requires_openai_auth === true &&
      !firstNonEmptyString(table.base_url)
    );
  } catch {
    return false;
  }
};

const normalizeBaseUrl = (value: unknown): string =>
  typeof value === "string"
    ? value.trim().replace(/\/+$/, "").toLowerCase()
    : "";

const extractCodexBaseUrl = (provider: Provider): string => {
  const config = provider.settingsConfig ?? {};
  const configText = config.config;
  if (typeof configText === "string") {
    try {
      const parsed = parseToml(configText) as Record<string, unknown>;
      const providerId = firstNonEmptyString(parsed.model_provider);
      const modelProviders = parsed.model_providers;
      if (
        providerId &&
        modelProviders &&
        typeof modelProviders === "object" &&
        !Array.isArray(modelProviders)
      ) {
        const providerConfig = (modelProviders as Record<string, unknown>)[
          providerId
        ];
        if (
          providerConfig &&
          typeof providerConfig === "object" &&
          !Array.isArray(providerConfig)
        ) {
          const baseUrl = firstNonEmptyString(
            (providerConfig as Record<string, unknown>).base_url,
          );
          if (baseUrl) return baseUrl;
        }
      }
    } catch {
      // Fall through to legacy auth values when config.toml is incomplete.
    }
  }

  return firstNonEmptyString(
    config.auth?.OPENAI_API_BASE,
    config.auth?.OPENAI_BASE_URL,
  );
};

export const resolveRemoteAgentProviderId = ({
  agentStatus,
  providers,
}: {
  agentStatus?: RemoteAgentStatus | null;
  providers: Record<string, Provider>;
}): string => {
  if (!agentStatus?.running) return "";

  const status = agentStatus.status;
  const explicitProviderId = firstNonEmptyString(
    status?.proxy?.provider_id,
    status?.codex?.current_provider_id,
    status?.agent?.provider_id,
  );
  if (explicitProviderId && providers[explicitProviderId]) {
    return explicitProviderId;
  }

  const upstreamUrl = normalizeBaseUrl(status?.proxy?.upstream_url);
  if (!upstreamUrl) return "";
  const upstreamModel = firstNonEmptyString(
    status?.proxy?.upstream_model,
    status?.codex?.current_model,
  );
  const candidates = Object.values(providers).filter((provider) => {
    if (provider.category === "official") return false;
    if (normalizeBaseUrl(extractCodexBaseUrl(provider)) !== upstreamUrl) {
      return false;
    }
    const providerModel = extractCodexModel(provider.settingsConfig?.config);
    return !upstreamModel || !providerModel || providerModel === upstreamModel;
  });

  return candidates.length === 1 ? candidates[0].id : "";
};

export const resolveRemoteModelLabel = ({
  appId,
  agentStatus,
  provider,
  matchedProvider,
}: {
  appId: AppId;
  agentStatus?: RemoteAgentStatus | null;
  provider?: Provider | null;
  matchedProvider?: Provider | null;
}): string => {
  const status = agentStatus?.status;
  const agentModel = firstNonEmptyString(
    status?.proxy?.upstream_model,
    status?.codex?.current_model,
  );
  if (agentStatus?.running && agentModel) return agentModel;

  const config = provider?.settingsConfig ?? {};
  if (appId === "codex") {
    return firstNonEmptyString(
      extractCodexModel(config.config),
      extractCodexModel(matchedProvider?.settingsConfig?.config),
      (isCodexOfficialProvider(provider) ||
        isCodexOfficialProvider(matchedProvider)) &&
        "GPT (default)",
    );
  }
  if (appId === "claude") {
    return firstNonEmptyString(
      config.env?.ANTHROPIC_MODEL,
      config.env?.ANTHROPIC_DEFAULT_SONNET_MODEL,
      config.env?.ANTHROPIC_DEFAULT_OPUS_MODEL,
      config.env?.ANTHROPIC_DEFAULT_HAIKU_MODEL,
    );
  }
  if (appId === "gemini") {
    return firstNonEmptyString(
      config.env?.GEMINI_MODEL,
      config.env?.GOOGLE_GEMINI_MODEL,
    );
  }
  return "";
};

export const resolveRemoteProviderLabel = ({
  appId,
  agentStatus,
  matchedProvider,
  remoteProvider,
}: {
  appId?: AppId;
  agentStatus?: RemoteAgentStatus | null;
  matchedProvider?: Provider;
  remoteProvider?: Provider | null;
}): string => {
  if (agentStatus?.running) {
    return firstNonEmptyString(
      matchedProvider?.name,
      agentStatus.status?.proxy?.display_name,
      agentStatus.status?.codex?.current_provider,
      remoteProvider?.name,
      "nexus-agent",
    );
  }
  return firstNonEmptyString(
    matchedProvider?.name,
    appId === "codex" && isCodexOfficialProvider(remoteProvider)
      ? "OpenAI Official"
      : "",
    remoteProvider?.name,
  );
};
