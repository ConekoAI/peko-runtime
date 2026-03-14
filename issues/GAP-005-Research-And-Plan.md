# GAP-005: Agent-to-Agent Messaging - Research & Implementation Plan

**Status:** Research Complete  
**Priority:** 🟠 High  
**Target:** v0.6.0  
**Est. Effort:** 2-3 days  

---

## Current Foundation Analysis

### What Exists

| Component | Location | Status | Issue |
|-----------|----------|--------|-------|
| `SessionMessagingTool` | `src/tools/session_messaging.rs` | ✅ Functional | Not shared between agents |
| `AgentInbox` | `src/tools/session_messaging.rs` | ✅ Functional | New instance per agent |
| `AgentBroadcastTool` | `src/tools/agent_management.rs` | ✅ Works | Uses ManagerCommand |
| `AgentSpawnTool` | `src/tools/agent_management.rs` | ✅ Works | Session overlay architecture |
| `EventRouter` | `src/orchestration/router.rs` | ✅ Ready | Can route agent messages |
| `EventSubscriber` | `src/orchestration/subscriber.rs` | ✅ Ready | Broadcasts events |

### The Core Problem

In `AgentManager::create_communication_tools()`:

```rust
// Line 405 in src/agent/manager.rs
let registry = Arc::new(AgentInbox::new()); // NEW INSTANCE PER AGENT!
tools.push(Arc::new(SessionMessagingTool::new(
    registry,
    agent_did.to_string(),
)));
```

**Result:** Each agent has their own isolated `AgentInbox`. When Agent A sends to Agent B, it goes into A's inbox, not B's.

### What Works Today

```rust
// ✅ Agent Broadcast (works via ManagerCommand)
agent_broadcast { message: "Hello all" }

// ✅ Subagent Spawn (works via session overlays)
sessions_spawn { agent: "helper", task: "analyze this" }

// ❌ Agent-to-Agent 1:1 messaging (broken)
session_messaging { command: "send", to: "agent_b", message: "hi" }
// Message goes into sender's inbox, not receiver's
```

---

## Target Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                     AgentManager                                │
│  ┌──────────────────────────────────────────────────────────┐  │
│  │         Shared AgentMessageRouter                        │  │
│  │  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐   │  │
│  │  │ Agent A Inbox│  │ Agent B Inbox│  │ Agent C Inbox│   │  │
│  │  │ - messages[] │  │ - messages[] │  │ - messages[] │   │  │
│  │  │ - subscriber │  │ - subscriber │  │ - subscriber │   │  │
│  │  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘   │  │
│  └─────────┼────────────────┼────────────────┼───────────┘  │
│            │                │                │               │
└────────────┼────────────────┼────────────────┼───────────────┘
             │                │                │
    ┌────────┴───────┐   ┌────┴────────┐   ┌───┴────────┐
    │   Agent A      │   │   Agent B   │   │   Agent C  │
    │                │   │             │   │            │
    │ agent_send ────┼───┼─────────────┼───┼────>       │
    │                │   │             │   │            │
    │ session_read<──┼───┼─────────────┼───┼────        │
    └────────────────┘   └─────────────┘   └────────────┘
```

---

## Implementation Plan

### Phase 1: Shared Message Router (Day 1)

**1.1 Create AgentMessageRouter**
```rust
// src/agent/messaging.rs
pub struct AgentMessageRouter {
    /// Inboxes by agent DID
    inboxes: RwLock<HashMap<String, AgentInbox>>,
    /// Event subscriber for integration
    event_subscriber: Option<EventSubscriber>,
}

impl AgentMessageRouter {
    pub fn new() -> Self { ... }
    
    /// Send message to specific agent
    pub async fn send_to(&self, to: &str, message: AgentMessage) -> Result<()>;
    
    /// Register agent inbox
    pub async fn register(&self, agent_did: &str) -> mpsc::Receiver<AgentMessage>;
    
    /// Get messages for agent
    pub async fn get_inbox(&self, agent_did: &str) -> Vec<AgentMessage>;
    
    /// Check if agent has messages
    pub async fn has_messages(&self, agent_did: &str) -> bool;
}
```

**1.2 Update AgentManager**
```rust
pub struct AgentManager {
    // ... existing fields ...
    /// Shared message router for agent-to-agent messaging
    message_router: Arc<AgentMessageRouter>,
}

