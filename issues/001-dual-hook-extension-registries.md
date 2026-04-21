# Issue 001: Dual Hook/Extension Registries — Competing Abstractions

**Severity:** CRITICAL  
**Status:** Open  
**Labels:** `architecture`, `naming-collision`, `adr-017`, `milestone-8`, `refactor`  
**Reported:** 2026-04-21  

---

## Summary

The codebase contains two entirely different `HookRegistry` types with overlapping names but completely different semantics. This creates naming collision, developer confusion, and an unclear boundary between the legacy external-trigger system (Milestone 8) and the future extension framework (ADR-017).

---

## Systems Involved

| System | Location | Purpose |
|--------|----------|---------|
| `hooks::HookRegistry` | `src/hooks/registry.rs` | Webhook / cron / file-watch / event-bus hooks (external triggers) |
| `extensions::core::HookRegistry` | `src/extensions/core/hook_registry.rs` | Extension framework hooks (plugin lifecycle: tool execution, prompt injection) |

---

## Evidence

### 1. Two `RegisteredHook` structs with different fields

**`src/hooks/mod.rs` (lines 28–42):**
```rust
pub struct RegisteredHook {
    pub id: String,
    pub instance_id: String,
    pub hook_type: HookType,   // Cron | Webhook | Event | FileWatch
    pub action: HookAction,    // Run { message }
    pub session_target: SessionTarget,
    pub enabled: bool,
}
```

**`src/extensions/core/hook_registry.rs` (lines 18–40):**
```rust
pub struct RegisteredHook {
    pub id: HookId,
    pub extension_id: ExtensionId,
    pub point: HookPoint,      // ToolExecute | PromptSystemSection | AgentInit ...
    pub handler: Arc<dyn HookHandler>,
    pub priority: HookPriority,
    pub enabled: bool,
    pub tool_metadata: Option<ToolMetadata>,
}
```

### 2. Two `HookRegistry` structs with `register()` / `unregister()`

**`src/hooks/registry.rs` (lines 12–21):**
```rust
pub struct HookRegistry {
    hooks: Arc<RwLock<HashMap<String, RegisteredHook>>>,
    webhooks: Arc<RwLock<HashMap<(String, String), String>>>,
    event_hooks: Arc<RwLock<HashMap<String, Vec<String>>>>,
    file_watches: Arc<RwLock<HashMap<(String, String), String>>>,
}
```

**`src/extensions/core/hook_registry.rs` (lines 104–117):**
```rust
pub struct HookRegistry {
    hooks: RwLock<HashMap<HookId, RegisteredHook>>,
    hooks_by_point: RwLock<HashMap<String, Vec<HookId>>>,
    services: Arc<ExtensionServices>,
    globally_enabled: RwLock<bool>,
}
```

### 3. Overlapping type names

| Name | In `hooks` module | In `extensions` module |
|------|-------------------|------------------------|
| `RegisteredHook` | ✅ | ✅ |
| `HookRegistry` | ✅ | ✅ |
| `HookType` | ✅ (external trigger types) | — |
| `HookAction` | ✅ (`Run { message }`) | — |
| `HookPoint` | — | ✅ (extension lifecycle points) |
| `HookResult` | — | ✅ (`Continue` / `PassThrough` / `Handled` / `Replace` / `Error`) |

---

## Impact

1. **Developer confusion:** Importing `HookRegistry` is ambiguous without a module prefix. IDEs and grep results conflate the two.
2. **No clear boundary:** The `hooks` module manages external triggers (webhooks, cron, file watches). The `extensions` module manages plugin lifecycle hooks (tool execution, prompt injection). Both use the word "hook" but mean different things.
3. **Maintenance burden:** Any change to hook semantics must be evaluated against both systems, even though they are unrelated.
4. **Future migration risk:** The extensions system is the intended future, but `hooks` is still actively used for system events. There is no documented plan for convergence or deprecation.

---

## Root Cause

- The `hooks` module was introduced for **Milestone 8** (outbound hooks and system events) before the extension framework (ADR-017) existed.
- ADR-017 introduced its own `HookRegistry` without renaming or reconciling with the existing one.
- No architectural decision record documents the coexistence strategy.

---

## Proposed Resolution

### Option A: Rename the legacy system (Recommended — low risk)

Rename the `hooks` module types to use "trigger" or "event" terminology, reserving "hook" for the extension framework:

| Current | Proposed |
|---------|----------|
| `hooks::HookRegistry` | `hooks::TriggerRegistry` |
| `hooks::RegisteredHook` | `hooks::RegisteredTrigger` |
| `hooks::HookType` | `hooks::TriggerType` |
| `hooks::HookAction` | `hooks::TriggerAction` |

This is a pure rename with no behavioral change. It immediately eliminates the naming collision.

### Option B: Merge into a unified registry (High risk)

Create a single registry that can handle both external triggers and extension lifecycle hooks. This is architecturally cleaner but requires significant design work to reconcile the different semantics (instance-scoped external triggers vs. extension-scoped handlers).

### Option C: Document the boundary (Minimal effort)

Add module-level documentation and ADR amendment clearly stating:
- `hooks::*` = external event triggers (webhook, cron, file watch, event bus)
- `extensions::core::*` = extension lifecycle hooks (tool execution, prompt injection, agent init)

This does not fix the collision but reduces confusion.

---

## Acceptance Criteria

- [ ] A decision is recorded in an ADR or architecture doc.
- [ ] The naming collision is resolved (either by rename, merge, or namespace isolation).
- [ ] All imports, tests, and documentation are updated to reflect the chosen naming.
- [ ] A deprecation timeline is established if one system is intended to replace the other.

---

## Related

- `src/hooks/mod.rs`
- `src/hooks/registry.rs`
- `src/extensions/core/hook_registry.rs`
- ADR-017 (Extension Framework)
- Milestone 8: Outbound Hooks and System Events
