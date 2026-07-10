# ADR-026: Separate Extension Runtime Lifecycle from Access Control

**Status**: Accepted — Phase 2 Complete  
**Date**: 2026-05-03  
**Last Updated**: 2026-07-10  
**Author**: Kimi Code CLI  
**Depends On**: ADR-017 (Unified Extension Architecture), ADR-021 (Daemon as Central Runtime), ADR-025 (Gateway Extension Architecture)  
**Replaces / Supersedes**: The overloaded semantics of `peko ext enable` / `peko ext disable` as defined in ADR-017 and ADR-025

> **Subsequent Change (2026-07-10).** The access-control `peko ext enable` and
> `peko ext disable` commands described in this ADR were later removed entirely
> in favor of capability-based authorization (`peko capability grant/revoke
> --principal`). Runtime lifecycle continues to be managed via `peko ext
> start` / `stop` / `restart` / `status`.

---

## Context

### The Problem

The `peko ext enable` and `peko ext disable` commands are currently overloaded. They conflate three distinct concerns into a single verb:

1. **Daemon runtime lifecycle** — start or stop a background process/task (e.g., MCP server, gateway bot).
2. **Extension registry state** — mark an extension as "active" in the `ExtensionManager`.
3. **Agent access permission** — control whether a specific agent or team may use an extension's capabilities (e.g., whitelisting tools in `tools.enabled`).

This overloading was manageable when most extensions were stateless (skills, universal tools) and "enabling" simply flipped a flag. It becomes problematic with the introduction of daemon-scoped background runtimes in ADR-025.

### Why This Matters Now

ADR-025 introduces `BackgroundRuntimeManager` to supervise long-running processes such as gateways and MCP servers. Under the current CLI design:

- `peko ext enable discord-gateway` would start a **daemon-wide** WebSocket bot — a global side-effect disguised as a generic enable operation.
- `peko ext disable mcp-filesystem` would stop an MCP server process, potentially breaking all agents that depend on it, even though the user's intent might have been only to revoke access for one team.
- There is no way to **start a gateway globally** while **restricting which agents can receive messages from it**.
- There is no way to **keep an MCP server running** while **disallowing a specific agent from calling its tools**.

### Current Behavior Matrix

| Extension Type | `ext enable` Does | `ext disable` Does |
|---|---|---|
| **Skill** | Flips `ExtensionManager` enabled flag | Flips flag off |
| **Universal Tool** | Flips flag + adds to agent whitelist | Flips flag off |
| **MCP** | Flips flag + starts MCP server process + adds to whitelist | Stops MCP server process + flips flag off |
| **Gateway** *(ADR-025)* | Flips flag + starts gateway process/task via `BackgroundRuntimeManager` | Stops gateway process + flips flag off |
| **Built-in** | Adds to agent whitelist + enables ExtensionCore hooks | Removes from whitelist + disables hooks |

The matrix reveals that `enable`/`disable` mean different things depending on extension type. This violates the principle of least surprise.

### Existing Architecture That Supports Separation

The codebase already has two separate subsystems that map cleanly to the proposed separation:

- **`BackgroundRuntimeManager`** (ADR-025) — owns spawn, stop, restart, health-check, and crash recovery for all background runtimes. It is daemon-scoped.
- **`ExtensionManager` + `ExtensionCore` + per-agent config** — own registration, hook enablement, and tool whitelisting. They are agent/team-scoped.

These two systems currently have no clean CLI boundary.

---

## Decision

### 1. Introduce Runtime Lifecycle Commands

New CLI commands for **daemon-scoped background runtime management**:

```bash
peko ext start  <extension-id>    # Spawn the background runtime
peko ext stop   <extension-id>    # Graceful shutdown
peko ext restart <extension-id>   # Restart with backoff policy
peko ext status <extension-id>    # Show RuntimeState (Running, Healthy, Crashed, etc.)
```

These commands operate directly on `BackgroundRuntimeManager`. They only apply to extensions that declare a background runtime in their manifest (`extension_type: "mcp"`, `"gateway"`, or any future runtime-bearing type).

Attempting to `start` a stateless extension (e.g., a skill) returns an error:
```
Error: 'my-skill' does not declare a background runtime. Use 'peko ext enable' instead.
```

### 2. Redefine `enable` / `disable` as Pure Access Control

`peko ext enable` and `peko ext disable` are redefined to control **only** access permissions and hook state:

