# Break dependency cycles 4 and 5

**Date:** 2026-06-22
**Branch:** `refactor/clippy-cleanup-rust196` (existing — do not push PR)
**Status:** Design — awaiting user approval

---

## Background

PR #62 (`refactor/clippy-cleanup-rust196`) claims to break 5 dependency cycles identified in `PLAN.md §2.5`. Review found that two cycles (4 and 5) are reshuffled, not broken:

- **Cycle 4 — `tools::core ↔ extension::types`:** `src/tools/core/traits.rs:3` imports `ToolContext, ToolError` from `crate::extensions::framework::types`, while extension adapters (e.g. `extensions/builtin`, `extensions/mcp`) import the `Tool` trait from `crate::tools::core`. The bidirectional dependency is real.
- **Cycle 5 — `tunnel ↔ agents`:** `src/tunnel/a2a_send_tool.rs:36` imports `StatelessAgentService` (concrete type) from `agents::stateless_service`, while `src/agents/agent.rs:138,140` imports `A2aSendTool` to register it with each agent.

This spec defines a refactor that actually breaks both cycles.

---

## Goal

After this refactor lands:

1. `extensions::framework` depends on `tools::core` (one-way, only).
2. `tunnel` defines a trait that `agents::StatelessAgentService` implements; `tunnel` no longer imports the concrete `agents::stateless_service` type.
3. `agents` calls a `tunnel`-side factory to construct the `a2a_send` tool.
4. `bash scripts/check_module_boundaries.sh` passes (after the script itself is fixed to scan the new paths).

## Non-goals

- No public Rust API change. None of these types are re-exported from `lib.rs`.
- No CLI surface change.
- No wire-format change.
- No persistence change.
- Cycle 1 (`portable ↔ identity`), 2 (`tools ↔ tunnel` surface), and 3 (`extension::types ↔ engine`) are already broken per PR #62 and are not in scope.

---

## Design — Cycle 4

### What moves

| From | To | Notes |
|---|---|---|
| `src/extensions/framework/types/tool_exec.rs` (7 types: `ToolProgressEvent`, `ToolError`, `ToolContext`, `AbortSignal`, `ToolContextAdapter`, `ToolWithContext`, `ToolResult`) | `src/tools/core/exec.rs` | New file. All 7 types land here. Tests move with them (lines 571-696 of the original). |
| `src/extensions/framework/protocols/shared/context_resolver.rs` (the `ContextSource` trait) | `src/tools/core/context_source.rs` | The trait moves to `tools/core` so extension-framework-side adapters can impl it without depending on `tools::core` for the trait itself. |
| The `impl ContextSource for ToolContextAdapter<'_>` (currently in `tool_exec.rs:462-482`) | `src/tools/core/context_source.rs` | Travels with the trait. |
| The blanked-impl comment `// impl<T: Tool> ToolWithContext for T {}` (currently a comment in `tool_exec.rs:497-502`) | The actual impl in `src/tools/core/exec.rs` | The cycle was the only thing preventing this impl. Once the cycle is broken, the impl lands. |

### Re-export strategy

`src/extensions/framework/types/mod.rs` keeps re-exports for one commit:

```rust
// Re-exported from tools::core for backward compatibility within the tree.
// Remove in the follow-up commit; no external callers exist (these are pub
// but not re-exported from lib.rs).
#[allow(deprecated)]
pub use crate::tools::core::exec::{
    AbortSignal, ToolContext, ToolContextAdapter, ToolError, ToolResult, ToolWithContext,
};
#[allow(deprecated)]
pub use crate::tools::core::context_source::ContextSource;
```

The follow-up commit deletes these re-exports.

### Affected import sites (cycle 4)

These files import `ToolContext`/`ToolError`/`ToolResult`/`AbortSignal`/`ToolWithContext`/`ContextSource` from `crate::extensions::framework::types` (or `crate::extensions::framework::protocols::shared::context_resolver`) and will be updated to import from `crate::tools::core::{exec, context_source}`:

