# Issue 014: Extension System Architecture Scattered Across Modules

**Severity:** HIGH  
**Status:** 🟢 **Resolved** (Phases 1–7 completed, 2026-05-05)  
**Labels:** `architecture`, `extensions`, `adr-017`, `refactor`, `module-boundaries`  
**Reported:** 2026-05-05  
**Related:** ADR-017 (Unified Extension Architecture), Issue 001, Issue 002, Issue 006

---

## Resolution Summary

This issue has been resolved through a phased migration:

- **Phase 2** ✅ — Universal Tool Protocol and shared utilities moved from `src/tools/framework/` to `src/extensions/protocols/`.
- **Phase 3** ✅ — Async Executor moved from `src/tools/framework/async_executor/` to `src/extensions/async_exec/executor/`.
- **Phase 4** ✅ — `BuiltinToolRegistrar` merged into `BuiltinToolAdapter` in `src/extensions/adapters/builtin_tool_adapter.rs`; `src/tools/registry/builtin.rs` deleted.
- **Phase 5** ✅ — MCP runtime adapter and tool proxies moved from `src/mcp/` to `src/extensions/runtime/`. `src/mcp/` now contains only protocol client, transport, and types.
- **Phase 6** ✅ — Dependency audit completed. `src/extensions/core/` has zero imports from `crate::mcp`, `crate::daemon`, or `crate::tools`.
- **Phase 7** ✅ — Gateway runtime concerns consolidated into `src/extensions/`:
  - `gateway_adapter.rs` → `src/extensions/runtime/gateway_runtime_adapter.rs`
  - `gateway_starter.rs` → `src/extensions/runtime/gateway_starter.rs`
  - `router.rs` → `src/extensions/runtime/gateway_router.rs`
  - `protocol.rs` → `src/extensions/protocols/gateway/protocol.rs`
  - `mcp_starter.rs` → `src/extensions/runtime/mcp_starter.rs` (symmetry with gateway)

### Remaining Work (Deferred)
- **CI lint** for preventing `extensions/core/` from importing `mcp`/`daemon`/`tools` is not yet implemented.

---

## Summary

The Unified Extension Architecture (ADR-017) is functionally working but **architecturally scattered**. Critical extension-related concerns have leaked into at least 5 top-level modules outside `src/extensions/`, creating blurry boundaries, bidirectional dependencies, and cognitive overload. To understand how a single extension type (e.g., MCP or Gateway) works end-to-end, a developer must read files across `src/extensions/`, `src/mcp/`, `src/daemon/background_runtime/`, `src/tools/`, and `src/agent/`.

This issue documents the current state and proposes a phased migration to consolidate extension concerns under `src/extensions/` with clean dependency directions.

---

## The Intended Architecture (ADR-017)

```
src/extensions/
├── core/          ← Hook points, registry, handler traits — NO external deps
├── adapters/      ← Map extension formats (skill, MCP, universal-tool, gateway) to hooks
├── manager/       ← Lifecycle: install, enable, disable, discover, bundle
├── types/         ← ExtensionManifest, HookPoint, HookResult, etc.
├── services/      ← Cross-cutting: param injection, tool execution, validation
└── transport/     ← Async task transport (local, daemon IPC)
```

**Design principle:** `src/extensions/` should be the **center of gravity** for all extension concerns. Other modules (`mcp`, `daemon`, `tools`) should depend *on* extensions, not the reverse.

---

## The Actual Architecture (Before Migration)

```
src/extensions/              ← Core + Adapters + Manager (good)
src/mcp/                     ← MCP protocol + runtime adapter + tool proxies (leaked)
src/daemon/background_runtime/  ← Process supervisor + MCP starter + Gateway starter (leaked)
src/tools/framework/universal/  ← Universal tool protocol (extension concern, under tools/)
src/tools/framework/async_executor/  ← Async task execution (cross-cutting, under tools/)
src/tools/registry/builtin.rs   ← Built-in tool registrar (duplicates adapter logic)
src/agent.rs, src/agentic_loop.rs  ← Agent init wires extensions, skills, built-ins
```

## The Actual Architecture (After Migration)

