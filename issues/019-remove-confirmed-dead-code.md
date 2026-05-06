# Issue 019 — Remove Confirmed Dead Code

**Status:** Open  
**Labels:** `cleanup`, `refactoring`, `dead-code`, `breaking-change-ok`  
**Milestone:** 0.1.0  
**Backward Compatibility:** Not required (pre-release dev stage)

---

## Summary

A code-review sweep identified 10 distinct locations of **confirmed dead code** — modules, functions, variants, and structs that are either never called, always return the same trivial value, or have been superseded by newer implementations. Because Pekobot is still at pre-release dev stage (0.1.0), we can remove them outright without deprecation cycles.

---

## Dead-Code Inventory

| # | File | Symbol / Lines | Nature of Dead Code | Suggested Action |
|---|------|----------------|---------------------|------------------|
| 1 | `src/agent/registry.rs` | Whole file (12 lines) | Placeholder after `LocalRegistry` deletion on 2026-04-22. Comment explicitly says *"Remove this file and its mod.rs entry once confident no external code references it."* | **Delete file** and remove `pub mod registry;` from `src/agent/mod.rs`. |
| 2 | `src/watcher.rs` | `FileWatcher`, `WatchEvent`, `WatchHandle`, `FileWatcherHandle`, `watch_agent_directory` (391 lines) | Complete `notify`-based file watcher. Declared in `src/lib.rs` but **zero non-test callers** anywhere in `src/`. Config fields in `types::config.rs` reference it, but nothing wires it up. | **Delete file**, remove `pub mod watcher;` from `src/lib.rs`, and drop the `notify` dependency from `Cargo.toml`. Also remove dead `file_watcher` config structs if they become unused. |
| 3 | `src/engine/agentic_loop.rs:1055` | `fallback_chat_with_tools` | `#[allow(dead_code)]`. Never called. Implements a legacy non-native-tool completion path. | **Remove** function. |
| 4 | `src/engine/agentic_loop.rs:1436` | `messages_to_prompt` | `#[allow(dead_code)]`. Only called by `fallback_chat_with_tools` (item 3). | **Remove** function. |
| 5 | `src/engine/agentic_loop.rs:1462` | `parse_legacy_response` | `#[allow(dead_code)]`. Only called by `fallback_chat_with_tools` (item 3). | **Remove** function. |
| 6 | `src/engine/agentic_loop.rs:1486` | `parse_content_block` | `#[allow(dead_code)]`. Only called by `parse_legacy_response` (item 5). | **Remove** function. |
| 7 | `src/engine/events.rs:67` | `AgenticEvent::Assistant` variant | `#[deprecated(since = "0.1.0")]`. Still matched in **3 places** (`agentic_loop.rs` ×2, `event_processor.rs` ×1) and tested in `event_processor.rs` tests. Superseded by `AssistantText` / `AssistantDelta`. | **Remove variant** and all `#[allow(deprecated)]` match arms. Update tests to use `AssistantText`. |
| 8 | `src/daemon/mod.rs:633` | `resolve_default_agent` | Always returns `None`. Comment admits *"service not exposed"*. Called once from `run_job_with_agent_service`. | **Remove** method and inline the `"default".to_string()` fallback at the call site. |
| 9 | `src/daemon/mod.rs:676` | `execute_isolated_job` fallback stub | After the early `if self.agent_service.is_some()` return, the remaining body is unreachable dead code (converges to the same path as `execute_main_job`). | **Remove** unreachable stub body; keep the early-return wrapper or merge with `execute_main_job` if logic is identical. |
| 10 | `src/engine/events.rs:313` | `EventRouter` | Defined but **zero non-test callers**. `src/commands/orchestration.rs:167` has a TODO referencing it, but never instantiates it. | **Remove** struct, impl, `Default`, and tests. Remove re-export from `src/engine/mod.rs`. |
| 11 | `src/engine/execution.rs` | `TaskExecutor` | Engine loop calls `runtime::execute_tool_via_core_with_context` directly, never `TaskExecutor`. | **Remove** `TaskExecutor` struct and impl. Keep `TaskId`, `ExecutionMode`, `TaskStatus`, `TaskSummary` if still used. Remove re-export from `src/engine/mod.rs`. |

---

## Why Remove Now?

1. **Compile-time hygiene** — Dead code triggers `dead_code` lints (suppressed with `#[allow]`), hiding real warnings.  
2. **Cognitive load** — New contributors waste time tracing call graphs that end in stubs.  
3. **Dependency bloat** — `notify = "6.1"` is pulled solely for `src/watcher.rs`. Removing it shrinks the dependency tree.  
4. **Pre-release window** — We are at 0.1.0; no external consumers to break.  

---

## Proposed Clean, Future-Proof Solution

### Phase A — Surgical Deletion (immediate)

1. **Delete `src/agent/registry.rs`** and its `mod.rs` entry.  
2. **Delete `src/watcher.rs`**, its `lib.rs` entry, and the `notify` crate from `Cargo.toml`.  
3. **Delete the 4 legacy fallback functions** in `src/engine/agentic_loop.rs` (items 3–6).  
4. **Delete `AgenticEvent::Assistant`** variant and all match arms / tests that reference it.  
5. **Delete `EventRouter`** and its re-export.  
6. **Delete `TaskExecutor`** and its re-export.  
7. **Refactor `resolve_default_agent`** into an inline expression.  
8. **Refactor `execute_isolated_job`** to remove the unreachable stub tail.

### Phase B — Prevent Regressions (CI / linting)

Add a **CI gate** (or local pre-push hook) that treats `dead_code` warnings as errors for the workspace:

```bash
RUSTFLAGS="-D warnings" cargo clippy --all-targets --all-features
```

This ensures dead code cannot re-enter `main` without an explicit `#[allow(dead_code)]` + PR justification.

### Phase C — Future Re-introduction Path (if needed)

If any of the removed capabilities are needed later, the git history preserves them. Re-introduction should follow these rules:

- **File watcher** — Re-implement on top of the existing `file_watcher` config schema in `types::config.rs` (kept), but only when a real consumer (e.g., daemon hot-reload) is ready.  
- **EventRouter** — If multi-consumer event broadcasting is needed, prefer `tokio::sync::broadcast` or a lightweight pub-sub trait rather than a hand-rolled vector of `mpsc::Sender`s.  
- **TaskExecutor** — If timeout-wrapping execution is needed, extract a thin `with_timeout` helper in `extension::async_exec` rather than a standalone struct.  
- **Registry** — Build on `crate::common::registry::SimpleRegistry` or `SharedRegistry` as noted in the placeholder comment.  

---

## Acceptance Criteria

- [ ] `cargo clippy` reports **zero** `dead_code` warnings in `src/` (excluding generated code).  
- [ ] `cargo test` passes.  
- [ ] `cargo build --release` passes.  
- [ ] `Cargo.lock` shrinks (no `notify` or related transitive deps).  
- [ ] `AGENTS.md` updated if any module structure changes are documented there.  

---

## Related

- `LocalRegistry` deletion commit (2026-04-22)  
- Issue 014 / 015 / 016 — Module boundary clean-ups  
- `src/engine/events.rs` — `AssistantText` introduced as replacement for `Assistant`
