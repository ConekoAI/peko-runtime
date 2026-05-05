# Pekobot User Guide

Welcome to the Pekobot User Guide! This guide will help you understand and use Pekobot effectively.

## Table of Contents

1. [What is Pekobot?](#what-is-pekobot)
2. [Installation](#installation)
3. [Core Concepts](#core-concepts)
4. [Running Your First Agent](#running-your-first-agent)
5. [Configuration](#configuration)
6. [Working with Sessions](#working-with-sessions)
7. [Multi-Agent Teams](#multi-agent-teams)
8. [Extensions](#extensions)
9. [Troubleshooting](#troubleshooting)

---

## What is Pekobot?

Pekobot 🐱 is a lightweight multi-agent runtime written in Rust. It allows you to:

- Run autonomous AI agents locally
- Connect agents to LLM providers (OpenAI, Anthropic, Kimi, Ollama, etc.)
- Organize agents into teams
- Manage persistent sessions
- Orchestrate multiple agents working together
- Schedule tasks with cron jobs

### Key Features

| Feature | Description |
|---------|-------------|
| **Lightweight** | Small binary, starts quickly |
| **Standalone** | Works without external services |
| **Multi-Agent** | Coordinate multiple agents in teams |
| **Persistent Sessions** | JSONL-based session storage |
| **DID Identity** | ed25519-based decentralized identifiers |
| **Extensions** | Unified Extension Architecture for skills, MCP, tools, channels, hooks |
| **Cron Scheduling** | Schedule recurring and one-time tasks |

---

## Installation

### Prerequisites

- Rust 1.70+ (install from [rustup.rs](https://rustup.rs))
- API key for at least one LLM provider

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

./target/release/pekobot --help
```

---

## Core Concepts

### Agent

An agent is the fundamental unit in Pekobot. It has:

- **Identity** — A unique DID (decentralized identifier)
- **Configuration** — Agent settings including provider, model, etc.
- **Sessions** — Conversation history stored as JSONL
- **Extensions** — Enabled capabilities (tools, skills, MCP, etc.)

### DID (Decentralized Identifier)

Every agent gets a unique identifier like:

```
did:pekobot:local:default:abc123def456
```

Format: `did:pekobot:{scope}:{tenant}:{identifier}`

- **Scope**: `local`, `tenant`, or `global`
- **Tenant**: Organization or namespace (team)
- **Identifier**: Unique hash-based ID

### Team

Teams group related agents together. Agents are referenced as `team/agent` or just `agent` (uses default team).

### Session

Sessions store conversation history as JSONL files. They support branching, switching, and compaction.

---

## Running Your First Agent

### 1. Set Your API Key

```bash
# For OpenAI
export OPENAI_API_KEY="sk-..."

# For Anthropic
export ANTHROPIC_API_KEY="sk-ant-..."

# For Kimi
export KIMI_API_KEY="your-kimi-key"
```

### 2. Create an Agent

```bash
# Create a new agent
./target/release/pekobot agent create my-agent --provider minimax

# Or initialize an agent directory
./target/release/pekobot agent init ./my-agent --provider minimax
```

### 3. Send a Message

```bash
# Send a message to the agent
./target/release/pekobot send my-agent "Hello, what can you do?"
```

You'll see the agent's response streamed to your terminal.

### 4. Start a New Session

```bash
# Start a fresh conversation
./target/release/pekobot send my-agent "Let's start fresh" --new
```

---

## Configuration

### Agent Configuration

Agents are configured via `config.toml` in the agent directory (when using `agent init`) or stored in the config directory (when using `agent create`).

Example `config.toml`:

```toml
[agent]
name = "my-agent"
description = "A helpful assistant"

[agent.provider]
type = "openai"
model = "gpt-4o-mini"
temperature = 0.7
max_tokens = 2048
```

### Environment Variables

| Variable | Description |
|----------|-------------|
| `OPENAI_API_KEY` | Your OpenAI API key |
| `ANTHROPIC_API_KEY` | Your Anthropic API key |
| `KIMI_API_KEY` | Your Kimi API key |
| `RUST_LOG` | Logging level (debug, info, warn, error) |
| `PEKOBOT_CONFIG_DIR` | Configuration directory override |
| `PEKOBOT_DATA_DIR` | Data directory override |

### Configuration Priority

1. Command-line arguments (highest)
2. Environment variables
3. Config file values
4. Defaults (lowest)

---

## Working with Sessions

### List Sessions

```bash
pekobot session list my-agent
```

### Show Session History

```bash
pekobot session show my-agent sess_xxx
```

### Branch a Session

```bash
pekobot session branch my-agent sess_xxx
```

### Switch Active Session

```bash
pekobot session switch my-agent sess_xxx
```

### Compact a Session

Compaction summarizes old messages to reduce context window usage:

```bash
pekobot session compact my-agent sess_xxx
```

---

## Multi-Agent Teams

### Create a Team

```bash
pekobot team create myteam
```

### Create Agents in a Team

```bash
pekobot agent create myteam/planner --provider minimax
pekobot agent create myteam/executor --provider minimax
```

### Send Messages to Team Agents

```bash
pekobot send myteam/planner "Plan a project"
pekobot send myteam/executor "Execute step 1"
```

---

## Extensions

Extensions provide additional capabilities to agents through the Unified Extension Architecture.

### List Extensions

```bash
pekobot ext list
```

### Install an Extension

```bash
pekobot ext install <path-or-url>
```

### Enable/Disable Capabilities

```bash
pekobot ext enable <capability>
pekobot ext disable <capability>
```

### MCP Servers

MCP (Model Context Protocol) servers are managed as extensions:

```bash
# MCP servers are installed and managed via the ext command
pekobot ext install <mcp-extension>
pekobot ext enable <mcp-extension>
```

---

## Troubleshooting

### Agent Won't Respond

**Problem:** Agent fails to respond to messages.

**Solution:**
- Check the agent exists: `pekobot agent list`
- Verify your API key is set: `echo $OPENAI_API_KEY`
- Check the agent configuration: `pekobot agent show my-agent`

### API Key Errors

**Problem:** "Failed to create provider" or API errors.

**Solution:**
- Verify the API key is set: `echo $OPENAI_API_KEY`
- Check the key has sufficient credits
- Verify network connectivity to the provider

### Session Issues

**Problem:** Session not found or corrupted.

**Solution:**
- List available sessions: `pekobot session list my-agent`
- Start a new session: `pekobot send my-agent "Hello" --new`
- Compact old sessions: `pekobot session compact my-agent sess_xxx`

### Getting Help

```bash
# Show help
./target/release/pekobot --help

# Show command-specific help
./target/release/pekobot agent --help
./target/release/pekobot send --help
./target/release/pekobot ext --help
```

---

## Next Steps

- Read the [CLI Reference](CLI_REFERENCE.md) for all commands
- Read the [Architecture Guide](../dev/ARCHITECTURE.md) to understand internals
- Read the [Tutorial: Building Your First Agent](../getting-started/TUTORIAL_BUILDING_FIRST_AGENT.md)

---

*Built with 🐱 by the Pekobot team*
