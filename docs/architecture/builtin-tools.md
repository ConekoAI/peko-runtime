# Built-in Tools Catalog

Peko exposes **19 built-in tool names**, all in PascalCase. Task, Plan, Cron, and Async each require an explicit `action`; no former per-action tool names are aliases. Session also rejects fields irrelevant to the selected action. This reference
describes the compiled Tool implementations; the emitted descriptions and
JSON Schemas in source are the executable contract. MCP and workspace tools
have their own names and schemas and are outside this inventory.

Every registered tool appears in the native wire catalog (ADR-066). ToolCatalog
owns registration/lookup; ToolDispatcher validates arguments and emits one
attributed audit event. Every principal can use every registered tool. There is
no tool allowlist or capability filter. Principal bindings select workspace and
service dependencies; ownership, peer permissions, and channel membership
control access to resources.

## Naming

Built-in wire names are exact PascalCase names: `Session`, `ModelList`,
`RoleCatalog`, etc. Old spellings are not aliases. MCP and workspace tool names
remain exactly as registered. Rust module names, IPC operation tags, action
values, and parameter names retain their existing spelling.

## Registration and lifetimes

[installation.rs](../../peko-rs/core/src/tools/installation.rs) owns the factories,
installation phases, and the 19-name inventory. The complete inventory derives from its manifest.

| Lifetime | Tools | Installation |
|---|---|---|
| Runtime defaults | Read/Write/Edit/Glob/Grep/Bash, Cron, ChannelRead | Runtime startup; fill missing defaults without replacing configured instances |
| Daemon services | ModelCall, Workflow, caller-aware Session and Agent | After PrincipalManager and daemon services exist |
| Principal workspace | Skill, RoleCatalog | Once per principal; RoleCatalog scans current role files on invocation |
| Principal services | Task, Plan, ChannelSend, ModelList, Async | When their session storage, plan, caller identity/channel, or inbox bindings become available |
| Run bindings | Agent, Session | Private catalog overlay for caller execution dependencies |

Each run inherits live runtime/principal registrations through its overlay.
Installing a run executor never replaces another run's binding. Workflow/IPC
callbacks resolve the active overlay by the attributed principal and caller
session; when no run is live, daemon Agent/Session adapters resolve principal
services per call. All calls use the same dispatcher implementation, hooks,
audit sink, and timeout router. Run admission remains principal-wide. ChannelSend
keeps principal identity/reply locks and resolves the current tunnel context per
call, including connections made after installation.

Async executors and task registries belong to the principal, so receipts remain
resolvable after a run ends. Async action spawn stamps the caller session on each task;
completion events go to that session's inbox. Background Bash and subagent tasks
remain accessible through the existing principal-filtered registry fallback.
Every background task has an owning principal: the principal of the call that
created it (Async spawn, background Bash, a call detached on timeout, subagent
runs, cron tool jobs). Async list/status/output/stop see only the caller's own
tasks. Work created without a principal is owned by the system principal and is
visible to no principal; the `ExecuteTool` IPC path refuses calls whose session
key resolves to no loaded principal (ADR-061 D2).
These lifetimes do not introduce a new authorization boundary; the principal
remains the trust boundary. See [ADR-069](adr/ADR-069-builtin-tool-installation-lifetimes.md).

## Parameter conventions

Required below means required for the documented action. Conditional requirements
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

All four actions require nonempty prompt, role, and path. new is create-or-resume; new/branch accept a single relative slug or an absolute session address, while resume/compact use absolute addresses such as sess:/worker. source and overwrite are branch-only; source defaults to the calling session and overwrite defaults false. page_limit is new/branch-only, 1–10000, omission preserves an existing cap (new sessions are unlimited); exceeding it permanently deletes the oldest closed pages. model is new/resume/branch-only and refused for compact. All live runs share the principal’s concurrency pool (default 20); delegation depth is unrestricted.

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
| `max_results` | integer | no | default 20, ≥ 0 |
| `title` | string | no | — |
| `page_limit` | integer | no | ≥ 0, ≤ 10000 |
| `recursive` | boolean | no | default false |
| `peer` | string | no | — |
| `agent_name` | string | no | — |
| `limit` | integer | no | ≥ 0; per-action defaults |
| `active_minutes` | integer | no | ≥ 0; ≤ floor(u64::MAX / 60000) |
| `include_tools` | boolean | no | default true |
| `timezone` | string | no | — |

