# Pekobot — Capability Interface Specification

**Version:** 1.0
**Date:** 2026-03-16
**Status:** Draft
**Companion docs:** `UNIFIED_ARCHITECTURE_SPEC.md` v4.0, `DATA_MODEL.md` v1.0, `API_CONTRACT.md` v1.0

This document defines the capability system in full: the four extension interfaces (tool, MCP, skill, session plugin), the complete built-in tool reference, the security sandbox model, and the cron lifecycle. It is the authoritative reference for anyone implementing a built-in tool, a custom capability, or the capability loader in the runtime.

---

## Table of Contents

1. [Overview](#1-overview)
2. [Extension Interfaces](#2-extension-interfaces)
3. [Built-in Tools — Reference](#3-built-in-tools--reference)
4. [MCP Integration](#4-mcp-integration)
5. [Skills](#5-skills)
6. [Session Plugins](#6-session-plugins)
7. [Security and Sandbox Model](#7-security-and-sandbox-model)
8. [Cron Lifecycle](#8-cron-lifecycle)
9. [Capability Resolution and Loading](#9-capability-resolution-and-loading)
10. [Changelog](#10-changelog)

---

## 1. Overview

### 1.1 Design Principle

The core runtime has zero knowledge of any specific capability implementation. It does not import a browser library, a search client, or a memory store. It only knows how to invoke capabilities through four stable interfaces. This means the runtime can be compiled, tested, and shipped independently of any capability ecosystem.

```
┌─────────────────────────────────────────────────────────┐
│                     Core Runtime                        │
│                                                         │
│   Agentic loop → calls Tool interface                   │
│                → calls MCP interface                    │
│                → calls Skill interface                  │
│   Session manager → calls SessionPlugin interface       │
└──────────┬──────────────┬──────────────┬────────────────┘
           │              │              │
    ┌──────▼──────┐ ┌─────▼─────┐ ┌────▼──────────────┐
    │ Built-in    │ │  Custom   │ │   MCP Servers      │
    │ Tools       │ │  Tools    │ │   (external proc)  │
    │ (compiled   │ │ (scripts/ │ │                    │
    │  into       │ │  binaries)│ │  mcp-web           │
    │  runtime)   │ │           │ │  mcp-browser       │
    └─────────────┘ └───────────┘ │  mcp-memory        │
                                  │  ...user MCPs      │
                                  └────────────────────┘
```

### 1.2 Capability Types

| Type | Interface | Delivery |
|------|-----------|----------|
| **Built-in tool** | `Tool` trait (Rust) | Compiled into the runtime binary |
| **Custom tool** | JSON over stdin/stdout | Executable script or binary in `tools/` |
| **MCP server** | Model Context Protocol | External process, proxied by runtime |
| **Skill** | Prompt + tool bundle | Markdown + optional tool deps |
| **Session plugin** | `SessionPlugin` trait (Rust) | Dynamically loaded shared library |

### 1.3 Boundary Rules

These rules are enforced by the runtime and must not be violated by capability implementations:

- A tool may only access filesystem paths within its granted sandbox (see §7).
- An agent tool call may not directly invoke another agent. Use `agent_spawn` instead.
- `sessions_send` is for human-to-agent and tooling use only. Agent-to-agent communication within a team must use the event bus (A2A).
- Built-in agent tools (`agents_list`, `agent_info`, `agent_spawn*`) are scoped to the current team by default. Cross-team access requires an explicit capability grant.
- Cron registrations are owned by the instance that created them and are persisted to disk (see §8).

---

## 2. Extension Interfaces

### 2.1 Tool Trait (Rust)

Built-in tools implement this trait. Custom tools use the stdin/stdout protocol (§2.2) which the runtime wraps in the same trait internally.

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    /// Unique tool name, lowercase snake_case
    fn name(&self) -> &str;

    /// Human-readable description passed to the LLM
    fn description(&self) -> &str;

    /// JSON Schema (draft-07) describing the `args` object
    fn parameters(&self) -> serde_json::Value;

    /// Whether this tool supports async (fire-and-forget) invocation
    fn supports_async(&self) -> bool { false }

    /// Execute the tool. Returns output string or an error.
    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: ToolContext,
    ) -> Result<ToolOutput, ToolError>;
}

pub struct ToolContext {
    pub tool_call_id:  String,
    pub instance_id:   String,
    pub session_id:    String,
    pub team_id:       Option<String>,
    pub workspace:     PathBuf,       // Instance private workspace
    pub shared_workspace: Option<PathBuf>, // Team shared workspace, if in a team
    pub sandbox:       SandboxConfig,
    pub timeout:       Duration,
}

pub struct ToolOutput {
    pub content:    String,
    pub receipt_id: Option<String>,  // Set for async tools; used to poll status
}

pub struct ToolError {
    pub code:    ToolErrorCode,
    pub message: String,
}

pub enum ToolErrorCode {
    Timeout,
    SandboxViolation,
    ExecutionFailed,
    InvalidArgs,
    NotFound,
    PermissionDenied,
}
```

### 2.2 Custom Tool Protocol (stdin/stdout)

Defined in full in `DATA_MODEL.md` §10. Summary:

- Runtime writes one JSON request object to stdin, followed by `\n`.
- Tool writes one JSON response object to stdout, followed by `\n`, then exits.
- Exit code `0` = success, `1` = tool error, `2` = protocol error.
- Stderr is captured and logged at `debug` level.
- The `context.workspace` field in the request gives the tool its allowed write path.

### 2.3 MCP Interface

The runtime acts as an MCP client. It starts MCP server processes at instance or team startup, discovers their tool list via the MCP `list_tools` call, and registers those tools in the LLM's tool list alongside built-in and custom tools.

Tool calls to MCP tools are proxied transparently — the LLM never knows whether a tool is built-in, custom, or MCP-backed.

MCP servers may be per-agent (declared in `mcp.json`) or shared across a team (declared in `team.toml` under `[[shared.mcps]]`). Shared MCP servers are started once and their process handle is reference-counted.

### 2.4 SessionPlugin Trait (Rust)

```rust
pub trait SessionPlugin: Send + Sync {
    fn name(&self) -> &str;

    /// Called before an event is written to the JSONL file.
    /// May transform the event (e.g. compress content) or return None to drop it.
    fn on_write(&self, event: &SessionEvent) -> Option<SessionEvent>;

    /// Called after an event is read from the JSONL file.
    /// Must be the inverse of on_write for any transformation applied.
    fn on_read(&self, event: &SessionEvent) -> SessionEvent;

    /// Called when the context window is being assembled from session history.
    /// May summarise, truncate, or reorder events before they are sent to the LLM.
    fn on_context_build(&self, events: Vec<SessionEvent>) -> Vec<SessionEvent>;
}
```

Session plugins are loaded as shared libraries (`.so` / `.dylib`) via `libloading`. Only one plugin may be active per session. Plugin is declared in `[capabilities.session]` in `config.toml`.

---

## 3. Built-in Tools — Reference

All built-in tools are compiled into the runtime binary. They are always available without installation. Individual tools may be disabled per-agent via `[capabilities]` in `config.toml`.

```toml
[capabilities]
disabled_tools = ["process", "cron"]  # Opt-out specific built-ins
```

---

### 3.1 `filesystem`

**Sync/Async:** Async only
**Purpose:** File operations within the sandbox.

All paths are resolved relative to the instance workspace unless they are within the explicitly granted shared workspace. Absolute paths outside the sandbox are rejected with `SandboxViolation`.

#### Operations

**`read`** — Read a file's contents.

```json
{
  "operation": "read",
  "path": "reports/q4.md"
}
```

Response: `{ "content": "...", "size_bytes": 4096, "encoding": "utf8" }`

For binary files, set `"encoding": "base64"` in the request to receive base64-encoded content.

---

**`write`** — Write content to a file. Creates parent directories if needed.

```json
{
  "operation": "write",
  "path": "output/summary.md",
  "content": "# Summary\n...",
  "mode": "overwrite"
}
```

`mode`: `"overwrite"` (default) | `"append"` | `"create_new"` (fails if file exists)

---

**`list`** — List directory contents.

```json
{
  "operation": "list",
  "path": ".",
  "recursive": false,
  "include_hidden": false
}
```

Response:

```json
{
  "entries": [
    { "name": "reports", "type": "dir",  "size_bytes": null,  "modified_at": "2026-03-16T08:00:00.000Z" },
    { "name": "notes.md","type": "file", "size_bytes": 1024,  "modified_at": "2026-03-16T07:55:00.000Z" }
  ]
}
```

---

**`exists`** — Check whether a path exists.

```json
{ "operation": "exists", "path": "reports/q4.md" }
```

Response: `{ "exists": true, "type": "file" }`. `type` is `"file"` | `"dir"` | `null`.

---

**`delete`** — Delete a file or directory.

```json
{
  "operation": "delete",
  "path": "tmp/scratch.txt",
  "recursive": false
}
```

Set `"recursive": true` to delete a non-empty directory.

---

**`move`** — Move or rename a file or directory.

```json
{
  "operation": "move",
  "from": "draft.md",
  "to": "final.md"
}
```

Both `from` and `to` must be within the sandbox.

---

#### Sandbox paths

By default the tool has access to:

- Instance private workspace: `.pekobot/agents/<n>/workspace/` (read/write)
- Team shared workspace: `.pekobot/teams/<t>/shared/files/` (read/write, only if in a team)
- Agent image `projects/` content: read-only

Access to any other path requires an explicit `filesystem_grant` in `config.toml`:

```toml
[capabilities.grants]
filesystem = [
  { path = "/data/reports/", mode = "read" },
  { path = "/tmp/pekobot/", mode = "read_write" }
]
```

---

### 3.2 `process`

**Sync/Async:** Both
**Purpose:** Execute system commands.

> **Security note:** `process` is powerful. In production environments, consider disabling it via `disabled_tools` and using specific MCP tools instead.

#### Arguments

```json
{
  "command":     "python3",
  "args":        ["analyse.py", "--input", "data.csv"],
  "cwd":         "workspace/scripts",
  "env":         { "PYTHONPATH": "." },
  "timeout_ms":  30000,
  "async":       false,
  "stdin":       null
}
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `command` | string | required | Executable name or absolute path |
| `args` | string[] | `[]` | Arguments |
| `cwd` | string | instance workspace | Working directory (must be within sandbox) |
| `env` | object | `{}` | Additional environment variables |
| `timeout_ms` | integer | 30000 | Kill timeout in milliseconds |
| `async` | boolean | `false` | If true, returns a receipt immediately |
| `stdin` | string\|null | `null` | Content to pipe to the process stdin |

#### Sync response

```json
{
  "exit_code": 0,
  "stdout":    "Analysis complete. 42 rows processed.",
  "stderr":    "",
  "duration_ms": 1240
}
```

#### Async response

```json
{
  "receipt_id": "proc_7kxmnq",
  "pid":        12345,
  "started_at": "2026-03-16T08:05:01.000Z"
}
```

Poll status with `process_status` (a companion built-in, not separately listed, invoked by the runtime when the LLM calls back with the receipt).

#### Restrictions

- `command` must not be a shell (`sh`, `bash`, `zsh`, `fish`, `cmd`, `powershell`). Use `args` to compose commands. Shell injection via `command` is rejected at parse time.
- `cwd` must resolve to a path within the sandbox.
- Processes inherit a restricted environment: no `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, or any key matching `*_API_KEY` or `*_SECRET` from the parent process.

---

### 3.3 `apply_patch`

**Sync/Async:** Async only
**Purpose:** Apply structured patches to files atomically with automatic backup.

Prefer `apply_patch` over `filesystem.write` for modifying existing files. It is safer (atomic, backed up), more auditable (patch is logged as a session event), and cheaper in context (the LLM sends a diff, not the full file).

#### Arguments

```json
{
  "operations": [
    {
      "op":      "modify",
      "path":    "src/main.rs",
      "hunks": [
        {
          "context_before": "fn main() {\n    println!(\"hello\");",
          "old":            "    println!(\"hello\");",
          "new":            "    println!(\"hello, world!\");"
        }
      ]
    },
    {
      "op":      "create",
      "path":    "src/utils.rs",
      "content": "pub fn helper() {}\n"
    },
    {
      "op":      "delete",
      "path":    "src/old.rs"
    }
  ],
  "backup": true,
  "dry_run": false
}
```

| `op` | Required fields | Description |
|------|-----------------|-------------|
| `create` | `path`, `content` | Create a new file. Fails if path exists |
| `modify` | `path`, `hunks` | Apply context-matched hunks to an existing file |
| `delete` | `path` | Delete a file |
| `move` | `path`, `to` | Move or rename a file |

#### Hunk matching

Each hunk in a `modify` operation is matched by `context_before` (the lines immediately before the change). The match is whitespace-normalised. If no match is found, the entire patch is rejected and no files are modified.

#### Response

```json
{
  "applied": true,
  "operations": [
    { "op": "modify", "path": "src/main.rs", "status": "ok", "backup_path": "src/main.rs.bak" },
    { "op": "create", "path": "src/utils.rs", "status": "ok", "backup_path": null },
    { "op": "delete", "path": "src/old.rs",  "status": "ok", "backup_path": "src/old.rs.bak" }
  ]
}
```

If `dry_run` is `true`, no files are modified and `applied` is always `false`. Use this to preview a patch before committing.

If any operation fails, all operations are rolled back (backups restored) and `applied` is `false` with per-operation `status` showing which step failed.

---

### 3.4 `agent_spawn`

**Sync/Async:** Both
**Purpose:** Spawn a subagent instance to perform an isolated task.

#### Arguments

```json
{
  "image":   "researcher:v2",
  "task":    "Summarise the document at workspace/reports/q4.md and return a 3-paragraph summary.",
  "async":   false,
  "timeout_ms": 120000,
  "share_workspace": false,
  "env": {}
}
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `image` | string | required | Image ref, digest, or local path |
| `task` | string | required | The first user message sent to the spawned agent |
| `async` | boolean | `false` | If false, wait for completion and return output |
| `timeout_ms` | integer | 120000 | For sync: max wait time. For async: max run time before auto-kill |
| `share_workspace` | boolean | `false` | If true, child gets read/write access to parent's workspace |
| `env` | object | `{}` | Additional env vars for the child instance |

#### Sync response

```json
{
  "instance_id":  "inst_9pqrst2x",
  "session_id":   "sess_2kxmnqp4",
  "output":       "The Q4 report covers three main areas...",
  "exit_reason":  "completed",
  "duration_ms":  45200,
  "usage": {
    "input_tokens":  3200,
    "output_tokens": 480,
    "total_tokens":  3680
  }
}
```

`exit_reason`: `"completed"` | `"timeout"` | `"error"`

#### Async response

```json
{
  "run_id":      "spawn_4bwmnq",
  "instance_id": "inst_9pqrst2x",
  "session_id":  "sess_2kxmnqp4",
  "started_at":  "2026-03-16T08:06:00.000Z"
}
```

Use `agent_spawn_status` with the `run_id` to poll for completion.

#### Scope rules

- The spawned instance is always created in the same team as the parent (if in a team), or as a standalone instance (if parent is standalone).
- The child inherits no session history from the parent. The `task` string is its entire context.
- If `share_workspace` is `false` (default), the child gets a fresh empty workspace.
- The child is automatically stopped and cleaned up after the task completes or times out. Its session is retained.

---

### 3.5 `agent_spawn_status`

**Sync/Async:** Async only
**Purpose:** Poll the status of an async `agent_spawn` call.

#### Arguments

```json
{ "run_id": "spawn_4bwmnq" }
```

#### Response

```json
{
  "run_id":      "spawn_4bwmnq",
  "status":      "completed",
  "instance_id": "inst_9pqrst2x",
  "session_id":  "sess_2kxmnqp4",
  "output":      "The Q4 report covers three main areas...",
  "error":       null,
  "started_at":  "2026-03-16T08:06:00.000Z",
  "ended_at":    "2026-03-16T08:06:45.200Z",
  "duration_ms": 45200,
  "usage": { "input_tokens": 3200, "output_tokens": 480, "total_tokens": 3680 }
}
```

`status`: `"running"` | `"completed"` | `"timeout"` | `"error"`

When `status` is `"running"`, `output`, `error`, `ended_at`, `duration_ms`, and `usage` are `null`.

---

### 3.6 `agent_spawn_list`

**Sync/Async:** Async only
**Purpose:** List all active or recent subagent runs for the current session.

#### Arguments

```json
{
  "status_filter": "running",
  "limit": 20
}
```

`status_filter` is optional. Omit to return all statuses.

#### Response

```json
{
  "runs": [
    {
      "run_id":      "spawn_4bwmnq",
      "status":      "running",
      "image":       "researcher:v2",
      "instance_id": "inst_9pqrst2x",
      "started_at":  "2026-03-16T08:06:00.000Z"
    }
  ]
}
```

---

### 3.7 `agents_list`

**Sync/Async:** Async only
**Purpose:** List agent instances that can be targeted via `agent_spawn` or `sessions_send`.

**Scope:** Returns only instances within the current team. Standalone agents calling this tool receive an empty list unless `cross_team` is explicitly granted in `config.toml`.

#### Arguments

```json
{
  "status_filter": "running",
  "role_filter":   "worker"
}
```

Both filters are optional.

#### Response

```json
{
  "instances": [
    {
      "id":     "inst_2nqkrp8x",
      "name":   "researcher-1",
      "image":  "pekohub.com/agents/researcher:v2.5",
      "status": "running",
      "role":   "worker",
      "team_id":"team_4hsdcl8e"
    }
  ]
}
```

---

### 3.8 `agent_info`

**Sync/Async:** Async only
**Purpose:** Query detailed information about a specific agent instance.

**Scope:** Same team-scoping rules as `agents_list`.

#### Arguments

```json
{ "instance_id": "inst_2nqkrp8x" }
```

#### Response

```json
{
  "id":               "inst_2nqkrp8x",
  "name":             "researcher-1",
  "image_ref":        "pekohub.com/agents/researcher:v2.5",
  "image_digest":     "sha256:a3b5c7d9...",
  "status":           "running",
  "role":             "worker",
  "team_id":          "team_4hsdcl8e",
  "active_session_id":"sess_7qrstu3v",
  "created_at":       "2026-03-16T08:00:01.342Z",
  "capabilities": {
    "tools":  ["github", "browser"],
    "skills": ["research"]
  }
}
```

---

### 3.9 `sessions_send`

**Sync/Async:** Both
**Purpose:** Send a message to another session. Intended for human-to-agent and tooling-to-agent communication. **Not for agent-to-agent communication within a team** — use the event bus (A2A) for that.

> **Boundary rule:** An agent must not call `sessions_send` targeting another agent instance within the same team. The runtime enforces this: if the target session belongs to an agent instance in the same team as the caller, the call is rejected with `cross_agent_send_forbidden`. Agent-to-agent communication must go through the bus.

#### Arguments

```json
{
  "session_id": "sess_7qrstu3v",
  "message":    "Please also check the EMEA region numbers.",
  "async":      false,
  "timeout_ms": 60000
}
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `session_id` | string | required | Target session ID |
| `message` | string | required | Message content |
| `async` | boolean | `true` | If false, wait for a response turn to complete |
| `timeout_ms` | integer | 60000 | Max wait for sync mode |

#### Sync response

```json
{
  "message_id": "msg_4xpwqr7n",
  "response":   "EMEA region shows $1.1B, up 8% YoY.",
  "session_id": "sess_7qrstu3v",
  "duration_ms": 8200
}
```

#### Async response

```json
{
  "message_id":  "msg_4xpwqr7n",
  "queued":      true,
  "session_id":  "sess_7qrstu3v"
}
```

---

### 3.10 `sessions_list`

**Sync/Async:** Async only
**Purpose:** List sessions visible to the current instance.

#### Arguments

```json
{
  "kind":             "own",
  "limit":            20,
  "active_within_ms": 300000
}
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `kind` | string | `"own"` | `"own"` = this instance's sessions only; `"team"` = all sessions within the team |
| `limit` | integer | 20 | Max results |
| `active_within_ms` | integer | null | Filter to sessions with activity in the last N ms |

#### Response

```json
{
  "sessions": [
    {
      "id":         "sess_9nrwbf1v",
      "instance_id":"inst_7k2mxp3q",
      "title":      "Q4 earnings analysis",
      "turn_count": 6,
      "updated_at": "2026-03-16T09:05:00.000Z",
      "ended":      false
    }
  ]
}
```

---

### 3.11 `sessions_history`

**Sync/Async:** Async only
**Purpose:** Retrieve message history for a session.

**Scope:** Agents may only access their own sessions or, if in a team, sessions of other instances in the same team.

#### Arguments

```json
{
  "session_id":          "sess_9nrwbf1v",
  "include_tool_calls":  true,
  "include_thinking":    false,
  "limit":               50,
  "cursor":              null
}
```

#### Response

Returns the event array as defined in `API_CONTRACT.md` §5.3 and `DATA_MODEL.md` §5.3.

---

### 3.12 `session_status`

**Sync/Async:** Async only
**Purpose:** Returns current session metadata including token usage and model information.

#### Arguments

```json
{}
```

(No arguments — always returns status for the current session.)

#### Response

```json
{
  "session_id":    "sess_9nrwbf1v",
  "instance_id":   "inst_7k2mxp3q",
  "started_at":    "2026-03-16T08:05:00.000Z",
  "last_active_at":"2026-03-16T09:04:55.000Z",
  "turn_count":    6,
  "model":         "claude-sonnet-4-6",
  "provider":      "anthropic",
  "usage": {
    "input_tokens":  18420,
    "output_tokens": 2280,
    "total_tokens":  20700,
    "context_window_limit": 200000,
    "context_window_used_pct": 10.4
  }
}
```

---

### 3.13 `cron`

**Sync/Async:** Async only
**Purpose:** Manage scheduled jobs from within a session. Registrations are persisted to disk and survive instance restarts.

See §8 for the full cron lifecycle, persistence model, and restart semantics.

#### Sub-commands

**`at`** — Run once at a specific time.

```json
{
  "sub_command": "at",
  "time":        "2026-03-17T09:00:00.000Z",
  "task":        "Send the weekly digest to #reports",
  "label":       "weekly-digest-2026-03-17"
}
```

---

**`every`** — Run on a repeating interval.

```json
{
  "sub_command": "every",
  "interval_ms": 3600000,
  "task":        "Check for new reports in inbox/",
  "label":       "inbox-check",
  "start_at":    null
}
```

`start_at` is optional. If omitted, starts immediately.

---

**`cron`** — Run on a crontab schedule (5-field standard syntax).

```json
{
  "sub_command": "cron",
  "schedule":    "0 9 * * 1-5",
  "task":        "Morning standup summary",
  "label":       "standup",
  "timezone":    "Asia/Tokyo"
}
```

`timezone` is optional, defaults to UTC.

---

**`idle`** — Run when the session has been idle for a given duration.

```json
{
  "sub_command": "idle",
  "idle_ms":     1800000,
  "task":        "Save a session summary to workspace/summaries/",
  "label":       "idle-summary",
  "repeat":      false
}
```

`repeat`: if `true`, fires every time the idle threshold is crossed. If `false` (default), fires once and deregisters.

---

**`event`** — Run when a specific event bus topic matches.

```json
{
  "sub_command": "event",
  "topic":       "team.research-team.tasks",
  "filter":      { "priority": "high" },
  "task":        "Handle high-priority task: {{payload}}",
  "label":       "high-priority-handler"
}
```

`filter` is optional JSON object. Only events whose payload contains all filter key-value pairs will trigger. `{{payload}}` in `task` is replaced with the JSON-encoded event payload.

---

**`list`** — List all registered cron jobs for this instance.

```json
{ "sub_command": "list" }
```

Response:

```json
{
  "jobs": [
    {
      "job_id":     "cron_7kxmnq",
      "label":      "standup",
      "sub_command":"cron",
      "schedule":   "0 9 * * 1-5",
      "task":       "Morning standup summary",
      "status":     "active",
      "last_run_at":"2026-03-16T09:00:00.000Z",
      "next_run_at":"2026-03-17T09:00:00.000Z",
      "run_count":  42
    }
  ]
}
```

---

**`cancel`** — Cancel a registered job by label or job ID.

```json
{
  "sub_command": "cancel",
  "label":       "standup"
}
```

Or by `job_id`:

```json
{
  "sub_command": "cancel",
  "job_id":      "cron_7kxmnq"
}
```

Response: `{ "cancelled": true, "job_id": "cron_7kxmnq" }`

---

#### Cron response (for all registration sub-commands)

```json
{
  "job_id":     "cron_7kxmnq",
  "label":      "standup",
  "status":     "registered",
  "next_run_at":"2026-03-17T09:00:00.000Z"
}
```

---

### 3.14 Built-in Tool Summary

| Tool | Sync | Async | Scope | Notes |
|------|------|-------|-------|-------|
| `filesystem` | — | ✓ | Sandboxed paths | See §7 for sandbox rules |
| `process` | ✓ | ✓ | Sandboxed cwd | Shell invocation blocked |
| `apply_patch` | — | ✓ | Sandboxed paths | Atomic; backs up files |
| `agent_spawn` | ✓ | ✓ | Team-scoped | Child cleaned up after task |
| `agent_spawn_status` | — | ✓ | Own spawns | Poll async spawn |
| `agent_spawn_list` | — | ✓ | Own spawns | List current session's spawns |
| `agents_list` | — | ✓ | Team-scoped | Lists running instances in team |
| `agent_info` | — | ✓ | Team-scoped | Detailed instance info |
| `sessions_send` | ✓ | ✓ | Non-team only | No agent-to-agent within team |
| `sessions_list` | — | ✓ | Own or team | |
| `sessions_history` | — | ✓ | Own or team | |
| `session_status` | — | ✓ | Current session | No args |
| `cron` | — | ✓ | Instance-owned | Persisted; see §8 |

---

## 4. MCP Integration

### 4.1 Bundled MCP Servers

These MCP servers are maintained by the Pekobot project and distributed separately from the runtime binary. They are installable via the capability ecosystem.

| Package | Provides | Notes |
|---------|----------|-------|
| `mcp-web` | `web_search`, `fetch`, `http` | Web search, URL fetch, raw HTTP |
| `mcp-browser` | `browser_navigate`, `browser_click`, `browser_type`, `browser_screenshot` | Playwright-based browser automation |
| `mcp-memory` | `memory_store`, `memory_retrieve`, `memory_search`, `memory_delete` | Persistent vector memory |

These are not built into the runtime. An agent that needs `web_search` must declare `mcps = ["mcp-web"]` in `config.toml`.

### 4.2 Tool Name Conflict Resolution

If a built-in tool and an MCP tool share the same name, the built-in takes precedence. A warning is logged. To prefer the MCP tool over the built-in, disable the built-in:

```toml
[capabilities]
disabled_tools = ["filesystem"]
mcps = ["my-custom-filesystem-mcp"]
```

### 4.3 MCP Tool Visibility

By default all tools exposed by an MCP server are registered with the LLM. To restrict which tools from an MCP are visible, use the `allow` filter in `mcp.json`:

```json
{
  "mcpServers": {
    "mcp-web": {
      "command": "pekobot-mcp-web",
      "allow_tools": ["web_search", "fetch"]
    }
  }
}
```

### 4.4 Shared MCP Servers

When an MCP server is declared under `[[shared.mcps]]` in `team.toml`, the runtime:

1. Starts exactly one instance of the MCP server process for the team.
2. Increments a reference count when each agent instance connects.
3. Decrements the reference count when an agent instance disconnects or stops.
4. Tears down the MCP server process when the reference count reaches zero.

The MCP server process sees all tool calls from all agents. Tool call routing is handled by the runtime proxy — agents do not communicate with the MCP process directly.

#### MCP Server Crash Recovery

If a shared MCP server process crashes:

| Phase | Behavior |
|-------|----------|
| Detection | Runtime detects crash within 1 second via process monitor |
| In-flight calls | Active tool calls receive `error: "mcp_server_disconnected"` with `duration_ms` indicating partial execution |
| Queued calls | Calls not yet sent to MCP are held pending restart |
| Restart | Automatic restart attempted (up to 3 times with 5-second exponential backoff) |
| Success | After successful restart, queued calls are processed; normal operation resumes |
| Failure after 3 attempts | MCP server marked as `failed`; queued calls rejected with `mcp_server_unavailable` |
| Agent notification | A `system` session event is emitted: `{ "event": "mcp_server_status", "server": "browser", "status": "failed" }` |
| Tool availability | Tools from the failed MCP are removed from the agent's tool list; subsequent tool calls return `tool_unavailable` |

**Recovery by agents:** Agents should handle `mcp_server_disconnected` errors gracefully. The session continues with remaining available tools.

---

## 5. Skills

### 5.1 What a Skill Is

A skill is a named, reusable bundle of:

- A markdown prompt file (`skill.md`) that is appended to the system prompt when the skill is active.
- An optional set of tool dependencies the skill requires.
- An optional set of example interactions.

Skills are not tools — the LLM does not "call" a skill. Skills are injected into context at session startup when declared in `config.toml`.

### 5.2 Skill Directory Layout

```
skills/
└── research/
    ├── skill.md           # Prompt injected into system context
    ├── skill.toml         # Metadata and tool dependencies
    └── examples/          # Optional few-shot examples
        ├── 01_input.md
        └── 01_output.md
```

### 5.3 skill.toml

```toml
[skill]
name        = "research"
version     = "1.0.0"
description = "Systematic research and citation skills"

[skill.dependencies]
tools = ["web_search", "fetch"]   # MCPs or built-ins this skill needs
mcps  = ["mcp-web"]
```

### 5.4 System Prompt Injection Order

When skills are active, their `skill.md` content is injected after the agent's own markdown files, in declaration order:

```
1. Agent markdown files (BOOTSTRAP, IDENTITY, SOUL, AGENT, TOOLS, USER, SKILLS)
2. skill.md for each declared skill, in config.toml declaration order
3. Few-shot examples, if present
```

### 5.5 Skill Inheritance

When a base image declares skills, the derived image inherits them. The derived image may add more skills but cannot remove inherited ones.

---

## 6. Session Plugins

### 6.1 Purpose

Session plugins intercept the session event stream at read and write time. They are used for:

- **Compression** — store `assistant.message` content in compressed form on disk.
- **Summarisation** — on context build, summarise older turns to reduce token usage.
- **Encryption** — encrypt sensitive events at rest.
- **Filtering** — strip certain event types from the context build (e.g. exclude raw tool outputs).

### 6.2 Loading

Session plugins are declared in `config.toml`:

```toml
[capabilities.session]
plugin = "lossless-compression"
```

The runtime loads the plugin's shared library at instance startup via `libloading`. The library must export a `create_plugin() -> Box<dyn SessionPlugin>` symbol.

### 6.3 Guarantees Required of Plugins

- `on_write` and `on_read` must be inverses. If `on_write` compresses content, `on_read` must decompress it. A plugin that breaks this invariant will corrupt the session.
- `on_context_build` must return a valid subsequence or transformation of the input events. It must not inject events that were not in the original session.
- All three methods must be deterministic and side-effect free (no network calls, no disk writes).
- Panic in a plugin is caught by the runtime and treated as a plugin error. The runtime falls back to raw (unmodified) events and logs a `system` event of `event: "plugin_error"` to the session.

### 6.4 Runtime Version Compatibility

Session plugins are compiled as shared libraries and loaded dynamically. ABI compatibility is guaranteed only within major runtime versions.

| Runtime Version | Plugin Compatibility |
|-----------------|---------------------|
| Same major, same or older minor | ✅ Fully compatible |
| Same major, newer minor | ⚠️ May use deprecated APIs; warning logged |
| Different major | ❌ Plugin rejected with `incompatible_plugin_version` error |

**Plugin manifest requirements:**

Plugins must declare their ABI version in `capability.toml`:
```toml
[session]
library = "libsession_lossless.so"
abi_version = "1.0"  # Must match runtime's plugin ABI version
```

**Version detection:**
- Runtime exposes its plugin ABI version via `GET /info` → `capabilities.plugin_abi_version`
- Plugin loader checks `abi_version` before loading; mismatch aborts with structured error:
  ```json
  {
    "error": {
      "code": "incompatible_plugin_version",
      "message": "Plugin compiled for ABI 2.0, runtime supports 1.0",
      "details": {
        "plugin_abi": "2.0",
        "runtime_abi": "1.0"
      }
    }
  }
  ```

**Best practice:** Plugin authors should compile against the oldest supported runtime version and test against the latest.

---

## 7. Security and Sandbox Model

### 7.1 Sandbox Levels

The sandbox level is configured per-agent in `config.toml`:

```toml
[resources]
sandbox = "none"   # "none" | "paths" | "namespace" | "container"
```

| Level | Description | Use case |
|-------|-------------|----------|
| `none` | No restriction. Tools run as the daemon process user | Dev mode, trusted local agents |
| `paths` | Filesystem access restricted to granted paths (default) | Most agents |
| `namespace` | Linux network and PID namespaces | Agents running untrusted code |
| `container` | OCI runtime (runc). Full process and network isolation | Production, high-trust boundary |

Default is `paths` for all agents.

### 7.2 Filesystem Sandbox (`paths` level)

At `paths` level, all `filesystem` and `apply_patch` operations are validated against the allowed path list before execution. The allowed paths are:

1. Instance private workspace: `.pekobot/agents/<n>/workspace/` (read/write, always granted)
2. Team shared workspace: `.pekobot/teams/<t>/shared/files/` (read/write, if in a team)
3. Agent image `projects/` content (read-only, always granted)
4. Additional grants from `[capabilities.grants]` in `config.toml`

Path traversal attempts (e.g. `../../etc/passwd`) are rejected with `SandboxViolation` after canonicalisation.

#### Symlink Handling

Symlinks are resolved during canonicalisation with the following rules:

| Symlink Location | Target Resolution | Allowed? |
|------------------|-------------------|----------|
| Inside sandbox | Points inside sandbox | ✅ Yes |
| Inside sandbox | Points outside sandbox | ❌ No (`SandboxViolation`) |
| Outside sandbox | Any target | ❌ No (`SandboxViolation` - cannot access symlink itself) |

**Example:** If `/workspace/link` is a symlink pointing to `/etc/passwd`:
- `filesystem.read("link")` → Resolves to `/etc/passwd` → `SandboxViolation`
- `filesystem.read("link")` where `link` → `/workspace/file.txt` → ✅ Allowed

**Detection:** Symlinks are detected via `lstat` before path resolution. The canonicalisation process follows symlinks and validates the final resolved path is within the sandbox.

### 7.3 Process Sandbox (`process` tool)

Regardless of sandbox level, `process` enforces:

- No shell execution (`sh`, `bash`, `zsh`, `cmd`, `powershell` as command are blocked).
- No access to parent process environment variables matching `*_API_KEY`, `*_SECRET`, `*_TOKEN`, `*_PASSWORD`.
- Working directory (`cwd`) must canonicalise to a path within the filesystem sandbox.
- Process inherits a minimal `PATH` and no other inherited environment unless explicitly set in `env`.

### 7.4 Cross-Team Access

By default, agent tools (`agents_list`, `agent_info`, `agent_spawn`) are scoped to the current team. To allow cross-team access, add an explicit grant:

```toml
[capabilities.grants]
cross_team_agent_access = true
```

This grant should be rare. Use it only for orchestrator agents that span multiple teams.

### 7.5 Sensitive Environment Variables

API keys and secrets are never passed to tool processes. They are available only within the runtime's own Rust code (for LLM API calls). This is enforced by the `process` tool's env filter, not by convention.

---

## 8. Cron Lifecycle

### 8.1 Persistence

Cron registrations are persisted to the instance workspace at:

```
.pekobot/agents/<n>/cron.json
```

Or for team agents:

```
.pekobot/teams/<t>/agents/<n>/cron.json
```

The file is written atomically (write to `.tmp`, then rename) every time a job is registered or cancelled.

### 8.2 Schema

```json
{
  "schema_version": 1,
  "instance_id": "inst_7k2mxp3q",
  "jobs": [
    {
      "job_id":      "cron_7kxmnq",
      "label":       "standup",
      "sub_command": "cron",
      "schedule":    "0 9 * * 1-5",
      "timezone":    "Asia/Tokyo",
      "task":        "Morning standup summary",
      "status":      "active",
      "created_at":  "2026-03-01T09:00:00.000Z",
      "last_run_at": "2026-03-16T09:00:00.000Z",
      "next_run_at": "2026-03-17T09:00:00.000Z",
      "run_count":   42,
      "error_count": 0
    }
  ]
}
```

### 8.3 Restart Semantics

When an instance starts (cold start or restart after crash), the daemon:

1. Loads `cron.json` from the instance workspace.
2. For each `active` job, recomputes `next_run_at` from the current time.
3. For `at` (one-shot) jobs whose `time` is in the past: if `last_run_at` is null (never ran), run immediately. If `last_run_at` is set, the job already ran — mark it `completed` and skip.
4. For `every` and `cron` jobs: schedule from `next_run_at` (which will be in the past after a crash — fire immediately, then continue schedule).
5. For `idle` jobs: reset idle timer from zero on restart.
6. For `event` jobs: re-subscribe to the event bus topic.

### 8.4 Job Execution

When a cron job fires, the daemon:

1. Checks if the instance is `running`. If not, the job is queued for up to `job_queue_ttl_seconds` (default 300). If the instance is still not running after TTL, the job is skipped and `error_count` is incremented.
2. Creates a new session (or uses the active session if `session = "active"` in the hook config).
3. Sends the `task` string as the first user message, with `source: "hook"` in the session event.
4. Updates `last_run_at`, `next_run_at`, and `run_count` in `cron.json`.

### 8.5 Image Upgrade Behaviour

When an instance is upgraded to a new image (`pekobot instance upgrade`):

- All cron jobs are preserved. They are owned by the instance, not the image.
- If the new image's `config.toml` adds new hook-defined jobs (via `[[hooks]]`), they are merged into `cron.json` as new entries.
- Hook-defined jobs from the old image that are no longer present in the new image's `config.toml` are marked `orphaned` in `cron.json` but not deleted. The operator must explicitly cancel them.

### 8.5 Cron Job Concurrency

When a cron job fires while a previous execution of the same job is still running:

| `sub_command` | Concurrency Behavior |
|---------------|---------------------|
| `at` (one-shot) | **Skip**: One-shot jobs that are still running are skipped; `error_count` incremented |
| `every` (interval) | **Queue**: New execution queued; runs immediately after current completes. Max queue depth: 1 (additional triggers dropped) |
| `cron` (scheduled) | **Queue**: Same as `every` |
| `idle` | **Skip**: Idle timer reset; fires again when idle threshold is crossed after current execution completes |
| `event` | **Concurrent**: Event-triggered jobs always execute; may result in concurrent sessions for the same instance |

**Job state tracking:** Each job tracks:
- `status`: `"active"`, `"running"`, `"completed"`, `"failed"`
- `last_run_at`: Timestamp of last execution start
- `run_count`: Total executions
- `error_count`: Failed executions (including skipped due to overrun)

**Overrun detection:** When a job fires:
1. If `status == "running"`, apply concurrency rules above
2. If queued job not processed within `job_queue_ttl_seconds` (default: 300), mark as failed

### 8.6 Ownership and Isolation

- Cron jobs are owned by the instance that registered them. An agent cannot register a cron job that targets a different instance.
- Jobs are not visible to other agents in the team.
- The `cron.list` sub-command only returns jobs for the calling instance.

---

## 9. Capability Resolution and Loading

### 9.1 Resolution Order

When an agent declares `tools = ["browser"]` in `config.toml`, the runtime resolves it in this order:

1. **Built-in tools** — if a built-in with that name exists and is not in `disabled_tools`, use it.
2. **Agent-local `tools/` directory** — if an executable named `browser` or `browser.py` etc. exists in the image's `tools/` folder, use it.
3. **Installed capabilities** — search `~/.pekobot/capabilities/tools/` for a capability named `browser` (any version unless pinned).
4. **Not found** — if `capabilities.options.auto_install = true`, attempt to fetch from the configured registry. Otherwise, fail startup with `capability_not_installed`.

MCP servers follow the same order for steps 2–4.

### 9.2 Version Pinning

```toml
[capabilities]
tools = ["browser@1.2.0"]    # Exact version
mcps  = ["mcp-web@^1.0"]     # Semver range (caret = compatible)
```

Unpinned references resolve to the latest installed version. If multiple versions are installed, the latest is used.

### 9.3 Startup Validation

At instance startup, before the agentic loop begins, the runtime:

1. Resolves all declared capabilities.
2. Validates tool parameter schemas are valid JSON Schema.
3. For MCP servers: starts the process and verifies it responds to `list_tools` within 5 seconds.
4. For session plugins: loads the shared library and verifies the exported symbol.
5. If any step fails and `auto_install` is false, instance startup is aborted with a structured error listing all missing or broken capabilities.

---

## 10. Changelog

| Version | Date | Changes |
|---------|------|---------|
| 1.0 | 2026-03-16 | Initial draft. Built-in tool set finalised. `message` tool moved to MCP. `sessions_send` scoping rules defined. Cron lifecycle specified. |

---

*Version: 1.0 · Last Updated: 2026-03-16 · Status: Draft*
