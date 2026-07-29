---
name: nexus-provider-integration
description: Add, update, or review Nexus Composer providers and model presets across Codex, Claude, Claude Desktop, Gemini, and other supported clients. Use when changing provider catalogs, endpoints, authentication modes, model capabilities, model catalogs, request defaults, provider forms, or cross-client preset consistency. Do not use for low-level stream conversion or terminal-outcome behavior; use nexus-request-lifecycle for those changes.
---

# Nexus Provider Integration

## Overview

Treat a provider as a cross-layer contract, not a label plus an endpoint. Keep
the stored provider, generated client configuration, UI form, model catalog,
authentication policy, and tests consistent.

## Locate the owned behavior

- Preset catalogs: `src/config/*ProviderPresets.ts`
- Provider forms and normalization: `src/components/providers/` and
  `src/utils/providerConfigUtils.ts`
- Renderer API and mutations: `src/lib/api/providers.ts` and provider hooks
- Persisted provider types and defaults: `src-tauri/src/provider.rs`,
  `src-tauri/src/provider_defaults.rs`, and `src-tauri/src/services/provider/`
- Client configuration writers: `src-tauri/src/codex_config.rs`,
  `src-tauri/src/claude_desktop_config.rs`, and
  `src-tauri/src/gemini_config.rs`
- Routing and protocol adapters: `src-tauri/src/proxy/providers/`; switch to
  `nexus-request-lifecycle` when changing these modules

## Define the contract first

Before editing code, record:

1. Supported clients and the exact upstream API format.
2. Exact model identifiers and user-facing display names.
3. Authentication requirements and whether an API key is optional.
4. Input modalities, context window, reasoning behavior, and tool support.
5. Endpoint ownership, timeout expectations, and request overrides.
6. Whether the provider is official, managed, user-defined, or remote-only.

Do not infer capabilities from a model name or copy another provider's settings
without verifying the upstream contract.

## Implement across layers

1. Follow the nearest existing preset for the same client and API format.
2. Update the canonical preset and shared metadata before adding UI exceptions.
3. Keep form defaults, saved values, and generated configuration round-trippable.
4. Preserve unknown user configuration unless the provider contract explicitly
   owns that field.
5. Keep model catalogs and capability metadata aligned with what the client can
   actually select and send.
6. Add client-specific behavior only at a real client boundary.
7. Update focused tests for presets, forms, generated configuration, and saved
   provider behavior.

## Protect these invariants

- Never bundle a real credential in a preset, fixture, or generated catalog.
- Keep official account authentication separate from custom-provider
  credentials.
- Keep model IDs exact even when display names differ.
- Declare text-only providers as text-only and reject unsupported input before
  forwarding it.
- Do not expose a provider as active until the effective client configuration
  matches the saved selection.
- Do not make provider switching responsible for protocol conversion; route
  conversion through the proxy adapter layer.

## Verify

Run the smallest relevant provider tests first, then the full frontend or Rust
gate for the layers changed. Common coverage includes:

```bash
pnpm exec vitest run tests/e2e/nexusFeatures.test.ts
pnpm exec vitest run tests/components/ProviderForm.nexus.test.tsx
pnpm exec vitest run tests/utils/providerConfigUtils.codex.test.ts
cargo test --manifest-path src-tauri/Cargo.toml provider
```

Also run type checking and a renderer build when forms or TypeScript contracts
change. Use `nexus-client-profile-safety` when activation writes client-owned
files or changes session visibility.