```
src/extensions/              ← Core + Adapters + Manager + Protocols + AsyncExec + Runtime (consolidated)
│   ├── protocols/
│   │   ├── gateway/         ← Gateway IPC protocol (was src/daemon/background_runtime/protocol.rs)
│   │   ├── universal/       ← Was src/tools/framework/universal/
│   │   └── shared/          ← Was src/tools/framework/shared/
│   ├── async_exec/executor/ ← Was src/tools/framework/async_executor/
│   └── runtime/             ← MCP + Gateway runtime adapters, starters, router (was src/mcp/ + src/daemon/background_runtime/)
src/mcp/                     ← MCP protocol client, transport, types only
src/daemon/background_runtime/  ← Generic process supervision ONLY (all extension-specific code moved out)
src/tools/                   ← Built-in tools, core traits, factory only (framework deleted)
```

---

## Detailed Scattering Analysis

### 1. MCP Concerns Split Across 3 Modules

| Concern | Location | Assessment |
|---------|----------|------------|
| MCP protocol client | `src/mcp/client.rs` | ✅ Correctly isolated |
| MCP transport | `src/mcp/transport.rs` | ✅ Correctly isolated |
| MCP manager | `src/mcp/manager.rs` | ⚠️ Manages lifecycle but delegates to `daemon::background_runtime` |
| MCP runtime adapter | `src/mcp/runtime_adapter.rs` → `src/extensions/runtime/mcp_runtime_adapter.rs` | ✅ Moved to extension runtime layer |
| MCP tool proxies | `src/mcp/tool_proxy.rs`, `injectable_proxy.rs` → `src/extensions/runtime/` | ✅ Moved to extension runtime layer |
| MCP extension adapter | `src/extensions/adapters/mcp_adapter.rs` | ✅ Correctly placed |
| MCP runtime starter | `src/daemon/background_runtime/mcp_starter.rs` → `src/extensions/runtime/mcp_starter.rs` | ✅ Moved to extension runtime layer |

**To understand MCP end-to-end, you must read:** `src/mcp/`, `src/extensions/adapters/mcp_adapter.rs`, and `src/extensions/runtime/`.

### 2. Gateway Concerns Split Across 3 Modules

| Concern | Location | Assessment |
|---------|----------|------------|
| Gateway extension adapter | `src/extensions/adapters/gateway_adapter.rs` | ✅ Correctly placed |
| Gateway runtime starter | `src/daemon/background_runtime/gateway_starter.rs` → `src/extensions/runtime/gateway_starter.rs` | ✅ Moved to extension runtime layer |
| Gateway runtime adapter | `src/daemon/background_runtime/gateway_adapter.rs` → `src/extensions/runtime/gateway_runtime_adapter.rs` | ✅ Moved to extension runtime layer |
| Gateway router | `src/daemon/background_runtime/router.rs` → `src/extensions/runtime/gateway_router.rs` | ✅ Moved to extension runtime layer |
| Gateway protocol | `src/daemon/background_runtime/protocol.rs` → `src/extensions/protocols/gateway/protocol.rs` | ✅ Moved to extension protocols |

### 3. Universal Tool Protocol in Wrong Module

| Concern | Location | Assessment |
|---------|----------|------------|
| Universal tool adapter | `src/extensions/adapters/universal_tool_adapter.rs` | ✅ Correctly placed |
| Universal tool protocol | `src/tools/framework/universal/` → `src/extensions/protocols/universal/` | ✅ Moved to extension protocols |
| Universal tool manifest | `src/tools/framework/universal/manifest.rs` → `src/extensions/protocols/universal/manifest.rs` | ✅ Moved to extension protocols |

### 4. Async Executor in Wrong Module

| Concern | Location | Assessment |
|---------|----------|------------|
| Async executor | `src/tools/framework/async_executor/` → `src/extensions/async_exec/executor/` | ✅ Moved to extension async execution |
| Async transport/router | `src/extensions/transport/` | ✅ Correctly placed |
| Async bridge | `src/extensions/core/async_bridge.rs` | ✅ Correctly placed |

**The async executor is the engine for background task execution across ALL extension types, but it lives under `tools/framework/`.**

### 5. Built-in Tool Registration Duplicated

