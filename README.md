# Nexus Composer

> [!IMPORTANT]
> This repository is historical and no longer accepts new development.
> Use [onenexus-team/nexus-composer](https://github.com/onenexus-team/nexus-composer)
> for code, issues, pull requests, and releases. Existing history remains here as
> provenance.

Nexus Composer is a desktop control panel for coding assistants. It connects
local tools like Claude Code, Codex, Gemini CLI, and more to custom model
endpoints through a local proxy with protocol conversion.

## What It Does

- **Provider switching**: Switch coding assistants between custom endpoints and
  their official APIs with one click
- **Protocol conversion**: Local proxy converts between Anthropic Messages,
  OpenAI Responses, and OpenAI Chat Completions formats
- **Multi-app support**: Claude Code, Codex, Gemini CLI, OpenCode, OpenClaw,
  and Hermes — enable only the apps you use
- **Usage tracking**: Logs requests, tokens, costs, and latency through the proxy
- **Extensible**: Add new providers and endpoints as your needs grow

## Getting Started

### Connecting an assistant to a custom endpoint

1. Open Nexus Composer and select the assistant tab (e.g., Claude Code)
2. Click **Add Provider** and select or create a provider preset
3. Switch to the provider to route through the local proxy
4. Enable local proxy in Settings → Routing if format conversion is needed

### Switching back to official API

Click the **Official** provider in the provider list. The proxy takeover is
automatically disabled and your original config is restored.

### Adding a custom provider

1. Select the assistant tab (Claude Code or Codex)
2. Click **Add Provider** → choose **Custom** at the bottom of the preset list
3. Fill in:
   - **Name**: your provider name
   - **Base URL**: your endpoint URL (e.g., `http://localhost:8080/v1`)
   - **API Key**: your key (or a dummy if the endpoint doesn't require auth)
   - **API format**: `openai_chat` for OpenAI-compatible endpoints, `anthropic`
     for Anthropic-compatible, `openai_responses` for OpenAI Responses API
4. Save and switch to the new provider

For Claude Code, the provider configures `ANTHROPIC_BASE_URL`,
`ANTHROPIC_AUTH_TOKEN`, and model mapping in `~/.claude/settings.json`.

For Codex, the provider configures `~/.codex/config.toml` and `~/.codex/auth.json`.

## Building from Source

```bash
pnpm install
pnpm tauri dev
```

Requires Node 22+ and Rust 1.85+.

## License

MIT — Forked from [CC Switch](https://github.com/farion1231/cc-switch)
(v3.16.5), rebranded and customized as Nexus Composer.
