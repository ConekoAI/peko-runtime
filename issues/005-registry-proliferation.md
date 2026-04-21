# Issue 005: Registry Proliferation — ~25+ Registry/Manager Types

**Severity:** MEDIUM-HIGH  
**Status:** Open  
**Labels:** `architecture`, `registry`, `infrastructure`, `refactor`, `dry`  
**Reported:** 2026-04-21  

---

## Summary

The codebase contains approximately 25+ registry or manager types, many of which are thin wrappers around `HashMap<K, V>` with `Arc<RwLock<...>>`. There is no unified registry trait, no shared infrastructure for indexing, locking, or lifecycle management, and no consistent naming convention (`Registry` vs `Manager` vs `Service`). This proliferation creates maintenance burden, duplicated locking patterns, and inconsistent APIs.

---

## Complete Inventory

### Hook / Extension Registries

| Type | Location | Purpose |
|------|----------|---------|
| `HookRegistry` | `src/hooks/registry.rs` | External triggers (webhook, cron, file watch) |
| `HookRegistry` | `src/extensions/core/hook_registry.rs` | Extension lifecycle hooks |
| `ToolRegistry` | `src/extensions/core/tool_registry.rs` | Tool metadata and enable/disable policy |
| `ExtensionManager` | `src/extensions/manager/mod.rs` | Extension loading and lifecycle |
| `AsyncToolRegistry` | `src/extensions/async_integration.rs` | Async tool registration |
| `ToolRegistryAccess` | `src/extensions/types.rs` | Accessor for tool registry |

### Agent Registries

| Type | Location | Purpose |
|------|----------|---------|
| `LocalRegistry` | `src/agent/registry.rs` | Agent capabilities registry |
| `SubagentRegistry` | `src/agent/subagent_registry.rs` | Subagent registration |
| `AsyncTaskRegistry` | `src/agent/async_tool_framework.rs` | Async task tracking |
| `AsyncResultQueueManager` | `src/agent/async_tool_framework.rs` | Async result queue |
| `LifecycleManager` | `src/agent/lifecycle.rs` | Agent lifecycle |
| `BackgroundTaskManager` | `src/agent/subagent_executor.rs` | Background task tracking |
| `StatelessAgentManager` | `src/agent/stateless_manager.rs` | Stateless agent management |
| `AgentRegistryView` | `src/agent/context.rs` | Read-only agent registry view |

### Provider / Tool Registries

| Type | Location | Purpose |
|------|----------|---------|
| `ProviderRegistry` | `src/providers/registry.rs` | LLM provider registration |
| `BuiltinRegistry` | `src/tools/builtin_registry.rs` | Built-in tool registration config |
| `McpManager` | `src/mcp/manager.rs` | MCP server management (acts as registry) |

### Session Registries

| Type | Location | Purpose |
|------|----------|---------|
| `SessionManager` | `src/session/manager.rs` | Session lifecycle |
| `LockManager` | `src/session/lock.rs` | Session locking |
| `AgentSessionRegistry` | `src/tools/session_introspection.rs` | Session introspection |
| `InMemorySessionRegistry` | `src/tools/session_introspection.rs` | In-memory session cache |

### Image / Remote Registries

| Type | Location | Purpose |
|------|----------|---------|
| `ImageRegistry` | `src/image/registry.rs` | Docker image registry config |
| `RegistryClient` | `src/registry/client.rs` | Remote registry client |
| `RegistryRef` | `src/registry/client.rs` | Registry reference |
| `RegistryConfig` | `src/registry/config.rs` | Registry configuration |
| `RegistrySource` | `src/registry/config.rs` | Registry source definition |
| `ToolRegistryRef` | `src/portable/manifest.rs` | Portable tool registry reference |
| `ToolRegistryConfig` | `src/portable/manifest.rs` | Portable tool registry config |

### Team / Service Registries

| Type | Location | Purpose |
|------|----------|---------|
| `TeamManager` | `src/team/mod.rs` | Team lifecycle |
| `TeamCapabilityManager` | `src/team/capability.rs` | Team capability management |
| `ServiceRegistry` | `src/common/services/mod.rs` | DI container for services |
| `ConnectionManager` | `src/ipc/connection.rs` | IPC connection management |

---

## Common Patterns

Most of these types follow one of these templates:

### Pattern A: `Arc<RwLock<HashMap<K, V>>>`

```rust
pub struct FooRegistry {
    items: Arc<RwLock<HashMap<String, Foo>>>,
}

impl FooRegistry {
    pub fn new() -> Self { ... }
    pub async fn register(&self, id: String, item: Foo) { ... }
    pub async fn get(&self, id: &str) -> Option<Foo> { ... }
    pub async fn list(&self) -> Vec<Foo> { ... }
    pub async fn unregister(&self, id: &str) -> bool { ... }
}
```

