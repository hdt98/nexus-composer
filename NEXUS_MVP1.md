# Nexus Composer MVP1

Nexus Composer is a desktop control panel that connects local coding assistants
(Codex and Claude Code) to a single GLM-5.2 model endpoint served by SGLang.

MVP1 is a focused rebrand of the open-source [CC Switch](https://github.com/farion1231/cc-switch)
desktop app (MIT license, preserved in [LICENSE](LICENSE)). It reuses CC Switch's
existing local proxy/protocol-conversion layer so that Codex (OpenAI Responses)
and Claude Code (Anthropic Messages) can both speak to the OpenAI-compatible
Chat Completions endpoint exposed by SGLang.

## Prerequisite: SGLang Tunnel

Nexus Composer MVP1 does **not** start, stop, or manage the SGLang service.
The endpoint is externally managed. Before launching Nexus Composer, ensure the
SSH tunnel to the SGLang endpoint is running on this Mac:

```
http://127.0.0.1:30000/v1
```

Quick health check:

```bash
curl http://127.0.0.1:30000/v1/models
```

You should see `GLM-5.2-SGLang` in the response. The Home dashboard also probes
this endpoint automatically every 15 seconds and shows a reachable/unreachable
status.

## Connecting Codex

1. Open Nexus Composer and switch to the **Codex** app tab.
2. Click **Add Provider** and select the **Nexus GLM-5.2** preset.
3. The preset is preconfigured with:
   - Base URL: `http://127.0.0.1:30000/v1`
   - API format: `openai_chat` (CC Switch's proxy converts Codex Responses to Chat Completions)
   - Model: `GLM-5.2-SGLang` (1,048,576 token context)
4. Switch to the Nexus GLM-5.2 provider to activate it.

## Connecting Claude Code

1. Open Nexus Composer and switch to the **Claude Code** app tab.
2. Click **Add Provider** and select the **Nexus GLM-5.2** preset.
3. The preset is preconfigured with:
   - `ANTHROPIC_BASE_URL`: `http://127.0.0.1:30000/v1`
   - `ANTHROPIC_AUTH_TOKEN`: `nexus-local` (dummy; SGLang does not enforce auth)
   - All Claude model roles mapped to `GLM-5.2-SGLang`
   - API format: `openai_chat` (CC Switch's proxy converts Anthropic Messages to Chat Completions)
4. Switch to the Nexus GLM-5.2 provider to activate it.

## What MVP1 Does Not Include

The long-term Nexus roadmap includes a local gateway/router (Trinity/Conductor),
trace store, inference serving, offline eval, training platform, and model
registry. **None of these are implemented in MVP1.** MVP1 connects assistants
directly to a single endpoint via CC Switch's existing conversion layer.

## Development

```bash
pnpm install
pnpm tauri dev
```

Requires Node 22+ and Rust 1.85+.
