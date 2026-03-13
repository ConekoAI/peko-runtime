# GAP-004: Event Router Implementation Plan

**Status:** Closed  
**Priority:** 🟠 High  
**Target:** v0.6.0  
**Est. Effort:** 5-7 days  
**Dependencies:** GAP-003 (Session Overlays) - ✅ Complete

---

## Overview

This document provides a detailed implementation plan for the Event Router (Orchestration Layer), which enables the system to **proactively invoke agents** based on external events.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                     ORCHESTRATION LAYER                          │
│                                                                  │
│  ┌─────────────┐  ┌─────────────┐  ┌──────────────────────┐    │
│  │ FileWatcher │  │ WebhookSrv  │  │   EventSubscriber    │    │
│  │             │  │             │  │                      │    │
│  │ • notify    │  │ • HTTP      │  │ • Internal events    │    │
│  │ • debounce  │  │ • Routes    │  │ • Cross-module       │    │
│  └──────┬──────┘  └──────┬──────┘  └──────────┬───────────┘    │
│         │                │                    │                │
│         └────────────────┴────────────────────┘                │
│                          │                                     │
│                   ┌──────▼──────┐                              │
│                   │ EventRouter │                              │
│                   │             │                              │
│                   │ • Routing   │                              │
│                   │ • Handlers  │                              │
│                   │ • Dispatch  │                              │
│                   └──────┬──────┘                              │
│                          │                                     │
│                   ┌──────▼──────┐                              │
│                   │AgentManager │                              │
│                   │             │                              │
│                   │ • Invoke    │                              │
│                   │ • Schedule  │                              │
│                   └─────────────┘                              │
└─────────────────────────────────────────────────────────────────┘
```

---

## Implementation Phases

### Phase 1: Core Event Types & Router (Day 1-2)

**Goal:** Define event taxonomy and core routing infrastructure

#### 1.1 Event Types (`src/orchestration/events.rs`)

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// System event types that can trigger agent invocation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SystemEvent {
    /// File system change event
    File {
        path: PathBuf,
        change_type: FileChangeType,
        timestamp: DateTime<Utc>,
    },
    
    /// Webhook received from external system
    Webhook {
        source: String,
        route: String,
        payload: serde_json::Value,
        headers: HashMap<String, String>,
        timestamp: DateTime<Utc>,
    },
    
    /// Internal system event
    Internal {
        event_type: String,
        source: String,
        payload: serde_json::Value,
        timestamp: DateTime<Utc>,
    },
    
    /// Timer/scheduled event (from scheduler)
    Timer {
        schedule_id: String,
        task_id: String,
        fired_at: DateTime<Utc>,
    },
}

/// Types of file changes
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum FileChangeType {
    Created,
    Modified,
    Deleted,
    Renamed { from: PathBuf },
}

impl SystemEvent {
    /// Get the event type as a string for routing
    pub fn event_type(&self) -> &'static str {
        match self {
            SystemEvent::File { .. } => "file",
            SystemEvent::Webhook { .. } => "webhook",
            SystemEvent::Internal { .. } => "internal",
            SystemEvent::Timer { .. } => "timer",
        }
    }
    
    /// Get event timestamp
    pub fn timestamp(&self) -> DateTime<Utc> {
        match self {
            SystemEvent::File { timestamp, .. } => *timestamp,
            SystemEvent::Webhook { timestamp, .. } => *timestamp,
            SystemEvent::Internal { timestamp, .. } => *timestamp,
            SystemEvent::Timer { fired_at, .. } => *fired_at,
        }
    }
}
```

#### 1.2 Event Router (`src/orchestration/router.rs`)

