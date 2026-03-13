# Pekobot Grand Architecture v2.0

> A **containerized multi-agent runtime** with portable agent packages, team composition, and shared service fabric. Zero security guarantees from core—audit trail only.

## 1. Vision

Pekobot is a **runtime for containerized AI agent systems** that enables:

- **Agent Packaging**: Bundle agents into portable, versioned, shareable containers
- **Team Composition**: Orchestrate multi-agent teams with defined coordination patterns  
- **Shared Services**: Efficiently share heavy resources (MCPs, memory, databases) across agents
- **Minimal Core**: ~5MB runtime, everything else packaged and distributed
- **Explicit Trust**: Security through registry reputation, signing, and user review—not runtime enforcement

### 1.1 The "Docker for Agents" Model

| Docker Concept | Pekobot Equivalent |
|----------------|-------------------|
| Docker Image | **Agent Package** - Portable agent definition |
| Dockerfile | `agent.toml` + build context |
| Docker Hub | **Pekohub** - Agent registry |
| Container | **Agent Instance** - Running agent process |
| Docker Compose | **Team** - Multi-agent composition |
| Volume | **Shared Service** - Team-wide resources |
| dockerd | **Pekobot Runtime** - Execution engine |

## 2. Design Philosophy

### 2.1 Four-Layer Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                     COMPOSITION LAYER                            │
│                    (Multi-Agent Teams)                           │
│                                                                  │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │ Team Specification (team.toml)                          │   │
│  │ • Agent definitions and scaling                         │   │
│  │ • Shared service configuration                          │   │
│  │ • Coordination patterns (hierarchy, pipeline, mesh)    │   │
│  └─────────────────────────────────────────────────────────┘   │
│                              │                                   │
│                              ▼                                   │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │ Team Runtime                                            │   │
│  │ • Agent lifecycle management                            │   │
│  │ • Shared service fabric                                 │   │
│  │ • Inter-agent message bus                               │   │
│  │ • Service discovery                                     │   │
│  └─────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                     PACKAGING LAYER                              │
│                   (Agent Container System)                       │
│                                                                  │
│  ┌─────────────┐  ┌─────────────┐  ┌──────────────────────┐    │
│  │   Package   │  │    Build    │  │     Registry         │    │
│  │   Format    │  │   System    │  │   Integration        │    │
│  │             │  │             │  │                      │    │
│  │• Manifest   │  │• Build ctx  │  │• Push/Pull           │    │
│  │• Layers     │  │• Layer cache│  │• Search              │    │
│  │• Base imgs  │  │• Export     │  │• Signing             │    │
│  └─────────────┘  └─────────────┘  └──────────────────────┘    │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│                     EXECUTION LAYER                              │
│                    (Agent Runtime Core)                          │
│                                                                  │
│  ┌─────────────┐  ┌─────────────┐  ┌──────────────────────┐    │
│  │   Agent     │  │  Invocation │  │   Session            │    │
│  │   Runtime   │  │   Router    │  │   Manager            │    │
│  │             │  │             │  │                      │    │
│  │• Identity   │  │• Channel    │  │• Session overlays    │    │
│  │• Provider   │  │  routing    │  │• Persistence         │    │
│  │• Tool loop  │  │• Scheduling │  │• Portability         │    │
│  └─────────────┘  └─────────────┘  └──────────────────────┘    │
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
│  │• agent_send │  │• browser    │  │• coding_assistant    │    │
│  │• agent_spawn│  │• database   │  │• group_chat_manager   │    │
│  │• read/write│  │• memory-*    │  │• workflow_engine     │    │
│  └─────────────┘  └─────────────┘  └──────────────────────┘    │
│                                                                  │
│  Sources: Built-in | Registry | Package-bundled                 │
└─────────────────────────────────────────────────────────────────┘
```

### 2.2 Core Provides Mechanisms, Not Policy

The core runtime is **deliberately agnostic** about security:

| Aspect | Core Stance |
|--------|-------------|
| Sandboxing | Optional (configurable per agent/team) |
| Permission system | None at core (team-level capability grants) |
| Content filtering | None |
| Execution limits | User-configurable (timeouts, resources) |
| Audit trail | **Complete** (session JSONL, tool logs, inter-agent messages) |

**Security is the user's responsibility** after reviewing package manifests, registry reputation, and team policies.

### 2.3 Trust Model

```
┌─────────────────────────────────────────────────────────────┐
│ EXTERNAL REGISTRY (e.g., Pekohub)                          │
│ • Package signing and verification                          │
│ • Content-addressable layers (digest verification)         │
│ • Community reputation / reviews                           │
│ • Vulnerability scanning                                    │
│ • Security audit attestations                              │
└─────────────────────────────────────────────────────────────┘
                            ↓ User decides to pull
