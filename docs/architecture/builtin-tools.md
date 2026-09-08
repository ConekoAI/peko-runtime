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

### `Read` 🔧

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

**Peko extensions:** `encoding` parameter to force base64 (Claude auto-detects
binary); binary auto-detection with base64 return.

### `Write` 🔧

Write or append content to files.

```json
{
  "file_path": "string (required)",
  "content": "string (required)",
  "mode": "create_new (default) | overwrite | append",
  "encoding": "utf8 (default) | base64"
}
```

Return:
```json
{
  "path": "string",
  "bytes_written": "integer",
  "size_bytes": "integer",
  "mode": "create_new | overwrite | append",
  "encoding": "utf8 | base64"
}
```

**Peko extensions:** `mode` and `encoding` parameters. The default mode is
`create_new` to match Claude Code's safety invariant (writing an existing
file errors). `overwrite` and `append` remain opt-in peko extensions.

### `Edit` 🔧

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
  "replacements": [{ "old": "string", "new": "string", "occurrences": "integer" }],
  "total_replacements": "integer",
  "success": "boolean"
}
```

**Peko extensions:** the return object includes `total_replacements` and
`success` top-level fields, plus `success`/`error` per replacement. The
canonical Claude shape is `{ path, replacements: [...] }`; the extras are
non-breaking but make the tool 🔧.

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

### `CronCreate` 🔧

Schedule future work. Two mutually exclusive job shapes:

- **`message`** — an instruction delivered to the principal's trunk
  session at fire time, running a full agent turn. Output is composed
  fresh on every fire (LLM-driven, costs tokens per fire); the agent
  reaches the user via `ChannelSend`. Use for reminders/pings whose
  text should vary.
- **`tool` + `params`** — a fixed tool call at fire time (no LLM cost).
  The daemon asks the `AsyncExecutor` to run `tool_name` with
  `tool_params` verbatim.

```json
{
  "message": "string (mode 1 — mutually exclusive with tool)",
  "tool": "string (mode 2, e.g. \"Agent\", \"Bash\", \"ChannelSend\")",
  "params": "object (mode 2, defaults to {})",
  "wake_on_completion": "boolean (SpawnTool only, default false)",
  "timeout_secs": "integer (SpawnTool only, default 7200s)"
}
```

**Peko extensions:** the schema does not require `cron` because peko
supports multiple schedule kinds (`at`, `interval_ms`, `idle_ms`). To
schedule a classic cron job, supply `cron`; otherwise supply one of
the extension fields. Extra fields:
`label`, `at`, `interval_ms`, `timezone`, `idle_ms`.

**Sprint 7 Commit D (2026-08-21)** restricted the tool to SpawnTool
jobs only — `prompt` / `message` / `target` / `description` /
`recurring` / `durable` / `task` were dropped from `CronCreateArgs`,
and the `CronJobAction::Notify` variant + the engine's `run_notify_job`
were deleted. **`message` was restored (2026-09-07)** after the
retirement of the `peko cron` CLI left the `Send` fire path with no
writer at all — `tool="Agent"` per fire proved fragile (the Agent tool
is only registered once an agent run has happened, and its fixed
`path` collides on repeat fires), while the trunk `Send` turn is the
designed dynamic path. `target` stays dropped: the trunk is the only
destination.

### `CronDelete` 🔧

```json
{
  "id": "string?",
  "label": "string? (peko extension)"
}
```

**Peko extensions:** accepts `label` as an alternative to `id` (the schema
uses `oneOf` rather than requiring `id`). The canonical Claude call passes
`id` only. **Sprint 7 Commit C** dropped the legacy `job_id` alias.

### `CronUpdate` 🔧

Patch a scheduled job's mutable fields by `id` (or `label`):

```json
{
  "id": "string",
  "label": "string? (alternative to id)",
  "enabled": "boolean? (pause/resume; re-enabling resets the failure budget)",
  "wake_on_completion": "boolean? (subscribe/unsubscribe the trunk inbox to each fire's result; tool jobs only)"
}
```

At least one of `enabled` / `wake_on_completion` is required. Use this to
unsubscribe from a noisy job's results, or to resume a paused job without
recreating it.

### `CronTrigger` 🔧

Fire a scheduled job immediately, out of schedule, by `id` (or `label`).
Works even when the job is **disabled** — this is the way to verify a
freshly-created job's wiring before its first scheduled fire. The run
executes in the background; a fire against a running job coalesces into
the in-flight run (returns its `run_id`).

```json
{ "id": "string", "label": "string? (alternative to id)" }
```

Returns `{ "triggered": true, "job_id", "run_id", "note" }`. Check the
outcome with `CronHistory`.

### `CronHistory` 🔧

Read a job's run history by `id` (or `label`), most recent first:

```json
{ "id": "string", "label": "string?", "limit": "integer? (default 10, max 50)" }
```

Returns `{ "job_id", "count", "runs": [...] }` where each run carries
`status`, `started_at`, `finished_at`, `output`, and `error` — the error
text and trend that `CronList`'s single `last_status` doesn't show.

### `CronList` 🔧

```json
{}
```

Returns:
```json
{
  "jobs": [ ... ],
  "count": "integer"
}
```

**Peko extensions:** the return is wrapped as `{ jobs, count }` instead of a
bare array. Sprint 7 Commit A dropped `status_filter` / `kind_filter`
(declared + schema'd but never read in `execute_with_context`).

## Agent control

### `Agent` ✅

Spawn a subagent.

```json
{
  "action": "new | resume | compact (default new)",
  "prompt": "string (required for all actions — for compact, the task the session continues with after compacting)",
  "agent": "string (required for all actions) — agent template name",
  "path": "string (required for new + resume + compact)",
  "model": "string? (ignored for compact)"
}
```

**Peko extensions:** `action` (3-value enum: `new` | `resume` | `compact`),
`path` (a uniform session address — see below; replaces the Claude Code
`session_key` / `name` pair).

**Path addressing (2026-09-08).** `path` is a uniform address for all
actions: a RELATIVE slug segment (no `/`) resolves against the caller's
session (`<caller>/<slug>`); an ABSOLUTE `/a/b` path resolves from the
tree root. `new` is create-or-resume: when the addressed spawn-created
session exists, the call attaches to it (recurring callers keep one
continuous session); multi-segment absolute paths can only attach, not
mint. Any session in the principal's store may be addressed (e.g. a
peer's `/user-bob`) — the principal is the trust boundary, not the
session tree.

`compact` (2026-09-05) no longer flags the session for a later run — it
starts a continuation run on the target immediately: the run
force-compacts first (phase `standalone_turn`, bypassing the threshold /
cooldown gates), then processes `prompt` against the compacted history,
and the tool returns the run's outcome like `resume` does. Unlike
`resume`, the target need not be a spawned session — any session in the
caller's tree compacts (but never the caller's own session or an
ancestor; the engine compacts those automatically).

`agent` resolves to a Markdown file at
`<workspace>/agents/<agent>/AGENT.md` (directory layout) or
`<workspace>/agents/<agent>.md` (flat layout). The Markdown supplies the
spawned subagent's system prompt body; the frontmatter supplies name +
description. **Sprint 8** renamed the LLM-facing field from
`subagent_type` to `agent` to match its semantic and retired the legacy
global TOML fallback (`{PEKO_HOME}/agents/<name>/config.toml`).

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
- `ChannelSend` — peko channel write primitive: one tool with a typed-prefix `channel` parameter that selects the dispatch —
  - `chan_<8 base36>`: bare post to the named channel (the original `ChannelSend` shape);
  - `principal:<did>`: principal-to-principal RPC over the pair's standing DM channel (reply awaited up to 1 minute, mirrored back onto the caller's own DM channel; cross-runtime via the 12a/12b invite/mirror fan-out);
  - `user:<id>`: fire-and-forget note to a human peer (delivered as a labeled session note by the originating agent or any subagent, gated to the originating user of the current run);
  - `group:<slug>`: fire-and-forget post to a named group channel. Groups are multi-principal, multi-user channels (ADR-049): principal-authored posts never wake other members (D4 loop safety — members read on their own rhythm via `ChannelRead`); a `user:*`-authored root post wakes every member principal, each in its own per-`(principal, channel)` session, and the reply posts back to the group.
  The legacy `send_peer` tool (sprint 2 rename of `principal_send`, itself the successor to `a2a_send` from ADR-023) is retired in sprint 4 — its principal branch (RPC) and user branch (messenger note) are now reachable through the `principal:<did>` and `user:<id>` channel-id forms respectively. The signed-RPC `PrincipalToPrincipalRequest` stack was retired in sprint 3 Phase 12b.
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