**Examples:** `HookRegistry` (both), `ToolRegistry`, `LocalRegistry`, `AsyncTaskRegistry`, `ProviderRegistry`

### Pattern B: `Arc<RwLock<HashMap<K, Vec<V>>>>` (indexed)

```rust
pub struct BarRegistry {
    items: Arc<RwLock<HashMap<String, Bar>>>,
    by_category: Arc<RwLock<HashMap<String, Vec<String>>>>,
}
```

**Examples:** `hooks::HookRegistry` (webhooks, event_hooks, file_watches), `extensions::core::HookRegistry` (hooks_by_point)

### Pattern C: Manager with lifecycle

```rust
pub struct BazManager {
    items: Arc<RwLock<HashMap<String, Baz>>>,
    // plus config, paths, metrics, etc.
}
```

**Examples:** `SessionManager`, `McpManager`, `TeamManager`, `ExtensionManager`

---

## Impact

1. **Code duplication:** Every registry re-implements `Arc<RwLock<HashMap>>`, error handling, and async locking patterns.
2. **Inconsistent APIs:** Some use `register(id, item)`, others `insert(item)`, others `add(item)`. Some return `Result`, others `Option`, others `bool`.
3. **No shared infrastructure:** There is no generic `Registry<K, V>` that provides indexing, locking, events, or metrics.
4. **Testing overhead:** Each registry needs its own test suite for basic CRUD operations.
5. **Composability issues:** Registries cannot be easily composed or wrapped because they have no common interface.
6. **Naming confusion:** `Registry` vs `Manager` vs `Service` is used inconsistently. `McpManager` is a registry. `SessionManager` is a registry + lifecycle manager. `ServiceRegistry` is a DI container.

---

## Root Cause

- Rust's type system makes generic registries slightly verbose (need to handle `Send + Sync + Clone` bounds).
- Each registry was added independently for a specific feature without evaluating shared infrastructure.
- No architectural guideline exists for when to use `Registry` vs `Manager` vs `Service`.
- The `dashmap` or `scc` crates were not adopted early, so every registry hand-rolls `RwLock<HashMap>`.

---

## Proposed Resolution

### Phase 1: Extract generic `Registry<K, V>` infrastructure (P1)

Create a `registry` module or crate with:

```rust
pub trait Registry<K, V> {
    async fn insert(&self, key: K, value: V) -> Result<()>;
    async fn get(&self, key: &K) -> Option<V>;
    async fn remove(&self, key: &K) -> Option<V>;
    async fn list(&self) -> Vec<V>;
    async fn contains(&self, key: &K) -> bool;
    async fn len(&self) -> usize;
}

pub struct AsyncRegistry<K, V> {
    inner: RwLock<HashMap<K, V>>,
}

impl<K: Eq + Hash + Clone, V: Clone> AsyncRegistry<K, V> {
    pub fn new() -> Self { ... }
    pub fn with_index<F, I>(index_fn: F) -> IndexedRegistry<K, V, I> { ... }
}
```

### Phase 2: Migrate simple registries to generic implementation (P2)

Migrate registries that are pure `HashMap` wrappers:

- `LocalRegistry`
- `AsyncTaskRegistry`
- `ProviderRegistry`
- `ToolRegistry` (metadata only)
- Both `HookRegistry` types (if not merged first — see Issue 001)

### Phase 3: Establish naming conventions (P2)

| Suffix | Meaning | Example |
|--------|---------|---------|
| `Registry` | Read-heavy, lookup-oriented collection | `ToolRegistry` |
| `Manager` | Lifecycle + stateful coordination | `SessionManager`, `ExtensionManager` |
| `Service` | Business logic / use-case orchestration | `SessionService` |
| `Client` | External API consumer | `RegistryClient` |

### Phase 4: Evaluate `dashmap` or `scc` for lock-free reads (P3)

Many registries are read-heavy (tool lookups, provider lookups). A concurrent hash map would eliminate `RwLock` contention.

---

## Acceptance Criteria

- [ ] A generic `Registry<K, V>` trait and implementation exist.
- [ ] At least 5 registries are migrated to the generic implementation.
- [ ] A naming convention document is added to `docs/architecture/` or `AGENTS.md`.
- [ ] No new hand-rolled `Arc<RwLock<HashMap>>` registry is added without justification.
- [ ] (Optional) A concurrent hash map crate is evaluated for read-heavy registries.

---

## Related

- Issue 001: Dual Hook/Extension Registries
- Issue 002: Tool Execution — Three Paths
- `src/hooks/registry.rs`
- `src/extensions/core/hook_registry.rs`
- `src/extensions/core/tool_registry.rs`
- `src/agent/registry.rs`
- `src/providers/registry.rs`
- `src/session/manager.rs`
- `src/team/mod.rs`
- `src/common/services/mod.rs`
