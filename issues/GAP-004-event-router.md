# GAP-004: Event Router (Orchestration Layer)

**Priority:** 🟠 High  
**Status:** Open  
**Target:** v0.6.0  
**Est. Effort:** 1 week  

---

## Problem Statement

The Grand Architecture specifies an Orchestration Layer with an Event Router that routes external events to agents:
- File system events
- Webhook deliveries
- Internal system events

Currently:
- `EventRouter` exists but only distributes streaming events
- No file system event watcher
- No webhook receiver
- No internal system event queue

---

## Current State

```rust
// src/engine/events.rs
pub struct EventRouter {
    subscribers: Vec<tokio::sync::mpsc::Sender<AgenticEvent>>,
}

impl EventRouter {
    pub async fn emit(&self, event: AgenticEvent) {
        // Only for streaming events, not orchestration
    }
}
```

This is for **streaming visibility**, not **orchestration**.

---

## Target State

Per [GRAND_ARCHITECTURE.md section 4.1.2](../GRAND_ARCHITECTURE.md#412-event-router):

```rust
// EventRouter routes external events to agents
let router = EventRouter::new();
router.register_handler(EventType::FileChanged, |event| {
    // Route to appropriate agent
});
router.register_handler(EventType::WebhookReceived, |event| {
    // Route to webhook agent
});
```

---

## Scope

### In Scope
- Event type definitions (File, Webhook, Internal)
- Event routing logic (which agent handles which event)
- File system watcher integration
- HTTP webhook receiver
- Internal event queue
- Event-to-agent dispatch

### Out of Scope (Future)
- Complex event processing (CEP)
- Event sourcing persistence
- Distributed event routing

---

## Goals

1. **Event Types**: Define system event taxonomy
2. **File Watcher**: Watch filesystem changes and emit events
3. **Webhook Server**: HTTP endpoint for external webhooks
4. **Event Dispatch**: Route events to registered agents
5. **Handler Registration**: Agents register for event types

---

## Proposed Implementation

### Core Types
```rust
// src/orchestration/events.rs
pub enum SystemEvent {
    File {
        path: PathBuf,
        change_type: FileChangeType,
        timestamp: DateTime<Utc>,
    },
    Webhook {
        source: String,
        payload: Value,
        headers: HashMap<String, String>,
    },
    Internal {
        event_type: String,
        payload: Value,
    },
    Timer {
        schedule_id: String,
        fired_at: DateTime<Utc>,
    },
}

pub enum FileChangeType {
    Created,
    Modified,
    Deleted,
    Renamed(PathBuf), // old path
}
```

### Event Router
```rust
// src/orchestration/router.rs
pub struct OrchestrationRouter {
    handlers: HashMap<EventType, Vec<EventHandler>>,
    agent_manager: Arc<AgentManager>,
}

type EventHandler = Box<dyn Fn(SystemEvent) -> Option<AgentAction> + Send + Sync>;

pub enum AgentAction {
    Invoke {
        agent_id: String,
        prompt: String,
        context: HashMap<String, Value>,
    },
    Broadcast {
        message: String,
    },
}

impl OrchestrationRouter {
    pub fn register_handler(
        &mut self,
        event_type: EventType,
        handler: EventHandler,
    );

    pub async fn route_event(&self, event: SystemEvent) -> Result<()> {
        if let Some(handlers) = self.handlers.get(&event.event_type()) {
            for handler in handlers {
                if let Some(action) = handler(event.clone()) {
                    self.execute_action(action).await?;
                }
            }
        }
        Ok(())
    }
}
```

### File Watcher
```rust
// src/orchestration/file_watcher.rs
pub struct FileWatcher {
    watcher: notify::RecommendedWatcher,
    watched_paths: HashMap<PathBuf, WatchConfig>,
}

pub struct WatchConfig {
    pub agent_id: String,
    pub filter: Option<Regex>,
    pub debounce_ms: u64,
}
```

### Webhook Server
```rust
// src/orchestration/webhook.rs
pub struct WebhookServer {
    port: u16,
    routes: HashMap<String, WebhookRoute>,
}

pub struct WebhookRoute {
    pub agent_id: String,
    pub secret: Option<String>, // For HMAC verification
    pub transform: Option<WebhookTransform>,
}
```

---

## Dependencies

- **Related to:** GAP-006 (Scheduler uses timer events)
- **Uses:** GAP-003 (Session overlays for event context)

---

## Success Criteria

- [ ] Can register event handlers for specific event types
- [ ] File watcher emits events on file changes
- [ ] Webhook server receives and routes webhooks
- [ ] Events are dispatched to appropriate agents
- [ ] Agents can register/unregister for event types
- [ ] Event delivery is logged for audit

---

## References

- [GRAND_ARCHITECTURE.md - Event Router](../GRAND_ARCHITECTURE.md#412-event-router)
- [GRAND_ARCHITECTURE.md - Orchestration Layer](../GRAND_ARCHITECTURE.md#41-orchestration-layer)
- Current events: `src/engine/events.rs`
