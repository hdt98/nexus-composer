use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;

use crate::services::skill::SkillStore;

/// MCP server app state tracking which clients use a server
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct McpApps {
    #[serde(default)]
    pub claude: bool,
    #[serde(default)]
    pub codex: bool,
    #[serde(default)]
    pub gemini: bool,
    #[serde(default)]
    pub opencode: bool,
    #[serde(default)]
    pub hermes: bool,
}

impl McpApps {
    /// Check whether the given app is enabled
    pub fn is_enabled_for(&self, app: &AppType) -> bool {
        match app {
            AppType::Claude => self.claude,
            AppType::Codex => self.codex,
            AppType::Gemini => self.gemini,
            AppType::OpenCode => self.opencode,
            AppType::OpenClaw => false, // OpenClaw doesn't support MCP
            AppType::Hermes => self.hermes,
            AppType::ClaudeDesktop => false,
        }
    }

    /// Set the enabled state for the given app
    pub fn set_enabled_for(&mut self, app: &AppType, enabled: bool) {
        match app {
            AppType::Claude => self.claude = enabled,
            AppType::Codex => self.codex = enabled,
            AppType::Gemini => self.gemini = enabled,
            AppType::OpenCode => self.opencode = enabled,
            AppType::OpenClaw => {} // OpenClaw doesn't support MCP, ignore
            AppType::Hermes => self.hermes = enabled,
            AppType::ClaudeDesktop => {} // Claude Desktop 3P provider config doesn't support MCP here
        }
    }

    /// Return all enabled apps
    pub fn enabled_apps(&self) -> Vec<AppType> {
        let mut apps = Vec::new();
        if self.claude {
            apps.push(AppType::Claude);
        }
        if self.codex {
            apps.push(AppType::Codex);
        }
        if self.gemini {
            apps.push(AppType::Gemini);
        }
        if self.opencode {
            apps.push(AppType::OpenCode);
        }
        if self.hermes {
            apps.push(AppType::Hermes);
        }
        apps
    }

    /// Check whether all apps are disabled
    pub fn is_empty(&self) -> bool {
        !self.claude && !self.codex && !self.gemini && !self.opencode && !self.hermes
    }
}

/// Skill app state tracking which clients use a skill
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SkillApps {
    #[serde(default)]
    pub claude: bool,
    #[serde(default)]
    pub codex: bool,
    #[serde(default)]
    pub gemini: bool,
    #[serde(default)]
    pub opencode: bool,
    #[serde(default)]
    pub hermes: bool,
}

impl SkillApps {
    /// Check whether the given app is enabled
    pub fn is_enabled_for(&self, app: &AppType) -> bool {
        match app {
            AppType::Claude => self.claude,
            AppType::Codex => self.codex,
            AppType::Gemini => self.gemini,
            AppType::OpenCode => self.opencode,
            AppType::Hermes => self.hermes,
            AppType::OpenClaw => false, // OpenClaw doesn't support Skills
            AppType::ClaudeDesktop => false,
        }
    }

    /// Set the enabled state for the given app
    pub fn set_enabled_for(&mut self, app: &AppType, enabled: bool) {
        match app {
            AppType::Claude => self.claude = enabled,
            AppType::Codex => self.codex = enabled,
            AppType::Gemini => self.gemini = enabled,
            AppType::OpenCode => self.opencode = enabled,
            AppType::Hermes => self.hermes = enabled,
            AppType::OpenClaw => {} // OpenClaw doesn't support Skills, ignore
            AppType::ClaudeDesktop => {} // Claude Desktop 3P profiles don't use Nexus Composer skill sync
        }
    }

    /// Return all enabled apps
    pub fn enabled_apps(&self) -> Vec<AppType> {
        let mut apps = Vec::new();
        if self.claude {
            apps.push(AppType::Claude);
        }
        if self.codex {
            apps.push(AppType::Codex);
        }
        if self.gemini {
            apps.push(AppType::Gemini);
        }
        if self.opencode {
            apps.push(AppType::OpenCode);
        }
        if self.hermes {
            apps.push(AppType::Hermes);
        }
        apps
    }

    /// Check whether all apps are disabled
    pub fn is_empty(&self) -> bool {
        !self.claude && !self.codex && !self.gemini && !self.opencode && !self.hermes
    }

    /// Enable only the given app and disable the rest
    pub fn only(app: &AppType) -> Self {
        let mut apps = Self::default();
        apps.set_enabled_for(app, true);
        apps
    }

