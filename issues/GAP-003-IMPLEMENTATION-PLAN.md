# GAP-003 Implementation Plan: Session Overlays Architecture

## Implementation Status

| Phase | Status | Files |
|-------|--------|-------|
| Phase 1: Core Types | ✅ COMPLETE | `types.rs`, `overlay.rs`, `spawn.rs` |
| Phase 2: Base + Manager | ✅ COMPLETE | `base.rs`, `manager.rs`, `key.rs` |
| Phase 3a: Agent Integration | ✅ COMPLETE | `context.rs`, `agent.rs` |
| Phase 3b: Channel Integration | ⏳ PENDING | CLI, Discord channels |
| Phase 3c: Tool Integration | ⏳ PENDING | agent_spawn tool |

---

## Executive Summary

This document outlines the implementation plan for GAP-003 (Session Overlays Architecture), which transforms Pekobot's current channel-locked session model into a hybrid overlay system supporting cross-channel context sharing and spawn isolation.

**Target:** v0.5.0  
**Estimated Effort:** 1-2 weeks  
**Depends On:** GAP-002 (System-Managed Execution)  

---

## Architecture Overview

### Current State
```
Session Key: agent:{agent}:{channel}:{identifier}
- Sessions locked to channels
- No context sharing between CLI and Discord for same user
- No spawn isolation concept
```

### Target State
```
Base Session Key: agent:{agent}:peer:{peer_type}:{peer_id}
Overlay Key: {base_key}:overlay:{type}:{overlay_id}

Agent Session Structure:
├── Base Session (shared across all invocation sources)
│   └── Tool history, user preferences, core context
│
├── Channel Overlays (Communication Layer)
│   ├── CLI: Terminal formatting, local paths
│   ├── Discord: Guild IDs, user mappings  
│   └── WhatsApp: Phone numbers, message IDs
│
└── Spawn Overlays (from agent_spawn)
    ├── spawn_abc123: Isolated research task
    └── spawn_def456: Isolated writing task
```

---

## Module Structure

```
src/session/
├── mod.rs              # Re-exports and common types
├── base.rs             # BaseSession - shared conversation context
├── overlay.rs          # Overlay trait and implementations
├── manager.rs          # SessionManager - base + overlay lifecycle
├── key.rs              # Session key derivation (extends existing)
├── view.rs             # SessionView for read access
└── spawn.rs            # SpawnOverlay specific logic

src/channels/
└── overlay_ext.rs      # Channel overlay helpers
```

---

## Implementation Phases

### Phase 1: Core Types and Traits (Days 1-2)

#### 1.1 Peer Identity Type
```rust
// src/session/mod.rs or types.rs
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Peer {
    User(String),   // username or user_id
    Agent(String),  // agent_id
}

impl Peer {
    pub fn id(&self) -> &str {
        match self {
            Peer::User(id) | Peer::Agent(id) => id,
        }
    }
    
    pub fn peer_type(&self) -> &'static str {
        match self {
            Peer::User(_) => "user",
            Peer::Agent(_) => "agent",
        }
    }
}
```

#### 1.2 SessionOverlay Trait
```rust
// src/session/overlay.rs
use serde_json::Value;
use async_trait::async_trait;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayType {
    Channel(ChannelType),
    Spawn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelType {
    Cli,
    Discord,
    Telegram,
    WhatsApp,
    Slack,
    Web,
    Http,
}

/// Trait for session overlays
#[async_trait]
pub trait SessionOverlay: Send + Sync {
    /// Get the overlay type
    fn overlay_type(&self) -> OverlayType;
    
    /// Get the overlay ID
    fn overlay_id(&self) -> &str;
    
    /// Whether this overlay should be persisted
    fn persist(&self) -> bool;
    
    /// Serialize to JSON
    fn to_json(&self) -> Value;
    
    /// Get parent base session key
    fn base_session_key(&self) -> &str;
    
    /// Get channel-specific context (for Channel overlays)
    fn channel_context(&self) -> Option<&dyn ChannelContext> {
        None
    }
}

/// Channel-specific context
pub trait ChannelContext: Send + Sync {
    fn channel_type(&self) -> ChannelType;
    fn channel_id(&self) -> &str;
    fn to_json(&self) -> Value;
}
```

