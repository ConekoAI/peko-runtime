# Peko 🐱

**Lightweight Multi-Principal Runtime**

Peko is a Rust-based multi-principal runtime: local AI Principals with DID identity, A2A protocol messaging, per-peer long-running memory, and a unified extension architecture. Principal is the only top-level runtime actor (ADR-041); agent prompts are thin Markdown files inside a Principal. Sessions are an internal storage noun (ADR-042) and are not surfaced in the CLI.

> **Version:** 0.1.0 | **License:** MIT
>
> **Terminology:** Principal is the only top-level actor (ADR-041). Sessions are internal storage (ADR-042). See the [terminology map](docs/architecture/adr/ADR-042-no-external-session-concept.md#5-terminology-map-canonical-reference) for the public vs. internal noun glossary.

## Philosophy

- **Lightweight** — Small binary, fast startup
- **Principal-centric** — A Principal is the single top-level actor; agents are thin prompts inside it
- **Secure** — ed25519 identity, DID-based addressing
- **Extensible** — Unified hook-based extension system
- **Daemon-first** — The CLI is a thin client; all execution happens in the daemon

## Features

### Core Architecture
- ✅ **DID Identity System** — ed25519-based decentralized identifiers
- ✅ **A2A Protocol** — Agent-to-Agent messaging between Principals
- ✅ **Principal Orchestration** — Top-level AI actors that own memory, intent, and governance
- ✅ **Per-Peer Long-Running Memory** — Each `(Principal, peer)` pair keeps a long-running thread; the runtime owns lifecycle (no CLI surface)
- ✅ **Event Router** — Central event routing and subscription system

### LLM & Providers
- ✅ **15+ LLM Providers** — OpenAI, Anthropic, Kimi, OpenRouter, and more
- ✅ **Streaming Output** — Real-time progressive output with tool visibility

### Tools & Extensions
- ✅ **MCP Support** — Model Context Protocol for external tool integration
- ✅ **Skills System** — Documentation-driven capabilities (SKILL.md)
- ✅ **Built-in Tools** — Filesystem, shell, cron, messaging, task management
- ✅ **Unified Extension Architecture** — Hook-based extension points for maximum composability

### Memory & Persistence
- ✅ **SQLite Memory** — Persistent memory with semantic search
- ✅ **Per-Peer JSONL Memory** — Threaded conversation history partitioned by `(Principal, peer)`. Internal storage; read via `peko log`.

### Scheduling & Execution
- ✅ **Cron/Daemon** — Scheduled task execution with daemon mode
- ✅ **Event Triggers** — React to file changes, webhooks, and system events

### Security & Portability
- ✅ **Security Sandbox** — Filesystem restrictions, command allowlisting
- ✅ **Portable Principals** — Export/import Principals as `.principal` packages

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
git clone https://github.com/ConekoAI/peko-runtime
cd peko-runtime

# Build
cargo build --release

# The binary will be at:
./target/release/peko
```

### Basic Usage

```bash
# Add a model to the catalog (only needed once; pick a template + wire id)
./target/release/peko model add --template openai --model gpt-4o --key "$OPENAI_API_KEY"

# Create a Principal (default model is the catalog default)
./target/release/peko principal create myprincipal

# Send a message to a Principal (primary interaction method)
./target/release/peko send myprincipal "Hello, what can you do?"

# Send from a file or stdin
echo "Hello" | ./target/release/peko send myprincipal --stdin
./target/release/peko send myprincipal --file prompt.txt

# Check version
./target/release/peko --version
```

---

## CLI Reference

Peko uses a hierarchical command structure (`peko <noun> <verb>`).

### Global Flags

```bash
--config-dir <PATH>     # Override config directory (env: PEKO_CONFIG_DIR)
--data-dir <PATH>       # Override data directory (env: PEKO_DATA_DIR)
--cache-dir <PATH>      # Override cache directory (env: PEKO_CACHE_DIR)
--json                  # Output results as JSON
-q, --quiet             # Suppress non-error output
-v, -vv, -vvv           # Verbose logging (repeat for more)
--debug                 # Show debug information including stack traces
-U, --user <USER>       # Caller Subject for `peko send` / `peko log` (peer axis on a Principal's thread)
```

### Commands

#### Principal Management
```bash
peko principal create <NAME>                       # Create a Principal
peko principal list [--long]                        # List all Principals
peko principal show <NAME>                          # Show Principal details
peko principal export <NAME> [--output <PATH>]      # Export to .principal package
peko principal import <FILE> [--name <NEW_NAME>]    # Import from .principal package
peko principal push <NAME>:<TAG>                    # Push to registry
peko principal pull <REF>                           # Pull from registry
peko principal permit <NAME> <SUBJECT> <PERMISSION> # Grant permission
peko principal revoke <NAME> <SUBJECT> <PERMISSION> # Revoke permission
peko principal agent list <NAME>                    # List agent prompts in a Principal
```

> **Note:** There is no top-level `peko agent` or `peko team` command tree. Agents are thin Markdown prompts inside a Principal; teams were removed in favor of Principal-to-Principal interaction.

#### Talk to a Principal (Primary Interaction)
```bash
peko send <PRINCIPAL> [MESSAGE]                    # Post to your thread; streams the reply
peko send <PRINCIPAL> "…" --wait                   # If busy: queued — block for the reply
peko send <PRINCIPAL> --file <PATH>                # Send message from file
peko send <PRINCIPAL> --stdin                      # Read message from stdin
peko stop <PRINCIPAL>                              # Soft-stop the running turn (idempotent)
peko log <PRINCIPAL>                               # Read the thread
peko log <PRINCIPAL> --watch                       # Follow the thread live
```

#### Authentication (v3: catalog + vault)
```bash
# 1. Add a model entry to the runtime catalog (`~/.peko/models.toml`)
peko model add --template openai --model gpt-4o
peko model add --custom --id my-local \
               --api-format openai_completions \
               --base-url http://localhost:8080 \
               --model llama-3.1-8b

# 2. Store the API key in the encrypted vault (one per model)
peko credential set llm openai-gpt-4o --kind api_key --material "$OPENAI_API_KEY"

# 3. Create a Principal — it inherits the catalog default model
peko principal create alice

# Inspect / manage the catalog and vault
peko model list
peko model show openai-gpt-4o
peko model compare openai-gpt-4o claude-sonnet-4-5
peko credential list --namespace llm
peko model test openai-gpt-4o

# PekoHub registry token (separate flow)
peko login --api-key ph_xxx --registry https://hub.example.com
peko logout
```

#### Extension Management

> **Deprecated as of ADR-047 (2026-08-25).** The `peko ext *` command
> tree was retired; tooling lives directly in the principal's workspace.
> Use the per-category CLI:
>
> ```bash
> peko principal tool list / install / remove
> peko principal skill list / install / remove
> peko principal mcp list / install / remove
> peko principal hook list / install / remove
> peko principal plugin list / install / remove
> ```
>
> The lines below are retained for historical reference.

```bash
peko ext install <PATH|URL>                       # Install an extension (deprecated)
peko ext list                                     # List installed extensions (deprecated)
peko ext enable <ID>                              # Enable an extension (deprecated)
peko ext disable <ID>                             # Disable an extension (deprecated)
peko ext uninstall <ID>                           # Uninstall an extension (deprecated)
peko ext info <ID>                                # Show extension info (deprecated)
peko ext bundle <PATH> [--output <PATH>]          # Bundle extension (deprecated)
peko ext config <ID>                              # Configure extension (deprecated)
peko ext validate <PATH>                          # Validate extension manifest (deprecated)
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

> **Note:** Advanced commands (`config`, `cron`, `registry`, `runtime`, `tunnel`, `vault`, and `auth apikey`) are hidden from `--help` because they expose operational internals. They remain functional for operators and scripts.

#### Model Management
```bash
peko model list                                   # List configured models
peko model list --detailed                        # Include the per-model note column
peko model show <MODEL_ID>                        # Detail view (incl. spec + note)
peko model compare <MODEL_ID>...                  # Side-by-side capability matrix
peko model search --vision --tools --thinking     # Filter by capability predicate
peko model search --contains cron                 # Substring-match id, display_name, note
peko model add --template <id> --model <wire-id>  # Add a model (catalog)
peko model add --note "very cheap, use it for cron"  # Free-text annotation for the agent
peko model edit <MODEL_ID> --note "..."           # Update note; --note "" clears it
peko model remove <MODEL_ID>                      # Remove a model from the catalog
peko model test <MODEL_ID>                        # Live-test a model
```

The `note` field on each catalog entry is the standardized way to
express subjective quality or routing intent that spec flags cannot
capture. Parent agents can read it via the `model_list` builtin tool
(`peko send` to a principal will surface these as filterable notes).

#### Cost Controls

Set per-spawn and rolling-cycle ceilings in `principal.toml`:

```toml
[quota]
cost_per_call_max = 0.50   # USD; spawn-time pre-flight refuses expensive picks
budget_per_cycle = 50.00   # USD; rolling cycle cap folds via QuotaMeter
```

`cost_per_call_max` runs at spawn time (4K-in + 1K-out token projection
× the chosen model's `PricingHint`); `budget_per_cycle` runs mid-stream
via `StackedMeteredProvider` and folds per-call cost alongside the
existing token/request counters. Refusals surface as a typed
`SpawnError::CostCeilingExceeded` before any LLM traffic. See
`peko quota list` to inspect current spend.

#### Update
```bash
peko update                                       # Update Peko
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

All capabilities — tools, skills, MCP servers, and channels — are implemented through a single, consistent hook-based system.

### Extension Types

| Extension | Type | Purpose |
|-----------|------|---------|
| **Skills** | `SKILL.md` | Documentation-driven agent capabilities |
| **MCP Servers** | `config.json` | External tool server integration |
| **Universal Tools** | `manifest.json` | Executable command-line tools |
| **Built-in Tools** | Native code | Core runtime tools |
| **Channels** | `CHANNEL.toml` | I/O adapters (CLI, HTTP, etc.) |
| **General Extensions** | `extension.yaml` | Multi-hook custom extensions |

> **Sprint 9** retired the `gateway` extension type (chat-platform
> adapters like Discord/Slack). External ingress now lands in per-peer
> standing children via the agent-session paradigm; `peko ext install`
> no longer accepts gateway manifests.

### Managing Extensions

> **Deprecated as of ADR-047 (2026-08-25).** The `peko ext *` flow was
> retired; tooling lives directly in the principal's workspace. Use:
>
> ```bash
> peko principal tool install <path>
> peko principal mcp install <path>
> peko principal hook install <path>
> ```
>
> The block below is retained for historical reference.

```bash
# Install any extension type (auto-detected) — deprecated
peko ext install ./my-skill
peko ext install ./mcp-server.json

# List all extensions — deprecated
peko ext list
# ID           TYPE      STATUS   HOOKS
# docker       skill     enabled  prompt:skills
# filesystem   mcp       enabled  prompt:tools, tool:*

# Enable/disable — deprecated
peko ext enable docker
peko ext disable docker
```

### The 22 Hook Points

Extensions hook into the agentic loop at 22 different points:

- **Prompt Hooks**: `PromptSystemSection` (deprecated for `section: "tools"` — F36 wire-only catalogs; supported sections: `skills`, `agents`, `mcp_context`), `PromptPreProcess`, `PromptPostProcess`
- **Tool Hooks**: `ToolRegister`, `ToolExecute`, `ToolExecuteAsync`, `ToolCheckStatus`, `ToolCancel`
- **Session Hooks**: `SessionStateChange`, `SessionCompaction`, `SessionContextBuild`
- **I/O Hooks**: `ChannelInput`, `ChannelOutput`, `MessagePreSend`, `MessagePostReceive`
- **Event Hooks**: `EventSubscribe`, `EventEmit`
- **Lifecycle Hooks**: `AgentShutdown`, `AgentIteration`

Learn more: [Principal Workspace Documentation](docs/architecture/PRINCIPAL_WORKSPACE.md) (ADR-047 — replaces the extension framework)

---

## Portable Principals

Export Principals as `.principal` packages and import them on other machines:

```bash
# Export a Principal to a .principal package
peko principal export my-principal --output ./my-principal.principal

# Import a Principal
peko principal import ./my-principal.principal --name imported-principal
```

**Package Contents:**
- Identity (DID document + encrypted keys)
- Configuration (allowed extensions, governance)
- Memory (SQLite database)
- Skills (bundled SKILL.md files)

**Security:**
- AES-256-GCM encryption with Argon2id key derivation
- Ed25519 signatures for package integrity
- Optional key rotation on import

---

## Daemon Mode

The daemon is a long-running process that owns Principals, executes `send`
requests, and polls for scheduled jobs.

```bash
# Start the daemon (foreground mode)
peko daemon start --foreground

# Check daemon status
peko daemon status

# Stop the daemon gracefully
peko daemon stop

# Restart the daemon
peko daemon restart
```

Scheduled jobs are managed by the Principal itself via the
`tool:Cron{Create,List,Delete}` tools (gated by the principal's
`tool:*` grants). Operators interact with schedules by sending a
message to the Principal that owns them.

---

## Configuration

Most users never need to edit `~/.peko/config.toml` directly — `peko model`
and `peko principal` write the required state. Operators who need low-level
can use the hidden `peko config` commands or edit the file by hand.

```toml
[daemon]
bind_address = "127.0.0.1:11435"
log_level = "info"
```

Model selection is now catalog-driven (PR 1 of `feature/model-first-config`).
The `[defaults]` block no longer exists — pick the default model via the
catalog (`peko model list` to see what's wired). Per-send overrides use
`peko send --model <id>`.

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
│   ├── general/        # General extension adapter
│   ├── mcp/            # MCP adapter
│   ├── skill/          # Skill adapter
│   └── universal/      # Universal tool adapter
├── identity/           # DID identity system, ed25519 keys, key storage, runtime identity
├── ipc/                # Inter-process communication
├── observability/      # Audit logging (pub(crate))
├── providers/          # LLM provider integrations (v3 catalog + resolver)
├── registry/           # `.principal` packaging/export/import and remote registry client
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
RUST_LOG=debug cargo run -- principal list

# Format code
cargo fmt

# Run clippy
cargo clippy
```

---

## Docker

```bash
# Build image
docker build -t peko:latest .

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
- [User's Guide](docs/user-guide/USERS_GUIDE.md) — Concepts, sessions, principals, workspace tooling
- [CLI Reference](docs/user-guide/CLI_REFERENCE.md) — Every `peko` command and flag
- [Principal Workspace](docs/architecture/PRINCIPAL_WORKSPACE.md) — Per-principal tooling layout (ADR-047)
- [PEKO Primitive](docs/architecture/PEKO.md) — Canonical term: Persistent Entity with Keepalive Orchestration
- [Agent–Session Paradigm](docs/architecture/AGENT_SESSION_PARADIGM.md) — Full design rationale, gap audit, build order
- [Architecture Decision Records](docs/architecture/adr/) — ADR-001 through ADR-042
- [MCP Overview](docs/mcp/MCP.md) — Model Context Protocol integration
- [Agent Guide](AGENTS.md) — Build, test, code-style rules for contributors
- [API Surface](API_SURFACE.md) — Public Rust API contracts
- [Data Model](DATA_MODEL.md) — On-disk and in-memory data formats

---

*Built with 🐰 by the Coneko team*
