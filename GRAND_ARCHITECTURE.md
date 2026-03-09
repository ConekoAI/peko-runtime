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
│  └─────────────────────────────────────────────────────┘   │
│  ┌─────────────────────────────────────────────────────┐   │
│  │ TIER 2: MCPs (bundled, stateful)                    │   │
│  │ • browser • database • email • memory-*             │   │
│  └─────────────────────────────────────────────────────┘   │
│  ┌─────────────────────────────────────────────────────┐   │
│  │ TIER 3: Skills (workflows)                          │   │
│  │ • coding_assistant • research_pipeline              │   │
│  └─────────────────────────────────────────────────────┘   │
│                                                             │
│  Agents PROACTIVELY invoke capabilities.                   │
└─────────────────────────────────────────────────────────────┘
```

**Orthogonality Examples:**
- Scheduler invokes agent → agent uses `web_search` (Tool)
- Discord message invokes agent → agent uses `browser` (MCP)
- Scheduled cron job → agent sends message via Discord (Orchestration + Communication)
- User on CLI → agent schedules follow-up (Communication + Orchestration)

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
│  └─────────────┘  └─────────────┘  └──────────────────────┘    │
│                                                                  │
│  Sources: Built-in (T0) | Registry (T1/T2/T3)                   │
│  Used by: ALL channels (orthogonal)                             │
└─────────────────────────────────────────────────────────────────┘
```

## 4. Component Details

### 4.1 Orchestration Layer

The Orchestration Layer **proactively invokes agents** based on time, events, or system state. Unlike Capabilities (which agents call) or Communication (where users initiate), Orchestration is the system calling agents.

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
    pub context: TaskContext,  // Custom session/overlay
    pub enabled: bool,
}

pub enum Trigger {
    Interval { duration: Duration },
    Idle { timeout: Duration },  // Resets on activity
    Cron { expression: String, timezone: String },
    Once { at: DateTime<Utc> },
}

pub enum Action {
    Tool { name: String, args: Value },
    Mcp { mcp: String, method: String, args: Value },
    Skill { name: String, input: Value },
    Message { channel: String, content: String },
}
```

**Idle Detection:**
```rust
pub struct IdleMonitor {
    last_activity: HashMap<String, Instant>,
    threshold: Duration,
}

impl IdleMonitor {
    pub fn record_activity(&mut self, source: &str) {
        self.last_activity.insert(source.to_string(), Instant::now());
    }
    
    pub fn is_idle(&self) -> bool {
        self.last_activity.values()
            .all(|t| t.elapsed() > self.threshold)
    }
}
```

**Pluggable Backends:**

| Backend | Use Case | Features |
|---------|----------|----------|
| `sqlite` (default) | Single-node, local | Lightweight, embedded |
| `postgres` | Multi-agent, team | Coordination, locking |

```rust
pub trait SchedulerBackend: Send + Sync {
    async fn store(&self, task: ScheduledTask) -> Result<()>;
    async fn load_due(&self, before: DateTime) -> Result<Vec<ScheduledTask>>;
    async fn delete(&self, id: TaskId) -> Result<()>;
    async fn claim(&self, id: TaskId, node: NodeId) -> Result<bool>;
}
```

**Session Context Model:**

Each scheduled task runs with its own session overlay:

```toml
[scheduler]
backend = "sqlite"  # or "postgres"

[[scheduler.tasks.check_email]]
trigger = { type = "interval", minutes = 5 }
action = { type = "mcp", mcp = "email", method = "check_inbox" }
context = { isolated = true, ttl = "1h" }

[[scheduler.tasks.daily_report]]
trigger = { type = "cron", expr = "0 9 * * 1-5", timezone = "Asia/Shanghai" }
action = { type = "skill", name = "generate_report" }
context = { isolated = false, inherit = "base" }