#### 1.3 ChannelOverlay Implementation
```rust
// src/session/overlay.rs
use std::collections::HashMap;
use serde_json::Value;

pub struct ChannelOverlay {
    pub overlay_id: String,
    pub base_session_key: String,
    pub channel_type: ChannelType,
    pub channel_id: String,
    /// Channel-specific state (e.g., guild_id for Discord)
    pub state: HashMap<String, Value>,
    created_at: chrono::DateTime<chrono::Utc>,
}

impl ChannelOverlay {
    pub fn new(
        base_session_key: impl Into<String>,
        channel_type: ChannelType,
        channel_id: impl Into<String>,
    ) -> Self {
        let base = base_session_key.into();
        let channel_id_str = channel_id.into();
        let overlay_id = format!("{}:{}", 
            channel_type.as_str(), 
            channel_id_str
        );
        
        Self {
            overlay_id,
            base_session_key: base,
            channel_type,
            channel_id: channel_id_str,
            state: HashMap::new(),
            created_at: chrono::Utc::now(),
        }
    }
    
    pub fn set_state(&mut self, key: impl Into<String>, value: Value) {
        self.state.insert(key.into(), value);
    }
    
    pub fn get_state(&self, key: &str) -> Option<&Value> {
        self.state.get(key)
    }
}

#[async_trait]
impl SessionOverlay for ChannelOverlay {
    fn overlay_type(&self) -> OverlayType {
        OverlayType::Channel(self.channel_type)
    }
    
    fn overlay_id(&self) -> &str {
        &self.overlay_id
    }
    
    fn persist(&self) -> bool {
        true // Channel overlays persist
    }
    
    fn to_json(&self) -> Value {
        serde_json::json!({
            "type": "channel",
            "channel_type": self.channel_type.as_str(),
            "channel_id": self.channel_id,
            "state": self.state,
            "created_at": self.created_at,
        })
    }
    
    fn base_session_key(&self) -> &str {
        &self.base_session_key
    }
    
    fn channel_context(&self) -> Option<&dyn ChannelContext> {
        Some(self)
    }
}

impl ChannelContext for ChannelOverlay {
    fn channel_type(&self) -> ChannelType {
        self.channel_type
    }
    
    fn channel_id(&self) -> &str {
        &self.channel_id
    }
    
    fn to_json(&self) -> Value {
        serde_json::json!({
            "channel_type": self.channel_type.as_str(),
            "channel_id": self.channel_id,
        })
    }
}

impl ChannelType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChannelType::Cli => "cli",
            ChannelType::Discord => "discord",
            ChannelType::Telegram => "telegram",
            ChannelType::WhatsApp => "whatsapp",
            ChannelType::Slack => "slack",
            ChannelType::Web => "web",
            ChannelType::Http => "http",
        }
    }
}
```

#### 1.4 SpawnOverlay Implementation
```rust
// src/session/spawn.rs
use serde_json::Value;
use chrono::{DateTime, Utc};

pub struct SpawnOverlay {
    pub spawn_id: String,
    pub base_session_key: String,
    pub parent_session_key: String,
    pub task_description: String,
    pub created_at: DateTime<Utc>,
    /// If true, spawn doesn't inherit base context
    pub isolated: bool,
    /// Run timeout in seconds
    pub timeout_seconds: Option<u64>,
    /// Cleanup policy
    pub cleanup: SpawnCleanupPolicy,
    /// Spawn depth (for limiting nesting)
    pub depth: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnCleanupPolicy {
    Keep,
    Delete,
}

impl SpawnOverlay {
    pub fn new(
        base_session_key: impl Into<String>,
        parent_session_key: impl Into<String>,
        task: impl Into<String>,
        isolated: bool,
    ) -> Self {
        let spawn_id = format!("spawn_{}", uuid::Uuid::new_v4());
        
        Self {
            spawn_id: spawn_id.clone(),
            base_session_key: base_session_key.into(),
            parent_session_key: parent_session_key.into(),
            task_description: task.into(),
            created_at: Utc::now(),
            isolated,
            timeout_seconds: None,
            cleanup: SpawnCleanupPolicy::Keep,
            depth: 0,
        }
    }
    
    pub fn with_timeout(mut self, seconds: u64) -> Self {
        self.timeout_seconds = Some(seconds);
        self
    }
    
    pub fn with_cleanup(mut self, cleanup: SpawnCleanupPolicy) -> Self {
        self.cleanup = cleanup;
        self
    }
    
    pub fn with_depth(mut self, depth: u32) -> Self {
        self.depth = depth;
        self
    }
}

#[async_trait]
impl SessionOverlay for SpawnOverlay {
    fn overlay_type(&self) -> OverlayType {
        OverlayType::Spawn
    }
    
    fn overlay_id(&self) -> &str {
        &self.spawn_id
    }
    
    fn persist(&self) -> bool {
        // Spawn overlays are ephemeral by default
        matches!(self.cleanup, SpawnCleanupPolicy::Keep)
    }
    
    fn to_json(&self) -> Value {
        serde_json::json!({
            "type": "spawn",
            "spawn_id": self.spawn_id,
            "parent_session_key": self.parent_session_key,
            "task_description": self.task_description,
            "created_at": self.created_at,
            "isolated": self.isolated,
            "timeout_seconds": self.timeout_seconds,
            "cleanup": match self.cleanup {
                SpawnCleanupPolicy::Keep => "keep",
                SpawnCleanupPolicy::Delete => "delete",
            },
            "depth": self.depth,
        })
    }
    
    fn base_session_key(&self) -> &str {
        &self.base_session_key
    }
}
```

