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

### CalendarTool

Integrates with Google Calendar and Outlook for scheduling.

```rust
use pekobot::tools::calendar::{CalendarCredentials, CalendarProvider, CalendarTool};

let credentials = CalendarCredentials {
    client_id: "...".to_string(),
    client_secret: "...".to_string(),
    access_token: "...".to_string(),
    refresh_token: Some("...".to_string()),
    token_expires_at: None,
};

let tool = CalendarTool::new(CalendarProvider::Google, credentials);

// List events
let events = tool.execute(json!({
    "command": "list_events",
    "start": "2026-02-17T09:00:00Z",
    "end": "2026-02-17T17:00:00Z"
})).await?;

// Find available slots
let slots = tool.execute(json!({
    "command": "find_slots",
    "start": "2026-02-17T09:00:00Z",
    "end": "2026-02-17T17:00:00Z",
    "duration_minutes": 60
})).await?;

// Create event
let event = tool.execute(json!({
    "command": "create_event",
    "title": "Team Meeting",
    "start": "2026-02-17T14:00:00Z",
    "end": "2026-02-17T15:00:00Z",
    "attendees": ["colleague@example.com"]
})).await?;
```

**Environment Variables:**
```bash
CALENDAR_PROVIDER=google  # or outlook
CALENDAR_CLIENT_ID=...
CALENDAR_CLIENT_SECRET=...
CALENDAR_ACCESS_TOKEN=...
CALENDAR_REFRESH_TOKEN=...
```

---

### DocumentTool

Process PDFs, images, and extract structured data from documents.

```rust
use pekobot::tools::document::DocumentTool;

let tool = DocumentTool::new();

// Extract text from PDF
let result = tool.execute(json!({
    "command": "extract_text",
    "file_path": "/path/to/document.pdf"
})).await?;

// OCR on scanned image
let result = tool.execute(json!({
    "command": "ocr",
    "image_path": "/path/to/scanned_receipt.png"
})).await?;

// Parse invoice
let result = tool.execute(json!({
    "command": "parse_invoice",
    "text": "Invoice #12345..."
})).await?;
```

**Prerequisites:**
```bash
# Ubuntu/Debian
sudo apt-get install poppler-utils tesseract-ocr tesseract-ocr-eng

# macOS
brew install poppler tesseract
```

---

### SocialMediaTool

Post and schedule content on Twitter/X and LinkedIn.

```rust
use pekobot::tools::social_media::SocialMediaTool;

let tool = SocialMediaTool::from_env()?;

// Draft a post
let result = tool.execute(json!({
    "command": "draft_post",
    "platform": "twitter",
    "content": "Excited to announce our new product!"
})).await?;

// Schedule for later
let result = tool.execute(json!({
    "command": "schedule_post",
    "post_id": "post_abc123",
    "scheduled_at": "2026-02-20T14:00:00Z"
})).await?;

// Publish immediately
let result = tool.execute(json!({
    "command": "publish",
    "post_id": "post_abc123"
})).await?;

// Get analytics
let result = tool.execute(json!({
    "command": "get_analytics",
    "post_id": "post_abc123"
})).await?;
```

**Environment Variables:**
```bash
# Twitter/X API
TWITTER_API_KEY=...
TWITTER_API_SECRET=...
TWITTER_ACCESS_TOKEN=...
TWITTER_ACCESS_SECRET=...

# LinkedIn API
LINKEDIN_CLIENT_ID=...
LINKEDIN_CLIENT_SECRET=...
LINKEDIN_ACCESS_TOKEN=...
```

---

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

## Channels

Channels provide different ways for users to interact with agents.

### CLI Channel

Interactive terminal interface (default).

```rust
use pekobot::channels::cli::{CliChannel, run_interactive_loop};

let mut channel = CliChannel::new();
run_interactive_loop(agent, &mut channel).await?;
```

### HTTP Channel

Webhook-based communication for external integrations.

```rust
use pekobot::channels::http::HttpChannel;

let channel = HttpChannel::new(
    "0.0.0.0:8080",
    "/webhook",
)?;
channel.start(agent).await?;
```

### WhatsApp Channel

WhatsApp Business API integration for customer service bots.

**Environment Variables:**
```bash
WHATSAPP_ACCESS_TOKEN=your_token
WHATSAPP_PHONE_NUMBER_ID=your_phone_id
WHATSAPP_VERIFY_TOKEN=your_verify_token
```

**Example:**
```rust
use pekobot::channels::whatsapp::WhatsAppChannel;

let mut channel = WhatsAppChannel::from_env()?;

// Send message
channel.send_to_number("+1234567890", "Hello!").await?;

// Parse incoming webhook
let messages = channel.parse_webhook_payload(&payload);
```

See `examples/whatsapp_customer_service.rs` for a complete customer service bot.

### Telegram Channel

Telegram Bot API integration.

```rust
use pekobot::channels::telegram::TelegramChannel;

let token = std::env::var("TELEGRAM_BOT_TOKEN")?;
let mut channel = TelegramChannel::new(token);
channel.start(agent).await?;
```

### Discord Channel

Discord bot integration with user allowlisting.

```rust
use pekobot::channels::discord::DiscordChannel;

let config = DiscordConfig {
    bot_token: std::env::var("DISCORD_BOT_TOKEN")?,
    allowed_users: vec!["user1#1234".to_string()],
};
let mut channel = DiscordChannel::new(config);
channel.start(agent).await?;
```

### Slack Channel

Slack workspace integration.

```rust
use pekobot::channels::slack::SlackChannel;

let config = SlackConfig {
    bot_token: std::env::var("SLACK_BOT_TOKEN")?,
    app_token: std::env::var("SLACK_APP_TOKEN")?,
};
let mut channel = SlackChannel::new(config)?;
channel.start(agent).await?;
```

### Matrix Channel

Matrix protocol (Element, etc.) support.

```rust
use pekobot::channels::matrix::MatrixChannel;

let config = MatrixConfig {
    homeserver: "https://matrix.org".to_string(),
    user_id: "@bot:matrix.org".to_string(),
    access_token: std::env::var("MATRIX_ACCESS_TOKEN")?,
};
let mut channel = MatrixChannel::new(config);
channel.start(agent).await?;
```

---

## Feature Flags

| Flag | Description |
|------|-------------|
| `default` | Core functionality |
| `coneko` | Coneko network integration |
| `openai` | OpenAI provider |
| `all-channels` | All communication channels |
| `whatsapp` | WhatsApp Business API |
| `telegram` | Telegram Bot API |
| `discord` | Discord integration |
| `slack` | Slack integration |
| `matrix` | Matrix protocol |

---

## Version

Current version: **0.1.0**
