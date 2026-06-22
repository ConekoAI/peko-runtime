# Break Dependency Cycles 4 and 5 — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Break the two dependency cycles that PR #62 reshuffled but did not actually break: cycle 4 (`tools::core ↔ extension::framework::types`) and cycle 5 (`tunnel ↔ agents`).

**Architecture:**
- **Cycle 4 fix:** Move 7 tool execution primitives + the `ContextSource` trait + `ToolContextAdapter` from `extensions/framework/types/` and `extensions/framework/protocols/shared/context_resolver.rs` into `tools::core/`. After the move, `extensions::framework` depends on `tools::core` (one-way).
- **Cycle 5 fix:** Introduce a `AgentMessageService` trait in `tunnel::a2a_send_tool`; move `MessageRequest`/`MessageResult` to a new `tunnel::a2a_message_types` module; add a `build_tool` factory in `tunnel::a2a_send_tool`. `agents::StatelessAgentService` impls the trait; `agents::agent.rs` calls the factory. Net dependency: `agents → tunnel` (one-way).

**Tech Stack:** Rust 2021, async-trait, tokio. Branch: `refactor/clippy-cleanup-rust196` (existing). No PR push per project policy.

**Spec:** `docs/superpowers/specs/2026-06-22-cycle-break-design.md` — read this first.

## Global Constraints

- All commits end with: `cargo check --lib --tests && cargo clippy --all-targets -- -D warnings && cargo test --lib` all green.
- Commit 3 must make `bash scripts/check_module_boundaries.sh` scan `src/extensions/framework/` instead of the obsolete `src/extension/` path. After commit 3, the script must PASS on the current tree.
- The boundary lint, after the cycle breaks, must produce zero violations for the relaxed Rule 3 (framework may import from `tools::core` only; never from `tools::builtin`, `tools::factory`, etc.).
- No public API change beyond what the spec lists (`A2aSendTool::new` signature change; `MessageRequest`/`MessageResult` location change; primitive type locations change).
- No new dependencies. No new files in `Cargo.toml`.
- The 6 commits in §"Commit Plan" of the spec are the required commit boundaries. Do not squash.

## File Structure

### New files

- `src/tools/core/exec.rs` — 7 primitives + tests (moved from `extensions/framework/types/tool_exec.rs:1-696`).
- `src/tools/core/context_source.rs` — `ContextSource` trait + `ToolContextAdapter` impl (moved from `extensions/framework/protocols/shared/context_resolver.rs:1-100`).
- `src/tunnel/a2a_message_types.rs` — `A2aMessageRequest` and `A2aMessageResponse` (moved from `agents/stateless_service.rs:156,316`).

### Deleted (in commit 2 and commit 5)

- `src/extensions/framework/types/tool_exec.rs` — content moved to `tools/core/exec.rs`; re-export shim lives in `types/mod.rs` only.
- `src/agents/stateless_service.rs` re-export shim for `MessageRequest`/`MessageResult` (commit 5).

### Modified (full list, with new import path)

- `src/tools/core/mod.rs` — add `pub mod exec; pub mod context_source;`
- `src/tools/core/traits.rs` — change `use crate::extensions::framework::types::{ToolContext, ToolError};` to `use crate::tools::core::exec::{ToolContext, ToolError};`
- `src/extensions/framework/types/mod.rs` — add (commit 1) and then remove (commit 2) re-exports for the 6 primitives + `ContextSource`.
- `src/extensions/framework/protocols/shared/mod.rs` — add (commit 1) and then remove (commit 2) re-export of `ContextSource`.
- `src/extensions/framework/services/reserved_params.rs:154` — `ToolContextAdapter` import path.
- `src/extensions/framework/services/tool_execution.rs:43` — `ToolContext` import path.
- `src/extensions/framework/async_exec/executor/types.rs:3` — `ToolResult` import path.
- `src/extensions/framework/async_exec/executor/executor.rs:10` — `ToolResult` import path.
- `src/extensions/framework/core/*` (12 files in the cycle-4 grep result) — many import `ContextSource` from `protocols::shared::context_resolver`. Update each to import from `tools::core::context_source` after commit 2.
- `src/extensions/framework/transport/async_router.rs`, `manager/storage.rs`, `manager/packaging.rs`, `manager/mod.rs`, `adapters/mod.rs`, `types/tool.rs`, `types/hook_io.rs`, `types/manifest.rs`, `core/*` — same pattern. Each gets one import-path edit.
- `src/extensions/skill/adapter.rs` — update primitive import paths.
- `src/extensions/universal/adapter.rs` — update primitive import paths.
- `src/tunnel/mod.rs` — add `pub mod a2a_message_types;`
- `src/tunnel/a2a_send_tool.rs` — add `AgentMessageService` trait, `build_tool` factory; change `A2aSendTool::new` to take `Arc<dyn AgentMessageService>`; remove `use crate::agents::stateless_service::{MessageRequest, StatelessAgentService}`; add `use crate::tunnel::a2a_message_types::{A2aMessageRequest, A2aMessageResponse}`.
- `src/tunnel/dispatcher.rs` and `src/ipc/server.rs` — update import of `MessageRequest`/`MessageResult` from agents to tunnel.
- `src/agents/stateless_service.rs` — add `impl AgentMessageService for StatelessAgentService`; add (commit 4) and then remove (commit 5) re-export shim for `MessageRequest`/`MessageResult`; rename the type definitions to be `pub use crate::tunnel::a2a_message_types::{A2aMessageRequest as MessageRequest, A2aMessageResponse as MessageResult};` in commit 5.
- `src/agents/agent.rs:138,140` — replace `A2aSendTool::new(agent_service).with_caller_did(...)` and `A2aSendTool::new(agent_service).with_caller(...)` with `tunnel::a2a_send_tool::build_tool(...)`.
- `src/daemon/state.rs`, `src/daemon/mod.rs`, `src/daemon/background_runtime/starter.rs` — these import `StatelessAgentService` for purposes unrelated to `A2aSendTool`. No change needed (they use the concrete type, not the trait). Confirmed via grep.
- `src/tunnel/a2a_e2e_tests.rs:236` — test-only `use crate::agents::stateless_service::StatelessAgentService;` allowed (tests are not part of the cycle). Update if signature changes break it.
- `src/tunnel/a2a_send_tool.rs:1356` — test-only import. Update to wrap as `Arc<dyn AgentMessageService>` (the test instantiates `A2aSendTool::new(...)` directly; the new signature requires the trait object).
- `scripts/check_module_boundaries.sh` — update paths from `src/extension/` to `src/extensions/framework/`. Relax Rule 3 to allow `crate::tools::core` imports in framework (forbid `crate::tools::builtin` and other sub-modules). Add new Rule 4 banning `crate::agents::`, `crate::tunnel::`, `crate::daemon::` imports from `extensions/framework/`.
- `AGENTS.md` — note that cycles 4 and 5 are now actually broken; document the new dependency direction `extensions::framework → tools::core`.
- `CHANGES.md` — add entry under the current version.