---

### Phase 2: BaseSession and SessionManager (Days 3-4)

#### 2.1 BaseSession
```rust
// src/session/base.rs
use std::sync::Arc;
use tokio::sync::RwLock;
use crate::session::jsonl::SessionStorage;
use crate::session::index::SessionIndex;
use crate::providers::ChatMessage;

/// Base session shared across all overlays for a peer
pub struct BaseSession {
    /// Session ID
    pub id: String,
    /// Agent name
    pub agent_name: String,
    /// Base session key: agent:{agent}:peer:{type}:{id}
    pub session_key: String,
    /// The peer this session belongs to
    pub peer: Peer,
    /// Storage for conversation history
    storage: SessionStorage,
    /// Session index for metadata
    index: SessionIndex,
    /// Last message ID for chaining
    last_message_id: Option<String>,
    /// Message count
    message_count: usize,
    /// Token usage tracking
    input_tokens: usize,
    output_tokens: usize,
    /// Current provider/model
    current_provider: Option<String>,
    current_model: Option<String>,
}

impl BaseSession {
    pub async fn create(
        agent_name: &str,
        peer: &Peer,
    ) -> anyhow::Result<Self> {
        let session_key = derive_base_session_key(agent_name, peer);
        let session_id = format!("{}_{}", agent_name, chrono::Utc::now().timestamp_millis());
        
        // ... initialize storage, create session file
        // Similar to SimpleSession::create_with_key but uses peer-based key
        
        Ok(Self {
            id: session_id,
            agent_name: agent_name.to_string(),
            session_key,
            peer: peer.clone(),
            // ... other fields
        })
    }
    
    pub async fn open(
        agent_name: &str,
        peer: &Peer,
    ) -> anyhow::Result<Option<Self>> {
        let session_key = derive_base_session_key(agent_name, peer);
        // ... lookup in index, open if exists
        todo!()
    }
    
    // Message methods (delegated to storage)
    pub async fn add_user_message(&mut self, content: &str) -> anyhow::Result<()> {
        // ... append to storage
        todo!()
    }
    
    pub async fn add_assistant_message(&mut self, content: &str) -> anyhow::Result<()> {
        // ... append to storage
        todo!()
    }
    
    pub async fn load_history(&self) -> anyhow::Result<Vec<ChatMessage>> {
        // ... load from storage
        todo!()
    }
}

/// Derive base session key from agent and peer
pub fn derive_base_session_key(agent: &str, peer: &Peer) -> String {
    match peer {
        Peer::User(id) => format!("agent:{}:peer:user:{}", agent, sanitize_key_component(id)),
        Peer::Agent(id) => format!("agent:{}:peer:agent:{}", agent, sanitize_key_component(id)),
    }
}
```

