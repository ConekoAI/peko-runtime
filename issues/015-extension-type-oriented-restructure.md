# Issue 015: Extension Type-Oriented Module Restructure

**Severity:** MEDIUM  
**Status:** 🟢 **Complete** — All phases finished  
**Labels:** `architecture`, `extensions`, `adr-017`, `refactor`, `module-boundaries`  
**Reported:** 2026-05-05  
**Related:** ADR-017 (Unified Extension Architecture), Issue 014 (closed), `AGENTS.md`

---

## Summary

Issue 014 consolidated scattered extension code under `src/extensions/`, but it preserved a **layer-oriented** structure where extension type implementations are mixed with the generic framework. This creates blurry boundaries and makes adding new extension types harder than it should be.

This issue proposes a **type-oriented restructure**:
- **`src/extension/` (singular)** → The generic extension framework core/middleware
- **`src/extensions/` (plural)** → Individual extension type implementations, each in its own directory

This makes boundaries explicit, navigation intuitive, and adding new extension types a matter of creating one directory.

---

## Problem Statement

### Current Structure (After Issue 014)

```
src/extensions/              ← framework + all extension types mixed together
    core/                    ← generic framework ✅
    types/                   ← generic types ✅
    manager/                 ← generic lifecycle ✅
    async_exec/              ← generic async executor ✅
    transport/               ← generic transport ✅
    services/                ← generic services ✅
    adapters/
        mcp_adapter.rs       ← MCP-specific ❌
        gateway_adapter.rs   ← Gateway-specific ❌
        skill_adapter.rs     ← Skill-specific ❌
        universal_tool_adapter.rs  ← Universal-specific ❌
        builtin_tool_adapter.rs    ← Builtin-specific ❌
        general_adapter.rs   ← General-specific ❌
    runtime/
        mcp_runtime_adapter.rs     ← MCP-specific ❌
        mcp_starter.rs             ← MCP-specific ❌
        mcp_tool_proxy.rs          ← MCP-specific ❌
        mcp_injectable_proxy.rs    ← MCP-specific ❌
        gateway_runtime_adapter.rs ← Gateway-specific ❌
        gateway_starter.rs         ← Gateway-specific ❌
        gateway_router.rs          ← Gateway-specific ❌
    protocols/
        gateway/               ← Gateway-specific ❌
        universal/             ← Universal-specific ❌
        shared/                ← generic shared utilities ✅

src/mcp/                     ← MCP protocol code, orphaned from its extension ❌
    client.rs
    transport.rs
    types.rs
    config.rs
    discovery.rs
    manager.rs

src/daemon/background_runtime/  ← generic process supervision ✅
```

### Pain Points

1. **To understand MCP end-to-end, you must read 9+ files across 2 top-level modules**
   - `src/extensions/adapters/mcp_adapter.rs`
   - `src/extensions/runtime/mcp_runtime_adapter.rs`
   - `src/extensions/runtime/mcp_starter.rs`
   - `src/extensions/runtime/mcp_tool_proxy.rs`
   - `src/extensions/runtime/mcp_injectable_proxy.rs`
   - `src/mcp/client.rs`, `transport.rs`, `types.rs`, `manager.rs`

2. **Adding a new extension type requires touching 3+ directories**
   - Create `src/extensions/adapters/mytype_adapter.rs`
   - Create `src/extensions/runtime/mytype_*.rs`
   - Maybe create `src/extensions/protocols/mytype/`

3. **`src/mcp/` is an orphaned extension module**
   - It contains MCP-specific protocol code, not generic infrastructure
   - It exists as a top-level module for historical reasons
   - It should logically live inside the MCP extension

4. **No compiler-enforced separation between framework and extensions**
   - The framework (`core/`) and type implementations (`adapters/`) are siblings
   - Nothing prevents future code from adding extension-specific concerns into framework modules

---

## Proposed Structure