```rust
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::agent::AgentManager;
use crate::orchestration::events::SystemEvent;

/// Handler function type for event processing
type EventHandler = Box<
    dyn Fn(&SystemEvent) -> Option<AgentAction> + Send + Sync
>;

/// Action to take when an event is handled
#[derive(Debug, Clone)]
pub enum AgentAction {
    /// Invoke an agent with a prompt
    Invoke {
        agent_id: String,
        prompt: String,
        context: HashMap<String, serde_json::Value>,
    },
    /// Broadcast to multiple agents
    Broadcast {
        agent_ids: Vec<String>,
        message: String,
    },
    /// Queue for later processing
    Queue {
        queue_name: String,
        event: SystemEvent,
    },
}

/// Event router that dispatches events to appropriate handlers
pub struct EventRouter {
    /// Event type -> handlers mapping
    handlers: RwLock<HashMap<String, Vec<EventHandler>>>,
    /// Agent manager for invoking agents
    agent_manager: Arc<RwLock<AgentManager>>,
    /// Event history for audit/debugging
    event_history: RwLock<Vec<(DateTime<Utc>, SystemEvent)>>,
    /// Maximum history size
    max_history: usize,
}

impl EventRouter {
    /// Create a new event router
    pub fn new(agent_manager: Arc<RwLock<AgentManager>>) -> Self {
        Self {
            handlers: RwLock::new(HashMap::new()),
            agent_manager,
            event_history: RwLock::new(Vec::new()),
            max_history: 1000,
        }
    }
    
    /// Register a handler for a specific event type
    pub async fn register_handler<F>(
        &self,
        event_type: &str,
        handler: F,
    ) where
        F: Fn(&SystemEvent) -> Option<AgentAction> + Send + Sync + 'static,
    {
        let mut handlers = self.handlers.write().await;
        handlers
            .entry(event_type.to_string())
            .or_insert_with(Vec::new)
            .push(Box::new(handler));
        
        info!("Registered handler for event type: {}", event_type);
    }
    
    /// Route an event to appropriate handlers
    pub async fn route_event(&self, event: SystemEvent) -> anyhow::Result<()> {
        let event_type = event.event_type().to_string();
        
        // Log event to history
        self.log_event(&event).await;
        
        info!("Routing event: type={} ", event_type);
        
        // Get handlers for this event type
        let handlers = {
            let handlers = self.handlers.read().await;
            handlers.get(&event_type).cloned()
        };
        
        if let Some(handlers) = handlers {
            for handler in handlers {
                match handler(&event) {
                    Some(action) => {
                        if let Err(e) = self.execute_action(action).await {
                            error!("Failed to execute action: {}", e);
                        }
                    }
                    None => {
                        debug!("Handler returned no action for event");
                    }
                }
            }
        } else {
            warn!("No handlers registered for event type: {}", event_type);
        }
        
        Ok(())
    }
    
    /// Execute an agent action
    async fn execute_action(&self, action: AgentAction) -> anyhow::Result<()> {
        match action {
            AgentAction::Invoke { agent_id, prompt, context } => {
                info!("Invoking agent {} with prompt", agent_id);
                let manager = self.agent_manager.read().await;
                // TODO: Implement agent invocation via AgentManager
                // manager.invoke_agent(&agent_id, &prompt, Some(context)).await?;
                Ok(())
            }
            AgentAction::Broadcast { agent_ids, message } => {
                info!("Broadcasting to {} agents", agent_ids.len());
                for agent_id in agent_ids {
                    let action = AgentAction::Invoke {
                        agent_id,
                        prompt: message.clone(),
                        context: HashMap::new(),
                    };
                    if let Err(e) = self.execute_action(action).await {
                        error!("Failed to broadcast to agent: {}", e);
                    }
                }
                Ok(())
            }
            AgentAction::Queue { queue_name, event } => {
                info!("Queueing event to {}", queue_name);
                // TODO: Implement queueing
                Ok(())
            }
        }
    }
    
    /// Log event to history
    async fn log_event(&self, event: &SystemEvent) {
        let mut history = self.event_history.write().await;
        history.push((Utc::now(), event.clone()));
        
        // Trim history if needed
        if history.len() > self.max_history {
            history.remove(0);
        }
    }
    
    /// Get recent event history
    pub async fn get_history(&self, limit: usize) -> Vec<(DateTime<Utc>, SystemEvent)> {
        let history = self.event_history.read().await;
        history.iter().rev().take(limit).cloned().collect()
    }
}
```

### Phase 2: File Watcher (Day 3)

**Goal:** Watch filesystem changes and emit events

#### 2.1 File Watcher (`src/orchestration/file_watcher.rs`)

