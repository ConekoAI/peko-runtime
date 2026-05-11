# Pekobot 🐱

**Lightweight Multi-Agent Runtime**

Pekobot is a Rust-based multi-agent runtime that supports local multi-agent orchestration with DID identity, A2A protocol messaging, session overlays, and a unified extension architecture.

> **Version:** 0.1.0 | **License:** MIT

## Philosophy

- **Lightweight** — Small binary, fast startup
- **Multi-agent** — Orchestrate multiple agents with A2A protocol
- **Secure** — ed25519 identity, DID-based addressing
- **Extensible** — Unified hook-based extension system
- **Daemon-first** — The CLI is a thin client; all execution happens in the daemon

## Features

### Core Architecture
- ✅ **DID Identity System** — ed25519-based decentralized identifiers
- ✅ **A2A Protocol** — Agent-to-Agent messaging
- ✅ **Multi-Agent Orchestration** — Local registry and message routing
- ✅ **Session Overlays** — Fork, spawn, and merge sessions for complex workflows
- ✅ **Event Router** — Central event routing and subscription system

### LLM & Providers
- ✅ **15+ LLM Providers** — OpenAI, Anthropic, Kimi, OpenRouter, and more
- ✅ **Streaming Output** — Real-time progressive output with tool visibility

### Tools & Capabilities
- ✅ **MCP Support** — Model Context Protocol for external tool integration
- ✅ **Skills System** — Documentation-driven agent capabilities (SKILL.md)
- ✅ **18 Built-in Tools** — Filesystem, shell, cron, session, messaging, and more
- ✅ **Unified Extension Architecture** — 22 hook points for maximum composability

### Memory & Persistence
- ✅ **SQLite Memory** — Persistent memory with semantic search
- ✅ **Session Storage** — JSONL-based session logs with overlays

### Scheduling & Execution
- ✅ **Cron/Daemon** — Scheduled task execution with daemon mode
- ✅ **Event Triggers** — React to file changes, webhooks, and system events

### Security & Portability
- ✅ **Security Sandbox** — Filesystem restrictions, command allowlisting
- ✅ **Portable Agents** — Export/import agents as `.agent` packages

---

## Quick Start

### Prerequisites

Set your LLM provider API key:

```bash
export OPENAI_API_KEY="your-key"  # or ANTHROPIC_API_KEY, KIMI_API_KEY, etc.
```

See `.env.example` for all available options.

### Build

```bash
# Clone the repository
git clone https://github.com/coneko/pekobot
cd pekobot

# Build
cargo build --release

# The binary will be at:
./target/release/pekobot
```

### Basic Usage

```bash
# Create an agent (default provider is minimax)
./target/release/pekobot agent create myagent --provider kimi

# Send a message to an agent (primary interaction method)
./target/release/pekobot send myagent "Hello, what can you do?"

# Send from a file or stdin
echo "Hello" | ./target/release/pekobot send myagent --stdin
./target/release/pekobot send myagent --file prompt.txt

# Check version
./target/release/pekobot --version
```

---

## CLI Reference

Pekobot uses a hierarchical command structure (`pekobot <noun> <verb>`).

### Global Flags

```bash
--config-dir <PATH>     # Override config directory (env: PEKOBOT_CONFIG_DIR)
--data-dir <PATH>       # Override data directory (env: PEKOBOT_DATA_DIR)
--cache-dir <PATH>      # Override cache directory (env: PEKOBOT_CACHE_DIR)
--json                  # Output results as JSON
-q, --quiet             # Suppress non-error output
-v, -vv, -vvv           # Verbose logging (repeat for more)
--debug                 # Show debug information including stack traces
-U, --user <USER>       # User identifier for session isolation
```

### Commands

