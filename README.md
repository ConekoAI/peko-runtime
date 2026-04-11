# Pekobot 🐱

**Lightweight Multi-Agent Runtime with Optional Coneko Network**

Pekobot is a Rust-based agent runtime that supports local multi-agent orchestration and optional connection to the Coneko network for cross-network agent discovery and messaging.

## Philosophy

- **Works standalone** — No Coneko required for local multi-agent coordination
- **Optional network** — Connect to Coneko when you need cross-network discovery
- **Embeddable** — Small binary (~5-8MB), fast startup (<50ms)
- **Multi-agent** — Orchestrate multiple agents with A2A protocol
- **Secure** — ed25519 identity, DID-based addressing

## Features

### Core Architecture
- ✅ **DID Identity System** — ed25519-based decentralized identifiers
- ✅ **A2A Protocol** — Full Agent-to-Agent messaging (12 message types)
- ✅ **Multi-Agent Orchestration** — Local registry and message routing
- ✅ **Session Overlays** — Fork, spawn, and merge sessions for complex workflows
- ✅ **Event Router** — Central event routing and subscription system
- ✅ **Agent-to-Agent Messaging** — Direct agent invocation with sync/async modes

### LLM & Providers
- ✅ **15 LLM Providers** — OpenAI, Anthropic, Kimi, OpenRouter, and more
- ✅ **Streaming Output** — Real-time progressive output with tool visibility

### Communication
- ✅ **7 Communication Channels** — CLI, HTTP, Telegram, Discord, Slack, Matrix, WhatsApp

### Tools & Capabilities
- ✅ **MCP Support** — Model Context Protocol for external tool integration
- ✅ **Skills System** — Documentation-driven agent capabilities
- ✅ **18 Built-in Tools** — Calendar, email, social media, documents, inventory, expenses
- ✅ **HTTP Tools** — Built-in web request capabilities
- ✅ **Research Agent** — Web search with source assessment and citations

### Memory & Persistence
- ✅ **SQLite Memory** — Persistent memory with semantic search
- ✅ **Hybrid Memory** — SQLite + vector search for better retrieval

### Scheduling & Execution
- ✅ **Cron/Daemon** — Scheduled task execution with daemon mode
- ✅ **Event Triggers** — React to idle state and system events
- ✅ **Session-Based Scheduling** — Schedule relative to session activity

### Security & Portability
- ✅ **Security Sandbox** — Filesystem restrictions, command allowlisting
- ✅ **Portable Agents** — Export/import agents as `.agent` packages
- ✅ **Coneko Adapter** — Optional network integration

## Unified Extension Architecture (New in 2.0)

Pekobot 2.0 introduces a **Unified Extension Architecture** where all capabilities—tools, skills, MCP servers, channels, and gateways—are implemented through a single, consistent hook-based system.

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

### Unified CLI

All extensions are managed through a single interface:

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
- **Lifecycle Hooks**: `AgentInit`, `AgentShutdown`, `AgentIteration`

Learn more: [Extension System Documentation](docs/architecture/EXTENSION_SYSTEM.md)

---

## Tool Architecture

Pekobot uses a **minimal core + on-demand tools** architecture:

### Default Build (Minimal Core ~2-3MB)
```bash
cargo build --release
# Core runtime only - tools downloaded on-demand from Pekohub
```

### With All Tools Bundled (~8MB)
```bash
cargo build --release --features full
# Legacy behavior - all tools included in binary
```

### On-Demand Tools (Recommended)
Configure Pekobot to download tools automatically:

```toml
# ~/.config/pekobot/config.toml
[tools]
core = ["http", "filesystem", "cron", "memory"]
on_demand = ["calendar", "email", "social_media"]

[tools.registry]
type = "pekohub"
url = "https://tools.coneko.ai"
```

Tools are downloaded on first use and cached locally.

### Air-Gapped / Offline Use
Install tools locally without internet:

```bash
git clone https://github.com/coneko/tool_bundle
cd tool_bundle
cargo build --release
./install.sh  # Installs to ~/.local/share/pekobot/tools/
```

### Available Tools

| Tool | Description | Size |
|------|-------------|------|
| `calendar` | Google/Outlook integration | ~120KB |
| `document` | PDF/OCR processing | ~200KB |
| `email` | Gmail/Outlook email | ~100KB |
| `expense` | Receipt tracking | ~80KB |
| `inventory` | Shopify/WooCommerce | ~90KB |
| `research` | Web search | ~110KB |
| `social_media` | Twitter/X, LinkedIn | ~150KB |

## MCP Support

Pekobot supports the Model Context Protocol (MCP) for integrating external tool servers:

```bash
# Add an MCP server
pekobot mcp add filesystem --command "npx -y @modelcontextprotocol/server-filesystem /tmp"

# Start the server
pekobot mcp start filesystem

# List available MCP tools
pekobot mcp tools
```

MCP servers provide capabilities like:
- File system access
- Web browser automation
- Database queries
- Custom business logic

## Skills System

Skills are documentation-driven capabilities that teach agents how to perform specific tasks:

```bash
# Create skills directory
mkdir -p ~/.pekobot/skills/github

# Add a skill
cat > ~/.pekobot/skills/github/SKILL.md << 'EOF'
---
name: github
description: GitHub CLI operations - manage repos, issues, PRs
---

# GitHub Skill

Use this skill when working with GitHub repositories.

## Common Commands

```bash
gh pr list --repo owner/repo
gh issue create --title "Bug" --body "Description"
```
EOF
```

Agents automatically detect skills and use them when appropriate.

---

## Quick Start

### Prerequisites

Depending on which channel you use, you'll need different API keys:

**For LLM providers:**
```bash
export OPENAI_API_KEY="your-key"  # or ANTHROPIC_API_KEY, KIMI_API_KEY, etc.
```

**For WhatsApp channel:**
```bash
export WHATSAPP_ACCESS_TOKEN="your-token"
export WHATSAPP_PHONE_NUMBER_ID="your-phone-id"
export WHATSAPP_VERIFY_TOKEN="your-verify-token"
```

See `.env.example` for all available options.

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
# Run a single agent
./target/release/pekobot agent --name myagent

# Run with memory enabled
./target/release/pekobot agent --name myagent --memory

# Run multi-agent orchestrator
./target/release/pekobot orchestrate

# Check version
./target/release/pekobot --version
```

### CLI Reference

Pekobot uses a hierarchical command structure (`pekobot <noun> <verb>`):

#### Global Flags
```bash
--config-dir <PATH>     # Override config directory (env: PEKOBOT_CONFIG_DIR)
--data-dir <PATH>       # Override data directory (env: PEKOBOT_DATA_DIR)
--cache-dir <PATH>      # Override cache directory (env: PEKOBOT_CACHE_DIR)
--json                  # Output results as JSON
-q, --quiet             # Suppress non-error output
-v, -vv                 # Verbose logging (repeat for more)
-V, --version           # Show version
```

#### Agent Management
```bash
# Create an agent
pekobot agent create <NAME> --provider <openai|anthropic|ollama|kimi>

# List all configured agents
pekobot agent list [--long]

# Show agent details
pekobot agent show <NAME>

# Run an agent interactively
pekobot agent start --name <NAME>
pekobot agent start --config <PATH>

# Delete an agent
pekobot agent delete <NAME> [--purge] [--force]

# Export to .agent package
pekobot agent export --name <NAME> [--output <PATH>] [--encrypt]

# Import from .agent package
pekobot agent import --file <PATH> [--name <NEW_NAME>]

# Inspect package without importing
pekobot agent inspect <FILE>
```

#### Tool Management
```bash
# List installed tools
pekobot tool list [--long]

# Search Pekohub registry
pekobot tool search <QUERY> [--limit <N>]

# Install a tool
pekobot tool install <NAME> [--version <VER>] [--force]