    /// Build enabled state from source labels
    ///
    /// Enable the matching app when a label equals AppType::as_str().
    /// Ignore other labels such as "agents" or "nexus-composer".
    pub fn from_labels(labels: &[String]) -> Self {
        let mut apps = Self::default();
        for label in labels {
            if let Ok(app) = label.parse::<AppType>() {
                apps.set_enabled_for(&app, true);
            }
        }
        apps
    }
}

/// Installed skill using the unified v3.10.0+ shape
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledSkill {
    /// Unique identifier in "owner/repo:directory" or "local:directory" format
    pub id: String,
    /// Display name
    pub name: String,
    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Installed directory name under the SSOT root
    pub directory: String,
    /// Repository owner such as a GitHub user or organization
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_owner: Option<String>,
    /// Repository name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_name: Option<String>,
    /// Repository branch
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_branch: Option<String>,
    /// README URL
    #[serde(skip_serializing_if = "Option::is_none")]
    pub readme_url: Option<String>,
    /// Per-app enabled state
    pub apps: SkillApps,
    /// Install time as a Unix timestamp
    pub installed_at: i64,
    /// Content hash using SHA-256 for update detection
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// Last update time as a Unix timestamp. 0 means never updated
    #[serde(default)]
    pub updated_at: i64,
}

/// Unmanaged skill found in app directories but not managed by Nexus Composer
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnmanagedSkill {
    /// Directory name
    pub directory: String,
    /// Display name parsed from SKILL.md
    pub name: String,
    /// Description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// App directories where this was found, for example ["claude", "codex"]
    pub found_in: Vec<String>,
    /// Resolved path for the first matching entry
    pub path: String,
}

/// MCP server definition using the unified v3.7.0 shape
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServer {
    pub id: String,
    pub name: String,
    pub server: serde_json::Value,
    pub apps: McpApps,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// MCP config for a single client, retained for v3.6.x and earlier compatibility
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct McpConfig {
    /// Server definitions keyed by id. Values are loose JSON objects with UI helper fields such as enabled and source
    #[serde(default)]
    pub servers: HashMap<String, serde_json::Value>,
}

impl McpConfig {
    /// Check whether the config is empty
    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }
}

/// MCP root config where legacy and unified shapes can coexist
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpRoot {
    /// Unified MCP server storage for v3.7.0+
    #[serde(skip_serializing_if = "Option::is_none")]
    pub servers: Option<HashMap<String, McpServer>>,

    /// Legacy per-app storage for v3.6.x and earlier, retained for migration
    #[serde(default, skip_serializing_if = "McpConfig::is_empty")]
    pub claude: McpConfig,
    #[serde(
        rename = "claude-desktop",
        alias = "claudeDesktop",
        alias = "claude_desktop",
        default,
        skip_serializing_if = "McpConfig::is_empty"
    )]
    pub claude_desktop: McpConfig,
    #[serde(default, skip_serializing_if = "McpConfig::is_empty")]
    pub codex: McpConfig,
    #[serde(default, skip_serializing_if = "McpConfig::is_empty")]
    pub gemini: McpConfig,
    /// OpenCode MCP config using opencode.json in practice
    #[serde(default, skip_serializing_if = "McpConfig::is_empty")]
    pub opencode: McpConfig,
    /// OpenClaw MCP config using openclaw.json in practice
    #[serde(default, skip_serializing_if = "McpConfig::is_empty")]
    pub openclaw: McpConfig,
    /// Hermes MCP config using config.yaml in practice
    #[serde(default, skip_serializing_if = "McpConfig::is_empty")]
    pub hermes: McpConfig,
}

impl Default for McpRoot {
    fn default() -> Self {
        Self {
            // v3.7.0+ defaults to the new unified shape with an empty HashMap.
            servers: Some(HashMap::new()),
            // Keep the legacy shape empty and only use it while deserializing old configs for migration.
            claude: McpConfig::default(),
            claude_desktop: McpConfig::default(),
            codex: McpConfig::default(),
            gemini: McpConfig::default(),
            opencode: McpConfig::default(),
            openclaw: McpConfig::default(),
            hermes: McpConfig::default(),
        }
    }
}

/// Prompt config for a single client
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PromptConfig {
    #[serde(default)]
    pub prompts: HashMap<String, crate::prompt::Prompt>,
}

