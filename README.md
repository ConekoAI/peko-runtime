# Peko 🐱

**Persistent AI agents you own.**

Peko is a runtime for **pekos**: AI agents that keep their identity, memory
and ongoing work between conversations, and run in the background on your
own machine. A peko is a directory of plain files you can read, edit, back
up and move. It runs on any model you bring, and it talks with people and
other pekos over channels.

> **Version:** 0.1.0 | **License:** MIT
>
> **Terminology:** a **peko** is one agent you create and talk to; the code
> calls it a `Principal` (ADR-059), which is why `principal.toml` and
> `~/.peko/principals/` keep that name. A **role** is a Markdown template in
> `roles/`; an **agent** is a live run of the model working as a role
> (ADR-064). **Peko** is the project, daemon and binary; a **`.peko` package**
> is a portable snapshot of one peko.

## Features

- **Yours, as files.** Identity, definition (`principal.toml`), roles,
  memory (`kb/`), rules (`POLICY.md`) and tooling are files in one
  directory. `peko export` / `peko import` move a peko with its identity.
- **Any model.** Add any endpoint that speaks the Anthropic Messages, OpenAI
  Chat Completions or OpenAI Responses wire format, hosted or local. Keys
  live in an encrypted vault.
- **Memory that names its sources.** A nightly pass journals the day and
  curates `kb/MEMORY.md`; a reviewer model checks each sourced line before
  it is written (ADR-080, ADR-085).
- **Background work without idle cost.** Cron jobs, check-ins on open plans,
  and a keepalive that calls no model when nothing is waiting (ADR-083,
  ADR-084).
- **Conversations with people and pekos.** Direct and group channels, mirrored
  across runtimes through PekoHub with signed, durable delivery (ADR-072,
  ADR-082).
- **Compartments and a trust floor.** Each conversation sees only itself; runs
  for other people get a reduced tool surface and a policy reviewer
  (ADR-078).
- **A ledger of every tool call.** Hash-chained per peko; read it with
  `peko audit ledger` and check it with `peko audit verify` (ADR-079).
- **Tooling as files.** Roles, skills, MCP servers, hooks and workflows are
  files in the workspace; there is no install step (ADR-050).

---

## Quick start

Supported platforms: macOS (Apple silicon or Intel) and Linux (x86_64 or
arm64, glibc 2.35+). Windows is not supported in this release.

```bash
curl -fsSL https://raw.githubusercontent.com/ConekoAI/peko-runtime/master/install.sh | bash
```

Before the first stable release, name a release candidate:
`... | bash -s -- --version 0.1.0-rc.2`. Then:

```bash
peko model add --id my-model --api-format anthropic_messages \
    --base-url https://llm.example.com/anthropic --model <wire-model-id> \
    --key "<your-api-key>"
peko daemon start
peko create my-peko --model my-model
peko send my-peko "Hello, what can you do?"
```

[Getting Started](docs/getting-started/GETTING_STARTED.md) walks through each
step with real output.

Optional: Python 3 (`python3` on `PATH`) for the `Workflow` tool, which runs
workflows the peko writes. Without it, `Workflow` returns an error and
everything else works.

### Build from source

```bash
git clone https://github.com/ConekoAI/peko-runtime
cd peko-runtime
cargo build --release -p peko-cli -p peko-daemon
# Binaries: target/release/peko and target/release/peko-daemon.
# Keep them in the same directory: peko starts the daemon from the binary beside it.
```

---

## CLI reference

The full reference is [CLI_REFERENCE.md](docs/user-guide/CLI_REFERENCE.md);
`peko <command> --help` documents every flag.

### Global flags

```bash
--config-dir <PATH>     # Override config directory (env: PEKO_CONFIG_DIR)
--data-dir <PATH>       # Override data directory (env: PEKO_DATA_DIR)
--cache-dir <PATH>      # Override cache directory (env: PEKO_CACHE_DIR)
--json                  # Output results as JSON
-q, --quiet             # Suppress non-error output
-v, -vv, -vvv           # Verbose logging (repeat for more)
--debug                 # Show debug information including stack traces
```

### Pekos

