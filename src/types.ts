export type ProviderCategory =
  | "official" // Official provider
  | "cn_official" // Open-source official provider (formerly "domestic official")
  | "cloud_provider" // Cloud provider, such as AWS Bedrock
  | "aggregator" // API aggregator
  | "third_party" // Third-party provider
  | "custom" // Custom provider
  | "omo" // Oh My OpenCode
  | "omo-slim"; // Oh My OpenCode Slim

export interface Provider {
  id: string;
  name: string;
  settingsConfig: Record<string, any>; // App config: settings.json for Claude; { auth, config } for Codex
  websiteUrl?: string;
  // Provider category used for contextual guidance and capability toggles.
  category?: ProviderCategory;
  createdAt?: number; // Creation timestamp in milliseconds
  sortIndex?: number; // Custom drag-and-drop sort index
  // Optional notes
  notes?: string;
  // Whether this is a commercial partner.
  isPartner?: boolean;
  // Provider metadata stored only in ~/.nexus-composer/config.json, not live config.
  meta?: ProviderMeta;
  // Icon configuration
  icon?: string; // Icon name, such as "openai" or "anthropic"
  iconColor?: string; // Hex icon color, such as "#00A67E"
  // Whether the provider is in the failover queue.
  inFailoverQueue?: boolean;
}

export interface AppConfig {
  providers: Record<string, Provider>;
  current: string;
}

// Custom endpoint configuration
export interface CustomEndpoint {
  url: string;
  addedAt: number;
  lastUsed?: number;
}

// Endpoint candidate used by the speed-test dialog
export interface EndpointCandidate {
  id?: string;
  url: string;
  isCustom?: boolean;
}

import type { TemplateType } from "./config/constants";

// Usage-query script configuration
export interface UsageScript {
  enabled: boolean; // Whether usage queries are enabled
  language: "javascript"; // Script language
  code: string; // Script code represented as JSON configuration
  timeout?: number; // Timeout in seconds; defaults to 10
  templateType?: TemplateType; // Template type used by backend validation
  apiKey?: string; // Usage-query API key for the generic template
  baseUrl?: string; // Usage-query base URL for generic and NewAPI templates
  accessToken?: string; // Access token for the NewAPI template
  userId?: string; // User ID for the NewAPI template
  accessKeyId?: string; // Volcengine Ark AccessKey ID used only to sign usage queries
  secretAccessKey?: string; // Volcengine Ark SecretAccessKey
  codingPlanProvider?: string; // Coding Plan provider ID, such as "kimi", "zhipu", or "minimax"
  autoQueryInterval?: number; // Automatic query interval in minutes; 0 disables it
  autoIntervalMinutes?: number; // Alias for the automatic query interval in minutes
  request?: {
    // Request configuration
    url?: string; // Request URL
    method?: string; // HTTP method
    headers?: Record<string, string>; // Request headers
    body?: any; // Request body
  };
}

const DEFAULT_USAGE_SCRIPT: UsageScript = {
  enabled: false,
  language: "javascript",
  code: "",
  timeout: 10,
  autoQueryInterval: 5,
};

export function createUsageScript(
  overrides?: Partial<UsageScript>,
): UsageScript {
  return { ...DEFAULT_USAGE_SCRIPT, ...overrides };
}

// Usage data for one plan
export interface UsageData {
  planName?: string; // Optional plan name
  extra?: string; // Optional supplemental display text
  isValid?: boolean; // Optional plan validity flag
  invalidMessage?: string; // Optional reason shown when isValid is false
  total?: number; // Optional total quota
  used?: number; // Optional used quota
  remaining?: number; // Optional remaining quota
  unit?: string; // Optional unit
}

// Usage-query result supporting multiple plans
export interface UsageResult {
  success: boolean;
  data?: UsageData[]; // Array form supports multiple plans
  error?: string;
}

// Per-provider connectivity-test configuration that overrides global settings
export interface ProviderTestConfig {
  // Whether to use this override; false uses the global configuration.
  enabled: boolean;
  // Timeout in seconds
  timeoutSecs?: number;
  // Degraded-response threshold in milliseconds
  degradedThresholdMs?: number;
  // Maximum retry count
  maxRetries?: number;
}

