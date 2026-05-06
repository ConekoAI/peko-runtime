# Issue 016: Module Boundary Violation — `extension` depends on `tools`

**Severity:** High  
**Status:** Closed  
**Labels:** `architecture`, `module-boundaries`, `refactoring`  
**Closed Date:** 2026-05-06

---

## Problem Statement

`AGENTS.md` (Issue 015 / ADR-017) mandates:

> `src/extension/core/` must have **zero dependencies** on `crate::extensions`, `crate::mcp`, `crate::daemon`, or `crate::tools`.

Reality: `src/extension/` currently imports from `crate::tools` in **6 files** (7+ import sites), creating a **bidirectional dependency** between the generic extension framework and the built-in tool system.

| File | Imports from `tools` |
|------|----------------------|
| `src/extension/integration/tool_bridge.rs` | `Tool` |
| `src/extension/services/tool_execution.rs` | `ToolContext` |
| `src/extension/services/reserved_params.rs` | `ToolContext`, `ToolContextAdapter` |
| `src/extension/transport/async_router.rs` | `AbortSignal` |
| `src/extension/protocols/shared/proxy_utils.rs` | `ToolContext`, `ToolError` |
| `src/extension/async_exec/executor/types.rs` | `ToolResult` |
| `src/extension/async_exec/executor/executor.rs` | `ToolResult` |

Additionally, the dependency is **bidirectional**:

- `src/tools/core/context.rs:16` imports `ContextSource` from `crate::extension::protocols::shared::context_resolver`.
- `src/tools/builtin/task_management.rs` imports from `crate::extension::async_exec::executor`.

This circularity prevents `src/extension/` from being a true zero-dependency framework layer and blocks future extraction of the extension system into a standalone crate.

---

## Root Cause Analysis

### 1. Tool execution primitives live in the wrong layer

`ToolContext`, `ToolError`, `AbortSignal`, and `ToolResult` are **execution primitives** — they describe how a tool runs (abort signals, progress callbacks, timeout checks, structured results). These primitives are consumed by:

- The async executor framework (`extension::async_exec`)
- The reserved parameter resolver (`extension::services::reserved_params`)
- The proxy utilities (`extension::protocols::shared::proxy_utils`)
- The async router (`extension::transport::async_router`)

Because these primitives live in `tools::core`, the extension framework *must* import them, violating its zero-dependency contract.

### 2. `ToolResult` vs. `HookOutput` duality

The extension framework has its own result type (`HookOutput` in `extension::types::hook_io`), but the async executor uses `ToolResult` (from `tools::core::traits`). This forces the executor to depend on `tools` and creates semantic overlap:

- `ToolResult` — `{ success, data, error, metadata }`
- `HookOutput::Json(Value)` — generic JSON output from a hook

The async executor should speak the framework's native result language, not the tool layer's.

### 3. `ToolContextAdapter` is on the wrong side of the bridge

`ToolContextAdapter` implements `ContextSource` (an extension trait) for `ToolContext` (a tool type). It currently lives in `tools::core::context` because `ToolContext` lives there. But this means `extension::services::reserved_params` must import the adapter from `tools`, further tightening the coupling.

---

## Proposed Solution

> **Backward compatibility is NOT required.** The project is at v0.1.0 dev stage. We will perform a single breaking refactor.

### Phase 1: Move execution primitives into `extension::types`

Create a new module `src/extension/types/tool_exec.rs` and move the following **as-is** (renames only where noted):

| Current Location | New Location | Notes |
|------------------|--------------|-------|
| `tools::core::context::ToolContext` | `extension::types::ToolContext` | Keep all fields and methods |
| `tools::core::context::ToolError` | `extension::types::ToolError` | Keep enum variants |
| `tools::core::context::AbortSignal` | `extension::types::AbortSignal` | Keep all methods |
| `tools::core::traits::ToolResult` | `extension::types::ToolResult` | Keep struct shape |
| `tools::core::context::ToolContextAdapter` | `extension::protocols::shared::context_resolver::ToolContextAdapter` | Bridges `ToolContext` → `ContextSource`; now both sides are in `extension` |
| `tools::core::context::ToolWithContext` | `extension::types::ToolWithContext` | Marker trait |

Update `src/extension/types/mod.rs` to re-export these:

```rust
pub mod tool_exec;
pub use tool_exec::{AbortSignal, ToolContext, ToolError, ToolResult, ToolWithContext};
```

### Phase 2: Update `tools::core` to import from `extension::types`

`src/tools/core/mod.rs` becomes a thin re-export layer:

```rust
pub use crate::extension::types::{AbortSignal, ToolContext, ToolError, ToolResult, ToolWithContext};
pub mod traits;  // Tool trait stays here
```

`src/tools/core/context.rs` is **deleted** (or reduced to extension-specific re-exports if needed for incremental migration).

`src/tools/core/traits.rs` updates its `use` statements:

```rust
use crate::extension::types::{ToolContext, ToolError, ToolResult};
```

The `Tool` trait itself **stays in `tools::core::traits`** — it is the consumer-facing interface that built-in tools implement. The framework does not need to know about `Tool`; only `tools` and `extensions::builtin` do.