/// Prompt root config stored separately per client
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PromptRoot {
    #[serde(default)]
    pub claude: PromptConfig,
    #[serde(
        rename = "claude-desktop",
        alias = "claudeDesktop",
        alias = "claude_desktop",
        default
    )]
    pub claude_desktop: PromptConfig,
    #[serde(default)]
    pub codex: PromptConfig,
    #[serde(default)]
    pub gemini: PromptConfig,
    #[serde(default)]
    pub opencode: PromptConfig,
    #[serde(default)]
    pub openclaw: PromptConfig,
    #[serde(default)]
    pub hermes: PromptConfig,
}

use crate::config::{copy_file, get_app_config_dir, get_app_config_path, write_json_file};
use crate::error::AppError;
use crate::prompt_files::prompt_file_path;
use crate::provider::ProviderManager;

/// App type
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AppType {
    Claude,
    #[serde(
        rename = "claude-desktop",
        alias = "claude_desktop",
        alias = "claudeDesktop"
    )]
    ClaudeDesktop,
    Codex,
    Gemini,
    OpenCode,
    OpenClaw,
    Hermes,
}

impl AppType {
    pub fn as_str(&self) -> &str {
        match self {
            AppType::Claude => "claude",
            AppType::ClaudeDesktop => "claude-desktop",
            AppType::Codex => "codex",
            AppType::Gemini => "gemini",
            AppType::OpenCode => "opencode",
            AppType::OpenClaw => "openclaw",
            AppType::Hermes => "hermes",
        }
    }

    /// Check if this app uses additive mode
    ///
    /// - Switch mode (false): Only the current provider is written to live config (Claude, Codex, Gemini)
    /// - Additive mode (true): All providers are written to live config (OpenCode, OpenClaw, Hermes)
    pub fn is_additive_mode(&self) -> bool {
        matches!(
            self,
            AppType::OpenCode | AppType::OpenClaw | AppType::Hermes
        )
    }

    /// Return an iterator over all app types
    pub fn all() -> impl Iterator<Item = AppType> {
        [
            AppType::Claude,
            AppType::ClaudeDesktop,
            AppType::Codex,
            AppType::Gemini,
            AppType::OpenCode,
            AppType::OpenClaw,
            AppType::Hermes,
        ]
        .into_iter()
    }
}

impl FromStr for AppType {
    type Err = AppError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normalized = s.trim().to_lowercase();
        match normalized.as_str() {
            "claude" => Ok(AppType::Claude),
            "claude-desktop" | "claude_desktop" | "claudedesktop" => Ok(AppType::ClaudeDesktop),
            "codex" => Ok(AppType::Codex),
            "gemini" => Ok(AppType::Gemini),
            "opencode" => Ok(AppType::OpenCode),
            "openclaw" => Ok(AppType::OpenClaw),
            "hermes" => Ok(AppType::Hermes),
            other => Err(AppError::localized(
                "unsupported_app",
                format!("Unsupported app id: '{other}'. Allowed: claude, claude-desktop, codex, gemini, opencode, openclaw, hermes."),
                format!("Unsupported app id: '{other}'. Allowed: claude, claude-desktop, codex, gemini, opencode, openclaw, hermes."),
            )),
        }
    }
}

/// Shared config snippets stored per app
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CommonConfigSnippets {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codex: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gemini: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opencode: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openclaw: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hermes: Option<String>,
}

impl CommonConfigSnippets {
    /// Get the shared config snippet for the given app
    pub fn get(&self, app: &AppType) -> Option<&String> {
        match app {
            AppType::Claude => self.claude.as_ref(),
            AppType::ClaudeDesktop => None,
            AppType::Codex => self.codex.as_ref(),
            AppType::Gemini => self.gemini.as_ref(),
            AppType::OpenCode => self.opencode.as_ref(),
            AppType::OpenClaw => self.openclaw.as_ref(),
            AppType::Hermes => self.hermes.as_ref(),
        }
    }

    /// Set the shared config snippet for the given app
    pub fn set(&mut self, app: &AppType, snippet: Option<String>) {
        match app {
            AppType::Claude => self.claude = snippet,
            AppType::ClaudeDesktop => {}
            AppType::Codex => self.codex = snippet,
            AppType::Gemini => self.gemini = snippet,
            AppType::OpenCode => self.opencode = snippet,
            AppType::OpenClaw => self.openclaw = snippet,
            AppType::Hermes => self.hermes = snippet,
        }
    }
}

