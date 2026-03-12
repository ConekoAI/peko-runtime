# Session Registry Design - Peer to Session Mapping

## Overview

Implemented a new **Session Registry** that enables:
- **UUID-based session file naming** (OpenClaw-style)
- **Multiple sessions per peer** (user/agent)
- **Session switching** between conversations
- **Session branching** (forking conversations)

## Architecture

### Before (Current)
```
Peer Key → Single File
agent:myagent:peer:user:alice → myagent_user_alice.jsonl

Problem: One file per peer. Can't switch/branch sessions.
```

### After (New Registry)
```
Peer Key → Registry Entry → Multiple Session Files

agent:myagent:peer:user:alice
    ↓
PeerRegistryEntry {
    active_session_id: "uuid-1",
    sessions: {
        "uuid-1": SessionInfo { file: "uuid-1.jsonl", parent: None },
        "uuid-2": SessionInfo { file: "uuid-2.jsonl", parent: None },
        "uuid-3": SessionInfo { file: "uuid-3.jsonl", parent: "uuid-1" },
    }
}
```

## Data Structures

### SessionInfo
```rust
pub struct SessionInfo {
    pub session_id: String,        // UUID
    pub transcript_file: String,   // "{uuid}.jsonl"
    pub created_at: u64,
    pub updated_at: u64,
    pub parent_id: Option<String>, // For branched sessions
    pub label: Option<String>,     // User-defined name
    pub message_count: usize,
    pub archived: bool,
}
```

### PeerRegistryEntry
```rust
pub struct PeerRegistryEntry {
    pub active_session_id: String,
    pub sessions: HashMap<String, SessionInfo>,
}
```

### SessionRegistry
```rust
pub struct SessionRegistry {
    pub peers: HashMap<String, PeerRegistryEntry>,
    pub version: u32,
}
```

## File Layout

```
~/.pekobot/agents/myagent/sessions/
├── registry.json                    ← New: Peer → Session mapping
├── {uuid-1}.jsonl                   ← Session files (UUID-named)
├── {uuid-2}.jsonl
├── {uuid-3}.jsonl
└── sessions.json                    ← Existing: Session index (for compatibility)
```

### registry.json Example
```json
{
    "version": 1,
    "peers": {
        "agent:myagent:peer:user:alice": {
            "active_session_id": "550e8400-e29b-41d4-a716-446655440000",
            "sessions": {
                "550e8400-e29b-41d4-a716-446655440000": {
                    "session_id": "550e8400-e29b-41d4-a716-446655440000",
                    "transcript_file": "550e8400-e29b-41d4-a716-446655440000.jsonl",
                    "created_at": 1773297887905,
                    "updated_at": 1773297887905,
                    "parent_id": null,
                    "label": "Main conversation",
                    "message_count": 42,
                    "archived": false
                },
                "6ba7b810-9dad-11d1-80b4-00c04fd430c8": {
                    "session_id": "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
                    "transcript_file": "6ba7b810-9dad-11d1-80b4-00c04fd430c8.jsonl",
                    "created_at": 1773298000000,
                    "updated_at": 1773298000000,
                    "parent_id": null,
                    "label": "Side project",
                    "message_count": 15,
                    "archived": false
                }
            }
        }
    }
}
```

## Operations

### /new - Create New Session
```rust
// Creates new UUID session, switches to it
let session_id = registry_manager.create_new(peer_key).await?;
// Creates empty {uuid}.jsonl file
```

### /branch - Branch Current Session
```rust
// Forks current session (copies file, sets parent)
let new_id = registry_manager.branch(peer_key, Some("feature-x")).await?;
// Copies parent transcript, sets parent_id in SessionInfo
```

### /switch - Switch Active Session
```rust
// Changes active_session_id pointer
registry_manager.switch_session(peer_key, "uuid-2").await?;
// Next message uses uuid-2.jsonl
```

### List Sessions
```rust
let sessions = registry_manager.list_sessions(peer_key).await?;
// Returns all non-archived sessions for the peer
```

## API - SessionRegistryManager

```rust
pub struct SessionRegistryManager {
    path: PathBuf,           // registry.json path
    sessions_dir: PathBuf,   // Directory containing session files
}

impl SessionRegistryManager {
    /// Create manager for an agent
    pub async fn for_agent(agent_name: &str) -> Result<Self>;
    
    /// Get active session ID for a peer
    pub async fn get_active_session_id(&self, peer_key: &str) -> Result<Option<String>>;
    
    /// Create new session (/new)
    pub async fn create_new(&self, peer_key: &str) -> Result<String>;
    
    /// Branch current session (/branch)
    pub async fn branch(&self, peer_key: &str, label: Option<String>) -> Result<String>;
    
    /// Switch to different session (/switch)
    pub async fn switch_session(&self, peer_key: &str, session_id: &str) -> Result<()>;
    
    /// List all sessions for a peer
    pub async fn list_sessions(&self, peer_key: &str) -> Result<Vec<SessionInfo>>;
}
```

## Integration with SessionManager

The `SessionManager` needs to be updated to:

1. **Use SessionRegistryManager** to resolve peer → session file
2. **On message**: Get active session ID, open that transcript file
3. **On /new**: Call `registry_manager.create_new()`
4. **On /branch**: Call `registry_manager.branch()`
5. **On /switch**: Call `registry_manager.switch_session()`

```rust
// Current (peer-based file naming)
let base = BaseSession::open(agent_name, peer).await?;

// New (registry-based file naming)
let peer_key = derive_base_session_key(agent_name, peer);
let session_id = registry_manager.get_active_session_id(&peer_key).await?;
let base = BaseSession::open_by_key(agent_name, &session_key).await?;
```

## Benefits

| Feature | Before | After |
|---------|--------|-------|
| **File naming** | `{agent}_{peer}.jsonl` | `{uuid}.jsonl` ✅ |
| **Sessions/peer** | 1 | Unlimited ✅ |
| **Session switching** | ❌ No | ✅ Yes |
| **Session branching** | ❌ No | ✅ Yes |
| **Session labels** | ❌ No | ✅ Yes |
| **Parent tracking** | ❌ No | ✅ Yes (for branches) |
| **Soft delete** | ❌ No | ✅ Yes (archive) |

## Migration Path

1. **Phase 1**: Add registry alongside existing index
2. **Phase 2**: Migrate existing peer-based files to registry
3. **Phase 3**: Update SessionManager to use registry
4. **Phase 4**: Add CLI commands (/new, /branch, /switch, /sessions)

## Implementation Status

✅ **Completed:**
- `SessionRegistry` data structures
- `SessionRegistryManager` for persistence
- `/new`, `/branch`, `/switch` operations
- Unit tests

⏳ **Pending:**
- CLI command integration
- SessionManager integration
- E2E tests
- Migration from existing sessions
