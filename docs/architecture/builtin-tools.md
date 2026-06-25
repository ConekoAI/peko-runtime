# Built-in Tools Catalog

This document is the source of truth for peko-runtime's built-in tool surface.
It is organized around the Claude Code core tool parity program: tools that
match Claude's name and schema exactly are marked ✅; peko extensions are
marked 🔧.

## Legend

- **✅ Claude parity** — name, schema, and return shape match Claude Code's
core built-in tool.
- **🔧 Peko extension** — intentionally diverges from Claude Code (extra
parameter, extra behavior, or no Claude equivalent).
- **⏳ Pending** — not yet implemented on the parity branch.

## Filesystem tools

### `Read` ✅

Read file contents with optional line ranges and binary support.

```json
{
  "file_path": "string (required)",
  "offset": "integer? (1-based line)",
  "limit": "integer?",
  "pages": "string? (PDF only, e.g. '1-5')"
}
```

Return:
```json
{
  "content": "string",
  "path": "string",
  "size_bytes": "integer",
  "encoding": "utf8 | base64",
  "total_lines?": "integer",
  "start_line?": "integer",
  "end_line?": "integer"
}
```

**Peko extensions:** binary auto-detection with base64 return.

### `Write` ✅

Write or append content to files.

```json
{
  "file_path": "string (required)",
  "content": "string (required)"
}
```

Return:
```json
{
  "path": "string",
  "bytes_written": "integer",
  "size_bytes": "integer",
  "mode": "overwrite | append | create_new",
  "encoding": "utf8 | base64"
}
```

**Peko extension:** `mode` field. Claude Code errors when writing an existing
file; peko defaults to `overwrite` and also supports `append` and `create_new`.

### `Edit` ✅

Targeted string replacement in files.

```json
{
  "file_path": "string (required)",
  "old_string": "string (required)",
  "new_string": "string (required)",
  "replace_all": "boolean (default false)"
}
```

Return:
```json
{
  "path": "string",
  "replacements": [{ "old": "string", "new": "string", "occurrences": "integer" }]
}
```

## Shell

### `Bash` ✅

Execute shell commands.

```json
{
  "command": "string (required)",
  "description": "string?",
  "run_in_background": "boolean (default false)",
  "timeout": "integer? (ms)"
}
```

Return (blocking):
```json
{
  "exit_code": "integer",
  "stdout": "string",
  "stderr": "string",
  "success": "boolean"
}
```

Return (background): async task receipt.

**Peko extension:** `cwd` parameter for per-tool working directory.

## Scheduling

### `CronCreate` ✅

Create a scheduled job.

```json
{
  "cron": "string (5-field cron)",
  "prompt": "string (required)",
  "recurring": "boolean (default true)",
  "durable": "boolean (default false)"
}
```

**Peko extensions:** `at`, `interval_ms`, `start_at`, `timezone`, `idle_ms`,
`event_topic`, `event_filter`.

### `CronDelete` ✅

```json
{ "id": "string (required)" }
```

### `CronList` ✅

No parameters. Returns an array of jobs.

## Agent control

### `Agent` ✅

Spawn a subagent.

```json
{
  "prompt": "string (required)",
  "subagent_type": "string (required)",
  "description": "string?",
  "model": "string?"
}
```

**Peko extensions:** `cleanup` (`keep` | `delete`), `parent_session_key`,
`isolated`.

`subagent_type` resolves to an agent directory under `~/.peko/agents/`.

## Planning todos

### `TaskCreate` ✅

Create a planning todo item.

```json
{
  "subject": "string (required)",
  "description": "string?",
  "activeForm": "string?"
}
```

### `TaskGet` ✅

```json
{ "taskId": "string (required)" }
```

### `TaskList` ✅

```json
{
  "status_filter": "string? (pending | in_progress | completed)"
}
```

### `TaskUpdate` ✅

```json
{
  "taskId": "string (required)",
  "status": "pending | in_progress | completed",
  "owner": "string?"
}
```

Planning todos are stored in a per-session `todos.jsonl` sidecar.

## Async execution control

### `AsyncSpawn` 🔧

Start any tool asynchronously and receive a task receipt.

```json
{
  "tool": "string (required)",
  "params": "object (required)",
  "label": "string?"
}
```

### `AsyncOutput` 🔧

Read output from a running async task.

```json
{
  "task_id": "string (required)",
  "block": "boolean (default false)",
  "timeout": "integer? (ms)",
  "tail_lines": "integer?"
}
```

### `AsyncStop` 🔧

```json
{ "task_id": "string (required)" }
```

### `AsyncStatus` 🔧

```json
{ "task_id": "string (required)" }
```

### `AsyncList` 🔧

```json
{
  "status_filter": "string?",
  "tool_filter": "string?"
}
```

These tools map semantically to Claude Code's `TaskOutput` / `TaskStop` but use
a peko-specific namespace because peko's async model is more general (any tool
can be spawned async, not just Bash).

## Out of scope

The following peko tools are intentionally not part of the Claude core subset
parity program:

- `glob`, `grep` — peko-specific filesystem helpers.
- `session` — peko-specific session introspection.
- `message` — peko-specific channel messaging.
- `a2a_send` — peko A2A protocol messaging.
- MCP-provided tools (`web_search`, `fetch`, etc.) — provided via MCP servers.
- Skills — still prompt-injected via the `prompt:skills` hook.

## Configuration gates

| Family | Factory flag | Registrar flag | Default |
|---|---|---|---|
| Filesystem | `enable_granular_fs` / `enable_granular_write` | same | `true` |
| Shell | `enable_shell` | `enable_shell` | `true` |
| Cron | `enable_cron` | `enable_cron` | `true` |
| Agent | (per-agent registration) | (per-agent registration) | `true` |
| Async control | `enable_async_tools` | `enable_async_tools` | `true` |
| Planning todos | `enable_task_tools` | `enable_task_tools` | `true` |

All tools also respect `disabled_tools: Vec<String>`.

## Related

- `src/tools/registry/factory.rs` — synchronous tool factory configuration.
- `src/extensions/builtin/adapter.rs` — production built-in tool registration.
- `src/extensions/framework/adapters/mod.rs` — canonical built-in tool name lists.
