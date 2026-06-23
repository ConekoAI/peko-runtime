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
- ✅ **Built-in Tools** — Filesystem, shell, cron, session, messaging, task management
- ✅ **Unified Extension Architecture** — Hook-based extension points for maximum composability

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
./target/release/peko
```

### Basic Usage

```bash
# Create an agent (default provider is minimax)
./target/release/peko agent create myagent --provider kimi

# Send a message to an agent (primary interaction method)
./target/release/peko send myagent "Hello, what can you do?"

# Send from a file or stdin
echo "Hello" | ./target/release/peko send myagent --stdin
./target/release/peko send myagent --file prompt.txt

# Check version
./target/release/peko --version
```

---

## CLI Reference

Pekobot uses a hierarchical command structure (`peko <noun> <verb>`).

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
peko agent create <NAME> --provider <PROVIDER>   # Create an agent
peko agent list [--long]                          # List all agents
peko agent show <NAME>                            # Show agent details
peko agent remove <NAME> [--force]                # Remove an agent
peko agent move <NAME> --team <TEAM>              # Move agent to team
peko agent export <NAME> [--output <PATH>]        # Export to .agent package
peko agent import <FILE> [--name <NEW_NAME>]      # Import from .agent package
peko agent inspect <FILE>                         # Inspect package without importing
peko agent config <NAME>                          # Edit agent configuration
```

> **Note:** There is no `peko agent start` command. Use `peko send` to interact with agents.

#### Team Management
```bash
peko team create <NAME>                           # Create a team
peko team list                                    # List all teams
peko team show <NAME>                             # Show team details
peko team remove <NAME> [--force]                 # Remove a team
peko team move <NAME> --to <TEAM>                 # Move team
peko team export <NAME> [--output <PATH>]         # Export team
peko team import <FILE> [--name <NEW_NAME>]       # Import team
```

#### Send Messages (Primary Interaction)
```bash
peko send <AGENT> [MESSAGE]                       # Send message to agent
peko send <AGENT> --file <PATH>                   # Send message from file
peko send <AGENT> --stdin                         # Read message from stdin
peko send <AGENT> "Hello" --session <ID>          # Send to specific session
```

#### Authentication (v3: catalog + keychain)
```bash
# 1. Add a provider entry to the runtime catalog (`~/.peko/providers.toml`)
peko provider add openai --template openai
peko provider add my-local --api-format openai_completions --base-url http://localhost:8080

# 2. Store the API key in the OS keychain (one per provider)
peko credential set openai            # prompts for the key
peko credential set my-local --key $MY_KEY

# 3. Create an agent — the provider id references the catalog entry
peko agent create alice --preferred-provider openai --preferred-model gpt-4o-mini

# Inspect / manage the catalog
peko provider list
peko provider set-default openai
peko credential list
peko credential test openai

# PekoHub registry token (separate flow)
peko login --api-key ph_xxx --registry https://hub.example.com
peko logout
```

#### Extension Management
```bash
peko ext install <PATH|URL>                       # Install an extension
peko ext list                                     # List installed extensions
peko ext enable <ID>                              # Enable an extension
peko ext disable <ID>                             # Disable an extension
peko ext uninstall <ID>                           # Uninstall an extension
peko ext info <ID>                                # Show extension info
peko ext bundle <PATH> [--output <PATH>]          # Bundle extension
peko ext config <ID>                              # Configure extension
peko ext validate <PATH>                          # Validate extension manifest
```

#### Session Management
```bash
peko session list <AGENT>                         # List sessions for agent
peko session show <ID>                            # Show session details
peko session branch <ID> --name <NAME>            # Branch a session
peko session remove <ID> [--force]                # Remove a session
peko session switch <AGENT> <ID>                  # Switch active session
peko session compact <ID>                         # Compact session (remove old turns)
```

#### Configuration
```bash
peko config validate [FILE]                       # Validate config file
peko config init [--output <FILE>]                # Generate a new config file
peko config defaults                              # Show default values
peko config path                                  # Show config paths
peko config get <KEY>                             # Get config value
peko config set <KEY> <VALUE>                     # Set config value
```

#### System
```bash
peko system status                                # Show system status
peko system info                                  # Show system info
peko system doctor                                # Run health check
peko system clean                                 # Clean up cache/logs
peko system update                                # Check for updates
```

> **Note:** There is no `peko status` top-level command. Use `peko system status`.

#### Daemon
```bash
peko daemon start [--foreground]                  # Start the daemon
peko daemon stop                                  # Stop the daemon
peko daemon status                                # Check daemon status
peko daemon restart                               # Restart the daemon
peko daemon check                                 # Trigger immediate check
```

#### Cron Jobs
```bash
peko cron list                                    # List all cron jobs
peko cron add --name <NAME> --schedule <CRON> --message <MSG>   # Add recurring job
peko cron at --name <NAME> --at <TIME> --message <MSG>          # One-shot job
peko cron every --name <NAME> --interval <INTERVAL> --message <MSG>  # Interval job
peko cron remove <ID>                             # Remove a job
peko cron history <ID>                            # View job history
peko cron run <ID>                                # Run job immediately
```

