# Peko 🐱

**Lightweight Multi-Peko Runtime**

Peko is a Rust-based multi-peko runtime: local AI pekos with DID identity, peko-to-peko messaging over channels, per-peer long-running threads, and a unified extension architecture. Peko is the only top-level runtime actor (ADR-041); roles are thin Markdown templates inside a peko. Sessions are an internal storage noun (ADR-042) and are not surfaced in the CLI.

> **Version:** 0.1.0 | **License:** MIT
>
> **Terminology:** A **peko** is the only top-level actor (ADR-041) — the user-facing name for what the code internally calls a `Principal` (ADR-059). Sessions are internal storage (ADR-042). Disambiguation: **the Peko runtime** (project/daemon/binary) · **a peko** (one actor) · **a `.peko` package** (portable archive) · **the PEKO model** (architecture). See also the [terminology map](docs/architecture/adr/ADR-042-no-external-session-concept.md#5-terminology-map-canonical-reference) and [ADR-059](docs/architecture/adr/ADR-059-peko-as-user-facing-term.md).

## Philosophy

- **Lightweight** — Small binary, fast startup
- **Peko-centric** — A peko is the single top-level actor; agents are thin prompts inside it
- **Secure** — ed25519 identity, DID-based addressing
- **Extensible** — Unified hook-based extension system
- **Daemon-first** — The CLI is a thin client; all execution happens in the daemon

## Features

### Core Architecture
- ✅ **DID Identity System** — ed25519-based decentralized identifiers
- ✅ **Peko-to-Peko Messaging** — DM and group channels, mirrored across runtimes over the pekohub tunnel
- ✅ **Peko Orchestration** — Top-level AI actors that own memory, intent, and governance
- ✅ **Per-Peer Long-Running Threads** — Each `(peko, peer)` pair keeps a long-running thread; the runtime owns lifecycle (no CLI surface)
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
- ✅ **Workspace Knowledge Base** — Each peko keeps a `kb/` workspace (MEMORY.md + topic files) it reads and curates itself
- ✅ **JSONL Conversation Store** — Per-session paged event logs; peer threads are read via `peko log`

### Scheduling & Execution
- ✅ **Cron/Daemon** — Scheduled task execution with daemon mode
- ✅ **Workspace Hooks** — Shell commands wired into prompt/session lifecycle points (ADR-047 §5)

### Security & Portability
- ✅ **Capability Gating** — Fail-closed per-peko tool/skill grants (ADR-046/047)
- ✅ **Portable pekos** — Export/import pekos as `.peko` packages

---

## Quick Start

### Prerequisites

Set your LLM provider API key:

```bash
export OPENAI_API_KEY="your-key"  # or ANTHROPIC_API_KEY, KIMI_API_KEY, etc.
```

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

# Create a peko (default model is the catalog default)
./target/release/peko create myprincipal

# Send a message to a peko (primary interaction method)
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
-U, --user <USER>       # Caller Subject for `peko send` / `peko log` (peer axis on a peko's thread)
```

### Commands

#### Peko Management
```bash
peko create <NAME>                       # Create a peko
peko create <NAME> -s <SEED.toml>        # Grow a peko from a seed
peko list [--long]                        # List all pekos
peko show <NAME>                          # Show peko details
peko export <NAME> [--output <PATH>]      # Export to .peko package
peko import <FILE> [--name <NEW_NAME>]    # Import from .peko package
peko permit <NAME> <SUBJECT> <PERMISSION> # Grant permission
peko revoke <NAME> <SUBJECT> <PERMISSION> # Revoke permission
```

> **Note:** There is no top-level `peko agent` or `peko team` command tree. Roles are thin Markdown templates in a peko's workspace (`roles/<name>.md`); teams were removed in favor of peko-to-peko interaction. Roles are listed via `peko show <NAME>` and managed as files (ADR-050, ADR-064).

#### Talk to a Peko (Primary Interaction)
```bash
peko send <PEKO> [MESSAGE]                    # Post to your thread; streams the reply
peko send <PEKO> "…" --wait                   # If busy: queued — block for the reply
peko send <PEKO> --file <PATH>                # Send message from file
peko send <PEKO> --stdin                      # Read message from stdin
peko stop <PEKO>                              # Soft-stop the running turn (idempotent)
peko log <PEKO>                               # Read the thread
peko log <PEKO> --watch                       # Follow the thread live
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

# 3. Create a peko — it inherits the catalog default model
peko create alice

# Inspect / manage the catalog and vault
peko model list
peko model show openai-gpt-4o
peko model compare openai-gpt-4o claude-sonnet-4-5
peko credential list --namespace llm
peko model test openai-gpt-4o

# PekoHub login (separate flow)
peko login --api-key ph_xxx --registry https://hub.example.com
peko logout
```

#### Extension Management

> **Retired.** The `peko ext *` command tree (ADR-047) and the
> per-category `peko tool|skill|mcp|hook|agent|persona` CLI
> (ADR-050) are both gone. Tooling is plain files in the peko's
> workspace — manage it with your editor and the filesystem:
>
> ```bash
> ls ~/.peko/principals/<name>/{roles,skills,mcp,hooks,workflows}/   # list
> cp -r ./my-skill ~/.peko/principals/<name>/skills/                 # install
> rm -r ~/.peko/principals/<name>/skills/my-skill                    # remove
> ```
>
> See [Peko Workspace](docs/architecture/PRINCIPAL_WORKSPACE.md).

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

> **Note:** Advanced commands (`config`, `runtime`, `tunnel`, `vault`, and `auth apikey`) are hidden from `--help` because they expose operational internals. They remain functional for operators and scripts.

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
(`peko send` to a peko will surface these as filterable notes).

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

## Capabilities as Workspace Files

A peko's capabilities are plain files in its workspace — presence in the
directory is what makes them visible to the model (ADR-050); no install
step, no restart:

| Capability | Form | Purpose |
|-----------|------|---------|
| **Roles** | `roles/<name>.md` | Thin Markdown persona templates for subagents (ADR-064) |
| **Skills** | `skills/<name>/SKILL.md` | Documentation-driven agent capabilities |
| **MCP Servers** | `mcp/` | External tool server integration |
| **Hooks** | `hooks/<id>/hook.toml` | Shell commands bound to lifecycle hook points |
| **Workflows** | `workflows/*.py` | Agent-authored Python automation (ADR-061) |
| **Built-in Tools** | Native code | Core runtime tools (fs, shell, cron, channels, …) |

> **Sprint 9** retired the `gateway` extension type (chat-platform
> adapters like Discord/Slack); **ADR-062** retired the
> `universal-tool` type — external code reaches the catalog via MCP
> servers or `workflows/*.py`.

### Hook Points

Extensions and workspace hooks bind into the agentic loop at these points
(see `peko-rs/core/src/extensions/framework/core/hook_points.rs`):

- **Prompt**: `PromptSystemSection`, `SessionContextBuild`
- **Tool**: `ToolRegister`, `PreToolUse`, `ToolExecute`, `ToolExecuteAsync`, `ToolCheckStatus`, `ToolCancel`, `PostToolUse`
- **Session**: `SessionStart`, `SessionStateChange`, `SessionCompaction`, `SessionCompactionPost`
- **Lifecycle**: `AgentInit`, `Stop`, `AfterAgent`, `AgentShutdown`
- **Events**: `EventSubscribe`

Learn more: [peko Workspace Documentation](docs/architecture/PRINCIPAL_WORKSPACE.md) (ADR-047 — replaces the extension framework)

---

## Portable Pekos

Export pekos as `.peko` packages and import them on other machines:

```bash
# Export a peko to a .peko package
peko export my-principal --output ./my-principal.peko

# Import a peko
peko import ./my-principal.peko --name imported-principal
```

**Package Contents:**
- Configuration and identity (DID document and exported private keys)
- Roles, skills, MCP, hooks, knowledge base, sessions, cron, and plans
- Flat `manifest.toml` inventory with SHA-256 checksums

**Security:**
- Import validates every payload checksum and rejects legacy OCI snapshots
- Hook/MCP commands and skill ids are printed and audited before restore
- Inspect snapshots with `tar -tf`; ground plain seed TOML with `peko create -s`

---

## Daemon Mode

The daemon is a long-running process that owns pekos, executes `send`
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

Scheduled jobs are managed by the peko itself via the
`tool:Cron{Create,List,Delete}` tools (gated by the peko's
`tool:*` grants). Operators interact with schedules by sending a
message to the peko that owns them.

---

## Configuration

Most users never edit a config file — `peko model add --key ...` writes
the model catalog and vault, and the peko lifecycle verbs write the rest.
The two files that exist, and what actually reads them:

- `~/.peko/peko.toml` — daemon config; only the `[provider.retry]` block
  is consumed (LLM transport retry knobs).
- `~/.peko/config.toml` — the `[compaction]` block (session compaction
  tuning) plus scratch space for the hidden `peko config get/set` CLI.

See [`config.example.toml`](config.example.toml) for the annotated
reference.

The daemon's IPC/HTTP bind is a loopback constant (`127.0.0.1:11435`) — there is
no `bind_address` knob (ADR-058 D6 removed the inert one); remote daemon
access is not a supported feature.

Model selection is catalog-driven — pick the default model via the
catalog (`peko model list` to see what's wired). Per-send overrides use
`peko send --model <id>`.

---

## Architecture

### Source Structure

Cargo workspace; all implementations live under `peko-rs/`.

```
peko-rs/
├── core/               # The peko facade crate: principal/agent/daemon/IPC/
│   │                   # registry/tunnel/extensions/tools/observability
│   ├── src/principal/  # Runtime-coupled principal layer
│   ├── src/extensions/ # Extension framework + role/skill/mcp adapters
│   ├── src/daemon/     # Axum HTTP + WS daemon, cron runtime
│   └── src/tools/      # Built-in tool implementations
├── cli/                # peko bin — clap parser + service delegates
├── engine/             # Agentic loop core + prompt renderer + compaction driver
├── session/            # JSONL session storage, ownership, paths, compaction
├── channel/            # Multi-party channels (store, subscribers, cursors)
├── cron/               # Cron scheduler + Cron* tools
├── providers/          # LLM provider catalog, resolver, adapters
├── message/            # Neutral message contract (leaf)
├── subject/            # Subject/PrincipalId types (leaf)
├── tools-core/         # Tool API traits (leaf)
├── events/             # Neutral agentic event contract (leaf)
├── protocol/           # IPC + tunnel wire contracts (serde only)
├── auth/  identity/  quota/  plan/  observability/  fs-persistence/
├── provider-api/       # Provider contract types
└── peko-daemon/        # peko-daemon binary
```

See [AGENTS.md](AGENTS.md) §3 for the full member table and the module
boundary rules enforced in CI.

### Key Architectural Decisions

- **Thin CLI (ADR-021)**: The CLI is a thin client — all execution happens in the daemon
- **Capabilities as workspace files (ADR-050)**: presence = visibility; no install step
- **Filesystem-First**: JSONL event logs are the conversation source of truth; all state on disk

---

## Development

```bash
# Run tests
cargo test

# Run with logging
RUST_LOG=debug cargo run -- list

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

- [Getting Started](docs/getting-started/GETTING_STARTED.md) — Build and run your first peko
- [Tutorial: Building Your First Peko](docs/getting-started/TUTORIAL_BUILDING_FIRST_AGENT.md) — Step-by-step walkthrough
- [User's Guide](docs/user-guide/USERS_GUIDE.md) — Concepts, sessions, principals, workspace tooling
- [CLI Reference](docs/user-guide/CLI_REFERENCE.md) — Every `peko` command and flag
- [peko Workspace](docs/architecture/PRINCIPAL_WORKSPACE.md) — Per-peko tooling layout (ADR-047)
- [PEKO Primitive](docs/architecture/PEKO.md) — Canonical term: Persistent Entity with Keepalive Orchestration
- [Agent–Session Paradigm](docs/architecture/AGENT_SESSION_PARADIGM.md) — Full design rationale, gap audit, build order
- [Architecture Decision Records](docs/architecture/adr/) — ADR-001 through ADR-064
- [MCP Overview](docs/mcp/MCP.md) — Model Context Protocol integration
- [Agent Guide](AGENTS.md) — Build, test, code-style rules for contributors
- [API Surface](API_SURFACE.md) — Public Rust API contracts
- [Data Model](DATA_MODEL.md) — On-disk and in-memory data formats

---

*Built with 🐰 by the Coneko team*
