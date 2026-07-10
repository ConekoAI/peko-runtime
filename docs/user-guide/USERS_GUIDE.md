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

### Key Features

| Feature | Description |
|---------|-------------|
| **Lightweight** | Small binary, starts quickly |
| **Standalone** | Works without external services |
| **Principal-Centric** | A Principal is the top-level actor you create and chat with |
| **Persistent Memory** | JSONL-based session storage managed automatically per Principal |
| **Owner-Root Activity Feed** | `peko log <PRINCIPAL>` reads the owner's thread — including cron wakes and async completions — without exposing the session abstraction |
| **DID Identity** | ed25519-based decentralized identifiers |
| **Extensions** | Unified Extension Architecture for skills, MCP, tools, channels, hooks |
| **Cron Scheduling** | Schedule recurring and one-time tasks, scoped to a Principal and run as its owner (operator/advanced) |

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
- **Configuration** — Settings including capability grants, provider hints, and governance
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

A Session is the implementation noun for one peer's running conversation
with a Principal. Sessions are stored as JSONL files under the
Principal's workspace, created automatically when a peer first sends,
resumed on every subsequent send from the same peer, and compacted
when context pressure demands. Sessions are an internal storage
detail — they are deliberately **not** exposed as a CLI command:

- `peko send <PRINCIPAL> "…"` — drive a conversation.
- `peko log <PRINCIPAL>` — read the **owner-root view** of activity
  (the conversation running on the owner's behalf, plus any background
  work the owner is entitled to see: cron wakes, async-task
  completions, etc.).
- `peko log <PRINCIPAL> --peer <X>` — read peer `X`'s thread. Only
  peer `X` itself or the principal's owner can request this; other
  peers cannot snoop on each other (see ADR-042 for the full
  privacy contract).

**There is no `peko session` command and there will never be one.** The
Principal is an integral actor; sessions are storage, not state that
the user names, lists, or routes through. Operators who need to inspect
raw session files read them directly from the Principal's workspace.

#### Privacy gate (ADR-042)

The default `peko log <PRINCIPAL>` invocation (no `--peer`) returns
the **owner-root view**, which is special: it is only readable by the
Principal's owner. The privacy contract is enforced strictly:

| Caller | Command | Result |
|---|---|---|
| Principal's owner | `peko log <P>` | OK — owner-root view |
| Principal's owner | `peko log <P> --peer user:<any>` | OK — owner can audit any peer thread |
| Peer `user:bob` with `Chat` grant | `peko log <P> --peer user:bob` | OK — peer self-read |
| Peer `user:bob` with `Chat` grant | `peko log <P>` (no `--peer`) | **rejected** — owner-root is not bob's view |
| Peer `user:bob` with `Chat` grant | `peko log <P> --peer user:alice` | **rejected** — bob cannot read alice's thread |
| Stranger (no `Chat` grant) | any `peko log …` | **rejected** |

Concretely, a non-owner caller must pass `--peer <self>` (e.g.
`--peer user:bob`) to read anything; the default form is owner-only.
There is no flag that widens access. If a future ADR wants asymmetric
read/write grants (e.g. a broadcast peer that can read but not chat),
a `ReadLog` permission variant will be designed and documented there.

See [ADR-042](../architecture/adr/ADR-042-no-external-session-concept.md)
for the underlying contract and the rationale for keeping it strict.

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

# Capability grants — the single source of truth for what this Principal
# may do. See `peko capability grant --help`.
[capabilities]
grants = ["tool:Bash", "tool:Read", "tool:Write"]
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

### Grant/Revoke Extension Capabilities

Extension access is controlled through capabilities on a Principal:

```bash
# Grant a capability to a Principal
peko capability grant --principal <principal-name> <capability>

# Revoke a capability from a Principal
peko capability revoke --principal <principal-name> <capability>
```

Principal-scoped authorization is configured in `principal.toml` under
`[capabilities] grants`. Capabilities are typed grant strings such as
`tool:<tool-name>`, `skill:<skill-id>`, `agent:<agent-type>`, or
`mcp:<server-id>`.

### MCP Servers

MCP (Model Context Protocol) servers are managed as extensions:

```bash
# MCP servers are installed and managed via the ext command
peko ext install <mcp-extension>

# Grant MCP capabilities to a Principal
peko capability grant --principal <principal-name> mcp:<mcp-extension>
```

---

## Cron Scheduling

The daemon can run scheduled jobs on behalf of a Principal. Each job is
scoped to exactly one Principal by name and executes as that Principal's
owner, so it reuses the same `peko send` permission path and session memory.

There are **two surfaces** for cron entries, both backed by the same
schedule store (`cron.json`). `peko cron list` shows entries from both
surfaces so you always see everything scheduled against a Principal:

1. **User cron (`peko cron`)** — a deferred `peko send`. Each fire
   delivers a user message to the Principal's owner root session. Built
   via the CLI subcommands `add`, `at`, `every`, `add-idle`,
   `add-event`. Use this when you want "send my Principal this prompt
   on a schedule."

2. **Agent cron (`CronCreate` tool)** — a deferred `AsyncSpawn`. Each
   fire asks the daemon's executor to invoke a tool on behalf of the
   Principal. Built from inside an agent turn. Use this when you want
   "have my Principal run a tool at a time." The `prompt` shorthand is
   `tool="Agent", params={ prompt }`. Any tool works: `Bash`, `Read`,
   `Agent`, etc.

Supported schedules:

- **Cron expression** — standard 6-field cron (`sec min hour dom mon dow`)
- **One-time `at`** — RFC3339 timestamp (one-shot; auto-deleted)
- **Interval `every`** — fixed millisecond interval
- **Idle** — fires after the Principal has had no user activity for N minutes
- **Event** — fires when a matching system event is published

### SpawnTool attribution: `wake_on_completion` and `timeout_secs`

`Agent`-cron entries can opt into two attributes per fire:

| Attribute | Default | What it does |
|-----------|---------|--------------|
| `wake_on_completion` | `false` for cron-spawned runs (`true` for natural agent spawns) | When `true`, the executor posts a `SteeringMessage` into the Principal's root inbox saying "Your N:NN cron job `{name}` completed. You can check details with the TaskOutput tool." |
| `timeout_secs` | `7200` (2h) for everyone | Per-run timeout. Both surfaces override per call. |

If the fire is an `Agent` tool run, the spawned session transcript
appears automatically in the Principal's session list.

### Example: daily Principal digest (user cron)

```bash
# Requires the daemon to be running
peko daemon start --foreground

# Schedule a daily message to your Principal
peko cron add --principal my-principal \
  --name daily-digest \
  --schedule "0 9 * * * *" \
  --message "Summarize my open tasks and today's priorities."
```

### Example: nightly shell command (agent cron)

Inside an agent turn:

```text
CronCreate {
  tool: "Bash",
  params: { command: "echo nightly-backup && /opt/backup/run.sh" },
  wake_on_completion: true,
  timeout_secs: 3600,
  every: 24h
}
```

On fire the daemon runs `Bash { command: ... }` on behalf of your
Principal. Because `wake_on_completion=true`, the Principal's next
turn opens with a one-line steer message pointing at the task id.

Jobs are stored in the daemon's cron DB (`<data_dir>/cron.json`) and can be
listed, inspected, and removed with `peko cron list` and `peko cron remove`.
For the full option set, see the [CLI Reference](CLI_REFERENCE.md#cron--principal-cron-jobs).

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
- Memory compaction is automatic; send another message to resume the current session
- Inspect the Principal workspace with `peko principal show my-principal`

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
