---
name: nexus-database-safety
description: Design, implement, diagnose, or review Nexus Composer persistent-storage changes safely. Use for SQLite schema versions, migrations, data-access objects, backups, integrity checks, database-too-new handling, app-data path resolution, legacy storage consolidation, interrupted migration recovery, restore behavior, or any change that can make providers, settings, usage, skills, or saved connections appear lost.
---

# Nexus Database Safety

## Overview

Treat persistence changes as recovery work first and schema work second. A
successful migration must preserve data, select the correct source, survive
interruption, and fail safely on unsupported versions.

## Locate the owned behavior

- App and path configuration: `src-tauri/src/config.rs` and
  `src-tauri/src/app_config.rs`
- Database initialization and supported version:
  `src-tauri/src/database/mod.rs`
- Schema creation and ordered upgrades: `src-tauri/src/database/schema.rs`
- Data migrations and repairs: `src-tauri/src/database/migration.rs`
- Backups: `src-tauri/src/database/backup.rs`
- Data access: `src-tauri/src/database/dao/`
- Database regression coverage: `src-tauri/src/database/tests.rs` and
  database-focused Rust integration tests

## Separate two migration classes

Do not combine these concerns in one opaque step:

1. **Storage-path migration:** choose and promote an authoritative app-data
   directory or database file among legacy and current candidates.
2. **SQLite schema migration:** upgrade the selected database's
   `PRAGMA user_version` through ordered, tested steps.

Resolve and preserve the physical source before mutating its schema.

## Design a safe migration

1. Inventory every supported source and destination without writing.
2. Select the authoritative source using deterministic content and version
   rules, not modification time alone.
3. Check SQLite readability, integrity, and `user_version` before mutation.
4. Stop with an actionable database-too-new result when the stored version is
   newer than the application supports.
5. Back up every candidate and any conflicting destination before promotion.
6. Perform one ordered schema step per version and advance `user_version` only
   after that step succeeds.
7. Use transactions for schema and data changes; use atomic filesystem
   promotion for path changes.
8. Record a completion marker or migration result only after verification.
9. Keep backups until the upgraded application has reopened and validated the
   promoted database.

## Preserve database invariants

- Never downgrade a newer database in place.
- Never delete a conflicting source to make migration appear successful.
- Keep startup, backup, restore, diagnostics, and tests on the same path
  resolver.
- Make migrations idempotent after interruption and safe to retry.
- Preserve unknown user rows and unrelated settings.
- Avoid network or external process calls while holding a database lock.
- Keep compatibility repairs explicit; do not silently overload a schema
  version with untracked shapes.
- Return user-safe errors without embedding private row content.

## Test from real prior states

For every new schema version, test fresh creation, each supported predecessor,
repeated startup, partial-shape repair when intentionally supported, and a
newer-version rejection. For path work, test no source, one source, conflicting
sources, interrupted promotion, corrupt source, and rollback on each supported
platform.

Run focused database tests while iterating, then the complete Rust test suite.
Inspect the resulting schema and representative row counts rather than relying
only on a successful open.
