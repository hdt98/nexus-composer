//! Data Access Object layer
//!
//! Database access operations for each domain

pub mod failover;
pub mod mcp;
mod nexus_migration;
pub(crate) use nexus_migration::{
    managed_nexus_endpoint_candidates, reconcile_managed_nexus_update,
    scrub_leaked_nexus_credentials, NexusMigrationOutcome,
};
#[cfg(test)]
pub(crate) use nexus_migration::{ENDPOINT as NEXUS_ENDPOINT, VERSION as NEXUS_VERSION};
pub mod prompts;
pub mod providers;
pub mod providers_seed;
pub mod proxy;
pub mod settings;
pub mod skills;
pub mod stream_check;
pub mod universal_providers;
pub mod usage_rollup;

// 所有 DAO 方法都通过 Database impl 提供，无需单独导出
// 导出 FailoverQueueItem 供外部使用
pub use failover::FailoverQueueItem;
