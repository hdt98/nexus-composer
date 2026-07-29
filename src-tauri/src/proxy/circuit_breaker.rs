//! Circuit breaker.
//!
//! Prevents requests from being sent to unhealthy providers.

use super::log_codes::cb as log_cb;
use super::types::AppProxyConfig;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

/// Circuit-breaker state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CircuitState {
    /// Closed and operating normally.
    Closed,
    /// Open with requests rejected.
    Open,
    /// Half-open while probing recovery with limited requests.
    HalfOpen,
}

impl std::fmt::Display for CircuitState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CircuitState::Closed => write!(f, "closed"),
            CircuitState::Open => write!(f, "open"),
            CircuitState::HalfOpen => write!(f, "half_open"),
        }
    }
}

/// Circuit-breaker configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CircuitBreakerConfig {
    /// Consecutive failures required to open the circuit.
    pub failure_threshold: u32,
    /// Half-open successes required to close the circuit.
    pub success_threshold: u32,
    /// Seconds to wait after opening before attempting half-open recovery.
    pub timeout_seconds: u64,
    /// Error-rate threshold for opening the circuit (0.0-1.0).
    pub error_rate_threshold: f64,
    /// Minimum request count before calculating the error rate.
    pub min_requests: u32,
}

impl From<&AppProxyConfig> for CircuitBreakerConfig {
    fn from(config: &AppProxyConfig) -> Self {
        Self {
            failure_threshold: config.circuit_failure_threshold,
            success_threshold: config.circuit_success_threshold,
            timeout_seconds: config.circuit_timeout_seconds as u64,
            error_rate_threshold: config.circuit_error_rate_threshold,
            min_requests: config.circuit_min_requests,
        }
    }
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 4,
            success_threshold: 2,
            timeout_seconds: 60,
            error_rate_threshold: 0.6,
            min_requests: 10,
        }
    }
}

/// Circuit-breaker instance.
pub struct CircuitBreaker {
    /// Current state.
    state: Arc<RwLock<CircuitState>>,
    /// Consecutive failure count.
    consecutive_failures: Arc<AtomicU32>,
    /// Consecutive success count while half-open.
    consecutive_successes: Arc<AtomicU32>,
    /// Total request count.
    total_requests: Arc<AtomicU32>,
    /// Failed request count.
    failed_requests: Arc<AtomicU32>,
    /// Most recent opening time.
    last_opened_at: Arc<RwLock<Option<Instant>>>,
    /// Hot-reloadable configuration.
    config: Arc<RwLock<CircuitBreakerConfig>>,
    /// Half-open requests currently permitted, used for rate limiting.
    half_open_requests: Arc<AtomicU32>,
}

/// Circuit-breaker admission result.
///
/// `used_half_open_permit` indicates whether this admission consumed a HalfOpen probe permit.
/// Pass it to `record_success` or `record_failure` when the request finishes to release it.
#[derive(Debug, Clone, Copy)]
pub struct AllowResult {
    pub allowed: bool,
    pub used_half_open_permit: bool,
}