---

## Task 1: Move execution primitives to `tools::core` (cycle 4, part 1)

**Files:**
- Create: `src/tools/core/exec.rs`
- Create: `src/tools/core/context_source.rs`
- Modify: `src/tools/core/mod.rs`
- Modify: `src/tools/core/traits.rs:1-10`
- Modify: `src/extensions/framework/types/mod.rs` (add re-exports)
- Modify: `src/extensions/framework/protocols/shared/mod.rs` (add re-export)

**Goal:** Move the 7 primitives and the `ContextSource` trait into `tools::core`. Keep re-exports in the old locations so the build stays green.

- [ ] **Step 1.1: Create `src/tools/core/exec.rs`**

  Copy the full content of `src/extensions/framework/types/tool_exec.rs:1-696` into `src/tools/core/exec.rs`. Then edit the file's `use` statements:
  - Remove the `use crate::extensions::framework::protocols::shared::context_resolver::ContextSource;` import (line 16).
  - Add `use super::context_source::ContextSource;` at the top.
  - Remove the `impl From<ToolResult> for crate::extensions::framework::types::HookOutput` (line 565) — change to `impl From<ToolResult> for crate::extensions::framework::types::HookOutput` becomes `impl From<ToolResult> for crate::extensions::framework::hook_io::HookOutput`. (If `HookOutput` is in `hook_io`, import it as `use crate::extensions::framework::types::HookOutput;` and add a `#[allow(unused_imports)]` on it for now — commit 2 cleans this up.)
  - Leave the `impl ContextSource for ToolContextAdapter<'_>` block (lines 462-482) where it is; it now uses the local `ContextSource` from `super::context_source`.

- [ ] **Step 1.2: Create `src/tools/core/context_source.rs`**

  Copy `src/extensions/framework/protocols/shared/context_resolver.rs:1-100` into `src/tools/core/context_source.rs`. Rename the file's `use` statements to drop the `crate::extensions::framework::protocols::shared::context_resolver` prefix (it's now self-referential). The `resolve_field` function and `ToContextValue` impl stay unchanged. The `MockContext` test impl stays. **Do not** move the `ToolContextAdapter` — that's in `exec.rs`.

- [ ] **Step 1.3: Update `src/tools/core/mod.rs`**

  Add at the end of the file:
  ```rust
  pub mod context_source;
  pub mod exec;
  ```

  Re-export the public primitives from the new modules so `crate::tools::core::ToolContext` works:
  ```rust
  pub use context_source::ContextSource;
  pub use exec::{AbortSignal, ToolContext, ToolContextAdapter, ToolError, ToolProgressEvent, ToolResult, ToolWithContext};
  ```

- [ ] **Step 1.4: Update `src/tools/core/traits.rs:3`**

  Change:
  ```rust
  use crate::extensions::framework::types::{ToolContext, ToolError};
  ```
  to:
  ```rust
  use crate::tools::core::exec::{ToolContext, ToolError};
  ```
  (The import is now intra-crate; the cycle is gone for this file.)

- [ ] **Step 1.5: Add re-exports in `src/extensions/framework/types/mod.rs`**

  Add at the end (above the `#[cfg(test)] mod tests` block):
  ```rust
  // Re-exports for in-flight compat while the execution primitives
  // are migrated to tools::core. Removed in the follow-up commit.
  #[allow(deprecated)]
  pub use crate::tools::core::exec::{
      AbortSignal, ToolContext, ToolContextAdapter, ToolError, ToolProgressEvent, ToolResult,
      ToolWithContext,
  };
  ```

