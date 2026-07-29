//! Data Access Object layer
//!
//! Database access operations for each domain

pub mod failover;
pub mod mcp;
pub mod nexus_presets;
pub mod prompts;
pub mod providers;
pub mod providers_seed;
pub mod proxy;
pub mod settings;
pub mod skills;
pub mod stream_check;
pub mod universal_providers;
pub mod usage_rollup;

// All DAO methods are exposed through the Database implementation and do not need
// separate exports. Export FailoverQueueItem for external consumers.
pub use failover::FailoverQueueItem;