| Scope | What `enable` does | What `disable` does |
|---|---|---|
| **Global** (no `--target`) | Sets `ExtensionManager` enabled flag; enables ExtensionCore hooks | Clears flag; disables hooks |
| **Team** (`--target team`) | Adds extension tools/capabilities to team-level config | Removes from team config |
| **Agent** (`--target team/agent`) | Adds to agent's `tools.enabled` whitelist; enables hooks for that agent | Removes from whitelist; disables hooks |

**Critical:** `enable`/`disable` **never** start or stop background processes.

### 3. Two Independent Dimensions

An extension can be in any combination of these states:

| Runtime | Access (Global) | Example Scenario |
|---|---|---|
| **Running** | **Enabled** | Normal operational state |
| **Running** | **Disabled** | MCP server is up, but no agent is currently permitted to use it (e.g., during a security review) |
| **Stopped** | **Enabled** | Extension is configured for use but the operator has intentionally shut down its runtime (e.g., maintenance window) |
| **Stopped** | **Disabled** | Fully inactive |

### 4. CLI Changes

#### New Commands

```bash
# ─── RUNTIME LIFECYCLE ───
peko ext start discord-gateway
peko ext stop discord-gateway
peko ext restart mcp-filesystem
peko ext status mcp-filesystem
```

#### Modified Commands

```bash
# ─── ACCESS CONTROL ───
peko ext enable my-skill --target myteam/myagent
peko ext disable shell --target myteam
peko ext enable discord-gateway --target myteam  # Which agents can receive from this gateway
```

#### Modified List Output

```bash
peko ext list
# → Shows all installed extensions with two status columns:
#   RUNTIME (running/stopped/n/a) and ACCESS (enabled/disabled per scope)

peko ext list --running
# → Only background runtimes that are currently up

peko ext list --enabled --target myteam/myagent
# → Only extensions this agent is permitted to use
```

#### Daemon Status Integration

`peko daemon status` already shows background runtimes (ADR-025). This remains the primary operational view for runtime health:

```
Background Runtimes:
  mcp:filesystem    Healthy    (pid 12345, 2 restarts)
  gateway:discord   Running    (task id 7, initializing)
```

### 5. Backward Compatibility

The separation is now complete. `enable`/`disable` are pure access control for **all** extension types. Runtime extensions must use `start`/`stop`.

- For **stateless** extensions (skill, general, universal-tool, built-in): `enable`/`disable` control hook state and tool whitelisting — unchanged.
- For **runtime** extensions (mcp, gateway): `enable`/`disable` control hook state and tool whitelisting only. They do **not** start or stop background processes. Use `ext start`/`ext stop` for lifecycle management.

---

## Reasoning

**Single Responsibility Principle.** A CLI command should do one thing. `start` controls processes; `enable` controls permissions. Users can reason about each independently.

**Operational Safety.** An operator can revoke an agent's access to an MCP server (`disable --target team/agent`) without risking other agents that still depend on the same server. Conversely, an operator can restart a crashed gateway (`restart`) without touching any agent's permission configuration.

**Alignment with Internal Architecture.** The CLI now mirrors the existing code boundary between `BackgroundRuntimeManager` (daemon-scoped runtime supervision) and `ExtensionManager`/`ExtensionCore` (agent-scoped registration and permissions).

**Scalable Mental Model.** As new background runtime types are added (persistent universal tools, custom integrations, future streaming adapters), they automatically fit the `start`/`stop`/`restart`/`status` model without special-casing `enable`.

**Team Isolation.** Gateways and MCP servers are inherently daemon-scoped resources. With separated commands, a gateway can be started once globally while access is granted or revoked per-team or per-agent via `enable`/`disable --target`.

---

## Tradeoffs Accepted

**Breaking Change (with mitigation).** The semantics of `peko ext enable` for MCP and gateway extensions will change. Mitigated by a deprecation phase with clear warnings.

**Two Commands for Full MCP Setup.** Instead of one `enable` command, users now need `start` + `enable --target` to make an MCP server fully operational for an agent. This is more explicit and avoids hidden side-effects, but slightly more verbose. The convenience alias in Phase 1 preserves the one-command workflow temporarily.

**New CLI Surface.** Four new commands (`start`, `stop`, `restart`, `status`) add to CLI maintenance. Mitigated by the fact that they delegate directly to `BackgroundRuntimeManager` methods that already exist (ADR-025).

---

## Migration Path

### Phase 1: Add New Commands ✅ Complete

