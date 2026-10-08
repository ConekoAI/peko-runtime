# Built-in Tools Catalog

Peko exposes **37 built-in tool names**, all in PascalCase. This reference
describes the compiled Tool implementations; the emitted descriptions and
JSON Schemas in source are the executable contract. MCP and workspace tools
have their own names and schemas and are outside this inventory.

Every registered tool appears in the native wire catalog (ADR-066). ToolCatalog
owns registration/lookup; ToolDispatcher validates arguments and emits one
attributed audit event. Presence = visibility = executability; legacy principal
capability grants do not filter tools.

## Naming and compatibility

| Previous wire name | Canonical wire name |
|---|---|
| session | Session |
| model_list | ModelList |
| role_catalog | RoleCatalog |

Legacy spellings resolve to the corresponding built-in when no exact tool name
matches. They are lookup aliases, not additional wire catalog entries. Exact
workspace/MCP names retain precedence; aliases do not cross principal scopes.
Rust module names, configuration keys such as enable_model_list, IPC operation
tags, action values, and parameter names retain their existing spelling.

## Registration and lifetimes

[installation.rs](../../peko-rs/core/src/tools/installation.rs) owns the factories,
installation phases, and the 37-name inventory. Metadata-only inventories and
scope checks derive from its manifest.

| Lifetime | Tools | Installation |
|---|---|---|
| Runtime defaults | Read/Write/Edit/Glob/Grep/Bash, Cron*, ChannelRead | Runtime startup; fill missing defaults without replacing configured instances |
| Daemon services | ModelCall, Workflow, caller-aware Session and Agent | After PrincipalManager and daemon services exist |
| Principal workspace | Skill, RoleCatalog | Once per principal; RoleCatalog scans current role files on invocation |
| Principal services | Task*, Plan*, ChannelSend, Async* | When their session storage, plan, caller identity/channel, or inbox bindings become available |
| Run bindings | Agent, Session, ModelList | Private catalog overlay; ModelList requires its flag and catalog |

Each run inherits live runtime/principal registrations through its overlay.
Installing a run executor never replaces another run's binding. Workflow/IPC
callbacks resolve the active overlay by the attributed principal and caller
session; when no run is live, daemon Agent/Session adapters resolve principal
services per call. All calls use the same dispatcher implementation, hooks,
audit sink, and timeout router. Run admission remains principal-wide. ChannelSend
keeps principal identity/reply locks and resolves the current tunnel context per
call, including connections made after installation.

Async executors and task registries belong to the principal, so receipts remain
resolvable after a run ends. AsyncSpawn stamps the caller session on each task;
completion events go to that session's inbox. Background Bash and subagent tasks
remain accessible through the existing principal-filtered registry fallback.
These lifetimes do not introduce a new authorization boundary; the principal
remains the trust boundary. See [ADR-069](adr/ADR-069-builtin-tool-installation-lifetimes.md).

## Parameter conventions

Required below means required by the top-level schema. Conditional requirements
are stated under each tool and encoded in its schema. Defaults may be applied
by runtime code rather than JSON Schema; schema defaults alone do not inject values.

## Files and shell

### Read

Read text with inline line numbers, or binary content as base64.

