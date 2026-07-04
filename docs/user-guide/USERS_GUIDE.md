# Peko User Guide

Welcome to the Peko User Guide! This guide will help you understand and use Peko effectively.

## Table of Contents

1. [What is Peko?](#what-is-peko)
2. [Installation](#installation)
3. [Core Concepts](#core-concepts)
4. [Running Your First Principal](#running-your-first-principal)
5. [Configuration](#configuration)
6. [Extensions](#extensions)
7. [Troubleshooting](#troubleshooting)
8. [Next Steps](#next-steps)

---

## What is Peko?

Peko 🐱 is a lightweight multi-agent runtime written in Rust. It allows you to:

- Run autonomous AI Principals locally
- Connect Principals to LLM providers (OpenAI, Anthropic, Kimi, Ollama, etc.)
- Manage persistent conversation memory automatically
- Orchestrate tools, skills, MCP servers, and gateways through a unified extension system
- Schedule tasks with cron jobs

### Key Features

| Feature | Description |
|---------|-------------|
| **Lightweight** | Small binary, starts quickly |
| **Standalone** | Works without external services |
| **Principal-Centric** | A Principal is the top-level actor you create and chat with |
| **Persistent Memory** | JSONL-based session storage managed automatically per Principal |
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
git clone https://github.com/coneko/peko
cd peko

# Build in release mode
cargo build --release

# The binary is now available at:
./target/release/peko
```

### Verify Installation

```bash
./target/release/peko --version

./target/release/peko --help
```

---

## Core Concepts

### Principal

A **Principal** is the top-level AI actor in Peko. It owns:

- **Identity** — A unique DID (decentralized identifier)
- **Configuration** — Settings including allowed extensions, provider hints, and governance
- **Memory** — Conversation history stored as JSONL, managed automatically per peer
- **Agent Prompts** — Thin Markdown files that shape the Principal's behavior
- **Extensions** — Allowed tools, skills, MCP servers, and gateways

You interact with a Principal through `peko send`.

### DID (Decentralized Identifier)

Every Principal gets a unique identifier like:

```
did:peko:local:default:abc123def456
```

Format: `did:peko:{scope}:{tenant}:{identifier}`

- **Scope**: `local`, `tenant`, or `global`
- **Tenant**: Organization or namespace
- **Identifier**: Unique hash-based ID

### Session

Sessions store conversation history as JSONL files. They are created, resumed,
branched, and compacted automatically by the Principal — there is no dedicated
`peko session` command. Advanced inspection is available through
`peko principal memory session <principal>`.

---

## Running Your First Principal

### 1. Set Your API Key

```bash
# For OpenAI
export OPENAI_API_KEY="sk-..."

# For Anthropic
export ANTHROPIC_API_KEY="sk-ant-..."

# For Kimi
export KIMI_API_KEY="your-kimi-key"
```

### 2. Add a Provider

```bash
./target/release/peko provider add openai --template openai --default
```

### 3. Create a Principal

```bash
./target/release/peko principal create my-principal
```

### 4. Send a Message

```bash
./target/release/peko send my-principal "Hello, what can you do?"
```

You'll see the Principal's response streamed to your terminal.

---

## Configuration

### Principal Configuration

Principals are configured via `principal.toml` stored in the Principal's workspace.

Example `principal.toml`:

```toml
name = "my-principal"
description = "A helpful assistant"

# Optional per-Principal provider override. Most Principals should
# omit this and use the global catalog default.
# preferred_provider_id = "openai"
# preferred_model_id = "gpt-4o-mini"

allowed_extensions = ["Bash", "Read", "Write"]
```

### Environment Variables

| Variable | Description |
|----------|-------------|
| `OPENAI_API_KEY` | Your OpenAI API key |
| `ANTHROPIC_API_KEY` | Your Anthropic API key |
| `KIMI_API_KEY` | Your Kimi API key |
| `RUST_LOG` | Logging level (debug, info, warn, error) |
| `PEKO_CONFIG_DIR` | Configuration directory override |
| `PEKO_DATA_DIR` | Data directory override |

### Configuration Priority

1. Command-line arguments (highest)
2. Environment variables
3. Config file values
4. Defaults (lowest)

---

## Extensions

Extensions provide additional capabilities to a Principal through the Unified Extension Architecture.

### List Extensions

```bash
peko ext list
```

### Install an Extension

```bash
peko ext install <path-or-url>
```

### Enable/Disable Extensions

```bash
peko ext enable <extension-id-or-built-in>
peko ext disable <extension-id-or-built-in>
```

Principal-scoped authorization is configured in `principal.toml` under
`[allowed_extensions]`. The `--target` flag on `peko ext enable/disable` scopes
changes to a single legacy agent prompt's `[extensions] enabled` whitelist.

### MCP Servers

MCP (Model Context Protocol) servers are managed as extensions:

```bash
# MCP servers are installed and managed via the ext command
peko ext install <mcp-extension>
peko ext enable <mcp-extension>
```

---

## Troubleshooting

### Principal Won't Respond

**Problem:** Principal fails to respond to messages.

**Solution:**
- Check the Principal exists: `peko principal list`
- Verify your API key is set: `echo $OPENAI_API_KEY`
- Check a provider is configured: `peko provider get-default`
- Check the Principal configuration: `peko principal show my-principal`

### API Key Errors

**Problem:** "Failed to create provider" or API errors.

**Solution:**
- Verify the API key is set: `echo $OPENAI_API_KEY`
- Check the key has sufficient credits
- Verify network connectivity to the provider

### Memory Issues

**Problem:** Conversation history appears missing or corrupted.

**Solution:**
- List sessions for the Principal: `peko principal memory session my-principal`
- Memory compaction is automatic; send another message to resume the current session

### Getting Help

```bash
# Show help
./target/release/peko --help

# Show command-specific help
./target/release/peko principal --help
./target/release/peko send --help
./target/release/peko ext --help
```

---

## Next Steps

- Read the [CLI Reference](CLI_REFERENCE.md) for all commands
- Read the [Extension System](../architecture/EXTENSION_SYSTEM.md) to understand how extensions plug in
- Read the [Tutorial: Building Your First Agent](../getting-started/TUTORIAL_BUILDING_FIRST_AGENT.md)

---

*Built with 🐱 by the Peko team*