/// Multi-app config shape kept for backward compatibility
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiAppConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    /// App managers keyed by app id
    #[serde(flatten)]
    pub apps: HashMap<String, ProviderManager>,
    /// MCP config stored per client
    #[serde(default)]
    pub mcp: McpRoot,
    /// Prompt config stored per client
    #[serde(default)]
    pub prompts: PromptRoot,
    /// Claude Skills config
    #[serde(default)]
    pub skills: SkillStore,
    /// Shared config snippets stored per app
    #[serde(default)]
    pub common_config_snippets: CommonConfigSnippets,
    /// Legacy Claude shared config snippet field retained for backward-compatible migration
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_common_config_snippet: Option<String>,
}

fn default_version() -> u32 {
    2
}

impl Default for MultiAppConfig {
    fn default() -> Self {
        let mut apps = HashMap::new();
        apps.insert("claude".to_string(), ProviderManager::default());
        apps.insert("claude-desktop".to_string(), ProviderManager::default());
        apps.insert("codex".to_string(), ProviderManager::default());
        apps.insert("gemini".to_string(), ProviderManager::default());
        apps.insert("opencode".to_string(), ProviderManager::default());
        apps.insert("openclaw".to_string(), ProviderManager::default());
        apps.insert("hermes".to_string(), ProviderManager::default());

        Self {
            version: 2,
            apps,
            mcp: McpRoot::default(),
            prompts: PromptRoot::default(),
            skills: SkillStore::default(),
            common_config_snippets: CommonConfigSnippets::default(),
            claude_common_config_snippet: None,
        }
    }
}

impl MultiAppConfig {
    /// Load config from disk. Only the v2 shape is supported
    pub fn load() -> Result<Self, AppError> {
        let config_path = get_app_config_path();

        if !config_path.exists() {
            log::info!("Config file is missing. Creating a new multi-app config and auto-importing prompts");
            // Use the new path so prompt auto-import is supported.
            let config = Self::default_with_auto_import()?;
            // Persist it immediately.
            config.save()?;
            return Ok(config);
        }

        // Try to read the file.
        let content =
            std::fs::read_to_string(&config_path).map_err(|e| AppError::io(&config_path, e))?;

        // Parse as Value first so we can detect the v1 shape strictly.
        // Treat it as v1 when the top level has providers(object) and current(string) and does not include apps.
        let value: serde_json::Value =
            serde_json::from_str(&content).map_err(|e| AppError::json(&config_path, e))?;
        let is_v1 = value.as_object().is_some_and(|map| {
            let has_providers = map.get("providers").map(|v| v.is_object()).unwrap_or(false);
            let has_current = map.get("current").map(|v| v.is_string()).unwrap_or(false);
            // The v1 condition is providers plus current, with no apps key. version or mcp do not make it v2.
            let has_apps = map.contains_key("apps");
            has_providers && has_current && !has_apps
        });
        if is_v1 {
            return Err(AppError::localized(
                "config.unsupported_v1",
                "Detected legacy v1 config. Runtime auto-migration is no longer supported.\n\nSolutions:\n1. Install v3.2.x for one-time auto-migration\n2. Or manually edit ~/.nexus-composer/config.json to adjust the top-level structure:\n   {\"version\": 2, \"claude\": {...}, \"codex\": {...}, \"mcp\": {...}}\n\n",
                "Detected legacy v1 config. Runtime auto-migration is no longer supported.\n\nSolutions:\n1. Install v3.2.x for one-time auto-migration\n2. Or manually edit ~/.nexus-composer/config.json to adjust the top-level structure:\n   {\"version\": 2, \"claude\": {...}, \"codex\": {...}, \"mcp\": {...}}\n\n",
            ));
        }

        let has_skills_in_config = value
            .as_object()
            .is_some_and(|map| map.contains_key("skills"));

        // Parse the v2 shape.
        let mut config: Self =
            serde_json::from_value(value).map_err(|e| AppError::json(&config_path, e))?;
        let mut updated = false;

        if !has_skills_in_config {
            let skills_path = get_app_config_dir().join("skills.json");
            if skills_path.exists() {
                match std::fs::read_to_string(&skills_path) {
                    Ok(content) => match serde_json::from_str::<SkillStore>(&content) {
                        Ok(store) => {
                            config.skills = store;
                            updated = true;
                            log::info!("Imported Claude Skills config from legacy skills.json");
                        }
                        Err(e) => {
                            log::warn!("Failed to parse legacy skills.json: {e}");
                        }
                    },
                    Err(e) => {
                        log::warn!("Failed to read legacy skills.json: {e}");
                    }
                }
            }
        }

        // Ensure the gemini app exists for older config files.
        if !config.apps.contains_key("gemini") {
            config
                .apps
                .insert("gemini".to_string(), ProviderManager::default());
            updated = true;
        }

        // Run the MCP migration from v3.6.x to v3.7.0.
        let migrated = config.migrate_mcp_to_unified()?;
        if migrated {
            log::info!("Migrated MCP config to the unified v3.7.0 shape. Saving config");
            updated = true;
        }

        // For existing config files, if the older version had no Prompt feature
        // and prompts are still empty, try to auto-import existing prompt files.
        let imported_prompts = config.maybe_auto_import_prompts_for_existing_config()?;
        if imported_prompts {
            updated = true;
        }

        // Migrate the shared config snippet field to common_config_snippets.claude.
        if let Some(old_claude_snippet) = config.claude_common_config_snippet.take() {
            log::info!(
                "Migrating shared config: claude_common_config_snippet -> common_config_snippets.claude"
            );
            config.common_config_snippets.claude = Some(old_claude_snippet);
            updated = true;
        }

        if updated {
            log::info!(
                "Config shape changed due to MCP migration or prompt auto-import. Saving config"
            );
            config.save()?;
        }

        Ok(config)
    }