#### Agent Management
```bash
pekobot agent create <NAME> --provider <PROVIDER>   # Create an agent
pekobot agent list [--long]                          # List all agents
pekobot agent show <NAME>                            # Show agent details
pekobot agent remove <NAME> [--force]                # Remove an agent
pekobot agent move <NAME> --team <TEAM>              # Move agent to team
pekobot agent export <NAME> [--output <PATH>]        # Export to .agent package
pekobot agent import <FILE> [--name <NEW_NAME>]      # Import from .agent package
pekobot agent inspect <FILE>                         # Inspect package without importing
pekobot agent config <NAME>                          # Edit agent configuration
```

> **Note:** There is no `pekobot agent start` command. Use `pekobot send` to interact with agents.

#### Team Management
```bash
pekobot team create <NAME>                           # Create a team
pekobot team list                                    # List all teams
pekobot team show <NAME>                             # Show team details
pekobot team remove <NAME> [--force]                 # Remove a team
pekobot team move <NAME> --to <TEAM>                 # Move team
pekobot team export <NAME> [--output <PATH>]         # Export team
pekobot team import <FILE> [--name <NEW_NAME>]       # Import team
```

#### Send Messages (Primary Interaction)
```bash
pekobot send <AGENT> [MESSAGE]                       # Send message to agent
pekobot send <AGENT> --file <PATH>                   # Send message from file
pekobot send <AGENT> --stdin                         # Read message from stdin
pekobot send <AGENT> "Hello" --session <ID>          # Send to specific session
```

#### Authentication
```bash
pekobot auth set <PROVIDER> --key <KEY>              # Set provider API key
pekobot auth list                                    # List configured credentials
pekobot auth remove <PROVIDER>                       # Remove credential
pekobot auth test <PROVIDER>                         # Test credential
```

#### Extension Management
```bash
pekobot ext install <PATH|URL>                       # Install an extension
pekobot ext list                                     # List installed extensions
pekobot ext enable <ID>                              # Enable an extension
pekobot ext disable <ID>                             # Disable an extension
pekobot ext uninstall <ID>                           # Uninstall an extension
pekobot ext info <ID>                                # Show extension info
pekobot ext bundle <PATH> [--output <PATH>]          # Bundle extension
pekobot ext config <ID>                              # Configure extension
pekobot ext validate <PATH>                          # Validate extension manifest
```

#### Session Management
```bash
pekobot session list <AGENT>                         # List sessions for agent
pekobot session show <ID>                            # Show session details
pekobot session branch <ID> --name <NAME>            # Branch a session
pekobot session remove <ID> [--force]                # Remove a session
pekobot session switch <AGENT> <ID>                  # Switch active session
pekobot session compact <ID>                         # Compact session (remove old turns)
```

#### Configuration
```bash
pekobot config validate [FILE]                       # Validate config file
pekobot config init [--output <FILE>]                # Generate a new config file
pekobot config defaults                              # Show default values
pekobot config path                                  # Show config paths
pekobot config get <KEY>                             # Get config value
pekobot config set <KEY> <VALUE>                     # Set config value
```

#### System
```bash
pekobot system status                                # Show system status
pekobot system info                                  # Show system info
pekobot system doctor                                # Run health check
pekobot system clean                                 # Clean up cache/logs
pekobot system update                                # Check for updates
```

> **Note:** There is no `pekobot status` top-level command. Use `pekobot system status`.

#### Daemon
```bash
pekobot daemon start [--foreground]                  # Start the daemon
pekobot daemon stop                                  # Stop the daemon
pekobot daemon status                                # Check daemon status
pekobot daemon restart                               # Restart the daemon
pekobot daemon check                                 # Trigger immediate check
```

#### Cron Jobs
```bash
pekobot cron list                                    # List all cron jobs
pekobot cron add --name <NAME> --schedule <CRON> --message <MSG>   # Add recurring job
pekobot cron at --name <NAME> --at <TIME> --message <MSG>          # One-shot job
pekobot cron every --name <NAME> --interval <INTERVAL> --message <MSG>  # Interval job
pekobot cron remove <ID>                             # Remove a job
pekobot cron history <ID>                            # View job history
pekobot cron run <ID>                                # Run job immediately
```

