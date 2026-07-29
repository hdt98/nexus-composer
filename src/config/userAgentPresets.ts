/**
 * Custom User-Agent presets.
 *
 * Values are based on curl tests of the Kimi Coding Plan
 * (api.kimi.com/coding) User-Agent allowlist from PR #3671:
 * `claude-cli/*`, `claude-code/*`, and `Kilo-Code/*` pass, while `codex-cli`
 * and `kimi-cli` receive HTTP 403. The allowlist checks only the User-Agent
 * name prefix, not its version, so static values remain valid across Claude
 * Code upgrades.
 *
 * The first entry uses the complete format sent by the official Claude Code
 * CLI; see `claude-cli/2.1.2 (external, cli)` used by `stream_check.rs`. It most
 * closely resembles the real client and is safest for strict validation. The
 * remaining entries are shorter variants.
 *
 * These presets primarily let coding agents outside the allowlist, including
 * Codex, Gemini, Hermes, and OpenClaw, connect to upstreams restricted by
 * User-Agent. They make forwarded requests resemble an allowed client and are
 * used only when explicitly selected by the user.
 */
export const USER_AGENT_PRESETS: readonly string[] = [
  "claude-cli/2.1.161 (external, cli)",
  "claude-cli/2.1.161",
  "claude-code/1.0.0",
  "claude-code/0.1.0",
  "Kilo-Code/1.0",
];