    /// Save config to disk
    pub fn save(&self) -> Result<(), AppError> {
        let config_path = get_app_config_path();
        // Back up the existing file to ~/.nexus-composer/config.json.bak before writing new content.
        if config_path.exists() {
            let backup_path = get_app_config_dir().join("config.json.bak");
            if let Err(e) = copy_file(&config_path, &backup_path) {
                log::warn!("Failed to back up config.json to .bak: {e}");
            }
        }

        write_json_file(&config_path, self)?;
        Ok(())
    }

    /// Get the manager for the given app
    pub fn get_manager(&self, app: &AppType) -> Option<&ProviderManager> {
        self.apps.get(app.as_str())
    }

    /// Get the mutable manager for the given app
    pub fn get_manager_mut(&mut self, app: &AppType) -> Option<&mut ProviderManager> {
        self.apps.get_mut(app.as_str())
    }

    /// Ensure the app entry exists
    pub fn ensure_app(&mut self, app: &AppType) {
        if !self.apps.contains_key(app.as_str()) {
            self.apps
                .insert(app.as_str().to_string(), ProviderManager::default());
        }
    }

    /// Get the MCP config for the given client
    pub fn mcp_for(&self, app: &AppType) -> &McpConfig {
        match app {
            AppType::Claude => &self.mcp.claude,
            AppType::ClaudeDesktop => &self.mcp.claude_desktop,
            AppType::Codex => &self.mcp.codex,
            AppType::Gemini => &self.mcp.gemini,
            AppType::OpenCode => &self.mcp.opencode,
            AppType::OpenClaw => &self.mcp.openclaw,
            AppType::Hermes => &self.mcp.hermes,
        }
    }

    /// Get the mutable MCP config for the given client
    pub fn mcp_for_mut(&mut self, app: &AppType) -> &mut McpConfig {
        match app {
            AppType::Claude => &mut self.mcp.claude,
            AppType::ClaudeDesktop => &mut self.mcp.claude_desktop,
            AppType::Codex => &mut self.mcp.codex,
            AppType::Gemini => &mut self.mcp.gemini,
            AppType::OpenCode => &mut self.mcp.opencode,
            AppType::OpenClaw => &mut self.mcp.openclaw,
            AppType::Hermes => &mut self.mcp.hermes,
        }
    }

    /// Create the default config and auto-import existing prompt files
    fn default_with_auto_import() -> Result<Self, AppError> {
        log::info!("First launch detected. Creating default config and checking for prompt files");

        let mut config = Self::default();

        // Try to auto-import prompts for each app.
        Self::auto_import_prompt_if_exists(&mut config, AppType::Claude)?;
        Self::auto_import_prompt_if_exists(&mut config, AppType::Codex)?;
        Self::auto_import_prompt_if_exists(&mut config, AppType::Gemini)?;
        Self::auto_import_prompt_if_exists(&mut config, AppType::OpenCode)?;
        Self::auto_import_prompt_if_exists(&mut config, AppType::OpenClaw)?;
        Self::auto_import_prompt_if_exists(&mut config, AppType::Hermes)?;

        Ok(config)
    }

