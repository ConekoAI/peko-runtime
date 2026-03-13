# Pekobot Agent Container Specification (PACS)

> Version: 1.0.0-draft  
> Status: Proposed Extension to Pekobot Grand Architecture

## 1. Overview

Pekobot Agent Container Specification (PACS) defines a portable, shareable, and composable format for AI agents. It enables:

- **Agent Packaging**: Bundle an agent's complete definition into a portable unit
- **Agent Distribution**: Share agents via registries (like Pekohub)
- **Team Composition**: Compose multiple agents into coordinated teams
- **Runtime Isolation**: Execute agents with their dependencies isolated

### 1.1 Design Goals

| Goal | Description |
|------|-------------|
| **Portability** | Run the same agent package across different Pekobot runtimes |
| **Composability** | Combine agents into teams with shared resources |
| **Layering** | Efficient storage through content-addressable layers |
| **Extensibility** | Add new capability types without breaking changes |
| **Familiarity** | Model inspired by OCI/Docker for developer ergonomics |

### 1.2 Core Concepts

```
┌─────────────────────────────────────────────────────────────────┐
│                    AGENT PACKAGE                                │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │ MANIFEST                                                │   │
│  │ • Package metadata (name, version, author)              │   │
│  │ • Base image reference                                  │   │
│  │ • Layer digests                                         │   │
│  │ • Provider configuration                                │   │
│  └─────────────────────────────────────────────────────────┘   │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │ LAYERS (content-addressable, immutable)                 │   │
│  │                                                         │   │
│  │ Layer 0: base://pekohub/agents/minimal:v1.2.0          │   │
│  │ Layer 1: system (bootstrap, prompt, identity)          │   │
│  │ Layer 2: capabilities (tools, MCPs, skills)            │   │
│  │ Layer 3: knowledge (files, memory, graphs)             │   │
│  │ Layer 4+: writable (runtime state, ephemeral)          │   │
│  └─────────────────────────────────────────────────────────┘   │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │ CONFIG (runtime parameters, mutable)                    │   │
│  │ • Environment variables                                 │   │
│  │ • Resource limits                                       │   │
│  │ • Network policies                                      │   │
│  └─────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
```

---

## 2. Agent Package Format

### 2.1 Package Structure

An agent package is a tar archive with the following structure:

```
agent-package.tar
├── manifest.toml          # Package manifest
├── config.toml            # Default runtime configuration
├── layers/
│   ├── 01-system.tar.gz   # System layer
│   ├── 02-capabilities.tar.gz
│   ├── 03-knowledge.tar.gz
│   └── ...
└── signatures/            # Optional: cryptographic signatures
    └── manifest.sig
```

### 2.2 Manifest Specification

```toml
# manifest.toml
[package]
name = "research-assistant"
version = "2.1.0"
description = "Multi-modal research agent with web and document capabilities"
author = "pekohub.com/user/researcher"
license = "MIT"

[package.labels]
category = "productivity"
tags = ["research", "web", "documents"]
homepage = "https://pekohub.com/packages/research-assistant"

# Base image - another agent package to extend
[base]
image = "pekohub.com/agents/minimal:v1.2.0"
digest = "sha256:abc123..."

# Runtime provider configuration
[provider]
provider_type = "anthropic"
model = "claude-3-5-sonnet-20241022"
temperature = 0.7
max_tokens = 4096

# Required runtime version
[runtime]
min_version = "2.0.0"
max_version = "3.0.0"

# Layer definitions (content-addressed)
[[layers]]
index = 1
digest = "sha256:layer1abc..."
media_type = "application/vnd.pekobot.layer.system.v1"
size = 10240

[[layers]]
index = 2
digest = "sha256:layer2def..."
media_type = "application/vnd.pekobot.layer.capabilities.v1"
size = 51200

[[layers]]
index = 3
digest = "sha256:layer3ghi..."
media_type = "application/vnd.pekobot.layer.knowledge.v1"
size = 1048576

# Capability requirements
[capabilities]
tools = [
    { name = "web_search", source = "pekohub.com/tools/web-search:v1", required = true },
    { name = "file_system", source = "builtin", required = true },
]

mcps = [
    { name = "browser", source = "pekohub.com/mcps/browser:v2", required = true },
    { name = "vector-memory", source = "pekohub.com/mcps/memory-chroma:v1", required = false },
]

skills = [
    { name = "research_pipeline", source = "pekohub.com/skills/research:v1", required = true },
]

# Exposed interfaces (for team composition)
[interfaces]
# Communication interfaces this agent supports
channels = ["cli", "http", "agent_protocol"]

# Tools this agent exposes to other agents
exposed_tools = ["research", "summarize"]

# Events this agent can emit/subscribe to
events = ["research.complete", "research.error"]
```