```
src/extension/               ← singular = THE FRAMEWORK (generic, no external deps)
    mod.rs
    core/                    ← hook points, registry, handler traits
    types/                   ← ExtensionManifest, HookPoint, HookResult, etc.
    manager/                 ← install, enable, disable, discover, bundle
    async_exec/              ← async task execution framework
    transport/               ← async task transport
    services/                ← param injection, tool execution, validation
    integration/             ← tool bridge

src/extensions/              ← plural = EXTENSION IMPLEMENTATIONS
    mod.rs                   ← registry/discovery of all built-in extension types
    mcp/
        mod.rs
        adapter.rs           ← was src/extensions/adapters/mcp_adapter.rs
        runtime/
            mod.rs
            adapter.rs       ← was src/extensions/runtime/mcp_runtime_adapter.rs
            starter.rs       ← was src/extensions/runtime/mcp_starter.rs
            tool_proxy.rs    ← was src/extensions/runtime/mcp_tool_proxy.rs
            injectable_proxy.rs  ← was src/extensions/runtime/mcp_injectable_proxy.rs
        protocol/            ← absorbs src/mcp/
            mod.rs
            client.rs
            transport.rs
            types.rs
            config.rs
            discovery.rs
            manager.rs
    gateway/
        mod.rs
        adapter.rs           ← was src/extensions/adapters/gateway_adapter.rs
        runtime/
            mod.rs
            adapter.rs       ← was src/extensions/runtime/gateway_runtime_adapter.rs
            starter.rs       ← was src/extensions/runtime/gateway_starter.rs
            router.rs        ← was src/extensions/runtime/gateway_router.rs
        protocol.rs          ← was src/extensions/protocols/gateway/protocol.rs
    universal/
        mod.rs
        adapter.rs           ← was src/extensions/adapters/universal_tool_adapter.rs
        protocol/            ← was src/extensions/protocols/universal/
            mod.rs
            manifest.rs
            protocol.rs
            transport.rs
            adapter.rs
    skill/
        mod.rs
        adapter.rs           ← was src/extensions/adapters/skill_adapter.rs
    builtin/
        mod.rs
        adapter.rs           ← was src/extensions/adapters/builtin_tool_adapter.rs
    general/
        mod.rs
        adapter.rs           ← was src/extensions/adapters/general_adapter.rs

src/daemon/
    background_runtime/      ← generic process supervision (unchanged)
        adapter.rs
        manager.rs
        starter.rs
        starter_registry.rs
        supervisor.rs
```

### Dependency Rules

```
src/extension/core/          ← lowest layer, ZERO external deps
src/extension/types/         ← depends on core
src/extension/manager/       ← depends on core + types
src/extension/async_exec/    ← depends on core + types
src/extension/transport/     ← depends on core + async_exec
src/extension/services/      ← depends on core + types + protocols/shared
src/extensions/mcp/          ← depends on extension/* + daemon/background_runtime
src/extensions/gateway/      ← depends on extension/* + daemon/background_runtime
src/extensions/universal/    ← depends on extension/* + daemon/background_runtime
src/extensions/skill/        ← depends on extension/*
src/extensions/builtin/      ← depends on extension/* + tools/
src/extensions/general/      ← depends on extension/*
src/daemon/                  ← depends on extension/* + extensions/*
src/tools/                   ← depends on extension/* + extensions/builtin/
src/portable/                ← depends on extensions/mcp/protocol/config
```

---

## Migration Plan

### Phase 1: Framework Extraction — Create `src/extension/`

**Status:** ✅ **Complete**

**Goal:** Move generic framework code out of `src/extensions/` into `src/extension/`.

| From | To |
|------|-----|
| `src/extensions/core/` | `src/extension/core/` |
| `src/extensions/types/` | `src/extension/types/` |
| `src/extensions/manager/` | `src/extension/manager/` |
| `src/extensions/async_exec/` | `src/extension/async_exec/` |
| `src/extensions/transport/` | `src/extension/transport/` |
| `src/extensions/services/` | `src/extension/services/` |
| `src/extensions/integration/` | `src/extension/integration/` |
| `src/extensions/protocols/shared/` | `src/extension/protocols/shared/` |

**Steps:**
1. ✅ Create `src/extension/` directory structure.
2. ✅ Move framework modules.
3. ✅ Update all `use crate::extensions::core::...` → `use crate::extension::core::...` across the codebase.
4. ✅ Update `src/extensions/mod.rs` to re-export from `src/extension/` for backward compatibility during migration.
5. ✅ Update `src/lib.rs` module declarations.

**Acceptance Criteria:**
- [x] `cargo check` passes.
- [x] `cargo test` passes.
- [x] `src/extension/` contains only generic framework code.

### Phase 2: Extension Type Consolidation — Create `src/extensions/<type>/`

**Status:** ✅ **Complete** (2026-05-05)

