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
│  │ • agent_send • agent_spawn                          │   │
│  └─────────────────────────────────────────────────────┘   │
│  ┌─────────────────────────────────────────────────────┐   │
│  │ TIER 2: MCPs (bundled, stateful)                    │   │
│  │ • browser • database • email • memory-*             │   │
│  └─────────────────────────────────────────────────────┘   │
│  ┌─────────────────────────────────────────────────────┐   │
│  │ TIER 3: Skills (workflows)                          │   │
│  │ • coding_assistant • research_pipeline              │   │
│  │ • group_chat_manager • broadcast_hub                │   │
│  └─────────────────────────────────────────────────────┘   │
│                                                             │
│  Agents PROACTIVELY invoke capabilities.                   │
└─────────────────────────────────────────────────────────────┘
```

**Key Insight:** Complex coordination patterns (group chat, broadcast, workflows) are **built as tools/skills**, not core features. Core only provides basic 1-to-1 messaging primitives.

**Orthogonality Examples:**
- Scheduler invokes agent → agent uses `web_search` (Tool)
- Discord message invokes agent → agent uses `browser` (MCP)
- Agent uses `agent_send` tool to message another agent (1-to-1)
- Agent uses `agent_spawn` tool to multitask (sync/async)
- Complex group chat? Use `group_chat_manager` skill built on `agent_send`

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

### 2.4 Unified Async Model

All tool invocations (including messaging and spawn) support **sync** and **async** modes:

```
┌─────────────────────────────────────────────────────────────┐
│              UNIFIED TOOL INVOCATION MODEL                   │
├─────────────────────────────────────────────────────────────┤
│                                                              │
│  ANY TOOL can be invoked:                                   │
│  • tool.call_sync()    → Block until result                 │
│  • tool.call_async()   → Return receipt immediately         │
│                                                              │
│  Includes:                                                   │
│  • Regular tools (web_search, filesystem)                   │
│  • Messaging tools (agent_send)                             │
│  • Spawn tools (agent_spawn)                                │
│                                                              │
│  ASYNC FLOW:                                                │
│  1. Agent calls tool.async() → Gets Receipt                 │
│  2. Operation runs in background                            │
│  3. Result lands in Agent Inbox when complete               │
│  4. Agent polls inbox or gets notified                      │
│                                                              │
└─────────────────────────────────────────────────────────────┘
```

**Benefits:**
- Single mental model for all async operations
- No special cases for messaging vs tools
- Agent can multitask naturally

### 2.5 Session-Centric State with Overlays

- **Agents are stateless runtime instances**
- **Base sessions hold shared conversation context** (JSONL files)
- **Overlays hold context-specific state** (isolated or linked)
  - *Channel overlays* - Communication-specific (Discord guild, CLI terminal)
  - *Orchestration overlays* - Scheduled task context
  - *Spawn overlays* - Sub-session isolation
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
└── Spawn Overlays (from agent_spawn)
    ├── spawn_abc123: Isolated research task
    └── spawn_def456: Isolated writing task
```

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
│  │  └─ Manage session overlays                              │   │
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
│                              │                                    │
│  ┌───────────────────────────▼──────────────────────────┐       │
│  │              Agent Inbox                              │       │
│  │  ├─ Async results queue (tools, spawn, messages)     │       │
│  │  ├─ Poll for completed items                         │       │
│  │  └─ Notification on completion                       │       │
│  └──────────────────────────────────────────────────────┘       │
└─────────────────────────────────────────────────────────────────┘
                           │
                           │ Agent invokes tools
                           ▼
