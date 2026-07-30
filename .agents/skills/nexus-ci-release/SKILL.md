---
name: nexus-ci-release
description: Validate Nexus Composer changes and prepare reliable branches, pull requests, native packages, and releases. Use when checking CI failures, choosing required test gates, preparing a PR, reconciling a target branch, reviewing packaging metadata, qualifying platform-specific behavior, or deciding whether a change is ready to merge or release.
---

# Nexus CI and Release

## Overview

Use fresh evidence to decide readiness. Read the current workflow files before
assuming that local commands, toolchain versions, platforms, or release targets
have remained unchanged.

## Locate the owned behavior

- Pull-request CI: `.github/workflows/ci.yml`
- Release automation: `.github/workflows/release.yml`
- Frontend commands and dependencies: `package.json` and `pnpm-lock.yaml`
- Rust commands and dependencies: `src-tauri/Cargo.toml` and `Cargo.lock`
- Tauri packaging: `src-tauri/tauri.conf.json` and platform-specific configs
- Packaging regressions: `tests/config/`, installer templates, and icon assets

## Select the validation level

Run focused tests while iterating. Before a pull request, run every gate affected
by the diff. Before a release, run the complete clean-checkout gate plus native
qualification for changed platform behavior.

Current core gates are:

```bash
pnpm install --frozen-lockfile
pnpm typecheck
pnpm format:check
pnpm test:unit
cargo fmt --check --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings
cargo test --manifest-path src-tauri/Cargo.toml
pnpm build:renderer
```

Run `pnpm tauri build` when packaging, bundle configuration, native resources,
or release output changes. Do not treat a renderer build as proof that a native
bundle is correct.

## Diagnose CI failures

1. Open the failing job and identify the first substantive failure.
2. Reproduce the exact job command and relevant operating-system assumptions.
3. Determine whether the failure is source, test isolation, platform support,
   dependency, cache, or workflow configuration.
4. Fix the production contract or test harness that is wrong. Do not skip a
   meaningful test merely because CI exposed it on another platform.
5. Re-run the focused failure, then the complete affected job locally when
   practical.
6. Record environmental limitations honestly when exact reproduction requires a
   different platform or signing identity.

## Prepare a branch and pull request

1. Confirm the authoritative remote, base branch, target branch, and clean
   status before branching.
2. Fetch the target and create one clearly named branch for one scope. Name
   the branch after the product scope, never after the coding agent you are
   (no codex/, claude/, or similar agent-style prefixes). Harness names are
   reserved words in this repository (codex, claude, and gemini are app
   types), so even a scope like Codex model routing must read as product
   scope from the branch name alone. Prefer a name such as
   feat/multi-provider-model-routing over feat/codex-model-routing.
3. Preserve unrelated and untracked files; stage explicit paths only.
4. Review `git diff --check`, the complete diff, and fresh validation output.
5. Commit with a concise conventional message that describes the behavior.
6. Fetch again and rebase or resolve target changes when necessary.
7. Push to the intended repository and open the PR against the confirmed target.
8. Include the problem, solution, validation, and issue linkage in the PR body.
9. Monitor all required checks and investigate failures before recommending a
   merge.

Never merge without explicit authorization. Do not call a PR ready when required
checks are queued, failing, stale, or absent.

## Qualify native releases

For affected platforms, verify the installed artifact rather than only source
assets. Check launch, upgrade, signing or integrity where applicable, bundle
identity, icons, tray behavior, configuration paths, and rollback. Control for
OS caches and pre-existing installations before attributing a visual or runtime
result to the new artifact.