### 2.3 Layer Types

#### 2.3.1 System Layer (`application/vnd.pekobot.layer.system.v1`)

```
system/
├── identity/
│   ├── did.json              # Decentralized identifier
│   └── public_key.pem        # Signing key for agent-to-agent auth
├── bootstrap/
│   ├── system_prompt.md      # Primary system prompt
│   ├── persona.toml          # Personality configuration
│   └── init.kb               # Initialization knowledge base
└── hooks/
    ├── pre-start.sh          # Called before agent starts
    └── post-stop.sh          # Called after agent stops
```

#### 2.3.2 Capabilities Layer (`application/vnd.pekobot.layer.capabilities.v1`)

```
capabilities/
├── tools/
│   └── custom_tool.py        # Custom tool implementations
├── mcps/
│   └── custom_mcp_config.toml
├── skills/
│   └── custom_skill/
│       ├── skill.toml
│       └── workflow.py
└── schemas/
    └── tool_schemas.json     # JSON Schema for tools
```

#### 2.3.3 Knowledge Layer (`application/vnd.pekobot.layer.knowledge.v1`)

```
knowledge/
├── memory/
│   ├── vectors/              # Vector database snapshot
│   │   └── chroma_snapshot.tar.gz
│   └── index.json
├── documents/
│   ├── guidelines.md
│   ├── templates/
│   └── references/
├── graph/
│   ├── entities.jsonl
│   ├── relations.jsonl
│   └── schema.cypher
└── database/
    └── sqlite/
        └── knowledge.db
```

#### 2.3.4 Writable Layer (`application/vnd.pekobot.layer.writable.v1`)

Runtime-modified state that persists across restarts:

```
writable/
├── sessions/                 # Conversation history
│   └── {session_id}.jsonl
├── state/
│   ├── preferences.toml
│   └── checkpoints/
└── cache/
    └── tool_results/
```

### 2.4 Configuration Layer

Runtime parameters that can be overridden at instantiation:

```toml
# config.toml
[environment]
# Key-value pairs available to agent
SEARCH_API_KEY = "${SEARCH_API_KEY}"  # Interpolated from host env
WORKSPACE_DIR = "/workspace"

[resources]
max_memory_mb = 2048
max_cpu_percent = 50
disk_quota_gb = 10

[network]
# Network policies for tool/MCP access
allowed_hosts = ["api.example.com", "*.pekohub.com"]
blocked_hosts = ["internal.corp.local"]

[security]
sandbox_mode = "container"  # none, container, vm
read_only_paths = ["/knowledge"]
writable_paths = ["/workspace", "/writable"]

[runtime]
# Runtime behavior
auto_restart = true
health_check_interval_sec = 30
idle_timeout_min = 60
```

---

## 3. Team Composition

### 3.1 Team Specification

A team is a composition of agents with defined coordination patterns:

```toml
# team.toml
team_version = "1.0.0"

[metadata]
name = "enterprise-research-team"
description = "Multi-agent team for comprehensive research tasks"
author = "corp@example.com"

# Agent definitions
[[agents]]
name = "coordinator"
package = "pekohub.com/agents/coordinator:v3.0"
instance_count = 1

[agents.config]
environment = { PRIORITY_QUEUE = "true", MAX_SUBTASKS = "10" }
resources = { max_memory_mb = 1024 }

[agents.role]
type = "manager"
can_spawn = ["researcher", "writer", "analyst"]

[[agents]]
name = "researcher"
package = "pekohub.com/agents/researcher:v2.5"
instance_count = 3  # Scale-out workers

[agents.config]
environment = { SEARCH_DEPTH = "3", PARALLEL_SOURCES = "5" }

[agents.role]
type = "worker"
accepts_from = ["coordinator"]

[[agents]]
name = "writer"
package = "pekohub.com/agents/writer:v1.8"
instance_count = 2

[agents.role]
type = "worker"
accepts_from = ["coordinator", "analyst"]

[[agents]]
name = "analyst"
package = "pekohub.com/agents/analyst:v2.0"
instance_count = 1

[agents.role]
type = "worker"
accepts_from = ["researcher"]
emits_to = ["writer"]

# Shared resources (singletons per team)
[shared]

[shared.message_bus]
type = "in-memory"  # or "redis", "nats", "kafka"
config = { persistence = "false" }

[shared.memory]
# Shared vector database for team context
type = "chroma"
config = { embedding_model = "text-embedding-3-large" }
persistent_volume = "team-memory"

[shared.database]
# Shared structured storage
type = "postgresql"
config = { database = "team_db" }

[shared.filesystem]
# Shared workspace
type = "volume"
path = "/shared/workspace"

# Shared MCPs (heavy resources, shared across agents)
[[shared.mcps]]
name = "shared-browser"
package = "pekohub.com/mcps/browser:v3"
max_instances = 2  # Pool size

[[shared.mcps]]
name = "shared-email"
package = "pekohub.com/mcps/email:v1"
shared_config = { smtp_server = "mail.corp.com" }

# Coordination patterns
[coordination]
# Default communication pattern
default_pattern = "hierarchical"

# Custom coordination topologies
[[coordination.patterns]]
name = "research_pipeline"
type = "pipeline"
steps = [
    { agent = "coordinator", action = "decompose" },
    { agent = "researcher", action = "gather", parallel = true },
    { agent = "analyst", action = "synthesize" },
    { agent = "writer", action = "draft", parallel = true },
    { agent = "coordinator", action = "review" },
]

[[coordination.patterns]]
name = "emergency_broadcast"
type = "pubsub"
publisher = "coordinator"
subscribers = ["researcher", "writer", "analyst"]
topic = "critical_alert"

# Service discovery
[discovery]
# How agents find each other
registry_type = "local"  # local, consul, etcd, kubernetes

# Health and monitoring
[health]
check_interval_sec = 30
unhealthy_threshold = 3
restart_policy = "always"  # never, on-failure, always

# Team-wide secrets
[secrets]
# References to external secret stores
vault_provider = "file"  # file, hashicorp_vault, aws_secrets_manager
```

### 3.2 Agent Roles

Roles define behavioral contracts within a team:

```rust
pub enum AgentRole {
    // Manager: Can spawn, delegate, and coordinate other agents
    Manager {
        can_spawn: Vec<String>,        // Agent types this manager can spawn
        max_subordinates: usize,
        escalation_targets: Vec<String>,
    },
    
    // Worker: Executes tasks, reports to managers
    Worker {
        accepts_from: Vec<String>,     // Which agent types can assign work
        reports_to: Vec<String>,       // Which agents receive results
        max_concurrent_tasks: usize,
    },
    
    // Service: Stateless, shared utility agent
    Service {
        exposed_capabilities: Vec<String>,
        rate_limit: RateLimit,
    },
    
    // Observer: Read-only, monitors and logs
    Observer {
        watches: Vec<String>,          // Which agents to observe
        audit_level: AuditLevel,
    },
}
```

### 3.3 Communication Patterns

#### 3.3.1 Hierarchical (Manager-Worker)

```
┌─────────────┐
│ Coordinator │ (Manager)
└──────┬──────┘
       │ agent_spawn / agent_send
       ▼
┌─────────────┐  ┌─────────────┐  ┌─────────────┐
│ Researcher  │  │   Writer    │  │   Analyst   │  (Workers)
│     x3      │  │     x2      │  │     x1      │
└─────────────┘  └─────────────┘  └─────────────┘
```

#### 3.3.2 Pipeline

```
Input ──► [Stage 1] ──► [Stage 2] ──► [Stage 3] ──► Output
          Research     Analysis       Writing
```

#### 3.3.3 Pub-Sub

```
         ┌─────────────┐
         │  Publisher  │
         └──────┬──────┘
                │ publish(topic="alerts")
       ┌────────┼────────┐
       ▼        ▼        ▼
┌─────────┐ ┌─────────┐ ┌─────────┐
│  Sub 1  │ │  Sub 2  │ │  Sub 3  │
└─────────┘ └─────────┘ └─────────┘
```

#### 3.3.4 Mesh (Peer-to-Peer)

```
┌─────┐ ←────→ ┌─────┐
│  A  │        │  B  │
└──┬──┘        └──┬──┘
   │              │
   └──────┬───────┘
          ▼
       ┌─────┐
       │  C  │
       └──┬──┘
          │
   ┌──────┴───────┐
   ▼              ▼
┌─────┐        ┌─────┐
│  D  │ ←────→ │  E  │
└─────┘        └─────┘
```