# Uninstall a tool
pekobot tool uninstall <NAME> [--force]

# Show tool information
pekobot tool info <NAME>
```

#### Session Management
```bash
# List active sessions
pekobot session list [--all] [--agent <NAME>]

# Show session details
pekobot session show <ID> [--history]

# Send message to a session
pekobot session send <ID> <MESSAGE>

# Terminate a session
pekobot session kill <ID> [--force]
```

#### Configuration Management
```bash
# Validate a config file
pekobot config validate [FILE]

# Generate a new config file
pekobot config init --output <FILE> --template <minimal|coding|research|full>

# Show default values
pekobot config defaults [--json]

# Show config paths
pekobot config path [--json]

# Get/set config values (placeholder)
pekobot config get <KEY> [--file <PATH>]
pekobot config set <KEY> <VALUE> [--file <PATH>]
```

#### System Commands
```bash
# Show system status
pekobot system status [--resources]

# Show system info
pekobot system info [--json]

# Run health check
pekobot system doctor [--fix]

# Clean up cache/logs
pekobot system clean [--tools] [--logs] [--all]

# Check for updates
pekobot system update [--check]
```

#### Gateway Management
```bash
# List installed gateways
pekobot gateway list

# Search available gateways
pekobot gateway search [QUERY]

# Install a gateway
pekobot gateway install <NAME> [--version <VER>]

# Show gateway info
pekobot gateway info <NAME>
```

#### Shell Completions
```bash
# Generate shell completions
pekobot completions bash    > /etc/bash_completion.d/pekobot
pekobot completions zsh     > /usr/local/share/zsh/site-functions/_pekobot
pekobot completions fish    > ~/.config/fish/completions/pekobot.fish
pekobot completions powershell  # For PowerShell
```

### Legacy Commands

The following commands are deprecated and will be removed:

```bash
# Agent (deprecated - use 'pekobot agent start')
pekobot agent --name <NAME>

# Orchestrate (deprecated)
pekobot orchestrate --config <FILE>

# Export/Import (deprecated - use 'pekobot agent export/import')
pekobot export --agent <NAME>
pekobot import --file <FILE>
pekobot inspect --file <FILE>

# Secret management (deprecated - use plain text or env vars)
pekobot secret set <NAME>
pekobot secret get <NAME>
pekobot secret list
```

### Shell Completion Installation

Enable tab completion for your shell:

**Bash:**
```bash
pekobot completions bash | sudo tee /etc/bash_completion.d/pekobot > /dev/null
# Or for user-local:
pekobot completions bash >> ~/.bash_completion
```

**Zsh:**
```bash
pekobot completions zsh | sudo tee /usr/local/share/zsh/site-functions/_pekobot > /dev/null
# Or for Oh-My-Zsh:
pekobot completions zsh > ~/.oh-my-zsh/completions/_pekobot
```

**Fish:**
```bash
pekobot completions fish > ~/.config/fish/completions/pekobot.fish
```

**PowerShell:**
```powershell
pekobot completions powershell | Out-String | Invoke-Expression
# Or save to profile:
pekobot completions powershell >> $PROFILE
```

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

[coneko]
enabled = true
endpoint = "http://localhost:8080"
auth_token = "${CONEKO_TOKEN}"
auto_register = true
poll_interval_seconds = 30
```

## Examples

### Echo Agent
```bash
# Run the echo agent example
cargo run --example echo_agent
```

### Multi-Agent Orchestration
```bash
# Run multiple agents working together
cargo run --example multi_agent
```

### Coneko Network
```bash
# Connect to Coneko network
export CONEKO_ENDPOINT=http://localhost:8080
export CONEKO_TOKEN=your-token
export AGENT_ENDPOINT=http://localhost:3000
cargo run --example coneko_network
```

### HTTP Tool Usage
```bash
# Demonstrates HTTP tool capabilities
cargo run --example http_tool
```

## Architecture

### Unified Extension Architecture (Post-ADR-017)

