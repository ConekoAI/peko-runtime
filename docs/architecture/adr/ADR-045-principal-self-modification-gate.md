# ADR-045: Principal Self-Modification Gate

**Status**: Proposed (PRs #1-#6 to land on `feature/principal-self-modification-gate`)
**Date**: 2026-08-02
**Author**: Peko Runtime Team
**Depends On**: ADR-019 (dynamic tool updates), ADR-021 (daemon-as-central-runtime), ADR-034 (runtime authentication), ADR-039 (principal model), ADR-041 (principal-as-container)

## Context

Peko runs agents autonomously. The peko philosophy does not assume a user is
always present alongside agents — most capability grants, extension activations,
and tool invocations happen without explicit user consent in the loop (also why
the runtime has no plan-mode UX).

The escape vector this exposes: **a principal with `tool:Bash` can launch a
subprocess that talks to the daemon IPC, launder a self-modifying operation
through the daemon, and exit its own capability boundary**. Concrete examples:

1. `peko principal grant-permission --self tool:Dangerous` — grants the
   principal a new capability via the daemon's `PrincipalGrantPermission` IPC
   handler. Today the daemon treats this as a request from the *local CLI
   user*, not from the principal. (`peko-rs/core/src/ipc/handlers/principal.rs:750`)
2. `peko extension install foo` — installs an extension that brings its own
   capability surface. Handler is explicitly marked `TODO(phase-c)` with no
   auth. (`peko-rs/core/src/ipc/handlers/extension.rs:194-262`)
3. Direct write to `~/.peko/<principal>/shared/agents/primary.md` — bypasses
   authority entirely (CLI does the file write directly, no daemon roundtrip).
   (`peko-rs/cli/src/commands/principal.rs:471`)
4. Cron-scheduled future self-modification — once a cron job is scheduled, it
   runs unattended and can persist an escape that outlives a future capability
   revocation.
5. Subagent capability inheritance — if a principal edits its own agent
   definitions, it changes what subagents inherit.

The threat model: **the principal can mutate the configuration that defines
its own capability boundary**.

Bash content introspection is not a viable defense. Bash is a turing-complete
scripting language; every approach is defeatable:

| Approach | Bypass |
|---|---|
| Block `peko` on PATH | `python -c "import subprocess; subprocess.run(['/usr/local/bin/peko', ...])"` |
| Regex on bash args | `bash -c 'p""eko ...'`, heredocs, `printf 'p\145ko ...'` |
| Block argv inspection | Script file, `setsid` |
| Block interpreters | `socat -U UNIX-CONNECT:/path/to/daemon.sock` |
| Sandbox bash itself | Most sandboxes have known escapes given enough effort |

## Problem Statement

We need a structural defense against self-modification that does not rely on
inspecting what the principal is doing, and does not require user presence for
the common case (since peko philosophy is autonomous-by-default).

The defense must:

1. **Close the bash-launched escape.** A principal that spawns a subprocess
   that talks to the daemon IPC must not be able to launder a self-modifying
   operation, regardless of how the subprocess is constructed.
2. **Support unattended operation.** Most capability grants, extension
   activations, and tool invocations happen without the user present. The
   gate cannot require synchronous user consent for the common case.
3. **Preserve the user's escape hatch.** When the user explicitly wants to
   grant a capability, install an extension, or edit a config, they must be
   able to do so from their terminal — and they must be able to *also*
   approve an agent's request for the same, asynchronously.
4. **Not require runtime-side bookkeeping that can be defeated by a clever
   principal.** The defense should rely on something the kernel provides, not
   something the runtime maintains in user-space.

## Decision

We split self-modification into two clean primitives with two different
mechanisms, and adopt a three-layer defense stack.

### The split

**1. Tool-call inline gating** (deferred to later, NOT this ADR).

For bash content patterns like `rm -rf`, network commands, etc. — the
codex-style ToolOrchestrator pattern: block on a oneshot, resume on approval.
Peko already has F31x `PreToolUse` hooks (`peko-rs/extension-api/src/hook_points.rs:67-78`)
but they're observe-only today. Promoting them to gate-able is a future ADR.

**2. Meta-action self-modification** (this ADR).

For principal-initiated changes to its own capability boundary:
- Granting capabilities to itself
- Installing extensions
- Editing its own `agents/` configuration
- Editing its own cron schedule

These are out-of-band operations that may bypass the tool funnel entirely.
They use an async-inbox flow, but critically: **the runtime is in-process
with the daemon**, so the agent's request path is an in-process function call,
not an IPC packet.

### Three-layer defense

#### Layer 1 — Session-group IPC auth (PR #1)

The daemon's IPC accept path gains a session-group check. Every Unix process
belongs to exactly one session group (`getsid(pid)`); session ID is set at
`fork()` time by the kernel and cannot be changed by a non-privileged
process. The kernel exposes the connecting PID's SID via `SO_PEERCRED` on
Linux and `LOCAL_PEERPID` on macOS.

```
IPC connection arrives
  ├─ peer SID = getsid(SO_PEERCRED_pid)
  ├─ if peer SID in AuthTable:
  │     allow with full caps (bypasses credential check)
  ├─ else if credential == None + local trust:
  │     REJECT (changed from previous "allow")
  ├─ else if credential == Jwt + PekoHub JWT enabled:
  │     allow (existing path, unchanged)
  ├─ else if credential == ApiKey + API key enabled:
  │     allow (existing path, unchanged)
  └─ else: reject
```

Empty `AuthTable` means everything is denied. Production builds cannot disable
this; only test/dev workflows can via `--allow-anon`.

The session-group check is strictly stronger than runtime-side spawn-tree
attribution:

- Set at fork time by the kernel; can't be spoofed from inside bash
- No runtime bookkeeping needed (lower implementation cost, smaller blast radius)
- Maps to standard Unix process group semantics — `tmux`/`screen` "just work"
  because their panes share a session ID

A principal spawned by the runtime (via the `Bash` tool) inherits the
runtime's session ID, which is distinct from any user terminal's session ID.
Even if the principal runs `peko principal grant-permission --self`, the
daemon sees the runtime's SID, finds no AuthTable entry, and denies the
request. The principal cannot discover a code, forge a session, or otherwise
bypass the check.

#### Layer 2 — Interactive session auth (PR #2)

`peko daemon start` generates a 6-word diceware code at startup and writes it
to a `CodeSink`:

1. stdout (foreground mode)
2. `~/.peko/code` file (mode 0600) — fallback for daemonized mode
3. Desktop notification (if registered) — UI surface in peko-desktop

The user runs `peko principal list` (or any other CLI command). The daemon
responds with `Unauthorized { reason: "session not authenticated, code at ~/.peko/code" }`.
The CLI prompts the user for the code, sends `AuthSubmit { code_hash }` to the
daemon. The daemon verifies, adds an `AuthEntry { sid: user_sid, caps: all,
token_hash, expires_at: now+8h }` to `AuthTable`, and returns a 256-bit
random session token.

Subsequent CLI invocations in the same session group include the token in
the IPC handshake. The daemon looks up by SID, verifies the token in constant
time, and allows the request.

Tokens are in-memory on both sides — not persisted to disk. Daemon restart
or CLI exit invalidates. TTL (default 8h, configurable) is the source of
truth on the daemon side.

#### Layer 3 — `peko_self` tool + inbox flow (PRs #3 + #4)

The runtime exposes a `peko_self` tool that calls the in-process
`Arc<dyn DaemonApi>::request_self_modify(op, ctx)` directly — no IPC, no
serialization, no protocol surface.

```
Agent's loop → peko_self tool → DaemonApi::request_self_modify
                                          │
                                          ├─ validate op scope
                                          ├─ persist to ~/.peko/runtime/pending-requests/<uuid>.json
                                          ├─ broadcast ApprovalRequested event to user IPC streams
                                          └─ return request_id to caller

(tool returns { status: "pending", request_id } to agent)

... user decides asynchronously ...

user IPC ApprovalDecision { request_id, decision }
  └─► ApprovalEngine::execute(op) with daemon authority
        └─► AsyncInboxItem::Approval { request_id, result } to principal's session inbox
              └─► runtime inbox-drain picks up next iteration
```

The runtime never holds a token that lets it execute. It can only request.
The daemon executes, with its own authority (`Subject::Daemon` = full caps).

#### Service tokens (PR #5)

Long-lived processes (runtime startup, cron daemon, persistent agents)
authenticate via service tokens instead of interactive auth. Service tokens
are 256-bit random, stored at `~/.peko/service.tokens/<name>` (mode 0600),
capability-scoped at creation (cannot grow), revocable.

The runtime itself needs a service token because it starts before any user
is logged in. The token is generated by the user with
`peko service-token create --name runtime --caps ...` (which itself requires
an interactive session token).

#### Observability (PR #6)

Every auth attempt, every self-modify request, every decision is captured in:

- Counters (in `peko-observability`): `auth.success`, `auth.failure`,
  `ipc.unauthorized`, `approval.requested`, `approval.decided`, etc.
- Audit log: `~/.peko/audit.jsonl` (append-only, mode 0600), every request
  and decision recorded with timestamp + principal + op + decision.

### Meta-capability immutability

Capabilities starting with `principal:*` or `runtime:*` are categorically
not self-grantable by any principal under any circumstance. They require
`Subject::User` (terminal owner) invocation. This is a hard rule at the
authority layer, not a per-capability flag.

Tool capabilities (`tool:*`) are user-grantable but not self-grantable.
The principal cannot grow its own toolset without user consent.

### Architectural diagram

```
┌──────────────────────────────────────────────────────────────────────┐
│                          daemon process                              │
│                                                                      │
│   ┌─────────────┐    ┌────────────────────┐                           │
│   │  AgenticLoop│───►│ ToolExecutor       │                           │
│   │             │    │  ├─ Bash tool      │──► spawn()                │
│   │             │    │  ├─ Read/Write     │                           │
│   │             │    │  └─ peko_self tool │                           │
│   │             │    │       │            │                           │
│   │             │    │       ▼            │                           │
│   │             │    │  Arc<dyn DaemonApi>│                           │
│   │             │    │       │            │                           │
│   │             │    │       ├─ persist   │                           │
│   │             │    │       ├─ emit event│                           │
│   │             │    │       └─ return id │                           │
│   │             │    └────────┬───────────┘                           │
│   │             │             │                                       │
│   │   inbox ◄───┼─────────────┘  (AsyncInboxItem::Approval)          │
│   └─────────────┘                                                      │
│                                                                      │
│       ▲                       ▲                                       │
│       │ user IPC events       │ user IPC commands                     │
│       │ (push)                │ (pull)                                │
└───────┼───────────────────────┼───────────────────────────────────────┘
        │                       │
   ┌────┴────────┐         ┌────┴──────┐
   │  peko CLI   │         │  desktop  │
   │  (TTY)      │         │  (UI)     │
   └─────────────┘         └───────────┘

AuthTable:
  ┌────────────────────────────────────────────────┐
  │ sid_tty1 ── token_hash_a, caps=[*], exp=t+8h   │
  │ sid_runtime ── token_hash_b, caps=[...], exp=t+24h │
  └────────────────────────────────────────────────┘

Session-group check at IPC accept:
  peer_sid = getsid(SO_PEERCRED_pid)
  if peer_sid ∈ AuthTable:
    allow
  else:
    deny (unless credential is Jwt/ApiKey, unchanged)
```

## Implementation Plan

The full implementation lands on a single branch
(`feature/principal-self-modification-gate`) as 6 commits, one per PR.

### PR #1 — Session-group IPC auth
- `peko-rs/core/src/ipc/auth.rs` (new) — `AuthTable`, `AuthEntry`, `SessionGroup`
- `peko-rs/core/src/ipc/connection.rs:304` — add session-group lookup
- `peko-rs/core/src/daemon/state.rs` — add `auth_table` field
- `peko-rs/core/src/ipc/errors.rs` — `IpcError::Unauthorized`
- `peko-rs/protocol/src/ipc.rs` — `AuthSubmit`, `AuthRequired` packets
- `peko-rs/core/src/ipc/handlers/auth.rs` — handler stub (full impl in #2)
- `--allow-anon` dev flag for tests

### PR #2 — Interactive session auth
- `peko-rs/peko-daemon/src/startup.rs` (new) — diceware code generation
- `peko-rs/core/src/code_sink.rs` (new) — `CodeSink` trait
- `peko-rs/cli/src/commands/auth.rs` (new) — `peko auth` command
- `peko-rs/core/src/ipc/handlers/auth.rs` — full AuthSubmit implementation
- CLI session token in-process storage

### PR #3 — `peko_self` tool + event
- `peko-rs/core/src/daemon/api.rs` (new) — `DaemonApi` trait
- `peko-rs/core/src/daemon/approval_queue.rs` (new) — `ApprovalQueue`
- `peko-rs/core/src/tools/builtin/peko_self.rs` (new) — tool implementation
- `peko-rs/core/src/tools/mod.rs` — register
- `peko-rs/core/src/runtime_paths.rs` (new) — pending-requests dir
- `peko-rs/core/src/ipc/packet.rs` — `ResponsePacket::ApprovalRequested`

### PR #4 — Decision + execution + result delivery
- `peko-rs/core/src/ipc/packet.rs` — `RequestPacket::ApprovalDecision`
- `peko-rs/core/src/ipc/handlers/approval.rs` (new)
- `peko-rs/core/src/daemon/approval_engine.rs` (new)
- `peko-rs/extension-api/src/async_inbox.rs:34-39` — `AsyncInboxItem::Approval`
- `peko-rs/session/src/inbox_registry.rs` — handle new variant
- `peko-rs/cli/src/commands/inbox.rs` (new) — `peko inbox` CLI

### PR #5 — Service tokens
- `peko-rs/cli/src/commands/service_token.rs` (new)
- `peko-rs/core/src/ipc/auth.rs` — extend with `ServiceToken`
- `peko-rs/core/src/storage/service_token_store.rs` (new)
- `peko-rs/core/src/runtime_paths.rs` — `service.tokens/` layout

### PR #6 — Observability + audit
- `crates/observability/src/counters.rs` — new counters
- `peko-rs/cli/src/commands/audit.rs` (new) — `peko audit log`
- Counter increments threaded through PRs #1-#5 (finalize here)

### PR #7 (separate repo, `peko-desktop`)
- Notification on `ApprovalRequested`
- Inbox UI for accept/deny/session
- Daemon-startup code display

## Consequences

### Positive

- **Structural defense.** The bash-launched escape vector is closed. There is
  no longer any path by which a principal can mutate its own capability
  boundary through the daemon, regardless of how the principal constructs the
  subprocess. The session-group check is set at fork time by the kernel and
  cannot be spoofed.
- **Unattended operation preserved.** The common case (auto-approved) is
  unchanged. Most capability grants, extension activations, and tool calls
  work without the user present. The new gate is invoked only when the
  principal attempts a meta-action.
- **Clean inbox escape hatch.** When the principal does want a capability,
  it can ask via `peko_self`, and the user gets a notification. The user
  can approve or deny from their terminal or desktop.
- **Defense in depth.** Spawn-tree attribution (previously considered) is
  still useful for audit metadata even though it's no longer the primary
  defense. The session-group check + capability tier check + audit log give
  three independent layers.
- **No protocol surface for the request path.** The agent-to-daemon
  self-modify path is in-process function calls. No IPC packet types, no
  protocol versioning burden, no protocol-level attacks.

### Negative

- **Auth friction.** The user must enter a code on first CLI invocation per
  daemon session. Mitigations: 8h TTL (rare re-prompt), session-group caching
  (all processes in the same terminal session work), service tokens for
  long-lived processes.
- **Recovery complexity.** Forgot the code? Must restart daemon (in-memory
  code dies) or `peko reset-auth` (destructive). Documented; uncommon.
- **Code-display in daemonized mode.** Where does the diceware code go?
  Default: `~/.peko/code` file (0600) + warning in daemon log. UX worse
  than in-foreground mode. Acceptable for v1; revisit if it's a UX disaster.
- **Multi-container / multi-UID boxes.** Session IDs are per-kernel-namespace.
  Inside a container, all processes share one SID; user SSHed in has a
  different SID from the host session. Single-machine peko is fine;
  distributed setups need separate consideration.
- **Two-channel mental model.** Code paths must distinguish "user-driven via
  IPC" from "agent-driven in-process". The peko_self tool is the only
  in-process path; everything else is IPC. Documented; tested.

### Neutral

- **Code is not persisted.** Daemon restart requires re-auth. This is a
  feature (no on-disk secret) and a UX cost (re-prompt). Net positive given
  the threat model.
- **Service tokens are a new primitive.** Users must learn `peko service-token`
  for runtime/cron. Acceptable; they're the right shape for long-lived auth.

## Alternatives Considered

### Spawn-tree attribution (REJECTED for primary defense)

Runtime-side bookkeeping of `principal → process` relationships, walked up at
IPC accept time. Rejected because:

- Requires runtime bookkeeping (more code, more bugs)
- Set at fork time but mutable via `setsid`/`nohup`/`disown` (need extra
  defenses)
- Doesn't map to standard Unix process group semantics (`tmux`/`screen`
  would need custom logic)

Still useful as a secondary signal for audit logging, but not the primary
defense. Session-group check is strictly stronger.

### Two-channel IPC (REJECTED)

Have the daemon expose a separate IPC channel for "request-only" ops that
any process can submit to. Rejected because:

- It's a protocol surface to attack
- It's unnecessary — the runtime is in-process with the daemon, so the
  request path can be a direct function call
- Adds complexity (two protocol parsers, two auth paths) for no benefit

The in-process DaemonApi path is strictly cleaner.

### Content-based bash gating (REJECTED)

Pattern-match bash commands against a list of dangerous patterns (`rm`,
`sudo`, etc.). Rejected because:

- Bash is turing-complete; every matcher is defeatable
- High maintenance burden (false positives, false negatives)
- Doesn't address the underlying identity problem

The session-group check makes content gating unnecessary.

### Always-require-user-approval (REJECTED)

Force every meta-action through the inbox, even when the user is at the
terminal. Rejected because:

- Violates the peko philosophy (autonomous agents by default)
- Defeats the entire point of having CLI commands the user can run directly
- The user can run `peko principal grant-permission` from their terminal;
  that IS the user-driven path. The inbox is only for the agent-driven path.

### Cron-aware bypass (REJECTED)

Allow cron jobs to perform self-modifying ops because they're "trusted".
Rejected because:

- A principal that schedules a cron job is itself scheduling a future escape
- The threat model includes "principal writes a script, schedules it via
  cron, exits, cron fires later"
- Better: cron jobs operate within a fixed capability snapshot taken at
  schedule time. Self-modifying ops are refused at schedule time.

## References

- ADR-019 — Dynamic tool and prompt updates (F31x hooks)
- ADR-021 — Daemon as central runtime
- ADR-034 — Runtime authentication and authorization (existing AuthCredential)
- ADR-039 — Principal model
- ADR-041 — Principal as container
- `peko-rs/core/src/ipc/server.rs:752` — existing `resolve_caller` (to be extended)
- `peko-rs/core/src/ipc/handlers/auth.rs` — existing auth handler (to be extended)
- Codex `Approvable` trait and `ToolOrchestrator` — reference architecture for
  future inline gating work (NOT this ADR)