| Concern | Location | Assessment |
|---------|----------|------------|
| Built-in tool adapter | `src/extensions/adapters/builtin_tool_adapter.rs` | ✅ Correctly placed; now also hosts `BuiltinToolRegistrarConfig` and `register_all()` |
| Built-in tool registrar | `src/tools/registry/builtin.rs` | ✅ Merged into `BuiltinToolAdapter`; file deleted |
| Tool factory | `src/tools/registry/factory.rs` | ⚠️ Remains; `ExtensionManager` is the canonical factory for extensions, but `ToolFactory` still creates built-in tool instances synchronously |

### 6. Background Runtime Colonized by Extension-Specific Starters

The `src/daemon/background_runtime/` module is a **generic process supervisor** that accumulated extension-specific knowledge:

- ~~`mcp_starter.rs` — knows `McpServerConfig`, `TransportType`, parses `mcp_servers` from YAML~~
- ~~`gateway_starter.rs` — knows gateway manifest format, `GatewayRoutingConfig`~~
- ~~`gateway_adapter.rs` — `BackgroundRuntimeAdapter` implementation for gateways~~
- ~~`router.rs` — routes gateway messages to agents~~
- ~~`protocol.rs` — gateway IPC protocol types and codec~~
- ~~`runtime_adapter.rs` (in `src/mcp/`) — bridges MCP to the daemon runtime~~

**Post-migration status:**
- All extension-specific runtime code has been moved out of `src/daemon/background_runtime/`.
- `mcp_starter.rs` → `src/extensions/runtime/mcp_starter.rs`
- `gateway_starter.rs` → `src/extensions/runtime/gateway_starter.rs`
- `gateway_adapter.rs` → `src/extensions/runtime/gateway_runtime_adapter.rs`
- `router.rs` → `src/extensions/runtime/gateway_router.rs`
- `protocol.rs` → `src/extensions/protocols/gateway/protocol.rs`
- `runtime_adapter.rs`, `tool_proxy.rs`, and `injectable_proxy.rs` → `src/extensions/runtime/` (done in Phase 5).
- `src/daemon/background_runtime/` now contains **only** generic process supervision infrastructure: `adapter.rs`, `supervisor.rs`, `manager.rs`, `starter.rs`, `starter_registry.rs`.

---

## Dependency Direction Analysis

### Before Migration (Messy)

```
src/extensions/  ←── uses ── src/mcp/ (McpManager, McpClient)
src/extensions/  ←── uses ── src/tools/ (Tool trait, UniversalToolAdapter)
src/daemon/      ←── uses ── src/mcp/ (McpRuntimeAdapter)
src/daemon/      ←── uses ── src/extensions/ (ExtensionCore)
src/mcp/         ←── uses ── src/daemon/ (BackgroundRuntimeAdapter, ManagedRuntime)
src/tools/       ←── uses ── src/extensions/ (HookContext, HookHandler)
src/agent/       ←── uses ── src/extensions/ (ExtensionCore, BuiltinToolAdapter)
```

There were **bidirectional dependencies** between `src/mcp/` ↔ `src/daemon/` and `src/extensions/` ↔ `src/tools/`.

### After Migration (Cleaned)

```
src/extensions/core/        ←── lowest layer, no external deps
src/extensions/protocols/   ←── depends on core (universal tool protocol)
src/extensions/async_exec/  ←── depends on core + protocols/shared
src/extensions/runtime/     ←── depends on core + mcp (protocol types) + daemon (runtime traits)
src/extensions/adapters/    ←── depends on core + protocols + runtime
src/daemon/                 ←── depends on extensions (uses runtime adapters, ExtensionCore)
src/mcp/                    ←── depends on extensions (adapter implements traits, uses runtime types)
src/tools/                  ←── depends on extensions (tool adapter implements traits)
src/agent/                  ←── depends on extensions (ExtensionCore, BuiltinToolAdapter)
```

