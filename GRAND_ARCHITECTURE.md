# Pekobot Grand Architecture

> A minimal-core, multi-agent runtime with pluggable channels, tools, MCPs, and skills. Zero security guarantees from core—audit trail only.

## 1. Vision

Pekobot is a **runtime shell for AI agents** that executes what you give it, logs everything, and guarantees nothing. It prioritizes:

- **Minimal core**: ~2MB runtime, everything else is user-installed
- **Maximum flexibility**: No sandboxing, no restrictions, full user control
- **Explicit trust**: Security comes from external registries (reputation, reviews), not runtime enforcement
- **Auditability**: Complete execution trail for post-hoc review
- **Developer ergonomics**: Simple TOML config, CLI-first, clear abstractions

## 2. Design Philosophy

### 2.1 Core Provides Mechanisms, Not Policy

The core runtime is **deliberately agnostic** about security:

| Aspect | Core Stance |
|--------|-------------|
| Sandboxing | None |
| Permission system | None |
| Content filtering | None |
| Execution limits | None (user-configurable timeouts) |
| Audit trail | **Complete** (session JSONL, tool call logs) |

**Security is the user's responsibility** after reviewing tool/MCP/skill/channel manifests and external registry reputation.

### 2.2 Two Orthogonal Extension Layers

Pekobot separates **how agents communicate** from **what agents can do**:

```
┌─────────────────────────────────────────────────────────────┐
│           COMMUNICATION LAYER (Channels)                   │
│                                                             │
│  How agents interface with users.                          │
│  Multiple channels per agent supported.                    │
│                                                             │
│  • CLI        - Terminal interface (built-in)              │
│  • HTTP       - Webhook/REST (built-in)                    │
│  • Discord    - Discord bot (plugin)                       │
│  • WhatsApp   - WhatsApp Business (plugin)                 │
│  • TUI        - Custom terminal UI (user-built)            │
│  • Game       - Video game integration (user-built)        │
│                                                             │
│  All channels: pluggable, swappable, multi-instance        │
└─────────────────────────────────────────────────────────────┘
                              │
                              │ Messages flow through
                              ▼
┌─────────────────────────────────────────────────────────────┐
│            CAPABILITY LAYER (Tools/MCPs/Skills)            │
│                                                             │
│  What agents can do. Independent of channels.              │
│  Same capabilities across all channels.                    │
│                                                             │
│  Three-Tier Model:                                         │
│                                                             │
│  ┌─────────────────────────────────────────────────────┐   │
│  │ TIER 0: Bootstrap Tools (built-in, optional)        │   │
│  │ • process    - Shell execution  (⚠️ privileged)     │   │
│  │ • filesystem - File operations  (⚠️ privileged)     │   │
│  │ • fetch      - HTTP requests                        │   │
│  │ • agent_mgmt - Lifecycle control                    │   │
│  └─────────────────────────────────────────────────────┘   │
│                            ↓ Registry install              │
│  ┌─────────────────────────────────────────────────────┐   │
│  │ TIER 1: Tools (atomic, stateless)                   │   │
│  │ • web_search - Search APIs                          │   │
│  │ • calculator - Math operations                      │   │
│  │ • apply_patch - Code patching                       │   │
│  │ Single-purpose, no persistence                      │   │
│  └─────────────────────────────────────────────────────┘   │
│                            ↓ Registry install              │
│  ┌─────────────────────────────────────────────────────┐   │
│  │ TIER 2: MCPs (bundled, stateful)                    │   │
│  │ • browser - CDP connection (multi-function)         │   │
│  │ • database - Connection pool                        │   │
│  │ • email - IMAP/SMTP session                         │   │
│  │ "The Kitchen" - maintains state                     │   │
│  └─────────────────────────────────────────────────────┘   │
│                            ↓ User creates                  │
│  ┌─────────────────────────────────────────────────────┐   │
│  │ TIER 3: Skills (workflows)                          │   │
│  │ • coding_assistant - Multi-step workflows           │   │
│  │ • research_pipeline - Tool orchestration            │   │
│  │ "The Recipe" - combines Tools and MCPs              │   │
│  └─────────────────────────────────────────────────────┘   │
│                                                             │
└─────────────────────────────────────────────────────────────┘
```

**Orthogonality**: An agent can use `web_search` (Tool) on Discord, CLI, or WhatsApp. The capability doesn't care about the channel.

