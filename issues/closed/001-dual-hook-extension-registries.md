# Issue Dual Hook/Extension Registries — Competing Abstractions

**Severity:** CRITICAL  
**Status:** ✅ **Closed / Archived**  
**Labels:** `architecture`, `naming-collision`, `adr-017`, `milestone-8`, `refactor`  
**Reported:** 2026-04-21  
**Resolved:** 2026-04-21  

---

## Summary

The codebase contained two entirely different `HookRegistry` types with overlapping names but completely different semantics. The legacy `hooks` module (Milestone 8 external triggers) was found to be entirely orphaned — instantiated but never wired into the running system. It has been removed. The extension framework's `HookRegistry` (ADR-017) is now the sole registry.

---

## Resolution

### Decision

**Option D — Deprecate and remove the orphaned legacy system.**

The Milestone 8 `hooks/` machinery was a sophisticated skeleton that was never wired up. The real event infrastructure (`cron::events::SystemEvent`, `session::events::SystemEvent`) evolved separately. There was no value in preserving, renaming, or merging dead code.

### What Was Removed

| File | Lines | What It Was |
|------|-------|-------------|
| `src/hooks/registry.rs` | 478 | `HookRegistry` for external triggers |
| `src/hooks/trigger.rs` | 356 | `HookTrigger`, `TriggerSource`, `HookTriggerProcessor` |
| `src/hooks/event_bus.rs` | 374 | `EventBusHookIntegration` |
| `src/hooks/file_watch.rs` | 320 | `FileWatchHookManager` |
| `src/hooks/lifecycle.rs` | 339 | `LifecycleEmitter` |
| `src/hooks/mod.rs` | 453 | Module root + event types (`SystemEvent`, etc.) |
| `src/system_events.rs` | — | *Created then deleted* — extracted event taxonomy that nobody emitted or consumed |

### What Was Modified

- **`src/daemon/state.rs`** — Removed dead `hook_registry` and `event_broadcaster` fields/methods
- **`src/image/config.rs`** — Renamed `Hook` → `Trigger`, `HookType` → `TriggerType` for config-serialization types
- **`src/image/mod.rs`** — Updated re-export
- **`src/extensions/types.rs`** — Removed `HookOutput::Event` and `HookInput::SystemEvent` variants (bridge to nowhere)
- **`src/extensions/core/context.rs`** — Removed `as_system_event()` accessor
- **`src/extensions/adapters/gateway_adapter.rs`** — Removed `SystemEvent` passthrough arm
- **`src/extensions/adapters/general_adapter.rs`** — Removed `SystemEvent` passthrough arm
- **`src/lib.rs`** — Removed `hooks` and `system_events` module declarations

### Verification

- `cargo check` ✅ — compiles cleanly
- `cargo test` ✅ — **834 passed, 0 failed, 23 ignored**
- Net change: **−2,422 lines** of dead code removed

---

## Aftermath

### What remains

| Component | Location | Status |
|-----------|----------|--------|
| `extensions::core::HookRegistry` | `src/extensions/core/hook_registry.rs` | ✅ **Sole `HookRegistry`** — actively used for tool registration, prompt injection, etc. |
| `cron::events::SystemEvent` | `src/cron/events.rs` | ✅ **Active event system** — File/Webhook/Internal/Timer for scheduler |
| `session::events::SystemEvent` | `src/session/events.rs` | ✅ **Active event system** — Session event stream |
| `image::config::Trigger` / `TriggerType` | `src/image/config.rs` | ✅ **Config types** — external trigger declarations in `config.toml` |

### If external triggers are needed in the future

Rebuild them as **ADR-017 extensions** using the existing `HookPoint` taxonomy:
- `EventSubscribe { topic_pattern }` — subscribe to system events
- `EventEmit` — emit custom events
- `ChannelInput` / `ChannelOutput` — gateway-style I/O

Do not reintroduce a parallel trigger registry.

---

## Related

- ADR-017 (Extension Framework)
- Milestone 8: Outbound Hooks and System Events (deprecated)
- `src/extensions/core/hook_registry.rs`
- `src/extensions/core/registry.rs`
- `src/cron/events.rs`
- `src/session/events.rs`
- `src/image/config.rs`
