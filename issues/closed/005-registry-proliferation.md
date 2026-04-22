# Issue 005: Registry Proliferation — ~25+ Registry/Manager Types

**Severity:** MEDIUM-HIGH  
**Status:** ✅ COMPLETED  
**Labels:** `architecture`, `registry`, `infrastructure`, `refactor`, `dry`  
**Reported:** 2026-04-21  
**Updated:** 2026-04-22 — All phases complete, issue closed

---

## Summary

The codebase contains approximately 25+ registry or manager types, many of which are thin wrappers around `HashMap<K, V>` with `Arc<RwLock<...>>`. There is no unified registry trait, no shared infrastructure for indexing, locking, or lifecycle management, and no consistent naming convention (`Registry` vs `Manager` vs `Service`). This proliferation creates maintenance burden, duplicated locking patterns, and inconsistent APIs.

---

## Honest Assessment (2026-04-22)

### What Was Done

1. ✅ Created generic registry infrastructure (`SimpleRegistry`, `SharedRegistry`)
2. ✅ Migrated 7 registries to use the infrastructure
3. ✅ **Deleted `LocalRegistry`** — was dead code (nothing instantiated or used it)
4. ✅ **Deleted `IndexedRegistry`** — was only used in its own tests, not production
5. ✅ Renamed misleading types (`BuiltinRegistry` → `BuiltinToolRegistrar`, `InMemorySessionRegistry` → `SessionCache`)
6. ✅ Removed unused `Registry` trait from `agent/registry.rs`
7. ✅ Added naming convention documentation

### Net Code Change

| Change | Lines |
|--------|-------|
| Registry infrastructure (`core.rs` + `mod.rs`) | +436 |
| `IndexedRegistry` deleted | −470 |
| `LocalRegistry` deleted (was 259 lines, now 12) | −247 |
| `SessionCache` migration (3× `Mutex<HashMap>` → `SimpleRegistry`) | −3 |
| `AsyncToolRegistry` (ext) cleanup (`todo!()` removed) | −15 |
| **Net** | **−299** |

**The issue now achieves net code reduction.** The key insight from reviewer feedback: deleting dead code (`LocalRegistry`, `IndexedRegistry`) matters more than migrating live code to generic wrappers.

### What the Migrations Actually Did

The `SimpleRegistry`/`SharedRegistry` migrations are **storage standardization**, not line reduction per registry. Each migrated registry only had ~10–20 lines of `HashMap` boilerplate. The real wins were:

- **Deleting `LocalRegistry`** (~247 lines) — unused dead code
- **Deleting `IndexedRegistry`** (~470 lines) — unused abstraction
- **Standardizing locking patterns** in `ToolRegistry`, `SessionCache`, `LockManager` — prevents future duplication

---

## Complete Inventory

### Hook / Extension Registries

| Type | Location | Purpose | Status |
|------|----------|---------|--------|
| `HookRegistry` | `src/hooks/registry.rs` | External triggers — **REMOVED per Issue 001** | ✅ Gone |
| `HookRegistry` | `src/extensions/core/hook_registry.rs` | Extension lifecycle hooks — **Registry + Dispatcher** | Keep as-is |
| `ToolRegistry` | `src/extensions/core/tool_registry.rs` | Tool metadata and enable/disable policy | ✅ Migrated to `SharedRegistry` |
| `ExtensionManager` | `src/extensions/manager/mod.rs` | Extension loading and lifecycle — **Manager** | Keep as-is |
| `AsyncToolRegistry` | `src/extensions/async_integration.rs` | Async tool registration | ✅ Migrated to `SimpleRegistry` |
| `ToolRegistryAccess` | `src/extensions/types.rs` | Accessor for tool registry | Keep as-is |

### Agent Registries

| Type | Location | Purpose | Status |
|------|----------|---------|--------|
| ~~`LocalRegistry`~~ | ~~`src/agent/registry.rs`~~ | ~~Agent capabilities registry~~ | ✅ **Deleted** — dead code |
| `SubagentRegistry` | `src/agent/subagent_registry.rs` | Subagent registration | ✅ Migrated to `SimpleRegistry` |
| `AsyncTaskRegistry` | `src/agent/async_tool_framework.rs` | Async task tracking | ✅ Migrated to `SimpleRegistry` |
| `AsyncResultQueueManager` | `src/agent/async_tool_framework.rs` | Async result queue | ✅ Migrated to `SimpleRegistry` |
| `LifecycleManager` | `src/agent/lifecycle.rs` | Agent lifecycle — **Manager** | Keep as-is |
| `BackgroundTaskManager` | `src/agent/subagent_executor.rs` | Background task tracking — **Manager** | Keep as-is |
| `StatelessAgentManager` | `src/agent/stateless_manager.rs` | Stateless agent management — **Manager** | Keep as-is |
| `AgentRegistryView` | `src/agent/context.rs` | Read-only agent registry view | Keep as-is |