The `src/extensions/` ↔ `src/tools/` bidirectional dependency is resolved. The `src/mcp/` ↔ `src/daemon/` dependency is simplified: `mcp` no longer depends on `daemon` (the runtime adapter moved to `extensions/runtime/`). `daemon` still depends on `mcp` protocol types via the starters, which is acceptable. The `src/daemon/background_runtime/` → `src/agent/` dependency (via `GatewayRouter` using `StatelessAgentService`) is also resolved: `GatewayRouter` now lives in `src/extensions/runtime/`.

### Target (Clean)

```
src/extensions/core/        ←── lowest layer, no external deps
src/extensions/adapters/    ←── depends on core + protocol submodules
src/extensions/protocols/   ←── extension protocols (mcp, universal, gateway)
src/extensions/runtime/     ←── runtime starters for daemon integration
src/daemon/                 ←── depends on extensions (uses starters/adapters)
src/mcp/                    ←── depends on extensions (adapter implements traits)
src/tools/                  ←── depends on extensions (tool adapter implements traits)
```

---

## Root Causes

1. **Feature-driven development without architectural guardrails** — ADRs were written after code was scattered.
2. **The "Unified Extension Architecture" is only unified at the hook level** — lifecycle, transport, and protocol concerns were not unified.
3. **`src/tools/` predates `src/extensions/`** — the tool system was built first, then retrofitted with extension adapters.
4. **Daemon-specific runtime management grew organically** — background runtime started generic but accumulated extension-specific knowledge.
5. **No clear boundary between "extension protocol" and "extension runtime"** — MCP protocol vs. MCP process supervision are mixed.

---

## Migration Plan

### Phase 1: Document & Prepare (Low Risk)

**Goal:** Establish baseline understanding and safety nets before moving files.

| Task | Files | Effort |
|------|-------|--------|
| Update `AGENTS.md` with actual dependency graph | `AGENTS.md` | Low |
| Add module-boundary comments to key files | `src/extensions/mod.rs`, `src/mcp/mod.rs`, `src/daemon/mod.rs` | Low |
| Ensure `cargo test` passes as baseline | All | Low |

**Acceptance Criteria:**
- [x] `AGENTS.md` contains a "Module Boundaries" section documenting the intended vs. actual architecture.
- [x] `cargo test` passes with zero failures.

---

### Phase 2: Move Universal Tool Protocol (Low Risk)

**Goal:** Move extension protocol code out of `tools/`.

| From | To | Rationale |
|------|-----|-----------|
| `src/tools/framework/universal/` | `src/extensions/protocols/universal/` | Extension protocol, not tool implementation |
| `src/tools/framework/shared/` | `src/extensions/protocols/shared/` | Shared utilities for extension protocols |

**Steps:**
1. Create `src/extensions/protocols/` directory.
2. Move `src/tools/framework/universal/` → `src/extensions/protocols/universal/`.
3. Move `src/tools/framework/shared/` → `src/extensions/protocols/shared/`.
4. Update all `use` statements across the codebase.
5. Update `src/tools/mod.rs` and `src/extensions/mod.rs` re-exports.

**Acceptance Criteria:**
- [x] `cargo check` passes.
- [x] `cargo test` passes.
- [x] No file under `src/tools/framework/` contains extension-protocol-specific logic.

---

### Phase 3: Move Async Executor (Medium Risk)

**Goal:** Move cross-cutting async infrastructure to `extensions/`.

| From | To | Rationale |
|------|-----|-----------|
| `src/tools/framework/async_executor/` | `src/extensions/async/executor/` | Async execution is an extension concern, not tool-specific |

**Steps:**
1. Create `src/extensions/async/` directory.
2. Move `src/tools/framework/async_executor/` → `src/extensions/async/executor/`.
3. Update all `use` statements (wide blast radius — `agent/`, `daemon/`, `extensions/`, `tools/`, `mcp/`).
4. Update `src/tools/mod.rs` re-exports.

**Acceptance Criteria:**
- [x] `cargo check` passes.
- [x] `cargo test` passes.
- [x] `src/tools/framework/` is deleted.

---

### Phase 4: Consolidate Built-in Tool Registration (Low Risk)

**Goal:** Eliminate duplication between `BuiltinToolAdapter` and `BuiltinToolRegistrar`.