export type AuthBindingSource = "provider_config" | "managed_account";

export interface AuthBinding {
  source: AuthBindingSource;
  authProvider?: string;
  accountId?: string;
}

export interface ClaudeDesktopModelRoute {
  model: string;
  labelOverride?: string;
  supports1m?: boolean;
}

export type CodexChatThinkingParam =
  | "none"
  | "thinking"
  | "enable_thinking"
  | "reasoning_split";

export type CodexChatEffortParam =
  | "none"
  | "reasoning_effort"
  // OpenRouter's native normalized reasoning:{effort} object, distinct from the top-level OpenAI reasoning_effort alias
  | "reasoning.effort";

export type CodexChatEffortValueMode =
  | "passthrough"
  | "low_high"
  | "deepseek"
  // OpenRouter effort values: xhigh|high|medium|low|minimal; max is clamped to xhigh
  | "openrouter";

export type CodexChatReasoningOutputFormat =
  | "auto"
  | "reasoning_content"
  | "reasoning"
  | "reasoning_details"
  | "think_tags";

export interface CodexChatReasoning {
  supportsThinking?: boolean;
  supportsEffort?: boolean;
  thinkingParam?: CodexChatThinkingParam;
  effortParam?: CodexChatEffortParam;
  effortValueMode?: CodexChatEffortValueMode;
  // Declares where upstream reasoning is returned. Extraction currently checks
  // known fields directly and does not read this value; think_tags is not wired yet.
  outputFormat?: CodexChatReasoningOutputFormat;
}

export interface LocalProxyRequestOverrides {
  headers?: Record<string, string>;
  body?: Record<string, unknown>;
}

// Provider metadata; field names match the backend and retain snake_case.
export interface ProviderMeta {
  // Custom endpoints keyed by URL.
  custom_endpoints?: Record<string, CustomEndpoint>;
  // Whether switching or syncing to live config applies the common snippet.
  commonConfigEnabled?: boolean;
  // Claude Desktop third-party configuration write mode
  claudeDesktopMode?: "direct" | "proxy";
  // Claude Desktop local routing: Claude-safe route -> upstream model
  claudeDesktopModelRoutes?: Record<string, ClaudeDesktopModelRoute>;
  // Usage-query script configuration
  usage_script?: UsageScript;
  // Automatically select the best endpoint after speed testing.
  endpointAutoSelect?: boolean;
  // Whether this is an official partner.
  isPartner?: boolean;
  // Partner promotion key used by the backend to identify PackyCode and others.
  partnerPromotionKey?: string;
  // Per-provider model-test configuration
  testConfig?: ProviderTestConfig;
  // Provider cost multiplier
  costMultiplier?: string;
  // Source of the provider pricing model
  pricingModelSource?: string;
  // API format used by Claude and Codex providers
  // - "anthropic": native Anthropic Messages API, passed through directly
  // - "openai_chat": OpenAI Chat Completions, requiring format conversion
  // - "openai_responses": OpenAI Responses, requiring format conversion
  // - "gemini_native": Gemini Native generateContent, requiring format conversion
  apiFormat?:
    | "anthropic"
    | "openai_chat"
    | "openai_responses"
    | "gemini_native";
  // Shared authentication binding
  authBinding?: AuthBinding;
  // Claude authentication field name
  apiKeyField?: ClaudeApiKeyField;
  // Whether base_url is a complete API endpoint used without appending a path.
  isFullUrl?: boolean;
  // Prompt cache key for OpenAI Responses-compatible endpoints (improves cache hit rate)
  promptCacheKey?: string;
  // Codex OAuth FAST mode: injects service_tier="priority" on ChatGPT Codex requests
  codexFastMode?: boolean;
  // Codex Responses -> Chat Completions reasoning capability metadata
  codexChatReasoning?: CodexChatReasoning;
  // Custom User-Agent for local proxy routing. Only applied by the local proxy.
  customUserAgent?: string;
  // Local proxy request overrides. Only applied by the local proxy after route transforms.
  localProxyRequestOverrides?: LocalProxyRequestOverrides;
  // Version of the Nexus-managed defaults applied to this provider.
  managedNexusPresetVersion?: number;
  // Provider type used to identify special providers such as Copilot.
  providerType?: string;
  // Legacy GitHub Copilot account ID retained for compatibility reads.
  githubAccountId?: string;
}

