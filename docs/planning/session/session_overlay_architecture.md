# Session Overlay Architecture (GAP-003)

## Overview

The Session Overlay Architecture is a major redesign of how Pekobot manages conversation context. It enables:

1. **Cross-Channel Session Sharing**: Same peer on different channels shares base conversation history
2. **Subagent Isolation**: Spawn isolated or shared sessions for task-specific work
3. **Flexible State Management**: Channel-specific state separate from conversation history
4. **Backward Compatibility**: Existing session keys continue to work

## Core Concepts

### BaseSession

The `BaseSession` is the foundation of the session system. It contains:

- **Conversation History**: All messages (system, user, assistant, tool results)
- **Token Usage**: Input/output token tracking
- **Persistence**: OpenClaw-compatible JSONL format
- **Peer-Based Keys**: Keys derived from `agent:{name}:peer:{type}:{id}`

```rust
pub struct BaseSession {
    pub id: String,
    pub agent_name: String,
    pub session_key: String,
    pub peer: Peer,
    // ... message storage, token tracking
}
```

### SessionOverlay Trait

All overlays implement the `SessionOverlay` trait:

```rust
#[async_trait]
pub trait SessionOverlay: Send + Sync {
    fn overlay_type(&self) -> OverlayType;
    fn overlay_id(&self) -> &str;
    fn persist(&self) -> bool;
    fn to_json(&self) -> Value;
    fn base_session_key(&self) -> &str;
    fn peer(&self) -> &Peer;
}
```

### ChannelOverlay

Channel-specific state overlay for Discord, CLI, Telegram, etc.

```rust
pub struct ChannelOverlay {
    pub overlay_id: String,
    pub base_session_key: String,
    pub peer: Peer,
    pub channel_type: ChannelType,
    pub channel_id: String,
    pub state: HashMap<String, Value>,  // Channel-specific state
}
```

**Use Cases:**
- Discord guild-specific settings
- CLI preferences (colors, verbosity)
- Web session metadata

### SpawnOverlay

Task-specific overlay for subagent isolation.

```rust
pub struct SpawnOverlay {
    pub overlay_id: String,
    pub base_session_key: String,
    pub spawn_id: String,
    pub parent_session_key: String,
    pub task_description: String,
    pub status: SpawnStatus,  // Created → Running → Completed/Failed/Cancelled
    pub isolated: bool,       // true = new base session, false = inherit parent
    pub depth: u32,           // Spawn nesting level
    pub cleanup: SpawnCleanupPolicy,  // Keep or Delete after completion
}
```

**Use Cases:**
- Isolated analysis tasks (confidential data)
- Shared context research (builds on parent knowledge)
- Nested subagent workflows

### HybridSession

Combines a base session with an optional overlay:

```rust
pub struct HybridSession {
    pub base: Arc<RwLock<BaseSession>>,
    pub overlay: OverlayRef,
}

pub enum OverlayRef {
    Channel(Arc<RwLock<ChannelOverlay>>),
    Spawn(Arc<RwLock<SpawnOverlay>>),
    None,
}
```

### SessionContext

Unified API for session operations:

```rust
pub struct SessionContext {
    pub hybrid: HybridSession,
    pub channel_type: Option<ChannelType>,
    pub is_subagent: bool,
}
```

**Key Methods:**
- `load_history()` - Get conversation history
- `add_user_message()` - Add user message
- `add_assistant_message()` - Add assistant response
- `is_isolated()` - Check if this is an isolated spawn
- `get_channel_state()` - Get channel-specific state
- `set_channel_state()` - Set channel-specific state

## Session Manager

The `SessionManager` manages all sessions and overlays:

```rust
pub struct SessionManager {
    base_sessions: HashMap<(String, Peer), Arc<RwLock<BaseSession>>>,
    channel_overlays: HashMap<String, Arc<RwLock<ChannelOverlay>>>,
    spawn_overlays: HashMap<String, Arc<RwLock<SpawnOverlay>>>,
}
```

### Key Methods

- `get_session_for_channel()` - Get or create channel session
- `create_spawn_overlay()` - Create spawn session
- `get_or_create_base()` - Get or create base session for peer

## Session Key Format (v2)

### Base Session Key
```
agent:{agent_name}:peer:user:{user_id}
agent:{agent_name}:peer:agent:{agent_id}
```

### Overlay Keys
```
{base_key}:overlay:channel:{channel_type}:{channel_id}
{base_key}:overlay:spawn:{spawn_id}
```

### Examples

```rust
// Base session for user "alice" with agent "helper"
"agent:helper:peer:user:alice"

// Discord channel overlay
"agent:helper:peer:user:alice:overlay:channel:discord:guild123"

// Spawn overlay (isolated)
"agent:helper:peer:user:alice:overlay:spawn:task_abc123"
```

## Cross-Channel Sharing

### Scenario: Same User, Different Channels

```
User: alice@example.com
├── CLI Channel (agent:bot:peer:user:alice)
│   └── Messages visible to all channels
├── Discord DM (shares base session ↑)
│   └── Messages visible to all channels
└── Slack DM (shares base session ↑)
    └── Messages visible to all channels
```