```rust
use notify::{Config, Event as NotifyEvent, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::orchestration::events::{FileChangeType, SystemEvent};
use crate::orchestration::router::EventRouter;

/// Configuration for watching a path
#[derive(Debug, Clone)]
pub struct WatchConfig {
    /// Path to watch
    pub path: PathBuf,
    /// Agent to invoke on changes
    pub agent_id: String,
    /// File pattern filter (glob)
    pub filter: Option<String>,
    /// Debounce duration
    pub debounce_ms: u64,
    /// Watch recursively
    pub recursive: bool,
}

/// File watcher that emits system events on file changes
pub struct FileWatcher {
    /// Watcher instances
    watchers: Vec<RecommendedWatcher>,
    /// Event sender
    event_tx: mpsc::Sender<SystemEvent>,
    /// Watch configurations
    configs: HashMap<PathBuf, WatchConfig>,
}

impl FileWatcher {
    /// Create a new file watcher
    pub fn new(event_router: Arc<EventRouter>) -> anyhow::Result<Self> {
        let (event_tx, mut event_rx) = mpsc::channel(100);
        
        // Spawn event forwarding task
        tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                if let Err(e) = event_router.route_event(event).await {
                    error!("Failed to route file event: {}", e);
                }
            }
        });
        
        Ok(Self {
            watchers: Vec::new(),
            event_tx,
            configs: HashMap::new(),
        })
    }
    
    /// Add a path to watch
    pub fn watch(&mut self, config: WatchConfig) -> anyhow::Result<()> {
        let path = config.path.clone();
        let event_tx = self.event_tx.clone();
        let filter = config.filter.clone();
        
        // Create watcher for this path
        let mut watcher = notify::recommended_watcher(
            move |res: Result<NotifyEvent, notify::Error>| {
                match res {
                    Ok(event) => {
                        // Apply filter if specified
                        if let Some(ref pattern) = filter {
                            // Simple glob matching
                            let matches = event.paths.iter().any(|p| {
                                p.to_string_lossy().contains(pattern)
                            });
                            if !matches {
                                return;
                            }
                        }
                        
                        // Convert notify event to SystemEvent
                        for path in &event.paths {
                            let change_type = match event.kind {
                                notify::EventKind::Create(_) => FileChangeType::Created,
                                notify::EventKind::Modify(_) => FileChangeType::Modified,
                                notify::EventKind::Remove(_) => FileChangeType::Deleted,
                                _ => continue,
                            };
                            
                            let system_event = SystemEvent::File {
                                path: path.clone(),
                                change_type,
                                timestamp: Utc::now(),
                            };
                            
                            if let Err(e) = event_tx.try_send(system_event) {
                                error!("Failed to send file event: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        error!("File watcher error: {}", e);
                    }
                }
            }
        )?;
        
        // Start watching
        let recursive_mode = if config.recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        
        watcher.watch(&path, recursive_mode)?;
        
        info!("Started watching: {:?} (recursive={})", path, config.recursive);
        
        self.watchers.push(watcher);
        self.configs.insert(path, config);
        
        Ok(())
    }
    
    /// Stop watching a path
    pub fn unwatch(&mut self, path: &PathBuf) -> anyhow::Result<()> {
        // Note: notify doesn't support unwatching individual paths easily
        // Would need to rebuild watcher without that path
        info!("Unwatch not yet implemented for: {:?}", path);
        Ok(())
    }
    
    /// Get active watch configurations
    pub fn get_watches(&self) -> &HashMap<PathBuf, WatchConfig> {
        &self.configs
    }
}
```

### Phase 3: Webhook Server (Day 4)

**Goal:** HTTP endpoint for receiving external webhooks

#### 3.1 Webhook Server (`src/orchestration/webhook.rs`)

