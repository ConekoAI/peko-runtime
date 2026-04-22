# Issue 003: Agent Execution — Four Layers of Delegation

**Severity:** HIGH  
**Status:** Closed (Resolved via Option A)  
**Labels:** `architecture`, `agent-execution`, `orchestration`, `refactor`, `adr-016`  
**Reported:** 2026-04-21  
**Closed:** 2026-04-22  

---

## Summary

Agent execution involved four layers of delegation for the core operation. `Agent` had four execute methods that all delegated to `AgentExecutor`. `AgentExecutor` had the same four methods, delegating to `AgenticLoop`. Then `StatelessAgentService` cold-started an `Agent` and called `execute_with_session()`. This excessive layering added no value, complicated state management, and made the execution flow difficult to trace.

---

## Delegation Chain (Before)

```
StatelessAgentService::execute_message()
  → StatelessAgentService::execute()
    → StatelessAgentService::execute_inner()
      → Agent::new(config)
      → Agent::execute_with_session(prompt, session, history, on_event)
        → AgentExecutor::execute_with_session(prompt, session, history, on_event)
          → AgenticLoop::new(agent, provider, extension_core)
          → AgenticLoop::run_with_resume(prompt, on_event, session, history)
            → AgenticLoop::run_streaming_with_resume(...)
              → AgenticLoop::run_inner(...)
```

**Total: 4 orchestrator layers** (`StatelessAgentService`, `Agent`, `AgentExecutor`, `AgenticLoop`) for a single operation.

---

## Delegation Chain (After)

```
StatelessAgentService::execute_message()
  → StatelessAgentService::execute()
    → StatelessAgentService::execute_inner()
      → Agent::new(config)
      → Agent::execute_with_session(prompt, session, history, on_event)
        → AgenticLoop::new(Arc::new(agent.clone()), provider, extension_core)
        → AgenticLoop::run_with_resume(prompt, on_event, session, history)
          → AgenticLoop::run_inner(...)
```

**Total: 2 orchestrator layers** (`StatelessAgentService`, `Agent`) — `Agent` directly creates `AgenticLoop`.

---

## Resolution

**Option A was implemented:** Collapsed `Agent` + `AgentExecutor` into a single `Agent` layer.

### Changes Made

1. **`src/agent/agent.rs`**
   - Added `Clone` impl for `Agent` (replaces `as_executor_agent()` workaround)
   - Replaced 4 thin delegation methods with direct `AgenticLoop` construction and execution
   - Added `prepare_execution()` private method (moved from `AgentExecutor`)
   - Deleted `as_executor_agent()` — no longer needed
   - Updated stale doc comments referencing `AgentExecutor`

2. **`src/agent/mod.rs`**
   - Removed `AgentExecutor` module declaration and re-export

3. **`src/agent/executor.rs`**
   - **Deleted** — entire `AgentExecutor` struct and impl removed (~267 lines)

### Key Benefits

- **~270 lines of delegation code deleted**
- **No shallow clone workaround** — `Agent::clone()` is clean and shares `Arc` fields
- **State management centralized** in `Agent` — no race conditions between original and clone
- **Zero external API changes** — all `Agent::execute*()` signatures unchanged
- **No call-site changes required** — `AgentExecutor` was only used internally by `Agent`

---

## Acceptance Criteria

- [x] The execution stack has at most 2 layers between the caller and `AgenticLoop`.
- [x] `as_executor_agent()` is removed.
- [x] There is a single, clear entry point for agent execution (`Agent::execute*`).
- [x] `StatelessAgentService` does not bypass layers or create redundant `Agent` clones.
- [x] All existing tests pass without increasing mock complexity. (836 passed, 23 ignored)

---

## Related

- `src/agent/agent.rs`
- ~~`src/agent/executor.rs`~~ (deleted)
- `src/agent/stateless_service.rs`
- `src/engine/agentic_loop.rs`
- ADR-016: Stateless Agent Service
- ADR-020: Daemon-Based Async Execution