#### Orchestration
```bash
pekobot orchestration event-router                   # Manage event router
pekobot orchestration webhook                        # Manage webhooks
pekobot orchestration watch                          # Manage file watchers
```

> **Note:** There is no `pekobot orchestrate` top-level command. Use `pekobot orchestration`.

#### Provider Management
```bash
pekobot provider list                                # List available providers
```

#### Update
```bash
pekobot update                                       # Update Pekobot
pekobot update --check                               # Check for updates only
```

#### Shell Completions
```bash
pekobot completions bash                             # Bash completions
pekobot completions zsh                              # Zsh completions
pekobot completions fish                             # Fish completions
pekobot completions powershell                       # PowerShell completions
```

---

## Unified Extension Architecture

All capabilities — tools, skills, MCP servers, channels, and gateways — are implemented through a single, consistent hook-based system.

### Extension Types

| Extension | Type | Purpose |
|-----------|------|---------|
| **Skills** | `SKILL.md` | Documentation-driven agent capabilities |
| **MCP Servers** | `config.json` | External tool server integration |
| **Universal Tools** | `manifest.json` | Executable command-line tools |
| **Built-in Tools** | Native code | Core runtime tools |
| **Channels** | `CHANNEL.toml` | I/O adapters (CLI, HTTP, etc.) |
| **Gateways** | `GATEWAY.toml` | Platform integrations (Discord, Slack) |
| **General Extensions** | `extension.yaml` | Multi-hook custom extensions |

### Managing Extensions

```bash
# Install any extension type (auto-detected)
pekobot ext install ./my-skill
pekobot ext install ./mcp-server.json
pekobot ext install ./discord-gateway

# List all extensions
pekobot ext list
# ID           TYPE      STATUS   HOOKS
# docker       skill     enabled  prompt:skills
# filesystem   mcp       enabled  prompt:tools, tool:*
# discord      gateway   enabled  channel:*, event:*

# Enable/disable
pekobot ext enable docker
pekobot ext disable docker
```

### The 22 Hook Points

Extensions hook into the agentic loop at 22 different points:

- **Prompt Hooks**: `PromptSystemSection`, `PromptPreProcess`, `PromptPostProcess`
- **Tool Hooks**: `ToolRegister`, `ToolExecute`, `ToolExecuteAsync`, `ToolCheckStatus`, `ToolCancel`
- **Session Hooks**: `SessionStateChange`, `SessionCompaction`, `SessionContextBuild`
- **I/O Hooks**: `ChannelInput`, `ChannelOutput`, `MessagePreSend`, `MessagePostReceive`
- **Event Hooks**: `EventSubscribe`, `EventEmit`
- **Lifecycle Hooks**: `AgentShutdown`, `AgentIteration`

Learn more: [Extension System Documentation](docs/architecture/EXTENSION_SYSTEM.md)

---

## Portable Agents

Export agents as `.agent` packages and import them on other machines:

```bash
# Export an agent to a .agent package
pekobot agent export my-agent --output ./my-agent.agent

# Import an agent
pekobot agent import ./my-agent.agent --name imported-agent

# Inspect a package without importing
pekobot agent inspect ./my-agent.agent
```

**Package Contents:**
- Identity (DID document + encrypted keys)
- Configuration (system prompts, capabilities)
- Memory (SQLite database)
- Skills (bundled SKILL.md files)

**Security:**
- AES-256-GCM encryption with Argon2id key derivation
- Ed25519 signatures for package integrity
- Optional key rotation on import

---

## Cron System & Daemon Mode

Pekobot includes a full cron system for scheduling tasks with a daemon mode for automatic execution.

### Managing Cron Jobs