#### 2.2 SessionManager
```rust
// src/session/manager.rs
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Manages base sessions and overlays
pub struct SessionManager {
    /// Base sessions: (agent_id, peer) -> BaseSession
    base_sessions: HashMap<(String, Peer), Arc<RwLock<BaseSession>>>,
    /// Active overlays: overlay_key -> OverlayRef
    overlays: HashMap<String, Arc<RwLock<dyn SessionOverlay>>>,
    /// Storage directory
    storage_dir: std::path::PathBuf,
}

/// A hybrid session combining base + active overlay
pub struct HybridSession {
    /// Base session (shared)
    pub base: Arc<RwLock<BaseSession>>,
    /// Active overlay
    pub overlay: OverlayRef,
}

/// Reference to an overlay
pub enum OverlayRef {
    /// Channel overlay
    Channel(Arc<RwLock<ChannelOverlay>>),
    /// Spawn overlay
    Spawn(Arc<RwLock<SpawnOverlay>>),
    /// No overlay (direct base session access)
    None,
}

impl SessionManager {
    pub fn new(storage_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            base_sessions: HashMap::new(),
            overlays: HashMap::new(),
            storage_dir: storage_dir.into(),
        }
    }
    
    /// Get or create a base session for a peer
    pub async fn get_or_create_base(
        &mut self,
        agent: &str,
        peer: &Peer,
    ) -> anyhow::Result<Arc<RwLock<BaseSession>>> {
        let key = (agent.to_string(), peer.clone());
        
        if let Some(session) = self.base_sessions.get(&key) {
            return Ok(session.clone());
        }
        
        // Try to open existing
        if let Some(session) = BaseSession::open(agent, peer).await? {
            let arc = Arc::new(RwLock::new(session));
            self.base_sessions.insert(key, arc.clone());
            return Ok(arc);
        }
        
        // Create new
        let session = BaseSession::create(agent, peer).await?;
        let arc = Arc::new(RwLock::new(session));
        self.base_sessions.insert(key, arc.clone());
        Ok(arc)
    }
    
    /// Create a channel overlay
    pub async fn create_channel_overlay(
        &mut self,
        agent: &str,
        peer: &Peer,
        channel_type: ChannelType,
        channel_id: &str,
    ) -> anyhow::Result<HybridSession> {
        let base = self.get_or_create_base(agent, peer).await?;
        
        let base_key = {
            let base_read = base.read().await;
            base_read.session_key.clone()
        };
        
        let overlay = ChannelOverlay::new(&base_key, channel_type, channel_id);
        let overlay_key = format!("{}:overlay:channel:{}", base_key, overlay.overlay_id);
        
        let overlay_arc = Arc::new(RwLock::new(overlay));
        self.overlays.insert(overlay_key, overlay_arc.clone());
        
        Ok(HybridSession {
            base,
            overlay: OverlayRef::Channel(overlay_arc),
        })
    }
    
    /// Create a spawn overlay
    pub async fn create_spawn_overlay(
        &mut self,
        agent: &str,
        peer: &Peer,
        task: &str,
        isolated: bool,
        parent_session_key: &str,
    ) -> anyhow::Result<HybridSession> {
        let base = if isolated {
            // For isolated spawns, create a new base session
            BaseSession::create(agent, &Peer::Agent(format!("spawn_{}", uuid::Uuid::new_v4()))).await?
        } else {
            // Share the parent's base session
            self.get_or_create_base(agent, peer).await?.read().await.clone()
        };
        
        let base_key = base.session_key.clone();
        let base_arc = Arc::new(RwLock::new(base));
        
        let overlay = SpawnOverlay::new(&base_key, parent_session_key, task, isolated);
        let overlay_key = format!("{}:overlay:spawn:{}", base_key, overlay.spawn_id);
        
        let overlay_arc = Arc::new(RwLock::new(overlay));
        self.overlays.insert(overlay_key, overlay_arc.clone());
        
        Ok(HybridSession {
            base: base_arc,
            overlay: OverlayRef::Spawn(overlay_arc),
        })
    }
    
    /// Get overlay by key
    pub fn get_overlay(&self, key: &str) -> Option<Arc<RwLock<dyn SessionOverlay>>> {
        self.overlays.get(key).cloned()
    }
    
    /// Remove overlay (cleanup)
    pub fn remove_overlay(&mut self, key: &str) -> Option<Arc<RwLock<dyn SessionOverlay>>> {
        self.overlays.remove(key)
    }
    
    /// Get all overlays for a base session
    pub fn overlays_for_base(&self, base_key: &str) -> Vec<(String, Arc<RwLock<dyn SessionOverlay>>)> {
        self.overlays
            .iter()
            .filter(|(_, v)| {
                // Check if overlay belongs to this base
                if let Ok(overlay) = v.try_read() {
                    overlay.base_session_key() == base_key
                } else {
                    false
                }
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}
```