```bash
peko create <NAME> --model <MODEL_ID>     # Create a peko and wait for its first turn
peko create <NAME> -s <SEED.toml>         # Grow a peko from a seed
peko list                                 # List pekos
peko show <NAME>                          # Identity, model, roles, tool catalog
peko export <NAME> [-o <PATH>]            # Snapshot to a .peko package
peko import <FILE> [--name <NEW_NAME>]    # Restore from a .peko package
peko remove <NAME>                        # Delete a peko and all its data
peko permit <NAME> <ADDRESS> <PERMISSION> # Grant a permission (chat, manage_settings, trusted)
peko revoke <NAME> <ADDRESS> <PERMISSION> # Revoke it
peko permissions <NAME>                   # List permissions
peko invite <NAME>                        # Mint a one-person invite link
```

Roles are Markdown files in a peko's `roles/`; `peko show` lists them.
There is no `peko agent` command (ADR-050, ADR-064).

### Talk to a peko

```bash
peko send <PEKO> [MESSAGE]                # Post to your conversation; streams the reply
peko send <PEKO> "…" --wait               # If it is busy, block for the reply
peko send <PEKO> --file <PATH>            # Message from a file
peko send <PEKO> --stdin                  # Message from stdin
peko stop <PEKO>                          # Stop the turn it is running for you
peko log <PEKO>                           # Read the conversation
peko log <PEKO> --watch                   # Follow it live
```

### Models and keys

```bash
peko model add --id my-model --api-format anthropic_messages \
    --base-url https://llm.example.com/anthropic --model <wire-model-id> \
    --key "<your-api-key>"
peko model add --no-key --id my-local --api-format openai_completions \
    --base-url http://localhost:8080/v1 --model llama-3.1-8b
peko model test my-model
peko credential list --namespace llm
```

#### Model Management
```bash
peko model list                                   # List configured models
peko model list --detailed                        # Include the per-model note column
peko model show <MODEL_ID>                        # Detail view (incl. spec + note)
peko model compare <MODEL_ID>...                  # Side-by-side capability matrix
peko model search --vision --tools --thinking     # Filter by capability predicate
peko model search --contains cron                 # Substring-match id, display_name, note
peko model add --api-format <fmt> --base-url <url> --model <wire-id>  # Add a model
peko model edit <id> --note "very cheap, use it for cron"  # Free-text annotation for the agent
peko model edit <MODEL_ID> --note "..."           # Update note; --note "" clears it
peko model remove <MODEL_ID>                      # Remove a model from the catalog
peko model test <MODEL_ID>                        # Live-test a model
```

The `note` field on each catalog entry is the standardized way to
express subjective quality or routing intent that spec flags cannot
capture. Parent agents can read it via the `ModelList` builtin tool
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

#### Cost controls

Set per-spawn and rolling-cycle ceilings in `principal.toml`:

```toml
[quota]
cost_per_call_max = 0.50   # USD; spawn-time pre-flight refuses expensive picks
budget_per_cycle = 50.00   # USD; rolling cycle cap
```

`cost_per_call_max` is checked when an agent starts, against a 4K-in +
1K-out token projection at the model's price; `budget_per_cycle` is checked
during the run. `peko quota status <NAME>` shows current spend.

### Daemon, PekoHub and system

```bash
peko daemon start                         # Start the background daemon
peko daemon status
peko daemon restart
peko daemon stop

peko runtime register --api-key <KEY>     # Link this runtime and its local user to PekoHub
peko register <NAME> [--public]           # Put a peko online (private by default)
peko unregister <NAME>

peko system status | info | doctor | clean
peko audit ledger <NAME>                  # The peko's tool ledger
peko update [--check]
peko completions <bash|zsh|fish|powershell>
```

`runtime`, `config`, `tunnel` and `vault` are advanced commands hidden from
`--help`.

---

## Workspace tooling

A peko's tooling is plain files in its workspace (ADR-050). There is no
install command: add a file to add a capability.

| Kind | Form | Purpose |
|------|------|---------|
| **Roles** | `roles/<name>.md` | Templates agents run as (ADR-064) |
| **Skills** | `skills/<name>/SKILL.md` | Instructions the peko loads when a task calls for them |
| **MCP servers** | `mcp/<id>/server.json` | External tool servers (ADR-075) |
| **Hooks** | `hooks/<id>/hook.toml` | Handlers for workspace hook points |
| **Workflows** | `workflows/*.py` | Scripts the peko writes for the `Workflow` tool (ADR-061) |
| **Built-in tools** | Native code | 23 tools: files, shell, agents, sessions, channels, plans, cron, MCP, ledger |

New roles and skills appear on the peko's next turn, and so do edits to the
root role (`roles/root.md`) and `principal.toml`.