```bash
# List all cron jobs
pekobot cron list

# Add a recurring cron job
pekobot cron add \
  --name "daily-report" \
  --schedule "0 9 * * *" \
  --message "Generate daily sales report"

# Add an interval-based job (every 5 minutes)
pekobot cron every \
  --name "heartbeat" \
  --interval "5m" \
  --message "Check system status"

# Add a one-shot job at specific time
pekobot cron at \
  --name "reminder" \
  --at "2026-03-01T09:00:00Z" \
  --message "Meeting in 1 hour"

# Remove a job
pekobot cron remove <ID>

# View job history
pekobot cron history <ID>

# Run a job immediately (manual trigger)
pekobot cron run <ID>
```

### Daemon Mode

The daemon is a long-running process that polls for due jobs and executes them automatically.

```bash
# Start the daemon (foreground mode)
pekobot daemon start --foreground

# Check daemon status
pekobot daemon status

# Trigger immediate cron check
pekobot daemon check

# Stop the daemon gracefully
pekobot daemon stop

# Restart the daemon
pekobot daemon restart
```

---

## Configuration

Create a `pekobot.toml` file:

```toml
[agent]
name = "my-agent"
description = "A helpful agent"
capabilities = ["messaging", "task_execution"]

[agent.memory]
enabled = true
database_path = "~/.local/share/pekobot/memory.db"

[agent.provider]
type = "openai"
api_key = "${OPENAI_API_KEY}"  # or set env var
base_url = "https://api.openai.com/v1"
timeout_seconds = 30

[agent.provider.default_model]
name = "gpt-4"
max_tokens = 2000
temperature = 0.7
```

---

## Architecture

### Source Structure

```
src/
├── agent/              # Agent runtime, lifecycle, registry, subagent execution
├── commands/           # CLI command handlers
├── common/             # Shared utilities, registry, services, time, identifiers
├── compaction/         # Session compaction
├── cron/               # Scheduling system
├── daemon/             # Daemon process management
├── engine/             # Agentic loop, event processing, streaming, state machine
├── extensions/         # Unified Extension Architecture (core, adapters, manager, services, transport)
├── identity/           # DID identity, keys, resolver, storage
├── image/              # Agent image building
├── ipc/                # Inter-process communication
├── mcp/                # Model Context Protocol support
├── observability/      # Logging, metrics
├── portable/           # Portable agent packages
├── prompt/             # Prompt construction
├── providers/          # LLM provider integrations (15+ providers)
├── registry/           # Tool registry
├── runtime/            # Shared runtime components
├── session/            # Session storage, overlays, JSONL
├── team/               # Multi-agent team runtime
├── tools/              # Tool framework (core, builtin, framework, registry)
└── types/              # Core type definitions
```

### Key Architectural Decisions

- **Thin CLI (ADR-021)**: The CLI is a thin client — all execution happens in the daemon
- **Unified Extensions**: Single architecture for all capabilities (tools, skills, MCP, etc.)
- **Hook-Based Registration**: 22 extension points for maximum composability
- **Filesystem-First**: All state stored on disk for easy backup and migration

---

## Development

```bash
# Run tests
cargo test

# Run with logging
RUST_LOG=debug cargo run -- agent list

# Format code
cargo fmt

# Run clippy
cargo clippy
```

---

## Docker

```bash
# Build image
docker build -t pekobot:latest .

# Run with docker-compose
docker-compose up
```

---

## License

MIT

---

## Documentation

- [Executive Summary](docs/executive/EXECUTIVE_SUMMARY.md) — Overview and value proposition
- [Architecture Overview](docs/architecture/OVERVIEW.md) — Technical architecture
- [Extension System](docs/architecture/EXTENSION_SYSTEM.md) — Unified extension architecture
- [Getting Started](docs/getting-started/GETTING_STARTED.md) — Installation and first steps
- [CLI Reference](docs/user-guide/CLI_REFERENCE.md) — Command reference

---

*Built with 🐰 by the Coneko team*