1. Implemented `ExtCommands::Start`, `Stop`, `Restart`, `Status` in `src/commands/ext.rs`.
2. Wired commands to `BackgroundRuntimeManager::start()` / `stop()` / `restart()` / `get_state()` via daemon IPC.
3. Updated `peko ext list` to show both runtime and access status.

### Phase 2: Pure Access Control Semantics ✅ Complete

1. `handle_enable`/`handle_disable` for runtime extensions no longer trigger any background runtime side-effects.
2. `enable`/`disable` are pure access control (hook state + tool whitelist) for all extension types.
3. Deprecation warnings removed — the separation is now the canonical behavior.
4. E2E tests updated to use `start`/`stop` for runtime lifecycle and `enable`/`disable` for access control.

### Phase 3: Documentation Update (Short Term)

1. Update ADR-025 Section 11 ("Single Mental Model") to reference ADR-026 commands.
2. Update `docs/architecture/EXTENSION_SYSTEM.md` and `README.md` CLI examples.

---

## Consequences

### Positive

- **Clear separation of concerns.** Runtime lifecycle and access control are independent operations.
- **Operational safety.** Stopping a runtime and disabling access are no longer accidentally coupled.
- **Consistent CLI semantics.** `enable`/`disable` mean the same thing for every extension type.
- **Team-level isolation.** Daemon-wide runtimes can be managed independently of per-agent permissions.
- **Aligns CLI with architecture.** Commands map directly to `BackgroundRuntimeManager` vs `ExtensionCore` boundaries.
- **Future-proof.** New runtime-bearing extension types require no special CLI design.

### Negative / Risks

| Risk | Mitigation |
|---|---|
| Breaking change for existing MCP users | Deprecation phase with warnings; clear migration docs |
| Slightly more verbose MCP setup (two commands) | Convenience alias during deprecation; shell aliases/scripts for power users |
| Users may forget to `start` after `enable` | `ext list` shows runtime status; `daemon status` shows runtimes; error messages guide user |
| Documentation drift | Update ADR-025, EXTENSION_SYSTEM.md, README.md, and E2E tests in lockstep |

---

## Success Criteria

| Criterion | Status | Notes |
|---|---|---|
| `peko ext start <id>` spawns background runtime via `BackgroundRuntimeManager` | ✅ Done | New command; delegates to daemon IPC |
| `peko ext stop <id>` gracefully stops background runtime | ✅ Done | New command; delegates to daemon IPC |
| `peko ext restart <id>` restarts with backoff | ✅ Done | New command; delegates to daemon IPC |
| `peko ext status <id>` shows `RuntimeState` | ✅ Done | New command; delegates to daemon IPC |
| `peko ext start <id>` works for **MCP extensions** | ✅ Done | `McpRuntimeStarter` registered in `ExtensionRuntimeStarterRegistry`; parses unified manifest and legacy config |
| `peko ext start <id>` works for **Gateway extensions** | ✅ Done | `GatewayRuntimeStarter` registered in `ExtensionRuntimeStarterRegistry` |
| IPC server has **no hardcoded type checks** for extension runtime dispatch | ✅ Done | `handle_ext_start`/`stop`/`restart` delegate to `ExtensionRuntimeStarterRegistry` |
| `peko ext enable` no longer starts processes for any extension type | ✅ Done | `manager.enable()` only toggles hooks |
| `peko ext disable` no longer stops processes for any extension type | ✅ Done | `manager.disable()` only toggles hooks |
| `peko ext list` shows both runtime and access status | ✅ Done | Enhanced output with RUNTIME column |
| Deprecation warnings removed | ✅ Done | Phase 2 — warnings removed, clean semantics |
| All E2E tests updated to use `start`/`stop` for runtime extensions | ✅ Done | MCP E2E tests use `ext start`/`ext stop` |
| Documentation updated across ADR-025, EXTENSION_SYSTEM.md, README.md | ⏸️ Pending | Phase 3 |

---

## Related Documents

- ADR-017: Unified Extension Architecture — defines original `enable`/`disable` semantics
- ADR-021: Daemon as Central Runtime — establishes daemon-scoped services
- ADR-025: Gateway Extension Architecture — introduces `BackgroundRuntimeManager`
- `src/commands/ext.rs` — CLI command definitions and handlers
- `src/daemon/background_runtime/manager.rs` — `BackgroundRuntimeManager` implementation
- `src/extensions/manager.rs` — `ExtensionManager` implementation
- `docs/architecture/EXTENSION_SYSTEM.md` — user-facing extension system documentation
