# Peko User Guide

Welcome to the Peko User Guide! This guide will help you understand and use Peko effectively.

## Table of Contents

1. [What is Peko?](#what-is-peko)
2. [Installation](#installation)
3. [Core Concepts](#core-concepts)
4. [Running Your First peko](#running-your-first-peko)
5. [Configuration](#configuration)
6. [Extensions](#extensions)
7. [Known Limitations](#known-limitations)
8. [Troubleshooting](#troubleshooting)
9. [Next Steps](#next-steps)

---

## What is Peko?

Peko 🐱 is a lightweight multi-agent runtime written in Rust. It allows you to:

- Run autonomous AI pekos locally
- Connect pekos to any LLM endpoint that speaks the Anthropic Messages, OpenAI Chat Completions, or OpenAI Responses wire format (hosted or local)
- Manage persistent conversation memory automatically
- Orchestrate tools, skills, MCP servers, and gateways through a unified extension system

### Key Features

| Feature | Description |
|---------|-------------|
| **Lightweight** | Small binary, starts quickly |
| **Standalone** | Works without external services |
| **Peko-Centric** | A peko is the top-level actor you create and chat with |
| **Persistent Memory** | JSONL-based session storage managed automatically per peko |
| **Owner-Root Activity Feed** | `peko log <PRINCIPAL>` reads the owner's thread — including cron wakes and async completions — without exposing the session abstraction |
| **DID Identity** | ed25519-based decentralized identifiers |
| **Extensions** | Unified Extension Architecture for skills, MCP, tools, channels, hooks |
| **Cron Scheduling** | Schedule recurring and one-time tasks, scoped to a peko and run as its owner (operator/advanced) |

---

## Installation

### Prerequisites

- Linux (x64, arm64) or macOS (x64, arm64). Windows is not supported in the
  first release.
- Rust 1.70+ (install from [rustup.rs](https://rustup.rs))
- API key for at least one LLM provider
- Optional: Python 3 (`python3` on PATH) for the `Workflow` tool, which runs
  workflows the peko writes. Without it, `Workflow` returns an error and
  everything else works.

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

### Peko

A **Peko** is the top-level AI actor in Peko. It owns:

- **Identity** — A unique DID (decentralized identifier)
- **Configuration** — Settings including capability grants, provider hints, and governance
- **Memory** — Conversation history stored as JSONL, managed automatically per peer
- **Agent Prompts** — Thin Markdown files that shape the peko's behavior
- **Extensions** — Allowed tools, skills, MCP servers, and gateways

You interact with a peko through `peko send` (post to your
thread), `peko stop` (halt the running turn), and `peko log` (read or
follow the thread).

### DID (Decentralized Identifier)

Every peko gets a unique identifier like:

```
did:peko:local:default:abc123def456
```

Format: `did:peko:{scope}:{tenant}:{identifier}`

- **Scope**: `local`, `tenant`, or `global`
- **Tenant**: Organization or namespace
- **Identifier**: Unique hash-based ID

### Session

A Session is the implementation noun for one peer's running conversation
with a peko. Sessions are stored as JSONL files under the
peko's workspace, created automatically when a peer first sends,
resumed on every subsequent send from the same peer, and compacted
when context pressure demands. Sessions are an internal storage
detail — they are deliberately **not** exposed as a CLI command:

- `peko send <PRINCIPAL> "…"` — drive a conversation. If a turn is
  already running on the thread, the message is queued and folds into
  it at the next step; `--wait` blocks for the reply.
- `peko stop <PRINCIPAL>` — soft-stop the running turn on your thread
  (idempotent: nothing running → friendly notice, exit 0).
- `peko log <PRINCIPAL>` — read the **owner-root view** of activity
  (the conversation running on the owner's behalf, plus any background
  work the owner is entitled to see: cron wakes, async-task
  completions, etc.). `--watch` follows the thread live.
- `peko log <PRINCIPAL> --peer <X>` — read peer `X`'s thread. Only
  peer `X` itself or the peko's owner can request this; other
  peers cannot snoop on each other (see ADR-042 for the full
  privacy contract). The same rule governs `peko stop --peer <X>` and
  `peko log --watch --peer <X>`.

**There is no `peko session` command and there will never be one.** The
Peko is an integral actor; sessions are storage, not state that
the user names, lists, or routes through. Operators who need to inspect
raw session files read them directly from the peko's workspace.

#### Privacy gate (ADR-042)

The default `peko log <PRINCIPAL>` invocation (no `--peer`) returns
the **owner-root view**, which is special: it is only readable by the
peko's owner. The privacy contract is enforced strictly, and
applies identically to `peko log --watch` and `peko stop`:

| Caller | Command | Result |
|---|---|---|
| peko's owner | `peko log <P>` | OK — owner-root view |
| peko's owner | `peko log <P> --peer user:<any>` | OK — owner can audit any peer thread |
| peko's owner | `peko stop <P> --peer user:<any>` | OK — owner can stop any peer's run |
| Peer `user:bob` with `Chat` grant | `peko log <P> --peer user:bob` | OK — peer self-read |
| Peer `user:bob` with `Chat` grant | `peko log <P> --watch --peer user:bob` | OK — peer self-watch |
| Peer `user:bob` with `Chat` grant | `peko stop <P> --peer user:bob` | OK — peer stops their own run |
| Peer `user:bob` with `Chat` grant | `peko log <P>` (no `--peer`) | **rejected** — owner-root is not bob's view |
| Peer `user:bob` with `Chat` grant | `peko log <P> --peer user:alice` | **rejected** — bob cannot read alice's thread |
| Peer `user:bob` with `Chat` grant | `peko stop <P> --peer user:alice` | **rejected** — bob cannot stop alice's run |
| Stranger (no `Chat` grant) | any `peko log …` / `peko stop …` | **rejected** |

Concretely, a non-owner caller must pass `--peer <self>` (e.g.
`--peer user:bob`) to read or stop anything; the default form is
owner-only. There is no flag that widens access. If a future ADR wants
asymmetric read/write grants (e.g. a broadcast peer that can read but
not chat), a `ReadLog` permission variant will be designed and
documented there.

See [ADR-042](../architecture/adr/ADR-042-no-external-session-concept.md)
for the underlying contract and the rationale for keeping it strict,
and [ADR-048](../architecture/adr/ADR-048-channel-native-cli-surface.md)
for the send/stop/log surface built on it.

---

## Running Your First Peko

### 1. Add a Model

Models are added generically — there are no built-in vendor presets. Supply
the endpoint's wire format, base URL, and wire model id:

```bash
./target/release/peko model add --id my-model \
    --api-format openai_completions \
    --base-url https://llm.example.com/v1 \
    --model <wire-model-id> \
    --key "<your-api-key>"
```

`--api-format` is one of `anthropic_messages`, `openai_completions`, or
`openai_responses`. `--key` stores the API key in the encrypted vault (OS
keychain); `~/.peko/models.toml` keeps only a reference. Use `--no-key` for
local endpoints that need no key. Provider keys are never read from
environment variables.

### 2. Verify the Model

```bash
./target/release/peko model test my-model
```

### 3. Create a Peko

```bash
./target/release/peko create my-principal --model my-model
```

### 4. Send a Message

```bash
./target/release/peko send my-principal "Hello, what can you do?"
```

You'll see the peko's response streamed to your terminal.

---

## Configuration

### Peko Configuration

Pekos are configured via `principal.toml` stored in the peko's workspace.

Example `principal.toml`:

```toml
name = "my-principal"
description = "A helpful assistant"

# Optional per-peko provider override. Most pekos should
# omit this and use the global catalog default.
# preferred_provider_id = "openai"
# preferred_model_id = "gpt-4o-mini"

# Capability grants — the single source of truth for what this peko
# may do. See `peko capability grant --help`.
[capabilities]
grants = ["tool:Bash", "tool:Read", "tool:Write"]
```

#### Standing named children

A peko can declare **standing children** — named subagent sessions
that persist for the peko's lifetime and are resumed by name (e.g.
`/memory`):

```toml
[children.memory]
subagent_type = "archivist"        # required
description = "Long-term memory"   # optional; used as the session title
```

Each table key is the child's name (1–64 chars, no `/`, no surrounding
whitespace). At startup the runtime ensures each declared child exists as
a session under the peko's root — creating it without running any
LLM turn if missing, or leaving it untouched when it already exists.
Standing children are exempt from idle-session pruning. Inside the
peko, the `Agent` tool's `new` action with a matching `name`
attaches to the standing session (keeping its full history) instead of
spawning a fresh one; `subagent_type` must match the declaration.

### Environment Variables

| Variable | Description |
|----------|-------------|
| `RUST_LOG` | Logging level (debug, info, warn, error) |
| `PEKO_CONFIG_DIR` | Configuration directory override |
| `PEKO_DATA_DIR` | Data directory override |

Model API keys are **not** read from environment variables — they live in
the encrypted vault (OS keychain). Store them with `peko model add --key`
or `peko credential set llm <model-id> --kind api_key`.

### Configuration Priority

1. Command-line arguments (highest)
2. Environment variables
3. Config file values
4. Defaults (lowest)

---

## Tooling (Workspace Plugins)

> **ADR-047 (2026-08-25).** The legacy "Unified Extension Architecture"
> and `peko ext *` CLI were retired. Tooling — tools, skills, MCP
> servers, hooks, and opaque plugins — now lives directly in the
> peko's workspace. See
> [PRINCIPAL_WORKSPACE.md](../architecture/PRINCIPAL_WORKSPACE.md) for
> the full layout and migration steps.

The peko workspace at `~/.peko/principals/<name>/` is the canonical
trust boundary for tooling.

### List Installed Tooling

Tooling is plain files — list the workspace directories directly:

```bash
ls ~/.peko/principals/<name>/{tools,skills,mcp,hooks,plugins}/
peko show <name>          # catalog summary
```

### Install a Plugin

Copy the tooling into the workspace (ADR-050 — there is no install CLI):

```bash
cp ./my-tool.toml        ~/.peko/principals/<name>/tools/<id>/tool.toml
cp -r ./my-skill         ~/.peko/principals/<name>/skills/<id>/   # contains SKILL.md
cp ./server.json         ~/.peko/principals/<name>/mcp/<id>/server.json
cp ./hook.toml           ~/.peko/principals/<name>/hooks/<id>/hook.toml
```

New `agents/` and `skills/` files appear in the peko's system
prompt on the next agentic iteration — no restart required.

### Grant/Revoke Capabilities

Access to installed tooling is controlled by capability grants, edited
directly in the peko's `principal.toml` — there is no grant CLI
(ADR-050):

```toml
# ~/.peko/principals/<name>/principal.toml
[capabilities]
# Fresh pekos ship wildcard grants that allow everything:
# grants = ["tool:*", "agent:*", "skill:*", ...]
# Replace the wildcards with specific grants to restrict the peko:
grants = ["tool:Read", "tool:Bash", "skill:docker"]
```

Capabilities are typed grant strings such as `tool:<tool-name>`,
`skill:<skill-id>`, `agent:<agent-type>`, or `mcp:<server-id>`; a
trailing `*` matches by prefix. Skills specifically are plain
`SKILL.md` files under `skills/` — see
[Skills](../architecture/SKILLS.md) for format and authoring.

### MCP Servers

MCP (Model Context Protocol) servers live in the peko workspace
under `~/.peko/principals/<name>/mcp/<server-id>/server.json`:

```bash
# MCP servers live in the principal's workspace — install by copying
# the manifest in (there is no install CLI, ADR-050)
mkdir -p ~/.peko/principals/<name>/mcp/<server-id>
cp <server-path>/server.json ~/.peko/principals/<name>/mcp/<server-id>/server.json
```

If the peko's `[capabilities].grants` in `principal.toml` have been
restricted, add an `mcp:<server-id>` grant there to allow the server.

No `start` / `stop` / `restart` / `status` step is required — the
runtime discovers MCP servers at peko boot.

---

## Cron Scheduling

The daemon can run scheduled jobs on behalf of a peko. Each job is
scoped to exactly one peko by DID and executes as that peko's
owner, so it reuses the same `peko send` permission path and session memory.

Cron is now an **internal peko tool** (like Bash, Session). Operators
interact with schedules by asking the peko itself to manage them via
the `Cron` tool with create/list/delete/update/trigger/history actions. Every
principal can use registered tools; schedules remain owned by their principal.
The `peko cron` CLI was retired.

Supported schedules:

- **Cron expression** — standard 6-field cron (`sec min hour dom mon dow`)
- **One-time `at`** — RFC3339 timestamp (one-shot; auto-deleted)
- **Interval `every`** — fixed millisecond interval
- **Idle** — fires after the peko has had no user activity for N minutes

### SpawnTool attribution: `wake_on_completion` and `timeout_secs`

`Agent`-cron entries can opt into two attributes per fire:

| Attribute | Default | What it does |
|-----------|---------|--------------|
| `wake_on_completion` | `false` for cron-spawned runs (`true` for natural agent spawns) | When `true`, the executor posts a `SteeringMessage` into the peko's root inbox saying "Your N:NN cron job `{name}` completed. You can check details with the Async tool with action="output"." |
| `timeout_secs` | `7200` (2h) for everyone | Per-run timeout. The cron tool overrides per call. |

If the fire is an `Agent` tool run, the spawned session transcript
appears automatically in the peko's session list.

### Example: daily Peko digest

Send a message to your peko asking it to schedule the digest. From
inside an agent turn (or as a user invoking the peko's chat):

```text
Cron {
  action: "create",
  interval_ms: 86400000,
  tool: "Agent",
  params: { prompt: "Summarize my open tasks and today's priorities.", role: "root", path: "daily-digest" },
  label: "daily-digest"
}
```

### Example: nightly shell command

Inside an agent turn:

```text
Cron {
  action: "create",
  tool: "Bash",
  params: { command: "echo nightly-backup && /opt/backup/run.sh" },
  wake_on_completion: true,
  timeout_secs: 3600,
  interval_ms: 86400000
}
```

On fire the daemon runs `Bash { command: ... }` on behalf of your
peko. Because `wake_on_completion=true`, the peko's next
turn opens with a one-line steer message pointing at the task id.

Jobs are stored in the per-peko schedule file
(`<data_dir>/principals/<name>/local/cron/schedule.toml`) and can be
listed, inspected, and removed with `Cron` with list/delete actions from inside the owning peko's agent turn.

---

## Known Limitations

### Messages between runtimes

A message to a peko on another runtime travels through PekoHub, which relays
it but does not store it. Your runtime keeps each outgoing message on disk and
resends it until the other runtime confirms it (after a reconnect, after a
restart, and on a backoff of 30 seconds doubling up to an hour). `ChannelSend`
reports `delivered` or `queued`.

So a message reaches an offline runtime only once **both runtimes are online
at the same time**. A message still unconfirmed after **30 days** is dropped
with a warning. If two runtimes are never online together, their messages do
not get through.

### Security limits

Peko is a personal runtime. It trusts the operating-system user it runs as.
The limits below are known and accepted for this release.

**Runs acting for you are not reviewed.** The policy reviewer, and the
`approve <id>` / `deny <id>` questions it sends you, apply only to runs
acting for someone else: guests in a conversation, or other pekos. Runs
acting for you are never reviewed. These include the scheduled `keepalive`
and nightly `memory` runs.

So if a peko has an MCP server that can send email, post, delete or pay, a
scheduled run can use it without asking you. Instructions in the peko's
files are guidance, not enforcement. Connect only tools you are willing to
let the peko use on its own.

**The tool ledger is tamper-evident, not tamper-proof.** Every tool call is
recorded in a hash-chained ledger: `peko audit ledger` reads it and
`peko audit verify` checks it. `verify` detects edited, deleted or truncated
entries.

A run with `Bash` runs as your user. It could rewrite the ledger, the global
audit log and the checkpoints together, and nothing outside your account
anchors the chain yet.

**PekoHub can read relayed messages.** Messages between runtimes are signed
by the peko that sent them, so the hub cannot forge or alter them. They are
not encrypted end to end, though. The hub terminates TLS, so it could read
messages between runtimes and chats from Hub web visitors.

**Background tasks do not survive a restart.** A task started with `Async`
that is still running when the daemon stops is lost. Cron jobs are saved to
disk and resume after a restart, so use a cron job for work that must
survive one.

**The UDP fallback trusts any local process.** The CLI normally reaches the
daemon over a Unix socket in a `0700` directory with `0600` permissions. On
Linux the daemon also rejects messages from a different user id.

If the socket cannot be created, the daemon falls back to UDP on
`127.0.0.1:11435`, and it logs `falling back to UDP` when it does. Any
process on the machine can reach the daemon on that port. On a shared
machine, fix the socket problem instead of running on the fallback.

---

## Troubleshooting

### Peko Won't Respond

**Problem:** peko fails to respond to messages.

**Solution:**
- Check the peko exists: `peko list`
- Check a model is configured: `peko model list`
- Live-test the model and its stored key: `peko model test <model-id>`
- Check the peko configuration: `peko show my-principal`

### API Key Errors

**Problem:** "Failed to create provider" or API errors.

**Solution:**
- Check the model's credential wiring: `peko model show <model-id>` (a
  missing `credential_id` means no key is stored for it)
- List stored keys: `peko credential list --namespace llm`
- Replace a stored key in place: `peko credential set llm <model-id> --kind api_key`
  (omit `--material` for a hidden prompt)
- Live-test the endpoint: `peko model test <model-id>`
- Check the key has sufficient credits
- Verify network connectivity to the provider

### Memory Issues

**Problem:** Conversation history appears missing or corrupted.

**Solution:**
- Memory compaction is automatic; send another message to resume the current session
- Inspect the peko workspace with `peko show my-principal`

### Getting Help

```bash
# Show help
./target/release/peko --help

# Show command-specific help
./target/release/peko --help
./target/release/peko send --help
# peko ext and the per-category install CLI were retired (ADR-047 / ADR-050) —
# workspace tooling is managed as plain files; see PRINCIPAL_WORKSPACE.md
```

---

## Next Steps

- Read the [CLI Reference](CLI_REFERENCE.md) for all commands
- Read the [peko Workspace](../architecture/PRINCIPAL_WORKSPACE.md) to understand where tooling lives (ADR-047)
- Read the [Tutorial: Building Your First Agent](../getting-started/TUTORIAL_BUILDING_FIRST_AGENT.md)

---

*Built with 🐱 by the Peko team*