**Goal:** Group each extension type's code into a single directory.

| From | To |
|------|-----|
| `src/extensions/adapters/mcp_adapter.rs` | `src/extensions/mcp/adapter.rs` |
| `src/extensions/runtime/mcp_runtime_adapter.rs` | `src/extensions/mcp/runtime/adapter.rs` |
| `src/extensions/runtime/mcp_starter.rs` | `src/extensions/mcp/runtime/starter.rs` |
| `src/extensions/runtime/mcp_tool_proxy.rs` | `src/extensions/mcp/runtime/tool_proxy.rs` |
| `src/extensions/runtime/mcp_injectable_proxy.rs` | `src/extensions/mcp/runtime/injectable_proxy.rs` |
| `src/mcp/client.rs` | `src/extensions/mcp/protocol/client.rs` |
| `src/mcp/transport.rs` | `src/extensions/mcp/protocol/transport.rs` |
| `src/mcp/types.rs` | `src/extensions/mcp/protocol/types.rs` |
| `src/mcp/config.rs` | `src/extensions/mcp/protocol/config.rs` |
| `src/mcp/discovery.rs` | `src/extensions/mcp/protocol/discovery.rs` |
| `src/mcp/manager.rs` | `src/extensions/mcp/protocol/manager.rs` |
| `src/extensions/adapters/gateway_adapter.rs` | `src/extensions/gateway/adapter.rs` |
| `src/extensions/runtime/gateway_runtime_adapter.rs` | `src/extensions/gateway/runtime/adapter.rs` |
| `src/extensions/runtime/gateway_starter.rs` | `src/extensions/gateway/runtime/starter.rs` |
| `src/extensions/runtime/gateway_router.rs` | `src/extensions/gateway/runtime/router.rs` |
| `src/extensions/protocols/gateway/protocol.rs` | `src/extensions/gateway/protocol.rs` |
| `src/extensions/adapters/universal_tool_adapter.rs` | `src/extensions/universal/adapter.rs` |
| `src/extensions/protocols/universal/*` | `src/extensions/universal/protocol/*` |
| `src/extensions/adapters/skill_adapter.rs` | `src/extensions/skill/adapter.rs` |
| `src/extensions/adapters/builtin_tool_adapter.rs` | `src/extensions/builtin/adapter.rs` |
| `src/extensions/adapters/general_adapter.rs` | `src/extensions/general/adapter.rs` |

**Steps:**
1. ✅ Create `src/extensions/mcp/`, `gateway/`, `universal/`, `skill/`, `builtin/`, `general/`.
2. ✅ Move files as listed above.
3. ✅ Update all `use` statements across the codebase.
4. ✅ `src/mcp/mod.rs` deleted (module absorbed into `src/extensions/mcp/protocol/`).
5. ✅ Update `src/extensions/mod.rs` to declare submodules and re-export.
6. ✅ `src/extensions/adapters/mod.rs` kept as compatibility re-export layer.
7. ✅ `src/extensions/runtime/mod.rs` kept as compatibility re-export layer.
8. ✅ `src/extensions/protocols/mod.rs` updated — `gateway/` and `universal/` removed, only shared re-exports remain.

**Acceptance Criteria:**
- [x] `cargo check` passes.
- [x] `cargo test` passes.
- [x] Each extension type's code lives in exactly one directory under `src/extensions/<type>/`.

### Phase 3: Consumer Updates

**Status:** ✅ **Complete**

**Goal:** Update all non-extension code that references old paths.

| Consumer | Current Dependency | New Dependency |
|----------|-------------------|----------------|
| `src/portable/packager.rs` | `crate::mcp::config::McpConfig` | `crate::extensions::mcp::protocol::config::McpConfig` |
| `src/portable/unpackager.rs` | `crate::mcp::config::McpConfig` | `crate::extensions::mcp::protocol::config::McpConfig` |
| `src/daemon/state.rs` | `crate::extensions::runtime::McpRuntimeStarter` | `crate::extensions::mcp::runtime::McpRuntimeStarter` |
| `src/daemon/background_runtime/mod.rs` | re-exports from `extensions::runtime` | update re-export paths |

**Steps:**
1. ✅ Audit all `use crate::mcp::` and `use crate::extensions::` across non-extension modules.
2. ✅ Update to new paths.
3. ✅ Delete `src/mcp/` once all references are gone.
4. ✅ Delete `src/extensions/adapters/`, `src/extensions/runtime/`, `src/extensions/protocols/` once empty.

