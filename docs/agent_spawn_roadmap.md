# Agent Spawn Implementation Roadmap (OpenClaw-Style)

## Overview
Full implementation of subagent spawning with async execution, result announcement, and lifecycle management.

## Phase 1: Core Infrastructure (Foundation)

### 1.1 Subagent Registry
**File**: `src/agents/subagent_registry.rs`

Create a registry to track active subagent runs:
```rust
pub struct SubagentRun {
    pub run_id: String,
    pub child_session_key: String,
    pub parent_session_key: String,
    pub task: String,
    pub status: SubagentStatus,
    pub started_at: DateTime<Utc>,
    pub cleanup: SpawnCleanupPolicy,
    pub result: Option<SubagentResult>,
}

pub enum SubagentStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
    TimedOut,
}
```

**Tasks**:
- [ ] Create registry data structures
- [ ] Implement `register_run()`, `update_run()`, `complete_run()`
- [ ] Add cleanup on completion (keep/delete sessions)
- [ ] Thread-safe access (Arc<RwLock<>>)

### 1.2 Result Announcement System
**File**: `src/agents/subagent_announce.rs`

Handle announcing results back to parent sessions:
```rust
pub async fn announce_subagent_result(
    parent_session_key: &str,
    child_run_id: &str,
    result: SubagentResult,
) -> Result<()>;
```

**Tasks**:
- [ ] Build announcement message format
- [ ] Add message to parent base session
- [ ] Handle async notification

### 1.3 Session Key Utilities
**File**: `src/session/subagent_key.rs`

Standardized key format for subagent sessions:
```
agent:{agent_name}:subagent:{uuid}  # OpenClaw style
# OR hybrid
agent:{agent_name}:peer:user:{parent}:subagent:{uuid}
```

**Tasks**:
- [ ] Define subagent key format
- [ ] Implement key generation
- [ ] Parse subagent keys for depth tracking

---

## Phase 2: Async Execution Engine

### 2.1 Subagent Executor
**File**: `src/agents/subagent_executor.rs`

Async task executor for subagents:
```rust
pub struct SubagentExecutor {
    registry: Arc<RwLock<SubagentRegistry>>,
    max_concurrent: usize,
}

impl SubagentExecutor {
    pub async fn spawn_and_execute(
        &self,
        task: &str,
        child_session: SessionContext,
        parent_key: &str,
        config: ExecutionConfig,
    ) -> Result<String>; // Returns run_id
}
```

**Tasks**:
- [ ] Create executor with task queue
- [ ] Implement actual agent execution in spawn session
- [ ] Handle timeouts and cancellation
- [ ] Register runs and update status

### 2.2 Background Task Management
**File**: `src/agents/task_manager.rs`

Manage background subagent tasks:
```rust
pub struct BackgroundTaskManager {
    handles: HashMap<String, JoinHandle<()>>,
}
```

**Tasks**:
- [ ] Spawn tasks in background
- [ ] Track active handles
- [ ] Cleanup completed tasks
- [ ] Graceful shutdown on exit

---

## Phase 3: Agent Spawn Tool Rewrite

### 3.1 Tool Implementation
**File**: `src/tools/agent_spawn.rs` (new file)

Complete rewrite of AgentSpawnTool:
```rust
pub struct AgentSpawnTool {
    session_router: SessionRouter,
    executor: Arc<SubagentExecutor>,
    registry: Arc<RwLock<SubagentRegistry>>,
    current_session: Option<SessionContext>,
}

#[async_trait]
impl Tool for AgentSpawnTool {
    async fn execute(&self, params: Value) -> Result<Value> {
        // 1. Parse parameters
        // 2. Validate depth limits
        // 3. Validate max children
        // 4. Create spawn session (shared or isolated)
        // 5. Add task message to spawn session
        // 6. Register subagent run
        // 7. Spawn background execution
        // 8. Return "accepted" response immediately
    }
}
```

**Tasks**:
- [ ] Implement full spawn flow
- [ ] Add depth limit validation
- [ ] Add max children validation
- [ ] Return OpenClaw-compatible response format

### 3.2 Response Format
Match OpenClaw's format:
```json
{
  "status": "accepted",
  "childSessionKey": "agent:testagent:subagent:uuid",
  "runId": "run_uuid",
  "note": "auto-announces on completion, do not poll/sleep"
}
```

---

## Phase 4: Integration & Message Deduplication Fix

### 4.1 Fix Message Duplication
**File**: `src/channels/cli.rs`

In `send_single_message_with_session`:
- Remove `session_ctx.add_user_message()` before calling execute
- Remove `session_ctx.add_assistant_message()` after calling execute
- Let the engine handle all message persistence

**Tasks**:
- [ ] Remove duplicate message adds
- [ ] Ensure engine persists messages correctly
- [ ] Test message flow end-to-end

### 4.2 Agent Integration
**File**: `src/agent/agent.rs`

Wire up the new spawn tool with executor:
```rust
pub struct Agent {
    // ... existing fields ...
    pub subagent_executor: Arc<SubagentExecutor>,
    pub subagent_registry: Arc<RwLock<SubagentRegistry>>,
}
```

