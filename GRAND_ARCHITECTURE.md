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

### 2.4 System-Managed Async Model

The **system** manages sync/async execution, not the tools themselves. Tools are simple synchronous functions.

```
┌─────────────────────────────────────────────────────────────┐
│              SYSTEM-MANAGED ASYNC MODEL                      │
├─────────────────────────────────────────────────────────────┤
│                                                              │
│  TOOL: Simple synchronous function                          │
│  pub async fn call(&self, args: Value) -> Result<Value>      │
│                                                              │
│  SYSTEM manages execution mode:                             │
│  • sync  → Block until result                               │
│  • async → Return receipt, run in background                │
│                                                              │
│  ASYNC FLOW (managed by Execution Engine):                  │
│  1. Agent requests async tool call                          │
│  2. System spawns task, returns Receipt                     │
│  3. Tool runs synchronously in background                   │
│  4. Result stored, agent notified via MCP (optional)        │
│                                                              │
└─────────────────────────────────────────────────────────────┘
```

**Benefits:**
- Tools are simple - no sync/async complexity
- System handles all concurrency
- Consistent behavior across all tools
- Async inbox can be provided by MCP (not core)

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

The Agent Runtime manages agent state, session context, and tool execution coordination.

**Components:**
- **Invocation Router** - Route messages from Orchestration vs Communication
- **AgentManager** - Pool, registry, lifecycle
- **Individual Agent** - Identity, Session, Provider
- **Async Task Manager** - System-managed background task tracking

**Note:** Agent inbox/queue for async results is **not built into core** - agents needing inbox functionality should use an MCP (e.g., `inbox-sqlite`, `inbox-redis`). Simple async is handled by the Execution Engine.

### 4.4 Execution Engine

The **AgenticLoopV4** handles tool invocation. Tools are simple synchronous functions; the system manages sync/async execution.

```rust
pub struct AgenticLoopV4 {
    /// Execute tool (system manages sync vs async)
    async fn execute(
        &self, 
        tool: &str, 
        args: Value,
        mode: ExecutionMode,
    ) -> Result<ExecutionResult>;
}

pub enum ExecutionMode {
    /// Block until result
    Sync { timeout: Duration },
    /// Return immediately with handle
    Async,
}

pub enum ExecutionResult {
    /// Sync result
    Value(Value),
    /// Async handle
    Handle(TaskHandle),
}

/// Handle for async operation (system-managed)
pub struct TaskHandle {
    pub id: String,
    pub status: TaskStatus,
}

pub enum TaskStatus {
    Pending,
    Running,
    Completed { result: Value },
    Failed { error: String },
    Timeout,
}

impl TaskHandle {
    /// Check current status
    pub async fn status(&self) -> TaskStatus;
    
    /// Block and wait for result
    pub async fn wait(&self, timeout: Duration) -> Result<Value>;
}
```

**Tool Trait (Simple):**
```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn schema(&self) -> ToolSchema;
    
    /// Execute tool (always synchronous from tool's perspective)
    async fn call(&self, args: Value) -> Result<Value>;
}
```

**Key Point:** Tools don't know about sync/async. They just execute and return. The system manages:
- Spawning async tasks
- Tracking handles
- Returning results
- Timeouts and cancellation

### 4.5 Capability Layer

#### 4.5.1 Tools (Atomic, Stateless)

**Core Built-in Tools:**

| Tool | Category | Purpose |
|------|----------|---------|
| `read` | Filesystem | Read file contents from workspace |
| `write` | Filesystem | Write/create files in workspace |
| `edit` | Filesystem | Apply search/replace edits to files |
| `exec` | Process | Execute shell commands with optional sandboxing |
| `process` | Process | Manage background processes (list, kill, send signals) |
| `apply_patch` | Code | Apply unified diff patches to codebase |
| `agents_list` | Agent | List available agents in the system |
| `agent_send` | Agent | Send message to another agent (1-to-1 communication) |
| `agent_spawn` | Agent | Spawn sub-session for parallel/multitasking workflows |
| `subagents` | Agent | Manage and interact with running subagents |
| `sessions_list` | Session | List active and historical sessions |
| `sessions_history` | Session | Retrieve conversation history for a session |
| `session_status` | Session | Check current session state and metadata |
| `cron` | Scheduling | Schedule recurring tasks and delayed one-time jobs |

**Design Rationale:** Only fundamental primitives are built-in. Everything else is provided via MCPs or skills. Complex tools like `web_search`, `browser`, `memory_*`, and platform-specific actions are externalized to MCPs.

