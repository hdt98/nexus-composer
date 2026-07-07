# Nexus Composer

Nexus Composer is a desktop control panel that connects local coding assistants
(Claude Code and Codex) to a single GLM-5.2 model endpoint served by SGLang.

## What It Does

- **Provider switching**: Switch Claude Code and Codex between the Nexus GLM-5.2
  endpoint and their official APIs with one click
- **Protocol conversion**: Reuses the local proxy to convert between
  Anthropic Messages / OpenAI Responses and OpenAI Chat Completions formats
- **Endpoint health**: Monitors the SGLang endpoint status
- **Usage tracking**: Logs requests, tokens, and latency through the proxy

## Prerequisites

- An SGLang endpoint running GLM-5.2 (e.g., via SSH tunnel on `http://127.0.0.1:30000/v1`)
- Claude Code CLI and/or Codex installed on your machine

## Connecting Claude Code

1. Open Nexus Composer and select the **Claude Code** tab
2. Click **Add Provider** and select the **Nexus GLM-5.2** preset
3. Switch to the Nexus GLM-5.2 provider to route through the proxy
4. Enable local proxy in Settings → Routing if format conversion is needed

To switch back to normal Anthropic: click **Claude Official** in the provider list.
The proxy takeover is automatically disabled and your original config is restored.

## Connecting Codex

1. Select the **Codex** tab
2. Add the **Nexus GLM-5.2** preset
3. Switch to it to route through the proxy

## Building from Source

```bash
pnpm install
pnpm tauri dev
```

Requires Node 22+ and Rust 1.85+.

## License

MIT — Forked from [CC Switch](https://github.com/farion1231/cc-switch)
(v3.16.5), rebranded and customized as Nexus Composer.
