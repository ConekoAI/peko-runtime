# GAP-009: Cross-Channel Session Sharing

**Priority:** 🟡 Medium  
**Status:** Open  
**Target:** v0.7.0  
**Est. Effort:** 3-5 days  

---

## Problem Statement

The Grand Architecture specifies that the same peer on different channels shares the same session context. Currently:
- Session keys include channel: `agent:{agent}:{channel}:{identifier}`
- Sessions are isolated per channel
- User loses context when switching from CLI to Discord

---

## Current State

```rust
// src/session/key.rs
pub fn derive_session_key(agent: &str, scope: SessionScope, ctx: &SessionContext) -> String {
    match scope {
        SessionScope::PerSender => {
            let channel = ctx.channel.as_deref().unwrap_or("unknown");
            let sender = ctx.sender_id.as_deref().unwrap_or("anonymous");
            format!("agent:{}:{}:{}", agent, channel, sender)
            // ^^ Channel is part of key - sessions are isolated!
        }
        // ...
    }
}
```

Same user on CLI and Discord gets different session keys → different sessions.

---

## Target State

Per [GRAND_ARCHITECTURE.md section 4.6](../GRAND_ARCHITECTURE.md#46-channel--session-routing):

```
User "alice"
     │
     ├──► CLI ──► Session A ──► Agent ──► Reply CLI
     │
     ├──► Discord ──► Session A ──► Agent ──► Reply Discord
     │
     └──► WhatsApp ──► Session A ──► Agent ──► Reply WhatsApp

Same session context across all channels!
```

---

## Scope

### In Scope
- Peer-based session lookup (not channel-based)
- Channel overlays for channel-specific state
- Session key format change
- Migration for existing sessions

### Out of Scope (Future)
- Complex session merging
- Cross-instance session sync
- Session access control per channel

---

## Goals

1. **Peer-Based Sessions**: Same peer = same base session regardless of channel
2. **Channel Overlays**: Each channel adds its own overlay
3. **Seamless Switching**: User maintains context across channels
4. **Reply Routing**: Responses go to source channel

---

## Proposed Implementation

### New Session Key Format
```rust
// src/session/key.rs

// OLD: agent:{agent}:{channel}:{identifier}
// NEW: agent:{agent}:peer:{peer_type}:{peer_id}

pub enum Peer {
    User { id: String, username: Option<String> },
    Agent { did: String },
}

pub fn derive_base_session_key(agent: &str, peer: &Peer) -> String {
    match peer {
        Peer::User { id, .. } => format!("agent:{}:peer:user:{}", agent, id),
        Peer::Agent { did } => format!("agent:{}:peer:agent:{}", agent, did),
    }
}

// Overlay keys add channel info
pub fn derive_overlay_key(base_key: &str, channel: &str, channel_id: &str) -> String {
    format!("{}:overlay:{}:{}", base_key, channel, channel_id)
}
```

### Session Resolution
```rust
// src/session/manager.rs
pub struct SessionManager {
    /// Base sessions indexed by peer
    base_sessions: HashMap<String, Arc<RwLock<BaseSession>>>,
    /// Channel overlays
    overlays: HashMap<String, Arc<RwLock<ChannelOverlay>>>,
}

impl SessionManager {
    /// Get or create session for peer across any channel
    pub async fn get_session(
        &mut self,
        agent: &str,
        peer: &Peer,
        source_channel: &ChannelInfo,
    ) -> Result<HybridSession> {
        // Get base session for peer (channel-independent)
        let base_key = derive_base_session_key(agent, peer);
        let base = self.base_sessions
            .entry(base_key.clone())
            .or_insert_with(|| Arc::new(RwLock::new(BaseSession::new())))
            .clone();

        // Get or create channel overlay
        let overlay_key = derive_overlay_key(&base_key, &source_channel.channel_type, &source_channel.id);
        let overlay = self.overlays
            .entry(overlay_key)
            .or_insert_with(|| Arc::new(RwLock::new(ChannelOverlay::new(source_channel))))
            .clone();

        Ok(HybridSession::new(base, overlay, source_channel.clone()))
    }
}
```

### Routing Responses
```rust
// src/channels/mod.rs
pub struct HybridSession {
    base: Arc<RwLock<BaseSession>>,
    overlay: Arc<RwLock<ChannelOverlay>>,
    source_channel: ChannelInfo, // Remember where message came from
}

impl HybridSession {
    /// Send response to the channel where message originated
    pub async fn reply(&self, message: &str) -> Result<()> {
        let channel = self.source_channel.get_channel().await?;
        channel.send(message).await
    }

    /// Send to specific channel (cross-channel communication)
    pub async fn send_to(&self, channel: &str, message: &str) -> Result<()> {
        // Lookup channel and send
    }
}
```

### Identity Mapping
```rust
// Need to map channel-specific IDs to unified peer IDs
pub struct IdentityMapper {
    /// Map: channel_type:channel_user_id -> unified_user_id
    mappings: HashMap<String, String>,
}

impl IdentityMapper {
    pub fn resolve_peer(&self, channel: &str, channel_id: &str) -> Peer {
        let key = format!("{}:{}", channel, channel_id);
        if let Some(unified_id) = self.mappings.get(&key) {
            Peer::User { id: unified_id.clone(), username: None }
        } else {
            // Create new mapping
            Peer::User { id: channel_id.to_string(), username: None }
        }
    }
}
```

### Configuration
```toml
# config.toml
[sessions]
cross_channel = true  # Enable cross-channel session sharing

# Optional: Map identities across channels
[[identity_mappings]]
user_id = "alice"
discord = "discord:12345678"
whatsapp = "whatsapp:+1234567890"
```

---

## Dependencies

- **Requires:** GAP-003 (Session overlays for channel-specific state)
- **Related to:** GAP-010 (Channel plugins need to support overlay model)

---

## Migration Strategy

1. **New sessions**: Use new peer-based key format
2. **Existing sessions**: Keep old format, migrate on resume
3. **Config flag**: `cross_channel = false` to keep old behavior during transition

---

## Success Criteria

- [ ] Same user on CLI and Discord shares base session context
- [ ] Channel-specific state is isolated in overlays
- [ ] Agent remembers context from previous channel interaction
- [ ] Responses are routed to correct source channel
- [ ] Migration path for existing sessions

---

## References

- [GRAND_ARCHITECTURE.md - Channel & Session Routing](../GRAND_ARCHITECTURE.md#46-channel--session-routing)
- [GRAND_ARCHITECTURE.md - Cross-Channel Same Session](../GRAND_ARCHITECTURE.md#46-channel--session-routing)
- Current session keys: `src/session/key.rs`