```
pekobot/
├── extensions/           # Unified extension system
│   ├── core/            # ExtensionCore - 22 hook points
│   ├── adapters/        # Type-specific adapters
│   │   ├── skill_adapter.rs
│   │   ├── mcp_adapter.rs
│   │   ├── universal_tool_adapter.rs
│   │   ├── builtin_tool_adapter.rs
│   │   ├── channel_adapter.rs
│   │   ├── gateway_adapter.rs
│   │   └── general_adapter.rs
│   └── manager/         # ExtensionManager - lifecycle
├── agent/               # Agent management and orchestration
├── a2a/                 # A2A Protocol implementation
├── channels/            # Communication channels (CLI, HTTP)
├── coneko/              # Optional Coneko network adapter
├── config/              # TOML/JSON configuration
├── identity/            # DID identity and ed25519 keys
├── memory/              # SQLite persistence
├── providers/           # LLM provider integrations
├── tools/               # Built-in tools
└── types/               # Core type definitions
```

### Key Architectural Decisions

- **Stateless Execution**: Agents cold-start on every request for reproducibility
- **Unified Extensions**: Single architecture for all capabilities (tools, skills, MCP, etc.)
- **Hook-Based Registration**: 22 extension points for maximum composability
- **Filesystem-First**: All state stored on disk for easy backup and migration

## Project Status

| Phase | Status | Description |
|-------|--------|-------------|
| 1 | ✅ | Project skeleton |
| 2 | ✅ | Core types |
| 3 | ✅ | Identity system |
| 4 | ✅ | Single agent runtime |
| 5 | ✅ | Multi-agent orchestration |
| 6 | ✅ | Channels (CLI + HTTP) |
| 7 | ✅ | Coneko adapter |
| 8 | ✅ | Polish (docs, examples, tests, Docker) |
| 9 | ✅ | CLI Redesign — Hierarchical commands, shell completions |
| 10 | ✅ | **Unified Extension Architecture (ADR-017)** — Single system for tools, skills, MCP |

### CLI Redesign (2026-02-24)

The CLI has been completely redesigned with a hierarchical command structure:

**Before (flat):**
```bash
pekobot agent --name my-agent
pekobot export --agent my-agent
pekobot tool search calendar
```

**After (hierarchical):**
```bash
pekobot agent start --name my-agent
pekobot agent export --name my-agent
pekobot tool search calendar
```

**New Features:**
- ✅ 7 command groups: agent, tool, session, config, system, gateway, completions
- ✅ 30+ subcommands
- ✅ Global flags: `--json`, `--quiet`, `-v`, `--config-dir`
- ✅ Shell completions for bash, zsh, fish, powershell, elvish
- ✅ Environment variable overrides

## Tool Registry (Pekohub)

Pekobot supports on-demand tool installation from the Pekohub cloud registry:

```bash
# Search for tools
pekobot tool search calendar

# Install a tool
pekobot tool install calendar

# List installed tools
pekobot tool list

# Check for updates
pekobot tool update --check
```

Tools are downloaded from Pekohub, verified (SHA256 + Ed25519), and cached locally at `~/.cache/pekobot/tools/`.

**Minimal Runtime Mode:**
Build Pekobot with only the core runtime (~2MB), then download tools as needed:

```bash
# Build minimal runtime
cargo build --release --no-default-features

# Tools downloaded on first use
pekobot agent --minimal --auto-install-tools
```

## Portable Agents (Docker-like Packaging)

Export agents as `.agent` packages and import them on other machines:

```bash
# Export an agent to a .agent package
pekobot agent export --name my-agent --output ./my-agent.agent

# Export with encryption (recommended for sharing)
pekobot agent export --name my-agent --output ./my-agent.agent --encrypt

# Import an agent
pekobot agent import --file ./my-agent.agent --name imported-agent

# Import with key rotation (new DID)
pekobot agent import --file ./my-agent.agent --name imported-agent

# Inspect a package without importing
pekobot agent inspect ./my-agent.agent
```

