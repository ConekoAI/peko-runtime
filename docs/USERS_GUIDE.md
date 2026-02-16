# Pekobot User Guide

Welcome to the Pekobot User Guide! This guide will help you understand and use Pekobot effectively.

## Table of Contents

1. [What is Pekobot?](#what-is-pekobot)
2. [Installation](#installation)
3. [Core Concepts](#core-concepts)
4. [Running Your First Agent](#running-your-first-agent)
5. [Configuration](#configuration)
6. [Working with Memory](#working-with-memory)
7. [Multi-Agent Orchestration](#multi-agent-orchestration)
8. [Coneko Network Integration](#coneko-network-integration)
9. [Troubleshooting](#troubleshooting)

---

## What is Pekobot?

Pekobot 🐱 is a lightweight multi-agent runtime written in Rust. It allows you to:

- Run autonomous AI agents locally
- Connect agents to LLM providers (OpenAI, etc.)
- Enable persistent memory for your agents
- Orchestrate multiple agents working together
- Optionally connect to the Coneko network for cross-network agent discovery

### Key Features

| Feature | Description |
|---------|-------------|
| **Lightweight** | ~5-8MB binary, starts in <50ms |
| **Standalone** | Works without external services |
| **Multi-Agent** | Coordinate multiple agents with A2A protocol |
| **Persistent Memory** | SQLite-backed memory with search |
| **DID Identity** | ed25519-based decentralized identifiers |
| **Optional Network** | Connect to Coneko when needed |

---

## Installation

### Prerequisites

- Rust 1.70+ (install from [rustup.rs](https://rustup.rs))
- OpenAI API key (for LLM capabilities)

### Building from Source

```bash
# Clone the repository
git clone https://github.com/coneko/pekobot
cd pekobot

# Build in release mode
cargo build --release

# The binary is now available at:
./target/release/pekobot
```

### Verify Installation

```bash
./target/release/pekobot --version
# Output: pekobot 0.1.0

./target/release/pekobot --help
```

---

## Core Concepts

### Agent

An agent is the fundamental unit in Pekobot. It has:

- **Identity** — A unique DID (decentralized identifier)
- **State** — Current status (Initializing, Idle, Busy, etc.)
- **Memory** — SQLite database for persistence
- **Provider** — LLM integration (OpenAI, etc.)
- **Capabilities** — What the agent can do

### DID (Decentralized Identifier)

Every agent gets a unique identifier like:

```
did:pekobot:local:default:abc123def456
```

Format: `did:pekobot:{scope}:{tenant}:{identifier}`

- **Scope**: `local`, `tenant`, or `global`
- **Tenant**: Organization or namespace
- **Identifier**: Unique hash-based ID

### Agent State

Agents transition through states:

```
Initializing → Idle ↔ Busy → ShuttingDown
     ↓           ↓      ↓
   Error      Paused
```

| State | Description |
|-------|-------------|
| `Initializing` | Starting up |
| `Idle` | Ready to receive tasks |
| `Busy` | Processing a task |
| `Paused` | Temporarily stopped |
| `Error` | Encountered an error |
| `ShuttingDown` | Cleaning up |

---

## Running Your First Agent

### Basic Agent

```bash
# Set your OpenAI API key
export OPENAI_API_KEY="sk-..."

# Run a basic agent
./target/release/pekobot agent --name my-agent
```

You'll see output like:

```
🐱 Agent 'my-agent' started successfully!
   DID: did:pekobot:local:default:a1b2c3d4...
   State: Idle
   Coneko: ⚪ Disabled (use --coneko to enable)

╔════════════════════════════════════════╗
║     🐱 Pekobot Agent Interface         ║
╚════════════════════════════════════════╝
   Channel: my-agent

⚡ Agent 'my-agent' is ready! Type 'exit' or 'quit' to stop.

💬 You:
```

### Try Commands

```
💬 You: hello
🐱 Agent: Received: 'hello' (agent processing not yet implemented)

💬 You: help
🐱 Agent: Available commands:
  help - Show this message
  exit/quit/bye - Stop the agent

💬 You: exit
⚡ Goodbye! 👋
```

### With Memory Enabled

```bash
./target/release/pekobot agent --name memory-agent --db ~/.local/share/pekobot/memory.db
```

### With Coneko Network

```bash
./target/release/pekobot agent \
  --name networked-agent \
  --coneko http://localhost:8080 \
  --coneko-token your-token
```

---

## Configuration

### Using a Config File

Create `pekobot.toml`:

```toml
[agent]
name = "my-agent"
description = "A helpful assistant"
capabilities = ["messaging", "task_execution"]

[agent.memory]
enabled = true
database_path = "~/.local/share/pekobot/memory.db"
max_entries = 10000

[agent.provider]
type = "openai"
api_key = "${OPENAI_API_KEY}"
model = "gpt-4o-mini"
temperature = 0.7
max_tokens = 2048
timeout_seconds = 30

[coneko]
enabled = true
endpoint = "http://localhost:8080"
auth_token = "${CONEKO_TOKEN}"
auto_register = true
poll_interval_seconds = 30
```

Run with config:

```bash
./target/release/pekobot agent --config pekobot.toml
```

### Environment Variables

| Variable | Description |
|----------|-------------|
| `OPENAI_API_KEY` | Your OpenAI API key |
| `CONEKO_ENDPOINT` | Coneko server URL |
| `CONEKO_TOKEN` | Coneko authentication token |
| `AGENT_ENDPOINT` | Your agent's public endpoint |

### Configuration Priority

1. Command-line arguments (highest)
2. Environment variables
3. Config file values
4. Defaults (lowest)

---

## Working with Memory

### How Memory Works

Pekobot uses SQLite for persistent memory:

- Stores all prompts and responses
- Full-text search capability
- Organized by agent DID (namespace)
- Metadata support for filtering

### Memory Operations

**Store Memory:**
```rust
agent.store_memory("Important information", None)?;
```

**Search Memory:**
```rust
let results = agent.search_memory("query", 10)?;
for entry in results {
    println!("{}", entry.content);
}
```

### Memory Schema

Each memory entry contains:

| Field | Description |
|-------|-------------|
| `id` | Unique identifier |
| `content` | The stored text |
| `timestamp` | When it was stored |
| `metadata` | Optional JSON metadata |

---

## Multi-Agent Orchestration

### Creating Multiple Agents

```rust
use pekobot::{Agent, Config, agent::Orchestrator, a2a::registry::create_registry};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Create registry and orchestrator
    let (registry, _receiver) = create_registry();
    let mut orchestrator = Orchestrator::with_registry(registry);

    // Create agents
    let planner = Agent::new(
        Config::agent("planner")
            .with_capabilities(vec!["planning".to_string()])
            .build()
    ).await?;

    let executor = Agent::new(
        Config::agent("executor")
            .with_capabilities(vec!["execution".to_string()])
            .build()
    ).await?;

    // Add to orchestrator
    orchestrator.add_agent(planner).await?;
    orchestrator.add_agent(executor).await?;

    // Start all
    orchestrator.start_all().await?;

    // List agents
    for (did, name) in orchestrator.list_agents().await {
        println!("{}: {}", name, did);
    }

    // Stop all
    orchestrator.stop_all().await?;
    Ok(())
}
```

### Finding Agents

```rust
// By name
if let Some(agent) = orchestrator.find_by_name("planner").await {
    // Use agent...
}

// By DID
if let Some(agent) = orchestrator.find_by_did("did:pekobot:...").await {
    // Use agent...
}
```

---

## Coneko Network Integration

### What is Coneko?

Coneko is an optional network layer that enables:

- Cross-network agent discovery
- Agent-to-agent messaging across networks
- Capability-based search
- Federated agent registries

### Checking Coneko Status

```bash
./target/release/pekobot coneko status --endpoint http://localhost:8080
```

### Registering an Agent

```bash
./target/release/pekobot coneko register \
  --endpoint http://localhost:8080 \
  --did did:pekobot:local:default:abc123 \
  --name my-agent \
  --agent-endpoint http://my-server:3000 \
  --capabilities messaging,task_execution
```

### Discovering Agents

```bash
# All agents
./target/release/pekobot coneko discover --endpoint http://localhost:8080

# By capability
./target/release/pekobot coneko discover \
  --endpoint http://localhost:8080 \
  --_capability messaging
```

### Unregistering

```bash
./target/release/pekobot coneko unregister \
  --endpoint http://localhost:8080 \
  --did did:pekobot:local:default:abc123
```

---

## Troubleshooting

### Agent Won't Start

**Problem:** Agent fails to start with error about identity.

**Solution:** Check write permissions for the data directory:

```bash
ls -la ~/.local/share/pekobot/
# or on macOS
ls -la ~/Library/Application\ Support/pekobot/
```

### OpenAI API Errors

**Problem:** "Failed to create provider" or API errors.

**Solution:**
- Verify `OPENAI_API_KEY` is set: `echo $OPENAI_API_KEY`
- Check API key has sufficient credits
- Verify network connectivity to api.openai.com

### Coneko Connection Fails

**Problem:** "Failed to connect" when using Coneko commands.

**Solution:**
- Verify Coneko server is running: `curl http://localhost:8080/health`
- Check firewall settings
- Verify `CONEKO_TOKEN` is correct

### Memory Database Locked

**Problem:** SQLite "database is locked" error.

**Solution:**
- Only one agent can use a database at a time
- Use different database paths for different agents
- Check for zombie processes: `ps aux | grep pekobot`

### Getting Help

```bash
# Show help
./target/release/pekobot --help

# Show command-specific help
./target/release/pekobot agent --help
./target/release/pekobot coneko --help
```

---

## Next Steps

- Read the [API Documentation](API.md) for programmatic usage
- Try the [examples](../examples/) to see Pekobot in action
- Read the [Architecture Guide](ARCHITECTURE.md) to understand internals
- Build your own agent following the [Tutorial](TUTORIAL_BUILDING_FIRST_AGENT.md)

---

*Built with 🐰 by the Coneko team*