- [ ] **Step 1.6: Add re-export in `src/extensions/framework/protocols/shared/mod.rs`**

  Find the `pub mod` declarations and add:
  ```rust
  pub use crate::tools::core::context_source::ContextSource;
  ```
  Place it next to the other `pub use` statements at the top of the file. **Do not** remove the `pub mod context_resolver;` line yet — that comes in commit 2.

- [ ] **Step 1.7: Verify the build is green**

  Run: `cargo check --lib --tests`
  Expected: success (no errors, no warnings). If the build fails, the most likely cause is a missing import in `exec.rs` — fix and re-run.

- [ ] **Step 1.8: Commit**

  ```bash
  git add src/tools/core/exec.rs src/tools/core/context_source.rs src/tools/core/mod.rs src/tools/core/traits.rs src/extensions/framework/types/mod.rs src/extensions/framework/protocols/shared/mod.rs
  git commit -m "refactor(tools): move execution primitives to tools::core (cycle 4, part 1)

  Adds src/tools/core/exec.rs (7 primitives: ToolContext, ToolError,
  ToolResult, AbortSignal, ToolWithContext, ToolProgressEvent,
  ToolContextAdapter) and src/tools/core/context_source.rs (ContextSource
  trait). The old locations keep thin re-exports for one commit so the
  tree stays buildable in-flight.

  tools/core/traits.rs now imports from crate::tools::core::exec, breaking
  the cycle at the source level. The re-exports are deleted in the next
  commit.

  Co-Authored-By: Claude <noreply@anthropic.com>"
  ```

---

## Task 2: Remove `extensions::framework` re-exports (cycle 4, part 2)

**Files:**
- Modify: `src/extensions/framework/types/mod.rs` (remove re-exports; remove `pub mod tool_exec;`)
- Modify: `src/extensions/framework/protocols/shared/mod.rs` (remove re-export; remove `pub mod context_resolver;`)
- Delete: `src/extensions/framework/types/tool_exec.rs`
- Delete: `src/extensions/framework/protocols/shared/context_resolver.rs`
- Modify: every file in the cycle-4 import list (callers must now use the new path)

**Goal:** Force all callers to import from `tools::core`. The cycle is now truly broken at the type-system level.

- [ ] **Step 2.1: Inventory remaining import sites**

  Run: `grep -rln 'crate::extensions::framework::types::\(ToolContext\|ToolError\|ToolResult\|AbortSignal\|ToolWithContext\|ToolProgressEvent\|ToolContextAdapter\)\|crate::extensions::framework::protocols::shared::context_resolver' src/ tests/`

  This produces the list of files that still need updating. Expected files (from exploration):
  - `src/extensions/framework/services/reserved_params.rs:154` (ToolContextAdapter)
  - `src/extensions/framework/services/tool_execution.rs:43` (ToolContext)
  - `src/extensions/framework/async_exec/executor/types.rs:3` (ToolResult)
  - `src/extensions/framework/async_exec/executor/executor.rs:10` (ToolResult)
  - `src/extensions/framework/core/*` (multiple files)
  - `src/extensions/framework/transport/async_router.rs`
  - `src/extensions/framework/manager/storage.rs`
  - `src/extensions/framework/manager/packaging.rs`
  - `src/extensions/framework/manager/mod.rs`
  - `src/extensions/framework/adapters/mod.rs`
  - `src/extensions/framework/types/tool.rs`
  - `src/extensions/framework/types/hook_io.rs`
  - `src/extensions/framework/types/manifest.rs`
  - `src/extensions/framework/protocols/shared/proxy_utils.rs`
  - `src/extensions/framework/services/tool_execution.rs`
  - `src/extensions/builtin/adapter.rs` (if it imports primitives)
  - `src/extensions/gateway/adapter.rs` (if)
  - `src/extensions/general/adapter.rs` (if)
  - `src/extensions/skill/adapter.rs` (if)
  - `src/extensions/universal/adapter.rs` (if)

- [ ] **Step 2.2: For each file from Step 2.1, update the import path**

  Pattern:
  - `use crate::extensions::framework::types::{X, Y};` → `use crate::tools::core::{X, Y};` (where X, Y are primitive names)
  - `use crate::extensions::framework::types::Foo;` → `use crate::tools::core::Foo;`
  - `use crate::extensions::framework::protocols::shared::context_resolver::ContextSource;` → `use crate::tools::core::ContextSource;`

  Edit each file with a single-line change. Do NOT change anything else in the file.

- [ ] **Step 2.3: Remove re-exports from `src/extensions/framework/types/mod.rs`**

  Delete the 6-line `pub use crate::tools::core::exec::{...}` block added in Step 1.5. Also delete the `pub mod tool_exec;` line (the file `src/extensions/framework/types/tool_exec.rs` will be deleted in Step 2.5; the `mod.rs` must not reference it).