// Skill synchronization method
export type SkillSyncMethod = "auto" | "symlink" | "copy";

// Skill storage location
export type SkillStorageLocation = "cc_switch" | "unified";

// Claude API format
// - "anthropic": native Anthropic Messages API, passed through directly
// - "openai_chat": OpenAI Chat Completions, requiring format conversion
// - "openai_responses": OpenAI Responses, requiring format conversion
// - "gemini_native": Gemini Native generateContent, requiring format conversion
export type ClaudeApiFormat =
  | "anthropic"
  | "openai_chat"
  | "openai_responses"
  | "gemini_native";

// Codex API format
// - "openai_responses": OpenAI Responses API, passed through directly
// - "openai_chat": OpenAI Chat Completions, requiring local route conversion
export type CodexApiFormat = "openai_responses" | "openai_chat";

export interface CodexCatalogModel {
  model: string;
  displayName?: string;
  contextWindow?: string | number;
  // Native Responses (direct) profile overrides for the generated
  // model-catalogs.json. Ignored by the chat/proxy profile.
  // e.g. MiniMax: supportsParallelToolCalls=true, inputModalities=["text","image"].
  supportsParallelToolCalls?: boolean;
  inputModalities?: string[];
  // Vendor's OFFICIAL base_instructions (model identity / system preamble).
  // Codex requires this field in every catalog entry; when omitted the backend
  // falls back to a neutral default. e.g. MiMo "developed by Xiaomi".
  baseInstructions?: string;
}

// Claude authentication field type
export type ClaudeApiKeyField = "ANTHROPIC_AUTH_TOKEN" | "ANTHROPIC_API_KEY";

// Applications visible on the main page
export interface VisibleApps {
  claude: boolean;
  "claude-desktop": boolean;
  codex: boolean;
  gemini: boolean;
  opencode: boolean;
  openclaw: boolean;
  hermes: boolean;
}

// WebDAV synchronization status
export interface WebDavSyncStatus {
  lastSyncAt?: number | null;
  lastError?: string | null;
  lastErrorSource?: string | null;
  lastRemoteEtag?: string | null;
  lastLocalManifestHash?: string | null;
  lastRemoteManifestHash?: string | null;
}

// WebDAV synchronization settings
export interface WebDavSyncSettings {
  enabled?: boolean;
  autoSync?: boolean;
  baseUrl?: string;
  username?: string;
  password?: string;
  remoteRoot?: string;
  profile?: string;
  status?: WebDavSyncStatus;
}

// S3 synchronization settings
export interface S3SyncSettings {
  enabled?: boolean;
  autoSync?: boolean;
  region?: string;
  bucket?: string;
  accessKeyId?: string;
  secretAccessKey?: string;
  endpoint?: string;
  remoteRoot?: string;
  profile?: string;
  status?: WebDavSyncStatus;
}

export type RemoteSnapshotLayout = "current" | "legacy";

// Remote snapshot metadata shown before download
export interface RemoteSnapshotInfo {
  deviceName: string;
  createdAt: string;
  snapshotId: string;
  version: number;
  protocolVersion: number;
  dbCompatVersion?: number | null;
  compatible: boolean;
  artifacts: string[];
  layout: RemoteSnapshotLayout;
  remotePath: string;
}