---

## 4. Runtime Architecture

### 4.1 Package Manager

```rust
pub trait PackageManager {
    /// Pull an agent package from a registry
    async fn pull(&self, ref: PackageRef) -> Result<LocalPackage>;
    
    /// Push a local package to a registry
    async fn push(&self, pkg: LocalPackage, registry: &str) -> Result<()>;
    
    /// Export a running agent as a package
    async fn export(&self, agent_id: &str, name: &str) -> Result<Package>;
    
    /// List locally cached packages
    async fn list(&self) -> Vec<LocalPackage>;
    
    /// Remove a package from local cache
    async fn remove(&self, ref: PackageRef) -> Result<()>;
    
    /// Build a package from a directory
    async fn build(&self, context: &Path, tag: &str) -> Result<Package>;
}

pub struct PackageRef {
    pub registry: Option<String>,  // None = default registry
    pub namespace: String,         // user or org
    pub name: String,
    pub tag: String,               // version tag or "latest"
    pub digest: Option<String>,    // Optional: exact content hash
}
```

### 4.2 Team Runtime

```rust
pub struct TeamRuntime {
    /// Team specification
    spec: TeamSpec,
    
    /// Running agent instances
    agents: HashMap<String, AgentInstance>,
    
    /// Shared services
    shared: SharedServices,
    
    /// Message bus for inter-agent communication
    bus: Arc<dyn MessageBus>,
    
    /// Service registry for discovery
    registry: Arc<dyn ServiceRegistry>,
    
    /// Lifecycle manager
    lifecycle: Arc<dyn LifecycleManager>,
}

impl TeamRuntime {
    /// Deploy a team from specification
    pub async fn deploy(spec: TeamSpec) -> Result<Self>;
    
    /// Scale an agent type up or down
    pub async fn scale(&self, agent_name: &str, count: usize) -> Result<()>;
    
    /// Send a message to a specific agent
    pub async fn send(&self, target: &str, message: Message) -> Result<()>;
    
    /// Broadcast to all agents matching filter
    pub async fn broadcast(&self, filter: AgentFilter, message: Message) -> Result<()>;
    
    /// Get team status
    pub async fn status(&self) -> TeamStatus;
    
    /// Graceful shutdown
    pub async fn shutdown(&self, timeout: Duration) -> Result<()>;
}
```

### 4.3 Shared Services Fabric

```rust
pub struct SharedServices {
    /// Shared MCP instances
    mcps: HashMap<String, SharedMcpHandle>,
    
    /// Shared storage volumes
    volumes: HashMap<String, VolumeHandle>,
    
    /// Shared memory/vector stores
    memory: Option<Arc<dyn SharedMemory>>,
    
    /// Shared databases
    databases: HashMap<String, DatabaseHandle>,
    
    /// Message bus
    bus: Arc<dyn MessageBus>,
}

/// Handle to a shared MCP (reference counted)
pub struct SharedMcpHandle {
    /// The MCP instance
    mcp: Arc<dyn Mcp>,
    
    /// Reference count (how many agents are using it)
    refs: AtomicUsize,
    
    /// Configuration
    config: McpConfig,
}
```

### 4.4 Inter-Agent Protocol

Agents communicate via a standardized protocol:

```rust
pub enum AgentMessage {
    /// Direct message (1-to-1)
    Direct {
        from: AgentId,
        to: AgentId,
        content: MessageContent,
        correlation_id: Option<String>,
    },
    
    /// Task assignment (manager to worker)
    Task {
        from: AgentId,
        to: AgentId,
        task_id: String,
        task_type: String,
        parameters: Value,
        deadline: Option<DateTime<Utc>>,
        priority: Priority,
    },
    
    /// Task result (worker to manager)
    TaskResult {
        from: AgentId,
        to: AgentId,
        task_id: String,
        status: TaskStatus,
        result: Option<Value>,
        error: Option<String>,
    },
    
    /// Event broadcast (pub-sub)
    Event {
        from: AgentId,
        topic: String,
        event_type: String,
        payload: Value,
    },
    
    /// Heartbeat for health monitoring
    Heartbeat {
        from: AgentId,
        status: HealthStatus,
        metrics: Option<Value>,
    },
    
    /// Capability discovery
    Discovery {
        from: AgentId,
        capabilities: Vec<Capability>,
    },
}

pub struct MessageContent {
    pub format: ContentFormat,  // text, json, markdown, binary
    pub data: Vec<u8>,
    pub metadata: HashMap<String, String>,
}
```