### 2.3 Trust Model

```
┌─────────────────────────────────────────────────────────────┐
│ EXTERNAL REGISTRY (e.g., Pekohub)                          │
│ • Code signing                                              │
│ • Community reputation / reviews                           │
│ • Download statistics                                       │
│ • Security audits (3rd party)                              │
└─────────────────────────────────────────────────────────────┘
                            ↓ User decides to install
┌─────────────────────────────────────────────────────────────┐
│ PEKOBOT CORE                                                │
│ • Downloads channel/tool/MCP/skill                          │
│ • Verifies checksum (if provided)                          │
│ • Logs installation event                                   │
│ • Executes without restrictions                            │
└─────────────────────────────────────────────────────────────┘
                            ↓ Runtime
┌─────────────────────────────────────────────────────────────┐
│ AUDIT TRAIL                                                 │
│ • Every channel message logged                             │
│ • Every tool call logged with full arguments               │
│ • Session transcripts in JSONL                             │
│ • Queryable via `pekobot audit`                            │
└─────────────────────────────────────────────────────────────┘
```

**The user is the security boundary.** Core executes. Registry recommends. User decides. Audit logs for review.

### 2.4 Session-Centric State with Channel Overlays

- **Agents are stateless runtime instances**
- **Base sessions hold shared conversation context** (JSONL files)
- **Channel overlays hold channel-specific context** (isolated or linked)
- **Tools/MCPs are stateless/stateful independently of channels**
- Sessions are portable, inspectable, long-lived

**Hybrid Session Model:**
```
Agent Session Structure:
├── Base Session (shared across all channels)
│   └── Tool history, user preferences, core context
├── Channel Overlay: CLI (optional isolation)
│   └── Terminal formatting, local paths
├── Channel Overlay: Discord (optional isolation)
│   └── Guild IDs, Discord user mappings
└── Channel Overlay: WhatsApp (optional isolation)
    └── Phone numbers, message IDs
```

## 3. System Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                    COMMUNICATION LAYER                          │
│                     (Pluggable Channels)                        │
│                                                                  │
│  ┌─────────────┐  ┌─────────────┐  ┌──────────────────────┐    │
│  │   Built-in  │  │   Registry  │  │       Custom         │    │
│  │             │  │   Channels  │  │      Channels        │    │
│  │ • CLI       │  │             │  │                      │    │
│  │ • HTTP      │  │ • Discord   │  │ • TUI (user-built)   │    │
│  │             │  │ • WhatsApp  │  │ • Game integration   │    │
│  │             │  │ • Telegram  │  │ • Web dashboard      │    │
│  │             │  │ • Slack     │  │ • IoT interface      │    │
│  └─────────────┘  └─────────────┘  └──────────────────────┘    │
│                                                                  │
│  All implement Channel trait. Multi-channel per agent.         │
└─────────────────────────────────────────────────────────────────┘
                              │
                              │ Messages
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                      AGENT RUNTIME                               │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │  Channel Router                                           │   │
│  │  ├─ Route messages from any channel to agent             │   │
│  │  ├─ Manage session overlays (isolated vs shared)         │   │
│  │  └─ Optional broadcast to multiple channels              │   │
│  └──────────────────────────────────────────────────────────┘   │
│                              │                                    │
│  ┌───────────────────────────▼──────────────────────────┐       │
│  │              AgentManager                             │       │
│  │  ├─ AgentPool (running agents)                       │       │
│  │  ├─ LocalRegistry (agent metadata)                   │       │
│  │  └─ LifecycleManager (spawn/stop/restart)            │       │
│  └──────────────────────────────────────────────────────┘       │
│                              │                                    │
│  ┌───────────────────────────▼──────────────────────────┐       │
│  │              Individual Agent                         │       │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────────┐       │       │
│  │  │ Identity │  │  Session │  │   Provider   │       │       │
│  │  │  (DID)   │  │  (JSONL) │  │   (LLM)      │       │       │
│  │  └──────────┘  └──────────┘  └──────────────┘       │       │
│  └──────────────────────────────────────────────────────┘       │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                     EXECUTION ENGINE                             │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │  AgenticLoopV4 (native tool calling)                     │   │
│  │  ├─ Tool/MCP dispatch (independent of channels)          │   │
│  │  ├─ Streaming event generation                           │   │
│  │  └─ Session persistence (JSONL)                         │   │
│  └──────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                  CAPABILITY LAYER                                │
│                                                                  │
│  ┌─────────────┐  ┌─────────────┐  ┌──────────────────────┐    │
│  │   Tools     │  │    MCPs     │  │       Skills         │    │
│  │  (atomic)   │  │  (bundled)  │  │    (workflows)       │    │
│  │             │  │             │  │                      │    │
│  │ • web_search│  │ • browser   │  │ • coding_assistant   │    │
│  │ • calc      │  │ • database  │  │ • research_pipe      │    │
│  │ • patch     │  │ • email     │  │ • deploy_workflow    │    │
│  └─────────────┘  └─────────────┘  └──────────────────────┘    │
│                                                                  │
│  Sources: Built-in (T0) | Registry (T1/T2/T3)                   │
│  Used by: ALL channels (orthogonal)                             │
└─────────────────────────────────────────────────────────────────┘
```

## 4. Component Details

### 4.1 Communication Layer (Channels)

**Channel Trait:**
```rust
#[async_trait]
pub trait Channel: Send + Sync {
    /// Unique channel ID within agent
    fn id(&self) -> &str;
    