- [ ] **Step 2.4: Remove re-export from `src/extensions/framework/protocols/shared/mod.rs`**

  Delete the `pub use crate::tools::core::context_source::ContextSource;` line added in Step 1.6. Also delete the `pub mod context_resolver;` line.

- [ ] **Step 2.5: Delete the old files**

  ```bash
  git rm src/extensions/framework/types/tool_exec.rs
  git rm src/extensions/framework/protocols/shared/context_resolver.rs
  ```

- [ ] **Step 2.6: Add the blanket impl in `src/tools/core/exec.rs`**

  At the end of the impl block section (after the existing `ToolResult` impls), add:
  ```rust
  // Blanket impl that was deliberately omitted from extensions/framework
  // (the cycle prevented it). Now that Tool lives in tools::core alongside
  // ToolWithContext, the impl is sound.
  impl<T: crate::tools::core::Tool + ?Sized> ToolWithContext for T {}
  ```

  Place this after the `ToolResult` block and before the `#[cfg(test)] mod tests` block.

- [ ] **Step 2.7: Verify the build is green**

  Run:
  ```bash
  cargo check --lib --tests
  cargo clippy --all-targets -- -D warnings
  cargo test --lib
  ```
  Expected: all green. If a test fails, the most likely cause is a missed import in Step 2.2 — re-run the grep from Step 2.1 to find it.

- [ ] **Step 2.8: Verify cycle 4 is broken at the boundary**

  Run:
  ```bash
  grep -rn 'use crate::extensions::framework::types::\(ToolContext\|ToolError\|ToolResult\|AbortSignal\|ToolWithContext\|ToolProgressEvent\|ToolContextAdapter\)' src/ tests/
  grep -rn 'use crate::extensions::framework::protocols::shared::context_resolver' src/ tests/
  ```
  Expected: zero matches. If any match exists, Step 2.2 missed a file.

- [ ] **Step 2.9: Commit**

  ```bash
  git add -A
  git commit -m "refactor(tools): remove extensions::framework re-exports of moved primitives (cycle 4, part 2)

  Cycle 4 (tools::core ↔ extensions::framework::types) is now broken at
  the type-system level. All callers must import execution primitives
  from crate::tools::core. The blanket impl Tool: ToolWithContext lands
  here — the cycle was the only thing preventing it.

  Co-Authored-By: Claude <noreply@anthropic.com>"
  ```

---

## Task 3: Fix `scripts/check_module_boundaries.sh` to scan the new path

**Files:**
- Modify: `scripts/check_module_boundaries.sh`

**Goal:** Make the CI lint actually enforce the new module boundaries. After this commit, `bash scripts/check_module_boundaries.sh` must pass on the current tree (post-cycle-4-break).

- [ ] **Step 3.1: Replace `src/extension/` with `src/extensions/framework/` in the script**

  Edit `scripts/check_module_boundaries.sh`:
  - Line 6: change `1. src/extension/ must NOT import from src/extensions/` to `1. src/extensions/framework/ must NOT import from src/extensions/<type>/`
  - Line 23: change echo to `Rule 1: src/extensions/framework/ must NOT import from src/extensions/<type>/`
  - Line 26: change `grep -rE '^[[:space:]]*use crate::extensions::' src/extension/ --include="*.rs"` to `grep -rE '^[[:space:]]*use crate::extensions::(builtin|gateway|general|mcp|skill|universal)::' src/extensions/framework/ --include="*.rs"`
  - Line 31: change `grep -r "crate::extensions::" src/extension/ --include="*.rs"` to `grep -rE "crate::extensions::(builtin|gateway|general|mcp|skill|universal)::" src/extensions/framework/ --include="*.rs"`
  - Line 39: change echo to `❌ FAIL: src/extensions/framework/ imports from src/extensions/<type>/`
  - Lines 107-108: change `src/extension/core/` to `src/extensions/framework/core/`
  - Line 104: change echo to `Rule 3: src/extensions/framework/core/ must NOT import from src/daemon/, src/tools/builtin/, or any other sub-module of src/tools/ that pulls in agents/tunnel/daemon`
  - Line 111: change echo to `❌ FAIL: src/extensions/framework/core/ imports from forbidden modules`

- [ ] **Step 3.2: Relax Rule 3 to allow `tools::core` imports**

  In the Rule 3 violation greps (lines 107-108), change the second grep to also allow `tools::core`:
  - Line 107 stays as `grep -r "crate::daemon::" src/extensions/framework/core/ --include="*.rs"`
  - Line 108 changes to scan for forbidden tools sub-modules only:
    ```bash
    VIOLATIONS_3B=$(grep -rE "crate::tools::(builtin|registry|factory)" src/extensions/framework/core/ --include="*.rs" || true)
    ```
  - Update the line 111 echo to: `❌ FAIL: src/extensions/framework/core/ imports from forbidden modules (daemon, tools::builtin, tools::registry, tools::factory)`

