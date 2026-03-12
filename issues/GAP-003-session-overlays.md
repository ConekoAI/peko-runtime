# GAP-003: Session Overlays Architecture

**Priority:** 🔴 Critical  
**Status:** In Progress (Phase 3a Complete)  
**Target:** v0.5.0  
**Est. Effort:** 1-2 weeks  

---

## Problem Statement

The Grand Architecture specifies a hybrid session model with:
- **Base Session**: Shared conversation context
- **Channel Overlays**: Communication-specific state
- **Spawn Overlays**: Sub-task isolation

Currently:
- Only simple `SimpleSession` exists
- No overlay concept
- Sessions are isolated per channel (no cross-channel sharing)
- No spawn isolation

---

## Current State

```rust
// src/engine/simple_session.rs
pub struct SimpleSession {
    pub id: String,
    pub agent_name: String,
    pub session_key: Option<String>,
    storage: SessionStorage,
    // ... no overlays
}
```

Session keys include channel: `agent:{agent}:{channel}:{identifier}` locks session to channel.

---

## Target State

Per [GRAND_ARCHITECTURE.md section 2.5](../GRAND_ARCHITECTURE.md#25-session-centric-state-with-overlays):

```
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

## Scope

### In Scope
- Base session + overlay data model
- Channel overlay types (CLI, Discord, etc.)
- Spawn overlay lifecycle
- Cross-channel session sharing (same peer = same base session)
- Overlay persistence/isolation policies

### Out of Scope (Future)
- Overlay synchronization across distributed nodes
- Complex overlay merging strategies
- Overlay access control

---

## Goals

1. **Overlay Types**: Define overlay structure and semantics
2. **Session Manager**: Manage base sessions and overlays
3. **Cross-Channel**: Same peer on different channels shares base session
4. **Spawn Isolation**: Spawned tasks get isolated overlays
5. **Persistence**: Base session persists, overlays may be ephemeral

---

## Proposed Implementation

### Core Types
```rust
// src/session/overlay.rs
pub struct HybridSession {
    /// Base session (shared)
    base: Arc<RwLock<BaseSession>>,
    /// Active overlay (channel or spawn)
    active_overlay: OverlayRef,
    /// All overlays for this session
    overlays: HashMap<OverlayId, Arc<RwLock<dyn SessionOverlay>>>,
}

pub trait SessionOverlay: Send + Sync {
    fn overlay_type(&self) -> OverlayType;
    fn persist(&self) -> bool; // Whether to persist
    fn to_json(&self) -> Value;
    fn from_json(value: Value) -> Result<Self> where Self: Sized;
}

pub enum OverlayType {
    Channel(ChannelType),
    Spawn { parent: String, task: String },
}

pub struct ChannelOverlay {
    pub channel_type: ChannelType,
    pub channel_id: String,
    pub channel_specific: HashMap<String, Value>, // Guild IDs, etc.
}

pub struct SpawnOverlay {
    pub spawn_id: String,
    pub parent_session: String,
    pub task_description: String,
    pub created_at: DateTime<Utc>,
    pub isolated: bool, // If true, doesn't inherit base context
}
```

### Session Manager
```rust
// src/session/manager.rs
pub struct SessionManager {
    /// Base sessions by (agent_id, peer)
    base_sessions: HashMap<(String, Peer), Arc<RwLock<BaseSession>>>,
    /// Active overlays
    overlays: HashMap<SessionKey, Arc<RwLock<dyn SessionOverlay>>>,
}

impl SessionManager {
    /// Get or create base session for peer
    pub async fn get_base_session(
        &mut self,
        agent: &str,
        peer: &Peer,
    ) -> Arc<RwLock<BaseSession>>;

    /// Create channel overlay
    pub async fn create_channel_overlay(
        &mut self,
        base: Arc<RwLock<BaseSession>>,
        channel: ChannelType,
        channel_id: &str,
    ) -> Result<HybridSession>;

    /// Create spawn overlay
    pub async fn create_spawn_overlay(
        &mut self,
        parent: Arc<RwLock<BaseSession>>,
        task: &str,
        isolated: bool,
    ) -> Result<HybridSession>;
}
```

### Session Key Format Update
```rust
// New format: separate peer from channel
// Base session key: agent:{agent}:peer:{peer_type}:{peer_id}
// Overlay key: {base_key}:overlay:{overlay_type}:{overlay_id}

pub fn derive_base_key(agent: &str, peer: &Peer) -> String {
    match peer {
        Peer::User(id) => format!("agent:{}:peer:user:{}", agent, id),
        Peer::Agent(id) => format!("agent:{}:peer:agent:{}", agent, id),
    }
}
```

---

## Dependencies

- **Requires:** GAP-002 (Async execution for spawn overlays)
- **Required by:** GAP-009 (Cross-channel session sharing)
- **Related to:** GAP-005 (Agent messaging uses peer-based sessions)

---

## Implementation Progress

### Phase 1: Core Types ✅
- [x] `Peer` enum (User/Agent)
- [x] `ChannelType` enum
- [x] `SessionOverlay` trait
- [x] `ChannelOverlay` implementation
- [x] `SpawnOverlay` implementation

### Phase 2: Base + Manager ✅
- [x] `BaseSession` - shared conversation context
- [x] `SessionManager` - overlay lifecycle
- [x] `HybridSession` - base + overlay combo
- [x] Peer-based session key derivation
- [x] Cross-channel session sharing

### Phase 3a: Agent Integration ✅
- [x] `SessionContext` - unified session interface
- [x] `SessionRouter` - message routing
- [x] Agent session manager integration
- [x] Agent helper methods

### Phase 3b: Channel Integration ⏳
- [ ] Update CLI channel
- [ ] Update Discord channel

### Phase 3c: Tool Integration ⏳
- [ ] Update agent_spawn tool

---

## Success Criteria

- [x] Can create channel overlays with channel-specific state
- [x] Can create spawn overlays for task isolation
- [x] Same user on CLI and Discord shares base session context
- [x] Spawned tasks can run in isolated or inherited mode
- [x] Base session persists across overlays
- [x] Overlays can be ephemeral or persisted

---

## References

- [GRAND_ARCHITECTURE.md - Session Overlays](../GRAND_ARCHITECTURE.md#25-session-centric-state-with-overlays)
- [GRAND_ARCHITECTURE.md - Channel & Session Routing](../GRAND_ARCHITECTURE.md#46-channel--session-routing)
- Current session: `src/engine/simple_session.rs`
- Session keys: `src/session/key.rs`