**Tasks**:
- [ ] Add executor and registry to Agent
- [ ] Create executor during agent initialization
- [ ] Pass to AgentSpawnTool

### 4.3 Manager Integration
**File**: `src/agent/manager.rs`

Shared executor across managed agents:
```rust
pub struct Manager {
    // ... existing fields ...
    subagent_executor: Arc<SubagentExecutor>,
}
```

**Tasks**:
- [ ] Share executor between agents
- [ ] Handle cross-agent spawning

---

## Phase 5: Advanced Features

### 5.1 Depth Limits & Policy
**File**: `src/agents/subagent_policy.rs`

```rust
pub struct SubagentPolicy {
    pub max_spawn_depth: u32,  // default: 1 (no nested spawns)
    pub max_children_per_agent: usize,  // default: 5
}
```

**Tasks**:
- [ ] Read policy from agent config
- [ ] Enforce depth limits
- [ ] Enforce max children
- [ ] Return proper error messages

### 5.2 Cross-Agent Spawning
Allow spawning subagents of different agents:
```rust
pub async fn spawn_cross_agent(
    &self,
    target_agent_id: &str,
    task: &str,
    // ...
) -> Result<SpawnResult>;
```

**Tasks**:
- [ ] Allowlist configuration
- [ ] Cross-agent session management
- [ ] Security checks

### 5.3 Cleanup Policies
Implement proper cleanup:
- **Keep**: Session persists after completion
- **Delete**: Session deleted after result announced

**Tasks**:
- [ ] Implement delete policy
- [ ] Add session cleanup job
- [ ] Handle partial failures

### 5.4 Model Overrides
Allow specifying different models for subagents:
```rust
pub struct SpawnParams {
    pub model: Option<String>,  // e.g., "gpt-4", "claude-3"
    pub thinking: Option<String>,  // thinking level
}
```

**Tasks**:
- [ ] Parse model overrides
- [ ] Apply to subagent session
- [ ] Validate model availability

---

## Phase 6: Testing & Documentation

### 6.1 Unit Tests
- [ ] Subagent registry tests
- [ ] Executor tests
- [ ] Policy enforcement tests
- [ ] Tool response format tests

### 6.2 Integration Tests
- [ ] Spawn with shared context
- [ ] Spawn with isolated context
- [ ] Nested spawn (depth > 1)
- [ ] Max children limit
- [ ] Cleanup policies

### 6.3 E2E Tests
- [ ] Full spawn lifecycle
- [ ] Result announcement
- [ ] Timeout handling
- [ ] Cross-agent spawn

### 6.4 Documentation
- [ ] Architecture overview
- [ ] Tool usage examples
- [ ] Policy configuration
- [ ] Troubleshooting guide

---

## Implementation Order

```
Week 1: Phase 1 (Core Infrastructure)
  - Subagent Registry
  - Result Announcement
  - Session Key Utilities

Week 2: Phase 2 (Async Execution)
  - Subagent Executor
  - Background Task Manager

Week 3: Phase 3 (Tool Rewrite)
  - Agent Spawn Tool v2
  - Response Format

Week 4: Phase 4 (Integration)
  - Fix Message Deduplication
  - Agent/Manager Integration

Week 5: Phase 5 (Advanced Features)
  - Depth Limits
  - Cleanup Policies
  - Model Overrides

Week 6: Phase 6 (Testing & Docs)
  - All tests
  - Documentation
```

---

## Key Design Decisions

### 1. Async Execution
Subagents run in background tasks, not blocking the parent. Results are announced asynchronously.

### 2. Session Ownership
- Parent owns the conversation context
- Child has reference to parent for result announcement
- Cleanup policy determines child session lifetime

### 3. Error Handling
- Validation errors returned immediately (forbidden, depth exceeded)
- Execution errors announced as failed results
- Timeouts handled gracefully

### 4. Isolation Levels
- **Shared**: Child shares parent's base session (sees history)
- **Isolated**: Child gets new base session (fresh start)

---

## Open Questions

1. **Result Format**: What format should subagent results be announced in? (Markdown, JSON, etc.)
2. **Max Concurrent**: Default limit for concurrent subagents? (Suggest: 5)
3. **Timeout Handling**: Default timeout for subagent execution? (Suggest: 300s)
4. **Cross-Agent**: Should cross-agent spawning be allowed by default? (Suggest: no, allowlist required)

---

## Migration Path

1. **Phase 1-2**: New code, doesn't affect existing functionality
2. **Phase 3**: AgentSpawnTool v2 replaces v1 (breaking change for responses)
3. **Phase 4**: Message deduplication fix (behavior change)
4. **Phase 5+**: Additive features

---

## Success Criteria

- [ ] `agent_spawn` creates session, executes task, announces result
- [ ] No message duplication in sessions
- [ ] Depth limits enforced
- [ ] Max children enforced
- [ ] Cleanup policies work
- [ ] All E2E tests pass
- [ ] Documentation complete