- [ ] **Step 3.3: Add new Rule 4**

  After Rule 3 (before the Summary block), add:
  ```bash
  # -----------------------------------------------------------------------------
  # Rule 4: src/extensions/framework/ must NOT import from src/agents/, src/tunnel/, or src/daemon/
  # -----------------------------------------------------------------------------
  echo "Rule 4: src/extensions/framework/ must NOT import from src/agents/, src/tunnel/, or src/daemon/"
  echo ""

  VIOLATIONS_4A=$(grep -rE "crate::(agents|tunnel|daemon)::" src/extensions/framework/ --include="*.rs" 2>/dev/null || true)
  # Exclude self-references to src/extensions/framework
  VIOLATIONS_4A=$(echo "$VIOLATIONS_4A" | grep -v "src/extensions/framework/" || true)

  if [ -n "$VIOLATIONS_4A" ]; then
      echo "  ❌ FAIL: src/extensions/framework/ imports from agents/tunnel/daemon"
      echo ""
      echo "$VIOLATIONS_4A" | while read -r line; do
          echo "     $line"
      done
      echo ""
      EXIT_CODE=1
  else
      echo "  ✓ PASS: No forbidden cross-domain imports"
  fi
  echo ""
  ```

- [ ] **Step 3.4: Run the script and verify it passes on the current tree**

  Run: `bash scripts/check_module_boundaries.sh`
  Expected: All 4 rules PASS. If any rule fails, the most likely cause is:
  - Rule 1 fails: framework imports from a specific extension type (e.g. `use crate::extensions::mcp::`). Investigate and either (a) move the importer to the framework, or (b) move the dependency into a shared submodule. Document the violation in a follow-up task.
  - Rule 3 fails: framework core imports from a tools sub-module other than `core`. Investigate and refactor.
  - Rule 4 fails: framework imports from agents, tunnel, or daemon. Investigate and refactor.

- [ ] **Step 3.5: Commit**

  ```bash
  git add scripts/check_module_boundaries.sh
  git commit -m "fix(ci): update check_module_boundaries.sh to scan src/extensions/framework/

  The previous script greped src/extension/ (singular) which no longer
  exists after the move to src/extensions/framework/. Rules 1 and 3
  silently passed (zero matches on a nonexistent directory).

  Also relaxes Rule 3 to allow framework/core to import from
  tools::core (one-way dep, established in commits 1-2), and adds
  Rule 4 banning framework from importing from agents/tunnel/daemon.

  The lint tier is now a real hard gate.

  Co-Authored-By: Claude <noreply@anthropic.com>"
  ```

---

## Task 4: Introduce `AgentMessageService` trait + `build_tool` factory (cycle 5, part 1)

**Files:**
- Create: `src/tunnel/a2a_message_types.rs`
- Modify: `src/tunnel/mod.rs` (add `pub mod a2a_message_types;`)
- Modify: `src/tunnel/a2a_send_tool.rs` (add trait + factory; change `A2aSendTool::new` signature)
- Modify: `src/agents/stateless_service.rs` (add one-commit re-export shim; type definitions remain)
- Modify: `src/tunnel/dispatcher.rs` (update import of `MessageRequest`/`MessageResult`)
- Modify: `src/ipc/server.rs` (update import of `MessageRequest`/`MessageResult`)
- Modify: `src/tunnel/a2a_e2e_tests.rs:236` (test-only; verify it still compiles)

**Goal:** Define the trait and factory. Move the request/response types to `tunnel::a2a_message_types`. Keep a one-commit re-export shim in `agents::stateless_service` so internal callers keep compiling.

- [ ] **Step 4.1: Create `src/tunnel/a2a_message_types.rs`**

  Copy the structs from `src/agents/stateless_service.rs`:
  - `MessageRequest` (line 156-186) → `A2aMessageRequest`
  - `MessageResult` (line 316-339) → `A2aMessageResponse`

  Copy ALL fields verbatim, including their doc comments, derives, and `with_*` builder methods. Only the type name changes.

- [ ] **Step 4.2: Update `src/tunnel/mod.rs`**

  Add `pub mod a2a_message_types;` to the module list. Place it next to `a2a_send_tool` for proximity.

- [ ] **Step 4.3: Add the re-export shim in `src/agents/stateless_service.rs`**

  At the top of the file (after the `use` statements, before the structs), add:
  ```rust
  // One-commit compat shim. The MessageRequest/MessageResult types
  // are now defined in tunnel::a2a_message_types. The local names
  // here are re-export aliases. Removed in the follow-up commit; all
  // internal callers will then use the tunnel path directly.
  pub use crate::tunnel::a2a_message_types::{
      A2aMessageRequest as MessageRequest, A2aMessageResponse as MessageResult,
  };
  ```

- [ ] **Step 4.4: Update `src/tunnel/a2a_send_tool.rs` imports**

  Remove line 36 (`use crate::agents::stateless_service::{MessageRequest, StatelessAgentService};`).

  Add at the top of the import block:
  ```rust
  use crate::tunnel::a2a_message_types::{A2aMessageRequest as MessageRequest, A2aMessageResponse as MessageResult};
  ```

  Note the local `as` aliases so the rest of the file compiles unchanged. The aliases are removed in the follow-up commit (Task 5) when the local names are removed from `agents::stateless_service`.