### Implementation

```rust
// Same peer, different channels = shared base session
let peer = Peer::User("alice".to_string());

let cli = manager
    .get_session_for_channel("bot", &peer, ChannelType::Cli, "default")
    .await?;

let discord = manager
    .get_session_for_channel("bot", &peer, ChannelType::Discord, "guild123")
    .await?;

// Both share the same base session
assert!(Arc::ptr_eq(&cli.base, &discord.base));
```

## Spawn Isolation Modes

### Shared Context (isolated=false)

```rust
// Parent session
let parent = manager
    .get_session_for_channel("bot", &peer, ChannelType::Cli, "default")
    .await?;

// Spawn shares parent's base session
let spawn = manager
    .create_spawn_overlay("bot", &peer, "Research task", false, "parent_key")
    .await?;

// Both can see the same conversation history
assert!(Arc::ptr_eq(&parent.base, &spawn.base));
```

### Isolated Context (isolated=true)

```rust
// Spawn creates new base session
let isolated_spawn = manager
    .create_spawn_overlay("bot", &peer, "Secret analysis", true, "parent_key")
    .await?;

// Different base sessions = no shared history
assert!(!Arc::ptr_eq(&parent.base, &isolated_spawn.base));
```

## Agent Integration

### Agent Structure

```rust
pub struct Agent {
    // ... existing fields ...
    pub session_manager: Arc<RwLock<SessionManager>>,
    pub session_router: SessionRouter,
}
```

### Getting Session Context

```rust
// Get context for a channel
let peer = Peer::User("alice".to_string());
let ctx = agent
    .get_session_context(&peer, ChannelType::Cli, "default")
    .await?;
```

### Spawning Subagent Sessions

```rust
// Spawn a subagent session
let spawn_ctx = agent
    .spawn_session(&peer, "Research task", false, &parent_key, Some(300))
    .await?;
```

## Agent Spawn Tool

The `agent_spawn` tool allows agents to create subagent sessions:

### Parameters

```json
{
    "task": "Description of the task",
    "label": "Optional label for tracking",
    "isolated": false,
    "timeout_seconds": 300,
    "cleanup": "keep"
}
```

### Example Usage

```rust
// Shared context - can see parent's history
{"task": "Continue research on Rust", "isolated": false}

// Isolated context - fresh session
{"task": "Analyze confidential data", "isolated": true, "cleanup": "delete"}
```

### Response

```json
{
    "success": true,
    "spawn": {
        "id": "spawn_abc123",
        "session_key": "agent:bot:peer:user:alice:overlay:spawn:spawn_abc123",
        "parent_session_key": "agent:bot:peer:user:alice",
        "isolated": false,
        "depth": 1,
        "status": "created"
    }
}
```

## CLI Channel Integration

The CLI channel now uses `SessionContext`:

```rust
pub async fn send_single_message_with_session(
    agent: &Agent,
    message: &str,
    new_session: bool,
) -> Result<String> {
    let peer = Peer::User("default".to_string());
    
    let ctx = if new_session {
        // Clear existing and create new
        agent.clear_cli_sessions().await?;
        agent.get_session_context(&peer, ChannelType::Cli, "default").await?
    } else {
        // Resume existing
        agent.get_session_context(&peer, ChannelType::Cli, "default").await?
    };
    
    // Load history and execute
    let history = ctx.load_history().await?;
    // ... execute with history
}
```

## Testing

### Unit Tests

```bash
# Run all session tests
cargo test --lib session::

# Run with filesystem access
cargo test --lib session:: -- --include-ignored
```

### E2E Tests

```bash
# Session overlay E2E
./test_scripts/session/test_session_overlay_e2e.sh

# Cross-channel sharing E2E
./test_scripts/session/test_cross_channel_sharing_e2e.sh

# Agent spawn tool E2E
./test_scripts/tools/test_agent_spawn_e2e.sh
```

## Migration from v1 Sessions

### Backward Compatibility

Legacy session keys are automatically migrated:

```rust
// Old format (v1)
"agent:testagent:discord:123456"

// New format (v2)
"agent:testagent:peer:user:alice:overlay:channel:discord:123456"
```

The migration happens lazily when sessions are accessed.

## Performance Considerations

1. **Base Session Sharing**: Reduces memory by sharing history across channels
2. **Overlay Isolation**: Channel/spawn state is separate from base
3. **Lazy Loading**: Sessions are loaded from disk only when needed
4. **Arc<RwLock<>>**: Efficient concurrent access to shared base sessions

## Security Considerations

1. **Isolated Spawns**: Confidential tasks run in separate base sessions
2. **Peer-Based Keys**: Prevents session key collisions between users
3. **Cleanup Policies**: Automatic cleanup of sensitive spawn sessions
4. **Depth Limiting**: Prevents infinite spawn recursion

## Future Enhancements

1. **Distributed Sessions**: Share sessions across agent cluster
2. **Encrypted Overlays**: Encrypt sensitive channel/spawn state
3. **Session Federation**: Cross-agent session sharing
4. **Analytics**: Session usage metrics and optimization