┌─────────────────────────────────────────────────────────────┐
│ PACKAGE MANAGER                                             │
│ • Signature verification                                    │
│ • Layer integrity checks (sha256)                          │
│ • Provenance tracking                                       │
│ • Local layer caching                                       │
└─────────────────────────────────────────────────────────────┘
                            ↓ User decides to run
┌─────────────────────────────────────────────────────────────┐
│ TEAM RUNTIME                                                │
│ • Optional container sandboxing (if configured)            │
│ • Network policy enforcement                               │
│ • Resource limits (cgroup/systemd)                         │
│ • Inter-agent authentication                               │
└─────────────────────────────────────────────────────────────┘
                            ↓ Runtime
┌─────────────────────────────────────────────────────────────┐
│ AUDIT TRAIL                                                 │
│ • Every package installation logged                        │
│ • Every agent spawn/stop logged                            │
│ • Every inter-agent message logged                         │
│ • Every tool call logged with full arguments               │
│ • Session transcripts in JSONL                             │
│ • Queryable via `pekobot audit`                            │
└─────────────────────────────────────────────────────────────┘
```

**The user is the security boundary.** Registry verifies. Manager validates. Runtime isolates (if configured). User decides. Audit logs for review.

### 2.4 Tool Execution Model

Tools are **synchronous and blocking**. The agent loop waits for tool completion before continuing.

```
┌─────────────────────────────────────────────────────────────┐
│                 SYNCHRONOUS TOOL EXECUTION                   │
├─────────────────────────────────────────────────────────────┤
│                                                              │
│  TOOL: Simple async function                                │
│  pub async fn call(&self, args: Value) -> Result<Value>      │
│                                                              │
│  EXECUTION: Block until result                              │
│  • Agent calls tool → Waits → Receives result → Continues   │
│  • Timeout prevents runaway processes                       │
│                                                              │
│  ASYNC PATTERNS (when needed):                              │
│  • Shell background: command &                              │
│  • MCP async: submit_task → poll status → get_result        │
│  • Agent spawn: creates independent agent instance          │
│  • Team broadcast: fire-and-forget via message bus          │
│                                                              │
└─────────────────────────────────────────────────────────────┘
```

**Rationale:**
- Agent loop is inherently sequential (needs tool result to continue reasoning)
- Simpler mental model - no async complexity in core
- Long-running tasks: use shell background, MCP async patterns, or spawn agents
- True parallelism: use `agent_spawn` to create separate agent instances
- Team coordination: use message bus for async inter-agent communication

## 3. System Architecture

### 3.1 Composition Layer (Teams)

The **Team Runtime** orchestrates multi-agent systems:

```
┌─────────────────────────────────────────────────────────────────┐
│                      TEAM RUNTIME                                │
│                                                                  │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │ Team Controller                                           │   │
│  │ • Parses team.toml                                        │   │
│  │ • Manages agent lifecycle                                 │   │
│  │ • Handles scaling events                                  │   │
│  │ • Monitors health                                         │   │
│  └─────────────────────────────────────────────────────────┘   │
│                              │                                    │
│  ┌───────────────────────────▼──────────────────────────┐       │
│  │              Shared Services Fabric                   │       │
│  │                                                       │       │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────────────┐   │       │
│  │  │  Shared  │  │  Shared  │  │  Message Bus     │   │       │
│  │  │   MCPs   │  │  Memory  │  │  (inter-agent)   │   │       │
│  │  │  (pool)  │  │ (vector) │  │                  │   │       │
│  │  └──────────┘  └──────────┘  └──────────────────┘   │       │
│  │                                                       │       │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────────────┐   │       │
│  │  │  Shared  │  │  Shared  │  │  Service         │   │       │
│  │  │ Database │  │Filesystem│  │  Registry        │   │       │
│  │  │          │  │ (volume) │  │  (discovery)     │   │       │
│  │  └──────────┘  └──────────┘  └──────────────────┘   │       │
│  └──────────────────────────────────────────────────────┘       │
│                              │                                    │
│  ┌───────────────────────────▼──────────────────────────┐       │
│  │              Agent Instances                          │       │
│  │                                                       │       │
│  │  ┌────────┐ ┌────────┐ ┌────────┐ ┌────────┐        │       │
│  │  │Agent 1 │ │Agent 2 │ │Agent 3 │ │Agent 4 │ ...    │       │
│  │  │(coord) │ │(worker)│ │(worker)│ │(worker)│        │       │
│  │  └────────┘ └────────┘ └────────┘ └────────┘        │       │
│  │                                                       │       │
│  └──────────────────────────────────────────────────────┘       │
└─────────────────────────────────────────────────────────────────┘
```

#### Team Communication Patterns

**Hierarchical (Manager-Worker):**
```
┌─────────────┐
│ Coordinator │ (Manager)
│  spawns and │
│  delegates  │
└──────┬──────┘
       │ agent_spawn / agent_send
       ▼
