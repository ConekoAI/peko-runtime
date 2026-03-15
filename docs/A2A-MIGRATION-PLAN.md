# A2A and Async Tool Unification Migration Plan

## Executive Summary

Unify Pekobot's fragmented async delivery mechanisms (subagent_spawn, agent_invoke, process async) to match OpenClaw's clean single-queue architecture. This eliminates code duplication, simplifies mental models, and enables bidirectional A2A messaging.

## Current State (Fragmented)

```
┌─────────────────────────────────────────────────────────────────────────┐
│                        Current Architecture                              │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│  ┌──────────────┐     UnifiedAsyncExecutor     ┌─────────────────────┐  │
│  │ process      │ ────────────────────────────> │ AsyncResultQueue    │  │
│  │ (async)      │                               │ Manager             │  │
│  └──────────────┘                               └─────────────────────┘  │
│                                                                          │
│  ┌──────────────┐     UnifiedAsyncExecutor     ┌─────────────────────┐  │
│  │ subagent_    │ ────────────────────────────> │ AsyncResultQueue    │  │
│  │ spawn        │                               │ Manager             │  │
│  └──────────────┘                               └─────────────────────┘  │
│                                                                          │
│  ┌──────────────┐     EventSubscriber          ┌─────────────────────┐  │
│  │ agent_invoke │ ────────────────────────────> │ InvocationRegistry  │  │
│  │ (A2A)        │        (separate!)            │ (separate!)         │  │
│  └──────────────┘                               └─────────────────────┘  │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘

Problems:
- Two different result delivery paths
- A2A can't use queue-based delivery modes (steer, collect, interrupt)
- agent_invoke has its own registry, status tracking, cancellation
- Session ownership unclear for A2A
```

## Target State (Unified - OpenClaw Parity)

```
┌─────────────────────────────────────────────────────────────────────────┐
│                     Target Architecture (OpenClaw-Style)                │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│                         UnifiedAsyncExecutor                             │
│  ┌──────────────────────────────────────────────────────────────────┐   │
│  │  - Task registration (AsyncTaskRegistry)                         │   │
│  │  - Pluggable delivery (ResultDelivery trait)                     │   │
│  │  - Queue management (AsyncResultQueueManager)                    │   │
│  │  - Status tracking, cancellation                                 │   │
│  └──────────────────────────────────────────────────────────────────┘   │
│                              │                                           │
│           ┌──────────────────┼──────────────────┐                        │
│           ▼                  ▼                  ▼                        │
│    ┌──────────┐      ┌──────────────┐   ┌──────────────┐                │
│    │ process  │      │ subagent_    │   │ sessions_send│                │
│    │ (async)  │      │ spawn        │   │ (A2A)        │                │
│    └──────────┘      └──────────────┘   └──────────────┘                │
│                                                  │                       │
│                              Same delivery path  │                       │
│                                                  ▼                       │
│    ┌──────────────────────────────────────────────────────────────┐     │
│    │                   AsyncResultQueueManager                     │     │
│    │  - Per-session result queues                                  │     │
│    │  - Delivery modes: QueueWhenBusy, Interrupt, Collect, Steer  │     │
│    │  - A2A responses queued like subagent results                 │     │
│    └──────────────────────────────────────────────────────────────┘     │
│                                                                          │
│  Key Principle: ALL async results go through the same queue mechanism.  │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
```

## Key Architectural Changes

### 1. Session-Centric A2A (Like OpenClaw)

**Current**: `agent_invoke` uses `InvocationRegistry` + `EventSubscriber`

**Target**: A2A = sending message to another agent's session

```rust
// BEFORE: Fragmented A2A
AgentInvokeTool::execute_async() {
    // Registers in InvocationRegistry
    // Uses EventSubscriber for delivery
    // Separate from subagent queue
}

// AFTER: Unified A2A (OpenClaw-style)
SessionsSendTool::execute_async() {
    // Target agent must have a session
    // Message queued to target's session
    // Response queued back to caller's session
    // Same queue mechanism as subagent_spawn
}
```

### 2. Unified Result Types