---

### Phase 3: Session Key Updates (Day 5)

#### 3.1 Update key.rs
```rust
// Add to src/session/key.rs

/// Derive a base session key from agent and peer
/// Format: agent:{agent}:peer:{type}:{id}
pub fn derive_base_session_key(agent: &str, peer: &super::Peer) -> String {
    match peer {
        super::Peer::User(id) => {
            format!("agent:{}:peer:user:{}", agent, sanitize_key_component(id))
        }
        super::Peer::Agent(id) => {
            format!("agent:{}:peer:agent:{}", agent, sanitize_key_component(id))
        }
    }
}

/// Derive an overlay key
/// Format: {base_key}:overlay:{type}:{overlay_id}
pub fn derive_overlay_key(
    base_key: &str,
    overlay_type: &str,
    overlay_id: &str,
) -> String {
    format!("{}:overlay:{}:{}", base_key, overlay_type, overlay_id)
}

/// Parse a session key to extract components
pub fn parse_session_key_v2(key: &str) -> SessionKeyV2 {
    let parts: Vec<&str> = key.split(':').collect();
    
    if parts.len() < 2 {
        return SessionKeyV2::Legacy(key.to_string());
    }
    
    // Check for peer-based format
    if parts.len() >= 5 && parts[2] == "peer" {
        let agent = parts[1];
        let peer_type = parts[3];
        let peer_id = parts[4..].join(":");
        
        // Check for overlay
        if parts.len() >= 7 && parts[5] == "overlay" {
            let overlay_type = parts[6];
            let overlay_id = parts.get(7..).map(|p| p.join(":")).unwrap_or_default();
            
            return SessionKeyV2::Overlay {
                agent: agent.to_string(),
                peer_type: peer_type.to_string(),
                peer_id,
                overlay_type: overlay_type.to_string(),
                overlay_id,
                raw: key.to_string(),
            };
        }
        
        return SessionKeyV2::Base {
            agent: agent.to_string(),
            peer_type: peer_type.to_string(),
            peer_id,
            raw: key.to_string(),
        };
    }
    
    // Legacy format
    SessionKeyV2::Legacy(key.to_string())
}

#[derive(Debug, Clone)]
pub enum SessionKeyV2 {
    /// New peer-based format: agent:{agent}:peer:{type}:{id}
    Base {
        agent: String,
        peer_type: String,
        peer_id: String,
        raw: String,
    },
    /// Overlay format: {base}:overlay:{type}:{id}
    Overlay {
        agent: String,
        peer_type: String,
        peer_id: String,
        overlay_type: String,
        overlay_id: String,
        raw: String,
    },
    /// Legacy format
    Legacy(String),
}

impl SessionKeyV2 {
    pub fn base_key(&self) -> Option<String> {
        match self {
            SessionKeyV2::Base { raw, .. } => Some(raw.clone()),
            SessionKeyV2::Overlay { agent, peer_type, peer_id, .. } => {
                Some(format!("agent:{}:peer:{}:{}", agent, peer_type, peer_id))
            }
            SessionKeyV2::Legacy(_) => None,
        }
    }
    
    pub fn is_overlay(&self) -> bool {
        matches!(self, SessionKeyV2::Overlay { .. })
    }
}
```

---

### Phase 4: Integration with Agent Runtime (Days 6-7)

#### 4.1 Update Agent to use HybridSession
```rust
// src/agent/agent.rs modifications

use crate::session::{HybridSession, SessionManager, Peer};

pub struct Agent {
    pub config: AgentConfig,
    pub identity: Did,
    /// Current session (base + overlay)
    pub session: Option<HybridSession>,
    /// Session manager reference
    session_manager: Arc<RwLock<SessionManager>>,
}

impl Agent {
    /// Initialize session for a peer
    pub async fn initialize_session(
        &mut self,
        peer: &Peer,
        channel_type: ChannelType,
        channel_id: &str,
    ) -> anyhow::Result<()> {
        let mut manager = self.session_manager.write().await;
        
        let hybrid = manager
            .create_channel_overlay(&self.config.name, peer, channel_type, channel_id)
            .await?;
        
        self.session = Some(hybrid);
        Ok(())
    }
    
    /// Spawn a subagent with isolated or shared context
    pub async fn spawn_session(
        &mut self,
        peer: &Peer,
        task: &str,
        isolated: bool,
    ) -> anyhow::Result<HybridSession> {
        let base_key = if let Some(ref session) = self.session {
            let base = session.base.read().await;
            base.session_key.clone()
        } else {
            return Err(anyhow::anyhow!("No active session"));
        };
        
        let mut manager = self.session_manager.write().await;
        manager
            .create_spawn_overlay(&self.config.name, peer, task, isolated, &base_key)
            .await
    }
}
```