┌─────────────┐  ┌─────────────┐  ┌─────────────┐
│ Researcher  │  │   Writer    │  │   Analyst   │
│     x3      │  │     x2      │  │     x1      │
└─────────────┘  └─────────────┘  └─────────────┘
```

**Pipeline:**
```
Input ──► [Research] ──► [Analyze] ──► [Write] ──► Output
              │              │            │
              └──────────────┴────────────┘
                    Shared Memory
```

**Pub-Sub:**
```
         ┌─────────────┐
         │  Publisher  │
         │  (emits     │
         │   events)   │
         └──────┬──────┘
                │ publish(topic="results")
       ┌────────┼────────┐
       ▼        ▼        ▼
┌─────────┐ ┌─────────┐ ┌─────────┐
│  Sub 1  │ │  Sub 2  │ │  Sub 3  │
└─────────┘ └─────────┘ └─────────┘
```

### 3.2 Packaging Layer

**Agent Package Format** (inspired by OCI):

```
agent-package.tar
├── manifest.toml          # Package manifest
├── config.toml            # Default runtime configuration  
├── layers/
│   ├── 00-base.tar.gz     # Base image layer
│   ├── 01-system.tar.gz   # System layer (prompts, identity)
│   ├── 02-capabilities.tar.gz  # Tools, MCPs, skills
│   ├── 03-knowledge.tar.gz     # Memory, documents, graphs
│   └── 04-writable.tar.gz      # Runtime state (ephemeral)
└── signatures/
    └── manifest.sig
```

**Layer Types:**

| Layer | Media Type | Content | Mutable |
|-------|------------|---------|---------|
| System | `application/vnd.pekobot.layer.system.v1` | Identity, prompts, bootstrap | No |
| Capabilities | `application/vnd.pekobot.layer.capabilities.v1` | Tools, MCPs, skills | No |
| Knowledge | `application/vnd.pekobot.layer.knowledge.v1` | Memory, documents, graphs | No |
| Writable | `application/vnd.pekobot.layer.writable.v1` | Sessions, state, cache | Yes |

### 3.3 Execution Layer

```rust
pub struct AgentRuntime {
    /// Agent identity (DID)
    identity: AgentIdentity,
    
    /// Session management
    session_manager: SessionManager,
    
    /// Tool registry (built-in + package-bundled)
    tools: ToolRegistry,
    
    /// MCP connections (shared or per-agent)
    mcps: McpManager,
    
    /// LLM provider
    provider: Arc<dyn Provider>,
    
    /// Execution engine
    engine: AgenticLoopV4,
    
    /// Inter-agent communication (if part of team)
    team_bus: Option<Arc<dyn MessageBus>>,
}

impl AgentRuntime {
    /// Main agent loop
    pub async fn run(&mut self) -> Result<()>;
    
    /// Handle incoming message (from channel or another agent)
    pub async fn handle_message(&mut self, msg: AgentMessage) -> Result<Response>;
    
    /// Send message to another agent (via team bus)
    pub async fn send_to(&self, target: &AgentId, msg: MessageContent) -> Result<()>;
}
```

### 3.4 Capability Layer

**Three-Tier Model:**

| Tier | Scope | Examples | Source |
|------|-------|----------|--------|
| **Tools** | Atomic, stateless | `read`, `write`, `agent_send`, `agent_spawn` | Built-in, package |
| **MCPs** | Bundled, stateful | `browser`, `database`, `memory-*` | Package, shared |
| **Skills** | Workflows | `coding_assistant`, `research_pipeline` | Package |

**Capability Sources:**

1. **Built-in**: Core primitives (`agent_send`, `agent_spawn`, filesystem)
2. **Package-bundled**: Included in agent package layers
3. **Shared**: Team-level singletons (efficient for heavy MCPs)
4. **Registry**: Dynamically fetched from Pekohub

## 4. Component Details

### 4.1 Team Specification

```toml
# team.toml
team_version = "1.0.0"

