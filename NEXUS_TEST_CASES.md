# Nexus Composer — Test Cases

## Test Results

### TypeScript typecheck: PASS (0 errors)
### Rust tests: 1727/1727 PASS
### Frontend tests: 373/374 PASS (1 pre-existing flaky timeout)
### E2E tests: 26/26 PASS

## E2E Test Files

- `tests/e2e/nexusFeatures.test.ts` (14 tests): preset arrays, config correctness, sponsor filter
- `tests/e2e/nexusLanguage.test.ts` (9 tests): en/vi locale files, i18n config, diacritics
- `tests/e2e/nexusSwitchFix.test.ts` (3 tests): switch-back-to-official fix verified

## Live Inference Test

Verified end-to-end: assistant → proxy → custom endpoint → response → proxy → assistant
- Format conversion working (Anthropic Messages ↔ OpenAI Chat Completions)
- Usage tracking: requests, tokens, costs, latency all logged correctly
- Switch back to Official: proxy auto-disables, config restored from backup