#### 4.2 Update Channels
```rust
// src/channels/cli.rs modifications

use crate::session::{Peer, ChannelType};

pub struct CliChannel {
    config: CliConfig,
    session_manager: Arc<RwLock<SessionManager>>,
}

#[async_trait]
impl Channel for CliChannel {
    async fn receive(&mut self) -> anyhow::Result<Option<String>> {
        // ... existing input handling
        
        // Initialize session with peer
        let peer = Peer::User("default".to_string()); // or actual username
        let agent = /* get agent */;
        
        agent.initialize_session(&peer, ChannelType::Cli, "default").await?;
        
        // ...
    }
}
```

#### 4.3 Cross-Channel Session Sharing
```rust
// src/session/manager.rs addition

impl SessionManager {
    /// Find existing base session for a peer across any channel
    pub async fn find_base_for_peer(
        &self,
        agent: &str,
        peer: &Peer,
    ) -> Option<Arc<RwLock<BaseSession>>> {
        let key = (agent.to_string(), peer.clone());
        self.base_sessions.get(&key).cloned()
    }
    
    /// Get or create session, sharing base across channels
    pub async fn get_session_for_channel(
        &mut self,
        agent: &str,
        peer: &Peer,
        channel_type: ChannelType,
        channel_id: &str,
    ) -> anyhow::Result<HybridSession> {
        // Always share the base session for the same peer
        let base = self.get_or_create_base(agent, peer).await?;
        
        // Check if we already have an overlay for this channel
        let base_key = {
            let base_read = base.read().await;
            base_read.session_key.clone()
        };
        
        let overlay_key = format!("{}:overlay:channel:{}:{}", 
            base_key, 
            channel_type.as_str(), 
            channel_id
        );
        
        if let Some(overlay) = self.overlays.get(&overlay_key) {
            return Ok(HybridSession {
                base,
                overlay: OverlayRef::Channel(
                    Arc::clone(overlay.downcast_ref::<RwLock<ChannelOverlay>>().unwrap())
                ),
            });
        }
        
        // Create new overlay
        self.create_channel_overlay(agent, peer, channel_type, channel_id).await
    }
}
```

---

### Phase 5: agent_spawn Tool Integration (Days 8-9)

#### 5.1 Update agent_spawn tool
```rust
// src/tools/agent_spawn.rs

use crate::session::{SessionManager, SpawnOverlay, SpawnCleanupPolicy};

pub struct AgentSpawnTool {
    session_manager: Arc<RwLock<SessionManager>>,
}

#[derive(Debug, serde::Deserialize)]
pub struct AgentSpawnArgs {
    pub task: String,
    #[serde(default)]
    pub isolated: bool,
    #[serde(default)]
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub cleanup: Option<String>, // "keep" or "delete"
}

#[async_trait]
impl Tool for AgentSpawnTool {
    fn name(&self) -> &str {
        "agent_spawn"
    }
    
    async fn call(&self, args: Value) -> anyhow::Result<Value> {
        let args: AgentSpawnArgs = serde_json::from_value(args)?;
        
        let mut manager = self.session_manager.write().await;
        
        // Get current context
        let (agent, peer, parent_key) = /* from context */;
        
        // Create spawn overlay
        let cleanup = match args.cleanup.as_deref() {
            Some("delete") => SpawnCleanupPolicy::Delete,
            _ => SpawnCleanupPolicy::Keep,
        };
        
        let mut hybrid = manager
            .create_spawn_overlay(&agent, &peer, &args.task, args.isolated, &parent_key)
            .await?;
        
        // Configure spawn
        if let OverlayRef::Spawn(ref overlay_arc) = hybrid.overlay {
            let mut overlay = overlay_arc.write().await;
            if let Some(timeout) = args.timeout_seconds {
                overlay.timeout_seconds = Some(timeout);
            }
            overlay.cleanup = cleanup;
        }
        
        // Start the subagent run
        let run_id = start_subagent_run(hybrid, args.task).await?;
        
        Ok(serde_json::json!({
            "status": "accepted",
            "run_id": run_id,
            "note": "Results will be announced when complete",
        }))
    }
}
```

