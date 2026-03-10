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

### 2.2 Three Orthogonal Extension Layers

Pekobot separates concerns along three independent axes:

| Axis | Direction | Who Controls | Purpose |
|------|-----------|--------------|---------|
| **Orchestration** | System → Agent | Core/System | *When* agents run |
| **Communication** | External → Agent | Users | *How* users talk to agents |
| **Capabilities** | Agent → Service | Agents | *What* agents can do |

```
┌─────────────────────────────────────────────────────────────┐
│         ORCHESTRATION LAYER (System → Agent)               │
│                                                             │
│  • Scheduler     - Time/idle-based invocation              │
│  • Event Router  - Event-driven agent dispatch             │
│  • Lifecycle     - Spawn/stop/manage agents                │
│                                                             │
│  The system PROACTIVELY invokes agents.                    │
└─────────────────────────────────────────────────────────────┘
                              │
                              │ Scheduled invocations
                              ▼
┌─────────────────────────────────────────────────────────────┐
│           COMMUNICATION LAYER (External → Agent)           │
│                                                             │
│  • CLI        - Terminal interface (built-in)              │
│  • HTTP       - Webhook/REST (built-in)                    │
│  • Discord    - Discord bot (plugin)                       │
│  • WhatsApp   - WhatsApp Business (plugin)                 │
│                                                             │
│  Users PROACTIVELY talk to agents.                         │
└─────────────────────────────────────────────────────────────┘
                              │
                              │ Messages flow through
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                      AGENT RUNTIME                           │
│                                                             │
│  Agents are PASSIVE - they receive from:                   │
│  - Orchestration layer (scheduled runs)                    │
│  - Communication layer (user messages)                     │
│                                                             │
│  Agents are ACTIVE when calling:                           │
│  - Capability layer (tools/MCPs/skills)                    │
│                                                             │
└─────────────────────────────────────────────────────────────┘
                              │
                              │ Agent invokes
                              ▼
┌─────────────────────────────────────────────────────────────┐
│            CAPABILITY LAYER (Agent → Service)              │
│                                                             │
│  Three-Tier Model:                                         │
│                                                             │
│  ┌─────────────────────────────────────────────────────┐   │
│  │ TIER 1: Tools (atomic, stateless)                   │   │
│  │ • web_search • calculator • apply_patch             │   │
│  │ • send_message • spawn_subagent • check_inbox       │   │
│  └─────────────────────────────────────────────────────┘   │
│  ┌─────────────────────────────────────────────────────┐   │
│  │ TIER 2: MCPs (bundled, stateful)                    │   │
│  │ • browser • database • email • memory-*             │   │
│  └─────────────────────────────────────────────────────┘   │
│  ┌─────────────────────────────────────────────────────┐   │
│  │ TIER 3: Skills (workflows)                          │   │
│  │ • coding_assistant • group_chat_manager             │   │
│  │ • broadcast_hub • research_pipeline                 │   │
│  └─────────────────────────────────────────────────────┘   │
│                                                             │
│  Agents PROACTIVELY invoke capabilities.                   │
└─────────────────────────────────────────────────────────────┘
```

**Key Insight:** Complex coordination patterns (group chat, broadcast) are **built as tools/skills** on top of simple primitives (`send_message`, `spawn_subagent`), not core features.

**Orthogonality Examples:**
- Scheduler invokes agent → agent uses `web_search` (Tool)
- Discord message invokes agent → agent uses `send_message` to another agent (Tool)
- Scheduled cron job → agent spawns subagent via `spawn_subagent` (Tool)
- User on CLI → agent calls `group_chat_manager` skill (built on messaging tools)

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

### 2.4 Session-Centric State with Overlays

- **Agents are stateless runtime instances**
- **Base sessions hold shared conversation context** (JSONL files)
- **Overlays hold context-specific state** (isolated or linked)
  - *Channel overlays* - Communication-specific (Discord guild, CLI terminal)
  - *Orchestration overlays* - Scheduled task context