---

## 5. Lifecycle Management

### 5.1 Agent Lifecycle States

```
┌─────────┐    pull     ┌─────────┐    create    ┌─────────┐
│  Remote │ ───────────►│  Local  │ ───────────►│ Created │
│ Package │             │ Package │             │         │
└─────────┘             └─────────┘             └────┬────┘
                                                   │
                         start                       │
                          ▼                         │
┌─────────┐    stop    ┌─────────┐    fail        │
│ Stopped │ ◄───────── │ Running │ ◄──────────────┘
│         │            │         │────┐
└────┬────┘            └────┬────┘    │
     │                      │         │
     │ restart              │ pause   │
     ▼                      ▼         │
┌─────────┐              ┌─────────┐  │
│ Failed  │              │ Paused  │◄─┘
└─────────┘              └─────────┘
     ▲
     │ destroy
     └────────────────────┐
                         ▼
                   ┌─────────┐
                   │ Destroy │
                   └─────────┘
```

### 5.2 Team Lifecycle

```rust
pub enum TeamState {
    /// Team spec loaded, validating
    Validating,
    
    /// Pulling required packages
    Pulling,
    
    /// Initializing shared services
    Initializing,
    
    /// Starting agents
    Starting,
    
    /// All agents running and healthy
    Running,
    
    /// Some agents unhealthy but team operational
    Degraded,
    
    /// Team failure, cannot continue
    Failed,
    
    /// Graceful shutdown in progress
    Stopping,
    
    /// Team stopped
    Stopped,
}
```

---

## 6. Security Model

### 6.1 Trust Layers

```
┌─────────────────────────────────────────────────────────────┐
│                    USER (Ultimate Trust)                    │
│  • Reviews package manifests                                │
│  • Approves network/resource policies                       │
│  • Configures secret access                                 │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                 REGISTRY (Reputation Layer)                 │
│  • Package signing and verification                         │
│  • Vulnerability scanning                                   │
│  • Download statistics and reviews                          │
│  • Security audit attestations                              │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│               PACKAGE MANAGER (Verification)                │
│  • Signature verification                                   │
│  • Digest validation                                        │
│  • Layer integrity checks                                   │
│  • Provenance tracking                                      │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                  RUNTIME (Isolation)                        │
│  • Optional container sandboxing                            │
│  • Network policy enforcement                               │
│  • Resource limits                                          │
│  • Audit logging                                            │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                TEAM FABRIC (Authorization)                  │
│  • Agent-to-agent authentication                            │
│  • Capability-based access control                          │
│  • Message encryption (optional)                            │
│  • Audit trail of all inter-agent communication             │
└─────────────────────────────────────────────────────────────┘
```

### 6.2 Agent-to-Agent Authentication

```rust
/// Mutual authentication between agents
pub struct AgentIdentity {
    /// Decentralized identifier
    did: String,
    
    /// Public key for verification
    public_key: Ed25519PublicKey,
    
    /// Certificate from team CA (optional)
    certificate: Option<X509Certificate>,
    
    /// Team membership proof
    team_attestation: TeamAttestation,
}

pub struct AuthenticatedMessage {
    /// Encrypted payload
    payload: EncryptedData,
    
    /// Sender identity
    from: AgentIdentity,
    
    /// Signature
    signature: Signature,
    
    /// Timestamp for replay protection
    timestamp: DateTime<Utc>,
    
    /// Nonce
    nonce: [u8; 32],
}
```

### 6.3 Capability-Based Security

Agents only access what they're explicitly granted:

```toml
# Team-level capability grants
[[capabilities.grants]]
agent = "researcher"
permissions = [
    "shared.browser:read",
    "shared.browser:write",
    "shared.memory:query",
    "agent.writer:send",
]

[[capabilities.grants]]
agent = "writer"
permissions = [
    "shared.filesystem:read",
    "shared.filesystem:write",
    "shared.database:read",
    "agent.coordinator:send",
]
```

---

## 7. CLI Interface

### 7.1 Package Management