| Action | Target | Rationale |
|--------|--------|-----------|
| Merge `src/tools/registry/builtin.rs` into `src/extensions/adapters/builtin_tool_adapter.rs` | `builtin_tool_adapter.rs` | Single source of truth for built-in tool registration |
| Deprecate `src/tools/registry/factory.rs` | `factory.rs` | `ExtensionManager` should be the canonical factory |

**Steps:**
1. Compare `BuiltinToolRegistrar::register()` with `BuiltinToolAdapter::register_tool()`.
2. Move any logic in `BuiltinToolRegistrar` not present in `BuiltinToolAdapter` into the adapter.
3. Update all callers to use `BuiltinToolAdapter`.
4. Delete `src/tools/registry/builtin.rs`.

**Acceptance Criteria:**
- [x] `BuiltinToolRegistrar` no longer exists.
- [x] All built-in tool registration flows through `BuiltinToolAdapter`.
- [x] `cargo test` passes.

---

### Phase 5: Extract Extension-Specific Runtime Starters (Medium Risk)

**Goal:** Move extension-specific runtime starters out of the generic daemon runtime.

| From | To | Rationale |
|------|-----|-----------|
| `src/daemon/background_runtime/mcp_starter.rs` | `src/extensions/runtime/mcp_starter.rs` | MCP-specific lifecycle logic belongs with MCP adapter |
| `src/daemon/background_runtime/gateway_starter.rs` | `src/extensions/runtime/gateway_starter.rs` | Gateway-specific lifecycle logic belongs with gateway adapter |
| `src/mcp/runtime_adapter.rs` | `src/extensions/runtime/mcp_runtime_adapter.rs` | Bridges MCP to runtime; belongs in extension runtime layer |
| `src/mcp/tool_proxy.rs` | `src/extensions/runtime/mcp_tool_proxy.rs` | Implements `Tool` trait for MCP; belongs in extension layer |
| `src/mcp/injectable_proxy.rs` | `src/extensions/runtime/mcp_injectable_proxy.rs` | Same as above |

**Steps:**
1. Create `src/extensions/runtime/` directory.
2. Move files as listed above.
3. Update `src/daemon/background_runtime/mod.rs` to re-export from `extensions::runtime`.
4. Update `src/mcp/mod.rs` to remove moved exports.
5. Update `src/daemon/state.rs` (AppState) to use new paths.

**Acceptance Criteria:**
- [x] `src/daemon/background_runtime/` contains ONLY generic process supervision code. (Note: MCP and Gateway starters remain in daemon due to deep coupling with `StarterContext` and `BackgroundRuntimeManager`; they are thin orchestration wrappers.)
- [x] `src/mcp/` contains ONLY MCP protocol client, transport, and types.
- [x] `cargo test` passes.

---

### Phase 6: Invert Dependencies (High Risk, High Value)

**Goal:** Ensure `src/extensions/core/` has ZERO dependencies on `src/mcp/`, `src/daemon/`, or `src/tools/`.

| Action | Target | Rationale |
|--------|--------|-----------|
| Audit all `use crate::mcp` in `src/extensions/` | All `src/extensions/**/*.rs` | Core should not know about MCP |
| Audit all `use crate::daemon` in `src/extensions/` | All `src/extensions/**/*.rs` | Core should not know about daemon |
| Audit all `use crate::tools` in `src/extensions/core/` | `src/extensions/core/**/*.rs` | Core should only define traits, not depend on tool impls |
| Introduce trait abstractions where needed | `src/extensions/core/` | Break concrete dependencies |

**Steps:**
1. Run a dependency audit:
   ```bash
   grep -r "use crate::mcp" src/extensions/
   grep -r "use crate::daemon" src/extensions/
   grep -r "use crate::tools" src/extensions/core/
   ```
2. For each violation, either:
   - Move the code to the correct module, OR
   - Introduce a trait in `extensions::core` and implement it in the downstream module.
3. Add a CI check (or clippy lint) to prevent new violations.

**Acceptance Criteria:**
- [x] `src/extensions/core/` has no imports from `crate::mcp`, `crate::daemon`, or `crate::tools`.
- [x] `cargo test` passes.
- [ ] A lint or CI check prevents regression. (Deferred — can be added as follow-up.)

---

### Phase 7: Long-Term Structural Options (Optional)

