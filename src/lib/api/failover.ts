import { invoke } from "@tauri-apps/api/core";
import type {
  ProviderHealth,
  CircuitBreakerConfig,
  CircuitBreakerStats,
  FailoverQueueItem,
} from "@/types/proxy";

export interface Provider {
  id: string;
  name: string;
  settingsConfig: unknown;
  websiteUrl?: string;
  category?: string;
  createdAt?: number;
  sortIndex?: number;
  notes?: string;
  meta?: unknown;
  icon?: string;
  iconColor?: string;
}

export const failoverApi = {
  // ========== Circuit Breaker API ==========

  // Get provider health.
  async getProviderHealth(
    providerId: string,
    appType: string,
  ): Promise<ProviderHealth> {
    return invoke("get_provider_health", { providerId, appType });
  },

  // Reset a circuit breaker.
  async resetCircuitBreaker(
    providerId: string,
    appType: string,
  ): Promise<void> {
    return invoke("reset_circuit_breaker", { providerId, appType });
  },

  // Get circuit-breaker configuration.
  async getCircuitBreakerConfig(): Promise<CircuitBreakerConfig> {
    return invoke("get_circuit_breaker_config");
  },

  // Update circuit-breaker configuration.
  async updateCircuitBreakerConfig(
    config: CircuitBreakerConfig,
  ): Promise<void> {
    return invoke("update_circuit_breaker_config", { config });
  },

  // Get circuit-breaker statistics.
  async getCircuitBreakerStats(
    providerId: string,
    appType: string,
  ): Promise<CircuitBreakerStats | null> {
    return invoke("get_circuit_breaker_stats", { providerId, appType });
  },

  // ========== Failover Queue API ==========

  // Get a failover queue.
  async getFailoverQueue(appType: string): Promise<FailoverQueueItem[]> {
    return invoke("get_failover_queue", { appType });
  },

  // Get providers that are not already in the failover queue.
  async getAvailableProvidersForFailover(appType: string): Promise<Provider[]> {
    return invoke("get_available_providers_for_failover", { appType });
  },

  // Add a provider to the failover queue.
  async addToFailoverQueue(appType: string, providerId: string): Promise<void> {
    return invoke("add_to_failover_queue", { appType, providerId });
  },

  // Remove a provider from the failover queue.
  async removeFromFailoverQueue(
    appType: string,
    providerId: string,
  ): Promise<void> {
    return invoke("remove_from_failover_queue", { appType, providerId });
  },

  // Get automatic failover status for an application.
  async getAutoFailoverEnabled(appType: string): Promise<boolean> {
    return invoke("get_auto_failover_enabled", { appType });
  },

  // Set automatic failover status for an application.
  async setAutoFailoverEnabled(
    appType: string,
    enabled: boolean,
  ): Promise<void> {
    return invoke("set_auto_failover_enabled", { appType, enabled });
  },
};
