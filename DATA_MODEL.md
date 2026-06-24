# Peko — Data Model Specification

**Version:** 0.1.0
**Date:** 2026-06-23 (review pass)
**Status:** Current
**Companion docs:** [`AGENTS.md`](AGENTS.md) (build & module rules), [`API_SURFACE.md`](API_SURFACE.md) (public Rust API), [`docs/architecture/adr/`](docs/architecture/adr/) (decisions)

This document defines every on-disk and in-memory data format used by the Peko runtime. It is the authoritative reference for anyone implementing the filesystem loader, session manager, image builder, or any component that reads or writes Peko data. All formats described here must be treated as stable contracts — breaking changes require a version increment.

---

## Table of Contents

1. [Conventions](#1-conventions)
2. [config.toml — Agent Image Config](#2-configtoml--agent-image-config)
3. [runtime.toml — Runtime Config](#3-runtimetoml--runtime-config)
4. [team.toml — Team Definition](#4-teamtoml--team-definition)
5. [Session JSONL — Conversation History](#5-session-jsonl--conversation-history)
   - [5.1 File Conventions](#51-file-conventions)
   - [5.2 Event Envelope](#52-event-envelope)
   - [5.3 Event Types](#53-event-types)
   - [5.4 Session Index File](#54-session-index-file)
   - [5.5 Context Cache (Derived)](#55-context-cache-derived)
   - [5.6 Compaction](#56-compaction)
6. [Agent Package Format (.agent)](#6-agent-package-format-agent)
7. [Team Package Format (.team)](#7-team-package-format-team)
8. [Extension Package Format (.ext)](#8-extension-package-format-ext)
9. [Local Registry Store](#9-local-registry-store)
10. [Instance State](#10-instance-state)
11. [Markdown Files](#11-markdown-files)
12. [mcp.json — MCP Server Config](#12-mcpjson--mcp-server-config)
13. [Tool Protocol — stdin/stdout](#13-tool-protocol--stdinstdout)
14. [Extension Manifest](#14-extension-manifest)
15. [Type Reference](#15-type-reference)
16. [Changelog](#16-changelog)

---

## 1. Conventions

### 1.1 Null vs. Absent

Fields marked **optional** may be omitted entirely from the file. A present field with a `null` value is equivalent to omitting it. Parsers must treat both the same way.

### 1.2 Unknown Fields

Parsers must ignore unknown fields. This allows forward-compatible additions without breaking older runtime versions.

### 1.3 Timestamps

All timestamps are ISO 8601 in UTC with millisecond precision: `2026-03-16T08:00:00.000Z`.

### 1.4 IDs

Runtime-assigned IDs use a `prefix_` + 8-character base36 string:

| Prefix | Resource |
|--------|----------|
| `inst_` | Agent instance |
| `sess_` | Session |
| `team_` | Team |
| `evt_` | Session event |
| `tc_` | Tool call |
| `msg_` | Message |

Image IDs use `sha256:` + the full hex digest of the image manifest.

### 1.5 Defaults

When a field is omitted and a default is listed, the runtime behaves as if the default value was written. Defaults are never written to disk by the runtime — they remain implicit.

### 1.6 TOML Versions

All TOML files conform to TOML v1.0. String values use double quotes. Multi-line strings use `"""`. Comments use `#`.

---

## 2. config.toml — Agent Image Config

`config.toml` is the only required file in an agent image. It defines the agent's identity, LLM provider, capabilities, hooks, and optional inheritance from a base image.

**Location:** `<agent-image-root>/config.toml`

### 2.1 Full Schema

```toml
# ── Identity ─────────────────────────────────────────────────────────────────

[agent]
name        = "researcher"          # REQUIRED. Lowercase alphanumeric + hyphens
version     = "1.0.0"              # REQUIRED. Semver string
description = "..."                 # Optional. Free text, shown in registry listings
author      = "user@example.com"   # Optional
license     = "MIT"                # Optional

# ── Provider preference (v3+) ────────────────────────────────────────────────
# Provider/model/API-key wiring lives in the runtime-owned catalog at
# `~/.peko/providers.toml` plus the OS keychain (service "peko",
# account = provider_id). An agent only carries soft hints:

preferred_provider_id = "anthropic"   # Optional. Provider id from the catalog.
preferred_model_id    = "claude-sonnet-4-5"  # Optional. Model id within that provider.

# These are *hints* — `LlmResolver` chooses the actual provider/model
# at request time using this precedence:
#   1. explicit caller override (`peko send --provider X --model Y`)
#   2. session-pinned choice from a prior turn
#   3. agent.preferred_provider_id / preferred_model_id  <-- here
#   4. runtime default (`peko provider set-default X`)
#   5. first enabled catalog entry

# v3 agents only carry soft hints. The actual provider wiring
# (base_url, api_key, model catalog) lives in `~/.peko/providers.toml`
# and the OS keychain via `peko provider add` / `peko credential set`.

# ── Base image inheritance ─────────────────────────────────────────────────

[base]
image = "pekohub.com/agents/base-researcher:v2"  # Optional. Full image ref or digest

# ── Capability dependencies ────────────────────────────────────────────────

[capabilities]
tools  = ["github", "browser"]           # Optional. Named tool capabilities
skills = ["research"]                    # Optional. Named skill capabilities
mcps   = ["vector-store-memory"]         # Optional. Named MCP capabilities

[capabilities.session]
plugin = "lossless-compression"          # Optional. Session plugin capability

[capabilities.options]
auto_install = true                      # Optional. Default: true in dev, false in prod

# ── Hooks ──────────────────────────────────────────────────────────────────

[[hooks]]
type     = "cron"
schedule = "0 8 * * *"            # Standard crontab syntax (5-field)
action   = "run"                  # "run" = start a session with trigger as first message
session  = "new"                  # "new" | "active". Default: "new"
enabled  = true                   # Optional. Default: true

[[hooks]]
type    = "webhook"
path    = "/hooks/github"         # Path suffix under /webhooks/{instance_id}/{token}
token   = "wh_7kxmnqp4vrlbsdcf8e2nthz9a0yqw5j"  # Optional shared secret
action  = "run"
session = "new"
enabled = true

[[hooks]]
type    = "event"
topic   = "team.task.assigned"    # Event bus topic pattern (glob supported)
action  = "run"
session = "active"                # Inject into the running active session
enabled = true

[[hooks]]
type    = "file_watch"
path    = "./inbox/"              # Relative to instance workspace
pattern = "*.pdf"                 # Optional glob filter
action  = "run"
session = "new"
enabled = true

# ── Resource limits ────────────────────────────────────────────────────────

[resources]
max_concurrent_tools = 5          # Optional. Default: 5
tool_timeout_seconds = 30         # Optional. Default: 30
session_timeout_seconds = 3600    # Optional. Idle session timeout. Default: no timeout
max_session_tokens  = 200000      # Optional. Truncate context if exceeded
```

### 2.2 Provider catalog (v3+)

As of v3, providers are not declared in `config.toml`. They live in
the runtime-owned catalog at `~/.peko/providers.toml`:

```toml
# ~/.peko/providers.toml
version = "3.0"

[entries.openai]
display_name = "OpenAI"
api_format   = "openai_completions"
base_url     = "https://api.openai.com/v1"
default_model_id = "gpt-4o-mini"
models = [
    { id = "gpt-4o",      context_length = 128000, capabilities = ["tool_use", "vision"] },
    { id = "gpt-4o-mini", context_length = 128000 },
]

[entries.anthropic]
display_name = "Anthropic"
api_format   = "anthropic_messages"
base_url     = "https://api.anthropic.com"
default_model_id = "claude-sonnet-4-5"
models       = [...]

default_provider_id = "openai"
default_model_id    = "gpt-4o-mini"
```

API keys are NOT in this file. They live in the OS keychain under
service `"peko"` with the provider id as the account name. Manage
them with:

```bash
peko provider add --template anthropic   # seeds a catalog entry
peko credential set anthropic             # stores the API key in the keychain
peko provider set-default anthropic --model claude-sonnet-4-5
```

Agent configs carry `preferred_provider_id` / `preferred_model_id`
only — never an inline `[provider]` block. The v3-cleanup series
deleted the deprecated field entirely (PR #43 follow-up).

### 2.3 Inheritance and Merging

When `[base]` is specified, the runtime loads the base image's `config.toml` first, then merges the local config on top. Merge rules:

- Scalar fields: local value overwrites base.
- Array fields (`capabilities.tools`, `capabilities.mcps`, etc.): local array is **appended** to base array, then deduplicated.
- `[[hooks]]`: local hooks are appended to base hooks. Base hooks cannot be removed, only added to.
- `[provider]`: entire section replaced by local if present; otherwise base provider is used.

#### 2.3.1 Circular Dependency Detection

The runtime detects circular base image dependencies and aborts with error `circular_base_dependency`.

**Detection rules:**
- Maximum inheritance depth: 10 levels
- If loading a base image that is already in the current inheritance chain, the operation fails
- Error includes the chain of inheritance that caused the cycle (e.g., `A → B → C → B`)

**Example error:**
```json
{
  "error": {
    "code": "circular_base_dependency",
    "message": "Circular base image dependency detected",
    "details": {
      "chain": ["my-agent", "base-v2", "base-v1", "base-v2"],
      "depth_limit": 10
    }
  }
}
```

### 2.4 Validation Rules

- `name` must match `^[a-z0-9][a-z0-9-]*[a-z0-9]$` (3–64 chars).
- `version` must be a valid semver string.
- `schedule` (cron hooks) must be a valid 5-field crontab expression.
- `token` (webhook hooks), if present, must be 32–128 printable ASCII characters.
- Duplicate `path` values across `webhook` hooks within the same agent are not permitted.

#### 2.4.1 Webhook Token Security

Webhook tokens are optional but **strongly recommended**. The runtime enforces token requirements based on configuration:

**In `runtime.toml`:**
```toml
[daemon]
require_webhook_tokens = false  # Default: false (development)
                                # Set to true for production
```

| Setting | Behavior |
|---------|----------|
| `require_webhook_tokens = false` (default) | Token optional; omission logs a warning |
| `require_webhook_tokens = true` | Token mandatory; webhook hooks without tokens fail validation with `missing_webhook_token` error |

**Best practice:** Always set tokens in production. Tokens should be:
- Randomly generated (32+ characters)
- Treated as secrets (stored in environment variables, not committed to version control)
- Rotated periodically

---

## 3. runtime.toml — Runtime Config

The daemon's own configuration. Lives at the runtime root.

**Location:** `.peko/config.toml` (project-level) or `~/.peko/config.toml` (global)

### 3.1 Full Schema

```toml
# ── Daemon ─────────────────────────────────────────────────────────────────

[daemon]
port        = 11435               # Optional. Default: 11435
host        = "127.0.0.1"        # Optional. Default: 127.0.0.1 (localhost only)
log_level   = "info"             # Optional. "error"|"warn"|"info"|"debug"|"trace". Default: "info"
log_format  = "text"             # Optional. "text"|"json". Default: "text"
pid_file    = ".peko/run/daemon.pid"  # Optional. Default: as shown

# ── Registry ───────────────────────────────────────────────────────────────

[registry]
default = "pekohub.com"          # Optional. Default: "pekohub.com"

[[registry.sources]]
url      = "pekohub.com"
priority = 1                     # Lower = checked first

[[registry.sources]]
url      = "registry.internal.example.com"
priority = 2
auth     = { type = "token", env = "INTERNAL_REGISTRY_TOKEN" }

# ── Provider credentials ───────────────────────────────────────────────────
# API keys are resolved at runtime from environment variables.
# Never store keys in this file.

[providers]
anthropic_api_key_env = "ANTHROPIC_API_KEY"   # Optional. Default: "ANTHROPIC_API_KEY"
openai_api_key_env    = "OPENAI_API_KEY"       # Optional. Default: "OPENAI_API_KEY"

# ── Capabilities ───────────────────────────────────────────────────────────

[capabilities]
auto_install    = false           # Optional. Default: false (safe for prod)
install_dir     = "~/.peko/capabilities"  # Optional. Default: as shown

# ── Team defaults ──────────────────────────────────────────────────────────

[teams]
workspace_root  = ".peko/teams"  # Optional. Default: as shown

# ── Agents defaults ────────────────────────────────────────────────────────

[agents]
workspace_root  = ".peko/agents" # Optional. Default: as shown
```

### 3.2 Auth Types for Registry Sources

| `type` | Fields | Description |
|--------|--------|-------------|
| `token` | `env` | Bearer token read from the named env var |
| `basic` | `user_env`, `password_env` | HTTP Basic, credentials from env vars |
| `none` | — | Unauthenticated (public registries) |

---

## 4. team.toml — Team Definition

Defines the agents, scaling, shared services, and bus configuration for a team.

**Location:** `.peko/teams/<team-name>/config.toml`

### 4.1 Full Schema

```toml
# ── Team identity ──────────────────────────────────────────────────────────

[team]
name        = "research-team"     # REQUIRED. Lowercase alphanumeric + hyphens
description = "..."               # Optional

# ── Agent definitions ──────────────────────────────────────────────────────

[[agents]]
name      = "coordinator"         # REQUIRED. Unique within this team
image     = "./agents/coordinator" # REQUIRED. Local path, image ref, or digest
instances = 1                     # Optional. Default: 1
role      = "coordinator"         # Optional. "coordinator"|"worker"|null
env       = { CUSTOM_VAR = "value" }  # Optional. Instance-level env overrides

[[agents]]
name      = "researcher"
image     = "pekohub.com/agents/researcher:v2.5"
instances = 3
role      = "worker"

[[agents]]
name      = "writer"
image     = "pekohub.com/agents/writer:v1.0"
instances = 1

# ── Shared services ────────────────────────────────────────────────────────

[shared.bus]
backend = "in-memory"             # "in-memory"|"redis"|"nats". Default: "in-memory"
url     = null                    # Required for redis/nats. e.g. "redis://localhost:6379"

[shared.memory]
type    = "chroma"                # "chroma"|"qdrant"|"in-memory". Default: "in-memory"
url     = null                    # Required for chroma/qdrant
persist = true                    # Optional. Default: true

[shared.files]
enabled = true                    # Optional. Default: true
path    = ".peko/teams/research-team/shared/files"  # Optional. Default: as shown

# ── Shared MCPs ────────────────────────────────────────────────────────────
# These are started once and proxied to all agents in the team.

[[shared.mcps]]
name    = "browser"
command = ["npx", "-y", "@browserbasehq/mcp"]
env     = { BROWSERBASE_API_KEY = "${BROWSERBASE_API_KEY}" }
```

### 4.2 Agent `image` Resolution

| Value pattern | Resolution |
|---------------|------------|
| Starts with `./` or `/` | Filesystem path to agent directory |
| Contains `:` but not `/` | Local image `name:tag` |
| Contains `/` | Full registry ref `host/path:tag` |
| `sha256:...` | Exact digest, registry-independent |

### 4.3 Agent Instance Naming and Identity

When a team is deployed, the runtime creates instances from the `[[agents]]` definitions:

| Field | Usage | Instance Naming |
|-------|-------|-----------------|
| `name` | Agent type identifier within the team | Used as prefix for instance names |
| `instances` | Count of identical instances to create | Instances are named `{name}-{N}` where N starts at 1 |

**Example:**
```toml
[[agents]]
name = "researcher"
instances = 3
```

Creates instances with IDs like `inst_7k2mxp3q`, named:
- `researcher-1` (ID: `inst_7k2mxp3q`)
- `researcher-2` (ID: `inst_2nqkrp8x`)
- `researcher-3` (ID: `inst_5vlmds4w`)

**Addressing instances:**
- Via daemon API: Use the instance ID (`inst_...`)
- Via A2A bus: Use the instance name (`researcher-1`) with `Direct` message type
- Instance names are unique within a team; instance IDs are globally unique

### 4.4 Shared Bus Backend Requirements

| Backend | Requirement |
|---------|-------------|
| `in-memory` | None. All instances must be in the same process |
| `redis` | Redis 6.2+ with Streams support. Set `url` |
| `nats` | NATS 2.2+ with JetStream. Set `url` |

---

## 5. Session JSONL — Conversation History

Sessions are stored as JSONL (newline-delimited JSON) files. Each line is exactly one event. The file is append-only — events are never modified or deleted in place.

**Location:** `<instance-workspace>/sessions/<session-id>.jsonl`

**Example:** `.peko/teams/research-team/agents/researcher-1/sessions/sess_9nrwbf1v.jsonl`

### 5.1 File Conventions

- Each line is a complete, independently parseable JSON object.
- Lines are separated by `\n` (Unix line endings).
- No trailing comma. No outer array wrapper.
- The file is written atomically: new events are first written to `<filename>.tmp`, then `rename()` appends to the live file. A partial `.tmp` file indicates a crash mid-write and must be discarded.
- Files are UTF-8 encoded.

### 5.2 Event Envelope

Every line shares this envelope:

```json
{
  "id":         "evt_001",
  "type":       "<event-type>",
  "session_id": "sess_9nrwbf1v",
  "ts":         "2026-03-16T08:05:00.000Z",
  "seq":        1,
  ...type-specific fields
}
```

| Field | Type | Description |
|-------|------|-------------|
| `id` | string | Unique event ID within the session |
| `type` | string | Event type (see §5.3) |
| `session_id` | string | ID of the session this event belongs to |
| `ts` | string | ISO 8601 timestamp of when the event was written |
| `seq` | integer | Monotonically increasing sequence number, starting at 1 |

### 5.3 Event Types

#### `session.created`

Written as the very first line of every new session file.

```json
{
  "id": "evt_001",
  "type": "session.created",
  "session_id": "sess_9nrwbf1v",
  "ts": "2026-03-16T08:05:00.000Z",
  "seq": 1,
  "instance_id": "inst_7k2mxp3q",
  "image_digest": "sha256:a3b5c7d9e1f2...",
  "parent_session_id": null,
  "trigger": "user"
}
```

| `trigger` | Meaning |
|-----------|---------|
| `user` | Interactive session started by a user |
| `cron` | Started by a cron hook |
| `webhook` | Started by a webhook delivery |
| `event` | Started by an event bus message |
| `file_watch` | Started by a file watch trigger |
| `branch` | Created from a branch of another session |
| `spawn` | Created as a subagent by a parent session |

---

#### `user.message`

A message sent by the user (or a hook trigger payload).

```json
{
  "id": "evt_002",
  "type": "user.message",
  "session_id": "sess_9nrwbf1v",
  "ts": "2026-03-16T08:05:00.000Z",
  "seq": 2,
  "message_id": "msg_3xpwqr7n",
  "content": "Summarise the Q4 earnings report.",
  "source": "user"
}
```

| `source` | Meaning |
|----------|---------|
| `user` | Typed by a human |
| `hook` | Injected by a hook trigger |
| `a2a` | Sent by another agent via the event bus |
| `spawn_parent` | Sent by the spawning parent agent |

---

#### `assistant.message`

The final, complete text response from the LLM for a turn.

```json
{
  "id": "evt_003",
  "type": "assistant.message",
  "session_id": "sess_9nrwbf1v",
  "ts": "2026-03-16T08:05:04.100Z",
  "seq": 5,
  "message_id": "msg_7xpwqr9n",
  "content": "The Q4 earnings report shows revenue of $4.2B...",
  "usage": {
    "input_tokens": 1240,
    "output_tokens": 382,
    "total_tokens": 1622
  }
}
```

`usage` reflects the token counts for the entire turn (all LLM calls combined, including tool result re-submissions).

---

#### `thinking`

Extended thinking content produced before or during a response. Only written if the provider and model support it and it is enabled in `[provider]`.

```json
{
  "id": "evt_004",
  "type": "thinking",
  "session_id": "sess_9nrwbf1v",
  "ts": "2026-03-16T08:05:00.800Z",
  "seq": 3,
  "content": "I need to search for recent financial data for ACME Corp..."
}
```

---

#### `tool.call`

An invocation of a tool requested by the LLM.

```json
{
  "id": "evt_005",
  "type": "tool.call",
  "session_id": "sess_9nrwbf1v",
  "ts": "2026-03-16T08:05:01.800Z",
  "seq": 3,
  "tool_call_id": "tc_4bwmnq",
  "tool": "web_search",
  "args": {
    "query": "Q4 2025 earnings ACME Corp"
  },
  "async": false,
  "timeout_seconds": 30
}
```

---

#### `tool.result`

The result returned by a tool invocation.

```json
{
  "id": "evt_006",
  "type": "tool.result",
  "session_id": "sess_9nrwbf1v",
  "ts": "2026-03-16T08:05:03.221Z",
  "seq": 4,
  "tool_call_id": "tc_4bwmnq",
  "output": "ACME Corp reported $4.2B revenue in Q4 2025, up 12% YoY...",
  "error": null,
  "duration_ms": 1421
}
```

When the tool fails, `output` is `null` and `error` is a string describing the failure.

---

#### `spawn.request`

This agent spawned a subagent.

```json
{
  "id": "evt_007",
  "type": "spawn.request",
  "session_id": "sess_9nrwbf1v",
  "ts": "2026-03-16T08:06:00.000Z",
  "seq": 6,
  "tool_call_id": "tc_5cxnor",
  "child_image": "researcher:v2",
  "child_instance_id": "inst_9pqrst2x",
  "child_session_id": "sess_2kxmnqp4",
  "task": "Summarise the Q4 earnings report at https://...",
  "async": false
}
```

---

#### `spawn.result`

The spawned subagent completed.

```json
{
  "id": "evt_008",
  "type": "spawn.result",
  "session_id": "sess_9nrwbf1v",
  "ts": "2026-03-16T08:06:45.200Z",
  "seq": 7,
  "tool_call_id": "tc_5cxnor",
  "child_instance_id": "inst_9pqrst2x",
  "child_session_id": "sess_2kxmnqp4",
  "output": "The Q4 report summary is...",
  "error": null,
  "duration_ms": 45200
}
```

---

#### `a2a.sent`

This agent sent a message to the team event bus.

```json
{
  "id": "evt_009",
  "type": "a2a.sent",
  "session_id": "sess_9nrwbf1v",
  "ts": "2026-03-16T08:07:00.000Z",
  "seq": 8,
  "message_type": "Task",
  "topic": "team.research-team.tasks",
  "to": "inst_2nqkrp8x",
  "payload": {
    "task": "Find recent news about ACME Corp",
    "priority": "high"
  }
}
```

---

#### `a2a.received`

This agent received a message from the team event bus.

```json
{
  "id": "evt_010",
  "type": "a2a.received",
  "session_id": "sess_9nrwbf1v",
  "ts": "2026-03-16T08:07:01.100Z",
  "seq": 9,
  "message_type": "TaskResult",
  "topic": "team.research-team.results",
  "from": "inst_2nqkrp8x",
  "payload": {
    "result": "ACME Corp announced a new product line...",
    "task_id": "tc_5cxnor"
  }
}
```

---

#### `hook.trigger`

The session was activated by a hook.

```json
{
  "id": "evt_011",
  "type": "hook.trigger",
  "session_id": "sess_9nrwbf1v",
  "ts": "2026-03-16T08:00:00.000Z",
  "seq": 1,
  "hook_type": "cron",
  "schedule": "0 8 * * *",
  "payload": null
}
```

For webhook hooks, `payload` contains the decoded request body. For `file_watch`, it contains `{ "path": "...", "event": "created" }`.

---

#### `system`

A system-level annotation. Used for recording significant runtime events that affect the session but are not part of the conversation content.

```json
{
  "id": "evt_012",
  "type": "system",
  "session_id": "sess_9nrwbf1v",
  "ts": "2026-03-16T09:00:00.000Z",
  "seq": 10,
  "event": "context_truncated",
  "detail": {
    "reason": "max_session_tokens exceeded",
    "tokens_before": 210000,
    "tokens_after": 180000,
    "events_dropped": 4
  }
}
```

Common `event` values:

| `event` | Meaning | `detail` fields |
|---------|---------|-----------------|
| `context_truncated` | Context was truncated due to token limit | `reason`, `tokens_before`, `tokens_after`, `events_dropped` |
| `session_resumed` | Session was resumed from storage | `resumed_at` |
| `instance_restarted` | The instance was restarted | `restart_count` |
| `plugin_applied` | A plugin was applied | `plugin_id` |
| `compaction` | Conversation history was compacted (ADR-022) | `summary`, `messages_compacted`, `tokens_before`, `tokens_after`, `compaction_number` |
| `model_change` | The LLM model was changed mid-session | `provider`, `model_id` |

**Compaction event (ADR-022):**

```json
{
  "id": "evt_015",
  "type": "system",
  "session_id": "sess_9nrwbf1v",
  "ts": "2026-04-26T10:30:00.000Z",
  "seq": 15,
  "event": "compaction",
  "detail": {
    "summary": "## Goal\nSummarise Q4 earnings...",
    "messages_compacted": 12,
    "tokens_before": 45000,
    "tokens_after": 8000,
    "compaction_number": 1
  }
}
```

When a `compaction` event is present, the runtime builds the LLM context by emitting the summary as a system message, followed only by messages that appear **after** the compaction event in the JSONL. Messages before the compaction are not sent to the LLM — they are preserved in the JSONL for audit but do not contribute to the context window.

**Model change event:**

```json
{
  "id": "evt_016",
  "type": "system",
  "session_id": "sess_9nrwbf1v",
  "ts": "2026-04-26T11:00:00.000Z",
  "seq": 16,
  "event": "model_change",
  "detail": {
    "provider": "openai",
    "model_id": "gpt-4o"
  }
}
```

---

#### `session.ended`

Written as the final line when a session is explicitly closed. Not written on crash — the absence of this event indicates an unclean termination.

```json
{
  "id": "evt_013",
  "type": "session.ended",
  "session_id": "sess_9nrwbf1v",
  "ts": "2026-03-16T09:05:00.000Z",
  "seq": 11,
  "reason": "user_closed",
  "turn_count": 6,
  "total_tokens": 9840
}
```

| `reason` | Meaning |
|----------|---------|
| `user_closed` | User explicitly ended the session |
| `instance_stopped` | Instance was stopped while session was active |
| `idle_timeout` | Session exceeded `session_timeout_seconds` |
| `max_tokens_reached` | Hard token limit hit and session cannot continue |

---

### 5.4 Session Index File

Alongside each `.jsonl` file is a `.index.json` sidecar. It is maintained by the runtime and provides O(1) lookup of session metadata without scanning the full JSONL.

**Location:** `<session-id>.index.json`

```json
{
  "session_id": "sess_9nrwbf1v",
  "instance_id": "inst_7k2mxp3q",
  "created_at": "2026-03-16T08:05:00.000Z",
  "updated_at": "2026-03-16T09:05:00.000Z",
  "turn_count": 6,
  "event_count": 13,
  "total_tokens": 9840,
  "parent_session_id": null,
  "trigger": "user",
  "ended": true,
  "title": "Q4 earnings analysis"
}
```

The `title` field is auto-generated by the runtime after the first assistant response (first 60 characters, stripped of newlines). It can be overwritten by the user.

---

### 5.5 Context Cache (Derived)

Alongside each `.jsonl` file is an optional `.context.cache` file. It is a **derived, discardable** cache that provides fast session resume by storing the current LLM message list after compaction has been applied.

**Location:** `<session-id>.context.cache`

**Format:**

```text
# peko-context-cache v1
# checksum: <blake3 hash of .jsonl content>
# entries: <number of lines in .jsonl>
[<array of ChatMessage>]
```

**Properties:**

- **Derived**: Can be rebuilt at any time by walking the `.jsonl` and applying compaction events.
- **Discardable**: Safe to delete. The runtime will transparently rebuild it on next load.
- **Validated**: On load, the runtime verifies the blake3 checksum and entry count match the current `.jsonl`. If stale, the cache is ignored and rebuilt.
- **Rewritten after compaction**: When compaction occurs, the cache is rewritten to contain `[system_prompt, summary_message, …recent_messages]`.

**Lifecycle:**

```
1. Session created → .jsonl written, no cache yet
2. First resume → build_context() walks .jsonl, writes cache
3. Subsequent resumes → load_context_cache() validates checksum, loads directly
4. Compaction occurs → update_context_cache() rewrites cache
5. External .jsonl modification → checksum mismatch on next load → cache rebuilt
```

**Backward compatibility:** Old single-file sessions (`.jsonl` only) work transparently. The cache is generated on first `load_context_fast()`.

---

### 5.6 Compaction

Compaction is the process of summarizing old conversation history to stay within the LLM's context window. It is triggered automatically when token usage crosses a configurable threshold, or manually via `peko session compact`.

#### 5.6.1 Trigger Conditions

Compaction triggers when **either** condition is met:

1. **Ratio threshold**: `estimated_tokens >= (context_window * auto_threshold_percent / 100)`
2. **Reserved headroom**: `estimated_tokens >= (context_window - reserve_tokens)`

Default values (from `config.toml` `[compaction]` section):

```toml
[compaction]
enabled = true
auto_threshold_percent = 85
reserve_tokens = 16384
keep_recent_tokens = 20000
```

#### 5.6.2 Context Building with Compaction

When building the LLM context from a session that contains compaction events:

1. Find the **latest** `system` event with `event: "compaction"`.
2. Emit the compaction's `summary` as a system message.
3. Emit only messages that appear **after** the compaction event in the JSONL.
4. Messages before the compaction are preserved in `.jsonl` for audit but excluded from the LLM context.

If multiple compaction events exist, only the latest one is applied. Earlier compactions are superseded.

#### 5.6.3 Turn Boundaries

Compaction respects conversation turn boundaries:

- **Valid cut points**: user messages, assistant messages, system prompts
- **Never cut at**: tool results (they must stay paired with their tool call)

If the token-based cut would land on a tool result, it is moved backward to the nearest valid cut point.

#### 5.6.4 Split-Turn Handling

When a single turn exceeds `keep_recent_tokens`, the cut may land mid-turn. In this case:

1. A **history summary** is generated from complete turns before the cut.
2. A **turn-prefix summary** is generated from the early part of the split turn.
3. The two summaries are merged with a `**Turn Context (split turn):**` separator.

#### 5.6.5 Structured Summary Format

The built-in compactor produces summaries in this structured format:

```markdown
## Goal
[What the user is trying to accomplish]

## Constraints & Preferences
- [Requirements mentioned by user]

## Progress
### Done
- [x] [Completed tasks]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues, if any]

## Key Decisions
- **[Decision]**: [Rationale]

## Next Steps
1. [What should happen next]

## Critical Context
- [Data needed to continue]

<read-files>
path/to/file1.rs
path/to/file2.rs
</read-files>

<modified-files>
path/to/changed.rs
</modified-files>
```

File operations are tracked across compactions by scanning tool calls in the messages being summarized.

---

## 6. Agent Package Format (.agent)

A `.agent` file is a gzip-compressed tar archive containing a portable agent package. It can be built from a source directory, exported from a running agent, pushed to/pulled from a registry, and imported into a new runtime.

### 6.1 Package Structure

```
my-agent.agent (gzip-compressed tar)
├── manifest.toml          # Package metadata and layer digests
├── config/
│   ├── agent.toml         # Agent configuration (SSOT for behaviour)
│   └── (no other files — prompts are built dynamically at runtime)
├── identity/
│   ├── did.json           # DID document
│   └── keys.enc           # Encrypted private keys
├── skills/
│   └── {name}/
│       └── SKILL.md
├── workspace/
│   └── SYSTEM.md
└── sessions/              # Optional (can be large)
```

### 6.2 Manifest Schema (TOML)

The manifest contains **only packaging metadata**. Agent behaviour configuration (tools, MCP, skills, extensions) lives in `config/agent.toml` — the single source of truth.

```toml
[agent]
name = "researcher"
version = "1.0.0"
description = "A web research agent"
created_at = "2026-05-08T10:00:00Z"
export_format = "1.1"
did = "did:peko:local:researcher:abc123"
peko_version = "0.1.0"

[identity]
key_algorithm = "ed25519"
encrypted = false

[layers]
config = "sha256:1a2b3c4d..."
identity = "sha256:5e6f7g8h..."
skills = "sha256:9i0j1k2l..."
workspace = "sha256:3m4n5o6p..."

[packaging]
files = [
  "config/agent.toml",

  "identity/did.json",
  "identity/keys.enc",
  "skills/web-search/SKILL.md",
  "workspace/SYSTEM.md",
]
checksums = {
  "config/agent.toml" = "sha256:abc...",
  "identity/did.json" = "sha256:def...",
}
compression = "gzip"
archive_format = "tar"

[signatures]
manifest = "base64url_signature..."
algorithm = "ed25519"
```

### 6.3 Clean Manifest Principle

`AgentManifest` explicitly does **NOT** contain:
- `capabilities` — removed; extension framework is the single source of truth
- `tools` — lives in `agent.toml` / extension whitelist
- `mcp` — lives in `agent.toml` / extension configuration
- `tool_sources` — lives in `agent.toml`
- `memory` — lives in the sessions layer or external stores

This ensures packaging metadata never competes with `agent.toml` as a source of truth.

### 6.4 Layer Types

| Layer | Source Directory | Required | Contents |
|-------|------------------|----------|----------|
| `config` | `config/` | Yes | `agent.toml` (SSOT) |
| `identity` | `identity/` | Yes | `did.json`, `keys.enc` |
| `skills` | `skills/` | No | Skill directories with `SKILL.md` |
| `workspace` | `workspace/` | No | Workspace files (`SYSTEM.md`, etc.) |
| `sessions` | `sessions/` | No | Session history (optional, can be large) |
| `mcp` | `mcp/` | No | MCP configuration files |

Each layer is stored as a gzip-compressed tar archive in the local registry, deduplicated by SHA-256 digest.

---

## 7. Team Package Format (.team)

A `.team` file is a gzip-compressed tar archive containing multiple agents plus team metadata.

### 7.1 Package Structure

```
my-team.team (gzip-compressed tar)
├── team/
│   ├── manifest.toml      # Team metadata + packaging checksums
│   └── team.toml          # Original team definition (optional)
└── agents/
    └── {agent-name}/
        ├── config/agent.toml
        ├── identity/did.json
        └── ...
```

### 7.2 Team Manifest Schema (TOML)

```toml
[team]
name = "research-squad"
description = "A team of research agents"
agent_count = 2
created_at = "2026-05-08T10:00:00Z"
peko_version = "0.1.0"

[packaging]
files = [
  "team/manifest.toml",
  "team/team.toml",
  "agents/researcher/config/agent.toml",
  "agents/writer/config/agent.toml",
]
checksums = {
  "team/team.toml" = "sha256:abc...",
  "agents/researcher/config/agent.toml" = "sha256:def...",
}
compression = "gzip"
archive_format = "tar"
```

### 7.3 Checksum Validation

On import, all file checksums are verified before any agent is imported. If a checksum mismatch is detected, the import fails fast with a clear error. Legacy packages without `packaging` metadata import successfully with a warning.

### 7.4 Team Registry Format (Push/Pull)

When a team is pushed to a registry via `peko team push`, it is **not** stored as a single opaque blob. Instead, the team is decomposed into content-addressable layers, enabling cross-team agent deduplication.

#### Layer Types

| Layer Type | `LayerType` variant | Contents |
|-----------|---------------------|----------|
| TeamConfig | `TeamConfig` | `team.toml` (optional) + `manifest.toml` with agent index |
| Config | `Config` | `config/agent.toml` |
| Identity | `Identity` | `identity/did.json`, `identity/keys.enc` |
| Skills | `Skills` | `skills/{name}/SKILL.md` |
| Workspace | `Workspace` | `workspace/` files |
| Sessions | `Sessions` | `sessions/` JSONL files |
| MCP | `Mcp` | `mcp/` configuration |

#### TeamConfig Layer

The `TeamConfig` layer is a gzipped tarball containing:

```
manifest.toml   # TeamAgentIndex: team metadata + agent → layer digest mapping
```

The `manifest.toml` inside the `TeamConfig` layer uses this schema:

```toml
[team]
name = "prod-team"
version = "1.0.0"
agent_count = 3

[agents]
# agent_name → layer digests (same format as AgentManifest.layers)
researcher = { config = "sha256:abc...", identity = "sha256:def...", skills = "sha256:ghi..." }
coder = { config = "sha256:jkl...", identity = "sha256:mno..." }
reviewer = { config = "sha256:pqr...", identity = "sha256:stu...", workspace = "sha256:vwx..." }
```

#### RegistryManifest for Teams

```json
{
  "schema_version": 1,
  "kind": "team",
  "name": "prod-team",
  "version": "1.0.0",
  "ref": "pekohub.com/teams/prod-team:v1.0",
  "digest": "sha256:...",
  "created_at": "2026-05-08T10:00:00Z",
  "source": "local",
  "layers": [
    { "digest": "sha256:tc...", "type": "team_config", "size_bytes": 256 },
    { "digest": "sha256:a1c...", "type": "config", "size_bytes": 512 },
    { "digest": "sha256:a1i...", "type": "identity", "size_bytes": 384 },
    { "digest": "sha256:a2c...", "type": "config", "size_bytes": 512 },
    { "digest": "sha256:a2i...", "type": "identity", "size_bytes": 384 }
  ]
}
```

#### Deduplication Mechanics

When two teams share the same agent:

1. **Team A push**: Stores `TeamConfig(A)` + `Config(X)` + `Identity(X)` + ... as new blobs
2. **Team B push**: `HEAD /blobs/Config(X)` → 200 (exists), skipped. Only `TeamConfig(B)` + any unique agent layers are uploaded

Registry storage grows with **unique agent count**, not team count. The existing `RegistryClient::check_existing_layers()` handles this automatically.

#### Pull Flow

1. `client.pull()` downloads all layers into local `AgentRegistry`
2. Find the `TeamConfig` layer, extract `manifest.toml`
3. Parse the agent index to discover which agents exist and their layer digests
4. Reconstruct each agent's files from its layers in-memory
5. Import each agent directly via `Unpackager::import_from_files()` — no temporary `.team` file is created

---

## 8. Extension Package Format (.ext)

A `.ext` file is a gzip-compressed tar archive containing an installed extension.

### 8.1 Package Structure

```
docker-skill.ext (gzip-compressed tar)
├── manifest.toml          # Extension metadata + checksums
└── extension/
    ├── manifest.yaml      # Extension manifest
    ├── SKILL.md           # Skill definition (for skill types)
    └── ...
```

### 8.2 Extension Manifest Schema (TOML)

```toml
[format]
version = "1.0"
peko_version = "0.1.0"

[extension]
id = "docker-skill"
name = "docker-skill"
extension_type = "skill"
version = "1.0.0"
description = "Manage Docker containers"

[packaging]
files = ["extension/manifest.yaml", "extension/SKILL.md"]
checksums = {
  "extension/manifest.yaml" = "sha256:abc...",
  "extension/SKILL.md" = "sha256:def...",
}
compression = "gzip"
archive_format = "tar"
```

---

## 9. Local Registry Store

The local registry is a content-addressable store for `.agent` layers and manifests.

### 9.1 Storage Layout

```
~/.peko/registry/
├── layers/
│   └── sha256-abc123.../
│       └── layer.tar.gz
├── manifests/
│   └── sha256-xyz789.../
│       └── manifest.toml
├── registry_manifests/
│   └── sha256-xyz789.../
│       └── manifest.json    # JSON wire format for registry push/pull
└── tags/
    └── my-agent_v1.0        # file contains manifest digest
```

### 9.2 Registry Manifest (JSON Wire Format)

Used for HTTP push/pull protocol. Converted to/from `AgentManifest` at the boundaries.

```json
{
  "schema_version": 1,
  "name": "researcher",
  "version": "1.0.0",
  "ref": "pekohub.com/agents/researcher:v1.0",
  "digest": "sha256:abc123...",
  "created_at": "2026-05-08T10:00:00Z",
  "source": "local",
  "layers": [
    {
      "digest": "sha256:1a2b3c4d...",
      "type": "config",
      "size_bytes": 512
    }
  ]
}
```

---

## 10. Instance State

Runtime state for a running or stopped instance. Stored in SQLite.

**Location:** `.peko/run/state.db`

This is not a user-editable file. It is the daemon's working state — sessions, instance metadata, team membership. It is rebuilt from the filesystem if deleted. The JSONL session files are the source of truth; the SQLite file is a read-optimised index over them.

### 7.1 Tables

#### `instances`

| Column | Type | Description |
|--------|------|-------------|
| `id` | TEXT PK | Instance ID (`inst_...`) |
| `name` | TEXT | Human name |
| `image_ref` | TEXT | Image ref at creation time |
| `image_digest` | TEXT | Pinned image digest |
| `status` | TEXT | Current `InstanceStatus` |
| `team_id` | TEXT NULL | Foreign key to `teams.id` |
| `workspace_path` | TEXT | Absolute path to instance workspace |
| `active_session_id` | TEXT NULL | Currently active session |
| `created_at` | TEXT | ISO 8601 timestamp |
| `started_at` | TEXT NULL | ISO 8601 timestamp |
| `stopped_at` | TEXT NULL | ISO 8601 timestamp |
| `error` | TEXT NULL | Error message if `status = 'error'` |

#### `sessions`

| Column | Type | Description |
|--------|------|-------------|
| `id` | TEXT PK | Session ID (`sess_...`) |
| `instance_id` | TEXT | Foreign key to `instances.id` |
| `jsonl_path` | TEXT | Absolute path to `.jsonl` file |
| `created_at` | TEXT | ISO 8601 timestamp |
| `updated_at` | TEXT | ISO 8601 timestamp |
| `turn_count` | INTEGER | Number of complete turns |
| `event_count` | INTEGER | Total events in the JSONL |
| `total_tokens` | INTEGER | Cumulative token usage |
| `parent_session_id` | TEXT NULL | Set if branched |
| `trigger` | TEXT | How the session was started |
| `ended` | INTEGER | Boolean (0/1) |
| `title` | TEXT NULL | Auto-generated or user-set |

#### `teams`

| Column | Type | Description |
|--------|------|-------------|
| `id` | TEXT PK | Team ID (`team_...`) |
| `name` | TEXT | Team name |
| `status` | TEXT | `starting` / `running` / `stopping` / `stopped` |
| `config_path` | TEXT | Absolute path to `config.toml` |
| `created_at` | TEXT | ISO 8601 timestamp |

---

## 11. Markdown Files

Optional markdown files in the agent image root provide identity, behavior, and context. All follow the same conventions.

### 8.1 General Conventions

- UTF-8 encoded.
- Standard CommonMark markdown.
- No required internal structure — the runtime loads the full file content and passes it to the LLM as part of the system prompt. Authors may structure with headings for readability, but the runtime does not parse internal structure.
- Files are loaded at instance startup and held in memory for the session lifetime. Changes to files on disk are not hot-reloaded in a running instance (only in `--watch` dev mode).

### 8.2 Load Order and Prompt Assembly

When the runtime assembles the system prompt, markdown files are concatenated in this order:

```
1. BOOTSTRAP.md   — sets the high-level framing
2. IDENTITY.md    — who the agent is
3. SOUL.md        — values and personality
4. AGENT.md       — responsibilities and behaviour
5. TOOLS.md       — tool usage guidelines
6. USER.md        — context about the user
7. SKILLS.md      — available skills description
```

If a file is absent, its slot is skipped. If `[provider] system_prompt` is also set in `config.toml`, it is prepended before `BOOTSTRAP.md`.

### 8.3 File Reference

| File | Purpose | Typical Content |
|------|---------|-----------------|
| `BOOTSTRAP.md` | Sets the initial framing for every session | Opening context, task domain, high-level goals |
| `IDENTITY.md` | Who the agent is | Name, role, communication style |
| `SOUL.md` | Core values and personality traits | Tone, principles, what the agent cares about |
| `AGENT.md` | Operational behaviour | Responsibilities, what to do, what not to do |
| `TOOLS.md` | How to use available tools | When to call each tool, argument conventions |
| `USER.md` | Context about the user | Name, preferences, background — populated by the user |
| `SKILLS.md` | Available skills | Brief descriptions of skills the agent can invoke |

### 8.4 Inheritance

When a base image is used, markdown files from the base are loaded first. If the local image provides the same file (e.g. both have `AGENT.md`), the local file **replaces** the base file entirely. Files present only in the base are inherited as-is.

---

## 12. mcp.json — MCP Server Config

Declares MCP servers available to the agent. The runtime starts these processes and proxies tool calls to them.

**Location:** `<agent-image-root>/mcp.json`

### 9.1 Schema

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
      "env": {}
    },
    "github": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-github"],
      "env": {
        "GITHUB_PERSONAL_ACCESS_TOKEN": "${GITHUB_TOKEN}"
      }
    },
    "vector-memory": {
      "command": "peko-mcp-proxy",
      "args": ["vector-store-memory"],
      "env": {}
    }
  }
}
```

### 9.2 Field Reference

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `command` | string | Yes | Executable to run |
| `args` | string[] | No | Arguments passed to the command |
| `env` | object | No | Environment variables. Values starting with `${` are substituted from the runtime environment |

### 9.3 Environment Variable Substitution

Values in `env` support `${VAR_NAME}` substitution. The variable is resolved from the process environment at instance startup. If the variable is not set, the field is set to an empty string and a warning is logged.

---

## 13. Tool Protocol — stdin/stdout

Custom tools in `tools/` are executables invoked by the runtime via a simple JSON-over-stdin/stdout protocol. Any language that can read stdin and write stdout works.

### 10.1 Invocation

The runtime spawns the tool as a subprocess and writes a single JSON object to its stdin, followed by a newline:

```json
{
  "tool_call_id": "tc_4bwmnq",
  "tool":         "my_tool",
  "args":         { "query": "ACME Corp Q4 earnings" },
  "timeout_ms":   30000,
  "context": {
    "instance_id": "inst_7k2mxp3q",
    "session_id":  "sess_9nrwbf1v",
    "workspace":   "/home/user/project/.peko/agents/researcher-1/workspace"
  }
}
```

### 10.2 Response

The tool writes a single JSON object to stdout, followed by a newline, then exits:

**Success:**

```json
{
  "tool_call_id": "tc_4bwmnq",
  "output":       "ACME Corp reported $4.2B revenue in Q4 2025...",
  "error":        null
}
```

**Failure:**

```json
{
  "tool_call_id": "tc_4bwmnq",
  "output":       null,
  "error":        "HTTP request failed: connection timeout"
}
```

### 10.3 Exit Codes

| Exit code | Meaning |
|-----------|---------|
| `0` | Success — parse stdout as JSON |
| `1` | Tool error — runtime treats as a failed tool call; `error` in response is used |
| `2` | Protocol error — runtime logs a warning and reports a generic failure to the LLM |

Anything written to stderr is captured and appended to the daemon log at `debug` level.

### 10.4 Tool Manifest (optional)

A tool may include a `<toolname>.json` sidecar that describes its schema. The runtime uses this to generate the tool definition sent to the LLM. Without it, the runtime generates a minimal definition using just the tool name.

```json
{
  "name":        "my_tool",
  "description": "Searches the web for recent information.",
  "parameters": {
    "type": "object",
    "properties": {
      "query": {
        "type":        "string",
        "description": "The search query"
      },
      "max_results": {
        "type":    "integer",
        "default": 5
      }
    },
    "required": ["query"]
  }
}
```

The `parameters` schema is JSON Schema draft-07.

---

## 14. Extension Manifest

> **Note:** The pre-extension `capability.toml` / `AgentCapability` system has been removed. The extension framework (`extensions.enabled` whitelist, `ExtensionManager`) is the single mechanism for controlling tool/MCP/skill access.

Every installable extension includes a manifest that describes it.

### 14.1 Extension Types

| Type | Description | Manifest File |
|------|-------------|---------------|
| `skill` | Markdown-based skill | `SKILL.md` with YAML frontmatter |
| `mcp` | MCP server adapter | `manifest.yaml` |
| `gateway` | Gateway adapter | `manifest.yaml` |
| `builtin` | Built-in tool | Embedded in runtime |
| `general` | General extension | `manifest.yaml` |
| `universal` | Universal tool adapter | `manifest.yaml` |

### 14.2 SKILL.md Frontmatter (Skill Extensions)

```yaml
---
name: docker-skill
description: Manage Docker containers
version: 1.0.0
---

# Docker Skill

Skill content in Markdown...
```

### 14.3 Extension Package Layout

```
~/.peko/extensions/
├── skill/
│   └── docker-skill/
│       ├── SKILL.md
│       └── templates/
└── mcp/
    └── filesystem/
        └── manifest.yaml
```

Every installable capability (tool, skill, MCP, session plugin) includes a `capability.toml` that describes it.

**Location:** `<capability-root>/capability.toml`

### 11.1 Schema

```toml
[capability]
type        = "tool"              # REQUIRED. "tool"|"skill"|"mcp"|"session"
name        = "web-browser"       # REQUIRED. Lowercase alphanumeric + hyphens
version     = "1.2.0"            # REQUIRED. Semver
description = "Playwright-based browser for web interaction"
author      = "peko-community"
license     = "MIT"

# For type = "tool"
[tool]
entrypoint  = "browser.py"        # Executable within the capability directory
platforms   = ["linux", "darwin"] # Optional. Omit to indicate all platforms

# For type = "mcp"
[mcp]
command     = ["npx", "-y", "@browserbasehq/mcp"]
env_vars    = ["BROWSERBASE_API_KEY"]  # Required env vars; runtime warns if missing

# For type = "skill"
[skill]
prompts     = ["skill.md"]        # Markdown prompt files included in the skill
tools       = ["web_search"]      # Tool capabilities this skill depends on

# For type = "session"
[session]
library     = "libsession_lossless.so"  # Shared library implementing SessionPlugin
```

### 11.2 Installed Capability Layout

```
~/.peko/capabilities/
├── tools/
│   └── web-browser@1.2.0/
│       ├── capability.toml
│       └── browser.py
├── skills/
│   └── research@2.0.1/
│       ├── capability.toml
│       └── skill.md
├── mcps/
│   └── vector-store-memory@1.0.0/
│       └── capability.toml
└── session/
    └── lossless-compression@1.0.0/
        ├── capability.toml
        └── libsession_lossless.so
```

Multiple versions of the same capability may be installed side by side. An agent's `config.toml` may pin to a version:

```toml
[capabilities]
tools = ["browser@1.2.0"]         # Pinned
skills = ["research"]             # Latest installed
```

---

## 15. Type Reference

Quick-reference table of all primitive types used across formats.

| Type name | Format | Example |
|-----------|--------|---------|
| `Timestamp` | ISO 8601 UTC, ms precision | `2026-03-16T08:00:00.000Z` |
| `InstanceId` | `inst_` + 8 base36 chars | `inst_7k2mxp3q` |
| `SessionId` | `sess_` + 8 base36 chars | `sess_9nrwbf1v` |
| `TeamId` | `team_` + 8 base36 chars | `team_4hsdcl8e` |
| `EventId` | `evt_` + 8 base36 chars | `evt_001` |
| `ToolCallId` | `tc_` + 6 base36 chars | `tc_4bwmnq` |
| `MessageId` | `msg_` + 8 base36 chars | `msg_3xpwqr7n` |
| `ImageDigest` | `sha256:` + 64 hex chars | `sha256:a3b5c7d9...` |
| `ImageRef` | `[host/]name:tag` | `pekohub.com/agents/researcher:v2.5` |
| `Semver` | Major.Minor.Patch | `1.2.0` |
| `AgentName` | `^[a-z0-9][a-z0-9-]*[a-z0-9]$` | `my-researcher` |
| `InstanceStatus` | Enum | `starting \| running \| stopping \| stopped \| error` |
| `HookType` | Enum | `cron \| webhook \| event \| file_watch` |
| `SessionTrigger` | Enum | `user \| cron \| webhook \| event \| file_watch \| branch \| spawn` |
| `CapabilityType` | Enum | `tool \| skill \| mcp \| session` |
| `BusBackend` | Enum | `in-memory \| redis \| nats` |

---

## 16. Changelog

| Version | Date | Changes |
|---------|------|---------|
| 0.1.0 | 2026-04-26 | Initial draft. ADR-022: Session compaction — added `compaction` and `model_change` system events (§5.3), context cache file format (§5.5), compaction semantics including dual-threshold triggers, turn boundaries, split-turn handling, and structured summary format (§5.6) |
| 0.1.0 | 2026-05-08 | Packaging restructure (Phases 1–7): `src/image/` merged into `src/portable/`. Clean manifest — `AgentManifest` stripped of `capabilities`, `tools`, `mcp`, `tool_sources`, `memory`. Added `layers` section with content-addressable digests. Added `.agent` build from directory (`AgentBuilder`). Added registry push/pull with mock server. Added team checksum validation and `team.toml` preservation. Added `.ext` export for extensions. `agent.toml` is the single source of truth for agent behaviour. |
| 0.1.0 | 2026-05-11 | Issue 023: Team registry push/pull now decomposes teams into content-addressable layers (`TeamConfig` + per-agent `Config`/`Identity`/`Skills`/etc.) instead of a single opaque blob. Added `TeamAgentIndex` and `AgentLayerRef` types. Added `TeamLayerBuilder` and `TeamLayerReconstructor`. Cross-team agent deduplication works automatically via existing `RegistryClient::check_existing_layers()`. Pull reconstructs agents directly from layers without temporary `.team` files. Documented in §7.4. |

---

*Version: 0.1.0 · Last Updated: 2026-04-26 · Status: Draft*