[[scheduler.tasks.cleanup]]
trigger = { type = "idle", minutes = 10 }  # After 10min of no activity
action = { type = "tool", name = "filesystem", args = { op = "cleanup_temp" } }
context = { isolated = true }
```

**Execution Guarantees:**

| Aspect | Behavior |
|--------|----------|
| Persistence | Schedules survive restarts |
| Overlap | Skip if previous run still executing (configurable) |
| Missed runs | Catch-up or drop (configurable) |
| Error handling | Exponential backoff, max retries |
| Concurrency | Parallel execution per task, isolated contexts |

**Agent Self-Scheduling:**

Agents can schedule themselves dynamically:

```rust
async fn handle_message(&mut self, msg: Message) -> Result<Response> {
    if msg.content.contains("remind me") {
        let when = parse_time(&msg.content)?;
        
        self.scheduler.schedule(ScheduledTask {
            trigger: Trigger::Once { at: when },
            action: Action::Message { 
                channel: "cli".to_string(), 
                content: "Reminder!".to_string() 
            },
            context: TaskContext::isolated(),
            ..Default::default()
        }).await?;
    }
    Ok(Response::text("Scheduled!"))
}
```

#### 4.1.2 Event Router

Routes internal events to agents:
- File system events
- Webhook deliveries
- Inter-agent messages
- System notifications

#### 4.1.3 Lifecycle Manager

Manages agent lifecycle:
- Spawn new agents
- Stop/restart agents
- Health checks
- Resource limits

#### 4.1.4 Agent Coordination

Agent Coordination enables **multiple agents to communicate and collaborate** via message passing. Unlike shared memory or direct method calls, agents remain isolated and interact through the coordination layer.

**Core Principle:** Message passing, not shared state. Each agent has its own memory, tools, and configuration.

**Coordination Patterns:**

| Pattern | Description | Use Case |
|---------|-------------|----------|
| **A2A Messaging** | Direct agent-to-agent request/reply | Researcher → Writer → Critic |
| **Broadcast** | One agent publishes to many subscribers | Market data to traders |
| **Group Chat** | Multiple agents + humans in shared thread | Collaborative content creation |
| **Workflows** | Sequential/parallel agent chains | Research → Write → Edit → Publish |

**Coordination API:**
```rust
pub trait AgentCoordination {
    /// Send message to specific agent
    async fn send_to_agent(
        &self,
        target_agent: &str,
        message: &str,
        wait_for_reply: bool,
    ) -> Result<AgentReply>;
    
    /// Subscribe agent to broadcast topic
    async fn subscribe(&self, agent: &str, topic: &str) -> Result<()>;
    
    /// Publish message to topic
    async fn publish(&self, topic: &str, message: &str) -> Result<()>;
    
    /// Start workflow instance
    async fn start_workflow(
        &self,
        workflow: &str,
        input: Value,
    ) -> Result<WorkflowId>;
}
```

**A2A Messaging:**
```toml
[coordination]
enabled = true

[coordination.a2a]
# Who can message whom (security boundary)
allow_rules = [
    { from = "researcher", to = ["writer", "critic"] },
    { from = "orchestrator", to = ["*"] },
    { from = "critic", to = ["writer"] },
]
max_ping_pong = 3  # Max reply turns per conversation
timeout_seconds = 60
```

**Broadcast / Pub-Sub:**
```toml
[coordination.broadcast]
topics = [
    { name = "alerts", subscribers = ["ops", "oncall"] },
    { name = "market_data", subscribers = ["trader", "analyst"] },
    { name = "system_events", subscribers = ["*"] },  # All agents
]
```

**Group Chat:**
```toml
[group_chats.market_analysis]
participants = ["researcher", "writer", "fact_checker"]
trigger_mode = "interval"  # or "immediate", "idle"
trigger_interval = "5m"    # Agents respond every 5 minutes
mention_patterns = ["@researcher", "@writer"]

[group_chats.code_review]
participants = ["coder", "reviewer", "tester"]
trigger_mode = "immediate"  # Respond immediately when mentioned
```

**Workflow Sequencing:**
```toml
[workflows.content_pipeline]
name = "Content Creation Pipeline"

[[workflows.content_pipeline.steps]]
agent = "researcher"
prompt_template = "Research topic: {{input.topic}}"
timeout = "5m"

[[workflows.content_pipeline.steps]]
agent = "writer"
prompt_template = "Write article based on: {{previous.output}}"
timeout = "10m"

[[workflows.content_pipeline.steps]]
agent = "editor"
prompt_template = "Edit: {{previous.output}}"
timeout = "5m"

workflows.content_pipeline.on_complete = { type = "notify", agent = "orchestrator" }
```

**Scheduled + Coordination Hybrid:**
```toml
[scheduler.tasks.market_update]
trigger = { type = "interval", minutes = 30 }
action = { type = "broadcast", topic = "market_data", message = "Fetch prices" }

[scheduler.tasks.daily_report]
trigger = { type = "cron", expr = "0 9 * * *" }
action = { type = "workflow", workflow = "content_pipeline", input = { topic = "Daily Summary" } }
```

**Backend Storage:**
Coordination state shares the scheduler backend (SQLite/Postgres):
- `a2a_conversations` - Active agent-to-agent conversations
- `broadcast_topics` - Topic subscriptions
- `group_chats` - Group chat configurations
- `workflow_instances` - Running workflow state

### 4.2 Communication Layer (Channels)

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

### 4.3 Memory Architecture

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

### 4.4 Bootstrap Components

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

### 4.5 Capability Tiers

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

### 4.6 Session Storage

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

### 7.4 Agent with Scheduled Tasks

```toml
name = "maintenance_bot"

[provider]
provider_type = "anthropic"

