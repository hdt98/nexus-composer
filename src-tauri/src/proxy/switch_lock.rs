//! Per-app switch lock
//!
//! Ensures that only one provider switch runs for an application at a time,
//! preventing concurrent switches from desynchronizing `is_current` and the live backup.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, OwnedMutexGuard, RwLock};

/// One mutex per application type serializes switches for that application.
///
/// Different applications, such as Claude and Codex, may switch concurrently.
#[derive(Clone, Default)]
pub struct SwitchLockManager {
    locks: Arc<RwLock<HashMap<String, Arc<Mutex<()>>>>>,
}

impl SwitchLockManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Acquires the switch lock for an application.
    ///
    /// Other switches for the same `app_type` wait while the returned
    /// `OwnedMutexGuard` is held.
    pub async fn lock_for_app(&self, app_type: &str) -> OwnedMutexGuard<()> {
        let lock = {
            let locks = self.locks.read().await;
            if let Some(lock) = locks.get(app_type) {
                lock.clone()
            } else {
                drop(locks);
                let mut locks = self.locks.write().await;
                locks
                    .entry(app_type.to_string())
                    .or_insert_with(|| Arc::new(Mutex::new(())))
                    .clone()
            }
        };
        lock.lock_owned().await
    }
}