impl AgentManager {
    pub async fn new() -> Result<(Self, Receiver)> {
        // ... existing code ...
        let message_router = Arc::new(AgentMessageRouter::new());
        
        Ok((Self { 
            // ... existing fields ...
            message_router,
        }, events_rx))
    }
}
```

**1.3 Replace Per-Agent AgentInbox**
```rust
// In create_communication_tools()
// OLD: let registry = Arc::new(AgentInbox::new());
// NEW: Use shared router
let message_router = Arc::clone(&self.message_router);
tools.push(Arc::new(AgentSendTool::new(
    message_router,
    agent_did.to_string(),
)));
tools.push(Arc::new(AgentReceiveTool::new(
    message_router,
    agent_did.to_string(),
)));
```

### Phase 2: New Tools (Day 1-2)

**2.1 Create AgentSendTool**
```rust
/// Tool for sending messages to other agents (1-to-1)
pub struct AgentSendTool {
    router: Arc<AgentMessageRouter>,
    from_did: String,
}

#[async_trait]
impl Tool for AgentSendTool {
    fn name(&self) -> &'static str { "agent_send" }
    
    async fn execute(&self, params: Value) -> Result<Value> {
        let to = params["to"].as_str().ok_or(...)?;
        let message = params["message"].as_str().ok_or(...)?;
        let priority = params["priority"].as_str().unwrap_or("normal");
        
        let msg = AgentMessage {
            id: Uuid::new_v4().to_string(),
            from: self.from_did.clone(),
            to: to.to_string(),
            content: message.to_string(),
            timestamp: Utc::now(),
            priority: priority.parse()?, // normal, high, urgent
            reply_to: params["reply_to"].as_str().map(String::from),
        };
        
        self.router.send_to(to, msg).await?;
        
        Ok(json!({
            "success": true,
            "message_id": msg.id,
            "status": "delivered",
            "timestamp": msg.timestamp
        }))
    }
}
```

**2.2 Create AgentReceiveTool**
```rust
/// Tool for receiving messages from other agents
pub struct AgentReceiveTool {
    router: Arc<AgentMessageRouter>,
    agent_did: String,
}

#[async_trait]
impl Tool for AgentReceiveTool {
    fn name(&self) -> &'static str { "agent_receive" }
    
    async fn execute(&self, params: Value) -> Result<Value> {
        let mode = params["mode"].as_str().unwrap_or("poll");
        
        match mode {
            "poll" => {
                // Return pending messages
                let messages = self.router.get_inbox(&self.agent_did).await;
                Ok(json!({
                    "success": true,
                    "messages": messages,
                    "count": messages.len(),
                    "has_more": !messages.is_empty()
                }))
            }
            "wait" => {
                // Block until message arrives (with timeout)
                let timeout_ms = params["timeout_ms"].as_u64().unwrap_or(30000);
                // Implementation using subscriber channel
            }
            _ => Err(anyhow!("Invalid mode"))
        }
    }
}
```

**2.3 Update Existing Tools**
```rust
// Update AgentBroadcastTool to use message_router
// Update SessionMessagingTool to use message_router (or deprecate)
```

### Phase 3: Event Integration (Day 2)

**3.1 Emit Events on Message Delivery**
```rust
// In AgentMessageRouter::send_to()
if let Some(ref subscriber) = self.event_subscriber {
    let event = SystemEvent::Internal {
        event_type: "agent_message".to_string(),
        source: format!("agent:{}", message.from),
        payload: json!({
            "message_id": message.id,
            "from": message.from,
            "to": message.to,
            "preview": &message.content[..100.min(message.content.len())],
        }),
        timestamp: Utc::now(),
    };
    let _ = subscriber.publish(event);
}
```

**3.2 Support for Message-Driven Triggers**
- Jobs can be triggered by `agent_message` events
- Agents can react to messages via cron/event triggers

### Phase 4: Delivery Guarantees (Day 2-3)

**4.1 Add Message Status Tracking**
```rust
pub struct AgentMessage {
    // ... existing fields ...
    pub status: MessageStatus, // Pending, Delivered, Read
    pub delivered_at: Option<DateTime<Utc>>,
    pub read_at: Option<DateTime<Utc>>,
}

impl AgentMessageRouter {
    /// Get message status
    pub async fn get_status(&self, message_id: &str) -> Option<MessageStatus>;
    
