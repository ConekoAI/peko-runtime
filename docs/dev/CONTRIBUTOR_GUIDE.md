# Pekobot Contributor Guide

Guide for developers contributing to Pekobot.

---

## Table of Contents

1. [Development Setup](#development-setup)
2. [Project Structure](#project-structure)
3. [Key Concepts](#key-concepts)
4. [Adding Features](#adding-features)
5. [Testing](#testing)
6. [Code Style](#code-style)

---

## Development Setup

### Prerequisites

- Rust 1.70+ with Cargo
- Git
- (Optional) SQLite for memory features

### Build

```bash
# Clone
git clone https://github.com/coneko/pekobot
cd pekobot

# Build debug (fast compile)
cargo build

# Build release (optimized)
cargo build --release

# Run tests
cargo test --lib

# Run with logging
RUST_LOG=debug cargo run -- daemon start --foreground
```

---

## Project Structure

```
src/
├── api/              # HTTP API server (Axum)
│   ├── client.rs     # HTTP client for CLI
│   ├── error.rs      # API error types
│   ├── middleware/   # Auth, logging, headers
│   ├── routes/       # Endpoint handlers
│   │   ├── agents.rs     # Instance CRUD
│   │   ├── chat.rs       # Chat streaming
│   │   ├── images.rs     # Image build/pull/push
│   │   ├── metrics.rs    # Performance metrics
│   │   ├── sessions.rs   # Session management
│   │   └── teams.rs      # Team management
│   ├── server.rs     # Axum server setup
│   ├── state.rs      # Shared app state
│   ├── streaming.rs  # SSE streaming utilities
│   ├── types.rs      # API request/response types
│   └── websocket.rs  # WebSocket handlers
│
├── agent/            # Agent runtime
│   ├── agent.rs      # Core agent struct
│   ├── context.rs    # Agent execution context
│   ├── lifecycle.rs  # Start/stop/upgrade
│   ├── manager.rs    # Instance management
│   ├── pool.rs       # Multi-agent pool
│   └── registry.rs   # Instance registry
│
├── commands/         # CLI commands
│   ├── agent.rs      # Agent management
│   ├── daemon.rs     # Daemon control
│   ├── session.rs    # Session commands
│   ├── tool.rs       # Tool management
│   └── ...
│
├── engine/           # Agentic loop
│   ├── loop_v4.rs    # Main execution loop
│   ├── input.rs      # Input types (user, hook, A2A)
│   ├── execution.rs  # Tool execution
│   ├── events.rs     # Event types
│   ├── runner.rs     # Turn runner
│   ├── state.rs      # Loop state machine
│   └── task_manager.rs # Sync/async task handling
│
├── session/          # Session management
│   ├── jsonl.rs      # JSONL storage
│   ├── index.rs      # Sidecar indexes
│   ├── manager.rs    # Session lifecycle
│   ├── types.rs      # Session types
│   └── recovery.rs   # Crash recovery
│
├── tools/            # Built-in tools
│   ├── filesystem.rs # File operations
│   ├── process.rs    # Process execution
│   ├── agent_spawn.rs # Subagent spawning
│   ├── cron_tool.rs  # Cron scheduling
│   └── ...
│
├── team/             # Team runtime
│   ├── mod.rs        # Team management
│   ├── bus/          # Event bus backends
│   │   ├── memory.rs # In-memory backend
│   │   └── mod.rs    # Bus trait
│   ├── config.rs     # Team config parsing
│   └── shared.rs     # Shared services
│
├── image/            # Image management
│   ├── builder.rs    # Image building
│   ├── config.rs     # config.toml parsing
│   ├── manifest.rs   # Image manifest
│   └── registry.rs   # Local registry storage
│
├── registry/         # Remote registry client
│   ├── client.rs     # Push/pull client
│   └── config.rs     # Registry auth
│
├── mcp/              # MCP client
│   ├── client.rs     # MCP protocol client
│   ├── config.rs     # mcp.json parsing
│   ├── discovery.rs  # Tool discovery
│   └── tool_proxy.rs # Tool call proxying
│
├── providers/        # LLM providers
│   ├── anthropic.rs  # Claude
│   ├── openai.rs     # GPT models
│   ├── ollama.rs     # Local models
│   ├── kimi.rs       # Moonshot AI
│   └── traits.rs     # Provider trait
│
├── hooks/            # Hook system
│   ├── event_bus.rs  # A2A messaging
│   ├── file_watch.rs # File watcher
│   ├── registry.rs   # Hook registration
│   └── trigger.rs    # Hook activation
│
├── observability/    # Monitoring
│   ├── audit.rs      # Audit logging
│   ├── metrics.rs    # General metrics
│   └── performance.rs # Performance tracking
│
└── main.rs           # CLI entry point
```

---

## Key Concepts

### 1. Image vs Instance

**Image** = Blueprint (immutable)
- Built from agent directory
- Has SHA-256 digest
- Stored in `.pekobot/registry/images/`

**Instance** = Running agent
- Created from an image
- Has state (running, stopped, etc.)
- Owns sessions and workspace

```rust
// Build image
let image = ImageBuilder::new("./my-agent/").build()?;

// Create instance
let instance = Instance::create(&image, "my-instance").await?;
```

### 2. The Agentic Loop

The core execution flow:

```
1. Receive input (user message / hook trigger / A2A message)
2. Build context (system prompt + history + tools)
3. Call LLM provider
4. Parse response (content vs tool calls)
5. Execute tools (sync or async)
6. Loop until final response
7. Persist to session
```

**Key files:**
- `src/engine/loop_v4.rs` — Main loop
- `src/engine/input.rs` — Input enum
- `src/engine/execution.rs` — Tool execution

### 3. Session Storage

Sessions are stored as JSONL files:

```
.pckobot/agents/{instance}/sessions/{session}.jsonl
.pckobot/agents/{instance}/sessions/{session}.index.json
```

Each line is a JSON event:
```json
{"id":"evt_1","type":"user_message","session_id":"sess_abc","ts":"...","seq":1,"data":{}}
```

**Key files:**
- `src/session/jsonl.rs` — Atomic writes
- `src/session/index.rs` — Sidecar indexes

### 4. Tool System

Tools have three sources (in order of precedence):

1. **Built-in** — `src/tools/*.rs`
2. **Local** — `tools/` directory in agent
3. **MCP** — External MCP servers

**Adding a built-in tool:**
```rust
// src/tools/my_tool.rs
use crate::tools::traits::{Tool, ToolContext};

pub struct MyTool;

#[async_trait]
impl Tool for MyTool {
    fn name(&self) -> &str { "my_tool" }
    
    async fn execute(&self, ctx: &ToolContext, params: Value) -> Result<Value> {
        // Implementation
    }
}
```

### 5. Event Bus (A2A)

Agents communicate via the event bus:

```rust
// Send message
bus.send(A2AMessage::Direct {
    target: "agent-2",
    payload: json!({"task": "analyze"}),
}).await?;

// Receive message
bus.subscribe("agent-1").await?.for_each(|msg| {
    // Handle message
});
```

---

## Adding Features

### Adding a New CLI Command

1. **Add to commands module:**
```rust
// src/commands/myfeature.rs
use clap::Subcommand;

#[derive(Subcommand)]
pub enum MyFeatureCommands {
    /// Do something
    DoSomething { name: String },
}

pub async fn handle_myfeature(cmd: MyFeatureCommands, paths: &GlobalPaths) -> Result<()> {
    match cmd {
        MyFeatureCommands::DoSomething { name } => {
            println!("Doing something with {}", name);
            Ok(())
        }
    }
}
```

2. **Register in mod.rs:**
```rust
// src/commands/mod.rs
pub mod myfeature;

pub enum Commands {
    // ...
    MyFeature(MyFeatureCommands),
}

// In run_command()
Commands::MyFeature(cmd) => myfeature::handle_myfeature(cmd, paths).await,
```

### Adding a New API Endpoint

1. **Add route handler:**
```rust
// src/api/routes/myfeature.rs
use axum::{extract::State, Json};
use crate::api::state::AppState;

pub async fn my_endpoint(
    State(state): State<AppState>,
) -> Json<MyResponse> {
    Json(MyResponse { data: vec![] })
}
```

2. **Register in router:**
```rust
// src/api/routes/mod.rs
pub mod myfeature;

// In routes() function
.route("/myfeature", get(myfeature::my_endpoint))
```

### Adding a New Tool

1. **Create tool file:**
```rust
// src/tools/my_tool.rs
use crate::tools::traits::{Tool, ToolContext, ToolResult};
use serde_json::Value;

pub struct MyTool;

#[async_trait::async_trait]
impl Tool for MyTool {
    fn name(&self) -> &str { "my_tool" }
    
    fn description(&self) -> &str { "Does something useful" }
    
    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "input": {"type": "string"}
            },
            "required": ["input"]
        })
    }
    
    async fn execute(&self, ctx: &ToolContext, params: Value) -> ToolResult {
        let input = params["input"].as_str().unwrap_or_default();
        // Do work
        Ok(json!({"result": "success"}))
    }
}
```

2. **Register in factory:**
```rust
// src/tools/factory.rs
pub fn create_builtin_tools() -> Vec<Box<dyn Tool>> {
    vec![
        // ... existing tools
        Box::new(MyTool),
    ]
}
```

3. **Add tests:**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_my_tool() {
        let tool = MyTool;
        let ctx = ToolContext::default();
        let result = tool.execute(&ctx, json!({"input": "test"})).await;
        assert!(result.is_ok());
    }
}
```

### Adding a New LLM Provider

1. **Implement provider trait:**
```rust
// src/providers/my_provider.rs
use crate::providers::traits::{Provider, CompletionRequest, CompletionResponse};

pub struct MyProvider {
    api_key: String,
    base_url: String,
}

#[async_trait::async_trait]
impl Provider for MyProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse> {
        // Call LLM API
    }
    
    fn supports_streaming(&self) -> bool { true }
    
    fn supports_tools(&self) -> bool { true }
}
```

2. **Register in registry:**
```rust
// src/providers/mod.rs
pub mod my_provider;

// In provider factory
"my_provider" => Box::new(MyProvider::new(config)?),
```

---

## Testing

### Unit Tests

```bash
# Run all unit tests
cargo test --lib

# Run specific module
cargo test --lib session::

# Run with output
cargo test --lib tools::filesystem -- --nocapture
```

### Integration Tests

```bash
# Start daemon first
cargo run -- daemon start

# Run integration tests
cargo test --test integration

# Run use case tests
cargo test --test m12_use_case_tests -- --ignored
```

### Benchmarks

```bash
# Run performance benchmarks
cargo bench --bench m12_performance_benchmarks
```

### Test Organization

```
tests/
├── integration/          # Integration tests
│   ├── api_tests.rs
│   └── cli_tests.rs
├── m12_use_case_tests.rs # Use case tests
└── fixtures/             # Test data
```

---

## Code Style

### Formatting

```bash
# Format all code
cargo fmt

# Check formatting
cargo fmt -- --check
```

### Linting

```bash
# Run clippy
cargo clippy --lib

# Fix auto-fixable issues
cargo clippy --lib --fix
```

### Documentation

```bash
# Build docs
cargo doc --no-deps

# Build and open
cargo doc --no-deps --open
```

### Pre-commit Checklist

```bash
# Run before every commit
cargo fmt
cargo clippy --lib
cargo test --lib
cargo doc --no-deps
```

---

## Debugging

### Enable Debug Logging

```bash
# All logging
RUST_LOG=debug cargo run -- daemon start --foreground

# Specific module
RUST_LOG=pekobot::engine=trace cargo run -- ...

# With backtrace
RUST_BACKTRACE=1 cargo run -- ...
```

### Debug an Instance

```bash
# Attach to running instance
pekobot attach <instance-id>

# View logs
pekobot logs <instance-id> --follow

# Check session history
pekobot session show <instance-id> <session-id> --history
```

### API Debugging

```bash
# Verbose curl
curl -v http://localhost:11435/health

# With request ID for tracing
curl -H "X-Request-ID: debug-123" http://localhost:11435/agents
# Then grep logs: grep "debug-123" ~/.pekobot/logs/daemon.log
```

---

## Common Tasks

### Reset Development Environment

```bash
# Stop daemon
pekobot daemon stop

# Clear all data
rm -rf ~/.pekobot

# Start fresh
pekobot daemon start
```

### Test Against Different Providers

```bash
# Create agent with specific provider
pekobot agent init ./test-agent --provider anthropic
export ANTHROPIC_API_KEY="..."
pekobot run ./test-agent/
```

### Profile Performance

```bash
# Build with release
cargo build --release

# Run with timing
./target/release/pekobot daemon start --foreground &
time ./target/release/pekobot run my-agent:v1.0 --message "Hello"

# Get metrics
curl http://localhost:11435/metrics/performance | jq
```

---

## Resources

- [Architecture Overview](./ARCHITECTURE.md) — High-level design
- [API Contract](../../API_CONTRACT.md) — HTTP API spec
- [Error Codes](../reference/ERROR_CODES.md) — Error reference
- [API Examples](../api-examples.md) — Usage examples

---

## Getting Help

- **GitHub Issues:** Bug reports and feature requests
- **Discussions:** Questions and ideas
- **Discord:** Real-time chat (link in README)

---

Happy contributing! 🐱