[metadata]
name = "research-team"
description = "Multi-agent research team"

# Agent definitions
[[agents]]
name = "coordinator"
package = "pekohub.com/agents/coordinator:v3.0"
instance_count = 1

[agents.role]
type = "manager"
can_spawn = ["researcher", "writer"]

[[agents]]
name = "researcher"
package = "pekohub.com/agents/researcher:v2.5"
instance_count = 3  # Scale-out

[agents.role]
type = "worker"
accepts_from = ["coordinator"]

# Shared resources
[shared]

[shared.memory]
type = "chroma"
persistent_volume = "team-memory"

[shared.message_bus]
type = "in-memory"

# Shared MCPs (singletons)
[[shared.mcps]]
name = "shared-browser"
package = "pekohub.com/mcps/browser:v3"
max_instances = 2

# Coordination patterns
[coordination]
default_pattern = "hierarchical"

[[coordination.pipelines]]
name = "research_workflow"
steps = [
    { agent = "coordinator", action = "decompose" },
    { agent = "researcher", action = "gather", parallel = true },
    { agent = "writer", action = "draft" },
]
```

### 4.2 Agent Package Manifest

```toml
# manifest.toml
[package]
name = "research-assistant"
version = "2.1.0"
author = "pekohub.com/user/researcher"

# Base image (inheritance)
[base]
image = "pekohub.com/agents/minimal:v1.2.0"
digest = "sha256:abc123..."

# Provider configuration
[provider]
provider_type = "anthropic"
model = "claude-3-5-sonnet-20241022"

# Layer definitions
[[layers]]
index = 1
digest = "sha256:layer1abc..."
media_type = "application/vnd.pekobot.layer.system.v1"

# Capability requirements
[capabilities]
tools = [
    { name = "web_search", source = "pekohub.com/tools/web-search:v1" },
]

mcps = [
    { name = "browser", source = "pekohub.com/mcps/browser:v2" },
]

skills = [
    { name = "research_pipeline", source = "pekohub.com/skills/research:v1" },
]

# Exposed interfaces (for team composition)
[interfaces]
channels = ["cli", "http", "agent_protocol"]
exposed_tools = ["research", "summarize"]
events = ["research.complete", "research.error"]
```

### 4.3 Package Manager

```rust
pub trait PackageManager {
    /// Pull from registry
    async fn pull(&self, ref: PackageRef) -> Result<LocalPackage>;
    
    /// Push to registry
    async fn push(&self, pkg: LocalPackage, registry: &str) -> Result<()>;
    
    /// Build from directory
    async fn build(&self, context: &Path, tag: &str) -> Result<Package>;
    
    /// Export running agent
    async fn export(&self, agent_id: &str, name: &str) -> Result<Package>;
    
    /// List local packages
    async fn list(&self) -> Vec<LocalPackage>;
}
```

### 4.4 Session-Centric State

**Agent Session Structure:**

```
Agent Session:
├── Base Session (shared across all invocation sources)
│   ├── Conversation history (JSONL)
│   ├── Tool execution history
│   └── User preferences
│
├── Channel Overlays (Communication Layer)
│   ├── CLI: Terminal formatting, local paths
│   ├── Discord: Guild IDs, user mappings
│   └── HTTP: Request context, headers
│
├── Team Overlay (Team Layer)
│   ├── Team context (shared knowledge)
│   ├── Agent role state
│   └── Inter-agent message history
│
└── Spawn Overlays (from agent_spawn)
    ├── Isolated sub-task context
    └── Can be promoted to base or merged back
```

### 4.5 Inter-Agent Protocol

```rust
pub enum AgentMessage {
    /// Direct message
    Direct {
        from: AgentId,
        to: AgentId,
        content: MessageContent,
        correlation_id: Option<String>,
    },
    
    /// Task assignment
    Task {
        from: AgentId,
        to: AgentId,
        task_id: String,
        task_type: String,
        parameters: Value,
        deadline: Option<DateTime<Utc>>,
    },
    
    /// Task result
    TaskResult {
        from: AgentId,
        to: AgentId,
        task_id: String,
        status: TaskStatus,
        result: Option<Value>,
        error: Option<String>,
    },
    
    /// Event broadcast
    Event {
        from: AgentId,
        topic: String,
        event_type: String,
        payload: Value,
    },
    