- `src/tools/core/traits.rs` (the cycle site)
- `src/extensions/builtin/adapter.rs`
- `src/extensions/gateway/adapter.rs`
- `src/extensions/general/adapter.rs`
- `src/extensions/mcp/protocol/types.rs` (if it imports these)
- `src/extensions/skill/adapter.rs`
- `src/extensions/universal/adapter.rs`
- `src/extensions/framework/services/reserved_params.rs`
- `src/extensions/framework/services/tool_execution.rs`
- `src/extensions/framework/protocols/shared/schema_filter.rs` (if applicable)
- Any test file that imports the primitives from `extensions::framework::types`

(The exact list is produced by `grep -rln 'crate::extensions::framework::\(types\|protocols/shared/context_resolver\)' src/ tests/` after the move.)

### Affected import sites (cycle 5 type move)

All callers of `agents::stateless_service::MessageRequest` and `agents::stateless_service::MessageResult` continue to work via the one-commit re-export shim. In the follow-up commit, callers update to `crate::tunnel::a2a_message_types::{A2aMessageRequest, A2aMessageResponse}`. Likely callers include:

- `src/agents/agent_service.rs`
- `src/agents/announcement_service.rs`
- `src/agents/subagent_*.rs`
- `src/agents/stateless_service.rs` (internal)
- `src/agents/tests/subagent_integration_tests.rs`
- `src/engine/agentic_loop.rs`
- `src/session/manager.rs` (if it constructs `MessageRequest`)

(The exact list is produced by `grep -rln 'agents::stateless_service::\(MessageRequest\|MessageResult\)' src/ tests/` before the move.)

---

## Design — Cycle 5

### New trait in `tunnel::a2a_send_tool`

```rust
/// Minimum interface `A2aSendTool` needs from a peer agent service.
///
/// Lives in `tunnel` (not `agents`) so `A2aSendTool::new` can take a
/// trait object without importing the concrete `StatelessAgentService`
/// type. Implementations convert the tunnel-owned request/response
/// types to whatever internal shape they use.
///
/// Breaking cycle 5 (per PLAN §2.5).
#[async_trait::async_trait]
pub trait AgentMessageService: Send + Sync {
    async fn execute_message(
        &self,
        req: A2aMessageRequest,
    ) -> Result<A2aMessageResponse>;
}
```

### Type move: `MessageRequest`/`MessageResult` → `tunnel::a2a_message_types`

The trait method signatures use request and response types. Those types must live somewhere that `tunnel` can import without depending on `agents`. Move them to a new `tunnel::a2a_message_types` module:

| Was | Becomes |
|---|---|
| `agents::stateless_service::MessageRequest` | `tunnel::a2a_message_types::A2aMessageRequest` |
| `agents::stateless_service::MessageResult` | `tunnel::a2a_message_types::A2aMessageResponse` |