// Application settings used by the settings dialog and Tauri API.
// Stored locally in ~/.nexus-composer/settings.json and not synchronized with the database.
export interface Settings {
  // ===== Device-level UI settings =====
  // Whether to show an icon in the system tray or macOS menu bar.
  showInTray: boolean;
  // Whether the close button minimizes to the tray instead of quitting.
  minimizeToTrayOnClose: boolean;
  // Whether to use application-provided minimize, maximize, and close controls.
  useAppWindowControls?: boolean;
  // Enable Claude plugin integration by writing primaryApiKey to ~/.claude/config.json.
  enableClaudePluginIntegration?: boolean;
  // Skip Claude Code's first-install confirmation by setting hasCompletedOnboarding in ~/.claude.json.
  skipClaudeOnboarding?: boolean;
  // Whether to launch at system startup.
  launchOnStartup?: boolean;
  // Start silently without showing the main window.
  silentStartup?: boolean;
  // Whether the main-page local proxy is enabled; defaults to false.
  enableLocalProxy?: boolean;
  // User has confirmed the local proxy first-run notice
  proxyConfirmed?: boolean;
  // User has confirmed the usage query first-run notice
  usageConfirmed?: boolean;
  // User has confirmed the stream check first-run notice
  streamCheckConfirmed?: boolean;
  // Whether to show the failover toggle independently on the main page
  enableFailoverToggle?: boolean;
  // Preserve Codex ChatGPT login in auth.json when switching third-party providers
  preserveCodexOfficialAuthOnSwitch?: boolean;
  // Run official Codex under the shared "custom" provider id so future
  // sessions share one resume-history bucket with third-party providers
  unifyCodexSessionHistory?: boolean;
  // User opted in (enable dialog checkbox) to migrate existing official sessions
  unifyCodexMigrateExisting?: boolean;
  // User has confirmed the failover toggle first-run notice
  failoverConfirmed?: boolean;
  // User has confirmed the first-run welcome notice
  firstRunNoticeConfirmed?: boolean;
  // User has confirmed the auto-sync traffic warning
  autoSyncConfirmed?: boolean;
  // User has confirmed the common config first-run notice
  commonConfigConfirmed?: boolean;
  // Preferred language; optional and defaults to English.
  language?: "en" | "vi";

  // Applications shown on the main page; all are visible by default.
  visibleApps?: VisibleApps;

  // ===== Device-level directory overrides =====
  // Optional Claude Code configuration directory override
  claudeConfigDir?: string;
  // Optional Codex configuration directory override
  codexConfigDir?: string;
  // Optional Gemini configuration directory override
  geminiConfigDir?: string;
  // Optional OpenCode configuration directory override
  opencodeConfigDir?: string;
  // Optional OpenClaw configuration directory override
  openclawConfigDir?: string;
  // Optional Hermes configuration directory override
  hermesConfigDir?: string;

  // ===== Current provider IDs (device-level) =====
  // Current Claude provider ID; takes precedence over database is_current.
  currentProviderClaude?: string;
  // Current Claude Desktop provider ID; takes precedence over database is_current.
  currentProviderClaudeDesktop?: string;
  // Current Codex provider ID; takes precedence over database is_current.
  currentProviderCodex?: string;
  // Current Gemini provider ID; takes precedence over database is_current.
  currentProviderGemini?: string;

  // ===== Skill synchronization settings =====
  // Method: auto (default, preferring symlink), symlink, or copy.
  skillSyncMethod?: SkillSyncMethod;
  // Storage: cc_switch (default) or unified (~/.agents/skills/).
  skillStorageLocation?: SkillStorageLocation;

  // ===== WebDAV v2 synchronization settings =====
  webdavSync?: WebDavSyncSettings;

  // ===== S3 synchronization settings =====
  s3Sync?: S3SyncSettings;

  // ===== Backup policy settings =====
  // Auto-backup interval in hours (0=disabled, default 24)
  backupIntervalHours?: number;
  // Maximum backup files to retain (default 10)
  backupRetainCount?: number;

  // ===== Terminal settings =====
  // Optional preferred terminal application; defaults to the system terminal.
  // macOS: "terminal" | "iterm2" | "warp" | "alacritty" | "kitty" | "ghostty" | "wezterm" | "kaku"
  // Windows: "cmd" | "powershell" | "wt"
  // Linux: "gnome-terminal" | "konsole" | "xfce4-terminal" | "alacritty" | "kitty" | "ghostty"
  preferredTerminal?: string;