**Not Built-in (MCP/Skill candidates):**
| Tool Type | Moved to | Reason |
|-----------|----------|--------|
| Web tools (`web_search`, `web_fetch`) | **MCP: `web`** | Requires external API keys, rate limits |
| Browser | **MCP: `browser`** | Heavy dependency, optional use |
| Media (`image`, `tts`, `canvas`) | **MCP: `media`** | Optional capabilities, heavy deps |
| Memory | **MCP: `memory-*`** | Pluggable memory backends |
| Platform actions | **Channel plugins** | Platform-specific, not core |
| Gateway/Nodes | **MCP: `infrastructure`** | External service integration, distributed execution |

**Tool Trait (Simple):**
```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn schema(&self) -> ToolSchema;
    
    /// Execute tool - the system manages sync/async, not the tool
    async fn call(&self, args: Value) -> Result<Value>;
}
```

**Example: agent_send Tool:**
```rust
pub struct AgentSendTool;

impl Tool for AgentSendTool {
    fn name(&self) -> &str { "agent_send" }
    
    async fn call(&self, args: Value) -> Result<Value> {
        let target = args["target"].as_str().unwrap();
        let message = args["message"].as_str().unwrap();
        
        send_message(target, message).await?;
        Ok(json!({ "status": "sent" }))
    }
}
```

**Example: agent_spawn Tool:**
```rust
pub struct AgentSpawnTool;

impl Tool for AgentSpawnTool {
    fn name(&self) -> &str { "agent_spawn" }
    
    async fn call(&self, args: Value) -> Result<Value> {
        let task = args["task"].as_str().unwrap();
        
        let result = spawn_task(task).await?;
        Ok(json!({ "result": result }))
    }
}
```

**Note:** Tools are simple synchronous functions. The system (Execution Engine) manages whether to run them synchronously (blocking) or asynchronously (returning a handle). Tools don't need to implement separate sync/async paths.

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

### 4.6 Channel & Session Routing

**Core Principles:**
1. **Session per peer** - Each unique peer gets their own session
2. **Peer = user or agent** - Both treated the same for session purposes
3. **Default user** - If no username specified, use "default"
4. **Reply to source channel** - Agent responds on the channel where message was received
5. **Cross-channel same session** - Same peer on different channels = same session context

**Session Key Format:**
```
{agent_id}:{peer_type}:{peer_id}:{session_id}

Examples:
- main:user:default:session_abc123     (default user)
- main:user:alice:session_def456       (user "alice")
- main:agent:researcher:session_ghi789 (agent "researcher")
```

**Routing Logic:**
```
┌─────────────────────────────────────────────────────────────────┐
│                     MESSAGE ROUTING                              │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  INCOMING:                                                       │
│  Channel receives message ──► Identify peer                      │
│                               ├── User: extract username         │
│                               ├── Agent: agent ID from envelope  │
│                               └── None: "default"                │
│                                                                  │
│  ├───► Get or create session for (agent, peer)                   │
│  │                                                                │
│  └───► Invoke agent with (session, message, source_channel)      │
│                                                                  │
│  OUTGOING:                                                       │
│  Agent generates response ──► Route to source_channel            │
│                                                                  │
│  Special cases:                                                  │
│  - Agent-to-agent via tool: return via tool result               │
│  - Explicit channel override: use specified channel              │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

**Cross-Channel Same Session:**
```
User "alice"
     │
     ├──► CLI ──► Session A ──► Agent ──► Reply CLI
     │
     ├──► Discord ──► Session A ──► Agent ──► Reply Discord
     │
     └──► WhatsApp ──► Session A ──► Agent ──► Reply WhatsApp

Same session context across all channels!
Agent remembers context from previous channel.
```

**Implementation:**
```rust
pub struct SessionManager {
    /// Active sessions: (agent_id, peer) -> session
    sessions: HashMap<(String, Peer), Session>,
}

pub enum Peer {
    User(String),   // username
    Agent(String),  // agent_id
}

pub struct IncomingMessage {
    pub content: String,
    pub source: Source,
    pub peer: Peer,
}

pub struct Source {
    pub channel: String,      // "cli", "discord", "whatsapp"
    pub channel_specific_id: Option<String>,
}

impl SessionManager {
    /// Get or create session for peer
    pub fn get_session(&mut self, agent: &str, peer: &Peer) -> &mut Session {
        let key = (agent.to_string(), peer.clone());
        self.sessions.entry(key).or_insert_with(|| {
            Session::new(agent, peer)
        })
    }
}
```

**Channel Override:**
Agents can explicitly send to a different channel:
```rust
// Default: reply to source channel
agent.reply(response).await?;