### Provider / Tool Registries

| Type | Location | Purpose | Status |
|------|----------|---------|--------|
| `ProviderRegistry` | `src/providers/registry.rs` | LLM provider registration — read-only static | Keep as-is |
| `BuiltinToolRegistrar` | `src/tools/builtin_registry.rs` | Built-in tool registration helper (was `BuiltinRegistry`) | ✅ Renamed |
| `McpManager` | `src/mcp/manager.rs` | MCP server management — **Manager** | Keep as-is |

### Session Registries

| Type | Location | Purpose | Status |
|------|----------|---------|--------|
| `SessionManager` | `src/session/manager.rs` | Session lifecycle — **Manager** | Keep as-is |
| `LockManager` | `src/session/lock.rs` | Session locking | ✅ Migrated internal map to `SimpleRegistry` |
| ~~`AgentSessionRegistry`~~ | ~~`src/tools/session_introspection.rs`~~ | ~~Session introspection wrapper~~ | ✅ Renamed to `SessionIntrospector` |
| `SessionCache` | `src/tools/session_introspection.rs` | In-memory session cache (was `InMemorySessionRegistry`) | ✅ Renamed + migrated |

### Image / Remote Registries

| Type | Location | Purpose | Status |
|------|----------|---------|--------|
| `ImageRegistry` | `src/image/registry.rs` | Docker image registry config | Keep as-is (external concept) |
| `RegistryClient` | `src/registry/client.rs` | Remote registry client — **Client** | Keep as-is |
| `RegistryRef` | `src/registry/client.rs` | Registry reference | Keep as-is |
| `RegistryConfig` | `src/registry/config.rs` | Registry configuration | Keep as-is |
| `RegistrySource` | `src/registry/config.rs` | Registry source definition | Keep as-is |
| ~~`ToolRegistryRef`~~ | ~~`src/portable/manifest.rs`~~ | ~~Portable tool source reference~~ | ✅ Renamed to `ToolSourceRef` |
| ~~`ToolRegistryConfig`~~ | ~~`src/portable/manifest.rs`~~ | ~~Portable tool source config~~ | ✅ Renamed to `ToolSourceConfig` |

### Team / Service Registries

| Type | Location | Purpose | Status |
|------|----------|---------|--------|
| `TeamManager` | `src/team/mod.rs` | Team lifecycle — **Manager** | Keep as-is |
| `TeamCapabilityManager` | `src/team/capability.rs` | Team capability management — **Manager** | Keep as-is |
| ~~`ServiceRegistry`~~ | ~~`src/common/services/mod.rs`~~ | ~~DI container for services~~ | ✅ Renamed to `ServiceContainer` |
| `ConnectionManager` | `src/ipc/connection.rs` | IPC connection management — **Manager** | Keep as-is |

---

## Common Patterns

### Pattern A: `Arc<RwLock<HashMap<K, V>>>`

**Examples:** `HookRegistry` (both), `ToolRegistry` (migrated), ~~`LocalRegistry`~~ (deleted)

### Pattern B: `Arc<RwLock<HashMap<K, Vec<V>>>>` (indexed)

**Examples:** `hooks::HookRegistry` (webhooks, event_hooks, file_watches), `extensions::core::HookRegistry` (hooks_by_point)

### Pattern C: Manager with lifecycle

**Examples:** `SessionManager`, `McpManager`, `TeamManager`, `ExtensionManager`

---

## Impact

1. **Code duplication:** Every registry re-implements `Arc<RwLock<HashMap>>`, error handling, and async locking patterns.
2. **Inconsistent APIs:** Some use `register(id, item)`, others `insert(item)`, others `add(item)`. Some return `Result`, others `Option`, others `bool`.
3. **No shared infrastructure:** There is no generic `Registry<K, V>` that provides indexing, locking, or metrics.
4. **Testing overhead:** Each registry needs its own test suite for basic CRUD operations.
5. **Composability issues:** Registries cannot be easily composed or wrapped because they have no common interface.
6. **Naming confusion:** `Registry` vs `Manager` vs `Service` is used inconsistently.

---

## Root Cause

- Rust's type system makes generic registries slightly verbose (need to handle `Send + Sync + Clone` bounds).
- Each registry was added independently for a specific feature without evaluating shared infrastructure.
- No architectural guideline exists for when to use `Registry` vs `Manager` vs `Service`.
- The `dashmap` or `scc` crates were not adopted early, so every registry hand-rolls `RwLock<HashMap>`.

---

## Resolution Plan

### Phase 1: Extract Generic Registry Infrastructure ✅ COMPLETE

