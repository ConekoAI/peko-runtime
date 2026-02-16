# Pekobot Architecture

This document describes the internal architecture of Pekobot, explaining how the different components work together.

## Table of Contents

1. [High-Level Architecture](#high-level-architecture)
2. [Component Overview](#component-overview)
3. [Data Flow](#data-flow)
4. [Module Details](#module-details)
5. [Agent Lifecycle](#agent-lifecycle)
6. [A2A Protocol](#a2a-protocol)
7. [Memory System](#memory-system)
8. [Identity System](#identity-system)

---

## High-Level Architecture

```mermaid
graph TB
    subgraph "User Interface"
        CLI[CLI Channel]
        HTTP[HTTP Channel]
    end

    subgraph "Pekobot Runtime"
        AGENT[Agent]
        ORCH[Orchestrator]
        A2A[A2A Protocol]
        REG[Registry]
    end

    subgraph "Core Services"
        ID[Identity System]
        MEM[SQLite Memory]
        TOOLS[Tools]
    end

    subgraph "External"
        LLM[LLM Provider
        OpenAI/etc]
        CONEKO[Coneko Network
        Optional]
    end

    CLI --> AGENT
    HTTP --> AGENT
    AGENT --> ORCH
    ORCH --> A2A
    A2A --> REG
    AGENT --> ID
    AGENT --> MEM
    AGENT --> TOOLS
    AGENT --> LLM
    A2A --> CONEKO
```

---

## Component Overview

| Component | Purpose | Key Files |
|-----------|---------|-----------|
| **Agent** | Single agent runtime with lifecycle | `src/agent/mod.rs` |
| **Orchestrator** | Multi-agent coordination | `src/agent/mod.rs` |
| **A2A Protocol** | Agent-to-agent messaging | `src/a2a/` |
| **Identity** | DID and key management | `src/identity/` |
| **Memory** | SQLite persistence | `src/memory/` |
| **Providers** | LLM integrations | `src/providers/` |
| **Tools** | Agent capabilities | `src/tools/` |
| **Channels** | User interfaces | `src/channels/` |
| **Coneko** | Network adapter | `src/coneko/` |

---

## Data Flow

### Single Agent Execution

```mermaid
sequenceDiagram
    participant User
    participant CLI as CLI Channel
    participant Agent
    participant Memory as SQLite Memory
    participant Provider as LLM Provider

    User->>CLI: Type message
    CLI->>Agent: receive()
    Agent->>Agent: set_state(Busy)
    
    alt Memory Enabled
        Agent->>Memory: store(prompt)
    end
    
    Agent->>Provider: complete(prompt)
    Provider-->>Agent: response
    
    alt Memory Enabled
        Agent->>Memory: store(response)
    end
    
    Agent->>Agent: set_state(Idle)
    Agent-->>CLI: response
    CLI-->>User: Display response
```

### Multi-Agent Orchestration

```mermaid
sequenceDiagram
    participant User
    participant Orchestrator
    participant Registry
    participant Agent1 as Agent 1
    participant Agent2 as Agent 2
    participant A2A as A2A Protocol

    User->>Orchestrator: Create orchestrator
    Orchestrator->>Registry: Create registry
    
    User->>Orchestrator: add_agent(Agent1)
    Orchestrator->>Registry: register(Agent1)
    
    User->>Orchestrator: add_agent(Agent2)
    Orchestrator->>Registry: register(Agent2)
    
    User->>Orchestrator: start_all()
    Orchestrator->>Registry: start_all()
    Registry->>Agent1: start()
    Registry->>Agent2: start()
    
    User->>Orchestrator: Send message
    Orchestrator->>A2A: Route message
    A2A->>Agent1: handle_message()
    Agent1->>A2A: Response
    A2A->>Orchestrator: Forward response
```

---

## Module Details

### Agent Module

```mermaid
classDiagram
    class Agent {
        +AgentConfig config
        +Identity identity
        -AgentState state
        -Option~SqliteMemory~ memory
        -Option~Box~Provider~~ provider
        +new(config) Result~Self~
        +start() Result
        +stop() Result
        +execute(prompt) Result~String~
        +store_memory(content, metadata) Result~String~
        +search_memory(query, limit) Result~Vec~MemoryEntry~~
        +did() ~str
        +name() ~str
        +state() AgentState
    }

    class Orchestrator {
        -Option~A2AProtocol~ protocol
        -Option~SharedRegistry~ registry
        +new() Self
        +with_registry(registry) Self
        +add_agent(agent) Result
        +start_all() Result
        +stop_all() Result
        +list_agents() Vec~String, String~
        +find_by_did(did) Option~ArcAgent~
        +find_by_name(name) Option~ArcAgent~
    }

    class AgentState {
        <<enumeration>>
        Initializing
        Idle
        Busy
        Paused
        Error
        ShuttingDown
    }

    Agent --> AgentState : manages
    Orchestrator --> Agent : coordinates
```

### Identity System

```mermaid
graph LR
    subgraph "Identity Module"
        DID[DID Parser
        did.rs]
        KEYS[Key Manager
        keys.rs]
        STORAGE[Key Storage
        storage.rs]
        RESOLVER[DID Resolver
        resolver.rs]
    end

    DID --> KEYS
    KEYS --> STORAGE
    RESOLVER --> DID
    RESOLVER --> STORAGE
```

**DID Format:**

```
did:pekobot:{scope}:{tenant}:{identifier}

Examples:
- did:pekobot:local:default:abc123...
- did:pekobot:tenant:acme:def456...
- did:pekobot:global::ghi789...
```

### Memory System

```mermaid
graph TB
    subgraph "Memory Module"
        SQL[SQLite
        sqlite.rs]
        TYPES[Memory Types
        types.rs]
    end

    subgraph "Database Schema"
        TABLE[memories table]
        TABLE -->|has columns| COLUMNS[id, namespace, content, metadata, timestamp]
    end

    subgraph "Features"
        FTS[Full-Text Search]
        NS[Namespace Isolation]
        META[JSON Metadata]
    end

    SQL --> TABLE
    SQL --> FTS
    SQL --> NS
    SQL --> META
```

---

## Agent Lifecycle

```mermaid
stateDiagram-v2
    [*] --> Initializing: Agent::new()
    
    Initializing --> Idle: start()
    Initializing --> Error: init_failure
    
    Idle --> Busy: execute()
    Idle --> Paused: pause()
    Idle --> ShuttingDown: stop()
    
    Busy --> Idle: task_complete
    Busy --> Error: task_failure
    Busy --> ShuttingDown: stop()
    
    Paused --> Idle: resume()
    Paused --> ShuttingDown: stop()
    
    Error --> Idle: recover()
    Error --> ShuttingDown: stop()
    
    ShuttingDown --> [*]: cleanup_complete
```

### State Transitions

| From | To | Trigger | Description |
|------|-----|---------|-------------|
| `Initializing` | `Idle` | `start()` | Agent ready for tasks |
| `Idle` | `Busy` | `execute()` | Processing a prompt |
| `Busy` | `Idle` | Task complete | Ready for next task |
| `Idle` | `ShuttingDown` | `stop()` | Graceful shutdown |
| `Busy` | `ShuttingDown` | `stop()` | Emergency shutdown |

---

## A2A Protocol

The Agent-to-Agent (A2A) protocol enables standardized communication between agents.

### Message Types

```mermaid
graph TB
    subgraph "A2A Messages"
        INTENT[Intent
        Initiate action]
        QUOTE[Quote
        Pricing/estimate]
        ACCEPT[Accept
        Accept quote]
        REJECT[Reject
        Reject quote]
        CONTRACT[Contract
        Formal agreement]
        TASK[Task
        Execute work]
        UPDATE[Update
        Progress update]
        COMPLETE[Complete
        Task done]
        CANCEL[Cancel
        Cancel workflow]
        ESCALATE[Escalate
        Human help]
        QUERY[Query
        Information request]
        RESPONSE[Response
        Query answer]
    end

    subgraph "Workflows"
        NEG[Negotiation]
        EXEC[Execution]
        QUERY_FLOW[Query/Response]
    end

    INTENT --> QUOTE --> ACCEPT --> CONTRACT --> TASK --> UPDATE --> COMPLETE
    QUOTE --> REJECT
    CONTRACT --> CANCEL
    TASK --> ESCALATE
    QUERY --> RESPONSE
```

### Registry Architecture

```mermaid
graph TB
    subgraph "Registry"
        SHARED[SharedRegistry
        Arc~Mutex~Registry~]
        REG[Registry
        HashMap~DID, ArcAgent~]
        TX[Message Sender]
        RX[Message Receiver]
    end

    subgraph "Agents"
        A1[Agent 1]
        A2[Agent 2]
        A3[Agent 3]
    end

    SHARED --> REG
    REG --> A1
    REG --> A2
    REG --> A3
    TX -->|messages| RX
```

---

## Memory System

### Database Schema

```mermaid
erDiagram
    MEMORY {
        string id PK
        string namespace
        text content
        text metadata JSON
        datetime timestamp
    }
```

### Memory Operations

```mermaid
sequenceDiagram
    participant Agent
    participant SqliteMemory
    participant SQLite

    Note over Agent,SQLite: Store Operation
    Agent->>SqliteMemory: store(content, metadata)
    SqliteMemory->>SQLite: INSERT INTO memories
    SQLite-->>SqliteMemory: row_id
    SqliteMemory-->>Agent: id

    Note over Agent,SQLite: Search Operation
    Agent->>SqliteMemory: search(query, limit)
    SqliteMemory->>SQLite: SELECT with MATCH
    SQLite-->>SqliteMemory: results
    SqliteMemory-->>Agent: Vec~MemoryEntry~
```

---

## Coneko Integration

```mermaid
graph TB
    subgraph "Pekobot"
        ADAPTER[ConekoAdapter]
        CLIENT[ConekoClient]
        UNIFIED[UnifiedRegistry
        Local + Coneko]
    end

    subgraph "Coneko Network"
        SERVER[Coneko Server]
        DISCO[Discovery Service]
        MSG[Message Router]
    end

    subgraph "Remote Agents"
        RA1[Remote Agent 1]
        RA2[Remote Agent 2]
    end

    ADAPTER --> CLIENT
    CLIENT -->|HTTP| SERVER
    UNIFIED --> ADAPTER
    SERVER --> DISCO
    SERVER --> MSG
    MSG -->|WebSocket/HTTP| RA1
    MSG -->|WebSocket/HTTP| RA2
```

---

## Project Structure

```
pekobot/
├── src/
│   ├── main.rs           # CLI entry point
│   ├── lib.rs            # Library exports
│   ├── agent/            # Agent runtime & orchestrator
│   │   └── mod.rs
│   ├── a2a/              # A2A protocol
│   │   ├── mod.rs
│   │   ├── message.rs    # Message types
│   │   ├── protocol.rs   # Protocol handler
│   │   ├── registry.rs   # Agent registry
│   │   └── flows.rs      # Workflow definitions
│   ├── channels/         # User interfaces
│   │   ├── mod.rs
│   │   ├── cli.rs        # Terminal interface
│   │   └── http.rs       # HTTP webhook
│   ├── coneko/           # Coneko network
│   │   ├── mod.rs
│   │   ├── client.rs     # HTTP client
│   │   └── registry.rs   # Unified registry
│   ├── config/           # Configuration
│   │   └── mod.rs
│   ├── identity/         # DID & keys
│   │   ├── mod.rs
│   │   ├── did.rs        # DID parsing
│   │   ├── keys.rs       # Key management
│   │   ├── storage.rs    # Key storage
│   │   └── resolver.rs   # DID resolution
│   ├── memory/           # SQLite persistence
│   │   ├── mod.rs
│   │   ├── sqlite.rs     # Implementation
│   │   └── types.rs      # Memory types
│   ├── providers/        # LLM integrations
│   │   ├── mod.rs
│   │   ├── traits.rs     # Provider trait
│   │   └── openai.rs     # OpenAI provider
│   ├── tools/            # Agent tools
│   │   ├── mod.rs
│   │   ├── traits.rs     # Tool trait
│   │   ├── http.rs       # HTTP tool
│   │   └── memory_tool.rs # Memory tool
│   └── types/            # Core types
│       ├── mod.rs
│       ├── agent.rs      # Agent config/state
│       ├── config.rs     # General config
│       ├── memory.rs     # Memory types
│       ├── provider.rs   # Provider types
│       └── task.rs       # Task types
├── examples/             # Example code
├── tests/                # Integration tests
└── docs/                 # Documentation
```

---

## Configuration Flow

```mermaid
graph LR
    CLI[CLI Args]
    ENV[Environment]
    FILE[Config File]
    DEFAULT[Defaults]
    CONFIG[AgentConfig]

    CLI -->|highest priority| CONFIG
    ENV --> CONFIG
    FILE --> CONFIG
    DEFAULT -->|lowest priority| CONFIG
```

---

## Security Considerations

### Identity

- ed25519 keys for cryptographic identity
- DID-based addressing
- Keys stored in platform-specific secure storage

### Network

- HTTPS required for production
- Authentication tokens for Coneko
- Request timeouts configured

### Memory

- Namespace isolation per agent DID
- SQLite with bundled library
- No sensitive data in logs

---

*For implementation details, see the source code and [API Documentation](API.md)*
