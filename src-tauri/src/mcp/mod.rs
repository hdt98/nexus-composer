//! MCP (Model Context Protocol) server-management module.
//!
//! Validates, synchronizes, imports, and exports MCP server configuration.
//!
//! ## Module structure
//!
//! - `validation` - server configuration validation
//! - `claude` - Claude MCP synchronization and import
//! - `codex` - Codex MCP synchronization and import, including TOML conversion
//! - `gemini` - Gemini MCP synchronization and import
//! - `opencode` - OpenCode MCP synchronization and import, including local/remote conversion
//! - `hermes` - Hermes MCP synchronization and import

mod claude;
mod codex;
mod gemini;
mod hermes;
mod opencode;
mod validation;

// Re-export the public API.
pub use claude::{
    import_from_claude, remove_server_from_claude, sync_enabled_to_claude,
    sync_single_server_to_claude,
};
pub use codex::{
    import_from_codex, remove_server_from_codex, sync_enabled_to_codex, sync_single_server_to_codex,
};
pub use gemini::{
    import_from_gemini, remove_server_from_gemini, sync_enabled_to_gemini,
    sync_single_server_to_gemini,
};
pub use hermes::{import_from_hermes, remove_server_from_hermes, sync_single_server_to_hermes};
pub use opencode::{
    import_from_opencode, remove_server_from_opencode, sync_single_server_to_opencode,
};
