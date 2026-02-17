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
- ✅ **HTTP Tools** — Built-in web request capabilities
- ✅ **Security Sandbox** — Filesystem restrictions, command allowlisting
- ✅ **Cron/Heartbeat** — Scheduled task execution
- ✅ **Coneko Adapter** — Optional network integration

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