┌─────────────────────────────────────────────────────────────────┐
│                     EXECUTION ENGINE                             │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │  AgenticLoopV4 (native tool calling)                     │   │
│  │  ├─ Tool/MCP dispatch                                    │   │
│  │  ├─ Sync/Async handling                                  │   │
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
│  │• web_search│  │• browser    │  │• coding_assistant    │    │
│  │• agent_send│  │• database   │  │• group_chat_manager   │    │
│  │• agent_spawn│ │• email      │  │• broadcast_hub       │    │
│  └─────────────┘  └─────────────┘  └──────────────────────┘    │
│                                                                  │
│  Sources: Built-in | Registry | User-created                    │
└─────────────────────────────────────────────────────────────────┘
```

## 4. Component Details

### 4.1 Orchestration Layer

The Orchestration Layer **proactively invokes agents** based on time, events, or system state.

#### 4.1.1 Scheduler

Time-based and event-driven agent invocation.

**Trigger Types:**

| Trigger | Description | Use Case |
|---------|-------------|----------|
| `interval` | Every X minutes/seconds | Health checks, polling |
| `idle` | Every X minutes when idling | Cleanup, background sync |
| `cron` | Calendar-based (cron syntax) | Daily reports, weekly digests |
| `once` | One-shot at specific time | Reminders, delayed tasks |
| `event` | React to system events | File changes, webhooks |

**Scheduler Core:**
```rust
pub trait Scheduler {
    async fn schedule(&self, task: ScheduledTask) -> Result<TaskId>;
    async fn cancel(&self, id: TaskId) -> Result<()>;
    async fn list(&self) -> Vec<ScheduledTask>;
}

pub struct ScheduledTask {
    pub id: TaskId,
    pub trigger: Trigger,
    pub action: Action,  // Invoke agent with context
    pub enabled: bool,
}

pub enum Action {
    Tool { name: String, args: Value },
    Mcp { mcp: String, method: String, args: Value },
    Skill { name: String, input: Value },
    Message { channel: String, content: String },
}
```

**Pluggable Backends:**
- `sqlite` (default) - Single-node, embedded
- `postgres` - Multi-agent, distributed

#### 4.1.2 Event Router

Routes external events to agents:
- File system events
- Webhook deliveries
- Internal system events

#### 4.1.3 Lifecycle Manager

Manages agent lifecycle:
- Spawn new agents
- Stop/restart agents
- Health checks
- Resource limits

### 4.2 Communication Layer

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

### 4.3 Agent Runtime

#### 4.3.1 Agent Inbox

Queue for async operation results:

```rust
pub struct AgentInbox {
    /// Completed async operations waiting to be processed
    queue: VecDeque<InboxItem>,
    
    /// In-progress operations tracked by receipt
    pending: HashMap<String, AsyncStatus>,
}

pub struct InboxItem {
    pub receipt_id: String,
    pub source: Source,  // Tool, Spawn, Message
    pub result: String,
    pub timestamp: DateTime,
}

pub trait Inbox {
    /// Poll for completed items (non-blocking)
    async fn poll(&mut self) -> Vec<InboxItem>;
    
    /// Wait for next item (blocking)
    async fn next(&mut self) -> InboxItem;
    
    /// Get status of pending operation
    fn status(&self, receipt_id: &str) -> Option<AsyncStatus>;
}
```

### 4.4 Execution Engine

The **AgenticLoopV4** handles tool invocation with native sync/async support:

```rust
pub struct AgenticLoopV4 {
    /// Execute tool call
    async fn execute_tool(&self, call: ToolCall) -> Result<ToolResult>;
    
    /// Handle sync invocation - block until result
    async fn invoke_sync(&self, tool: &str, args: Value) -> Result<Value>;
    
    /// Handle async invocation - return receipt
    async fn invoke_async(&self, tool: &str, args: Value) -> Result<Receipt>;
}

/// Unified receipt for any async operation
pub struct Receipt {
    pub id: String,
    pub operation_type: OperationType,  // Tool, Spawn, Message
    pub status: AsyncStatus,
}

pub enum AsyncStatus {
    Pending,
    Running,
    Completed { result: String },
    Failed { error: String },
    Timeout,
}

impl Receipt {
    /// Check current status
    pub async fn status(&self) -> AsyncStatus;
    
    /// Block and wait for result
    pub async fn wait(&self, timeout: Duration) -> Result<String>;
    