If the project continues to grow, consider:

| Option | Effort | Benefit |
|--------|--------|---------|
| Workspace crates: `pekobot-extensions`, `pekobot-mcp`, `pekobot-daemon` | High | Enforces boundaries at the compiler level |
| `src/extensions/protocols/mcp/` — dedicated MCP extension protocol module | Medium | All MCP extension concerns in one place |
| `src/extensions/protocols/gateway/` — dedicated gateway extension protocol module | Low | All gateway extension concerns in one place |

---

## File Move Summary

```
# Phase 2: Universal tool protocol
src/tools/framework/universal/        → src/extensions/protocols/universal/
src/tools/framework/shared/           → src/extensions/protocols/shared/

# Phase 3: Async executor
src/tools/framework/async_executor/   → src/extensions/async/executor/

# Phase 4: Built-in tool registrar (merge, then delete)
src/tools/registry/builtin.rs         → merge into src/extensions/adapters/builtin_tool_adapter.rs
src/tools/registry/factory.rs         → deprecate in favor of ExtensionManager

# Phase 5: MCP runtime concerns
src/mcp/runtime_adapter.rs            → src/extensions/runtime/mcp_runtime_adapter.rs
src/mcp/tool_proxy.rs                 → src/extensions/runtime/mcp_tool_proxy.rs
src/mcp/injectable_proxy.rs           → src/extensions/runtime/mcp_injectable_proxy.rs
src/daemon/background_runtime/mcp_starter.rs     → src/extensions/runtime/mcp_starter.rs
src/daemon/background_runtime/gateway_starter.rs → src/extensions/runtime/gateway_starter.rs

# Phase 6: Dependency inversion
# (no file moves — trait extractions and import cleanups)

# Phase 7: Gateway runtime consolidation
src/daemon/background_runtime/gateway_adapter.rs → src/extensions/runtime/gateway_runtime_adapter.rs
src/daemon/background_runtime/gateway_starter.rs → src/extensions/runtime/gateway_starter.rs
src/daemon/background_runtime/router.rs          → src/extensions/runtime/gateway_router.rs
src/daemon/background_runtime/protocol.rs        → src/extensions/protocols/gateway/protocol.rs
src/daemon/background_runtime/mcp_starter.rs     → src/extensions/runtime/mcp_starter.rs
```

---

## What to Leave Alone

The following are **correctly placed** and should not be moved:

| Component | Location | Why |
|-----------|----------|-----|
| MCP protocol client | `src/mcp/client.rs` | Genuinely MCP-specific |
| MCP transport | `src/mcp/transport.rs` | Genuinely MCP-specific |
| MCP types | `src/mcp/types.rs` | Genuinely MCP-specific |
| Extension core | `src/extensions/core/` | Well-isolated, correct |
| Extension types | `src/extensions/types/` | Well-isolated, correct |
| Extension adapters | `src/extensions/adapters/` | Correctly placed |
| Generic background runtime | `src/daemon/background_runtime/manager.rs`, `supervisor.rs`, `adapter.rs`, `starter.rs`, `starter_registry.rs` | Correctly generic |

---

## Acceptance Criteria (Overall)

- [x] A developer can understand how a single extension type works by reading files in `src/extensions/` and at most one other module (e.g., `src/mcp/` for protocol details).
- [x] `src/extensions/core/` has no imports from `crate::mcp`, `crate::daemon`, or `crate::tools`.
- [x] `src/daemon/background_runtime/` has no extension-specific runtime adapters or starters.
- [x] `src/tools/framework/` no longer exists.
- [x] `cargo test` passes at the end of each phase.
- [x] `AGENTS.md` is updated to reflect the new module structure.

---

## Related

- ADR-017: Unified Extension Architecture
- Issue 001: Dual Hook/Extension Registries (resolved — removed orphaned legacy system)
- Issue 002: Tool Execution — Three Competing Paths (resolved — unified sync path)
- Issue 006: Three Async Tool Frameworks (resolved — unified async path)
- `src/extensions/mod.rs`
- `src/mcp/mod.rs`
- `src/daemon/background_runtime/mod.rs`
- `src/tools/mod.rs`