Workspace hooks bind to six points: `PreToolUse`, `PostToolUse`, `Stop`,
`AfterAgent`, `PromptSection` and `SessionContextBuild`. Handlers run in
registration order with a two-second budget each and cannot veto a call
(AGENTS.md §6.4).

See [Peko workspace](docs/architecture/PRINCIPAL_WORKSPACE.md) for the full
layout. Discord/Slack gateway adapters (sprint 9) and universal tools
(ADR-062) were retired; external code reaches a peko through MCP servers or
workflows.

---

## Portable pekos

```bash
peko export my-peko                       # writes ./my-peko.peko
peko import my-peko.peko                  # on another machine
```

A package holds the peko's identity (DID document and keys), configuration,
roles, skills, MCP servers, hooks, knowledge base, sessions, cron schedule
and plans, with a flat `manifest.toml` of SHA-256 checksums. Import checks
every checksum, prints hook and MCP commands and skill ids before restoring,
and audits them. A runtime holds one copy of a peko, so importing it where it
already lives is refused.

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

The daemon has no HTTP server. Clients talk to it over IPC: a Unix datagram
socket at `$PEKO_HOME/run/daemon.sock` (a named pipe on Windows), with UDP on
the loopback constant `127.0.0.1:11435` as the fallback — there is no
`bind_address` knob (ADR-058 D6 removed the inert one); remote daemon
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
│   ├── src/daemon/     # Daemon state, channel bindings, cron engine + runtime
│   ├── src/ipc/        # IPC server + handlers + packet types (the client surface)
│   ├── src/security/   # Trust floor, policy gate, tool ledger (ADR-078/079)
│   └── src/tools/      # Built-in tool implementations
├── cli/                # peko bin — clap parser + service delegates
├── engine/             # Agentic loop core + prompt renderer + compaction driver
├── session/            # JSONL session storage, ownership, paths, compaction
├── channel/            # Multi-party channels (store, subscribers, cursors)
├── cron/               # Cron scheduler + the Cron tool
├── providers/          # LLM provider catalog, resolver, adapters
├── message/            # Neutral message contract (leaf)
├── subject/            # ActorRef / PrincipalId types (leaf)
├── tools-core/         # Tool API traits (leaf)
├── events/             # Neutral agentic event contract (leaf)
├── protocol/           # IPC + tunnel wire contracts (serde only)
├── auth/  identity/  quota/  plan/  observability/  fs-persistence/
├── provider-api/       # Provider contract types
└── peko-daemon/        # peko-daemon binary
```

CI enforces the module boundaries with `scripts/check_module_boundaries.sh`
and the crate dependency graph with `scripts/check_workspace_deps.py`.

### Key Architectural Decisions

- **Thin CLI (ADR-021)**: The CLI is a thin client — all execution happens in the daemon
- **Capabilities as workspace files (ADR-050)**: presence = visibility; no install step
- **Filesystem-First**: JSONL event logs are the conversation source of truth; all state on disk

---

## Development

```bash
cargo test                       # full suite
cargo test --lib                 # unit tier
RUST_LOG=debug cargo run -- list
cargo fmt --all
cargo clippy --all-targets -- -D warnings
```

See [AGENTS.md](AGENTS.md) for the CI tiers and module-boundary rules.

---

## Documentation

- [Getting Started](docs/getting-started/GETTING_STARTED.md): install and talk to your first peko
- [Tutorial: shaping your first peko](docs/getting-started/TUTORIAL_FIRST_PEKO.md): persona, memory, a reminder, a skill, backup
- [User's Guide](docs/user-guide/USERS_GUIDE.md): concepts, configuration, tooling, limits, troubleshooting
- [CLI Reference](docs/user-guide/CLI_REFERENCE.md): every command
- [Peko workspace](docs/architecture/PRINCIPAL_WORKSPACE.md): the workspace layout
- [PEKO primitive](docs/architecture/PEKO.md): Persistent Entity with Keepalive Orchestration
- [Agent–session paradigm](docs/architecture/AGENT_SESSION_PARADIGM.md): design rationale
- [Architecture Decision Records](docs/architecture/adr/)
- [MCP overview](docs/mcp/MCP.md)
- [AGENTS.md](AGENTS.md): build, test and code-style rules for contributors
- [API surface](API_SURFACE.md) and [data model](DATA_MODEL.md)

---

## License

MIT

*Built with 🐱 by the Coneko team*
