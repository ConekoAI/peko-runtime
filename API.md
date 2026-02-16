# Pekobot API Documentation

## Core Types

### Agent

The main agent struct that manages identity, memory, and execution.

```rust
pub struct Agent {
    pub config: AgentConfig,
    state: Arc<RwLock<AgentState>>,
    pub identity: Identity,
    memory: Option<SqliteMemory>,
    provider: Option<Box<dyn Provider>>,
}
```

#### Methods

**`Agent::new(config: AgentConfig) -> Result<Self>`**

Creates a new agent with the given configuration. Loads or creates identity, initializes memory and provider.

**`agent.start().await -> Result<()>`**

Starts the agent, transitioning to Idle state.

**`agent.stop().await -> Result<()>`**

Stops the agent, transitioning to ShuttingDown state.

**`agent.execute(prompt: &str).await -> Result<String>`**

Executes a task with the configured LLM provider. Stores prompt and response in memory.

**`agent.store_memory(content: &str, metadata: Option<Value>) -> Result<String>`**

Stores content in agent memory with optional metadata.

**`agent.search_memory(query: &str, limit: usize) -> Result<Vec<MemoryEntry>>`**

Searches memory for entries matching the query.

**`agent.did() -> &str`**

Returns the agent's DID.

**`agent.name() -> &str`**

Returns the agent's name.

**`agent.state() -> AgentState`**

Returns the current agent state.

---

### Orchestrator

Manages multiple agents and A2A protocol communication.

```rust
pub struct Orchestrator {
    protocol: Option<A2AProtocol>,
    registry: Option<SharedRegistry>,
}
```

#### Methods

**`Orchestrator::new() -> Self`**

Creates an empty orchestrator without registry.

**`Orchestrator::with_registry(registry: SharedRegistry) -> Self`**

Creates an orchestrator with A2A registry support.

**`orchestrator.add_agent(agent: Agent).await -> Result<()>`**

Adds an agent to the orchestrator and registers it.

**`orchestrator.start_all().await -> Result<()>`**

Starts all registered agents.

**`orchestrator.stop_all().await -> Result<()>`**

Stops all registered agents.

**`orchestrator.list_agents().await -> Vec<(String, String)>`**

Returns list of (DID, name) for all agents.

**`orchestrator.find_by_did(did: &str).await -> Option<ArcAgent>`**

Finds agent by DID.

**`orchestrator.find_by_name(name: &str).await -> Option<ArcAgent>`**

Finds agent by name.

---

### Config

Builder for agent configuration.

```rust
let config = Config::agent("my-agent")
    .with_description("A helpful agent")
    .with_capabilities(vec!["task".to_string()])
    .with_memory(true)
    .build();
```

#### Methods

**`Config::agent(name: &str) -> ConfigBuilder`**

Starts building a configuration for an agent.

**`.with_description(desc: &str)`**

Sets agent description.

**`.with_capabilities(caps: Vec<String>)`**

Sets agent capabilities.

**`.with_memory(enabled: bool)`**

Enables/disables SQLite memory.

**`.with_coneko(config: ConekoConfig)`**

Sets Coneko network configuration.

**`.build() -> AgentConfig`**

Builds the final configuration.

---

## Identity

### Identity

```rust
pub struct Identity {
    pub did: String,
    pub scope: DIDScope,
    pub tenant: Option<String>,
    pub public_key: VerifyingKey,
    pub keypair: Option<Keypair>,
}
```

**`Identity::generate(scope, tenant, seed) -> Result<Self>`**

Generates a new identity with ed25519 keys.

**`Identity::from_keypair(keypair, scope, tenant) -> Self`**

Creates identity from existing keypair.

### DID

```rust
pub struct DID {
    pub method: String,       // "pekobot"
    pub scope: DIDScope,      // Local, Tenant, Global
    pub tenant: Option<String>,
    pub identifier: String,
}
```

