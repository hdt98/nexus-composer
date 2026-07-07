# Nexus Composer

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
- **Usage tracking**: Logs requests, tokens, and latency through the proxy
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

## Building from Source

```bash
pnpm install
pnpm tauri dev
```

Requires Node 22+ and Rust 1.85+.

## License

MIT — Forked from [CC Switch](https://github.com/farion1231/cc-switch)
(v3.16.5), rebranded and customized as Nexus Composer.
