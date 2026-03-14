# GAP-005: Agent-to-Agent Messaging

**Priority:** 🟠 High  
**Status:** ✅ **COMPLETE**  
**Target:** v0.6.0  
**Est. Effort:** 3-5 days  
**Actual:** 2 days

---

## ✅ Implementation Complete

**Tool:** `agent_invoke` (replaces inbox-based `agent_send` concept)

**See:**
- Implementation Plan: `GAP-005-Implementation-Plan.md`
- Documentation: `docs/reference/agent-invoke-tool.md`
- Tests: `tests/agent_invoke_integration.rs`
- E2E Test: `test_scripts/tools/test_agent_invoke_e2e.sh`

**Features Delivered:**
- ✅ Sync mode (blocks for result)
- ✅ Async mode (receipt + events)
- ✅ Timeout handling
- ✅ Error handling (missing targets, timeouts)
- ✅ Thread-safe Agent (Mutex wrapped SqliteMemory)
- ✅ 26 total tests (9 unit + 9 integration + 8 E2E scenarios)
- ✅ Full documentation  

---

## Problem Statement

The Grand Architecture specifies `agent_send` tool for 1-to-1 agent communication. Currently:
- `SessionMessagingTool` exists but uses in-memory registry
- Registry is not shared between agents (per-agent instances)
- No message delivery guarantees
- No inbox/queue for offline agents

---

## Current State

```rust
// src/tools/agent_management.rs
pub struct SessionMessagingTool {
    registry: Arc<SessionRegistry>, // In-memory only!
    agent_did: String,
}

// In src/agent/manager.rs:405
let registry = Arc::new(SessionRegistry::new()); // New instance per agent!
```

Each agent gets its own `SessionRegistry` - messages don't actually reach other agents.

---

## Target State

Per [GRAND_ARCHITECTURE.md section 4.5.1](../GRAND_ARCHITECTURE.md#451-tools-atomic-stateless):

```rust
// agent_send tool delivers message to target agent
agent_send {
    target: "researcher",
    message: "Please research this topic"
}
// Returns: { status: "delivered", message_id: "..." }
```

With proper delivery semantics and inbox support.

---

## Scope

### In Scope
- Shared message router (not per-agent)
- `agent_send` tool with delivery guarantees
- Agent inbox/queue (in-memory for now)
- Message delivery status tracking
- Agent-to-agent session establishment

### Out of Scope (Future)
- Persistent message queue (use MCP later)
- Distributed agent messaging
- Message encryption
- Delivery receipts with proof

---

## Goals

1. **Shared Router**: Single message router shared across all agents
2. **Delivery Guarantees**: At-least-once delivery to running agents
3. **Agent Inbox**: Queue for messages when agent is busy/offline
4. **Session Establishment**: Create sessions between agent pairs
5. **Status Tracking**: Track message delivery status

---

## Proposed Implementation

### Message Router
```rust
// src/agent/messaging.rs
pub struct AgentMessageRouter {
    /// Registered agent inboxes
    inboxes: HashMap<String, mpsc::Sender<AgentMessage>>,
    /// Shared registry for lookup
    registry: Arc<RwLock<LocalRegistry>>,
}

pub struct AgentMessage {
    pub id: String,
    pub from: String,
    pub to: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    pub reply_to: Option<String>,
}

pub enum DeliveryStatus {
    Delivered,
    Queued,
    Failed(String),
}

impl AgentMessageRouter {
    /// Register an agent's inbox
    pub async fn register(&mut self, agent_id: &str, inbox: mpsc::Sender<AgentMessage>);

    /// Send message to agent
    pub async fn send(&self, msg: AgentMessage) -> Result<DeliveryStatus> {
        if let Some(inbox) = self.inboxes.get(&msg.to) {
            inbox.send(msg).await?;
            Ok(DeliveryStatus::Delivered)
        } else {
            // Queue for later or fail
            Err(anyhow!("Agent offline"))
        }
    }

    /// Unregister agent (cleanup)
    pub async fn unregister(&mut self, agent_id: &str);
}
```

### Updated agent_send Tool
```rust
pub struct AgentSendTool {
    router: Arc<RwLock<AgentMessageRouter>>,
}

#[async_trait]
impl Tool for AgentSendTool {
    fn name(&self) -> &str { "agent_send" }

    async fn execute(&self, params: Value) -> Result<Value> {
        let target = params["target"].as_str().ok_or("Missing target")?;
        let message = params["message"].as_str().ok_or("Missing message")?;

        let msg = AgentMessage {
            id: generate_id(),
            from: self.agent_id.clone(),
            to: target.to_string(),
            content: message.to_string(),
            timestamp: Utc::now(),
            reply_to: None,
        };

        let status = self.router.read().await.send(msg).await?;

        Ok(json!({
            "status": match status {
                DeliveryStatus::Delivered => "delivered",
                DeliveryStatus::Queued => "queued",
                DeliveryStatus::Failed(e) => "failed",
            },
            "message_id": msg.id,
        }))
    }
}
```

### Agent Inbox Integration
```rust
// In Agent loop
async fn run_inbox_listener(&self, mut inbox: mpsc::Receiver<AgentMessage>) {
    while let Some(msg) = inbox.recv().await {
        // Inject message into conversation
        self.session.add_system(format!(
            "Message from {}: {}",
            msg.from, msg.content
        )).await;

        // Trigger agent to respond if idle
        if self.is_idle() {
            self.trigger_response().await;
        }
    }
}
```

### Session for Agent Pairs
```rust
// When two agents message each other, create a shared session context
pub struct AgentPairSession {
    pub agent_a: String,
    pub agent_b: String,
    pub session: Arc<RwLock<BaseSession>>,
}
```

---

## Dependencies

- **Requires:** GAP-002 (Async execution for message delivery)
- **Requires:** GAP-003 (Session overlays for agent-pair sessions)
- **Related to:** GAP-007 (Skills use agent_send internally)

---

## Success Criteria

- [ ] Can send message from one agent to another
- [ ] Message is delivered to running agent's inbox
- [ ] Agent receives notification of new message
- [ ] Can check delivery status
- [ ] Messages have unique IDs for tracking
- [ ] Agent-to-agent sessions share context

---

## References

- [GRAND_ARCHITECTURE.md - agent_send Tool](../GRAND_ARCHITECTURE.md#451-tools-atomic-stateless)
- [GRAND_ARCHITECTURE.md - Async Agent Messaging](../GRAND_ARCHITECTURE.md#82-async-agent-messaging)
- Current implementation: `src/tools/agent_management.rs`
- Session tools: `src/tools/session_messaging.rs`