#### Orchestration
```bash
peko orchestration event-router                   # Manage event router
peko orchestration webhook                        # Manage webhooks
peko orchestration watch                          # Manage file watchers
```

> **Note:** There is no `peko orchestrate` top-level command. Use `peko orchestration`.

#### Provider Management
```bash
peko provider list                                # List available providers
```

#### Update
```bash
peko update                                       # Update Pekobot
peko update --check                               # Check for updates only
```

#### Shell Completions
```bash
peko completions bash                             # Bash completions
peko completions zsh                              # Zsh completions
peko completions fish                             # Fish completions
peko completions powershell                       # PowerShell completions
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
peko ext install ./my-skill
peko ext install ./mcp-server.json
peko ext install ./discord-gateway

# List all extensions
peko ext list
# ID           TYPE      STATUS   HOOKS
# docker       skill     enabled  prompt:skills
# filesystem   mcp       enabled  prompt:tools, tool:*
# discord      gateway   enabled  channel:*, event:*

# Enable/disable
peko ext enable docker
peko ext disable docker
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
peko agent export my-agent --output ./my-agent.agent

# Import an agent
peko agent import ./my-agent.agent --name imported-agent

# Inspect a package without importing
peko agent inspect ./my-agent.agent
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
peko cron list

# Add a recurring cron job
peko cron add \
  --name "daily-report" \
  --schedule "0 9 * * *" \
  --message "Generate daily sales report"

# Add an interval-based job (every 5 minutes)
peko cron every \
  --name "heartbeat" \
  --interval "5m" \
  --message "Check system status"

# Add a one-shot job at specific time
peko cron at \
  --name "reminder" \
  --at "2026-03-01T09:00:00Z" \
  --message "Meeting in 1 hour"

# Remove a job
peko cron remove <ID>

# View job history
peko cron history <ID>

# Run a job immediately (manual trigger)
peko cron run <ID>
```

### Daemon Mode

The daemon is a long-running process that polls for due jobs and executes them automatically.

```bash
# Start the daemon (foreground mode)
peko daemon start --foreground

# Check daemon status
peko daemon status

# Trigger immediate cron check
peko daemon check

# Stop the daemon gracefully
peko daemon stop

# Restart the daemon
peko daemon restart
```

---

## Configuration

Create a `peko.toml` file:

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
├── agents/             # Agent management (stateless manager, config, lifecycle, prompts)
├── auth/               # Authentication, authorization, principal, ownership, JWT, API keys
├── commands/           # CLI command implementations (clap-based)
├── common/             # Shared services and core types (AgentService, vault, KV, types)
├── cron/               # Cron job scheduling and persistence
├── daemon/             # HTTP daemon (Axum-based), health, info endpoints
├── engine/             # Core agentic loop execution engine
├── extensions/         # Extension framework + type implementations
│   ├── framework/      # Generic extension framework (ADR-017)
│   ├── builtin/        # Built-in tool adapter
│   ├── gateway/        # Gateway adapter
│   ├── general/        # General extension adapter
│   ├── mcp/            # MCP adapter
│   ├── skill/          # Skill adapter
│   └── universal/      # Universal tool adapter
├── identity/           # DID identity system, ed25519 keys, key storage, runtime identity
├── ipc/                # Inter-process communication
├── observability/      # Metrics, logging, tracing, audit
├── providers/          # LLM provider integrations (v3 catalog + resolver)
├── registry/           # Packaging/export/import (.agent/.team/.ext) and remote registry client
├── session/            # JSONL persistence, branching, indexing, compaction
├── tools/              # Tool framework (core, builtin, registry, factory)
├── tunnel/             # Pekohub tunnel protocol, A2A dispatcher, runtime discovery
├── main.rs             # CLI entry point
└── lib.rs              # Library surface (public domains + re-exports)
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

- [Getting Started](docs/getting-started/GETTING_STARTED.md) — Build and run your first agent
- [Tutorial: Building Your First Agent](docs/getting-started/TUTORIAL_BUILDING_FIRST_AGENT.md) — Step-by-step walkthrough
- [User's Guide](docs/user-guide/USERS_GUIDE.md) — Concepts, sessions, teams, extensions
- [CLI Reference](docs/user-guide/CLI_REFERENCE.md) — Every `peko` command and flag
- [Extension System](docs/architecture/EXTENSION_SYSTEM.md) — Unified extension architecture
- [Architecture Decision Records](docs/architecture/adr/) — ADR-001 through ADR-039
- [MCP Overview](docs/mcp/MCP.md) — Model Context Protocol integration
- [Agent Guide](AGENTS.md) — Build, test, code-style rules for contributors
- [API Surface](API_SURFACE.md) — Public Rust API contracts
- [Data Model](DATA_MODEL.md) — On-disk and in-memory data formats

---

*Built with 🐰 by the Coneko team*
