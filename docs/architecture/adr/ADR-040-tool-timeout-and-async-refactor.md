# ADR-040: Tool Timeout and Async Refactor

**Status:** Accepted
**Date:** 2026-06-23
**Author:** Implementation team
**Supersedes:** Implicit behavior in `AsyncExecutionRouter` (ADR-018a) regarding `_async`/`_timeout` param injection.
**Related:** ADR-018a, ADR-020.

## Context

The async tool execution system in `peko-runtime` had grown a parameter-injection surface (`_async`, `_timeout`, `_callback`, `_progress`, `_priority`, `_retry`) that was leaky (tools had to defensively check `params.get("_async")` for the bypass case), bloaty (six reserved keys parsed on every tool call), and incomplete (the `AsyncResultQueueManager.process_queue()` was defined but never called by the agentic loop, so completions were never auto-injected). It also relied on the calling process's `tokio::spawn`, so tasks died with the CLI.

## Decision

1. A single constant `DEFAULT_TOOL_TIMEOUT_SECS = 300` (5 min) governs all tool calls. Agent config overrides per-agent.
2. On timeout, the work is *detached* (not killed) to a background task via the existing `AsyncExecutor` and a receipt is returned to the agent.
3. The reserved `_async`/`_timeout`/`_callback`/`_progress`/`_priority`/`_retry` parameters are removed from the public tool-call surface.
4. The `task` builtin tool gains two actions: `spawn` (invoke any tool async) and `output` (read a task's output, optionally blocking).
5. A new per-session `AsyncTaskCompletionQueue` accumulates `CompletionEvent`s as tasks reach terminal state.
6. The `AgenticLoop` drains the queue at the start of each `run_inner` iteration and surfaces all completions as a single synthetic user-role `LlmMessage` containing one `ContentBlock::ToolResult` per event.

## Consequences

- All tools get the same timeout. No per-tool configuration beyond a global default and per-agent override.
- `agent_spawn` and other long-running tools are no longer special-cased. They go through the same auto-detach path.
- The synthetic completion message carries a truncated preview of each result; the agent calls `task output` for full content.
- Tasks that complete mid-iteration wait for the next iteration to be visible. The drain is once per iteration, not continuous.
- In CLI mode (no daemon), `tokio::spawn` tasks still die with the process. ADR-020's daemon-based work remains the long-term fix.

## Migration

Five commits on `feature/tool-async-refactor` (no PR opened as of 2026-06-23):

1. `AsyncTaskCompletionQueue` + executor fan-out (additive, no behavior change)
2. `task` tool gains `spawn` and `output` actions
3. `AgenticLoop` drains the queue at iteration start
4. Strip `_async`/`_timeout` from `AsyncExecutionRouter` (with one release of `tracing::warn!`)
5. Delete `agent_spawn` special case, remove warning, write this ADR

See [`docs/superpowers/specs/2026-06-23-tool-async-refactor-design.md`](../../superpowers/specs/2026-06-23-tool-async-refactor-design.md) for the full design.