# Capabilities
[capabilities]
tools = ["filesystem", "fetch"]
mcp = ["database", "email"]

# Communication channels
[[channels]]
id = "cli"
type = "builtin"

# Scheduler configuration
[scheduler]
backend = "sqlite"  # or "postgres" for multi-agent

# Check email every 5 minutes
[[scheduler.tasks.check_email]]
trigger = { type = "interval", minutes = 5 }
action = { type = "mcp", mcp = "email", method = "check_inbox", args = { folder = "INBOX" } }
context = { isolated = true, ttl = "30m" }

# Daily report at 9 AM on weekdays
[[scheduler.tasks.daily_report]]
trigger = { type = "cron", expr = "0 9 * * 1-5", timezone = "Asia/Shanghai" }
action = { type = "skill", name = "generate_daily_report" }
context = { isolated = false, inherit = "base" }

# Cleanup temp files when idle for 10 minutes
[[scheduler.tasks.cleanup]]
trigger = { type = "idle", minutes = 10 }
action = { type = "tool", name = "filesystem", args = { operation = "cleanup", path = "/tmp" } }
context = { isolated = true }

# One-shot reminder
[[scheduler.tasks.reminder]]
trigger = { type = "once", at = "2026-03-15T09:00:00Z" }
action = { type = "message", channel = "cli", content = "Meeting in 15 minutes!" }
```

### 7.5 Multi-Agent Coordination

```toml
# Three specialized agents working together

# Researcher agent - gathers information
[[agents]]
id = "researcher"
name = "Research Bot"
provider = { type = "anthropic", model = "claude-sonnet-4" }
capabilities = { tools = ["web_search", "fetch"], mcp = ["browser"] }

# Writer agent - creates content
[[agents]]
id = "writer"
name = "Content Writer"
provider = { type = "anthropic", model = "claude-sonnet-4" }
capabilities = { tools = ["write", "edit"], mcp = ["memory-markdown"] }

# Critic agent - reviews and feedback
[[agents]]
id = "critic"
name = "Content Critic"
provider = { type = "anthropic", model = "claude-haiku-3" }
capabilities = { tools = ["read"] }

# Coordination configuration
[coordination]
enabled = true

[coordination.a2a]
# Define who can message whom
allow_rules = [
    { from = "researcher", to = ["writer"] },
    { from = "writer", to = ["critic", "researcher"] },
    { from = "critic", to = ["writer"] },
]
max_ping_pong = 3
timeout_seconds = 120

# Broadcast topics
[coordination.broadcast]
topics = [
    { name = "content_ideas", subscribers = ["researcher", "writer"] },
    { name = "review_requests", subscribers = ["critic"] },
]

# Group chat for collaborative sessions
[group_chats.content_team]
participants = ["researcher", "writer", "critic"]
trigger_mode = "immediate"
mention_patterns = ["@team"]

# Workflow: Automated content pipeline
[workflows.auto_content]
name = "Automated Content Pipeline"

[[workflows.auto_content.steps]]
agent = "researcher"
prompt_template = """
Research the topic: {{input.topic}}
Find 3-5 credible sources and extract key points.
Return a structured research summary.
"""
timeout = "10m"

[[workflows.auto_content.steps]]
agent = "writer"
prompt_template = """
Write a blog post based on this research:
{{previous.output}}

Target length: 500-800 words
Tone: {{input.tone | default: "professional"}}
"""
timeout = "15m"

[[workflows.auto_content.steps]]
agent = "critic"
prompt_template = """
Review this draft and provide feedback:
{{previous.output}}

Check for: accuracy, clarity, engagement
Return: "APPROVED" or specific revision requests.
"""
timeout = "5m"

# If critic approves, workflow ends
# If critic requests changes, loop back to writer
workflows.auto_content.on_complete = { type = "notify", agent = "orchestrator" }

# Scheduled workflow execution
[scheduler.tasks.daily_content]
trigger = { type = "cron", expr = "0 9 * * 1-5", timezone = "UTC" }
action = { 
    type = "workflow", 
    workflow = "auto_content",
    input = { topic = "Daily Tech News", tone = "casual" }
}
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
- **Scheduler with pluggable backends (SQLite, PostgreSQL)**

### Medium-term (6-12 months)
- External trust layer (signed extensions, audits)
- Multi-agent workflows (A2A orchestration)
- Memory MCP ecosystem (markdown, postgres, chroma, pinecone backends)
- Web dashboard channel
- **Advanced scheduler (Kubernetes CronJob, AWS EventBridge backends)**

### Long-term (12+ months)
- Distributed agent clusters
- WASM-based extensions
- Cross-runtime session portability

---

*Status: Revised per philosophy discussion - Three orthogonal layers (Orchestration, Communication, Capabilities)*
*Last updated: 2026-03-09*
