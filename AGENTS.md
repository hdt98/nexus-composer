# Repository Agent Guide

## Scope

Apply these instructions throughout the repository. A more specific `AGENTS.md`
in a subdirectory may override them for that subtree.

## Project structure

- `src/`: React and TypeScript renderer code.
- `src/lib/api/`: typed wrappers around Tauri commands.
- `src/hooks/`: renderer state, queries, and mutations.
- `src-tauri/src/commands/`: thin IPC entry points.
- `src-tauri/src/services/`: native business logic and orchestration.
- `src-tauri/src/proxy/`: routing, protocol adaptation, streaming, and usage
  outcomes.
- `src-tauri/src/database/`: schema, migrations, backups, and data access.
- `tests/` and `src-tauri/tests/`: frontend and Rust integration coverage.

Keep complex business logic out of React components and Tauri command
handlers. Put reusable behavior in the appropriate hook, service, proxy, or
database module.

## Working principles

1. Read the relevant implementation and tests before changing behavior.
2. Establish the root cause of a bug before proposing a fix.
3. Treat the issue acceptance criteria and current repository behavior as the
   source of truth.
4. Keep changes small, cohesive, and limited to the requested scope.
5. Follow existing subsystem conventions instead of introducing parallel
   abstractions.
6. Preserve unrelated user changes and untracked files.
7. Ask before performing destructive operations or expanding the task beyond
   its stated scope.

## Safety and data handling

- Never commit credentials, access tokens, private configuration, or real user
  data.
- Redact sensitive values from logs, diagnostics, fixtures, and screenshots.
- Design external configuration and persistent-data writes for interruption,
  validation failure, and recovery.
- Require backup and rollback behavior for destructive changes and migrations.
- Do not terminate or reconfigure user-owned processes unless the requested
  workflow explicitly authorizes it.
- Avoid duplicating configuration, protocol, or persistence logic across
  supported clients.

## Language

- Write source code, comments, logs, tests, and developer-facing errors in
  English.
- Put user-facing copy in the localization system.
- Keep every supported locale catalog structurally and semantically aligned.
- Do not translate protocol identifiers, model identifiers, user-provided
  values, or compatibility fixtures.

## Verification

- Run checks that directly exercise the changed behavior.
- For frontend changes, run formatting, type checking, relevant tests, and a
  renderer build as appropriate.
- For Rust changes, run Rust formatting and the relevant Cargo tests; run
  Clippy when production code changes.
- Qualify native, process, filesystem, networking, and cross-platform behavior
  on an appropriate target when unit tests cannot prove it.
- Do not claim that work is fixed, passing, or complete without fresh command
  output that supports the claim.

## Git hygiene

- Confirm the intended base and target branches before creating a branch.
- Keep one clear scope per branch and pull request.
- Stage only files that belong to the requested change.
- Rebase or otherwise reconcile the latest target branch before handoff when
  necessary.
- Do not use destructive Git commands to clean a worktree.
- Do not merge without explicit user or maintainer authorization.

## Repository skills

Read the matching skill before working in one of these domains:

- Provider presets, models, and client integration:
  `.agents/skills/nexus-provider-integration/SKILL.md`
- Proxy conversion, streaming, failover, health, and request outcomes:
  `.agents/skills/nexus-request-lifecycle/SKILL.md`
- Local client configuration, authentication, activation, and history safety:
  `.agents/skills/nexus-client-profile-safety/SKILL.md`
- SSH connections and the Remote Agent lifecycle:
  `.agents/skills/nexus-remote-agent/SKILL.md`
- Database schema, storage paths, migrations, and recovery:
  `.agents/skills/nexus-database-safety/SKILL.md`
- Product language and localization:
  `.agents/skills/nexus-localization/SKILL.md`
- CI, pull requests, packaging, and releases:
  `.agents/skills/nexus-ci-release/SKILL.md`

Use multiple skills when a change crosses boundaries. Keep detailed subsystem
rules in the skills rather than expanding this file with issue-specific policy.

## Generated content

Do not edit dependency directories, build output, or generated artifacts as
source. Examples include `node_modules/`, `dist/`, and Cargo `target/`
directories. Regenerate artifacts from their owned source when required.
