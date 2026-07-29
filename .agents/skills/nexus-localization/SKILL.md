---
name: nexus-localization
description: Add, revise, audit, or review Nexus Composer product language and maintained text. Use for English or Vietnamese UI copy, i18n keys, locale catalogs, language switching, interpolation, fallback text, logs and developer messages, removal of unintended Chinese text, language-quality review, or CI checks that enforce locale and script policy.
---

# Nexus Localization

## Overview

Maintain a complete English and Vietnamese product experience while keeping
source and developer-facing text in English. Treat catalog parity as a minimum
correctness gate, not proof of natural translation quality.

## Locate the owned behavior

- i18n initialization and supported languages: `src/i18n/index.ts`
- Locale catalogs: `src/i18n/locales/en.json` and
  `src/i18n/locales/vi.json`
- Language selector: `src/components/settings/LanguageSettings.tsx`
- Catalog and language-policy tests: `tests/e2e/nexusLanguage.test.ts`

User-facing copy in Rust should return stable structured information where
possible and let the renderer localize it. When a native error must contain
text, write a clear English message and avoid concatenating secrets.

## Change product copy

1. Identify every reachable surface: page, dialog, toast, validation message,
   empty state, error state, tray item, and accessibility label.
2. Reuse an existing semantic key when the meaning is identical; otherwise add
   a descriptive key in the correct catalog section.
3. Update English and Vietnamese entries together.
4. Preserve interpolation variable names and value types across catalogs.
5. Use concise, natural software terminology rather than literal translation.
6. Verify runtime-computed keys and fallback paths, not only literal `t()`
   calls.
7. Exercise English-to-Vietnamese switching, reload, and switching back when UI
   behavior changes.

## Audit maintained text

Keep source code, comments, logs, tests, and developer documentation in English.
Scan maintained text for unintended Han-script characters. Classify each match
before editing it:

- Replace owned UI, log, error, comment, test, and documentation text.
- Preserve exact external protocol values, third-party compatibility fixtures,
  proper names, and user-provided data only when required.
- Record unavoidable matches in a small explicit allowlist with a reason.
- Exclude dependencies, generated files, build output, and binary assets from
  the scan rather than rewriting them.

Do not perform blind bulk replacement. A translated error can affect assertions,
API behavior, or compatibility, and a Chinese string may be an exact external
fixture rather than owned prose.

## Protect localization invariants

- Keep only supported locale files registered in the i18n configuration.
- Keep catalog trees, value types, and interpolation tokens aligned.
- Keep internal implementation names out of ordinary product copy.
- Do not translate model IDs, protocol names, identifiers, file content, or
  values supplied by the user.
- Add a regression test or repository scan when removing a recurring class of
  language defect.

## Verify

Run the focused language test and the relevant component tests:

```bash
pnpm exec vitest run tests/e2e/nexusLanguage.test.ts
pnpm typecheck
pnpm build:renderer
```

For broad language work, inspect the native application in both languages and
check dialogs, toasts, tray labels, clipping, and mixed-language fallback states.
