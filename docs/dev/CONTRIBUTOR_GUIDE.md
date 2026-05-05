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
├── agent/            # Agent runtime, lifecycle, registry, subagent execution
│   ├── agent.rs      # Core agent struct
│   ├── lifecycle.rs  # Agent lifecycle management
│   └── registry.rs   # Agent registry
│
├── commands/         # CLI command handlers
│   ├── agent.rs      # Agent management commands
│   ├── daemon.rs     # Daemon control
│   ├── send.rs       # Send message command
│   ├── session.rs    # Session commands
│   ├── ext.rs        # Extension management
│   └── ...
│
├── common/           # Shared utilities, registry, services, time, identifiers
│
├── compaction/       # Session compaction
│
├── cron/             # Scheduling system
│
├── daemon/           # Daemon process management
│
├── engine/           # Agentic loop, event processing, streaming, state machine
│   ├── loop_v4.rs    # Main execution loop
│   ├── input.rs      # Input types
│   ├── events.rs     # Event types and routing
│   └── execution.rs  # Tool execution
│
├── extensions/       # Unified Extension Architecture
│   ├── core/         # Core registry, types, tool registration
│   ├── adapters/     # Builtin tool, MCP, skill adapters
│   └── services/     # Extension services
│
├── identity/         # DID identity, keys, resolver, storage
│
├── image/            # Agent image building
│
├── ipc/              # Inter-process communication
│
├── mcp/              # Model Context Protocol support
│   ├── client.rs     # MCP protocol client
│   ├── manager.rs    # MCP server management
│   └── transport.rs  # Transport implementations
│
├── observability/    # Logging, metrics
│
├── portable/         # Portable agent packages
│
├── prompt/           # Prompt construction
│
├── providers/        # LLM provider integrations
│   ├── anthropic.rs  # Claude
│   ├── openai.rs     # GPT models
│   ├── ollama.rs     # Local models
│   ├── kimi.rs       # Moonshot AI
│   └── traits.rs     # Provider trait
│
├── registry/         # Tool registry
│
├── runtime/          # Shared runtime components
│
├── session/          # Session storage, overlays, JSONL
│   ├── jsonl.rs      # JSONL storage
│   ├── manager.rs    # Session lifecycle
│   └── overlay.rs    # Session overlays
│
├── team/             # Multi-agent team runtime
│
├── tools/            # Tool framework
│   ├── builtin/      # Built-in tools
│   └── framework/    # Tool framework (async executor, traits)
│
├── types/            # Core type definitions
│   └── config.rs     # Configuration types
│
├── lib.rs            # Library entry point
├── main.rs           # CLI entry point
└── watcher.rs        # File watcher
```

---

## Key Concepts

### 1. Agent vs Agent Directory

**Agent (created with `agent create`)** = Configuration stored in Pekobot's config directory
- Managed by Pekobot
- No separate directory needed

**Agent Directory (created with `agent init`)** = Self-contained agent project
- Can be version controlled
- Contains config.toml, AGENT.md, tools/, workspace/

### 2. The Agentic Loop

The core execution flow:

```
1. Receive input (user message via CLI)
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
{data_dir}/agents/{agent}/sessions/{session}.jsonl
{data_dir}/agents/{agent}/sessions/{session}.index.json
```

Each line is a JSON event:
```json
{"id":"evt_1","type":"user_message","session_id":"sess_abc","ts":"...","seq":1,"data":{}}
```

**Key files:**
- `src/session/jsonl.rs` — Atomic writes
- `src/session/manager.rs` — Session lifecycle

### 4. Tool System

Tools have three sources (in order of precedence):

1. **Built-in** — `src/tools/builtin/*.rs`
2. **Local** — `tools/` directory in agent
3. **MCP** — External MCP servers (via extensions)

**Adding a built-in tool:**
```rust
// src/tools/builtin/my_tool.rs
use crate::tools::framework::traits::{Tool, ToolContext, ToolResult};
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

### 5. Extensions Architecture

The Unified Extension Architecture (`src/extensions/`) provides a single system for skills, MCP, tools, channels, and hooks.

Key components:
- **Registry** (`src/extensions/core/registry.rs`) — Extension registration
- **Adapters** (`src/extensions/adapters/`) — Bridge extensions to the runtime
- **Services** (`src/extensions/services/`) — Shared extension services

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

### Adding a New Tool

1. **Create tool file:**
```rust
// src/tools/builtin/my_tool.rs
use crate::tools::framework::traits::{Tool, ToolContext, ToolResult};
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
// src/tools/builtin/mod.rs or appropriate registry
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
cargo test --lib tools::builtin -- --nocapture
```

### Integration Tests

```bash
# Start daemon first
cargo run -- daemon start --foreground

# Run integration tests
cargo test --test integration
```

### Test Organization

```
tests/
├── integration/          # Integration tests
│   └── cli_tests.rs
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

### Debug an Agent

```bash
# Check session history
pekobot session show my-agent <session-id>

# Enable verbose output
pekobot send my-agent "Hello" -vv
```

---

## Common Tasks

### Reset Development Environment

```bash
# Stop daemon
pekobot daemon stop

# Clear all data
rm -rf ~/.local/share/pekobot
rm -rf ~/.config/pekobot

# Start fresh
pekobot daemon start --foreground
```

### Test Against Different Providers

```bash
# Create agent with specific provider
pekobot agent create test-agent --provider minimax
export ANTHROPIC_API_KEY="..."
pekobot send test-agent "Hello"
```

---

## Resources

- [Architecture Overview](./ARCHITECTURE.md) — High-level design
- [Error Codes](../reference/ERROR_CODES.md) — Error reference

---

## Getting Help

- **GitHub Issues:** Bug reports and feature requests
- **Discussions:** Questions and ideas

---

Happy contributing! 🐱