- [ ] **Step 4.5: Add the `AgentMessageService` trait to `src/tunnel/a2a_send_tool.rs`**

  Add at the end of the file (after the impl blocks, before the `#[cfg(test)] mod tests` block):
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
          req: MessageRequest,
      ) -> Result<MessageResult>;
  }
  ```

- [ ] **Step 4.6: Add the `build_tool` factory**

  Add after the trait:
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

- [ ] **Step 4.7: Change `A2aSendTool::new` signature**

  Change the constructor in `src/tunnel/a2a_send_tool.rs`:
  ```rust
  // Before:
  pub fn new(agent_service: Arc<StatelessAgentService>) -> Self { ... }

  // After:
  pub fn new(agent_service: Arc<dyn AgentMessageService>) -> Self { ... }
  ```

  No other changes to the `new` body — it just stores the `Arc<dyn AgentMessageService>` in the struct field.

- [ ] **Step 4.8: Change the `agent_service` field type**

  In `A2aSendTool`'s struct definition, change the field:
  ```rust
  // Before:
  agent_service: Arc<StatelessAgentService>,

  // After:
  agent_service: Arc<dyn AgentMessageService>,
  ```

  Update every internal use of `self.agent_service` (which currently calls `self.agent_service.execute_message(...)` or similar). The trait has the `execute_message` method so the call site is unchanged; the dispatch becomes a virtual call instead of a direct call.

- [ ] **Step 4.9: Update `src/tunnel/dispatcher.rs` and `src/ipc/server.rs`**

  In each file, find the import of `MessageRequest`/`MessageResult` from `agents::stateless_service` and change it to:
  ```rust
  use crate::tunnel::a2a_message_types::{A2aMessageRequest, A2aMessageResponse};
  ```

  Then update the type references in the file body from `MessageRequest` to `A2aMessageRequest` and `MessageResult` to `A2aMessageResponse`. (Mechanical find-and-replace per file.)

- [ ] **Step 4.10: Update `src/tunnel/a2a_e2e_tests.rs:236`**

  Change the test-only import from `use crate::agents::stateless_service::StatelessAgentService;` to:
  ```rust
  use crate::agents::stateless_service::StatelessAgentService;
  use crate::tunnel::a2a_send_tool::AgentMessageService;
  ```

  Find any `A2aSendTool::new(Arc::new(service))` calls in the test file. They become `A2aSendTool::new(Arc::new(service) as Arc<dyn AgentMessageService>)` or `A2aSendTool::new(Arc::clone(&service_arc) as Arc<dyn AgentMessageService>)` — depends on the local variable. If the local is already an `Arc<StatelessAgentService>`, the coercion is automatic.

- [ ] **Step 4.11: Update `src/agents/agent.rs:138,140` to use the new signature**

  The caller must be updated in this task so the commit ends with the build green (per Global Constraints). Find the `A2aSendTool::new(agent_service).with_caller_did(...)` and `A2aSendTool::new(agent_service).with_caller(...)` calls and replace them with the `build_tool` factory. Around line 124-128, where `agent_service` is extracted, coerce the concrete arc into the trait-object type:
  ```rust
  let dyn_service: Arc<dyn crate::tunnel::a2a_send_tool::AgentMessageService> =
      agent_service;
  let cross_ctx = self.extension_core.services().cross_runtime_a2a_ctx();
  ```

  Around line 138-140, replace the match expression that wires `with_caller_did`/`with_caller`/`with_cross_runtime` with:
  ```rust
  let tool = crate::tunnel::a2a_send_tool::build_tool(
      dyn_service,
      Some(&self.config.name),
      self.config.agent_did.as_deref(),
      cross_ctx,
  );
  tools.push(tool);
  ```

  Delete the now-unused `let mut a2a = match ...` block and the `if let Some(ctx) = cross_ctx { a2a = a2a.with_cross_runtime(ctx); }` block. The factory handles all three wires.

- [ ] **Step 4.12: Verify the build is green**

  Run:
  ```bash
  cargo check --lib --tests
  cargo clippy --all-targets -- -D warnings
  cargo test --lib
  ```
  Expected: all green. If a test fails, fix import-path issues; if the new `build_tool` call fails, fix the coercion in Step 4.11.

- [ ] **Step 4.13: Commit**

  ```bash
  git add -A
  git commit -m "refactor(tunnel): introduce AgentMessageService trait + build_tool factory (cycle 5, part 1)

  Adds the trait and the factory in tunnel::a2a_send_tool, and moves
  MessageRequest/MessageResult to a new tunnel::a2a_message_types module.
  A one-commit re-export shim in agents::stateless_service keeps
  internal callers compiling.

  A2aSendTool::new now takes Arc<dyn AgentMessageService> instead of
  Arc<StatelessAgentService>. agents/agent.rs routes through the
  build_tool factory so this commit ends green.

  Co-Authored-By: Claude <noreply@anthropic.com>"
  ```

---

## Task 5: Impl `AgentMessageService` + route construction through factory (cycle 5, part 2)

**Files:**
- Modify: `src/agents/stateless_service.rs` (add trait impl; remove re-export shim)
- Modify: `src/agents/agent.rs:138,140` (use `build_tool` factory)
- Modify: `src/tunnel/a2a_send_tool.rs` (remove the `as MessageRequest`/`as MessageResult` aliases now that the shim is gone)
- Modify: `src/agents/agent.rs` (also has imports to clean up)

**Goal:** Wire the trait. Remove the shim. Cycle 5 is now broken.

- [ ] **Step 5.1: Add the trait impl in `src/agents/stateless_service.rs`**

  At the end of the file (after the impl blocks for `StatelessAgentService`, before the `#[cfg(test)] mod tests` block), add:
  ```rust
  #[async_trait::async_trait]
  impl crate::tunnel::a2a_send_tool::AgentMessageService for StatelessAgentService {
      async fn execute_message(
          &self,
          req: crate::tunnel::a2a_message_types::A2aMessageRequest,
      ) -> Result<crate::tunnel::a2a_message_types::A2aMessageResponse> {
          // Delegate to the existing public method. The local MessageRequest
          // alias is the same type as A2aMessageRequest (via the re-export
          // shim); the compiler monomorphizes to the same call.
          Self::execute_message(self, req).await
      }
  }
  ```