- **Tools/MCPs are stateless/stateful independently**
- Sessions are portable, inspectable, long-lived

**Hybrid Session Model:**
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
└── Orchestration Overlays (Orchestration Layer)
    ├── check_email: Isolated email check context
    ├── daily_report: Inherited base + report state
    └── cleanup: Temporary cleanup session
```

Each overlay is independent - an agent can have both Discord and scheduled task contexts simultaneously.

## 3. System Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                   ORCHESTRATION LAYER                            │
│              (System-Proactive Agent Invocation)                 │
│                                                                  │
│  ┌─────────────┐  ┌─────────────┐  ┌──────────────────────┐    │
│  │  Scheduler  │  │   Event     │  │    Lifecycle         │    │
│  │             │  │   Router    │  │    Manager           │    │
│  │ • Interval  │  │             │  │                      │    │
│  │ • Idle      │  │ • File      │  │ • Spawn agents       │    │
│  │ • Cron      │  │ • Webhook   │  │ • Stop/Restart       │    │
│  │ • Once      │  │ • Internal  │  │ • Health checks      │    │
│  └──────┬──────┘  └──────┬──────┘  └──────────┬───────────┘    │
│         │                │                    │                │
│         └────────────────┴────────────────────┘                │
│                          │                                     │
│                    Invokes agent                               │
└──────────────────────────┼─────────────────────────────────────┘
                           │
                           ▼
┌─────────────────────────────────────────────────────────────────┐
│                    COMMUNICATION LAYER                           │
│              (User-Proactive Agent Invocation)                   │
│                                                                  │
│  ┌─────────────┐  ┌─────────────┐  ┌──────────────────────┐    │
│  │   Built-in  │  │   Registry  │  │       Custom         │    │
│  │             │  │   Channels  │  │      Channels        │    │
│  │ • CLI       │  │             │  │                      │    │
│  │ • HTTP      │  │ • Discord   │  │ • TUI (user-built)   │    │
│  │             │  │ • WhatsApp  │  │ • Game integration   │    │
│  │             │  │ • Telegram  │  │ • Web dashboard      │    │
│  │             │  │ • Slack     │  │ • IoT interface      │    │
│  └──────┬──────┘  └──────┬──────┘  └──────────┬───────────┘    │
│         │                │                    │                │
│         └────────────────┴────────────────────┘                │
│                          │                                     │
│                    Messages to agent                           │
└──────────────────────────┼─────────────────────────────────────┘
                           │
                           ▼
┌─────────────────────────────────────────────────────────────────┐
│                      AGENT RUNTIME                               │
│                                                                  │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │  Invocation Router                                          │   │
│  │  ├─ Route from Orchestration (scheduled runs)            │   │
│  │  ├─ Route from Communication (user messages)             │   │
│  │  ├─ Manage session overlays (orchestration vs comm)      │   │
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
                           │ Agent invokes
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
│  │ • send_msg  │  │ • memory-*  │  │ • group_chat_mgr     │    │
│  │ • spawn     │  │             │  │ • broadcast_hub      │    │
│  └─────────────┘  └─────────────┘  └──────────────────────┘    │
│                                                                  │
│  Sources: Built-in (T0) | Registry (T1/T2/T3)                   │
│  Used by: ALL channels (orthogonal)                             │
└─────────────────────────────────────────────────────────────────┘
```

## 4. Component Details

### 4.1 Orchestration Layer

The Orchestration Layer **proactively invokes agents** based on time, events, or system state.

#### 4.1.1 Scheduler

The Scheduler enables time-based and event-driven agent invocation with pluggable persistence backends.

**Trigger Types:**

| Trigger | Description | Use Case |
|---------|-------------|----------|
| `interval` | Every X minutes/seconds | Health checks, polling |
| `idle` | Every X minutes when idling | Cleanup, background sync |
| `cron` | Calendar-based (cron syntax) | Daily reports, weekly digests |
| `once` | One-shot at specific time | Reminders, delayed tasks |
| `event` | React to system events | File changes, webhooks |