    /// Receive message from user
    async fn recv(&mut self) -> Result<Option<Message>>;
    
    /// Send response to user
    async fn send(&mut self, response: Response) -> Result<()>;
    
    /// Stream events (typing indicators, progress, etc.)
    async fn stream(&mut self, events: EventStream) -> Result<()>;
}
```

**Channel Types:**

| Type | Examples | Source |
|------|----------|--------|
| Built-in | CLI, HTTP | Core |
| Registry | Discord, WhatsApp, Slack | Pekohub |
| Custom | TUI, Game mods, WebUI | User-built |

**Multi-Channel Session Model:**

```rust
pub struct AgentChannels {
    /// Base session (shared context across channels)
    base_session: Session,
    
    /// Channel overlays (isolated channel-specific context)
    overlays: HashMap<String, ChannelOverlay>,
    
    /// Channel implementations
    channels: HashMap<String, Box<dyn Channel>>,
}

impl AgentChannels {
    /// Handle message from any channel
    pub async fn handle_message(
        &mut self, 
        channel_id: &str, 
        message: Message
    ) -> Result<()> {
        // Use overlay if isolated, base if shared
        let context = self.get_context(channel_id)?;
        
        // Execute with agent (uses Capability Layer)
        let response = self.agent.execute(message, context).await?;
        
        // Send back to originating channel
        self.channels[channel_id].send(response).await?;
        
        Ok(())
    }
}
```

### 4.2 Memory Architecture

Pekobot separates **immediate context** (built-in) from **long-term memory** (pluggable MCP):

```
┌─────────────────────────────────────────────────────────────┐
│ 1ST ORDER MEMORY (Context) - Built-in, Always Present      │
│                                                             │
│ Session JSONL Files                                         │
│ • Immediate conversation history                           │
│ • Tool call logs                                           │
│ • System events                                            │
│ • Stored in: ~/.pekobot/agents/{agent}/sessions/           │
│                                                             │
│ Access: Automatic (injected into LLM context)              │
│ Lifetime: Session duration                                 │
└─────────────────────────────────────────────────────────────┘
                              │
                              │ Agent explicitly calls
                              ▼
┌─────────────────────────────────────────────────────────────┐
│ 2ND ORDER MEMORY (Long-term) - Pluggable MCP               │
│                                                             │
│ Optional, user-selected backend:                           │
│                                                             │
│ • memory-markdown    - MD files + SQLite vectors           │
│ • memory-postgres    - PostgreSQL + pgvector               │
│ • memory-chroma      - ChromaDB                           │
│ • memory-pinecone    - Pinecone (cloud)                   │
│ • memory-files       - Simple files (no vectors)          │
│ • memory-none        - No long-term memory                │
│                                                             │
│ Access: Via MCP tools (memory.search, memory.write)        │
│ Lifetime: Persistent across sessions                       │
└─────────────────────────────────────────────────────────────┘
```

**Memory MCP Interface:**
```rust
#[async_trait]
pub trait MemoryMCP: Send + Sync {
    /// Store content at path
    async fn write(&self, path: &str, content: &str) -> Result<()>;
    