- [ ] **Step 5.2: Remove the re-export shim from `src/agents/stateless_service.rs`**

  Delete the `pub use crate::tunnel::a2a_message_types::{A2aMessageRequest as MessageRequest, A2aMessageResponse as MessageResult};` block added in Task 4 Step 4.3.

  **Wait** — this is a structural decision. The shim is needed only if internal callers within `agents` (and possibly elsewhere) still reference `MessageRequest`/`MessageResult`. Two paths forward:

  - **Path A:** Keep the shim. Leave the impl from Step 5.1 in place. The shim stays "permanently" (or until a much later cleanup). Cycle 5 is broken (the cycle was `tunnel ↔ agents`, and the dependency is now `agents → tunnel` only). The shim is internal-only and never exposed to external consumers.
  - **Path B:** Update all internal `MessageRequest` references in `agents` (and any other internal sites) to use the new `A2aMessageRequest` name, then remove the shim.

  Per the spec, Path B is the goal. The expected internal sites (from the cycle-5 grep) are limited. Audit them now:
  ```bash
  grep -rn '\bMessageRequest\b\|\bMessageResult\b' src/agents/ src/tunnel/ src/daemon/ src/engine/ 2>&1 | grep -v "A2aMessage\|as MessageRequest\|test_" | head -30
  ```

  For each match outside the test code, decide: update to `A2aMessageRequest`/`A2aMessageResponse` (Path B) or keep the shim (Path A). Default to Path B unless the change would be invasive.

  If any change in this step feels like scope creep, switch to Path A and document the deferred cleanup in a follow-up note inside `CHANGES.md`. Do not let the shim removal block this commit.

- [ ] **Step 5.3: Update `src/agents/agent.rs` (only if a follow-up is needed)**

  Task 4 already routed `agents::agent.rs:138,140` through the `build_tool` factory, so no changes are required in this task unless the trait-impl wiring in Step 5.1 introduces a new import path. Verify with `cargo check --lib --tests` after Step 5.1; if anything fails, fix here.

- [ ] **Step 5.4: Update `src/tunnel/a2a_send_tool.rs` aliases (if Path B was taken in Step 5.2)**

  If the shim was removed in Step 5.2, the `as MessageRequest`/`as MessageResult` aliases in `src/tunnel/a2a_send_tool.rs` must be updated:
  - Find the import line that has `use crate::tunnel::a2a_message_types::{A2aMessageRequest as MessageRequest, A2aMessageResponse as MessageResult};`
  - Change to: `use crate::tunnel::a2a_message_types::{A2aMessageRequest, A2aMessageResponse};`
  - Find all in-file uses of `MessageRequest`/`MessageResult` and rename to `A2aMessageRequest`/`A2aMessageResponse`. Mechanical find-and-replace.

  If the shim was kept (Path A), no change to `a2a_send_tool.rs` is needed.

- [ ] **Step 5.5: Verify the build is green**

  Run:
  ```bash
  cargo check --lib --tests
  cargo clippy --all-targets -- -D warnings
  cargo test --lib
  ```
  Expected: all green. If `cargo test --lib` fails:
  - Most likely: a test imports the concrete `StatelessAgentService` and constructs `A2aSendTool::new(...)` directly. Wrap in `Arc<dyn AgentMessageService>` coercion.
  - Second most likely: an internal `MessageRequest` reference in `agents` broke because the shim was removed (Path B). Add the shim back and document as Path A.

- [ ] **Step 5.6: Verify cycle 5 is broken at the boundary**

  Run:
  ```bash
  grep -rn 'use crate::agents::stateless_service::StatelessAgentService' src/tunnel/ src/extensions/
  grep -rn 'A2aSendTool::new' src/agents/
  ```
  Expected: zero matches in both. The first proves tunnel doesn't import the concrete service. The second proves agents uses the factory.

- [ ] **Step 5.7: Verify the boundary lint still passes**

  Run: `bash scripts/check_module_boundaries.sh`
  Expected: all 4 rules PASS. The new `agents → tunnel` dependency (for `AgentMessageService`) is one-way, not a cycle.