// Override: send to specific channel
agent.send_to("discord:general", response).await?;
```

**Example Scenarios:**

| # | Scenario | Session | Reply To |
|---|----------|---------|----------|
| 1 | User (default) on CLI | Session A (user:default) | CLI |
| 2 | User (default) on Discord | Session A (same user) | Discord |
| 3 | User (default) on CLI with `/new` | Session B (new session, same user) | CLI |
| 4 | Agent B via tool call | Session C (peer=agent:researcher) | Tool result |
| 5 | User (default) on WhatsApp | Session A (same user) | WhatsApp |
| 6 | User "X" on any channel | Session D (user:X) | Same channel |

## 6. Memory Architecture

### 6.1 1st Order Memory (Context)

**Built-in, always present:**
- Session JSONL files
- Immediate conversation history
- Automatic LLM context injection
- Stored in: `~/.pekobot/agents/{agent}/sessions/`

### 6.2 2nd Order Memory (Long-term)

**Pluggable MCP, optional:**
- `memory-markdown` - MD files + SQLite vectors
- `memory-postgres` - PostgreSQL + pgvector
- `memory-chroma` - ChromaDB
- `memory-pinecone` - Pinecone
- `memory-files` - Simple files
- `memory-none` - Disabled

## 7. Async Flow Examples

### 8.1 Async Tool Call

```rust
// System executes tool asynchronously, returns handle immediately
let handle = execution_engine.execute(
    "web_search",
    json!({ "query": "Rust async programming" }),
    ExecutionMode::Async
).await?;

// Agent continues conversation while tool runs
// ...

// Later, check result
if let TaskStatus::Completed { result } = handle.status().await {
    // Process result
}
// Or block and wait
let result = handle.wait(Duration::from_secs(30)).await?;
```

### 8.2 Async Agent Messaging

```rust
// System sends message to another agent asynchronously
let handle = execution_engine.execute(
    "agent_send",
    json!({ "target": "researcher", "message": "Research this topic" }),
    ExecutionMode::Async
).await?;

// Continue working...
// Result (if any) can be retrieved later via handle
```

### 8.3 Async Spawn (Multitasking)

```rust
// System spawns 3 tasks in parallel
let h1 = execution_engine.execute(
    "agent_spawn",
    json!({"task": "Research asyncio"}),
    ExecutionMode::Async
).await?;
let h2 = execution_engine.execute(
    "agent_spawn",
    json!({"task": "Research trio"}),
    ExecutionMode::Async
).await?;
let h3 = execution_engine.execute(
    "agent_spawn",
    json!({"task": "Research curio"}),
    ExecutionMode::Async
).await?;

// Continue talking to user...

// Collect results later
let results = vec![h1.wait().await?, h2.wait().await?, h3.wait().await?];
```

### 8.4 Complex Coordination (External Skill)

```rust
// Group chat manager skill (built on agent_send)
group_chat_tool.call(json!({
    "room": "engineering",
    "action": "broadcast",
    "message": "Deploying to production"
}))?;

// Internally uses agent_send to each participant
```

## 8. Configuration Examples

### 8.1 Minimal Agent

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

### 8.2 Agent with External Coordination

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

### 8.3 Multi-Tasking Agent

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

### 8.4 Scheduled Async Tasks

```toml
[scheduler.tasks.poll_inbox]
trigger = { type = "interval", minutes = 5 }
action = { type = "tool", name = "inbox_poll", args = { max_items = 5 } }

[scheduler.tasks.cleanup_spawns]
trigger = { type = "idle", minutes = 30 }
action = { type = "tool", name = "spawn_cleanup" }
```

## 9. Anti-Goals

What Pekobot explicitly avoids:

- **Sandboxing**: Use OS-level isolation (containers, VMs) if needed
- **Enterprise RBAC**: Role-based access is organization-specific
- **Content moderation**: Speech is the user's responsibility
- **Vendor lock-in**: Open protocols, portable sessions
- **Cloud dependency**: Self-hosted by design, cloud optional
- **Complex coordination in core**: Group chat, broadcast are skills, not core
- **Agent inbox in core**: Use inbox MCP if needed

## 10. Related Concepts

| Concept | Analogy | Pekobot Equivalent |
|---------|---------|-------------------|
| Unix shell | Command execution | Core runtime |
| apt/npm | Package manager | Extension registry |
| Docker | Isolation | Not provided - use external |
| IRC bouncer | Multi-client presence | Multi-channel agent |
| X11/Wayland | Display server | Channel layer |
| Shell script | Automation | Skills |
| Thread pool | Parallel execution | Async spawn tool |
| Message queue | Async communication | Inbox MCP (optional) |

## 11. Future Directions

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
*Last updated: 2026-03-10*
