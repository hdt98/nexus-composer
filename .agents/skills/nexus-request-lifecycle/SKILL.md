---
name: nexus-request-lifecycle
description: Diagnose, implement, or review Nexus Composer proxy request behavior from ingress through downstream completion. Use for protocol conversion, streaming and SSE framing, reasoning or tool-call adaptation, retries, failover, circuit breaking, cancellation, provider health, usage attribution, terminal outcomes, or cases where transport success differs from client-visible success.
---

# Nexus Request Lifecycle

## Overview

Model each request as a lifecycle with one definitive outcome. Do not equate an
upstream HTTP response with a successful client-visible result.

## Follow the lifecycle

```text
ingress -> validate -> select provider -> attempt upstream
        -> decode -> transform -> stream or buffer downstream
        -> classify terminal outcome -> finalize usage and health once
```

Keep these facts distinct:

- Whether the transport connected and returned headers.
- Whether the upstream payload was valid and convertible.
- Whether the client received visible text, a refusal, or a valid tool call.
- Whether the provider should affect circuit and health state.
- Whether the client cancelled or timed out independently.

## Locate the owned behavior

- Server and handler flow: `src-tauri/src/proxy/server.rs`, `handlers.rs`, and
  `forwarder.rs`
- Provider selection and failover: `provider_router.rs`, `failover_switch.rs`,
  and `circuit_breaker.rs`
- Protocol adapters: `src-tauri/src/proxy/providers/`
- SSE and streaming: `sse.rs`, `response_handler.rs`, and provider streaming
  modules
- Outcome and usage recording: `response_processor.rs`, `proxy/usage/`, and
  `src-tauri/src/services/usage_*`
- Route lifecycle and locking: `src-tauri/src/services/proxy.rs` and
  `switch_lock.rs`

## Diagnose before editing

1. Reproduce with a stable request or correlation identifier.
2. Identify the client protocol, upstream protocol, streaming mode, and selected
   provider attempt.
3. Find the last correct lifecycle stage and the first incorrect stage.
4. Classify the terminal state: complete, incomplete, provider failure, local
   conversion failure, client cancellation, or local rejection.
5. Compare the client-visible result with Usage, proxy counters, provider
   health, and circuit state.
6. Fix the stage that owns the first incorrect fact rather than patching the UI
   symptom.

## Preserve lifecycle invariants

- Finalize each inbound request and provider attempt exactly once.
- Preserve request, session, provider, and upstream correlation through errors.
- Keep client-invalid and pre-routing failures provider-neutral.
- Treat client cancellation as provider-neutral unless provider failure is
  independently established.
- Do not mark semantic, decode, transform, or empty-visible failures successful
  merely because upstream headers were 2xx.
- Preserve event order, tool-call structure, refusals, and intentional literal
  text across streaming and non-streaming conversions.
- Never expose private reasoning to fabricate visible content.
- Bound retained stream state; do not solve framing bugs by buffering an entire
  long response.
- Reject late data after a terminal event and release circuit or failover
  permits exactly once.
- Keep single-provider load shedding distinct from multi-provider failover.

## Test the outcome matrix

Cover both streaming and non-streaming paths where applicable:

- Clean visible completion.
- Refusal and tool-call completion.
- Output-cap or other incomplete terminal.
- Invalid, truncated, or undecodable upstream data.
- Hidden-reasoning-only or empty-visible terminal.
- Client cancellation and downstream disconnect.
- Provider outage, open circuit, half-open recovery, and failover.

Run focused Rust tests while iterating, then run formatting, Clippy, and the full
Rust suite before handoff. Add a regression at the lowest layer that proves the
classification and an integration test when multiple lifecycle stages interact.
