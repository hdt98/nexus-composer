---
name: nexus-remote-agent
description: Implement, diagnose, or review Nexus Composer SSH connections and the Remote Agent lifecycle. Use for saved SSH targets, remote inspection, agent deploy or upgrade, proxy and control services, remote provider switching, profile backup and restore, history maintenance, status polling, automatic reconnect, cancellation, crash or reboot recovery, and remote security boundaries.
---

# Nexus Remote Agent

## Overview

The Remote Agent is a Node.js service deployed and controlled through system
SSH. Remote Codex sends Responses requests to the loopback proxy on port 18080;
Nexus Composer reaches the loopback control service on port 18090 by executing
commands through SSH. Neither listener may be exposed publicly.

## Locate the owned behavior

- UI and saved connections: `src/components/remote/`
- SSH target resolution and remote profile operations:
  `src-tauri/src/services/provider/remote.rs`
- Deploy, start, stop, status, and reconnect orchestration:
  `src-tauri/src/services/provider/remote_agent.rs`
- Deployed agent source: `src-tauri/src/resources/nexus_remote_agent.js`
- Remote history maintenance:
  `src-tauri/src/resources/nexus_remote_history_maintainer.py`
- End-to-end regression coverage:
  `tests/e2e/remoteAgentProfileBackup.test.ts`

## Preserve trust boundaries

- Use the system SSH client and explicit host, user, and port values.
- Persist non-secret connection metadata only after a successful connection.
- Never persist SSH passwords or embed credentials in commands, logs, or agent
  assets.
- Bind proxy and control listeners to `127.0.0.1` only.
- Quote remote paths and shell data deliberately; do not concatenate untrusted
  values into commands.
- Redact remote auth files and private configuration in diagnostics.

## Treat lifecycle operations as transactions

For deploy, start, switch, stop, and restore:

1. Resolve one canonical host identity and serialize operations for that host.
2. Inspect Node availability, deployed version, PID state, ports, agent status,
   and the current remote client profile.
3. Detect stale PID files, incompatible assets, partial deployment, and port
   ownership before mutating state.
4. Back up every remote file that may be changed and record whether it existed.
5. Deploy to temporary paths, validate, and atomically promote assets.
6. Start or switch idempotently, then verify control health and effective model
   state before reporting success.
7. On failure or cancellation, restore the last verified client profile and
   leave diagnostics that do not contain secrets.
8. On stop or Official switch, verify restoration before removing active backup
   markers.

## Model status truthfully

Keep UI states explicit: disconnected, connecting, deploying, starting,
running, degraded, reconnecting, restoring, stopped, and failed. Do not infer
`running` from a saved connection, PID file, or successful SSH command alone.
Use bounded polling with timeouts and backoff; stop polling when the page or
operation no longer owns the request.

## Test recovery paths

Cover clean and failure cases for:

- First deploy, upgrade, repeated deploy, start, switch, stop, and restore.
- SSH disconnect during upload, write, switch, and verification.
- Stale PID, process crash, reboot, port collision, and incompatible Node.
- Existing remote profile, missing files, failed rollback, and history repair.
- Concurrent UI actions, cancellation, timeout, and repeated reconnect.
- Long-running chat, tool calls, Official-to-custom switching, and custom-to-
  Official restoration.

Run the focused TypeScript end-to-end test and relevant Rust tests. Use a
disposable SSH target for destructive failure injection and protect unrelated
remote processes and profiles.