  // ===== Local automatic migration state =====
  localMigrations?: {
    codexThirdPartyHistoryProviderBucketV1?: {
      completedAt: string;
      targetProviderId: string;
      sourceProviderIds?: string[];
      migratedJsonlFiles?: number;
      migratedStateRows?: number;
    };
  };
}

export interface SessionMeta {
  providerId: string;
  sessionId: string;
  title?: string;
  summary?: string;
  projectDir?: string | null;
  createdAt?: number;
  lastActiveAt?: number;
  sourcePath?: string;
  resumeCommand?: string;
}

export interface SessionMessage {
  role: string;
  content: string;
  ts?: number;
}

// MCP server connection parameters; permissive to allow extension fields.
export interface McpServerSpec {
  // Optional because common community .mcp.json stdio entries may omit type.
  type?: "stdio" | "http" | "sse";
  // stdio fields
  command?: string;
  args?: string[];
  env?: Record<string, string>;
  cwd?: string;
  // HTTP and SSE fields
  url?: string;
  headers?: Record<string, string>;
  // Shared fields
  [key: string]: any;
}

// v3.7.0: MCP server enablement by application
export interface McpApps {
  claude: boolean;
  "claude-desktop"?: boolean;
  codex: boolean;
  gemini: boolean;
  opencode: boolean;
  openclaw: boolean;
  hermes: boolean;
}

// MCP server entry using the unified v3.7.0 structure
export interface McpServer {
  id: string;
  name: string;
  server: McpServerSpec;
  apps: McpApps; // v3.7.0: identifies which clients use this server
  description?: string;
  tags?: string[];
  homepage?: string;
  docs?: string;
  // Legacy fields retained for v3.6.x and earlier compatibility
  enabled?: boolean; // Deprecated; v3.7.0 uses the apps field
  source?: string;
  [key: string]: any;
}

// MCP server map (id -> McpServer)
export type McpServersMap = Record<string, McpServer>;

// MCP configuration status
export interface McpStatus {
  userConfigPath: string;
  userConfigExists: boolean;
  serverCount: number;
}

// MCP list response from config.json
export interface McpConfigResponse {
  configPath: string;
  servers: Record<string, McpServer>;
}

// ============================================================================
// Universal providers: configuration shared across applications
// ============================================================================

// Universal provider enablement by application
export interface UniversalProviderApps {
  claude: boolean;
  codex: boolean;
  gemini: boolean;
}

// Claude model configuration
export interface ClaudeModelConfig {
  model?: string;
  haikuModel?: string;
  sonnetModel?: string;
  opusModel?: string;
}

// Codex model configuration
export interface CodexModelConfig {
  model?: string;
  reasoningEffort?: string;
}

// Gemini model configuration
export interface GeminiModelConfig {
  model?: string;
}

// Per-application model configuration
export interface UniversalProviderModels {
  claude?: ClaudeModelConfig;
  codex?: CodexModelConfig;
  gemini?: GeminiModelConfig;
}

// Universal provider shared across applications
export interface UniversalProvider {
  id: string;
  name: string;
  providerType: string; // For example, "newapi" or "custom"
  apps: UniversalProviderApps;
  baseUrl: string;
  apiKey: string;
  models: UniversalProviderModels;
  websiteUrl?: string;
  notes?: string;
  icon?: string;
  iconColor?: string;
  meta?: ProviderMeta;
  createdAt?: number;
  sortIndex?: number;
}

// Universal provider map (id -> UniversalProvider)
export type UniversalProvidersMap = Record<string, UniversalProvider>;

// ============================================================================
// OpenCode-specific configuration (v3.9.2+)
// ============================================================================

// OpenCode model configuration
export interface OpenCodeModel {
  name: string;
  limit?: {
    context?: number;
    output?: number;
  };
  options?: Record<string, unknown>; // Additional model options, such as provider routing
  // Supports arbitrary additional fields such as cost, modalities, thinking, and variants.
  [key: string]: unknown;
}