Created `src/common/registry/core.rs` with:
- `SimpleRegistry<K, V>` — zero-cost `HashMap` wrapper for single-owner scenarios
- `SharedRegistry<K, V>` — `Arc<RwLock<HashMap>>` wrapper for shared scenarios

**Deleted:** `IndexedRegistry` — was only used in its own tests, not production code. Revisit when a real use case emerges.

### Phase 2: Migrate Simple Registries ✅ COMPLETE

Migrated 7 registries to generic infrastructure:
- `SubagentRegistry` → `SimpleRegistry`
- `AsyncTaskRegistry` → `SimpleRegistry`
- `AsyncResultQueueManager` → `SimpleRegistry`
- `ToolRegistry` → `SharedRegistry`
- `SessionCache` (was `InMemorySessionRegistry`) → 3× `SimpleRegistry`
- `AsyncToolRegistry` (ext) → `SimpleRegistry`
- `LockManager` internal map → `Mutex<SimpleRegistry>`

**Deleted:** `LocalRegistry` — was dead code (~247 lines removed).

### Phase 3: Establish Naming Conventions ✅ COMPLETE

Added `docs/architecture/NAMING_CONVENTIONS.md`:

| Suffix | Meaning | Example |
|--------|---------|---------|
| `Registry` | Read-heavy, lookup-oriented collection | `ToolRegistry` |
| `Manager` | Lifecycle + stateful coordination | `SessionManager` |
| `Service` | Business logic / use-case orchestration | `SessionService` |
| `Client` | External API consumer | `RegistryClient` |
| `Cache` | In-memory temporary storage | `SessionCache` |
| `Registrar` | One-time registration helper | `BuiltinToolRegistrar` |

### Phase 4: Rename Misleading Types ✅ COMPLETE

| Current | Proposed | Status |
|---------|----------|--------|
| `BuiltinRegistry` | `BuiltinToolRegistrar` | ✅ Done |
| `InMemorySessionRegistry` | `SessionCache` | ✅ Done |
| `ServiceRegistry` | `ServiceContainer` | ✅ Done |
| `ToolRegistryRef` | `ToolSourceRef` | ✅ Done |
| `ToolRegistryConfig` | `ToolSourceConfig` | ✅ Done |

### Phase 5: Evaluate Concurrent Hash Maps ⬜ PENDING

Many registries are read-heavy. `SharedRegistry` can internally switch to `dashmap` or `scc` without API changes once benchmarks justify it.

---

## Remaining Gaps (Reviewer Feedback)

1. **IndexedRegistry is built but not yet used by any registry** — It was deleted as dead code (only used in its own tests). The infrastructure in `SimpleRegistry`/`SharedRegistry` is sufficient for current needs. Revisit `IndexedRegistry` only when a real multi-index use case emerges.
2. **LocalRegistry was deleted as unused, not migrated** — Correct. Nothing in the codebase instantiated it. Migration would have been wasted work.
3. **Net addition in `common/registry/` is the trade-off** — ~+436 lines of infrastructure enable ~−630 lines of dead code removal and standardization across 7+ registries.

Many registries are read-heavy. `SharedRegistry` can internally switch to `dashmap` or `scc` without API changes once benchmarks justify it.

---

## What NOT to Migrate

These are genuine managers with lifecycle concerns and should NOT be forced into a generic registry abstraction:

- `SessionManager` — I/O, overlays, branching, metadata controller
- `McpManager` — External processes, health checks, reconnections
- `TeamManager` — Agent deployment, event buses, shared services
- `ExtensionManager` — File I/O, adapter registration, bundling
- `HookRegistry` (extensions) — Has dispatcher logic, services, global enable flag
- `ProviderRegistry` — Read-only static data, already clean
- `AsyncResultQueueManager` — Queue semantics, not registry semantics

---

## Acceptance Criteria

- [x] A generic registry infrastructure exists in `src/common/registry/`.
- [x] At least 5 registries are migrated to the generic implementation. (Actual: 7)
- [x] A naming convention document is added to `docs/architecture/`.
- [x] Net code reduction achieved (~−299 lines).
- [x] No new hand-rolled `Arc<RwLock<HashMap>>` registry is added without justification.
- [ ] (Optional) A concurrent hash map crate is evaluated for read-heavy registries. — Deferred to future benchmark-driven work.

---

## Related

- Issue 001: Dual Hook/Extension Registries
- Issue 002: Tool Execution — Three Paths
- `src/hooks/registry.rs` (removed)
- `src/extensions/core/hook_registry.rs`
- `src/extensions/core/tool_registry.rs`
- `src/agent/registry.rs`
- `src/providers/registry.rs`
- `src/session/manager.rs`
- `src/team/mod.rs`
- `src/common/services/mod.rs`