**Note:** There is a known issue where exporting by name may fail. As a workaround, provide the config file path directly:
```bash
pekobot agent export --agent ~/.config/pekobot/agents/my-agent.toml --output ./my-agent.agent
```
See [ISSUES.md](./ISSUES.md) for details.

**Package Contents:**
- Identity (DID document + encrypted keys)
- Configuration (system prompts, capabilities)
- Memory (SQLite database)
- Skills (bundled TOML files)

**Security:**
- AES-256-GCM encryption with Argon2id key derivation
- Ed25519 signatures for package integrity
- Optional key rotation on import

## Secret Manager (Secure Credential Storage)

Pekobot includes a built-in secret manager for securely storing API keys, tokens, and credentials with AES-256-GCM encryption.

### Quick Start

```bash
# Store a secret (you'll be prompted for the value)
pekobot secret set OPENAI_API_KEY --type api_key

# Retrieve a secret
pekobot secret get OPENAI_API_KEY

# List all secrets
pekobot secret list
```

### Using Secrets in Config

Reference secrets in your agent config files:

```toml
[provider]
provider_type = "openai"
api_key = "${secret:OPENAI_API_KEY}"  # Resolved at runtime

# Or per-agent secret
api_key = "${secret.agent:did:pekobot:local:my-agent:OPENAI_KEY}"
```

### Access Control

Control which agents can access which secrets:

```bash
# Grant read access to an agent
pekobot secret grant --secret OPENAI_API_KEY \
  --agent did:pekobot:local:shopify-bot \
  --permission read

# Revoke access
pekobot secret revoke --secret OPENAI_API_KEY \
  --agent did:pekobot:local:shopify-bot

# List permissions
pekobot secret permissions OPENAI_API_KEY
```

### Audit Logging

Track all secret access:

```bash
# View recent audit entries
pekobot secret audit

# Show statistics
pekobot secret audit --stats

# Export to CSV
pekobot secret audit --export audit.csv
```

### Migration

Import from environment variables:

```bash
# Import all env vars matching a pattern
pekobot secret import-env --pattern "OPENAI.*"

# Preview without importing
pekobot secret import-env --pattern ".*_API_KEY" --dry-run
```

### Security Features

- **Encryption:** AES-256-GCM with per-secret keys derived via HKDF
- **Password Hashing:** Argon2id (64MB memory, 3 iterations)
- **Storage:** SQLite database at `~/.config/pekobot/secrets.db`
- **Permissions:** Fine-grained access control per agent
- **Audit:** Complete access logging with export
- **Memory Safety:** Secrets cleared from memory after use

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
  --timezone "America/New_York" \
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
pekobot cron remove --id <job-id>

# View job history
pekobot cron history --id <job-id>

# Run a job immediately (manual trigger)
pekobot cron run --id <job-id>
```

### Daemon Mode (Automatic Execution)

The daemon is a long-running process that polls for due jobs and executes them automatically.

```bash
# Start the daemon (foreground mode)
pekobot daemon start --foreground --interval 15

# Check daemon status
pekobot daemon status

# Trigger immediate cron check
pekobot daemon check

# Stop the daemon gracefully
pekobot daemon stop

# Restart the daemon
pekobot daemon restart --interval 30

# Force kill if stuck
pekobot daemon stop --force
```

### Schedule Types

| Type | Example | Description |
|------|---------|-------------|
| `cron` | `"0 9 * * *"` | Standard cron expression |
| `every` | `"5m"`, `"1h"`, `"30s"` | Recurring interval |
| `at` | `"2026-03-01T09:00:00Z"` | One-shot at specific time |

### Execution Modes

**Main Session** (default):
- Jobs are enqueued as system events
- Processed on the next agent interaction
- Lower latency, shared context

**Isolated**:
- Spawns a dedicated agent instance
- Clean slate for each execution
- Higher overhead but complete isolation

```bash
# Isolated execution
pekobot cron add \
  --name "isolated-task" \
  --schedule "0 */6 * * *" \
  --execution isolated \
  --message "Run cleanup task"