```bash
# Pull an agent package from registry
pekobot pull pekohub.com/agents/researcher:v2.0

# Build a package from current directory
pekobot build -t my-agent:v1.0 .

# Push to registry
pekobot push my-agent:v1.0 pekohub.com/user/my-agent:v1.0

# List local packages
pekobot packages list

# Inspect package manifest
pekobot packages inspect pekohub.com/agents/researcher:v2.0

# Export running agent as package
pekobot export coordinator my-coordinator:v1.0-backup
```

### 7.2 Team Management

```bash
# Deploy a team from specification
pekobot team deploy -f research-team.toml

# Scale a specific agent
pekobot team scale research-team researcher 5

# View team status
pekobot team status research-team

# Send message to specific agent
pekobot team send research-team coordinator "Start new research on AI safety"

# View logs from all agents
pekobot team logs research-team --follow

# Stop team gracefully
pekobot team stop research-team

# Restart team
pekobot team restart research-team
```

### 7.3 Running Individual Agents

```bash
# Run agent package directly
pekobot run pekohub.com/agents/researcher:v2.0

# Run with custom config
pekobot run -e API_KEY=secret -v ./workspace:/workspace pekohub.com/agents/researcher:v2.0

# Run and export session on exit
pekobot run --save-session ./session.jsonl pekohub.com/agents/researcher:v2.0
```

---

## 8. Registry API

### 8.1 Package Discovery

```http
GET /v2/agents/search?q=research&sort=downloads
Authorization: Bearer {token}

{
  "packages": [
    {
      "name": "researcher",
      "namespace": "pekohub.com/agents",
      "versions": ["v2.0", "v1.9", "v1.8"],
      "downloads": 15000,
      "rating": 4.8,
      "verified": true,
      "labels": {
        "category": "productivity",
        "tags": ["research", "web", "documents"]
      }
    }
  ]
}
```

### 8.2 Package Manifest

```http
GET /v2/agents/{namespace}/{name}/manifests/{tag}

{
  "schemaVersion": 2,
  "mediaType": "application/vnd.pekobot.manifest.v1+json",
  "config": {
    "mediaType": "application/vnd.pekobot.config.v1+json",
    "size": 7023,
    "digest": "sha256:e3b0c44298fc1c149afbf4c8996fb924..."
  },
  "layers": [
    {
      "mediaType": "application/vnd.pekobot.layer.system.v1",
      "size": 32654,
      "digest": "sha256:e3b0c44298fc1c149afbf4c8996fb924..."
    }
  ],
  "annotations": {
    "org.opencontainers.image.title": "Research Assistant Agent",
    "org.opencontainers.image.description": "Multi-modal research agent"
  }
}
```

---

## 9. Migration from Current Architecture

### 9.1 Backward Compatibility

Existing TOML agent configs remain valid:

```toml
# Old format - still supported
name = "my-agent"
[provider]
provider_type = "anthropic"

# Automatically wrapped in package format internally
```

### 9.2 Upgrade Path

```bash
# Convert existing agent to package format
pekobot package convert ./my-agent.toml -t my-agent:v1.0

# Convert and push to registry
pekobot package convert ./my-agent.toml -t my-agent:v1.0 --push pekohub.com/user/my-agent:v1.0
```

---

## 10. Appendix

### 10.1 Media Types

| Media Type | Description |
|------------|-------------|
| `application/vnd.pekobot.manifest.v1+json` | Package manifest |
| `application/vnd.pekobot.config.v1+json` | Runtime configuration |
| `application/vnd.pekobot.layer.system.v1` | System layer (prompts, identity) |
| `application/vnd.pekobot.layer.capabilities.v1` | Tools, MCPs, skills |
| `application/vnd.pekobot.layer.knowledge.v1` | Memory, documents, graphs |
| `application/vnd.pekobot.layer.writable.v1` | Runtime-writable state |
| `application/vnd.pekobot.team.spec.v1+json` | Team specification |

### 10.2 Glossary

| Term | Definition |
|------|------------|
| **Agent Package** | Portable, versioned bundle containing agent definition |
| **Layer** | Content-addressable filesystem layer |
| **Base Image** | Parent agent package to extend |
| **Team** | Multi-agent composition with coordination patterns |
| **Shared Service** | Singleton resource shared across team agents |
| **Message Bus** | Inter-agent communication fabric |
| **Agent Role** | Behavioral contract (manager, worker, service, observer) |

---

*Version: 1.0.0-draft*  
*Status: Proposed*  
*Last updated: 2026-03-13*
