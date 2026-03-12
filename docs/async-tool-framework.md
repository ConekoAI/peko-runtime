# Async Tool Framework

## Overview

The Async Tool Framework provides infrastructure for tools that execute asynchronously. This is a generalization of the subagent spawn pattern, allowing any tool to:

1. Return a receipt immediately (non-blocking)
2. Execute work in the background
3. Queue results for delivery when the parent agent is ready

This design is inspired by OpenClaw's subagent announcement queue but generalized for any async tool operation.

## Core Concepts

### AsyncTaskRegistry

Tracks all async tasks across the system:

```rust
pub struct AsyncTaskRegistry {
    tasks: HashMap<AsyncTaskId, AsyncTaskEntry>,
    pending_announcements: HashMap<String, Vec<AsyncTaskId>>, // session_key -> tasks
}
```

### AsyncResultQueueManager

Manages per-session queues for async results:

```rust
pub struct AsyncResultQueueManager {
    queues: HashMap<String, AsyncResultQueue>, // session_key -> queue
}
```

### Delivery Modes

Inspired by OpenClaw's queue modes:

| Mode | Behavior |
|------|----------|
| `QueueWhenBusy` | Queue result and deliver when agent is idle (default) |
| `Interrupt` | Interrupt current agent execution with result |
| `Collect` | Batch multiple results together |
| `Steer` | Try to inject into running session (advanced) |

## Integration with Subagent Spawn

The subagent spawn tool becomes an **async tool** implementation:

```rust
#[async_trait]
impl AsyncTool for AgentSpawnToolV2 {
    fn name(&self) -> &str { "agent_spawn" }
    
    async fn spawn_async(
        &self,
        params: Value,
        parent_session_key: &str,
        config: AsyncToolConfig,
    ) -> Result<AsyncTaskReceipt> {
        // 1. Create child session
        // 2. Register task in AsyncTaskRegistry
        // 3. Spawn background task
        // 4. Return receipt immediately
    }
    
    async fn check_status(&self, task_id: &AsyncTaskId) -> Result<AsyncTaskStatus>;
    async fn cancel(&self, task_id: &AsyncTaskId) -> Result<bool>;
    
    fn format_result(&self, entry: &AsyncTaskEntry) -> String {
        // Format subagent result as system message
    }
}
```

## Result Delivery Flow

### When Parent is Busy

```
Child Task Completes
        ↓
Result stored in AsyncTaskRegistry
        ↓
Added to session's pending_announcements queue
        ↓
Parent agent loop finishes current iteration
        ↓
Check pending_announcements for session
        ↓
Deliver queued results as new "user" message
        ↓
Trigger new agent iteration
```

### When Parent is Idle

```
Child Task Completes
        ↓
Result stored in AsyncTaskRegistry
        ↓
Immediately deliver as "user" message
        ↓
Trigger agent iteration
```

## Message Format

Results are delivered as system-style messages (not assistant messages):

```
[System Message] [sessionId: xxx] A subagent task "taskLabel" just completed successfully.

Result:
<subagent's final assistant output>

[stats line]

Instruction: Convert this result into your normal assistant voice for the user.
```

This gives the parent agent:
1. Clear context about what completed
2. The raw result for synthesis
3. Instructions on how to handle it

## Architecture Benefits

1. **Unified async pattern**: Any tool can be async, not just subagent spawn
2. **Race condition safety**: No corruption from concurrent session writes
3. **Flexible delivery**: Different modes for different use cases
4. **Clean separation**: Async execution is orthogonal to tool implementation
5. **OpenClaw compatibility**: Similar queue semantics for familiar behavior

## Future Extensions

### Other Async Tools

- **Cron jobs**: Schedule future execution, result delivered when complete
- **Long-running processes**: Spawn process, get notified on completion
- **External webhooks**: Register webhook, result delivered when callback received
- **Batch operations**: Multiple file operations, results collected and delivered together

### Collect Mode Batching

When multiple async tasks complete while parent is busy:

```
[Multiple async tasks completed]

## subagent_spawn (task_1):
Result 1 content...

## subagent_spawn (task_2):
Result 2 content...

## process (task_3):
Process completed with exit code 0

Instruction: Synthesize these results for the user.
```

## Integration with Agent Loop

The agent loop needs to:

1. **Before each iteration**: Check `AsyncResultQueueManager` for pending events
2. **Process events**: Convert events to system/user messages
3. **Add to conversation**: Include in messages sent to LLM
4. **Continue loop**: Let LLM respond to the async results

```rust
// In agent loop
async fn run_iteration(&mut self) -> Result<()> {
    // 1. Check for async task completions
    let events = self.async_queue_manager
        .process_queue(&self.session_key)
        .await;
    
    // 2. Add events to conversation as system messages
    for event in events {
        self.add_system_message(&event.result_message).await?;
    }
    
    // 3. Normal loop execution...
}
```

## Comparison to OpenClaw

| Aspect | OpenClaw | Our Framework |
|--------|----------|---------------|
| Queue storage | In-memory + persistence | In-memory registry |
| Delivery modes | steer, queue, collect, interrupt | Same modes |
| Busy detection | `isEmbeddedPiRunActive()` | Agent state tracking |
| Result format | System message trigger | System message trigger |
| Batch handling | Collect mode with summary | Collect mode with batch event |
| Scope | Subagents only | Any async tool |