```

### Announcements

Get notified when jobs complete:

```bash
# Announce to default channel
pekobot cron add \
  --name "backup" \
  --schedule "0 2 * * *" \
  --message "Run database backup" \
  --announce
```

### Storage

- Jobs stored in SQLite at `~/.local/share/pekobot/cron.db`
- Run history preserved for auditing
- One-shot jobs auto-delete after successful execution
- Jobs survive daemon restarts

### Architecture

```
┌─────────────────────────────────────┐
│         Pekobot Daemon              │
│    (pekobot daemon start)           │
│                                     │
│  ┌─────────────────────────────┐   │
│  │    Polling Loop (15s)       │   │
│  └─────────────┬───────────────┘   │
│                │                    │
│                ▼                    │
│  ┌─────────────────────────────┐   │
│  │    Check Due Jobs           │   │
│  │    (SQLite query)           │   │
│  └─────────────┬───────────────┘   │
│                │                    │
│                ▼                    │
│  ┌─────────────────────────────┐   │
│  │    Execute Job              │   │
│  │  ├─ Main: Enqueue event     │   │
│  │  └─ Isolated: Spawn agent   │   │
│  └─────────────┬───────────────┘   │
│                │                    │
│                ▼                    │
│  ┌─────────────────────────────┐   │
│  │    Record Result            │   │
│  │    Update next_run          │   │
│  └─────────────────────────────┘   │
└─────────────────────────────────────┘
```

## Streaming Output

Pekobot supports real-time streaming for progressive output and tool visibility.

### Quick Start

```bash
# Enable streaming via CLI flag
pekobot agent start myagent --streaming

# Or configure in agent config
```toml
# ~/.pekobot/agents/myagent.toml
[streaming]
enabled = true
min_chars = 100
max_chars = 2000
break_preference = "sentence"
show_tools = true
```

### Features

- **Progressive Output**: Text appears as it's generated
- **Tool Visibility**: See 🔧 tool execution in real-time
- **Block Chunking**: Sentence/paragraph boundaries (not token spam)
- **Provider Support**: Native streaming for OpenAI, Anthropic, Kimi, Ollama, Groq

### Configuration Options

| Option | Default | Description |
|--------|---------|-------------|
| `enabled` | `false` | Enable streaming by default |
| `min_chars` | `100` | Minimum chars before emitting |
| `max_chars` | `2000` | Maximum chars per block |
| `break_preference` | `"sentence"` | paragraph, sentence, whitespace, hard |
| `show_tools` | `true` | Show tool notifications |
| `show_status` | `true` | Show "Thinking..." status |

See [docs/STREAMING.md](./docs/STREAMING.md) for full documentation.

## Development

```bash
# Run tests
cargo test

# Run with logging
RUST_LOG=debug cargo run -- agent

# Format code
cargo fmt

# Run clippy
cargo clippy
```

## Docker

```bash
# Build image
docker build -t pekobot:latest .

# Run agent
docker run -it pekobot:latest agent --name docker-agent

# Run with docker-compose
docker-compose up
```

## License

MIT

## Documentation

- [Executive Summary](docs/executive/EXECUTIVE_SUMMARY.md) — Overview and value proposition
- [Architecture Overview](docs/architecture/OVERVIEW.md) — Technical architecture
- [Extension System](docs/architecture/EXTENSION_SYSTEM.md) — Unified extension architecture
- [Getting Started](docs/getting-started/GETTING_STARTED.md) — Installation and first steps
- [CLI Reference](docs/user-guide/CLI_REFERENCE.md) — Command reference

## Related Projects

- [Coneko](https://github.com/coneko/coneko) — Agent coordination network (TypeScript)
- [A2A Protocol](protocols/A2A_v0.1.md) — Agent-to-Agent messaging specification

---

*Built with 🐰 by the Coneko team*