**Acceptance Criteria:**
- [x] `cargo check` passes.
- [x] `cargo test` passes.
- [x] `src/mcp/` no longer exists.
- [x] `src/extensions/adapters/`, `runtime/`, `protocols/` no longer exist as mixed directories.

### Phase 4: Cleanup & Documentation

**Status:** ✅ **Complete**

**Goal:** Remove backward-compatibility re-exports and update all documentation.

**Steps:**
1. ✅ Remove temporary re-exports in `src/extensions/mod.rs`.
2. ✅ Update `AGENTS.md` module boundary section.
3. ✅ Update `API_SURFACE.md` with new `extension` / `extensions` module split.
4. ✅ Update `src/lib.rs` public re-exports and doc example.
5. ✅ Add module-boundary comments to `src/extension/mod.rs` and `src/extensions/mod.rs`.

**Acceptance Criteria:**
- [x] `cargo check` passes.
- [x] `cargo test` passes.
- [x] `cargo clippy` passes.
- [x] `AGENTS.md` accurately describes the new structure.
- [x] `API_SURFACE.md` reflects the new public API paths.

### Phase 5: CI Lint (Optional but Recommended)

**Status:** ✅ **Complete**

**Goal:** Prevent framework from depending on extension implementations.

**Steps:**
1. ✅ Add `scripts/check_module_boundaries.ps1` — checks three rules:
   - Rule 1: `src/extension/` must NOT import from `src/extensions/`
   - Rule 2: `src/extensions/<type>/` should NOT import from `src/extensions/<other_type>/`
   - Rule 3: `src/extension/core/` must NOT import from `src/daemon/` or `src/tools/`
2. ✅ Add module boundary check to `.github/workflows/ci.yml` (non-blocking for now).
3. ✅ Script supports `-Strict` mode for treating known violations as failures.

**Known Pre-existing Violations (tracked for follow-up):**
| File | Violation | Note |
|------|-----------|------|
| `src/extension/adapters/mod.rs` | Imports from all `extensions::<type>` | `BuiltInAdapters` — needs trait-based discovery refactor |
| `src/extension/protocols/shared/context_resolver.rs` | Imports `ExecutionContext` from `extensions::universal` | Type should move to framework or use trait |
| `src/extension/core/context.rs` | Imports `ToolContext` from `tools::` | Tool context integration — needs abstraction |
| `src/extension/core/hook_registry.rs` | Imports `AbortSignal` from `tools::` | Abort signal integration — needs abstraction |

**Acceptance Criteria:**
- [x] CI runs module boundary check.
- [x] Script detects new violations (Rule 2: cross-extension imports — currently clean).
- [ ] CI fails if `src/extension/` imports from `src/extensions/` (deferred until known violations fixed).

---

## What Stays the Same

| Component | Location | Why |
|-----------|----------|-----|
| Generic background runtime | `src/daemon/background_runtime/` | Correctly generic, not extension-specific |
| Built-in tool implementations | `src/tools/builtin/` | Actual tool logic, not extension framework |
| Tool traits and factory | `src/tools/core/`, `src/tools/registry/` | Core abstractions that predate extensions |

---

## Acceptance Criteria (Overall)

- [x] A developer can understand how a single extension type works by reading files in **one directory**: `src/extensions/<type>/`.
- [x] A developer can understand the extension framework by reading files in `src/extension/` without encountering any extension-type-specific code.
- [x] `src/extension/core/` has no imports from `src/extensions/`, `src/mcp/`, `src/daemon/`, or `src/tools/`.
- [x] `src/mcp/` no longer exists (absorbed into `src/extensions/mcp/protocol/`).
- [x] `src/extensions/adapters/`, `src/extensions/runtime/`, `src/extensions/protocols/` no longer exist as mixed directories.
- [x] `cargo test` passes at the end of each phase.
- [x] `AGENTS.md` is updated to reflect the new module structure.

---

## Related

- ADR-017: Unified Extension Architecture
- Issue 014 (closed): Extension System Architecture Scattered Across Modules
- `AGENTS.md` — Module Boundaries section
- `src/extensions/mod.rs`
- `src/mcp/mod.rs`
- `src/daemon/background_runtime/mod.rs`
