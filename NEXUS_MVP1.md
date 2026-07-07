# Nexus Composer

Nexus Composer is a desktop control panel for coding assistants. It connects
local tools to custom model endpoints through a local proxy with protocol
conversion (Anthropic Messages ↔ OpenAI Chat Completions ↔ OpenAI Responses).

## How It Works

1. You add a provider preset pointing to your custom endpoint
2. You switch to that provider — Nexus Composer writes the config to the
   assistant's settings file (e.g., `~/.claude/settings.json`)
3. If the endpoint speaks a different protocol, enable the local proxy in
   Settings → Routing — the proxy converts formats transparently
4. The assistant sends requests to the proxy, which forwards to your endpoint

## Switching Back to Official

Click the **Official** provider in the provider list. The proxy takeover is
automatically disabled and your original config is restored from backup.

## Supported Assistants

- Claude Code (CLI)
- Codex (CLI)
- Claude Desktop
- Gemini CLI
- OpenCode
- OpenClaw
- Hermes

## License

MIT — Forked from CC Switch (v3.16.5).