**`DID::parse(did: &str) -> Result<Self>`**

Parses a DID string.

**`DID::generate(scope, tenant) -> Self`**

Generates a new random DID.

**Format:** `did:pekobot:{scope}:{tenant}:{identifier}`

---

## Memory

### SqliteMemory

```rust
pub struct SqliteMemory {
    conn: Connection,
    namespace: String,
}
```

**`SqliteMemory::new(path, namespace) -> Result<Self>`**

Creates/open SQLite database.

**`memory.store(content, metadata) -> Result<String>`**

Stores content, returns entry ID.

**`memory.search(query, limit) -> Result<Vec<MemoryEntry>>`**

Full-text search.

**`memory.get_by_id(id) -> Result<Option<MemoryEntry>>`**

Retrieves by ID.

---

## A2A Protocol

### A2AMessage

```rust
pub struct A2AMessage {
    pub message_id: String,
    pub thread_id: String,
    pub sender: AgentEndpoint,
    pub recipient: AgentEndpoint,
    pub message_type: A2AMessageType,
    pub timestamp: DateTime<Utc>,
    pub ttl_seconds: Option<u32>,
    pub signature: Option<String>,
}
```

### Message Types

- `Intent` — Initiate action request
- `Quote` — Response with pricing/estimate
- `Accept` — Accept a quote
- `Reject` — Reject a quote
- `Contract` — Formal agreement
- `Task` — Execute task
- `Update` — Progress update
- `Complete` — Task completion
- `Cancel` — Cancel workflow
- `Escalate` — Human escalation
- `Query` — Information request
- `Response` — Query response

---

## Providers

### Provider Trait

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &str;
    async fn complete(&self, prompt: &str) -> Result<String>;
    async fn chat(&self, messages: &[Message]) -> Result<String>;
}
```

### OpenAIProvider

```rust
let config = OpenAIConfig {
    api_key: "sk-...".to_string(),
    base_url: "https://api.openai.com/v1".to_string(),
    model: "gpt-4".to_string(),
    max_tokens: Some(2000),
    temperature: Some(0.7),
    timeout_seconds: Some(30),
};

let provider = OpenAIProvider::new(config)?;
```

---

## Coneko Integration

### ConekoClient

```rust
let client = ConekoClient::new(
    "http://localhost:8080",
    Some("auth-token"),
)?;

// Health check
client.health_check().await?;

// Register agent
client.register_agent(
    did,
    name,
    endpoint,
    capabilities,
    tenant,
    metadata,
).await?;

// Discover agents
let agents = client.discover_agents(
    Some("messaging"),
    Some("acme"),
).await?;

// Send message
client.send_message(
    sender_did,
    recipient_did,
    message_type,
    payload,
).await?;

// Poll messages
let messages = client.poll_messages(did).await?;
```

### ConekoAdapter

```rust
let adapter = ConekoAdapter::new(config)?;
adapter.start().await?;  // Starts background polling
adapter.stop().await?;
```

---

## Tools

### HttpTool

```rust
let http = HttpTool::new();

// GET request
let response = http.get("https://api.example.com/data").await?;

// GET with headers
let response = http.get_with_headers(url, vec![
    ("Authorization".to_string(), "Bearer token".to_string()),
]).await?;

// POST JSON
let body = json!({"key": "value"});
let response = http.post_json(url, &body).await?;
```

---

## Error Handling

All operations return `anyhow::Result<T>` for ergonomic error handling:

```rust
use anyhow::Result;

async fn run_agent() -> Result<()> {
    let agent = Agent::new(config).await?;
    agent.start().await?;
    let result = agent.execute("Hello").await?;
    println!("{}", result);
    Ok(())
}
```

---

## Feature Flags

| Flag | Description |
|------|-------------|
| `default` | Core functionality |
| `coneko` | Coneko network integration |
| `openai` | OpenAI provider |

---

## Version

Current version: **0.1.0**
