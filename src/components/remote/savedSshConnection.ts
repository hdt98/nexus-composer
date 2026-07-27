const LEGACY_SSH_CONNECTION_STORAGE_KEY =
  "nexus-composer.remote.savedSshConnection.v1";
const SSH_CONNECTIONS_STORAGE_KEY =
  "nexus-composer.remote.savedSshConnections.v1";

export interface SavedSshConnection {
  host: string;
  user?: string;
  port: number;
  /** Last successful/failed SSH probe. Passwords are never stored here. */
  lastStatus?: "connected" | "running" | "unreachable";
  lastProvider?: string;
  lastModel?: string;
  lastCheckedAt?: number;
}

const isValidPort = (value: unknown): value is number =>
  typeof value === "number" &&
  Number.isInteger(value) &&
  value > 0 &&
  value <= 65535;

const normalizeConnection = (
  value: Record<string, unknown>,
): SavedSshConnection | null => {
  const host = typeof value.host === "string" ? value.host.trim() : "";
  const user = typeof value.user === "string" ? value.user.trim() : "";
  if (!host || !isValidPort(value.port)) return null;

  const lastStatus =
    value.lastStatus === "connected" ||
    value.lastStatus === "running" ||
    value.lastStatus === "unreachable"
      ? value.lastStatus
      : undefined;
  const lastProvider =
    typeof value.lastProvider === "string" && value.lastProvider.trim()
      ? value.lastProvider.trim()
      : undefined;
  const lastModel =
    typeof value.lastModel === "string" && value.lastModel.trim()
      ? value.lastModel.trim()
      : undefined;
  const lastCheckedAt =
    typeof value.lastCheckedAt === "number" &&
    Number.isFinite(value.lastCheckedAt) &&
    value.lastCheckedAt > 0
      ? value.lastCheckedAt
      : undefined;

  return {
    host,
    user: user || undefined,
    port: value.port,
    lastStatus,
    lastProvider,
    lastModel,
    lastCheckedAt,
  };
};

export const getSavedSshConnectionKey = (
  connection: SavedSshConnection,
): string =>
  `${connection.user?.trim() ?? ""}@${connection.host.trim()}:${connection.port}`;

const persistConnections = (
  connections: SavedSshConnection[],
): SavedSshConnection[] => {
  try {
    globalThis.localStorage?.setItem(
      SSH_CONNECTIONS_STORAGE_KEY,
      JSON.stringify(connections),
    );
  } catch {
    // Keep the in-memory result usable when browser storage is unavailable.
  }
  return connections;
};

export const loadSavedSshConnections = (): SavedSshConnection[] => {
  try {
    const serialized = globalThis.localStorage?.getItem(
      SSH_CONNECTIONS_STORAGE_KEY,
    );
    if (serialized) {
      const values = JSON.parse(serialized) as unknown;
      if (!Array.isArray(values)) return [];

      const uniqueConnections = new Map<string, SavedSshConnection>();
      values.forEach((value) => {
        if (!value || typeof value !== "object") return;
        const connection = normalizeConnection(
          value as Record<string, unknown>,
        );
        if (connection) {
          uniqueConnections.set(
            getSavedSshConnectionKey(connection),
            connection,
          );
        }
      });
      return Array.from(uniqueConnections.values());
    }

    const legacySerialized = globalThis.localStorage?.getItem(
      LEGACY_SSH_CONNECTION_STORAGE_KEY,
    );
    if (!legacySerialized) return [];

    const legacyValue = JSON.parse(legacySerialized) as unknown;
    if (!legacyValue || typeof legacyValue !== "object") return [];
    const legacyConnection = normalizeConnection(
      legacyValue as Record<string, unknown>,
    );
    if (!legacyConnection) return [];

    globalThis.localStorage?.removeItem(LEGACY_SSH_CONNECTION_STORAGE_KEY);
    return persistConnections([legacyConnection]);
  } catch {
    return [];
  }
};

export const saveSshConnection = (
  connection: SavedSshConnection,
  previousKey?: string | null,
): SavedSshConnection[] => {
  const normalized = normalizeConnection(
    connection as unknown as Record<string, unknown>,
  );
  if (!normalized) return loadSavedSshConnections();

  const normalizedKey = getSavedSshConnectionKey(normalized);
  const existingConnections = loadSavedSshConnections();
  // A normal reconnect only supplies host/user/port. Preserve the most recent
  // detection snapshot until the new SSH inspection replaces it.
  const existingSameConnection = existingConnections.find(
    (item) => getSavedSshConnectionKey(item) === normalizedKey,
  );
  const connectionToPersist = {
    ...existingSameConnection,
    ...normalized,
    lastStatus: normalized.lastStatus ?? existingSameConnection?.lastStatus,
    lastProvider:
      normalized.lastProvider ?? existingSameConnection?.lastProvider,
    lastModel: normalized.lastModel ?? existingSameConnection?.lastModel,
    lastCheckedAt:
      normalized.lastCheckedAt ?? existingSameConnection?.lastCheckedAt,
  };
  const connections = existingConnections.filter((item) => {
    const key = getSavedSshConnectionKey(item);
    return key !== normalizedKey && key !== previousKey;
  });
  return persistConnections([connectionToPersist, ...connections]);
};

export const removeSavedSshConnection = (
  connectionKey: string,
): SavedSshConnection[] => {
  const connections = loadSavedSshConnections().filter(
    (item) => getSavedSshConnectionKey(item) !== connectionKey,
  );
  return persistConnections(connections);
};

export const updateSavedSshConnectionSnapshot = (
  connectionKey: string,
  snapshot: Pick<
    SavedSshConnection,
    "lastStatus" | "lastProvider" | "lastModel" | "lastCheckedAt"
  >,
): SavedSshConnection[] => {
  const connections = loadSavedSshConnections();
  let changed = false;
  const updated = connections.map((connection) => {
    if (getSavedSshConnectionKey(connection) !== connectionKey) {
      return connection;
    }
    const next = normalizeConnection({ ...connection, ...snapshot });
    if (!next) return connection;
    if (
      next.lastStatus === connection.lastStatus &&
      next.lastProvider === connection.lastProvider &&
      next.lastModel === connection.lastModel &&
      next.lastCheckedAt === connection.lastCheckedAt
    ) {
      return connection;
    }
    changed = true;
    return next;
  });
  return changed ? persistConnections(updated) : connections;
};