```rust
/// Current: agent_invoke has separate result handling
pub enum AsyncTaskResult {
    Process { ... },
    Subagent { ... },
    Invocation { ... },  // <-- Separate type for A2A
    Generic { ... },
}

/// Target: A2A is just another "session message"
pub enum AsyncTaskResult {
    Process { ... },
    Subagent { ... },
    SessionMessage {      // <-- Unified with A2A
        from_session: String,
        content: String,
        message_type: MessageType,  // Request, Response, Announcement
    },
    Generic { ... },
}
```

### 3. Session Ownership Clarification

| Aspect | Current | Target |
|--------|---------|--------|
| **Who owns sessions?** | Mixed (runtime + tools) | Runtime owns ALL sessions |
| **A2A target** | Agent DID | Session key |
| **Bidirectional** | Hard (who initiates?) | Easy (both have sessions) |
| **Lifetime** | Tool-managed | Runtime-managed |

## Migration Phases

### Phase 1: Foundation (Week 1)

#### 1.1 Add SessionMessage variant to AsyncTaskResult

```rust
// src/agent/async_tool_framework.rs

pub enum AsyncTaskResult {
    // ... existing variants ...
    
    /// Session-to-session message (A2A unified)
    SessionMessage {
        /// Source session key
        from_session: String,
        /// Target session key
        to_session: String,
        /// Message content
        content: String,
        /// Message type for routing
        message_type: SessionMessageType,
        /// Conversation ID for threading
        conversation_id: String,
    },
}

pub enum SessionMessageType {
    /// Initial request to another agent
    Request,
    /// Response to a request
    Response,
    /// Fire-and-forget announcement
    Announcement,
    /// Subagent completion (existing)
    Completion,
}
```

#### 1.2 Create SessionsSendTool (A2A replacement)

```rust
// src/tools/sessions_send.rs

/// Tool for sending messages to other agent sessions (A2A)
pub struct SessionsSendTool {
    executor: UnifiedAsyncExecutor,
    session_router: SessionRouter,
}

impl SessionsSendTool {
    /// Send message to another agent's session
    async fn send_to_session(
        &self,
        target_session_key: &str,
        message: &str,
        parent_session_key: &str,
    ) -> Result<AsyncTaskReceipt> {
        self.executor.execute(
            task_id,
            "sessions_send",
            params,
            parent_session_key,  // Results queue back HERE
            config,
            move || async move {
                // Deliver message to target session
                // Wait for response (if sync mode)
                // Return SessionMessage result
                Ok(AsyncTaskResult::SessionMessage { ... })
            },
        ).await
    }
}
```

#### 1.3 Add A2A delivery support to QueueDelivery

```rust
impl ResultDelivery for QueueDelivery {
    async fn deliver(&self, entry: &AsyncTaskEntry) -> Result<()> {
        match entry.result {
            Some(AsyncTaskResult::SessionMessage { ... }) => {
                // A2A result goes to same queue
                self.queue_manager.enqueue(event);
            }
            // ... existing variants ...
        }
    }
}
```

### Phase 2: Migration (Week 2)

#### 2.1 Deprecate agent_invoke

```rust
// src/tools/agent_invoke.rs

#[deprecated(
    since = "0.3.0",
    note = "Use sessions_send tool instead. See docs/A2A-MIGRATION-PLAN.md"
)]
pub struct AgentInvokeTool { ... }
```

#### 2.2 Update AgentSpawnTool to use unified executor

Already done in previous commits ✅

#### 2.3 Ensure all tools use QueueDelivery

- Verify ProcessTool uses QueueDelivery ✅
- Verify SubagentExecutor uses QueueDelivery ✅
- Add SessionsSendTool with QueueDelivery

### Phase 3: Session Management (Week 3)

#### 3.1 Ensure runtime owns all sessions

```rust
// src/session/manager.rs

impl SessionManager {
    /// Create a session for A2A target
    /// This ensures the target agent has a session to receive messages
    pub async fn ensure_agent_session(
        &self,
        agent_did: &str,
    ) -> Result<String> {
        // Check if agent has active session
        // If not, create one (ephemeral for A2A)
        // Return session key
    }
}
```

#### 3.2 Session key format for A2A

```rust
// Consistent session key format

// Subagent session
"agent:{agent_id}:subagent:{uuid}"

// User conversation session  
"agent:{agent_id}:user:{channel}:{user_id}"

// A2A session (ephemeral)
"agent:{agent_id}:a2a:{caller_agent_id}:{uuid}"
```

### Phase 4: Cleanup (Week 4)