    /// Get result if completed
    pub fn result(&self) -> Option<String>;
}
```

### 4.5 Capability Layer

#### 4.5.1 Tools (Atomic, Stateless)

**Core Built-in Tools:**

| Tool | Purpose | Modes |
|------|---------|-------|
| `web_search` | Search the web | sync/async |
| `filesystem` | File operations | sync/async |
| `process` | Shell execution | sync/async |
| `agent_send` | Send message to another agent | sync/async |
| `agent_spawn` | Spawn sub-session for multitasking | sync/async |

**Tool Trait:**
```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn schema(&self) -> ToolSchema;
    
    /// Execute synchronously
    async fn call(&self, args: Value) -> Result<Value>;
    
    /// Execute asynchronously - return receipt
    async fn call_async(&self, args: Value) -> Result<Receipt>;
}
```

**Example: agent_send Tool:**
```rust
pub struct AgentSendTool;

impl Tool for AgentSendTool {
    fn name(&self) -> &str { "agent_send" }
    
    async fn call(&self, args: Value) -> Result<Value> {
        // Synchronous: block until reply
        let target = args["target"].as_str().unwrap();
        let message = args["message"].as_str().unwrap();
        let timeout = args["timeout"].as_u64().unwrap_or(60);
        
        let reply = send_and_wait(target, message, Duration::from_secs(timeout)).await?;
        Ok(json!({ "reply": reply }))
    }
    
    async fn call_async(&self, args: Value) -> Result<Receipt> {
        // Asynchronous: return receipt immediately
        let target = args["target"].as_str().unwrap();
        let message = args["message"].as_str().unwrap();
        
        let receipt_id = queue_message(target, message).await?;
        Ok(Receipt::new(receipt_id, OperationType::Message))
    }
}
```

**Example: agent_spawn Tool:**
```rust
pub struct AgentSpawnTool;

impl Tool for AgentSpawnTool {
    fn name(&self) -> &str { "agent_spawn" }
    
    async fn call(&self, args: Value) -> Result<Value> {
        // Synchronous: block until spawn completes
        let task = args["task"].as_str().unwrap();
        let timeout = args["timeout"].as_u64().unwrap_or(300);
        
        let result = spawn_and_wait(task, Duration::from_secs(timeout)).await?;
        Ok(json!({ "result": result }))
    }
    
    async fn call_async(&self, args: Value) -> Result<Receipt> {
        // Asynchronous: return receipt, run in background
        let task = args["task"].as_str().unwrap();
        
        let receipt_id = spawn_background(task).await?;
        Ok(Receipt::new(receipt_id, OperationType::Spawn))
    }
}
```

#### 4.5.2 MCPs (Bundled, Stateful)

Stateful service connections:
- `browser` - Browser automation
- `database` - Database connections
- `email` - Email IMAP/SMTP
- `memory-*` - Pluggable memory backends

#### 4.5.3 Skills (Workflows)

Multi-step workflows. **Complex coordination patterns are skills:**

| Skill | Built On | Purpose |
|-------|----------|---------|
| `coding_assistant` | Tools + MCPs | Code generation workflow |
| `group_chat_manager` | `agent_send` tool | Multi-agent conversations |
| `broadcast_hub` | `agent_send` tool | Pub-sub messaging |
| `workflow_engine` | `agent_spawn` tool | Sequential/parallel chains |

**Why externalize?**
- Core stays minimal
- Patterns can evolve independently
- Users can customize
- No core bloat

## 5. Memory Architecture

### 5.1 1st Order Memory (Context)

**Built-in, always present:**
- Session JSONL files
- Immediate conversation history
- Automatic LLM context injection
- Stored in: `~/.pekobot/agents/{agent}/sessions/`

### 5.2 2nd Order Memory (Long-term)

**Pluggable MCP, optional:**
- `memory-markdown` - MD files + SQLite vectors
- `memory-postgres` - PostgreSQL + pgvector
- `memory-chroma` - ChromaDB
- `memory-pinecone` - Pinecone
- `memory-files` - Simple files
- `memory-none` - Disabled

## 6. Async Flow Examples

### 6.1 Async Tool Call

```rust
// Agent calls tool asynchronously
let receipt = tool.call_async(json!({
    "query": "Rust async programming"
})).await?;

// Agent continues conversation while tool runs
// ...

// Later, check result
if let Some(result) = receipt.result() {
    // Process result
}
// Or block and wait
let result = receipt.wait(Duration::from_secs(30)).await?;
```

### 6.2 Async Agent Messaging

```rust
// Send message to another agent asynchronously
let receipt = agent_send_tool.call_async(json!({
    "target": "researcher",
    "message": "Research this topic"
})).await?;