    /// Read content from path
    async fn read(&self, path: &str) -> Result<String>;
    
    /// Semantic search
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>>;
    
    /// Compact/trim old content
    async fn compact(&self, options: CompactOptions) -> Result<()>;
}
```

**Why Pluggable Memory?**

| Use Case | Recommended MCP |
|----------|-----------------|
| Single user, local-first | `memory-markdown` (OpenClaw-compatible) |
| Team sharing, concurrent | `memory-postgres` |
| Cloud-native, serverless | `memory-pinecone` |
| Embedded, minimal | `memory-files` |
| Stateless agents | `memory-none` |
| Existing Chroma setup | `memory-chroma` |

**Configuration:**
```toml
[mcp]
# Choose your memory backend (optional)
memory = "memory-markdown"

[mcp.memory-markdown]
workspace_dir = "./memory"
embedding_provider = "local"  # or "openai", "gemini"

[mcp.memory-postgres]
connection_string = "postgresql://..."
vector_dimension = 1536

[mcp.memory-none]
# Explicitly disable long-term memory
```

**Key Principle:** The core runtime provides session context (1st order). Long-term memory (2nd order) is an optional capability provided via MCP. Users choose their backend based on their needs—no lock-in.

### 4.3 Bootstrap Components

**Built-in Channels:**
```toml
[bootstrap_channels]
cli = true     # Terminal interface
http = true    # Webhook/REST endpoint
```

**Built-in Tools:**
```toml
[bootstrap_tools]
process = true         # Shell commands - ⚠️ privileged
filesystem = true      # File operations - ⚠️ privileged
fetch = true           # HTTP requests
agent_management = true
session_introspection = true
```

**Installation requirement:** At least one of `filesystem` or `fetch` must be enabled to install additional components from registry.

### 4.4 Capability Tiers

**Tier 1: Tools (atomic, stateless)**
- Single function, no persistence
- Examples: `web_search`, `calculator`, `weather`
- Interface: `async fn execute(args) -> Result<Value>`

**Tier 2: MCPs (bundled, stateful)**
- Multiple related functions from one service
- Maintains connections and state
- Examples: `browser` (CDP), `database` (pool), `email` (IMAP/SMTP), `memory-*` (long-term storage)
- Interface: Stateful struct with multiple methods

**Tier 3: Skills (workflows)**
- Multi-step processes using Tools and MCPs
- Declarative: SKILL.md defines workflow
- Examples: `coding_assistant`, `research_pipeline`

### 4.5 Session Storage

```
~/.pekobot/agents/{agent_name}/
├── config.toml
├── sessions/
│   ├── sessions.json              # Index
│   ├── base.{uuid}.jsonl          # Shared context (all channels)
│   ├── cli.{uuid}.jsonl           # CLI overlay (if isolated)
│   └── discord.{uuid}.jsonl       # Discord overlay (if isolated)
```

**Audit:**
```bash
# Query channel-specific activity
pekobot audit --agent myagent --channel discord_main

# Query tool usage across all channels
pekobot audit --agent myagent --tool web_search