impl CircuitBreaker {
    /// Creates a circuit breaker.
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            state: Arc::new(RwLock::new(CircuitState::Closed)),
            consecutive_failures: Arc::new(AtomicU32::new(0)),
            consecutive_successes: Arc::new(AtomicU32::new(0)),
            total_requests: Arc::new(AtomicU32::new(0)),
            failed_requests: Arc::new(AtomicU32::new(0)),
            last_opened_at: Arc::new(RwLock::new(None)),
            config: Arc::new(RwLock::new(config)),
            half_open_requests: Arc::new(AtomicU32::new(0)),
        }
    }

    /// Hot-updates configuration without resetting state.
    pub async fn update_config(&self, new_config: CircuitBreakerConfig) {
        *self.config.write().await = new_config;
    }

    /// Returns whether the provider can be included in the candidate route.
    ///
    /// This routing-stage availability check does not consume a HalfOpen probe permit:
    /// - Closed / HalfOpen: available
    /// - Open: transition to HalfOpen and return true after timeout; otherwise false
    ///
    /// Before sending a request, call `allow_request()` to obtain a HalfOpen probe permit,
    /// then release it through `record_success()` or `record_failure()`.
    pub async fn is_available(&self) -> bool {
        let state = *self.state.read().await;
        let config = self.config.read().await;

        match state {
            CircuitState::Closed | CircuitState::HalfOpen => true,
            CircuitState::Open => {
                if let Some(opened_at) = *self.last_opened_at.read().await {
                    if opened_at.elapsed().as_secs() >= config.timeout_seconds {
                        drop(config); // Release the read lock before changing state.
                        log::info!(
                            "[{}] Circuit breaker Open -> HalfOpen after recovery timeout",
                            log_cb::OPEN_TO_HALF_OPEN
                        );
                        self.transition_to_half_open().await;
                        return true;
                    }
                }
                false
            }
        }
    }

    /// Checks whether a request may proceed.
    pub async fn allow_request(&self) -> AllowResult {
        let state = *self.state.read().await;

        match state {
            CircuitState::Closed => AllowResult {
                allowed: true,
                used_half_open_permit: false,
            },
            CircuitState::Open => {
                let config = self.config.read().await;
                // Check whether half-open recovery should be attempted.
                if let Some(opened_at) = *self.last_opened_at.read().await {
                    if opened_at.elapsed().as_secs() >= config.timeout_seconds {
                        drop(config); // Release the read lock before changing state.
                        log::info!(
                            "[{}] Circuit breaker Open -> HalfOpen after recovery timeout",
                            log_cb::OPEN_TO_HALF_OPEN
                        );
                        self.transition_to_half_open().await;

                        // After transition, use the current state to determine permit requirements.
                        let current_state = *self.state.read().await;
                        return match current_state {
                            CircuitState::Closed => AllowResult {
                                allowed: true,
                                used_half_open_permit: false,
                            },
                            CircuitState::HalfOpen => self.allow_half_open_probe(),
                            CircuitState::Open => AllowResult {
                                allowed: false,
                                used_half_open_permit: false,
                            },
                        };
                    }
                }

                AllowResult {
                    allowed: false,
                    used_half_open_permit: false,
                }
            }
            CircuitState::HalfOpen => self.allow_half_open_probe(),
        }
    }

    /// Records a successful request.
    pub async fn record_success(&self, used_half_open_permit: bool) {
        let state = *self.state.read().await;
        let config = self.config.read().await;

        if used_half_open_permit {
            self.release_half_open_permit();
        }

        // Reset the failure count.
        self.consecutive_failures.store(0, Ordering::SeqCst);
        self.total_requests.fetch_add(1, Ordering::SeqCst);

        if state == CircuitState::HalfOpen {
            let successes = self.consecutive_successes.fetch_add(1, Ordering::SeqCst) + 1;

            if successes >= config.success_threshold {
                drop(config); // Release the read lock before changing state.
                log::info!(
                    "[{}] Circuit breaker HalfOpen -> Closed after recovery",
                    log_cb::HALF_OPEN_TO_CLOSED
                );
                self.transition_to_closed().await;
            }
        }
    }

    /// Records a failed request.
    pub async fn record_failure(&self, used_half_open_permit: bool) {
        let state = *self.state.read().await;
        let config = self.config.read().await;

        if used_half_open_permit {
            self.release_half_open_permit();
        }

        // Update counters.
        let failures = self.consecutive_failures.fetch_add(1, Ordering::SeqCst) + 1;
        self.total_requests.fetch_add(1, Ordering::SeqCst);
        self.failed_requests.fetch_add(1, Ordering::SeqCst);

        // Reset the success count.
        self.consecutive_successes.store(0, Ordering::SeqCst);

        // Check whether the circuit should open.
        match state {
            CircuitState::HalfOpen => {
                // A HalfOpen failure immediately reopens the circuit.
                log::warn!(
                    "[{}] Circuit breaker HalfOpen probe failed -> Open",
                    log_cb::HALF_OPEN_PROBE_FAILED
                );
                drop(config);
                self.transition_to_open().await;
            }
            CircuitState::Closed => {
                // Check consecutive failures.
                if failures >= config.failure_threshold {
                    log::warn!(
                        "[{}] Circuit breaker triggered by {failures} consecutive failures -> Open",
                        log_cb::TRIGGERED_FAILURES
                    );
                    drop(config); // Release the read lock before changing state.
                    self.transition_to_open().await;
                } else {
                    // Check the error rate.
                    let total = self.total_requests.load(Ordering::SeqCst);
                    let failed = self.failed_requests.load(Ordering::SeqCst);

                    if total >= config.min_requests {
                        let error_rate = failed as f64 / total as f64;

                        if error_rate >= config.error_rate_threshold {
                            log::warn!(
                                "[{}] Circuit breaker triggered by {:.1}% error rate -> Open",
                                log_cb::TRIGGERED_ERROR_RATE,
                                error_rate * 100.0
                            );
                            drop(config); // Release the read lock before changing state.
                            self.transition_to_open().await;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Returns the current state.
    #[allow(dead_code)]
    pub async fn get_state(&self) -> CircuitState {
        *self.state.read().await
    }

    /// Returns circuit-breaker statistics.
    #[allow(dead_code)]
    pub async fn get_stats(&self) -> CircuitBreakerStats {
        CircuitBreakerStats {
            state: *self.state.read().await,
            consecutive_failures: self.consecutive_failures.load(Ordering::SeqCst),
            consecutive_successes: self.consecutive_successes.load(Ordering::SeqCst),
            total_requests: self.total_requests.load(Ordering::SeqCst),
            failed_requests: self.failed_requests.load(Ordering::SeqCst),
        }
    }

    /// Manually resets the circuit breaker.
    #[allow(dead_code)]
    pub async fn reset(&self) {
        log::info!(
            "[{}] Circuit breaker manually reset -> Closed",
            log_cb::MANUAL_RESET
        );
        self.transition_to_closed().await;
    }

    fn allow_half_open_probe(&self) -> AllowResult {
        // Limit half-open probes to a fixed number of requests.
        let max_half_open_requests = 1u32;
        let current = self.half_open_requests.fetch_add(1, Ordering::SeqCst);

        if current < max_half_open_requests {
            AllowResult {
                allowed: true,
                used_half_open_permit: true,
            }
        } else {
            // Over the limit: roll back the counter and reject the request.
            self.half_open_requests.fetch_sub(1, Ordering::SeqCst);
            AllowResult {
                allowed: false,
                used_half_open_permit: false,
            }
        }
    }

    /// Releases only a HalfOpen permit without changing health statistics.
    ///
    /// Used when request results, such as rectifier traffic, should not affect provider
    /// health but must release the probe permit to avoid a stuck HalfOpen state.
    pub fn release_half_open_permit(&self) {
        let mut current = self.half_open_requests.load(Ordering::SeqCst);
        loop {
            if current == 0 {
                return;
            }

            match self.half_open_requests.compare_exchange(
                current,
                current - 1,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    /// Transitions to Open.
    async fn transition_to_open(&self) {
        *self.state.write().await = CircuitState::Open;
        *self.last_opened_at.write().await = Some(Instant::now());
        self.consecutive_failures.store(0, Ordering::SeqCst);
        self.consecutive_successes.store(0, Ordering::SeqCst);
    }

    /// Transitions to HalfOpen.
    async fn transition_to_half_open(&self) {
        let mut state = self.state.write().await;
        if *state != CircuitState::Open {
            return;
        }

        *state = CircuitState::HalfOpen;
        self.consecutive_successes.store(0, Ordering::SeqCst);
        // Reset the HalfOpen request-limit counter.
        self.half_open_requests.store(0, Ordering::SeqCst);
    }

    /// Transitions to Closed.
    async fn transition_to_closed(&self) {
        *self.state.write().await = CircuitState::Closed;
        self.consecutive_failures.store(0, Ordering::SeqCst);
        self.consecutive_successes.store(0, Ordering::SeqCst);
        // Reset counters.
        self.total_requests.store(0, Ordering::SeqCst);
        self.failed_requests.store(0, Ordering::SeqCst);
    }
}

/// Circuit-breaker statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CircuitBreakerStats {
    pub state: CircuitState,
    pub consecutive_failures: u32,
    pub consecutive_successes: u32,
    pub total_requests: u32,
    pub failed_requests: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_circuit_breaker_closed_to_open() {
        let config = CircuitBreakerConfig {
            failure_threshold: 3,
            ..Default::default()
        };
        let breaker = CircuitBreaker::new(config);

        // The initial state is Closed.
        assert_eq!(breaker.get_state().await, CircuitState::Closed);
        assert!(breaker.allow_request().await.allowed);

        // Record three failures.
        for _ in 0..3 {
            breaker.record_failure(false).await;
        }

        // The circuit transitions to Open.
        assert_eq!(breaker.get_state().await, CircuitState::Open);
        assert!(!breaker.allow_request().await.allowed);
    }

    #[tokio::test]
    async fn test_circuit_breaker_half_open_to_closed() {
        let config = CircuitBreakerConfig {
            failure_threshold: 2,
            success_threshold: 2,
            ..Default::default()
        };
        let breaker = CircuitBreaker::new(config);

        // Open the circuit.
        breaker.record_failure(false).await;
        breaker.record_failure(false).await;
        assert_eq!(breaker.get_state().await, CircuitState::Open);

        // Manually transition to HalfOpen.
        breaker.transition_to_half_open().await;
        assert_eq!(breaker.get_state().await, CircuitState::HalfOpen);

        // Record two successes.
        breaker.record_success(false).await;
        breaker.record_success(false).await;

        // The circuit transitions to Closed.
        assert_eq!(breaker.get_state().await, CircuitState::Closed);
    }

    #[tokio::test]
    async fn test_half_open_transition_does_not_reset_inflight_permit() {
        let config = CircuitBreakerConfig {
            timeout_seconds: 0,
            ..Default::default()
        };
        let breaker = CircuitBreaker::new(config);

        // Enter Open. With timeout_seconds=0, allow_request immediately enters HalfOpen
        // and consumes a probe permit.
        breaker.transition_to_open().await;
        let first = breaker.allow_request().await;
        assert!(first.allowed);
        assert!(first.used_half_open_permit);
        assert_eq!(breaker.get_state().await, CircuitState::HalfOpen);

        // Simulate a duplicate concurrent HalfOpen transition without resetting in-flight count.
        breaker.transition_to_half_open().await;

        // The second request is rejected because the permit remains in use.
        let second = breaker.allow_request().await;
        assert!(!second.allowed);
        assert!(!second.used_half_open_permit);
    }

    #[tokio::test]
    async fn test_circuit_breaker_reset() {
        let config = CircuitBreakerConfig {
            failure_threshold: 2,
            ..Default::default()
        };
        let breaker = CircuitBreaker::new(config);

        // Open the circuit.
        breaker.record_failure(false).await;
        breaker.record_failure(false).await;
        assert_eq!(breaker.get_state().await, CircuitState::Open);

        // Reset it.
        breaker.reset().await;
        assert_eq!(breaker.get_state().await, CircuitState::Closed);
        assert!(breaker.allow_request().await.allowed);
    }
}