action is required. Both Agent and Session reject unknown fields, explicit nulls, and fields outside the chosen action on dispatcher and direct calls. Session result limits, offsets, and page numbers must fit usize; active_minutes is bounded so conversion to milliseconds cannot overflow. See the action table below for conditional requirements and actual defaults. Absolute addresses use sess:/a/b; sess:/ identifies the trunk for reads. Omitted read paths select the calling session. Mutation ownership/run guards remain in the session runtime.

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

A move with target plus title/page_limit applies subsequent updates at the destination and returns that effective path. Invalid arguments reject before any storage mutation.

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

filter and contains are AND-combined; contains matches id, display_name, and note case-insensitively. Requires a bound model catalog; every run inherits the principal service.

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

### Task

Required `action`: `create`, `get`, `list`, `update`. Each action accepts only its documented fields.

#### create

Create a session-local todo.

[Implementation](../../peko-rs/core/src/tools/builtin/tasks/create.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `subject` | string | yes | — |
| `description` | string | no | — |
| `activeForm` | string | no | — |

Todos are stored in the calling session’s todos.jsonl sidecar.

#### get

Fetch one todo.

[Implementation](../../peko-rs/core/src/tools/builtin/tasks/get.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `taskId` | string | yes | — |

#### list

List session-local todos, optionally filtered by status.

[Implementation](../../peko-rs/core/src/tools/builtin/tasks/list.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `status_filter` | pending \| in_progress \| completed | no | — |

#### update

Change a todo’s status and/or owner.

[Implementation](../../peko-rs/core/src/tools/builtin/tasks/update.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `taskId` | string | yes | — |
| `status` | pending \| in_progress \| completed | no | — |
| `owner` | string | no | — |

At least one of status or owner is required, in addition to taskId.

## Durable plans

### Plan

Required `action`: `create`, `list`, `get`, `add_step`, `mark_step`, `record_evidence`, `close`. Each action accepts only its documented fields.

#### create

Create a principal-owned durable plan with dependency nodes.

[Implementation](../../peko-rs/core/src/tools/builtin/plan/create.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `title` | string | yes | — |
| `nodes` | object[] | yes | at least 1 item |

nodes contains at least one object: {step, nodeId?, dependsOn?: string[], status?}. nodeId is auto-assigned when omitted; status defaults to pending. Plans belong to the principal and persist across sessions.

#### list

List all plans owned by the current principal.

[Implementation](../../peko-rs/core/src/tools/builtin/plan/list.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `action` | string | yes | See action heading |

#### get

Fetch a plan record.

[Implementation](../../peko-rs/core/src/tools/builtin/plan/get.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `planId` | string | yes | — |

#### add_step

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

#### mark_step

Change a plan node’s status.

[Implementation](../../peko-rs/core/src/tools/builtin/plan/mark_step.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `planId` | string | yes | — |
| `nodeId` | string | yes | — |
| `status` | pending \| in_progress \| completed \| blocked \| failed | yes | — |
| `reason` | string | no | — |

reason applies to blocked/failed states. Statuses: pending, in_progress, completed, blocked, failed.

#### record_evidence

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

#### close

Close a plan with a reason.

[Implementation](../../peko-rs/core/src/tools/builtin/plan/close.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `planId` | string | yes | — |
| `reason` | string | yes | — |

Repeated closure returns AlreadyClosed.

## Background execution

### Async

Required `action`: `spawn`, `output`, `status`, `list`, `stop`. Each action accepts only its documented fields.

#### spawn

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

#### output

Fetch output, optionally waiting for completion.

[Implementation](../../peko-rs/core/src/tools/builtin/async_control/output.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `task_id` | string | yes | — |
| `block` | boolean | no | default false |
| `timeout` | integer | no | ≥ 0 |
| `tail_lines` | integer | no | default 0, ≥ 0 |

block defaults false. With block=true, timeout defaults to 300000 milliseconds. tail_lines=0 returns full output; positive values select the last N lines.

#### status

Inspect a background task’s state and metadata.

[Implementation](../../peko-rs/core/src/tools/builtin/async_control/status.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `task_id` | string | yes | — |

#### list

List background tasks in the bound async runtime.

[Implementation](../../peko-rs/core/src/tools/builtin/async_control/list.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `status_filter` | pending \| running \| completed \| failed \| cancelled \| timed_out | no | — |
| `tool_filter` | string | no | — |

Uses the principal-owned async runtime shared across runs. Statuses: pending, running, completed, failed, cancelled, timed_out.

#### stop

Cancel a background task.

[Implementation](../../peko-rs/core/src/tools/builtin/async_control/stop.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `task_id` | string | yes | — |

## Scheduling

### Cron

Required `action`: `create`, `list`, `delete`, `update`, `trigger`, `history`. Each action accepts only its documented fields.

#### create

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

Requires exactly one nonempty message or tool, plus a schedule. message starts an agent turn in the creating session (trunk fallback); tool invokes fixed parameters and may itself use an LLM. params defaults to {}; wake_on_completion defaults false and timeout_secs uses the executor’s 7200-second policy. delay is a positive relative duration (90s, 5m, 1h, 1d, or bare milliseconds) and cannot be combined with another schedule. Explicit fields resolve by precedence at > interval_ms > cron > idle_ms. timezone applies to cron and defaults UTC. idle_ms rounds down to whole minutes, with a one-minute minimum. one_shot=true deletes after the first fire; at/delay jobs are always one-shot. Same-job runs do not overlap, and overdue interval slots are skipped. message jobs on interval_ms require at least 60000 ms.

#### list

List the calling principal’s scheduled jobs.

[Implementation](../../peko-rs/cron/src/tools/list.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `action` | string | yes | See action heading |

#### delete

Delete a scheduled job.

[Implementation](../../peko-rs/cron/src/tools/delete.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `id` | string | no | — |
| `label` | string | no | — |

Supply exactly one of id or label.

#### update

Pause/resume a job or change completion wake behavior.

[Implementation](../../peko-rs/cron/src/tools/update.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `id` | string | no | — |
| `label` | string | no | — |
| `enabled` | boolean | no | — |
| `wake_on_completion` | boolean | no | — |

Requires id or label and at least one of enabled/wake_on_completion. A nonempty id takes precedence if both selectors are supplied. Re-enabling resets consecutive failures; completion subscription applies to tool jobs and uses the creating session (trunk fallback).

#### trigger

Fire a job now, including disabled jobs; coalesce with an in-flight run.

[Implementation](../../peko-rs/cron/src/tools/trigger.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `id` | string | no | — |
| `label` | string | no | — |

Supply exactly one of id or label. Disabled jobs may be triggered; an in-flight job coalesces and returns its actual run_id.

#### history

Fetch a job’s recent run history.

[Implementation](../../peko-rs/cron/src/tools/history.rs)

| Parameter | Type | Required | Schema default / bounds |
|---|---|---|---|
| `id` | string | no | — |
| `label` | string | no | — |
| `limit` | integer | no | — |

Supply exactly one of id or label. limit defaults to 10, caps at 50, and returns newest runs first. Fired one-shot jobs delete themselves but keep their runs: read them by the job_id returned at creation (labels resolve live jobs only).

## Testing

Three tiers cover the built-in tools; put a test in the lowest tier that can
observe the behavior.

| Tier | Where | Runs in | Use for |
|---|---|---|---|
| Unit | `#[cfg(test)]` beside each implementation | `cargo test --lib` | Argument handling and domain rules against the tool's own fake or backend |
| Harness | [`ToolHarness`](../../peko-rs/core/src/tools/builtin/test_harness.rs) | `cargo test --lib` | Dispatch semantics (validation, workspace injection, audit), multi-tool flows, caller/principal scoping |
| Daemon | [`cli_tools.rs`](../../peko-rs/core/tests/cli_tools.rs) | `make test-cli-tools` (mock LLM) | Daemon wiring only: workspace resolution, process spawning, result persistence |

`ToolHarness::new()` registers all 19 tools behind the production dispatcher,
backed by tempdir storage where it is cheap (workspace files, ChannelStore,
CronScheduler, ModelCatalog, AsyncExecutor) and fakes where the real backend
is a daemon (Task, Plan, Agent, Session). ModelCall and Workflow stay unbound
and fail closed. Its smoke test must cover the installation manifest, so a new
built-in fails until it has a backend and a smoke case. Cron actions take an
explicit runtime through `CronTool::with_runtime`; `peko_cron::testing` provides
`FileCronRuntime` behind the `test-support` feature.

Mock LLM replies are scripted, so a daemon test must assert on effects the
mock cannot produce (files, persisted tool results), never on a reply
sentinel alone. `make coverage` reports unit-tier line coverage via
cargo-llvm-cov.

## Related contracts

- [API_SURFACE.md](../../API_SURFACE.md)
- [DATA_MODEL.md](../../DATA_MODEL.md)
- [ADR-066](adr/ADR-066-pure-workspace-tooling.md)
- [ToolCatalog](../../peko-rs/core/src/tools/catalog.rs)
- [ToolDispatcher](../../peko-rs/core/src/tools/dispatcher.rs)