    /// Prompt auto-import logic for an existing config file
    ///
    /// Used for upgrades where an older version already created config.json
    /// before the Prompt feature existed.
    /// Rules:
    /// - Import only when prompts are empty for every app, so existing Prompt users are not interrupted.
    /// - Import each app at most once from its own prompt file such as CLAUDE.md, AGENTS.md, or GEMINI.md.
    ///
    /// Returns:
    /// - Ok(true) if at least one app imported prompts
    /// - Ok(false) if nothing needed to be imported
    fn maybe_auto_import_prompts_for_existing_config(&mut self) -> Result<bool, AppError> {
        // If any app already has prompts configured, avoid importing again.
        if !self.prompts.claude.prompts.is_empty()
            || !self.prompts.claude_desktop.prompts.is_empty()
            || !self.prompts.codex.prompts.is_empty()
            || !self.prompts.gemini.prompts.is_empty()
            || !self.prompts.opencode.prompts.is_empty()
            || !self.prompts.openclaw.prompts.is_empty()
            || !self.prompts.hermes.prompts.is_empty()
        {
            return Ok(false);
        }

        log::info!("Existing config detected with an empty Prompt list. Trying to auto-import from current prompt files");

        let mut imported = false;
        for app in [
            AppType::Claude,
            AppType::Codex,
            AppType::Gemini,
            AppType::OpenCode,
            AppType::OpenClaw,
            AppType::Hermes,
        ] {
            // Reuse the existing single-app import path.
            if Self::auto_import_prompt_if_exists(self, app)? {
                imported = true;
            }
        }

        Ok(imported)
    }