// Continue working...
// Researcher will reply to inbox when done
```

### 6.3 Async Spawn (Multitasking)

```rust
// Spawn 3 research tasks in parallel
let r1 = spawn_tool.call_async(json!({"task": "Research asyncio"})).await?;
let r2 = spawn_tool.call_async(json!({"task": "Research trio"})).await?;
let r3 = spawn_tool.call_async(json!({"task": "Research curio"})).await?;

// Continue talking to user...

// Collect results later
let results = vec![r1.wait().await?, r2.wait().await?, r3.wait().await?];
```

### 6.4 Complex Coordination (External Skill)

```rust
// Group chat manager skill (built on agent_send)
group_chat_tool.call(json!({
    "room": "engineering",
    "action": "broadcast",
    "message": "Deploying to production"
}))?;

// Internally uses agent_send to each participant
```

## 7. Configuration Examples

### 7.1 Minimal Agent

```toml
name = "minimal"

[provider]
provider_type = "kimi"

[capabilities]
tools = ["filesystem", "process"]
builtin = ["agent_send", "agent_spawn"]

[[channels]]
id = "cli"
type = "builtin"
```

### 7.2 Agent with External Coordination

```toml
name = "coordinator"

[provider]
provider_type = "anthropic"

[capabilities]
tools = ["web_search", "filesystem"]
builtin = ["agent_send", "agent_spawn"]
skills = ["group_chat_manager", "broadcast_hub"]

[[channels]]
id = "cli"
type = "builtin"

[[channels]]
id = "discord"
type = "registry"
plugin = "discord"
```

### 7.3 Multi-Tasking Agent

```toml
name = "research_assistant"

[provider]
provider_type = "anthropic"

[capabilities]
tools = ["web_search", "write", "read"]
builtin = ["agent_spawn"]

# Spawn configuration
[spawn]
max_concurrent = 3
```

### 7.4 Scheduled Async Tasks

```toml
[scheduler.tasks.poll_inbox]
trigger = { type = "interval", minutes = 5 }
action = { type = "tool", name = "inbox_poll", args = { max_items = 5 } }

[scheduler.tasks.cleanup_spawns]
trigger = { type = "idle", minutes = 30 }
action = { type = "tool", name = "spawn_cleanup" }
```

## 8. Anti-Goals

What Pekobot explicitly avoids:

- **Sandboxing**: Use OS-level isolation (containers, VMs) if needed
- **Enterprise RBAC**: Role-based access is organization-specific
- **Content moderation**: Speech is the user's responsibility
- **Vendor lock-in**: Open protocols, portable sessions
- **Cloud dependency**: Self-hosted by design, cloud optional
- **Complex coordination in core**: Group chat, broadcast are skills, not core
- **Special async primitives**: All tools support sync/async uniformly

## 9. Related Concepts

| Concept | Analogy | Pekobot Equivalent |
|---------|---------|-------------------|
| Unix shell | Command execution | Core runtime |
| apt/npm | Package manager | Extension registry |
| Docker | Isolation | Not provided - use external |
| IRC bouncer | Multi-client presence | Multi-channel agent |
| X11/Wayland | Display server | Channel layer |
| Shell script | Automation | Skills |
| Thread pool | Parallel execution | Async spawn tool |
| Message queue | Async communication | Agent inbox |

## 10. Future Directions

### Near-term (3-6 months)
- Pekohub production with reputation system
- Channel plugin architecture stabilization
- Tool/MCP migration from core to registry
- Multi-channel session management
- Scheduler with pluggable backends

### Medium-term (6-12 months)
- External trust layer (signed extensions, audits)
- Memory MCP ecosystem (markdown, postgres, chroma, pinecone)
- Web dashboard channel
- Advanced scheduler (Kubernetes CronJob, AWS EventBridge backends)

### Long-term (12+ months)
- Distributed agent clusters
- WASM-based extensions
- Cross-runtime session portability

---

*Status: Simplified Architecture - Core provides primitives, complex coordination externalized*
*Last updated: 2026-03-09*
