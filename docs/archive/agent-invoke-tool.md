# Agent Invoke Tool (GAP-005)

The `agent_invoke` tool provides session-based agent-to-agent messaging for Pekobot. It replaces the deprecated inbox-based messaging with a simpler, more reliable architecture.

## Overview

```rust
// Tool name
"agent_invoke"

// Supported modes
"sync"   // Blocks until target returns result
"async"  // Returns receipt, result via EventSubscriber
```

## Architecture

```
┌─────────────┐     agent_invoke      ┌──────────────────┐
│  Agent A    │ ─────────────────────▶│ InvocationService│
│  (invoker)  │                       │  (GAP-005)       │
└─────────────┘                       └──────────────────┘
                                              │
                     ┌────────────────────────┼────────────────────────┐
                     │                        │                        │
                     ▼                        ▼                        ▼
              ┌─────────────┐         ┌─────────────┐         ┌─────────────┐
              │   Sync Mode │         │  Async Mode │         │ EventRouter │
              │             │         │             │         │ (GAP-004)   │
              │  PoolExecute│         │  Emit Event │         │             │
              │  Handler    │         │  (receipt)  │         │  Route to   │
              │             │         │             │         │  subscriber │
              └──────┬──────┘         └─────────────┘         └─────────────┘
                     │
                     ▼
              ┌─────────────┐
              │  Agent B    │
              │  (target)   │
              └─────────────┘
```

## Usage

### Sync Mode

Block until the target agent returns a result:

```json
{
  "target": "researcher",
  "message": "Analyze this data and summarize key findings",
  "mode": "sync",
  "timeout_ms": 30000
}
```

**Response:**
```json
{
  "success": true,
  "status": "completed",
  "result": "The data shows...",
  "duration_ms": 5420,
  "from": "did:peko:researcher",
  "invocation_id": "uuid-123"
}
```

### Async Mode

Return immediately with a receipt, result delivered via event:

```json
{
  "target": "analyzer",
  "message": "Process these logs for anomalies",
  "mode": "async"
}
```

**Response:**
```json
{
  "success": true,
  "status": "accepted",
  "receipt_id": "uuid-456",
  "target": "analyzer",
  "mode": "async",
  "note": "Result will be delivered via event when ready"
}
```

**Event delivered via EventSubscriber:**
```json
{
  "event_type": "agent_invocation_complete",
  "source": "analyzer",
  "payload": {
    "receipt_id": "uuid-456",
    "status": "completed",
    "result_preview": "Found 3 anomalies...",
    "completed_at": "2026-03-13T23:45:00Z"
  }
}
```

## Parameters

| Parameter | Type | Required | Default | Description |
|-----------|------|----------|---------|-------------|
| `target` | string | Yes | - | Agent DID or name to invoke |
| `message` | string | Yes | - | The request/prompt to send |
| `mode` | string | No | "sync" | "sync" or "async" |
| `timeout_ms` | integer | No | 30000 | Timeout for sync mode (max 300000) |
| `context` | object | No | {} | Optional context object |

## Error Handling

### Target Not Found
```json
{
  "success": false,
  "error": "Target agent 'unknown' not found or has no active session"
}
```

### Timeout
```json
{
  "success": false,
  "error": "Timeout waiting for response from 'researcher' after 30000ms"
}
```

### Execution Failure
```json
{
  "success": false,
  "error": "Execution failed: <error details>"
}
```

## Implementation Details

### Thread Safety (Phase 3)

The sync mode implementation required making `Agent` thread-safe:

```rust
// Before: Not Sync (RefCell in rusqlite::Connection)
pub struct Agent {
    memory: Option<SqliteMemory>,
}

// After: Sync (Mutex wrapper)
pub struct Agent {
    memory: Option<Arc<std::sync::Mutex<SqliteMemory>>>,
}
```

This allows the `InvocationService` to execute agents in spawned tasks.

### ExecuteHandler Trait

```rust
#[async_trait]
pub trait ExecuteHandler: Send + Sync {
    async fn execute_on_target(
        &self,
        target: &str,
        prompt: &str,
        timeout_ms: u64,
    ) -> Result<InvocationResponse>;
}
```

The `PoolExecuteHandler` implementation:
1. Looks up target agent in `AgentPool` by DID or name
2. Executes prompt via `AgentHandle::execute()`
3. Returns structured `InvocationResponse`

### Prompt Format

Messages to target agents use this format:

```
AGENT_INVOCATION:{from_did}:{invocation_id}:{timeout_ms}
{message_content}
```

Example:
```
AGENT_INVOCATION:did:peko:alice:uuid-123:30000
Analyze this data and summarize key findings
```

## Migration from SessionMessagingTool

The old `SessionMessagingTool` is deprecated. Migration guide:

| Old (SessionMessagingTool) | New (agent_invoke) |
|---------------------------|-------------------|
| `session_messaging send` | `agent_invoke` with `mode: "async"` |
| `session_messaging read` | Use EventSubscriber to receive results |
| `session_messaging list` | Use `agents_list` + event tracking |

## Testing

Run agent_invoke tests:

```bash
cargo test --lib agent_invoke
```

Tests cover:
- Registry operations
- Message serialization
- Response handling
- Async mode functionality
- Error handling

## Related Components

- **GAP-003**: Session Overlays (base architecture)
- **GAP-004**: Event Router (async result delivery)
- **GAP-005**: Agent-to-Agent Messaging (this implementation)
- `AgentSpawnTool`: For spawning subagents (different use case)

## Future Enhancements

- Persistent invocation queue for offline agents
- Batch invocation support
- Invocation result caching
- Cross-tenant agent invocation