// OpenCode provider options
export interface OpenCodeProviderOptions {
  baseURL?: string;
  apiKey?: string;
  headers?: Record<string, string>;
  // Supports extra options such as timeout and setCacheKey.
  [key: string]: unknown;
}

// OpenCode provider configuration using the settings_config structure
export interface OpenCodeProviderConfig {
  npm: string; // AI SDK package, such as "@ai-sdk/openai-compatible"
  name?: string; // Provider display name
  options: OpenCodeProviderOptions;
  models: Record<string, OpenCodeModel>;
}

// OpenCode MCP server configuration, which differs from the unified format
export interface OpenCodeMcpServerSpec {
  type: "local" | "remote";
  // Local fields
  command?: string[]; // Unlike the unified format, command and arguments share one array
  environment?: Record<string, string>; // Uses environment instead of env
  // Remote fields
  url?: string;
  headers?: Record<string, string>;
  // Shared fields
  enabled?: boolean;
}

// ============================================================================
// OpenClaw-specific configuration (v3.11.0+)
// ============================================================================

// OpenClaw model configuration
export interface OpenClawModel {
  id: string;
  name: string;
  alias?: string;
  reasoning?: boolean; // Whether reasoning modes such as o1 or DeepSeek R1 are supported
  input?: string[]; // Supported input types, such as ["text"] or ["text", "image"]
  cost?: {
    input: number;
    output: number;
    cacheRead?: number; // Cache-read price
    cacheWrite?: number; // Cache-write price
  };
  contextWindow?: number;
  maxTokens?: number; // Maximum output token count
  compat?: {
    maxTokensField?: string;
  };
}

// OpenClaw default model configuration (agents.defaults.model)
export interface OpenClawDefaultModel {
  primary: string;
  fallbacks?: string[];
}

// OpenClaw model catalog entry stored in agents.defaults.models
export interface OpenClawModelCatalogEntry {
  alias?: string;
}

export interface OpenClawHealthWarning {
  code: string;
  message: string;
  path?: string;
}

export interface OpenClawWriteOutcome {
  backupPath?: string;
  warnings: OpenClawHealthWarning[];
}

export type OpenClawToolsProfile = "minimal" | "coding" | "messaging" | "full";

// OpenClaw provider configuration using the settings_config structure.
// Corresponds to OpenClaw models.providers.<provider-id> configuration.
export interface OpenClawProviderConfig {
  baseUrl?: string; // API endpoint
  apiKey?: string; // API key
  api?: string; // API protocol, such as "openai-completions" or "anthropic"
  models?: OpenClawModel[]; // Available models
  headers?: Record<string, string>; // Custom request headers, such as User-Agent
  authHeader?: boolean; // Provider-specific authentication toggle, such as for Longcat
}

// Complete OpenClaw agents.defaults configuration
export interface OpenClawAgentsDefaults {
  model?: OpenClawDefaultModel;
  models?: Record<string, OpenClawModelCatalogEntry>;
  timeoutSeconds?: number;
  timeout?: number;
  [key: string]: unknown; // preserve unknown fields
}

// OpenClaw env configuration from the openclaw.json env node
export interface OpenClawEnvConfig {
  [key: string]: unknown;
}

// OpenClaw tools configuration from the openclaw.json tools node
export interface OpenClawToolsConfig {
  profile?: OpenClawToolsProfile | string;
  allow?: string[];
  deny?: string[];
  [key: string]: unknown; // preserve unknown fields
}

// ============================================================================
// Hermes Agent-specific configuration
// ============================================================================

export interface HermesModelConfig {
  default?: string;
  provider?: string;
  base_url?: string;
  context_length?: number;
  max_tokens?: number;
  [key: string]: unknown;
}

export type HermesMemoryKind = "memory" | "user";

export interface HermesMemoryLimits {
  memory: number;
  user: number;
  memoryEnabled: boolean;
  userEnabled: boolean;
}
