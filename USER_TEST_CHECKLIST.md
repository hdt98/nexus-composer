# Nexus Composer — User Test Checklist

## Feature 1: Provider Management

### 1.1 Provider Listing
- **Test**: Open app, select Claude Code tab, verify provider list shows "Nexus Local" and "Claude Official"
- **DoD**: Both providers visible, Nexus Local marked as "In Use"
- **Evidence**: _(to be filled)_

### 1.2 Provider Switching
- **Test**: Switch from Nexus Local to Claude Official, verify ~/.claude/settings.json env becomes empty
- **DoD**: settings.json env={} after switch, proxy takeover disabled
- **Evidence**: _(to be filled)_

### 1.3 Provider Switch Back
- **Test**: Switch back to Nexus Local, verify proxy re-activates
- **DoD**: settings.json shows PROXY_MANAGED, proxy port 15721 listening
- **Evidence**: _(to be filled)_

### 1.4 Add Custom Provider
- **Test**: Add a new provider via DB, verify it appears in provider list
- **DoD**: New provider visible with correct settings_config
- **Evidence**: _(to be filled)_

### 1.5 Delete Provider
- **Test**: Delete the custom provider, verify it's gone
- **DoD**: Provider count decreases by 1
- **Evidence**: _(to be filled)_

### 1.6 Provider Icon/Color Customization
- **Test**: Set custom icon and color on a provider, verify persistence
- **DoD**: icon and icon_color fields saved and readable
- **Evidence**: _(to be filled)_

### 1.7 Provider Cost Multiplier & Limits
- **Test**: Set cost_multiplier=1.5, limit_daily_usd=$10, limit_monthly_usd=$300, verify persistence
- **DoD**: All three fields saved and readable, reset to defaults after test
- **Evidence**: _(to be filled)_

## Feature 2: Proxy with Protocol Conversion

### 2.1 Basic Proxy Request
- **Test**: Send Anthropic Messages format request to proxy, verify GLM-5.2 responds correctly
- **DoD**: Response contains correct answer, model=glm-5.2, usage tokens populated
- **Evidence**: _(to be filled)_

### 2.2 Streaming Response
- **Test**: Send streaming request, verify SSE events (message_start, content_block, message_stop)
- **DoD**: All three SSE event types present in response stream
- **Evidence**: _(to be filled)_

### 2.3 Multi-turn Conversation
- **Test**: Send multi-turn request with prior assistant message, verify context is maintained
- **DoD**: Response correctly answers the latest question using conversation context
- **Evidence**: _(to be filled)_

### 2.4 System Prompt Passthrough
- **Test**: Send request with system prompt (pirate persona), verify response follows it
- **DoD**: Response contains "Arrr" or pirate language
- **Evidence**: _(to be filled)_

### 2.5 Error Handling
- **Test**: Send request with invalid model name, verify graceful error response
- **DoD**: Error response returned without crash, HTTP error code
- **Evidence**: _(to be filled)_

### 2.6 Proxy Takeover & Restore
- **Test**: Verify proxy is active (port 15721), settings.json has PROXY_MANAGED, backup exists in DB
- **DoD**: Proxy listening, AUTH_TOKEN=PROXY_MANAGED, proxy_live_backup has claude entry
- **Evidence**: _(to be filled)_

### 2.7 Thinking Budget Rectifier
- **Test**: Send request with max_tokens=100 and budget_tokens=50000, verify rectifier bumps max_tokens
- **DoD**: Request succeeds (not rejected), correct answer returned
- **Evidence**: _(to be filled)_

### 2.8 Thinking Optimization
- **Test**: Send request with thinking enabled, verify both thinking and text blocks in response
- **DoD**: Response content array has blocks with type=thinking and type=text
- **Evidence**: _(to be filled)_

## Feature 3: Usage Tracking & Statistics

### 3.1 Real-time Request Logging
- **Test**: Send a proxy request, verify a new row appears in proxy_request_logs immediately
- **DoD**: Row count increases by 1, new row has correct provider_id, model, tokens
- **Evidence**: _(to be filled)_

### 3.2 Cost Calculation Correctness
- **Test**: Verify input_cost, output_cost, cache_read_cost match GLM-5.2 pricing ($1.4/M in, $4.4/M out, $0.26/M cache_read)
- **DoD**: Calculated costs match expected formula within rounding
- **Evidence**: _(to be filled)_