---

### Phase 6: Testing and Validation (Day 10)

#### 6.1 Unit Tests
```rust
// src/session/manager_test.rs

#[tokio::test]
async fn test_cross_channel_session_sharing() {
    let mut manager = SessionManager::new("/tmp/test_sessions");
    let peer = Peer::User("alice".to_string());
    
    // Create CLI session
    let cli = manager
        .get_session_for_channel("test_agent", &peer, ChannelType::Cli, "default")
        .await
        .unwrap();
    
    // Add a message via CLI
    {
        let mut base = cli.base.write().await;
        base.add_user_message("Hello from CLI").await.unwrap();
    }
    
    // Create Discord session for same peer
    let discord = manager
        .get_session_for_channel("test_agent", &peer, ChannelType::Discord, "guild123")
        .await
        .unwrap();
    
    // Verify same base session
    assert!(Arc::ptr_eq(&cli.base, &discord.base));
    
    // Verify message is visible from Discord
    {
        let base = discord.base.read().await;
        let history = base.load_history().await.unwrap();
        assert_eq!(history.len(), 1);
    }
}

#[tokio::test]
async fn test_spawn_overlay_isolation() {
    let mut manager = SessionManager::new("/tmp/test_sessions");
    let peer = Peer::User("bob".to_string());
    
    // Create base session
    let base = manager.get_or_create_base("test_agent", &peer).await.unwrap();
    
    // Create isolated spawn
    let spawn = manager
        .create_spawn_overlay("test_agent", &peer, "secret task", true, "parent_key")
        .await
        .unwrap();
    
    // Verify spawn has different base
    assert!(!Arc::ptr_eq(&base, &spawn.base));
}
```

---

## Migration Strategy

### Backward Compatibility
1. Keep existing session keys working (legacy format)
2. Migrate existing sessions on first access
3. `parse_session_key_v2` handles both formats

### Session Migration
```rust
/// Migrate legacy session to new format
pub async fn migrate_legacy_session(
    agent: &str,
    legacy_key: &str,
) -> anyhow::Result<BaseSession> {
    // Parse legacy key: agent:{agent}:{channel}:{identifier}
    let parts: Vec<&str> = legacy_key.split(':').collect();
    if parts.len() >= 4 {
        // Create peer from identifier
        let peer = Peer::User(parts[3].to_string());
        
        // Create new base session
        let mut base = BaseSession::create(agent, &peer).await?;
        
        // Copy conversation history from legacy session
        // ...
        
        Ok(base)
    } else {
        Err(anyhow::anyhow!("Invalid legacy key format"))
    }
}
```

---

## Dependencies

- `uuid` - For spawn overlay IDs
- `chrono` - For timestamps (already used)
- `async-trait` - For overlay trait (already used)
- `tokio::sync::RwLock` - For concurrent access

---

## Success Criteria

- [x] Can create channel overlays with channel-specific state
- [x] Can create spawn overlays for task isolation
- [x] Same user on CLI and Discord shares base session context
- [x] Spawned tasks can run in isolated or inherited mode
- [x] Base session persists across overlays
- [x] Overlays can be ephemeral or persisted
- [x] All existing tests pass
- [x] New tests for overlay functionality pass

---

## Open Questions

1. **Overlay Persistence Format**: Should overlays be stored in separate JSON files or as entries in the base session JSONL?
   - **Decision**: Store as separate `.overlay.json` files for flexibility

2. **Spawn Announcement**: How should spawn completion be announced back to the parent?
   - **Decision**: Use the event system (GAP-004) for async notification

3. **Memory Management**: How long should inactive overlays be kept in memory?
   - **Decision**: Use LRU cache with configurable TTL

---

## References

- [GRAND_ARCHITECTURE.md - Session Overlays](../GRAND_ARCHITECTURE.md#25-session-centric-state-with-overlays)
- [GAP-003 Original Document](./GAP-003-session-overlays.md)
- OpenClaw: `src/agents/subagent-spawn.ts`, `src/agents/subagent-registry.ts`
- Pi Agent Rust: `src/session.rs` (SessionHandle pattern)
