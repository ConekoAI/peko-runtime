# A2A Migration Summary

## The Problem

Currently, Pekobot has **two separate result delivery mechanisms**:

| Tool | Delivery Mechanism | Result Queue |
|------|-------------------|--------------|
| `process` (async) | UnifiedAsyncExecutor | AsyncResultQueueManager ✅ |
| `subagent_spawn` | UnifiedAsyncExecutor | AsyncResultQueueManager ✅ |
| `agent_invoke` (A2A) | EventSubscriber | InvocationRegistry ❌ (separate!) |

This means:
- A2A results don't support queue modes (steer, collect, interrupt)
- Two registries to maintain, test, and debug
- Bidirectional A2A is awkward (who owns the session?)

## OpenClaw's Clean Solution

OpenClaw uses **ONE mechanism** for everything:

```
┌─────────────────────────────────────────────────────┐
│              Unified Delivery                        │
│         (AsyncResultQueueManager)                   │
├─────────────────────────────────────────────────────┤
│  subagent_spawn ──┐                                 │
│  sessions_send ───┼──> Queue / Steer / Collect     │
│  cron jobs ───────┤                                 │
└─────────────────────────────────────────────────────┘
```

Key insight: **A2A is just sending a message to another agent's session.**

## The Migration

### Current (Fragmented)
```rust
// A2A uses separate event system
AgentInvokeTool::execute() {
    // Registers in InvocationRegistry
    // Uses EventSubscriber
    // Results delivered via events
}
```

### Target (Unified)
```rust
// A2A uses same queue as subagent_spawn
SessionsSendTool::execute() {
    // Uses UnifiedAsyncExecutor
    // Results queued in AsyncResultQueueManager
    // Supports steer, collect, interrupt modes
}
```

## Session Ownership Model

| Question | Current | Target (OpenClaw) |
|----------|---------|-------------------|
| Who owns sessions? | Tools create them | Runtime owns ALL |
| A2A target | Agent DID | Session key |
| Bidirectional A2A | Hard | Easy (both have sessions) |

## Migration Plan Overview

```
Phase 1 (Week 1): Foundation ✅ COMPLETE
├── Add SessionMessage to AsyncTaskResult
├── Create SessionsSendTool
└── Update QueueDelivery for A2A

Phase 2 (Week 2): Migration ✅ COMPLETE
├── Deprecate agent_invoke
├── Add backward compatibility shim
└── Update tests

Phase 3 (Week 3): Session Management ✅ COMPLETE
├── Ensure runtime owns all sessions
├── Add session resolution
└── Update session key format

Phase 4 (Week 4): Cleanup
├── Remove deprecated code
├── Update documentation
└── Final testing
```

## Key Benefits

1. **Simplicity**: One delivery mechanism instead of two
2. **Consistency**: A2A supports queue modes (steer, collect, interrupt)
3. **Bidirectional**: Both agents have sessions, can send to each other
4. **Testability**: Single registry to mock
5. **OpenClaw parity**: Easier feature porting

## Files to Modify

| File | Change |
|------|--------|
| `async_tool_framework.rs` | Add SessionMessage variant |
| `tools/sessions_send.rs` | **NEW** - A2A tool replacement |
| `tools/agent_invoke.rs` | Deprecate, add shim |
| `session/manager.rs` | Add session resolution |
| `docs/async-tool-framework.md` | Update documentation |

## Testing Strategy

```rust
#[tokio::test]
async fn test_unified_delivery() {
    // Both tools use same queue
    process.execute_async(...).await?;
    sessions_send.execute_async(...).await?;
    
    // Results in same queue
    let events = queue_manager.process_queue(session_key).await;
    assert_eq!(events.len(), 2);
}
```

## Backward Compatibility

- **v0.3.0**: Add `sessions_send`, deprecate `agent_invoke`
- **v0.4.0**: Remove `agent_invoke` (1 release cycle grace)

Migration shim provided:
```rust
impl AgentInvokeTool {
    async fn execute(&self, params) -> Result<Value> {
        warn!("agent_invoke is deprecated, use sessions_send");
        // Translate and delegate to SessionsSendTool
    }
}
```

## Open Questions

1. **Keep EventSubscriber for system events?** 
   - Yes, but rename to `SystemEventBus` for clarity
   - A2A uses queue, system events use broadcast

2. **A2A session lifecycle?**
   - Ephemeral sessions created on-demand
   - Timeout after idle period

3. **External agents (Coneko)?**
   - Same pattern - proxy session created locally

## References

- Full plan: `A2A-MIGRATION-PLAN.md`
- OpenClaw reference: `openclaw/src/agents/sessions-send-tool.a2a.ts`