The moved types are re-exported from `agents::stateless_service` for one commit (compat shim), then deleted. Internal call sites within `agents` (e.g. `agents::stateless_service::execute_message`'s callers) keep using the local name via the re-export during the in-flight commit, then update to the new path.

`StatelessAgentService::execute_message` keeps its public signature unchanged: callers within `agents` pass `MessageRequest`/`MessageResult` (the local re-exports, which are aliases for the new tunnel types). The trait impl for `AgentMessageService` does the type-conversion (a field-by-field copy, since the two type pairs have identical fields — this is a pure rename, not a redesign).

**Why this is not a redesign:** `MessageRequest` and `MessageResult` are pure data structs (no methods on them that need agents-side state). The only reason they live in `agents` is historical. Moving them to `tunnel` is a mechanical rename + path change.

### New factory function in `tunnel::a2a_send_tool`

```rust
/// Construct an `Arc<dyn Tool>` for the `a2a_send` capability.
///
/// Replaces direct `A2aSendTool::new(...)` calls in callers that
/// don't want to depend on the concrete `A2aSendTool` type. The
/// caller wires the service as `Arc<dyn AgentMessageService>`; the
/// factory wires the `caller` annotation, optional DID, and optional
/// cross-runtime context.
#[must_use]
pub fn build_tool(
    service: Arc<dyn AgentMessageService>,
    caller: Option<&str>,
    caller_did: Option<&str>,
    cross_runtime: Option<Arc<CrossRuntimeA2aCtx>>,
) -> Arc<dyn Tool> {
    let mut tool = A2aSendTool::new(service);
    if let Some(did) = caller_did {
        if let Some(name) = caller {
            tool = tool.with_caller_did(name, did);
        }
    } else if let Some(name) = caller {
        tool = tool.with_caller(name);
    }
    if let Some(ctx) = cross_runtime {
        tool = tool.with_cross_runtime(ctx);
    }
    Arc::new(tool)
}
```

### `A2aSendTool::new` signature change

```rust
// Before
pub fn new(agent_service: Arc<StatelessAgentService>) -> Self { ... }

// After
pub fn new(agent_service: Arc<dyn AgentMessageService>) -> Self { ... }
```

This is the cycle-breaking change. `tunnel::a2a_send_tool` no longer references `StatelessAgentService` at all.

### `impl AgentMessageService for StatelessAgentService`

In `src/agents/stateless_service.rs`:

```rust
#[async_trait::async_trait]
impl crate::tunnel::a2a_send_tool::AgentMessageService for StatelessAgentService {
    async fn execute_message(
        &self,
        req: A2aMessageRequest,
    ) -> Result<A2aMessageResponse> {
        // `MessageRequest`/`MessageResult` are re-export aliases
        // for `A2aMessageRequest`/`A2aMessageResponse` (commit 4
        // adds the shim; commit 5 removes it and this impl then
        // takes A2aMessageRequest directly with no alias dance).
        let local_resp = Self::execute_message(self, req).await?;
        Ok(local_resp)
    }
}
```

`agents` now imports the trait from `tunnel`. This is the new one-way dependency: `agents → tunnel`. The shim makes the type identity explicit (no real field-by-field copy needed) and gives a clean migration path: in commit 5 the local aliases vanish and the impl is unchanged at the source level.

### `agents::agent.rs::init_builtins_async` change

```rust
// Before (lines 138, 140):
let mut a2a = match self.config.agent_did.as_deref() {
    Some(did) if !did.is_empty() =>
        crate::tunnel::a2a_send_tool::A2aSendTool::new(agent_service)
            .with_caller_did(&self.config.name, did),
    _ =>
        crate::tunnel::a2a_send_tool::A2aSendTool::new(agent_service)
            .with_caller(&self.config.name),
};
if let Some(ctx) = cross_ctx {
    a2a = a2a.with_cross_runtime(ctx);
}
tools.push(Arc::new(a2a));

// After:
let dyn_service: Arc<dyn crate::tunnel::a2a_send_tool::AgentMessageService> =
    agent_service;
let tool = crate::tunnel::a2a_send_tool::build_tool(
    dyn_service,
    Some(&self.config.name),
    self.config.agent_did.as_deref(),
    cross_ctx.as_ref().map(Arc::clone),
);
tools.push(tool);
```

### Net dependency graph

After this refactor:
- `tools::core` — no deps on `extensions::framework` or `tunnel`
- `extensions::framework` → `tools::core` (one-way, for `Tool`, `ToolContext`, `ContextSource`)
- `tunnel` → `tools::core` (already true, for `Tool`)
- `agents` → `tunnel` (one-way, for `AgentMessageService` trait)
- `agents` → `tools::core` (already true, for `Tool`)

No cycles.

---

## Data flow

### Cycle 4 — reserved parameter injection (unchanged at runtime)

```
ExtensionCore hook invocation
  ↓
extensions/framework/services/reserved_params.rs
  ↓ constructs
ToolRuntimeContext (in extensions/framework/types/mod.rs:88) — unchanged
  ↓ uses
ContextSource trait (now imported from tools::core::context_source)
  ↓ implemented for
ToolContext (now in tools::core::exec)
  ↓
Adapter: ToolContextAdapter<'_> (now in tools::core::context_source)
```

The runtime behavior is identical. Only the import paths change.

### Cycle 5 — a2a_send execution (unchanged at runtime)

```
A2aSendTool::execute_with_context(params, ctx)
  ↓
existing internal logic in a2a_send_tool.rs::A2aSendTool::execute
  ↓
self.agent_service.execute_message(req).await
  ↓ (dyn-dispatches)
impl AgentMessageService for StatelessAgentService::execute_message(req)
  ↓ (delegates)
existing internal logic in agents::stateless_service.rs::execute_message
  ↓
unchanged from current behavior
```

---

## Error handling

No new error paths. The trait method signature matches the existing `StatelessAgentService::execute_message` signature exactly; the trait impl is a pure delegation. The `build_tool` factory is infallible (construction cannot fail). If a future variant needs to fail construction, the signature becomes `Result<Arc<dyn Tool>>` and the impl returns `Ok(...)` for now.

---

## Testing

### Tests that move with the code

`src/extensions/framework/types/tool_exec.rs` lines 571-696 contain 6 tests:
- `test_abort_signal`
- `test_tool_error_display`
- `test_progress_throttling`
- `test_timeout`
- `test_progress_reporting`
- `test_tool_result_success/failure/to_json`

These move with the primitives to `src/tools/core/exec.rs` and run from the new location. No test logic changes.

### New test for the blanket impl

`src/tools/core/exec.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::core::Tool;

    /// Compile-time + runtime check that the blanket impl works.
    /// A `&dyn Tool` is also `&dyn ToolWithContext`.
    fn assert_blanket_impl_works(t: &dyn Tool) -> &dyn ToolWithContext {
        t
    }

    #[test]
    fn test_blanket_impl_compiles() {
        // The function above is the test. If it compiles, the blanket
        // impl exists. No runtime assertion needed.
    }
}
```

### New test for `build_tool`

`src/tunnel/a2a_send_tool.rs`:

```rust
#[cfg(test)]
mod build_tool_tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct MockService {
        calls: Mutex<Vec<MessageRequest>>,
    }

    #[async_trait]
    impl AgentMessageService for MockService {
        async fn execute_message(&self, req: A2aMessageRequest) -> Result<A2aMessageResponse> {
            self.calls.lock().unwrap().push(req);
            Ok(A2aMessageResponse {
                response: String::new(),
                session_id: String::new(),
                iterations: None,
                tool_calls: None,
                duration_ms: None,
                error: None,
            })
        }
    }

    #[tokio::test]
    async fn test_build_tool_wires_caller_and_did() {
        let service = Arc::new(MockService { calls: Mutex::new(vec![]) });
        let tool = build_tool(
            service.clone(),
            Some("agent-a"),
            Some("did:peko:agent-a"),
            None,
        );
        assert_eq!(tool.name(), "a2a_send");
        // Inspect the internal state via a debug helper or by calling
        // execute and checking that MockService.calls got the right req.
    }
}
```

### Existing tests that must keep passing

- All 1522 unit tests in `cargo test --lib` per the PR body.
- `src/tunnel/a2a_send_tool.rs` line 1356 test (uses `agents::stateless_service::StatelessAgentService` directly via `#[cfg(test)]` import — this is allowed; tests are not part of the cycle).
- `src/agents/tests/subagent_integration_tests.rs` (exercises the agent tool-registration path).

### CI harness

`scripts/check_module_boundaries.sh` is currently a no-op (per PR review finding 2) because it greps `src/extension/` which no longer exists. Update it to scan `src/extensions/framework/`. After the update:

- **Rule 1** (framework → extensions forbidden) — scans `src/extensions/framework/` for `use crate::extensions::` (excluding the framework's own subdirs). Must pass.
- **Rule 2** (cross-extension) — unchanged.
- **Rule 3** (framework core must not depend on daemon or tools) — scans `src/extensions/framework/core/` for `use crate::tools::` and `use crate::daemon::`. **This rule was previously meant to keep framework independent of tools. After this refactor, the rule must be relaxed: framework may import from `tools::core` (one-way).** Update Rule 3 to scan only `src/extensions/framework/core/` for `use crate::daemon::` (keep the tools prohibition for the *core* subdir; the rest of the framework can use `tools::core`).

---

## Commit plan

All commits on `refactor/clippy-cleanup-rust196`. Do not push. Per project policy, do not open a PR.

1. **`refactor(tools): move execution primitives to tools::core`** — cycle 4 part 1. Create `src/tools/core/exec.rs` and `src/tools/core/context_source.rs` with the types and trait. Add intra-crate re-exports in `extensions::framework::types::mod.rs` and `extensions::framework::protocols::shared::mod.rs` for in-flight compat. Update `src/tools/core/traits.rs:3` to import from `tools::core::exec`. Update all extension adapters. `cargo check` and `cargo test --lib` pass. Re-exports keep the old paths working.

2. **`refactor(tools): remove extensions::framework re-exports of moved primitives`** — cycle 4 part 2. Delete the re-exports. Add the `impl<T: Tool> ToolWithContext for T {}` blanket. Update any remaining callers. `cargo check` and `cargo test --lib` and `cargo clippy --all-targets -- -D warnings` all pass. **Cycle 4 is now broken.**

3. **`fix(ci): update check_module_boundaries.sh for extensions/framework path`** — update the script to scan the new path. Relax Rule 3 to only check `daemon` (not `tools`) in the framework core. Run the script; must pass on the current tree.

4. **`refactor(tunnel): introduce AgentMessageService trait + build_tool factory; move MessageRequest/MessageResult to tunnel::a2a_message_types`** — cycle 5 part 1. Create `src/tunnel/a2a_message_types.rs` with `A2aMessageRequest` and `A2aMessageResponse`. Add the trait and factory in `tunnel::a2a_send_tool`. Change `A2aSendTool::new` to take `Arc<dyn AgentMessageService>`. Add a one-commit re-export shim in `agents::stateless_service` so internal callers keep compiling. Update the test on line 1356 to wrap `StatelessAgentService` as `Arc<dyn AgentMessageService>`. `cargo test --lib` passes; the public API of `A2aSendTool` is technically broken (changed signature) but the only caller is `agents::agent.rs` which we update in the next commit.

5. **`refactor(agents): impl AgentMessageService for StatelessAgentService; route construction through build_tool; remove re-export shim`** — cycle 5 part 2. Add the trait impl (with type conversion at the boundary). Replace the `A2aSendTool::new(...)` calls in `agents::agent.rs:138,140` with the factory. Remove the `MessageRequest`/`MessageResult` re-export shim from `agents::stateless_service`. Update internal callers to use the new path. `cargo check`, `cargo test --lib`, and `cargo clippy --all-targets -- -D warnings` all pass. **Cycle 5 is now broken.**

6. **`docs: update AGENTS.md and CHANGES.md`** — note that cycles 4 and 5 are actually broken (not reshuffled), and document the new dependency direction `extensions::framework → tools::core`.

---

## Verification (must all pass before declaring done)

```bash
cargo check --lib --tests
cargo clippy --all-targets -- -D warnings
cargo test --lib
bash scripts/check_module_boundaries.sh

# Cycle verification — these greps should return ZERO matches:
grep -rn 'use crate::extensions::framework::types::\(ToolContext\|ToolError\|ToolResult\|AbortSignal\|ToolWithContext\|ToolProgressEvent\|ToolContextAdapter\)' src/ tests/
grep -rn 'use crate::agents::stateless_service::StatelessAgentService' src/tunnel/ src/extensions/
grep -rn 'A2aSendTool::new' src/agents/  # should be empty after commit 5
```

The last three greps are the literal proof that the cycles are broken.

---

## Risks

- **Commit 4 breaks the public Rust API of `A2aSendTool::new`.** No external caller exists (the function is `pub` but not re-exported). Risk is contained.
- **Commit 4 also moves `MessageRequest`/`MessageResult` to a new `tunnel::a2a_message_types` module.** Callers that import the types by full path break; the re-export shim in commit 4 keeps them compiling, the follow-up removes the shim and they update. Risk is contained if the grep in commit 4 is run before commit 4 lands.
- **The blanket `impl<T: Tool> ToolWithContext for T` might conflict with future impls** if someone tries to add a specialized impl. This is a minor risk; the orphan-rule and coherence rules prevent most conflicts. The trait `ToolWithContext` has a default method `supports_progress` returning `true`; this differs from `Tool::supports_progress` (returns `false` by default), which is intentional per the comment in `tool_exec.rs:120-122`.
- **`check_module_boundaries.sh` rule relaxation** — relaxing Rule 3 to allow `tools::core` imports in framework could let a future PR re-introduce a deeper cycle (e.g. framework → `tools::builtin::messaging` which pulls in agents). Mitigation: the relaxed Rule 3 keeps the `crate::tools::builtin` prohibition (so the framework cannot pull in any built-in tool implementation, only the trait) and only allows `crate::tools::core`. Rule 4 (new) bans `use crate::agents::`, `use crate::tunnel::`, and `use crate::daemon::` from `extensions/framework/` entirely.