#### 4.1 Remove deprecated code

- Remove `agent_invoke.rs`
- Remove `InvocationRegistry`
- Remove `EventSubscriber` (if no longer needed)

#### 4.2 Update documentation

- Update `async-tool-framework.md`
- Add A2A usage examples
- Document session ownership model

## API Changes

### Before (agent_invoke)

```json
{
  "tool": "agent_invoke",
  "params": {
    "target": "analyzer",
    "message": "Review this code",
    "mode": "async"
  }
}
```

Result delivered via: `EventSubscriber`

### After (sessions_send)

```json
{
  "tool": "sessions_send",
  "params": {
    "target_session": "agent:analyzer:a2a:main:uuid",
    "message": "Review this code",
    "mode": "async",
    "delivery_mode": "queue_when_busy"
  }
}
```

Result delivered via: `AsyncResultQueueManager` (same as subagent_spawn)

## Benefits

| Benefit | Description |
|---------|-------------|
| **Simplicity** | One delivery mechanism instead of two |
| **Consistency** | A2A results use same queue modes (steer, collect, interrupt) |
| **Bidirectional** | Both agents have sessions, can send to each other |
| **Testability** | Single registry to mock/assert against |
| **Debugging** | One place to trace async results |
| **OpenClaw parity** | Easier to port features between projects |

## Backward Compatibility

### For agent_invoke users

```rust
// Shim layer for backward compatibility
impl AgentInvokeTool {
    async fn execute(&self, params: Value) -> Result<Value> {
        // Log deprecation warning
        warn!("agent_invoke is deprecated, use sessions_send");
        
        // Translate to sessions_send
        let sessions_params = json!({
            "target_session": self.resolve_agent_session(params["target"].as_str()?)?,
            "message": params["message"],
            "mode": params["mode"],
        });
        
        // Delegate to SessionsSendTool
        self.sessions_send.execute(sessions_params).await
    }
}
```

### Migration timeline

1. **v0.3.0**: Add `sessions_send`, deprecate `agent_invoke`
2. **v0.4.0**: Remove `agent_invoke` (after 1 release cycle)

## Testing Strategy

### Unit tests

```rust
#[tokio::test]
async fn test_a2a_result_goes_to_same_queue() {
    let queue_manager = Arc::new(RwLock::new(AsyncResultQueueManager::new()));
    let executor = UnifiedAsyncExecutor::with_registries(...);
    
    // Spawn subagent
    executor.execute(subagent_task).await?;
    
    // Send A2A message
    executor.execute(a2a_task).await?;
    
    // Both results should be in the same queue
    let events = queue_manager.read().await.process_queue(session_key).await;
    assert_eq!(events.len(), 2);
}
```

### Integration tests

```rust
#[tokio::test]
async fn test_bidirectional_a2a() {
    // Agent A sends to Agent B
    let receipt_a = agent_a.sessions_send("agent_b_session", "Hello B").await?;
    
    // Agent B responds to Agent A
    let receipt_b = agent_b.sessions_send("agent_a_session", "Hello A").await?;
    
    // Both results delivered through queue
    // No need for separate EventSubscriber
}
```

## Implementation Checklist

- [ ] Add `SessionMessage` variant to `AsyncTaskResult`
- [ ] Create `SessionsSendTool` 
- [ ] Update `QueueDelivery` for A2A messages
- [ ] Add session resolution logic
- [ ] Deprecate `AgentInvokeTool`
- [ ] Add migration shim for backward compatibility
- [ ] Update documentation
- [ ] Add comprehensive tests
- [ ] Remove deprecated code in v0.4.0

## Open Questions

1. **Should we keep EventSubscriber for non-A2A system events?**
   - Recommendation: Yes, but rename to `SystemEventBus` to clarify purpose
   - A2A should use queue, system events use broadcast

2. **How to handle A2A session lifecycle?**
   - Recommendation: Ephemeral sessions created on-demand, timeout after idle

3. **What about external agents (Coneko network)?**
   - Recommendation: Same pattern - proxy session created locally

## References

- OpenClaw `subagent-spawn.ts` - Spawn and announcement logic
- OpenClaw `sessions-send-tool.a2a.ts` - A2A implementation
- OpenClaw `subagent-announce.ts` - Queue-based delivery
- Current Pekobot `agent_invoke.rs` - To be deprecated