- [ ] **Step 5.8: Commit**

  ```bash
  git add -A
  git commit -m "refactor(agents): impl AgentMessageService for StatelessAgentService; route construction through build_tool (cycle 5, part 2)

  Cycle 5 (tunnel ↔ agents) is now broken at the type-system level.
  tunnel defines the trait; agents implements it. Net dependency is
  agents → tunnel (one-way, no cycle).

  agents/agent.rs now calls the build_tool factory instead of
  constructing A2aSendTool directly. The MessageRequest/MessageResult
  re-export shim from the previous commit is [kept / removed] per
  [Path A / Path B] decision documented in CHANGES.md.

  Co-Authored-By: Claude <noreply@anthropic.com>"
  ```

---

## Task 6: Update `AGENTS.md` and `CHANGES.md`

**Files:**
- Modify: `AGENTS.md` (cycle 4 + 5 notes)
- Modify: `CHANGES.md` (entry under current version)

**Goal:** Document the cycle breaks.

- [ ] **Step 6.1: Update `AGENTS.md`**

  In the "Module Boundaries (Issue 014 / 015 / 016)" section (around line 210-220), replace the dependency-direction bullets with:
  - `extensions::framework` depends on `tools::core` (one-way, for `Tool`, `ToolContext`, `ContextSource`, and other execution primitives)
  - `tools::core` does NOT depend on `extensions::framework`
  - `tunnel` depends on `tools::core` (for `Tool`); does NOT depend on `agents`
  - `agents` depends on `tunnel` (for the `AgentMessageService` trait used by `A2aSendTool`); does NOT depend on `tunnel::a2a_send_tool`'s concrete types
  - `extensions::framework` does NOT depend on `agents`, `tunnel`, or `daemon`

  Add a note: "Cycles 4 (`tools::core ↔ extension::types`) and 5 (`tunnel ↔ agents`) from PLAN §2.5 are now actually broken (not reshuffled)."

- [ ] **Step 6.2: Update `CHANGES.md`**

  Add a new entry at the top of the current-version section:
  ```markdown
  ## Unreleased

  ### Cycle breaks (commits 1-5 on `refactor/clippy-cleanup-rust196`)

  - **Cycle 4 (`tools::core ↔ extensions::framework`):** Moved 7 tool execution
    primitives + the `ContextSource` trait + `ToolContextAdapter` from
    `extensions/framework/types/` and `extensions/framework/protocols/shared/`
    into `tools::core/`. The dependency direction is now one-way
    `extensions::framework → tools::core`. The blanket impl
    `impl<T: Tool> ToolWithContext for T` is now in place (was previously
    blocked by the cycle).

  - **Cycle 5 (`tunnel ↔ agents`):** Introduced the `AgentMessageService` trait
    in `tunnel::a2a_send_tool`. Moved `MessageRequest`/`MessageResult` to a
    new `tunnel::a2a_message_types` module. `agents::StatelessAgentService`
    implements the trait; `agents::agent.rs` routes `a2a_send` tool
    construction through a new `build_tool` factory in `tunnel`. Net
    dependency: `agents → tunnel` (one-way).

  - **CI:** `scripts/check_module_boundaries.sh` now scans
    `src/extensions/framework/` (not the obsolete `src/extension/` path).
    Rule 3 relaxed to allow `tools::core` imports in framework. New Rule 4
    bans `agents`/`tunnel`/`daemon` imports from `extensions::framework/`.
  ```

  If a follow-up note is needed (e.g. "MessageRequest shim kept as Path A;
  cleanup deferred to #XYZ"), add it here.

- [ ] **Step 6.3: Commit**

  ```bash
  git add AGENTS.md CHANGES.md
  git commit -m "docs: note that cycles 4 and 5 are now actually broken

  Co-Authored-By: Claude <noreply@anthropic.com>"
  ```

---

## Final Verification (after Task 6)

- [ ] **Final: Run all verification commands**

  ```bash
  cargo check --lib --tests
  cargo clippy --all-targets -- -D warnings
  cargo test --lib
  bash scripts/check_module_boundaries.sh

  # Cycle 4 proof:
  grep -rn 'use crate::extensions::framework::types::\(ToolContext\|ToolError\|ToolResult\|AbortSignal\|ToolWithContext\|ToolProgressEvent\|ToolContextAdapter\)' src/ tests/
  grep -rn 'use crate::extensions::framework::protocols::shared::context_resolver' src/ tests/

  # Cycle 5 proof:
  grep -rn 'use crate::agents::stateless_service::StatelessAgentService' src/tunnel/ src/extensions/
  grep -rn 'A2aSendTool::new' src/agents/
  ```

  Expected: all commands succeed; the 4 grep commands return zero matches. If any grep returns matches, the corresponding cycle is not actually broken — investigate and fix.

- [ ] **Final: Inspect commit history**

  Run: `git log --oneline origin/master..HEAD`
  Expected: 6 new commits on top of the spec commit (`608c506`). The 6 commits are the ones from Tasks 1-6. Do not push.

- [ ] **Final: Report**

  Report to the user:
  - Number of commits landed.
  - Output of `cargo test --lib` (test count, pass count).
  - Output of `bash scripts/check_module_boundaries.sh` (all 4 rules PASS).
  - The Path A / Path B decision from Task 5 Step 5.2 (if Path B failed, document why).
  - Any deviations from the spec.