    /// Check for and auto-import a single app prompt file
    ///
    /// Returns:
    /// - Ok(true) if a non-empty file was imported
    /// - Ok(false) if nothing was imported because the file was missing, empty, or unreadable
    fn auto_import_prompt_if_exists(config: &mut Self, app: AppType) -> Result<bool, AppError> {
        let file_path = prompt_file_path(&app)?;

        // Check whether the file exists.
        if !file_path.exists() {
            log::debug!("Prompt file is missing. Skipping auto-import: {file_path:?}");
            return Ok(false);
        }

        // Read the file content.
        let content = match std::fs::read_to_string(&file_path) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("Failed to read prompt file: {file_path:?}, error: {e}");
                return Ok(false); // Do not fail the whole flow. Continue with other apps.
            }
        };

        // Check whether the content is empty.
        if content.trim().is_empty() {
            log::debug!("Prompt file is empty. Skipping import: {file_path:?}");
            return Ok(false);
        }

        log::info!("Found prompt file. Auto-importing: {file_path:?}");

        // Build the prompt object.
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_else(|_| {
                log::warn!("Failed to get system time, using 0 as timestamp");
                0
            });

        let id = format!("auto-imported-{timestamp}");
        let prompt = crate::prompt::Prompt {
            id: id.clone(),
            name: format!(
                "Auto-imported Prompt {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M")
            ),
            content,
            description: Some("Automatically imported on first launch".to_string()),
            enabled: true, // Enable imported prompts by default.
            created_at: Some(timestamp),
            updated_at: Some(timestamp),
        };

        // Insert it into the matching app config.
        let prompts = match app {
            AppType::Claude => &mut config.prompts.claude.prompts,
            AppType::ClaudeDesktop => &mut config.prompts.claude_desktop.prompts,
            AppType::Codex => &mut config.prompts.codex.prompts,
            AppType::Gemini => &mut config.prompts.gemini.prompts,
            AppType::OpenCode => &mut config.prompts.opencode.prompts,
            AppType::OpenClaw => &mut config.prompts.openclaw.prompts,
            AppType::Hermes => &mut config.prompts.hermes.prompts,
        };

        prompts.insert(id, prompt);

        log::info!("Auto-import completed: {}", app.as_str());
        Ok(true)
    }

    /// Migrate the per-app MCP shape from v3.6.x to the unified v3.7.0 shape
    ///
    /// Migration strategy:
    /// 1. Check whether migration already happened by looking for mcp.servers.
    /// 2. Collect MCP entries from every app and merge duplicates by ID.
    /// 3. Build unified McpServer entries and mark which clients use them.
    /// 4. Clear the old per-app configs.
    pub fn migrate_mcp_to_unified(&mut self) -> Result<bool, AppError> {
        // Skip migration when the new shape is already present.
        if self.mcp.servers.is_some() {
            log::debug!("MCP config is already in the unified shape. Skipping migration");
            return Ok(false);
        }

        log::info!("Detected legacy MCP config. Starting migration to the unified v3.7.0 shape");

        let mut unified_servers: HashMap<String, McpServer> = HashMap::new();
        let mut conflicts = Vec::new();

        // Collect MCP entries from all supported legacy apps.
        for app in [
            AppType::Claude,
            AppType::Codex,
            AppType::Gemini,
            AppType::OpenCode,
        ] {
            let old_servers = match app {
                AppType::Claude => &self.mcp.claude.servers,
                AppType::ClaudeDesktop => continue, // Claude Desktop 3P profiles don't use MCP here
                AppType::Codex => &self.mcp.codex.servers,
                AppType::Gemini => &self.mcp.gemini.servers,
                AppType::OpenCode => &self.mcp.opencode.servers,
                AppType::OpenClaw => continue, // OpenClaw MCP is still in development, skip
                AppType::Hermes => continue,   // Hermes didn't exist in v3.6.x, skip
            };

            for (id, entry) in old_servers {
                let enabled = entry
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);

                if let Some(existing) = unified_servers.get_mut(id) {
                    // This ID already exists, so merge the apps field.
                    existing.apps.set_enabled_for(&app, enabled);

                    // Detect config conflicts where the same ID has different server payloads.
                    if existing.server != *entry.get("server").unwrap_or(&serde_json::json!({})) {
                        conflicts.push(format!(
                            "MCP '{id}' has different configs in {} and an earlier app. The first config will be kept",
                            app.as_str()
                        ));
                    }
                } else {
                    // First time seeing this MCP, create a new entry.
                    let name = entry
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or(id)
                        .to_string();

                    let server = entry
                        .get("server")
                        .cloned()
                        .unwrap_or(serde_json::json!({}));

                    let description = entry
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    let homepage = entry
                        .get("homepage")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    let docs = entry
                        .get("docs")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());

                    let tags = entry
                        .get("tags")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default();

                    let mut apps = McpApps::default();
                    apps.set_enabled_for(&app, enabled);

                    unified_servers.insert(
                        id.clone(),
                        McpServer {
                            id: id.clone(),
                            name,
                            server,
                            apps,
                            description,
                            homepage,
                            docs,
                            tags,
                        },
                    );
                }
            }
        }

        // Log conflict warnings.
        if !conflicts.is_empty() {
            log::warn!("Detected config conflicts during MCP migration:");
            for conflict in &conflicts {
                log::warn!("  - {conflict}");
            }
        }

        log::info!(
            "MCP migration finished. Migrated {} servers{}",
            unified_servers.len(),
            if !conflicts.is_empty() {
                format!(" with {} conflicts", conflicts.len())
            } else {
                String::new()
            }
        );

        // Replace the legacy shape with the unified one.
        self.mcp.servers = Some(unified_servers);

        // Clear the old per-app configs.
        self.mcp.claude = McpConfig::default();
        self.mcp.codex = McpConfig::default();
        self.mcp.gemini = McpConfig::default();

        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn app_type_parses_claude_desktop_aliases() {
        assert_eq!(
            "claude-desktop".parse::<AppType>().unwrap(),
            AppType::ClaudeDesktop
        );
        assert_eq!(
            "claude_desktop".parse::<AppType>().unwrap(),
            AppType::ClaudeDesktop
        );
        assert_eq!(
            "claudeDesktop".parse::<AppType>().unwrap(),
            AppType::ClaudeDesktop
        );
        assert_eq!(AppType::ClaudeDesktop.as_str(), "claude-desktop");
    }

    struct TempHome {
        #[allow(dead_code)] // Drop manages the temporary directory lifetime.
        dir: TempDir,
        original_home: Option<String>,
        original_userprofile: Option<String>,
        original_test_home: Option<String>,
    }

    impl TempHome {
        fn new() -> Self {
            let dir = TempDir::new().expect("failed to create temp home");
            let original_home = env::var("HOME").ok();
            let original_userprofile = env::var("USERPROFILE").ok();
            let original_test_home = env::var("CC_SWITCH_TEST_HOME").ok();

            env::set_var("HOME", dir.path());
            env::set_var("USERPROFILE", dir.path());
            env::set_var("CC_SWITCH_TEST_HOME", dir.path());

            Self {
                dir,
                original_home,
                original_userprofile,
                original_test_home,
            }
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            match &self.original_home {
                Some(value) => env::set_var("HOME", value),
                None => env::remove_var("HOME"),
            }

            match &self.original_userprofile {
                Some(value) => env::set_var("USERPROFILE", value),
                None => env::remove_var("USERPROFILE"),
            }

            match &self.original_test_home {
                Some(value) => env::set_var("CC_SWITCH_TEST_HOME", value),
                None => env::remove_var("CC_SWITCH_TEST_HOME"),
            }
        }
    }

    fn write_prompt_file(app: AppType, content: &str) {
        let path = crate::prompt_files::prompt_file_path(&app).expect("prompt path");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(path, content).expect("write prompt");
    }

    #[test]
    #[serial]
    fn auto_imports_existing_prompt_when_config_missing() {
        let _home = TempHome::new();
        write_prompt_file(AppType::Claude, "# hello");

        let config = MultiAppConfig::load().expect("load config");

        assert_eq!(config.prompts.claude.prompts.len(), 1);
        let prompt = config
            .prompts
            .claude
            .prompts
            .values()
            .next()
            .expect("prompt exists");
        assert!(prompt.enabled);
        assert_eq!(prompt.content, "# hello");

        let config_path = crate::config::get_app_config_path();
        assert!(
            config_path.exists(),
            "auto import should persist config to disk"
        );
    }

    #[test]
    #[serial]
    fn skips_empty_prompt_files_during_import() {
        let _home = TempHome::new();
        write_prompt_file(AppType::Claude, "   \n  ");

        let config = MultiAppConfig::load().expect("load config");
        assert!(
            config.prompts.claude.prompts.is_empty(),
            "empty files must be ignored"
        );
    }

    #[test]
    #[serial]
    fn auto_import_happens_only_once() {
        let _home = TempHome::new();
        write_prompt_file(AppType::Claude, "first version");

        let first = MultiAppConfig::load().expect("load config");
        assert_eq!(first.prompts.claude.prompts.len(), 1);
        let claude_prompt = first
            .prompts
            .claude
            .prompts
            .values()
            .next()
            .expect("prompt exists")
            .content
            .clone();
        assert_eq!(claude_prompt, "first version");

        // Overwrite the prompt file while keeping config.json.
        write_prompt_file(AppType::Claude, "second version");
        let second = MultiAppConfig::load().expect("load config again");

        assert_eq!(second.prompts.claude.prompts.len(), 1);
        let prompt = second
            .prompts
            .claude
            .prompts
            .values()
            .next()
            .expect("prompt exists");
        assert_eq!(
            prompt.content, "first version",
            "should not re-import when config already exists"
        );
    }

    #[test]
    #[serial]
    fn auto_imports_gemini_prompt_on_first_launch() {
        let _home = TempHome::new();
        write_prompt_file(AppType::Gemini, "# Gemini Prompt\n\nTest content");

        let config = MultiAppConfig::load().expect("load config");

        assert_eq!(config.prompts.gemini.prompts.len(), 1);
        let prompt = config
            .prompts
            .gemini
            .prompts
            .values()
            .next()
            .expect("gemini prompt exists");
        assert!(prompt.enabled, "gemini prompt should be enabled");
        assert_eq!(prompt.content, "# Gemini Prompt\n\nTest content");
        assert_eq!(
            prompt.description,
            Some("Automatically imported on first launch".to_string())
        );
    }

    #[test]
    #[serial]
    fn auto_imports_all_three_apps_prompts() {
        let _home = TempHome::new();
        write_prompt_file(AppType::Claude, "# Claude prompt");
        write_prompt_file(AppType::Codex, "# Codex prompt");
        write_prompt_file(AppType::Gemini, "# Gemini prompt");

        let config = MultiAppConfig::load().expect("load config");

        // Verify that prompts were imported for all three apps.
        assert_eq!(config.prompts.claude.prompts.len(), 1);
        assert_eq!(config.prompts.codex.prompts.len(), 1);
        assert_eq!(config.prompts.gemini.prompts.len(), 1);

        // Verify that all imported prompts are enabled.
        assert!(
            config
                .prompts
                .claude
                .prompts
                .values()
                .next()
                .unwrap()
                .enabled
        );
        assert!(
            config
                .prompts
                .codex
                .prompts
                .values()
                .next()
                .unwrap()
                .enabled
        );
        assert!(
            config
                .prompts
                .gemini
                .prompts
                .values()
                .next()
                .unwrap()
                .enabled
        );
    }
}
