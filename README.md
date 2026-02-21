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

- ✅ **DID Identity System** — ed25519-based decentralized identifiers
- ✅ **A2A Protocol** — Full Agent-to-Agent messaging (12 message types)
- ✅ **Multi-Agent Orchestration** — Local registry and message routing
- ✅ **SQLite Memory** — Persistent memory with semantic search
- ✅ **15 LLM Providers** — OpenAI, Anthropic, Kimi, OpenRouter, and more
- ✅ **7 Communication Channels** — CLI, HTTP, Telegram, Discord, Slack, Matrix, WhatsApp
- ✅ **Calendar Integration** — Google Calendar and Outlook support
- ✅ **Document Processing** — PDF text extraction, OCR, invoice parsing
- ✅ **Social Media** — Twitter/X and LinkedIn posting and scheduling
- ✅ **Inventory Management** — Shopify and WooCommerce monitoring with alerts
- ✅ **Expense Tracking** — Receipt OCR, categorization, and accounting exports
- ✅ **Email Assistant** — Gmail and Outlook integration with smart replies
- ✅ **Research Agent** — Web search with source assessment and citations
- ✅ **HTTP Tools** — Built-in web request capabilities
- ✅ **Security Sandbox** — Filesystem restrictions, command allowlisting
- ✅ **Cron/Heartbeat** — Scheduled task execution
- ✅ **Coneko Adapter** — Optional network integration
- ✅ **Portable Agents** — Export/import agents as `.agent` packages

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

### CLI Commands

```bash
# Agent management
pekobot agent [OPTIONS]
  --name <NAME>           Agent name
  --memory                Enable SQLite memory
  --provider <TYPE>       LLM provider (openai)
  --model <MODEL>         Model name
  --coneko <URL>          Coneko server URL
  --coneko-token <TOKEN>  Coneko auth token

# Orchestrator
pekobot orchestrate [OPTIONS]
  --config <FILE>         Configuration file

# Coneko integration
pekobot coneko status --endpoint <URL>
pekobot coneko register --did <DID> --name <NAME> --agent-endpoint <URL>
pekobot coneko discover --endpoint <URL> [--capability <CAP>]

# Identity
pekobot identity create --name <NAME> [--scope local|tenant|global]
pekobot identity list
pekobot identity resolve <DID>

# Utilities
pekobot --version
pekobot --help
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

```
pekobot/
├── agent/        # Agent management and orchestration
├── a2a/          # A2A Protocol implementation
├── channels/     # Communication channels (CLI, HTTP)
├── coneko/       # Optional Coneko network adapter
├── config/       # TOML/JSON configuration
├── identity/     # DID identity and ed25519 keys
├── memory/       # SQLite persistence
├── providers/    # LLM provider integrations
├── tools/        # Agent tools (HTTP, etc.)
└── types/        # Core type definitions
```

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
pekobot export --agent my-agent --output ./my-agent.agent

# Export with encryption (recommended for sharing)
pekobot export --agent my-agent --output ./my-agent.agent --encrypt

# Import an agent
pekobot import --file ./my-agent.agent --name imported-agent

# Import with key rotation (new DID)
pekobot import --file ./my-agent.agent --name imported-agent --rotate-keys

# Inspect a package without importing
pekobot inspect --file ./my-agent.agent
```

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

## Related Projects

- [Coneko](https://github.com/coneko/coneko) — Agent coordination network (TypeScript)
- [A2A Protocol](protocols/A2A_v0.1.md) — Agent-to-Agent messaging specification

---

*Built with 🐰 by the Coneko team*