```rust
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

use crate::orchestration::events::SystemEvent;
use crate::orchestration::router::EventRouter;

/// Webhook route configuration
#[derive(Debug, Clone)]
pub struct WebhookRoute {
    /// Route path (e.g., "/github", "/slack")
    pub path: String,
    /// Agent to invoke
    pub agent_id: String,
    /// Optional secret for HMAC verification
    pub secret: Option<String>,
    /// Optional source identifier
    pub source: String,
}

/// Webhook server state
#[derive(Clone)]
pub struct WebhookState {
    pub router: Arc<EventRouter>,
    pub routes: Arc<RwLock<HashMap<String, WebhookRoute>>>,
}

/// Webhook server
pub struct WebhookServer {
    /// Server port
    port: u16,
    /// Routes configuration
    routes: HashMap<String, WebhookRoute>,
    /// Event router
    router: Arc<EventRouter>,
}

impl WebhookServer {
    /// Create a new webhook server
    pub fn new(port: u16, router: Arc<EventRouter>) -> Self {
        Self {
            port,
            routes: HashMap::new(),
            router,
        }
    }
    
    /// Register a webhook route
    pub fn register_route(&mut self, route: WebhookRoute) {
        info!("Registering webhook route: {} -> agent:{}", 
              route.path, route.agent_id);
        self.routes.insert(route.path.clone(), route);
    }
    
    /// Start the webhook server
    pub async fn start(&self) -> anyhow::Result<()> {
        let state = WebhookState {
            router: Arc::clone(&self.router),
            routes: Arc::new(RwLock::new(self.routes.clone())),
        };
        
        let app = Router::new()
            .route("/webhook/:route", post(handle_webhook))
            .route("/health", get(health_check))
            .with_state(state);
        
        let addr = SocketAddr::from(([0, 0, 0, 0], self.port));
        info!("Starting webhook server on {}", addr);
        
        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;
        
        Ok(())
    }
}

/// Handle incoming webhook
async fn handle_webhook(
    State(state): State<WebhookState>,
    Path(route): Path<String>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    debug!("Received webhook on route: {}", route);
    
    // Look up route configuration
    let routes = state.routes.read().await;
    let route_config = match routes.get(&format!("/{}", route)) {
        Some(config) => config.clone(),
        None => {
            warn!("Unknown webhook route: {}", route);
            return (StatusCode::NOT_FOUND, "Unknown route");
        }
    };
    drop(routes);
    
    // Parse payload as JSON
    let payload = match serde_json::from_str(&body) {
        Ok(json) => json,
        Err(_) => serde_json::json!({ "raw": body }),
    };
    
    // Extract headers
    let header_map: HashMap<String, String> = headers
        .iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|s| (k.to_string(), s.to_string()))
        })
        .collect();
    
    // Create system event
    let event = SystemEvent::Webhook {
        source: route_config.source.clone(),
        route: route.clone(),
        payload,
        headers: header_map,
        timestamp: Utc::now(),
    };
    
    // Route the event
    if let Err(e) = state.router.route_event(event).await {
        error!("Failed to route webhook event: {}", e);
        return (StatusCode::INTERNAL_SERVER_ERROR, "Routing failed");
    }
    
    (StatusCode::OK, "Webhook received")
}

/// Health check endpoint
async fn health_check() -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "service": "pekobot-webhook"
    }))
}
```

### Phase 4: Event Subscriber & Integration (Day 5)

**Goal:** Internal event subscription and integration with existing systems

#### 4.1 Event Subscriber (`src/orchestration/subscriber.rs`)

```rust
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, error, info};

use crate::orchestration::events::SystemEvent;
use crate::orchestration::router::EventRouter;

/// Internal event bus for cross-module communication
pub struct EventSubscriber {
    /// Broadcast sender for internal events
    sender: broadcast::Sender<SystemEvent>,
    /// Event router
    router: Arc<EventRouter>,
}

impl EventSubscriber {
    /// Create a new event subscriber
    pub fn new(router: Arc<EventRouter>) -> Self {
        let (sender, _) = broadcast::channel(100);
        
        // Spawn forwarding task
        let router_clone = Arc::clone(&router);
        let mut receiver = sender.subscribe();
        
        tokio::spawn(async move {
            while let Ok(event) = receiver.recv().await {
                if let Err(e) = router_clone.route_event(event).await {
                    error!("Failed to route internal event: {}", e);
                }
            }
        });
        
        Self { sender, router }
    }
    
    /// Publish an internal event
    pub fn publish(&self, event: SystemEvent) -> anyhow::Result<()> {
        self.sender
            .send(event)
            .map_err(|e| anyhow::anyhow!("Failed to publish event: {}", e))?;
        Ok(())
    }
    
    /// Subscribe to internal events
    pub fn subscribe(&self) -> broadcast::Receiver<SystemEvent> {
        self.sender.subscribe()
    }
    
    /// Get the sender for external use
    pub fn sender(&self) -> broadcast::Sender<SystemEvent> {
        self.sender.clone()
    }
}
```

### Phase 5: Configuration & CLI (Day 6-7)

**Goal:** Configuration support and CLI commands

#### 5.1 Configuration

```toml
# config.toml
[orchestration]
enabled = true

[orchestration.webhook]
enabled = true
port = 8080

[[orchestration.webhook.routes]]
path = "/github"
agent_id = "github-handler"
source = "github"
# secret = "${GITHUB_WEBHOOK_SECRET}"

[[orchestration.webhook.routes]]
path = "/slack"
agent_id = "slack-handler"
source = "slack"

[orchestration.file_watcher]
enabled = true

[[orchestration.file_watcher.watches]]
path = "/home/user/workspace"
agent_id = "file-processor"
filter = "*.rs"
recursive = true
debounce_ms = 1000
```