### 3.3 Session Log Cost Correctness
- **Test**: Verify session_log entries use GLM-5.2 pricing (not Claude pricing) when proxy is active
- **DoD**: pricing_model=glm-5.2, costs at GLM-5.2 rates
- **Evidence**: _(to be filled)_

### 3.4 No NULL/Empty Fields
- **Test**: Query for NULL model, NULL cost, NULL latency, zero-token 200 responses
- **DoD**: Zero NULLs, zero zero-token-200s
- **Evidence**: _(to be filled)_

### 3.5 No Duplicate Request IDs
- **Test**: Check for duplicate request_id values
- **DoD**: All request_ids unique
- **Evidence**: _(to be filled)_

### 3.6 No Negative Tokens
- **Test**: Check for negative input_tokens or output_tokens
- **DoD**: Zero negative token counts
- **Evidence**: _(to be filled)_

### 3.7 Daily Rollup Aggregation
- **Test**: Compare rollup totals vs raw log totals, verify within reasonable range
- **DoD**: Rollup data within 5x of raw (includes historical pre-dedup data)
- **Evidence**: _(to be filled)_

### 3.8 Usage Summary by App
- **Test**: Query usage grouped by app_type, verify Claude and Codex separated
- **DoD**: Both app types present with correct token/cost sums
- **Evidence**: _(to be filled)_

### 3.9 Usage by Model
- **Test**: Query usage grouped by model, verify glm-5.2 is the dominant model
- **DoD**: glm-5.2 has most requests and tokens
- **Evidence**: _(to be filled)_

### 3.10 Data Source Breakdown
- **Test**: Query usage grouped by data_source (proxy, session_log, codex_session)
- **DoD**: All three data sources present with correct counts
- **Evidence**: _(to be filled)_

## Feature 4: Session History

### 4.1 Session File Sync
- **Test**: Verify session_log_sync table has entries for both Claude and Codex
- **DoD**: Both app types present, files synced with last_line_offset > 0
- **Evidence**: _(to be filled)_

### 4.2 Session Dedup
- **Test**: Verify no duplicate request_ids across session and proxy sources
- **DoD**: All request_ids unique across all data sources
- **Evidence**: _(to be filled)_

## Feature 5: MCP Server Management

### 5.1 MCP Server Listing
- **Test**: Verify MCP servers in DB match live config files
- **DoD**: DB mcp_servers count matches, enabled flags correct per app
- **Evidence**: _(to be filled)_

### 5.2 MCP CRUD Operations
- **Test**: Verify upsert/delete/validate commands exist and Rust tests pass
- **DoD**: All MCP Rust tests pass
- **Evidence**: _(to be filled)_

## Feature 6: Skills Management

### 6.1 Skills CRUD
- **Test**: Insert a test skill, toggle it, delete it — verify each operation
- **DoD**: Insert, toggle, delete all succeed
- **Evidence**: _(to be filled)_

### 6.2 Skills API
- **Test**: Verify install/uninstall/toggle/import/scan commands exist and Rust tests pass
- **DoD**: All skill Rust tests pass
- **Evidence**: _(to be filled)_

## Feature 7: Prompts

### 7.1 Prompts CRUD
- **Test**: Insert a test prompt, edit it, delete it — verify each operation
- **DoD**: Insert, edit, delete all succeed
- **Evidence**: _(to be filled)_

## Feature 8: Agents Panel

### 8.1 Agents Panel Render
- **Test**: Verify AgentsPanel component exists and renders
- **DoD**: Component exported, OpenClaw agent defaults API present
- **Evidence**: _(to be filled)_

## Feature 9: Universal Providers

### 9.1 Universal Provider API
- **Test**: Verify CRUD + sync commands exist, Rust tests pass
- **DoD**: get/upsert/delete/sync all present
- **Evidence**: _(to be filled)_

## Feature 10: App Switcher

### 10.1 All Apps Visible
- **Test**: Verify 7 apps configured (claude, claude-desktop, codex, gemini, opencode, openclaw, hermes)
- **DoD**: All 7 app IDs in VALID_APPS array
- **Evidence**: _(to be filled)_

### 10.2 App Visibility Toggle
- **Test**: Verify visibility settings exist in settings page
- **DoD**: AppVisibilitySettings component present in settings
- **Evidence**: _(to be filled)_

## Feature 11: Failover

### 11.1 Failover Queue
- **Test**: Add a provider to failover queue, verify, remove it
- **DoD**: Queue add and remove both succeed
- **Evidence**: _(to be filled)_

### 11.2 Circuit Breaker
- **Test**: Verify Rust tests for closed/open/half-open/reset transitions pass
- **DoD**: 4 circuit breaker tests pass
- **Evidence**: _(to be filled)_

