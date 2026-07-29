---
name: nexus-client-profile-safety
description: Change or review local coding-client profiles without losing authentication, configuration, sessions, skills, MCP settings, or user state. Use for provider activation, official-to-custom switching, config.toml or auth.json writes, takeover state, model catalog projection, history unification or migration, restart-required behavior, rollback, and Codex Desktop process interaction.
---

# Nexus Client Profile Safety

## Overview

Treat client-owned files and histories as user data. Make provider activation a
recoverable transaction and distinguish saved intent from the configuration a
running client has actually adopted.

## Locate the owned behavior

- Codex profile generation and validation: `src-tauri/src/codex_config.rs`
- Local proxy takeover and restoration: `src-tauri/src/services/proxy.rs`
- History metadata migration: `src-tauri/src/codex_history_migration.rs`
- OAuth and model visibility: `src-tauri/src/commands/codex_oauth.rs` and
  `src-tauri/src/services/codex_oauth_models.rs`
- Session readers: `src-tauri/src/session_manager/`
- Renderer switching flow: `src/hooks/useProviderActions.ts` and provider UI
- Relevant regression coverage: `tests/e2e/nexusSwitchFix.test.ts` and session,
  OAuth, and provider-action tests

## Use a profile transaction

1. Resolve the actual client home and every file the operation may change.
2. Inspect the current profile without logging credentials or full private
   configuration.
3. Acquire the canonical per-client operation lock before reading the state
   used to write.
4. Create a verified backup that records whether each original file existed.
5. Stage generated files in the destination filesystem and validate their
   syntax and required references.
6. Replace files atomically and preserve restrictive permissions.
7. Determine whether the running client adopted the new profile. Report a
   precise restart-required state when live reload is unavailable.
8. Commit the saved state only after projection succeeds; otherwise restore the
   original profile and saved intent.

## Protect user state

- Never erase unrelated configuration sections, Skills, MCP servers, project
  trust, plugins, or desktop settings.
- Keep official account credentials separate from custom provider credentials.
- Preserve the original OAuth file when the selected provider does not own it.
- Use durable model-catalog paths and verify referenced files exist.
- Do not signal or terminate a user-owned desktop process to simulate reload.
- Do not claim a provider is active based only on a successful file write.
- Keep history metadata migration reversible and backed up.
- Do not promise that encrypted reasoning content can resume across backends
  that cannot decrypt one another's payloads.
- Make repeated enable, disable, restore, and failed retry operations idempotent.

## Test transitions, not only final files

Cover at least:

- Official to custom and custom to Official.
- Repeated switching to the same provider.
- Existing and missing config or auth files.
- Validation failure, write failure, activation timeout, and rollback.
- A running client that requires user restart.
- Preserved OAuth, history, Skills, MCP settings, and unrelated configuration.
- Concurrent provider update, delete, switch, and route projection where
  applicable.

Run focused frontend and Rust tests for the changed transaction, then perform a
native qualification when behavior depends on the real desktop client.