#### 5.2 CLI Commands

```bash
# List active event handlers
pekobot orchestration handlers

# Watch a directory
pekobot orchestration watch /path/to/dir --agent my-agent --pattern "*.md"

# Register webhook route
pekobot orchestration webhook add /custom agent-id --source "custom"

# View recent events
pekobot orchestration events --limit 50

# Replay an event
pekobot orchestration replay <event-id>
```

---

## Integration Points

### With AgentManager

```rust
// In AgentManager::new() or init()
pub async fn init_orchestration(&self) -> anyhow::Result<()> {
    let router = Arc::new(EventRouter::new(self.pool.clone()));
    
    // Register default handlers
    router.register_handler("file", |event| {
        // Default file handler
        Some(AgentAction::Invoke {
            agent_id: "default".to_string(),
            prompt: format!("File changed: {:?}", event),
            context: HashMap::new(),
        })
    }).await;
    
    // Start file watcher
    let mut file_watcher = FileWatcher::new(Arc::clone(&router))?;
    
    // Start webhook server
    let webhook_server = WebhookServer::new(8080, Arc::clone(&router));
    
    Ok(())
}
```

### With Session Overlays (GAP-003)

Events create new session contexts with appropriate overlays:

```rust
// When handling a file event
let session = session_manager
    .create_orchestration_session(
        agent_id,
        OrchestrationContext {
            trigger: TriggerType::FileChange,
            source_path: event.path.clone(),
        }
    )
    .await?;
```

---

## Testing Strategy

### Unit Tests

```rust
#[tokio::test]
async fn test_event_router_routing() {
    let router = EventRouter::new(mock_agent_manager());
    
    let mut received = false;
    router.register_handler("test", |event| {
        received = true;
        None
    }).await;
    
    let event = SystemEvent::Internal { ... };
    router.route_event(event).await.unwrap();
    
    assert!(received);
}
```

### Integration Tests

```rust
#[tokio::test]
async fn test_file_watcher_emits_events() {
    // Create temp directory
    // Watch it
    // Create file
    // Assert event received
}
```

### E2E Tests

```bash
# test_scripts/orchestration/test_event_router.sh
echo "Testing event router..."
pekobot orchestration watch /tmp/test &
pekobot agent start file-agent -M "Monitor /tmp/test"
touch /tmp/test/new_file.txt
# Verify agent was invoked
```

---

## Success Criteria

- [ ] Event types defined and serializable
- [ ] EventRouter can register and invoke handlers
- [ ] FileWatcher emits events on file changes
- [ ] WebhookServer receives and routes HTTP webhooks
- [ ] EventSubscriber enables internal cross-module events
- [ ] CLI commands for management
- [ ] Configuration file support
- [ ] Agent invocation works with session overlays
- [ ] Events are logged for audit
- [ ] All tests pass

---

## Dependencies

| Dependency | Crate | Purpose |
|------------|-------|---------|
| notify | `notify` | File system watching |
| axum | `axum` | HTTP webhook server |
| serde | `serde` | Event serialization |
| chrono | `chrono` | Timestamps |
| tokio | `tokio` | Async runtime |

---

## Timeline

| Day | Phase | Deliverable |
|-----|-------|-------------|
| 1 | Core Types | `SystemEvent` enum, `EventRouter` skeleton |
| 2 | Router | Full routing implementation, handler registration |
| 3 | File Watcher | `FileWatcher` with notify integration |
| 4 | Webhook Server | HTTP server with axum |
| 5 | Subscriber | Internal event bus |
| 6 | Config & CLI | TOML config, CLI commands |
| 7 | Integration & Tests | AgentManager integration, E2E tests |

---

## References

- [GRAND_ARCHITECTURE.md - Event Router](../GRAND_ARCHITECTURE.md#412-event-router)
- [GRAND_ARCHITECTURE.md - Orchestration Layer](../GRAND_ARCHITECTURE.md#41-orchestration-layer)
- [GAP-003 Implementation Plan](./GAP-003-IMPLEMENTATION-PLAN.md) (completed)
- [notify crate docs](https://docs.rs/notify/)
- [axum docs](https://docs.rs/axum/)

---

*Plan created: 2026-03-12*  
*Status: Ready for implementation*