    /// Mark message as read
    pub async fn mark_read(&self, message_id: &str) -> Result<()>;
}
```

**4.2 Create AgentMessageStatusTool**
```rust
/// Tool for checking message delivery status
pub struct AgentMessageStatusTool {
    router: Arc<AgentMessageRouter>,
}

impl Tool for AgentMessageStatusTool {
    fn name(&self) -> &'static str { "agent_message_status" }
    
    async fn execute(&self, params: Value) -> Result<Value> {
        let message_id = params["message_id"].as_str().ok_or(...)?;
        let status = self.router.get_status(message_id).await;
        Ok(json!({ "status": status }))
    }
}
```

### Phase 5: Testing & Integration (Day 3)

**5.1 Unit Tests**
```rust
#[test]
async fn test_message_delivery() {
    let router = Arc::new(AgentMessageRouter::new());
    
    // Register agents
    let mut rx_a = router.register("agent_a").await;
    let mut rx_b = router.register("agent_b").await;
    
    // Send message
    let msg = AgentMessage::new("agent_a", "agent_b", "Hello!");
    router.send_to("agent_b", msg).await.unwrap();
    
    // Verify delivery
    let received = rx_b.recv().await.unwrap();
    assert_eq!(received.content, "Hello!");
    assert_eq!(received.from, "agent_a");
}
```

**5.2 Integration Test**
```rust
#[tokio::test]
async fn test_agent_to_agent_messaging() {
    // Create agent manager
    let (manager, _) = AgentManager::new().await.unwrap();
    
    // Spawn two agents
    let agent_a = manager.spawn(config_a).await.unwrap();
    let agent_b = manager.spawn(config_b).await.unwrap();
    
    // Agent A sends message to Agent B
    let result = agent_a.execute(r#"
        Use agent_send to send "Hello from A" to agent_b
    "#).await.unwrap();
    
    // Agent B receives message
    let inbox = agent_b.execute(r#"
        Use agent_receive to check messages
    "#).await.unwrap();
    
    assert!(inbox.contains("Hello from A"));
}
```

---

## File Changes

| File | Changes |
|------|---------|
| `src/agent/messaging.rs` | NEW - AgentMessageRouter, AgentMessage types |
| `src/agent/manager.rs` | +20 lines - Add message_router field, update constructors |
| `src/tools/agent_send.rs` | NEW - AgentSendTool |
| `src/tools/agent_receive.rs` | NEW - AgentReceiveTool |
| `src/tools/agent_message_status.rs` | NEW - AgentMessageStatusTool |
| `src/tools/mod.rs` | +5 lines - Export new tools |
| `src/tools/agent_management.rs` | -10 lines - Deprecate SessionMessagingTool |

---

## Migration Path

**From SessionMessagingTool to AgentSendTool:**

```rust
// OLD (broken - doesn't actually reach other agent)
session_messaging {
    command: "send",
    to: "did:pekobot:local:agent_b",
    message: "Hello"
}

// NEW (works - uses shared router)
agent_send {
    to: "did:pekobot:local:agent_b",
    message: "Hello",
    priority: "normal"
}
```

**Receiving Messages:**

```rust
// OLD
session_messaging { command: "read", limit: 10 }

// NEW
agent_receive { mode: "poll" }  // or mode: "wait", timeout_ms: 30000
```

---

## Success Criteria

- [ ] Agents can send 1-to-1 messages via `agent_send` tool
- [ ] Messages are delivered to correct recipient (not sender's inbox)
- [ ] `agent_receive` returns messages sent to that agent
- [ ] Delivery status can be tracked via `agent_message_status`
- [ ] Messages trigger `SystemEvent::Internal` events
- [ ] Existing `agent_broadcast` continues to work
- [ ] Existing `agent_spawn` continues to work
- [ ] SessionMessagingTool deprecated (keep for backward compat)
- [ ] 10+ tests covering message delivery scenarios
- [ ] All 479+ existing tests still pass

---

## Dependencies Met

| Dependency | Status | How Used |
|------------|--------|----------|
| GAP-003 Session Overlays | ✅ | Spawn sessions already work |
| GAP-004 Event Router | ✅ | For message event broadcasting |
| GAP-006 Scheduler | ✅ | For message-driven job triggers |

---

*Research completed: 2026-03-13*  
*Plan ready for implementation*