**Scheduler Core Trait:**
```rust
pub trait Scheduler {
    async fn schedule(&self, task: ScheduledTask) -> Result<TaskId>;
    async fn cancel(&self, id: TaskId) -> Result<()>;
    async fn list(&self) -> Vec<ScheduledTask>;
    async fn history(&self, task_id: TaskId) -> Vec<ExecutionLog>;
}

pub struct ScheduledTask {
    pub id: TaskId,
    pub trigger: Trigger,
    pub action: Action,
    pub context: TaskContext,
    pub enabled: bool,
}

pub enum Trigger {
    Interval { duration: Duration },
    Idle { timeout: Duration },
    Cron { expression: String, timezone: String },
    Once { at: DateTime<Utc> },
}

pub enum Action {
    Tool { name: String, args: Value },
    Mcp { mcp: String, method: String, args: Value },
    Skill { name: String, input: Value },
}
```

**Pluggable Backends:**

| Backend | Use Case |
|---------|----------|
| `sqlite` (default) | Single-node, embedded |
| `postgres` | Multi-agent, distributed |

#### 4.1.2 Event Router

Routes internal events to agents: file system events, webhook deliveries, system notifications.

#### 4.1.3 Lifecycle Manager

Manages agent lifecycle: spawn, stop/restart, health checks, resource limits.

### 4.2 Unified Tool Model (Core Primitive)

All operations—whether calling a web search, sending a message to another agent, or spawning a subagent—are **tools**. Tools natively support **sync** and **async** invocation modes.

```rust
/// All tools implement this trait
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    
    /// Synchronous: Block until result
    async fn call(&self, args: Value, ctx: ToolContext) -> Result<Value>;
    
    /// Asynchronous: Return receipt immediately
    async fn call_async(&self, args: Value, ctx: ToolContext) -> Result<Receipt>;
}

/// Receipt for async operations
pub struct Receipt {
    pub id: String,
    pub tool: String,
    pub status: AsyncStatus,
}

pub enum AsyncStatus {
    Pending,
    Running,
    Completed { result: Value },
    Failed { error: String },
    Timeout,
}

/// Check status or get result from receipt
pub trait ReceiptOps {
    async fn status(&self) -> AsyncStatus;
    async fn wait(&self, timeout: Duration) -> Result<Value>;
    async fn result(&self) -> Option<Value>;
}
```

**Async Result Flow:**

```
Agent calls tool.call_async()
         │
         ▼
    Returns Receipt immediately
         │
         ▼
    Agent continues execution
         │
         ▼
    Tool runs in background
         │
         ▼
    On completion → Result goes to INBOX
         │
         ▼
    Agent checks inbox: receipt.wait() or inbox.poll()
```

**Inbox for Async Results:**

```rust
pub struct AgentInbox {
    /// Completed async operations waiting to be processed
    queue: VecDeque<InboxItem>,
}

pub struct InboxItem {
    pub receipt_id: String,
    pub tool: String,
    pub result: Value,
    pub timestamp: DateTime,
}

pub trait Inbox {
    /// Poll for completed items (non-blocking)
    async fn poll(&mut self) -> Vec<InboxItem>;
    
    /// Wait for next item (blocking)
    async fn next(&mut self) -> InboxItem;
    
    /// Check specific receipt
    async fn check(&self, receipt_id: &str) -> AsyncStatus;
}
```

### 4.3 Communication Layer (Channels)

**Channel Trait:**
```rust
#[async_trait]
pub trait Channel: Send + Sync {
    fn id(&self) -> &str;
    async fn recv(&mut self) -> Result<Option<Message>>;
    async fn send(&mut self, response: Response) -> Result<()>;
    async fn stream(&mut self, events: EventStream) -> Result<()>;
}
```