# Query cross-channel context
pekobot audit --agent myagent --session-linkage
```

## 5. Security Model (Explicit)

### 5.1 What Core Does NOT Do

| Security Feature | Core Implementation |
|------------------|---------------------|
| Sandboxing | ❌ None |
| Permission system | ❌ None |
| Code signing enforcement | ❌ None (checksums optional) |
| Content filtering | ❌ None |
| Network restrictions | ❌ None |
| Channel isolation enforcement | ❌ None (user-configurable) |
| Resource limits | ⚠️ Only timeouts (user-configurable) |

### 5.2 What Core DOES Do

| Feature | Implementation |
|---------|----------------|
| Audit logging | ✅ Complete JSONL transcripts |
| Channel message logging | ✅ Every message with channel ID |
| Tool call logging | ✅ Every call with arguments |
| Session isolation | ✅ Configurable per channel |
| Checksum verification | ✅ If provided by registry |
| Manifest display | ✅ Shows requirements before install |

### 5.3 Security Responsibilities

| Layer | Responsibility |
|-------|----------------|
| **External Registry** | Code review, reputation, community ratings, optional audits |
| **Component Author** | Secure coding, clear capability declarations |
| **User** | Review manifests, configure isolation, monitor audit logs |
| **Core** | Execute faithfully, log completely, stay out of the way |

## 6. Extension Registry

### 6.1 Multi-Backend Registry

```rust
enum ExtensionSource {
    BuiltIn,           // Bootstrap channels/tools
    Registry {         // Pekohub or custom registry
        url: String,
        verify: bool,
    },
    Local {            // Development/testing
        path: PathBuf,
    },
}
```

### 6.2 Installation Flow

```
User: pekobot channel install discord
           │
           ▼
    ┌───────────────┐
    │ Query Registry│──→ GET registry.pekohub.io/channels/discord
    └───────┬───────┘
            │
            ▼
    ┌───────────────┐
    │ Show Manifest │──→ "Communication layer plugin. 
    └───────┬───────┘      Requires: [network, persistent_storage]
            │              Reputation: 4.5★ (5k downloads)
            │              Continue? (y/N)"
            ▼
    ┌───────────────┐
    │ User Confirms │──→ y
    └───────┬───────┘
            │
            ▼
    ┌───────────────┐
    │ Download      │──→ Verify checksum
    │ Install       │
    └───────┬───────┘
            │
            ▼
    ┌───────────────┐
    │ Configure     │──→ shared_session? default_channel?
    │ Channel       │
    └───────┬───────┘
            │
            ▼
    ┌───────────────┐
    │ Log Install   │──→ "Installed discord channel v2.1.0"
    └───────────────┘
```

## 7. Configuration Examples

### 7.1 Minimal Agent (single channel)

```toml
# ~/.pekobot/agents/minimal/config.toml
name = "minimal"

[provider]
provider_type = "kimi"

[bootstrap_tools]
process = true
filesystem = true

[[channels]]
id = "cli"
type = "builtin"
enabled = true
shared_session = true
```

### 7.2 Multi-Channel Agent (Discord + CLI)

```toml
name = "social_bot"

[provider]
provider_type = "anthropic"

# Capabilities (used by ALL channels)
[capabilities]
tools = ["web_search", "calculator"]
mcp = ["browser"]
skills = ["coding_assistant"]

# Channels (communication interfaces)
[[channels]]
id = "cli"
type = "builtin"
shared_session = true

[[channels]]
id = "discord_main"
type = "registry"
plugin = "discord"
shared_session = false           # Isolated overlay
config = { guild_id = "123456" }
```

### 7.3 Same Capability, Different Channels

```toml
# Agent uses web_search (Tool) on both channels
# Same capability, different interfaces

[[channels]]
id = "cli"
type = "builtin"
# User types: "Search for Rust tutorials"
# Agent uses web_search, responds in terminal

[[channels]]
id = "discord"
type = "registry"
plugin = "discord"
# User types: !search Rust tutorials
# Agent uses SAME web_search, responds in Discord
```

## 8. Anti-Goals

What Pekobot explicitly avoids:

- **Sandboxing**: Use OS-level isolation (containers, VMs) if needed
- **Enterprise RBAC**: Role-based access is organization-specific
- **Content moderation**: Speech is the user's responsibility
- **Vendor lock-in**: Open protocols, portable sessions
- **Cloud dependency**: Self-hosted by design, cloud optional
- **Capability-Channel coupling**: Tools don't know about channels

## 9. Related Concepts

| Concept | Analogy | Pekobot Equivalent |
|---------|---------|-------------------|
| Unix shell | Command execution | Core runtime |
| apt/npm | Package manager | Extension registry |
| Docker | Isolation | Not provided - use external |
| IRC bouncer | Multi-client presence | Multi-channel agent |
| X11/Wayland | Display server | Channel layer |
| Shell script | Automation | Skills |

## 10. Future Directions

### Near-term (3-6 months)
- Pekohub production with reputation system
- Channel plugin architecture stabilization
- Tool/MCP migration from core to registry
- Multi-channel session management

### Medium-term (6-12 months)
- External trust layer (signed extensions, audits)
- Multi-agent workflows (A2A orchestration)
- Memory MCP ecosystem (markdown, postgres, chroma, pinecone backends)
- Web dashboard channel

### Long-term (12+ months)
- Distributed agent clusters
- WASM-based extensions
- Cross-runtime session portability

---

*Status: Revised per philosophy discussion*
*Last updated: 2026-03-09*