[Implementation](../../peko-rs/core/src/tools/builtin/fs/read.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `file_path` | string | yes | — |
| `offset` | integer | no | ≥ 1 |
| `limit` | integer | no | ≥ 1 |
| `encoding` | utf8 \| base64 | no | — |

Offset is 1-based. Omitting limit reads through EOF. Encoding defaults to utf8; binary content is detected automatically. There is no PDF pages selector.

### Write

Create, overwrite, or append files; creates parent directories.

[Implementation](../../peko-rs/core/src/tools/builtin/fs/write.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `file_path` | string | yes | — |
| `content` | string | yes | — |
| `mode` | overwrite \| create_new \| append | no | default "overwrite" |
| `encoding` | utf8 \| base64 | no | default "utf8" |

Mode defaults to overwrite; use create_new to refuse an existing file. Parent directories are created automatically.

### Edit

Replace exact text; requires a unique match unless replace_all is true.

[Implementation](../../peko-rs/core/src/tools/builtin/fs/edit.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `file_path` | string | yes | — |
| `old_string` | string | yes | — |
| `new_string` | string | yes | — |
| `replace_all` | boolean | no | default false |

old_string must match exactly and occur once unless replace_all is true.

### Glob

Find files and optionally directories by glob pattern.

[Implementation](../../peko-rs/core/src/tools/builtin/fs/glob.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `pattern` | string | yes | — |
| `path` | string | no | — |
| `include_hidden` | boolean | no | default false |
| `include_dirs` | boolean | no | default false |
| `limit` | integer | no | default 1000, ≥ 1, ≤ 10000 |

path defaults to the calling workspace. An explicit path overrides workspace injection.

### Grep

Regex search with context, filename-only, or count output.

[Implementation](../../peko-rs/core/src/tools/builtin/fs/grep.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `pattern` | string | yes | — |
| `path` | string | no | — |
| `include` | string | no | — |
| `limit` | integer | no | default 100, ≥ 1, ≤ 1000 |
| `include_content` | boolean | no | default true |
| `context_before` | integer | no | default 0, ≥ 0, ≤ 10 |
| `context_after` | integer | no | default 0, ≥ 0, ≤ 10 |
| `context` | integer | no | ≥ 0, ≤ 10 |
| `case_insensitive` | boolean | no | default false |
| `include_hidden` | boolean | no | default false |
| `output_mode` | content \| files_with_matches \| count | no | default "content" |

context overrides context_before/context_after. Content/context controls apply only to content mode. Output is a plain-text string with accompanying JSON metadata.

### Bash

Execute shell commands, synchronously or in the background.

[Implementation](../../peko-rs/core/src/tools/builtin/bash.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `command` | string | yes | — |
| `description` | string | no | — |
| `cwd` | string | no | — |
| `run_in_background` | boolean | no | default false |
| `timeout` | integer | no | ≥ 1 |
| `max_output_bytes` | integer | no | ≥ 1 |

timeout is milliseconds. max_output_bytes defaults to 100000 per stream and is ignored in background mode. Background execution returns an async receipt.

## Agents, roles, and skills

### Agent

Run work in a new, resumed, compacted, or branched session.

[Implementation](../../peko-rs/core/src/tools/builtin/messaging/agent.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `action` | new \| resume \| compact \| branch | no | default "new" |
| `prompt` | string | yes | nonempty |
| `role` | string | yes | nonempty |
| `path` | string | yes | nonempty |
| `model` | string | no | — |
| `source` | string | no | — |
| `overwrite` | boolean | no | — |
| `page_limit` | integer | no | ≥ 1, ≤ 10000 |

All four actions require nonempty prompt, role, and path. new is create-or-resume; new/branch accept a single relative slug or an absolute session address, while resume/compact use absolute addresses such as sess:/worker. source and overwrite are branch-only; source defaults to the calling session and overwrite defaults false. page_limit is new/branch-only, 1–10000, omitted for unlimited; exceeding it permanently deletes the oldest closed pages. model is ignored for compact. Legacy agent is accepted as an alias for role. All live runs share the principal’s concurrency pool (default 20); delegation depth is unrestricted.

### RoleCatalog

Discover role templates available in the principal.

[Implementation](../../peko-rs/core/src/tools/builtin/role_catalog.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| — | — | — | No parameters |

Returns {total, agents}. Entries expose id, name, description, and enabled; use an enabled entry’s id as Agent.role.

### Skill

Load a SKILL.md body, resolve dynamic shell context, and substitute arguments.

[Implementation](../../peko-rs/core/src/tools/builtin/skill/tool.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `name` | string | yes | — |
| `args` | string[] | no | — |

args is string[]. The body supports $ARGUMENTS, positional $0/$1/etc., named frontmatter arguments, and dynamic shell context. Presence in the principal workspace determines availability; there is no principal capability allowlist.

## Session storage

### Session

Inspect and manage persisted sessions and compaction pages.

[Implementation](../../peko-rs/core/src/tools/builtin/session/tool.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `action` | status \| list \| history \| find \| copy \| move \| remove \| list_pages \| read_page \| search_pages | yes | — |
| `path` | string | no | — |
| `target` | string | no | — |
| `query` | string | no | — |
| `page` | integer | no | ≥ 1 |
| `offset` | integer | no | default 0, ≥ 0 |
| `max_results` | integer | no | default 20 |
| `title` | string | no | — |
| `page_limit` | integer | no | ≥ 0, ≤ 10000 |
| `recursive` | boolean | no | default false |
| `peer` | string | no | — |
| `agent_name` | string | no | — |
| `limit` | integer | no | — |
| `active_minutes` | integer | no | — |
| `include_tools` | boolean | no | default true |
| `timezone` | string | no | — |

action is required. See the action table below for conditional requirements and actual defaults. Absolute addresses use sess:/a/b; sess:/ identifies the trunk for reads. Omitted read paths select the calling session. Legacy agent_id remains an alias for the list agent_name filter; legacy label remains an alias for copy title. Mutation ownership/run guards remain in the session runtime.

| Action | Purpose | Required fields besides action | Optional fields / runtime defaults |
|---|---|---|---|
| status | Metadata and usage | — | path, timezone |
| list | List/filter sessions | — | path, peer, agent_name, active_minutes, limit=50 |
| history | Messages | — | path, limit=100, include_tools=true |
| find | Transcript search | query | path, peer, limit=50 |
| copy | Copy to a new address | path, target | title |
| move | Reparent/rename/update retention | path and at least one of target/title/page_limit | page_limit=0 means unlimited; maximum 10000 |
| remove | Delete a session | path | recursive=false |
| list_pages | Compaction page catalog | — | path |
| read_page | Render one page | page (1-based) | path, offset=0, limit=200 |
| search_pages | Search all pages | query | path, max_results=20 |

Session manages storage; Agent runs work. A move/remove refuses the trunk,
the current session, and actively running targets. Destructive operations remain
subject to runtime ownership guards. Page retention prunes old pages permanently.

## Channels

### ChannelSend

Post to a channel/group, ask a peer principal and await a reply, or send the originating user a note.

[Implementation](../../peko-rs/core/src/tools/builtin/channel/channel_send.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `channel` | string | yes | — |
| `text` | string | yes | — |
| `parent` | string | no | — |
| `label` | string | no | — |

channel selects dispatch: chan_<id> = post; principal:<did> = request/reply; user:<id> = note; group:<slug> = post. parent applies to bare/group posts; label applies to user notes and defaults to the agent name. User notes are limited to the originating user. Posts carry session attribution.

### ChannelRead

Read or search channel events with backward/forward pagination.

[Implementation](../../peko-rs/core/src/tools/builtin/channel/channel_read.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `channel` | string | yes | — |
| `limit` | integer | no | ≥ 1 |
| `query` | string | no | — |
| `author` | string | no | — |
| `before` | string | no | — |
| `since` | string | no | — |

Accepts chan_<id> or group:<slug>; caller membership is required. limit defaults to 50, capped at 1000 for reads and 200 for searches. Nonempty query or author activates search mode. since overrides before for reads and is ignored in search mode. Returns events oldest-to-newest with has_more and next_cursor.

## Inference and workflows

### ModelList

List configured models, optionally filtered by capability and text.

[Implementation](../../peko-rs/core/src/tools/builtin/model_list.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `filter` | vision \| tools \| thinking \| priced \| json_mode | no | — |
| `contains` | string | no | — |

filter and contains are AND-combined; contains matches id, display_name, and note case-insensitively. Requires enable_model_list and a bound model catalog. Rust configuration names remain snake_case.

### ModelCall

Make one sessionless completion or structured judgment call.

[Implementation](../../peko-rs/core/src/tools/builtin/model_call.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `model` | string | no | — |
| `prompt` | string | no | — |
| `system` | string | no | — |
| `max_tokens` | integer | no | — |
| `temperature` | number | no | — |
| `state` | string \| object | no | — |
| `questions` | object | no | nonempty object |

Exactly one mode is required: prompt for completion, or state plus a nonempty questions map for judgment. system/max_tokens/temperature are completion-only. model defaults to the principal’s preferred model. Judgment requires decisions:true; question specs are passed through and support boolean, choice (options), and score (min/max). Calls are attributed to the principal and metered.

### Workflow

Run a saved Python workflow from the principal’s workflows directory.

[Implementation](../../peko-rs/core/src/tools/builtin/workflow.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `path` | string | yes | — |
| `args` | string[] | no | — |
| `timeout_ms` | integer | no | — |

path must resolve to a .py file inside workflows/. timeout_ms defaults to 300000 and caps at 3600000; zero/nonpositive values fall back to the default. The process receives runtime identity and can call tools through the workflow SDK. Nesting depth is injected internally from the run token and is absent from the public schema.

## Session todos

### TaskCreate

Create a session-local todo.

[Implementation](../../peko-rs/core/src/tools/builtin/tasks/create.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `subject` | string | yes | — |
| `description` | string | no | — |
| `activeForm` | string | no | — |

Todos are stored in the calling session’s todos.jsonl sidecar.

### TaskGet

Fetch one todo.

[Implementation](../../peko-rs/core/src/tools/builtin/tasks/get.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `taskId` | string | yes | — |

### TaskList

List session-local todos, optionally filtered by status.

[Implementation](../../peko-rs/core/src/tools/builtin/tasks/list.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `status_filter` | pending \| in_progress \| completed | no | — |

### TaskUpdate

Change a todo’s status and/or owner.

[Implementation](../../peko-rs/core/src/tools/builtin/tasks/update.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `taskId` | string | yes | — |
| `status` | pending \| in_progress \| completed | no | — |
| `owner` | string | no | — |

At least one of status or owner is required, in addition to taskId.

## Durable plans

### PlanCreate

Create a principal-owned durable plan with dependency nodes.

[Implementation](../../peko-rs/core/src/tools/builtin/plan/create.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `title` | string | yes | — |
| `nodes` | object[] | yes | at least 1 item |

nodes contains at least one object: {step, nodeId?, dependsOn?: string[], status?}. nodeId is auto-assigned when omitted; status defaults to pending. Plans belong to the principal and persist across sessions.

### PlanList

List all plans owned by the current principal.

[Implementation](../../peko-rs/core/src/tools/builtin/plan/list.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| — | — | — | No parameters |

### PlanGet

Fetch a plan record.

[Implementation](../../peko-rs/core/src/tools/builtin/plan/get.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `planId` | string | yes | — |

### PlanAddStep

Append a node to an open plan.

[Implementation](../../peko-rs/core/src/tools/builtin/plan/add_step.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `planId` | string | yes | — |
| `step` | string | yes | — |
| `nodeId` | string | no | — |
| `dependsOn` | string[] | no | — |
| `status` | pending \| in_progress \| completed \| blocked \| failed | no | — |

nodeId is auto-assigned when omitted; status defaults to pending. Closed plans refuse new nodes.

### PlanMarkStep

Change a plan node’s status.

[Implementation](../../peko-rs/core/src/tools/builtin/plan/mark_step.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `planId` | string | yes | — |
| `nodeId` | string | yes | — |
| `status` | pending \| in_progress \| completed \| blocked \| failed | yes | — |
| `reason` | string | no | — |

reason applies to blocked/failed states. Statuses: pending, in_progress, completed, blocked, failed.

### PlanRecordEvidence

Attach an outcome summary and artifact references to a node.

[Implementation](../../peko-rs/core/src/tools/builtin/plan/record_evidence.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `planId` | string | yes | — |
| `nodeId` | string | yes | — |
| `output` | string | yes | — |
| `artifacts` | string[] | no | — |
| `decidedBy` | string | no | — |

artifacts is string[] of paths/references; decidedBy is optional attribution.

### PlanClose

Close a plan with a reason.

[Implementation](../../peko-rs/core/src/tools/builtin/plan/close.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `planId` | string | yes | — |
| `reason` | string | yes | — |

Repeated closure returns AlreadyClosed.

## Background execution

### AsyncSpawn

Start a tool in the background and return an async receipt.

[Implementation](../../peko-rs/core/src/tools/builtin/async_control/spawn.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `tool` | string | yes | — |
| `params` | object | yes | — |
| `label` | string | no | — |
| `wake_on_completion` | boolean | no | — |
| `timeout_secs` | integer \| null | no | ≥ 1 |

params is forwarded verbatim. wake_on_completion defaults true; completion enters the spawning session’s inbox and may start an idle-session follow-up. timeout_secs defaults to the executor’s 7200-second policy; null/omit selects that default.

### AsyncOutput

Fetch output, optionally waiting for completion.

[Implementation](../../peko-rs/core/src/tools/builtin/async_control/output.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `task_id` | string | yes | — |
| `block` | boolean | no | default false |
| `timeout` | integer | no | ≥ 0 |
| `tail_lines` | integer | no | default 0, ≥ 0 |

block defaults false. With block=true, timeout defaults to 300000 milliseconds. tail_lines=0 returns full output; positive values select the last N lines.

### AsyncStatus

Inspect a background task’s state and metadata.

[Implementation](../../peko-rs/core/src/tools/builtin/async_control/status.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `task_id` | string | yes | — |

### AsyncList

List background tasks in the bound async runtime.

[Implementation](../../peko-rs/core/src/tools/builtin/async_control/list.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `status_filter` | pending \| running \| completed \| failed \| cancelled \| timed_out | no | — |
| `tool_filter` | string | no | — |

Uses the agent-bound async runtime. Statuses: pending, running, completed, failed, cancelled, timed_out.

### AsyncStop

Cancel a background task.

[Implementation](../../peko-rs/core/src/tools/builtin/async_control/stop.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `task_id` | string | yes | — |

## Scheduling

### CronCreate

Schedule an instruction-driven agent turn or a fixed tool invocation.

[Implementation](../../peko-rs/cron/src/tools/create.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `message` | string | no | nonempty |
| `tool` | string | no | nonempty |
| `params` | object | no | — |
| `wake_on_completion` | boolean | no | — |
| `timeout_secs` | integer | no | — |
| `one_shot` | boolean | no | default false |
| `label` | string | no | — |
| `cron` | string | no | — |
| `at` | string | no | — |
| `delay` | string | no | — |
| `interval_ms` | integer | no | — |
| `timezone` | string | no | — |
| `idle_ms` | integer | no | — |

Requires exactly one nonempty message or tool, plus a schedule. message starts an agent turn in the creating session (trunk fallback); tool invokes fixed parameters and may itself use an LLM. params defaults to {}; wake_on_completion defaults false and timeout_secs uses the executor’s 7200-second policy. delay is a positive relative duration (90s, 5m, 1h, 1d, or bare milliseconds) and cannot be combined with another schedule. Explicit fields resolve by precedence at > interval_ms > cron > idle_ms. timezone applies to cron and defaults UTC. idle_ms rounds down to whole minutes, with a one-minute minimum. one_shot=true deletes after the first fire; at/delay jobs are always one-shot. Same-job runs do not overlap, and overdue interval slots are skipped.

### CronList

List the calling principal’s scheduled jobs.

[Implementation](../../peko-rs/cron/src/tools/list.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| — | — | — | No parameters |

### CronDelete

Delete a scheduled job.

[Implementation](../../peko-rs/cron/src/tools/delete.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `id` | string | no | — |
| `label` | string | no | — |

Supply exactly one of id or label.

### CronUpdate

Pause/resume a job or change completion wake behavior.

[Implementation](../../peko-rs/cron/src/tools/update.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `id` | string | no | — |
| `label` | string | no | — |
| `enabled` | boolean | no | — |
| `wake_on_completion` | boolean | no | — |

Requires id or label and at least one of enabled/wake_on_completion. A nonempty id takes precedence if both selectors are supplied. Re-enabling resets consecutive failures; completion subscription applies to tool jobs and uses the creating session (trunk fallback).

### CronTrigger

Fire a job now, including disabled jobs; coalesce with an in-flight run.

[Implementation](../../peko-rs/cron/src/tools/trigger.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `id` | string | no | — |
| `label` | string | no | — |

Supply exactly one of id or label. Disabled jobs may be triggered; an in-flight job coalesces and returns its actual run_id.

### CronHistory

Fetch a job’s recent run history.

[Implementation](../../peko-rs/cron/src/tools/history.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `id` | string | no | — |
| `label` | string | no | — |
| `limit` | integer | no | — |

Supply exactly one of id or label. limit defaults to 10, caps at 50, and returns newest runs first.

## Related contracts

- [API_SURFACE.md](../../API_SURFACE.md)
- [DATA_MODEL.md](../../DATA_MODEL.md)
- [ADR-066](adr/ADR-066-pure-workspace-tooling.md)
- [ToolCatalog](../../peko-rs/core/src/tools/catalog.rs)
- [ToolDispatcher](../../peko-rs/core/src/tools/dispatcher.rs)