    /// Heartbeat
    Heartbeat {
        from: AgentId,
        status: HealthStatus,
    },
}
```

## 5. CLI Interface

### 5.1 Package Commands

```bash
# Pull an agent package
pekobot pull pekohub.com/agents/researcher:v2.0

# Build from directory
pekobot build -t my-agent:v1.0 .

# Push to registry
pekobot push my-agent:v1.0 pekohub.com/user/my-agent:v1.0

# List local packages
pekobot packages list

# Inspect package
pekobot packages inspect pekohub.com/agents/researcher:v2.0

# Export running agent
pekobot export coordinator my-coordinator:v1.0-backup
```

### 5.2 Team Commands

```bash
# Deploy team
pekobot team deploy -f research-team.toml

# Scale agent type
pekobot team scale research-team researcher 5

# View team status
pekobot team status research-team

# Send message to agent
pekobot team send research-team coordinator "Start research"

# View logs
pekobot team logs research-team --follow

# Stop team
pekobot team stop research-team
```

### 5.3 Run Commands

```bash
# Run single agent
pekobot run pekohub.com/agents/researcher:v2.0

# Run with custom config
pekobot run -e API_KEY=secret -v ./workspace:/workspace pekohub.com/agents/researcher:v2.0

# Run and save session
pekobot run --save-session ./session.jsonl pekohub.com/agents/researcher:v2.0
```

## 6. Memory Architecture

### 6.1 First-Order Memory (Context)

**Built-in, always present:**
- Session JSONL files
- Immediate conversation history
- Team context overlay

### 6.2 Second-Order Memory (Long-term)

**Package-bundled or Shared:**
- `memory-markdown` - MD files + SQLite vectors
- `memory-postgres` - PostgreSQL + pgvector (shared)
- `memory-chroma` - ChromaDB (shared)
- `memory-pinecone` - Pinecone (shared)

### 6.3 Third-Order Memory (Team)

**Shared across team agents:**
- Team knowledge graph
- Shared vector embeddings
- Collaborative memory spaces

## 7. Migration from v1.x

### 7.1 Backward Compatibility

Existing TOML agent configs remain valid and are automatically wrapped:

```toml
# v1 format - still supported
name = "my-agent"
[provider]
provider_type = "anthropic"

# Internally converted to package format:
# manifest.toml with implicit base image
```

### 7.2 Upgrade Path

```bash
# Convert existing agent to package
pekobot package convert ./my-agent.toml -t my-agent:v1.0

# Convert and push
pekobot package convert ./my-agent.toml -t my-agent:v1.0 --push
```

## 8. Anti-Goals

What Pekobot explicitly avoids:

- **Enterprise RBAC**: Role-based access is organization-specific (team-level only)
- **Content moderation**: Speech is the user's responsibility
- **Vendor lock-in**: Open protocols, portable sessions, standard formats
- **Cloud dependency**: Self-hosted by design, cloud optional
- **Heavy coordination in core**: Complex patterns are team configurations, not core features
- **Automatic trust**: Users must explicitly review and approve packages

## 9. Related Concepts

| Concept | Analogy | Pekobot Equivalent |
|---------|---------|-------------------|
| Unix shell | Command execution | Agent runtime |
| apt/npm | Package manager | `pekobot pull/push` |
| Docker | Containerization | Agent packages |
| Docker Compose | Multi-container apps | Teams |
| Kubernetes | Container orchestration | Team runtime (lightweight) |
| IRC bouncer | Multi-client presence | Multi-channel agent |
| Shell script | Automation | Skills |
| Thread pool | Parallel execution | Agent spawn / team workers |

## 10. Future Directions

### Near-term (3-6 months)
- PACS specification stabilization
- Pekohub agent registry launch
- Base agent images (minimal, standard, full)
- Basic team composition (hierarchical patterns)

### Medium-term (6-12 months)
- Advanced coordination patterns (mesh, pipeline, pub-sub)
- WASM-based tool sandboxing
- Distributed team runtime (multi-node)
- Web dashboard for team monitoring

### Long-term (12+ months)
- Autonomous team scaling based on load
- Cross-runtime team composition
- Agent package marketplace with ratings/reviews
- Formal verification of agent capabilities

---

*Version: 2.0*  
*Status: Proposed Architecture*  
*Last updated: 2026-03-13*

## Related Documents

- [Agent Container Specification (PACS)](./AGENT_CONTAINER_SPEC.md) - Detailed package format specification