**Channel Types:**

| Type | Examples | Source |
|------|----------|--------|
| Built-in | CLI, HTTP | Core |
| Registry | Discord, WhatsApp | Pekohub |
| Custom | TUI, Game mods | User-built |

### 4.4 Memory Architecture

Pekobot separates **immediate context** (built-in) from **long-term memory** (pluggable MCP):

**1st Order Memory (Context):**
- Session JSONL files
- Immediate conversation history
- Automatic LLM context injection

**2nd Order Memory (Long-term):**
- `memory-markdown` - MD files + SQLite vectors
- `memory-postgres` - PostgreSQL + pgvector
- `memory-chroma` - ChromaDB
- `memory-pinecone` - Pinecone
- `memory-files` - Simple files
- `memory-none` - Disabled

### 4.5 Capability Tiers

**Tier 1: Tools (atomic, stateless)**
- Single function, no persistence
- Examples: `web_search`, `calculator`, `send_message`, `spawn_subagent`

**Tier 2: MCPs (bundled, stateful)**
- Multiple related functions, maintains state
- Examples: `browser`, `database`, `email`, `memory-*`

**Tier 3: Skills (workflows)**
- Multi-step processes using tools and MCPs
- Examples: `coding_assistant`, `group_chat_manager`, `broadcast_hub`

## 5. Security Model

| Aspect | Core Implementation |
|--------|---------------------|
| Sandboxing | ❌ None |
| Permission system | ❌ None |
| Audit logging | ✅ Complete JSONL transcripts |
| Session isolation | ✅ Configurable per overlay |
| Checksum verification | ✅ If provided by registry |

## 6. Configuration Examples

### 6.1 Minimal Agent

```toml
name = "minimal"

[provider]
provider_type = "kimi"

[capabilities]
tools = ["process", "filesystem"]

[[channels]]
id = "cli"
type = "builtin"
```

### 6.2 Agent with Messaging

```toml
name = "coordinator"

[capabilities]
# Basic tools + messaging primitives
tools = ["web_search", "send_message", "spawn_subagent", "check_inbox"]
mcp = ["memory-markdown"]

[[channels]]
id = "cli"
type = "builtin"
```

### 6.3 Async Operations

```rust
// Spawn subagent asynchronously
let receipt = tools.spawn_subagent.call_async({
    "task": "Research async Rust patterns"
}, ctx).await?;

// Continue working...

// Check status later
match receipt.status().await? {
    AsyncStatus::Completed { result } => println!("Done: {}", result),
    AsyncStatus::Pending => println!("Still running..."),
    _ => {}
}

// Or block and wait
let result = receipt.wait(Duration::from_secs(60)).await?;
```

### 6.4 Complex Coordination as External Tool

```toml
# Group chat manager is a skill, not core
[capabilities]
tools = ["send_message", "check_inbox"]
skills = ["group_chat_manager"]

# The skill uses basic messaging tools internally
```

## 7. Anti-Goals

- **Sandboxing**: Use OS-level isolation if needed
- **Enterprise RBAC**: Organization-specific
- **Content moderation**: User's responsibility
- **Complex coordination in core**: Group chat, broadcast are tools/skills
- **Synchronous broadcast**: Async only (avoids context explosion)

## 8. Related Concepts

| Concept | Analogy | Pekobot |
|---------|---------|---------|
| Unix shell | Command execution | Core runtime |
| apt/npm | Package manager | Extension registry |
| Docker | Isolation | External |
| IRC bouncer | Multi-client | Multi-channel agent |
| Email | Async messaging | Agent inbox |

## 9. Future Directions

- Pekohub production with reputation system
- Tool/MCP migration from core to registry
- Memory MCP ecosystem (markdown, postgres, chroma, pinecone)
- Advanced scheduler backends (Kubernetes, cloud)

---

*Status: Simplified architecture - Unified Tool Model*
*Last updated: 2026-03-09*