### Phase 3: Break the bidirectional loop

After Phase 1–2, verify the dependency graph:

```
extension::types ──► (no tools deps)
        │
        ▼
tools::core ───────► imports ToolContext, ToolResult from extension::types
        │
        ▼
tools::builtin ────► may import async_exec from extension (acceptable: tools depends on extension)
```

`src/tools/core/context.rs` previously imported `ContextSource` from `extension`. After the move, `ToolContextAdapter` lives in `extension`, so `tools::core` no longer needs `ContextSource` at all. The bidirectional edge is removed.

### Phase 4: Unify `ToolResult` with `HookOutput` (optional but recommended)

`ToolResult` is essentially `HookOutput::Json(Value)` with a success bit. We can either:

- **Option A (minimal):** Keep `ToolResult` in `extension::types` and add a `From<ToolResult> for HookOutput` impl.
- **Option B (cleaner):** Replace `ToolResult` usage in `async_exec` with `HookOutput`. The async executor returns `HookOutput::Json(result_value)` on success and `HookResult::Error(e)` on failure. This eliminates the `ToolResult` type entirely.

**Recommendation:** Option A for this issue (smaller blast radius). Option B can be a follow-up issue once the boundary is clean.

### Phase 5: Update all call sites

The following files need import path changes only (no logic changes):

- `src/extension/integration/tool_bridge.rs` — `use crate::tools::Tool;` stays (this is the one legitimate dependency: it implements `Tool` for an extension wrapper). **Wait** — this file implements `Tool` for `ExtensionAsyncTool`. If `Tool` stays in `tools`, this file is in `extension` and imports `Tool`. That is still a boundary violation.

**Correction for `tool_bridge.rs`:**

`ExtensionAsyncTool` implements the `Tool` trait. This is an **adapter from extension → tool world**. It belongs in `src/extensions/builtin/` (the built-in extension type implementation) or `src/tools/registry/`, not in `src/extension/integration/`. Move `ExtensionAsyncTool` to `src/extensions/builtin/tool_bridge.rs` or `src/tools/registry/extension_async_tool.rs`. After the move, `src/extension/integration/tool_bridge.rs` can be deleted.

Revised call-site list:

| File | Change |
|------|--------|
| `src/extension/services/tool_execution.rs` | `use crate::tools::ToolContext;` → `use crate::extension::types::ToolContext;` |
| `src/extension/services/reserved_params.rs` | `use crate::tools::...` → `use crate::extension::types::{ToolContext, ToolContextAdapter};` |
| `src/extension/transport/async_router.rs` | `use crate::tools::AbortSignal;` → `use crate::extension::types::AbortSignal;` |
| `src/extension/protocols/shared/proxy_utils.rs` | `use crate::tools::core::{ToolContext, ToolError};` → `use crate::extension::types::{ToolContext, ToolError};` |
| `src/extension/async_exec/executor/types.rs` | `use crate::tools::core::traits::ToolResult;` → `use crate::extension::types::ToolResult;` |
| `src/extension/async_exec/executor/executor.rs` | `use crate::tools::core::traits::ToolResult;` → `use crate::extension::types::ToolResult;` |
| `src/extension/integration/tool_bridge.rs` | **Moved** to `src/tools/registry/extension_async_tool.rs` ✅ |
| `src/tools/core/context.rs` | **Deleted** ✅ |
| `src/tools/core/traits.rs` | Update `use` to `extension::types` |
| `src/tools/core/mod.rs` | Re-export from `extension::types` |
| `src/tools/builtin/*` | Update `use` paths if they referenced `tools::core::context` directly |

---

## Acceptance Criteria

- [x] `cargo clippy --all-features` passes with zero errors.
- [x] `cargo test` passes.
- [x] `grep -r "use crate::tools" src/extension/` returns **zero matches** (excluding comments).
- [x] `grep -r "use crate::extension" src/tools/core/` returns **zero matches** outside of `extension::types` (the bidirectional loop is broken).
- [x] `AGENTS.md` is updated to document the new module boundary: `tools::core` depends on `extension::types` for execution primitives; `extension` has zero deps on `tools`.
- [x] `src/tools/core/context.rs` deleted.
- [x] `ExtensionAsyncTool` moved from `src/extension/integration/tool_bridge.rs` to `src/tools/registry/extension_async_tool.rs`.
- [x] `From<ToolResult> for HookOutput` implemented (Phase 4, Option A).

---

## Future-Proofing Notes

1. **Standalone crate extraction:** Once this boundary is clean, `src/extension/` could theoretically be extracted into a `pekobot-extension-framework` crate with zero internal pekobot dependencies. This is the end goal of ADR-017.

2. **Third-party tool SDKs:** External tool authors should eventually only depend on `extension::types` (the execution primitives) and `tools::core::Tool` (the trait). If we publish the extension framework as a crate, tool authors get lightweight dependencies.

3. **HookOutput unification:** A follow-up issue should evaluate whether `ToolResult` can be fully subsumed by `HookOutput`, eliminating one more type from the system.

---

## Related

- ADR-017 (Extension Framework Architecture)
- Issue 015 (Type-oriented module restructure)
- `AGENTS.md` — Module Boundaries section