### 11.3 Failover Router
- **Test**: Verify Rust tests for failover queue ordering pass
- **DoD**: 4 failover router tests pass
- **Evidence**: _(to be filled)_

## Feature 12: Settings

### 12.1 Settings Tabs
- **Test**: Verify all 6 tabs present (general, proxy, auth, advanced, usage, about)
- **DoD**: All 6 TabsTrigger values present in SettingsPage
- **Evidence**: _(to be filled)_

### 12.2 Settings Persistence
- **Test**: Verify currentProviderClaude and currentProviderCodex saved in settings.json
- **DoD**: Both values present and correct
- **Evidence**: _(to be filled)_

## Feature 13: Import/Export

### 13.1 Export Round-trip
- **Test**: Export providers to JSON, reload, verify count matches
- **DoD**: Exported count == reloaded count
- **Evidence**: _(to be filled)_

## Feature 14: Theme & Language

### 14.1 Theme Toggle
- **Test**: Verify useDarkMode hook observes html class, theme-provider toggles dark class
- **DoD**: Hook exists with MutationObserver, theme-provider component present
- **Evidence**: _(to be filled)_

### 14.2 Language Switching
- **Test**: Verify en and vi locales exist, i18next configured
- **DoD**: Both locale files present, getInitialLanguage reads localStorage
- **Evidence**: _(to be filled)_

## Feature 15: Health Checks

### 15.1 Provider Health
- **Test**: Verify provider_health table populated, nexus-glm-5-2 is healthy
- **DoD**: nexus-glm-5-2 is_healthy=1, consecutive_failures=0
- **Evidence**: _(to be filled)_

## Feature 16: Global Proxy

### 16.1 Global Proxy Config
- **Test**: Verify global_proxy_url setting is readable (None = direct mode)
- **DoD**: Setting exists, correctly None when not configured
- **Evidence**: _(to be filled)_

## Feature 17: Deep Link Import

### 17.1 Deep Link Parsing
- **Test**: Verify 32 Rust tests for URL parsing, validation, import pass
- **DoD**: 32 deeplink tests pass
- **Evidence**: _(to be filled)_

## Feature 18: WebDAV Sync

### 18.1 WebDAV Sync
- **Test**: Verify 27 Rust tests for upload/download/archive pass
- **DoD**: 27 webdav tests pass
- **Evidence**: _(to be filled)_

## Feature 19: Subscription Quota

### 19.1 Subscription API
- **Test**: Verify subscription quota command exists, Rust tests pass
- **DoD**: get_subscription_quota command present, 5 tests pass
- **Evidence**: _(to be filled)_

## Feature 20: Claude Desktop Isolation

### 20.1 Claude Desktop Untouched
- **Test**: Verify Claude Desktop config has empty env (Anthropic default)
- **DoD**: env={} in claude_desktop_config.json
- **Evidence**: _(to be filled)_

## Feature 21: Live Inference

### 21.1 Claude CLI via Proxy
- **Test**: Run `claude --print "What is 3*4?"`, verify correct answer via GLM-5.2
- **DoD**: Response is "12", model=glm-5.2
- **Evidence**: _(to be filled)_

### 21.2 Codex CLI Direct
- **Test**: Run `codex exec --skip-git-repo-check "What is 5*6?"`, verify correct answer
- **DoD**: Response is "30", model=glm-5.2
- **Evidence**: _(to be filled)_

### 21.3 Codex Code Generation Quality
- **Test**: Ask Codex to write a Python function, verify code quality
- **DoD**: Response contains valid Python (def, return, sum)
- **Evidence**: _(to be filled)_

## Feature 22: Model Fetch

### 22.1 Model List from Endpoint
- **Test**: GET /v1/models from GLM endpoint, verify glm-5.2 in list
- **DoD**: Response contains model id "glm-5.2"
- **Evidence**: _(to be filled)_

### 22.2 Model Context Window
- **Test**: Verify max_model_len reported by endpoint
- **DoD**: max_model_len = 1048576
- **Evidence**: _(to be filled)_

## Feature 23: Speed Test

### 23.1 Speed Test API
- **Test**: Verify 3 Rust tests for endpoint latency pass
- **DoD**: 3 speedtest tests pass
- **Evidence**: _(to be filled)_

## Summary

- **Total Features**: 23
- **Total Tests**: 52
- **Passed**: _(to be filled)_
- **Failed**: _(to be filled)_
