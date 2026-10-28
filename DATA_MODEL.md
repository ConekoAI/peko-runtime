# Peko — Data Model Specification

> Current actor addressing and conversation binding are defined in ADR-073 and
> the ADR-073 section below. Earlier sprint records describe retired contracts:
> DIDs are signing credentials, actor references use local/Hub IDs, and sessions
> bind only to channels.

**Version:** 0.1.0
**Date:** 2026-10-03 (ADR-067)
**Status:** Current
**Companion docs:** [`AGENTS.md`](AGENTS.md) (build & module rules), [`API_SURFACE.md`](API_SURFACE.md) (public Rust API), [`docs/architecture/adr/`](docs/architecture/adr/) (decisions)

Output-limit recovery preserves partial assistant output with its usage and
existing tool-call/result entries. Continuation guidance is a Hook-origin user
message tagged `<output-limit-recovery attempt="N/2">`, never a human instruction.
The counter is run-local and allows two continuations; quota remains persisted
by the existing meter. This adds no wire or on-disk schema fields.
Empty billing-only assistant entries remain in JSONL for usage reconciliation,
but are omitted when building provider request history.

This document defines every on-disk and in-memory data format used by the Peko runtime. It is the authoritative reference for anyone implementing the filesystem loader, session manager, image builder, or any component that reads or writes Peko data. All formats described here must be treated as stable contracts — breaking changes require a version increment.

---

## Acting-for stamps and the trust floor (ADR-078)

- `SessionEntry.compartment` (optional; `{"kind":"internal"}` or
  `{"kind":"conversation","channel":"<id>"}`): written once at creation —
  conversation children take their channel, spawns take the calling run's
  compartment, copies and branches take their source's. Absent on older
  sessions, which derive it from the nearest channel-bound ancestor.
- `CronJob.created_by` (optional `ActingFor`): the stamp of the run that
  created the job. Seeded `keepalive`/`genesis` jobs carry the runtime stamp
  (`actor: "runtime"`, owner, internal). Unstamped non-seeded jobs fail at
  fire time.
- `ActingFor` JSON: `{actor, name?, trust: "owner"|"trusted"|"untrusted",
  compartment, origin: {kind: "ingress"|"cron"|"wake"|"runtime"|"approval", …}}`.
  Carried on tool calls, audit events (`tool.call`'s `acting_for`), run-token
  entries, and background-task dispatch; never read from a packet.
- `<workspace>/POLICY.md` (protected, seeded create-once): optional front
  matter (`judge_model`, `review_replies`, `memory_review`: `guests` (default)
  | `always` | `off`, ADR-085 D4), default rules, and
  `## <actor address>` sections. *Always allow* approvals append to the
  actor's section.
- `<workspace>/kb/inbox/<compartment>/<ts>-<path>.proposal.md`: a non-owner
  run's write to a protected file, with the proposer's stamp and the change.
  The same directory holds the `kb/MEMORY.md` and `kb/CONVENTIONS.md` lines a
  scheduled owner run could not source to the owner (ADR-080 D4, D8), or that
  the reviewer did not find supported by their sources (ADR-085): a list of
  held lines with their reasons, filed under the conversation each one cites
  (uncited, unknown-source and reviewer-held lines under the run's own
  compartment, `internal`). Runtime-written only; at most 50 per
  compartment.
- `<workspace>/kb/dreams/YYYY-MM-DD.md` (ADR-080 D7): the nightly memory pass's
  reflections on how the principal worked, one file per night, each lesson
  ending with `(source: …)`. Cold: never injected into a prompt. Protected (only
  owner runs write it). The scaffold seeds `kb/dreams/README.md`.
- `<local>/approvals/apr_<12 hex>.json`: an owner-approval request
  (`call`, `reason`, `created_at`, `expires_at` (24 h), `status`:
  `pending`|`approved`|`denied`, `digest` of tool + arguments).
- `Permission::Trusted` (`"trusted"`) in `principal.toml` grants: the owner's
  consent to give one named actor full tools. Never `public`, never in invites.
- MCP definitions gain `untrusted_ok` (default `false`) and the reserved
  runtime fields `acting_for` / `acting_for_trust`; the retired reserved field
  `peer_id` is replaced by `acting_for`. Workspace hooks receive
  `PEKO_ACTING_FOR` and `PEKO_ACTING_FOR_TRUST`.

---

## Tool ledger (ADR-079)

`{data_dir}/principals/<name>/local/ledger/ledger-YYYY-MM-DD.jsonl`
(`LocalLayout::ledger_dir`): one hash-chained, append-only file per UTC day
(dir 0700, files 0600, every line fsync'd). Only the dispatcher writes it;
`Write`/`Edit` refuse the directory at every trust level. Not packed into
`.peko` snapshots; an imported principal starts a new chain.

Every line carries `seq` (monotonic across days), `prev` (the previous line's
`hash`; the first line's is 64 zeros), and `hash`. The hash covers the line's
exact bytes up to `,"hash":"sha256:<hex>"}`, which replaces the record's closing
brace. A chain opens with a `genesis` record (`principal_id`). A torn final
line is closed with a newline on reopen and skipped.

Each tool call writes a `start` before it runs and an `end` after:

```jsonc
{"v":1,"kind":"start","ts":"…","call_id":"call_<uuidv7>","run_id":"…",
 "tool_call_id":"toolu_…","parent":{"kind":"task","id":"Bash:…","call_id":"call_…"},
 "principal_id":"…","principal_name":"…","agent_id":"did:…","session_id":"<uuid>",
 "acting_for":{…},"tool":"Bash","action":null,"source":"builtin",
 "params":{…},"params_digest":"sha256:…","seq":12,"prev":"sha256:…","hash":"sha256:…"}
{"v":1,"kind":"end","ts":"…","call_id":"call_…","outcome":"ok","duration_ms":1840,
 "exit_code":0,"result":{"excerpt":"…","bytes":48211,"truncated":true},"seq":13,…}
```

- `parent` (optional): `{kind:"task", id, call_id?}` for a background body,
  `{kind:"workflow", call_id}` for a workflow callback, `{kind:"approval", id,
  call_id?}` for an approved replay.
- `source`: `builtin`, `mcp:<server>`, or `unavailable` (unknown tool).
- `params`: redacted (runtime-held secrets → `[secret:<label>]`, sensitive keys
  and credential shapes → `[redacted]`); string leaves capped at 2 KiB, the whole
  at 8 KiB (`{_truncated, bytes, preview}` beyond). `params_digest` is computed
  over the unredacted params and equals the global `tool.call` digest.
- `outcome`: `ok`, `error` (also a tool-reported `"success": false`, e.g. a
  non-zero Bash exit), `cancelled`, `floor_denied`, `policy_denied`,
  `awaiting_owner`, `proposed`, `detached`. A `detached` end carries `task_id`;
  a second end with `"late": true` follows when the background body finishes.
- `result.excerpt`: redacted; process output keeps the last 2 KiB of stdout
  and of stderr (`[stderr]` separator), other results the first 4 KiB.

The global audit log gains `ledger.checkpoint` (`principal_id`, `seq`, `hash`;
hourly and at shutdown), `ledger.pruned` (`principal_id`, `seq`, `hash` of the
last pruned line, `before`), and `ledger.write_failed` (Security). Its
`tool.call` details add `call_id`, `run_id`, `tool_call_id`, `parent`, and
`outcome`.

`principal.toml` `[governance] ledger_retention_days` (positive, default 90)
sets how many days are kept. The retired `[governance] audit` level
(`all`/`commands`/`none`, never read) is ignored on load and not written back.

Retired with this ADR: per-task files under `{data_dir}/async_tasks/` (a
write-only, cross-principal record with plaintext params) and
`CompletionEvent.output_path`, which pointed at them. Existing
`async_tasks/` directories are no longer written or cleaned; delete them by hand.

---

## Built-in tool names and availability

Native tools use exact PascalCase names, including Session, ModelList, and
RoleCatalog. Old spellings are not dispatch aliases; historical JSONL records
are unchanged. Persisted cron/workflow calls must use the canonical names.
Agent uses role; Session uses agent_name and title. IPC operation tags, actions,
parameter names, and Rust configuration keys retain their own spelling.
Every principal can use every registered tool; there is no tool allowlist.

Task, Plan, Cron, and Async now bundle their operations under required action
fields (for example `{tool_name:"Task", params:{action:"create", subject:"review"}}`).
Former per-action tool names are not registered and are not dispatch aliases.
Update persisted cron invocations, workflows, and exact hook selectors to the
new tool name and supply action in params. Historical JSONL records remain intact.
Session rejects fields outside the selected action. Tool audit details add an
`action` string (a declared default when omitted, or null if absent/undeclared), while parameters remain hashed.
Storage formats for todos, plans, async receipts, and cron jobs are unchanged.

Cron action create now advertises the already-supported `one_shot` boolean; it continues
to set `delete_after_run`, and at/delay schedules remain one-shot automatically.
Workflow nesting depth remains internal run-token metadata rather than a public
tool parameter. No persisted schema or snapshot format changes are required.

Agent/Session calls reject unknown, irrelevant, null, and out-of-range arguments
before resolving runtime dependencies or mutating storage. Agent still defaults
to new; compact retains its session model and refuses a supplied model field.
Session move returns its effective destination path when target is supplied,
including when combined with title or page_limit. Historical JSONL is unchanged;
no storage migration is required.

## Cron interval audit timing

Cron action trigger's returned `run_id` is the persisted `CronRun.id`. Admission writes
the open row before returning the ID or spawning work; timer/manual coalescing
reuses that row. History and audit completion keep the same ID. Storage and
wire shapes are unchanged.

A `cron.nudge` audit event (ADR-086) records a peer post bringing a principal's
attention keepalive forward: `job_id`, `channel` (`pending` when applied after
an in-flight turn), `last_turn`, and the new `next_run_at`. No schedule field
is added; `next_run` simply moves earlier.

`cron.result` details retain status, error and execution `duration_ms`, and add
RFC3339 `scheduled_at`, `finished_at`, `next_run_at`, and integer
`skipped_interval_slots` for `Every` jobs (null otherwise), and boolean
`catch_up`. The slot count is the number of grid points omitted between the
fired scheduled slot and the new `next_run`, excluding both endpoints. It
includes overdue slots from late admission or execution; it does not count
failed turns or missed world reads. Run history and schedule storage formats
are unchanged. A same-job in-flight fire does not create another history row.
After a successful run that finished on or past its next slot, `next_run` is
the latest such slot (`catch_up: true`), so the job runs once more right away;
earlier overdue slots are skipped, never replayed. Failed runs skip to the
next future slot.
For an async run still `running` after the completion wait budget, `finished_at`
is null. `wait_finished_at` marks the end of that wait; `duration_ms` is its
duration. These fields do not fabricate task completion or full task duration.

## Anthropic-compatible prompt cache markers

With `ChatOptions.cache_retention` enabled, provider requests mark the system
text block, final tool definition, and final message's trailing text block.
A plain-text message tail is represented as
`[{"type":"text","text":"…","cache_control":{"type":"ephemeral"}}]`
so the breakpoint survives text-only conversion. Long retention also emits
`"ttl":"1h"`. Disabled caching retains the plain-string tail and emits no
markers. Earlier messages and tool-only tails retain their existing shapes.
These are provider request fields; session JSONL remains unchanged. A marker
requests caching but does not guarantee a hit from a compatible endpoint.

## Cron Send history after daemon restart

When a new daemon first opens a principal's cron schedule, unfinished
`running` rows whose owning job is a `Send`, or a `SpawnTool` without an
attached task id, become `interrupted`, with `finished_at` and an error stating
that the interrupted outcome may be partial. A `SpawnTool` row whose attached
task is missing from the new process's registry becomes `interrupted` the same
way (the hourly janitor still closes a task purged at any other time as
`failed`). The owning job's last status becomes `interrupted`; its failure
counter is left alone, since a restart says nothing about the job's health.
Authored enablement, next due time and run count are retained. Existing
terminal history retains its lifecycle.

A fire whose tool fails because the job's previous work is still in flight
(`has an active run in flight`, e.g. an `Agent` child that outlived its run's
timeout) is recorded as `skipped` and likewise leaves the failure counter
alone. `success` resets the counter; every other terminal status (`failed`,
`timed_out`, `cancelled`) bumps it toward `max_retries`
(`peko_cron::status_resets_failures`). Recovery is once per
schedule path in that daemon, shared across principal-id/DID aliases and engine
clones; subsequent opens cannot reap a live Send turn. There are no new wire
or storage fields. This clears stale coalescing state so ordinary due polling
can continue. It does not undo, certify, or deduplicate interrupted side effects.

## Principal bootstrap completion (ADR-054)

`principal.toml` retains the existing `boot_state` values: `provisioned`,
`defined`, `genesis_pending`, and `organized`. `organized` records a finalized
successful genesis run; it does not certify the quality of self-organization.
The cron engine writes it after finalizing history and reaping the bootstrap
one-shot. The transition reads the current authored configuration under the
workspace file lock, replaces it atomically, then publishes the in-memory
boot state. A stale seeding pass cannot rewind `organized`.

The CLI waits for both successful finalized `genesis` history and the persisted
marker. One-shot deletion alone is not completion evidence: cron still reaps
one-shots on all outcomes. At boot, unfinished principals with missing or failed
genesis jobs get a fresh one-shot. Finalized success recovers an interrupted
completion write and removes any remaining bootstrap job before handing cadence
to the principal. Organized principals' authored schedules are not re-seeded.
Before completion, an enabled, non-self-deleting recurring Send counts as a heartbeat only when
targeted at the trunk (`origin_session` absent or the trunk id); both `Every`
and `Cron` schedules count. Peer-session reminders do not count, nor does the
daily `memory` job.

**Daily memory maintenance (ADR-080).** Every principal's schedule gets one
recurring `memory` job (every 24 hours, `when: "quiet"`, trunk `Send`, runtime
stamp) the first time the boot pass sees it, whatever its boot state. The ids the runtime has
seeded live beside the schedule in `local/cron/seeded_jobs.json`
(`{"jobs": ["memory"]}`); a recorded id is never seeded again, so a principal
that deletes or replaces the job keeps it that way. A job the principal already
named `memory` is kept and only recorded. The seeded job stores a fixed marker
message, not the brief; the cron engine writes the current brief into the turn
when the job fires, so brief changes reach existing principals. A `memory` job
still carrying the full brief text an earlier build stored is read the same
way; any other message is the principal's own and is sent as written. Such a
job with no `when` is read as `quiet` (rev 3).

**Per-job model.** A `Send` action may carry `model` (a configured model id,
omitted when unset); each fire's turn runs on it instead of the principal's
pinned model. `SpawnTool` actions have no model. Older schedules read
unchanged.

**Quiet ticks (ADR-083).** A `Send` action may carry `when` (`always` |
`attention` | `quiet`, omitted when unset). A due `attention` fire with nothing waiting and
a last turn under an hour old writes no run row: it sets the job's `next_run`
and `last_status: "quiet"` and leaves `last_run` (the last turn), `run_count`
and `consecutive_failures` alone. A `keepalive` job with the runtime's tick text
and no `when` is read as `attention`. The seeded keepalive now stores
`when: "attention"`. A due `quiet` fire (ADR-080 D9; interval or cron schedules
only) is held while the principal's last activity in `idle_activity.json` is
under 4 hours old, up to 12 hours past its due time (the schedule's next slot
after `last_run`, else after `created_at`). A held fire is recorded the same
way as a quiet tick, with `next_run` set to when it may run.

**Plan check-ins (ADR-084).** A plan node (`plans/<plan_id>.jsonl`) may carry
`checkAt`, an RFC 3339 UTC time, omitted when unset. Records without it read
unchanged. Completing a node clears it. A ready or in-progress node with a future
`checkAt` does not count as waiting for an `attention` fire; a node whose
`checkAt` passed after the job's `last_run` does, once.

**Idle schedules.** `Idle` jobs read principal activity from
`<runtime_dir>/idle_activity.json`, a map of runtime `PrincipalId` to the
RFC 3339 time someone last talked to the principal: an ingress turn starting
or ending, or a message queued for a busy session. Cron turns, completion
wakes, and other principals' posts do not count. A principal with no entry
counts as active since daemon start. An `Idle { minutes }` job is due when
the principal has been quiet that long and the job's `last_run` predates the
last activity, so it fires once per quiet stretch.

## Principal avatar (`identity.avatar`)

`principal.toml` `[identity] avatar` is an emoji or a short monogram
(`"🦊"`, `"AB"`): at most 32 bytes, no whitespace or control characters,
and never an image URL — clients must not fetch remote content to draw a
peko. `principal_update { avatar }` sets it (`""` clears it; anything else
is checked by `principal::config::validate_avatar` and refused with
`[bad_request]`), and `principal_list` / `principal_get` summaries carry it
as `avatar` (omitted when unset). It travels in `.peko` snapshots with the
rest of `principal.toml`. Clients draw a peko without one from its name.

## Principal agent run limit (ADR-067)

`principal.toml` configures admission in the existing governance table:

```toml
[governance]
max_running_agents = 20
```

The positive integer defaults to 20 when omitted, including when the entire
`governance` table is absent. Zero is rejected. Legacy `max_delegation_depth`
is ignored on load and never written back. There is no depth restriction.
The in-memory admission pool is keyed by stable `PrincipalId` in the shared
runtime and is not persisted: live execution futures do not survive restart.
Idle session metadata and completed/cancelled registry records do not determine
capacity. Diagnostic subagent `depth` metadata remains unchanged.

Agent capacity refusals use the existing error envelope:

```json
{
  "status": "forbidden",
  "error_type": "ConcurrentLimitExceeded",
  "current_concurrent": 20,
  "max_concurrent": 20,
  "error": "Principal concurrent agent run limit reached: 20 (max: 20). Try again after an existing run finishes.",
  "note": "Principal concurrent agent run limit reached. Try again after an existing run finishes."
}
```

The tool funnel marks this structured result as a failure; no run or target
session mutation is admitted. Cron records the failure using normal history
and schedule handling. There is no automatic admission retry or queue.

## Workspace hook manifests (ADR-066 P4)

`<workspace>/hooks/<id>/hook.toml` supports six bind points:
`PreToolUse`, `PostToolUse`, `Stop`, `AfterAgent`, `PromptSection`, and
`SessionContextBuild`. For tool observers, omit `tool_name` to match all tools,
or provide an exact name. Wildcard selectors are rejected with guidance to
omit the field. `PromptSection` requires a nonempty `section` without control
characters. Legacy `priority` is parsed as an unknown field and ignored.
Hooks load by sorted directory name, then manifest bind order, and run only
for their owning principal. Every handler has a two-second soft-fail budget.
Commands receive principal/workspace/session/agent context through the
`PEKO_*` environment variables; prompt output retains tail-message change /
retraction semantics. Invalid binds reject the entire manifest; other hooks
still load.

## Runtime contract cleanup (ADR-066 P6)

Principal configuration, summary, agent context, and import IPC no longer carry
capability grants. PrincipalConfig uses ordinary derived deserialization; unknown
fields are ignored rather than interpreted as grants. Governance has no
auto_grant_tools field and AgentConfig has no enable_model_list field.
Inbound `permissions` remain the authorization policy for peer access.

`principal_import` carries file path, optional name, force, optional expected
manifest checksum, and `rotate_keys` (default `false`). `principal_import_preview`
carries file path, optional name, force, and `rotate_keys`; without `rotate_keys`
it refuses a snapshot of an actor hosted here before showing the inventory.
`principal_import_previewed` carries snapshot metadata,
roles (`agents`), executable inventory, checksum, validation errors and warnings.
The retired `selected_capabilities`, `required_capabilities`, and `extensions`
fields are omitted from serialization; unknown legacy JSON fields are ignored.

Mid-turn `EnvironmentSnapshot` contains `runtime_environment` only. The old
`permission_policy_summary` JSON field is ignored on read; rendered snapshots
no longer advertise an empty capability allowlist. Completion and steering
payload shapes remain unchanged and are owned by `peko-session`; background
task statuses remain unchanged and are owned by `peko-tools-core`.

## Table of Contents

1. [Conventions](#1-conventions)
2. [config.toml — Agent Image Config](#2-configtoml--agent-image-config)
3. [runtime.toml — Runtime Config](#3-runtimetoml--runtime-config)
4. [team.toml — Team Definition (RETIRED)](#4-teamtoml--team-definition-retired)
5. [Session JSONL — Conversation History](#5-session-jsonl--conversation-history)
   - [5.1 File Conventions](#51-file-conventions)
   - [5.2 Event Envelope](#52-event-envelope)
   - [5.3 Event Types](#53-event-types)
   - [5.4 Session Index File](#54-session-index-file)
   - [5.5 Context Cache (Derived)](#55-context-cache-derived)
   - [5.6 Compaction](#56-compaction)
   5½. [Chat Log — Consumer-Visible Conversation History (RETIRED)](#5½-chat-log--consumer-visible-conversation-history-retired)
   5¾. [Channel Store — Multi-Principal Chat Event Log](#5¾-channel-store--multi-principal-chat-event-log)
6. [Principal Snapshot Format (.peko)](#6-principal-snapshot-format-peko)
7. [Team Package Format (.team) (RETIRED)](#7-team-package-format-team-retired)
8. [Extension Package Format (.ext) (RETIRED)](#8-extension-package-format-ext-retired)
9. [Local Registry Store (RETIRED)](#9-local-registry-store-retired)
10. [Instance State](#10-instance-state)
11. [Markdown Files](#11-markdown-files)
12. [Workspace MCP server definitions](#12-workspace-mcp-server-definitions)
13. [Tool Protocol — stdin/stdout](#13-tool-protocol--stdinstdout)
   13½. [IPC — `ExecuteTool` Packet (ADR-061 phase 1)](#13½-ipc--executetool-packet-adr-061-phase-1)
   13⅝. [IPC — `principal_send_stream` packets](#13⅝-ipc--principal_send_stream-packets)
   13¾. [`ModelCall` — one-shot inference + the judgment wire (ADR-061 phase 2a)](#13¾-modelcall--one-shot-inference--the-judgment-wire-adr-061-phase-2a)
   13⅞. [`Workflow` runner + the `workflows/` directory (ADR-061 phase 2b)](#13⅞-workflow-runner--the-workflows-directory-adr-061-phase-2b)
14. [Extension Manifest (RETIRED)](#14-extension-manifest-retired)
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
| `team_` | Team (RETIRED — ADR-039/041) |
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
#   1. explicit caller override (`peko send --model <id>`)
#   2. session-pinned choice from a prior turn
#   3. agent.preferred_model_id  <-- here
#   4. first enabled catalog entry

# v3 agents only carry soft hints. The actual provider wiring
# (base_url, api_key, model catalog) lives in `~/.peko/models.toml`
# and the encrypted vault via `peko model add` / `peko credential set`.

# ── Base image inheritance ─────────────────────────────────────────────────

[base]
image = "pekohub.com/agents/base-researcher:v2"  # Optional. Full image ref or digest

# Tools are available by presence. There is no extension allowlist.

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
topic   = "tasks.assigned"        # Event bus topic pattern (glob supported)
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

### 2.2 Model catalog (v4, ADR-068)

Models live in the runtime-owned `~/.peko/models.toml`. Each entry describes
one endpoint/model combination; there is no vendor registry or preset list.

```toml
version = "4.0"

[entries.my-model]
id = "my-model"
display_name = "My model"
api_format = "anthropic_messages"
base_url = "https://api.example.com/anthropic"
model_id = "remote-model-id"
requires_key = true
enabled = true
credential_id = "vault-credential-id"
```

The API formats are `openai_completions`, `openai_responses`, and
`anthropic_messages`. Credentials live in the vault; the catalog stores only
references. Optional `context_window`, `max_output_tokens`, `headers`, `spec`
(capabilities/pricing), `compat` (adapter hints), and `note` are user supplied.
An absent `spec` means unknown capabilities and leaves the engine spec gate
inactive; an explicit `spec` enforces declared capabilities. New entries have
no `template_id`. Legacy entries retain that field and all copied settings so
removing templates does not change their endpoint, credential, or metadata.
The opt-in environment bootstrap uses `<ID>_API_KEY` (uppercased, hyphens
changed to underscores), or the legacy `template_id` spelling when present;
there is no vendor alias table. Normal operation uses the vault.

`ModelAddArgs` accepts explicit `api_format`, `base_url`, and exactly one
wire `model` id; `name` is optional and defaults to that wire id. Optional
headers, limits, spec, compat, and note match the catalog. The legacy `custom`
flag is ignored; `template` requests are rejected with migration guidance.
The legacy `ModelTemplates` IPC response is retained with an empty `presets`
list so old desktop clients can still use their custom-model form.

Agent configs carry `preferred_model_id` only — never an inline
`[provider]` block. The v3-cleanup series
deleted the deprecated field entirely (PR #43 follow-up).

### 2.3 Inheritance and Merging

When `[base]` is specified, the runtime loads the base image's `config.toml` first, then merges the local config on top. Merge rules:

- Scalar fields: local value overwrites base.
- Workspace tooling is supplied by plain files; capability arrays are retired.
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

# Remote registry settings are retired (ADR-066).

# ── Provider credentials ───────────────────────────────────────────────────
# There is no [providers] table. Models live in the catalog
# (~/.peko/models.toml, written by `peko model add`) and API keys live in
# the encrypted vault (OS keychain), referenced by each entry's
# `credential_id`. Keys are not read from environment variables.
# Never store keys in this file.

# Tooling is supplied by workspace files; no capability installation settings.

# ── Principal workspace ──────────────────────────────────────────────────────
#
# Teams and the standalone `.peko/agents` / `.peko/teams` roots were RETIRED in
# favor of the Principal-as-container model (ADR-039/041). Agents are thin
# Markdown prompts that live inside a Principal (`.peko/principals/<name>/` or
# bundled in a `.principal` package). The old `[teams]` / `[agents]` config
# tables and their `workspace_root` keys no longer exist.
```

### 3.2 Registry settings (retired)

Remote registry configuration and source credentials are retired in ADR-066 P5.
Local `.peko` snapshots need no registry connection. PekoHub login remains a
separate credential flow.


## 4. team.toml — Team Definition (RETIRED)

> **RETIRED.** Teams were removed in favor of the Principal-as-container model (ADR-039/041). There is no `team.toml` and no `.peko/teams/` directory in the current data model — agents are thin Markdown prompts inside a Principal (`.peko/principals/<name>/` or bundled in a `.principal` package). The schema below is preserved only as historical reference for the pre-ADR-039 layout.

**Historical location:** `.peko/teams/<team-name>/config.toml`

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

**Example (current):** `<principal-workspace>/sessions/sess_9nrwbf1v.jsonl` — sessions are blackboxed under the owning Principal (ADR-041). *(Pre-ADR-039 historical form: `.peko/teams/<team>/agents/<instance>/sessions/...`.)*

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

> **Reparenting:** the `Session` tool's `move` action rewrites
> `parent_session_id` in the session index (the source of truth for
> parentage) and appends a `system` event (`event: "reparent"`, with
> `old_parent` / `new_parent` in `detail`) to the session's JSONL as an
> audit trail. The `session.created` header's `parent_session_id` is
> **stale after a reparent** — it records parentage at creation time and
> is never read back. Moves that would create a parent↔child cycle are
> refused.

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

#### Tool calls and results

Tool calls and their results are `message.v2` content blocks
(`ContentBlock::ToolCall` / `ContentBlock::ToolResult`). The standalone
`tool.call` / `tool.result` event types were never written and are retired;
the per-principal tool ledger (ADR-079) is the durable record of every call.

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

This agent sent a message on the A2A event bus.

```json
{
  "id": "evt_009",
  "type": "a2a.sent",
  "session_id": "sess_9nrwbf1v",
  "ts": "2026-03-16T08:07:00.000Z",
  "seq": 8,
  "message_type": "Task",
  "topic": "tasks",
  "to": "inst_2nqkrp8x",
  "payload": {
    "task": "Find recent news about ACME Corp",
    "priority": "high"
  }
}
```

---

#### `a2a.received`

This agent received a message from the A2A event bus.

```json
{
  "id": "evt_010",
  "type": "a2a.received",
  "session_id": "sess_9nrwbf1v",
  "ts": "2026-03-16T08:07:01.100Z",
  "seq": 9,
  "message_type": "TaskResult",
  "topic": "tasks.results",
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
| `compaction` | Conversation history was compacted (ADR-022) | `summary`, `messages_compacted`, `tokens_before`, `tokens_after`, `compaction_number`, optional `details` (file ops, phase, `user_messages`) |
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
    "compaction_number": 1,
    "details": {
      "read_files": ["src/main.rs"],
      "modified_files": [],
      "image_token_count": 0,
      "phase": "pre_turn",
      "user_messages": [
        "Summarise the Q4 earnings report.",
        "Keep the summary under 200 words"
      ]
    }
  }
}
```

The optional `details` object carries the tracked file operations plus, since compaction audit fix #6, `user_messages`: the compacted-away region's verbatim user messages (oldest first, each capped at 2,000 characters, selected newest-first under a fixed 20,000-token budget). On context rebuild, the summary system message appends them as a `<user-messages>` block (see §5.6.5) so the user's exact intent survives summarization loss. Pre-fix-#6 events have no `user_messages` field and render exactly as before.

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

### 5.6 Planning Todo Sidecar

Planning todos created via the `Task action create`/`Task action get`/`Task action list`/`Task action update` tools are persisted in a per-session JSONL sidecar alongside the main conversation JSONL.

**Location:** `<session-id>.todos.jsonl`

**Format:** Each line is an independently parseable JSON object representing the current state of a single todo. Lines are separated by `\n` and the file is rewritten atomically on every mutation (`<filename>.todos.tmp` + `rename()`).

```json
{
  "taskId": "todo:abc123",
  "subject": "Write tests",
  "description": "Add unit tests for Task action create",
  "activeForm": "Writing tests",
  "status": "in_progress",
  "owner": "claude",
  "created_at": "2026-06-25T10:00:00.000Z",
  "updated_at": "2026-06-25T10:05:00.000Z"
}
```

| Field | Type | Description |
|-------|------|-------------|
| `taskId` | string | Unique todo identifier |
| `subject` | string | Short imperative title |
| `description` | string? | Longer details |
| `activeForm` | string? | Present-continuous form for UI spinners |
| `status` | string | `pending`, `in_progress`, or `completed` |
| `owner` | string? | Owner/agent name |
| `created_at` | string | ISO 8601 creation timestamp |
| `updated_at` | string | ISO 8601 last-mutation timestamp |

**Properties:**

- **Session-scoped**: Todos are tied to the session key; different sessions have independent sidecars.
- **Atomic**: Every create/update rewrites the entire sidecar via tmp + rename, so a crash cannot produce a partially updated file.
- **Optional**: Sessions that never use planning tools have no `.todos.jsonl` file.

---

### 5.7 Compaction

Compaction is the process of summarizing old conversation history to stay within the LLM's context window. It is triggered automatically when token usage crosses a configurable threshold. The `peko session compact` CLI command was removed in ADR-041; compaction is now initiated automatically by the Principal or through extensions/hooks that target the Principal's session-router.

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
2. Emit the compaction's `summary` as a system message (with `<read-files>` / `<modified-files>` and, when `details.user_messages` is present, the `<user-messages>` block — see §5.6.5).
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

<user-messages>
The user sent the following messages verbatim (oldest first):

<message>
Summarise the Q4 earnings report.
</message>

<message>
Keep the summary under 200 words
</message>
</user-messages>
```

File operations are tracked across compactions by scanning tool calls in the messages being summarized.

The `<user-messages>` block (compaction audit fix #6, the Codex
`COMPACT_USER_MESSAGE_MAX_TOKENS` pattern) preserves the compacted-away
region's verbatim user messages so exact user intent survives
summarization loss. The compactor collects them newest-first under a
fixed 20,000-token budget (chars/4 estimate), caps each message at
2,000 characters, truncates the oldest overflowing message to fit, and
skips empty messages and previous summary messages. The block is
appended only when at least one user message was preserved; both the
live compaction path and the resume path render it from
`details.user_messages`, so the reconstructed summary message is
identical after a restart.

The `<archived-pages>` footer (ADR-051, 2026-09-05) is appended after
the `<user-messages>` block whenever the session has at least one
compaction-archived page. It lists one line per page — page number,
compaction number, chars/4 token estimate, first-user-message title
excerpt — plus a pointer to the `Session` tool's `read_page` /
`search_pages` actions. The footer is **derived, never stored**: it is
not part of the boundary event's `detail`; both the live path (the
compaction driver appends it to the installed summary message after the
boundary is recorded) and the resume path (`compaction_summary_message`)
regenerate it by scanning the stored events
(`peko_session::pages::render_page_catalog`), so the two sites produce
identical text and the catalog is always current.

---

### 5.8 Agent-Owned Session Management (2026-08-09)

The unified session/run framework ("coin model") adds the following
format-level changes. Sessions stay blackboxed under the owning
Principal (ADR-041) and user-invisible (ADR-042); every surface here
is agent-facing only.

**`SessionEntry` / `SessionMetadata` new fields** (both
`#[serde(default)]` — pre-2026-08-09 `sessions.json` files load with
both = `false`):

| Field | Type | Description |
|-------|------|-------------|
| ~~`archived`~~ | — | Retired 2026-09-26; ignored when present in old files |
| ~~`standing`~~ | — | Retired; ignored when present in old files |
| ~~`privileged`~~ | — | Retired 2026-09-12 with the subtree rule; reach is now a property of the run (ADR-078: owner runs reach the whole store, others only their own compartment); ignored when present in old files |
| `slug` | string \| null | Per-parent-unique path segment for `/a/b` addressing (2026-08-15, Phase 1b). `#[serde(default, skip_serializing_if = "Option::is_none")]` on the entry. See "Session path addressing" below |
| `compaction_count` | u32 | Successful compactions recorded for this session (compaction audit fix #4, 2026-09-04). The per-session sequence the JSONL boundary event's `compaction_number` derives from; the per-run `BackgroundCompactor` hydrates from it at run start so `max_compactions_per_session` is a per-session limit, not per-run |
| `last_compaction_at` | u64 \| null | Last compaction attempt (success or failure) as epoch milliseconds; drives the cross-run cooldown / escalating failure backoff |
| `consecutive_auto_compactions` | u32 | Consecutive auto-compactions; the per-session gate behind `max_consecutive_auto` |
| `consecutive_compaction_failures` | u32 | Consecutive failed compaction attempts (reset on success); drives the escalating failure backoff across runs |

**`PeerInfo.active_session_id` is now `Option<String>`**
(`#[serde(default)]`). Deleting a session scrubs its id from the
peer's `session_ids` and clears the active pointer only when it
pointed at the deleted session — the peer entry (and its other
sessions) survives, staying routable/listable; the entry is dropped
only when no sessions remain. Pre-change `peers.json` (`String`
values) deserializes unchanged.

**Session ids are stable for life.** The trunk id (the session with
`parent_session_id = None`, looked up via
`peko_session::ownership::find_trunk_session(metas)`) and
spawned/peer-child session ids never change. **Sprint 6 (2026-08-20)
collapsed all engine-internal ids to opaque UUIDs** —
`peko_session::SessionId` is a newtype around `Uuid`, and the legacy
`root:self` magic string is retired. The per-peer
deterministic conversational ids (`root:{peer}`, `root:cron:{peer}`)
were RETIRED on 2026-08-17 (Phase 7) — external ingress lands in
per-peer standing children of the trunk (§5.10); those ids are never
created anymore, and legacy files carrying them are ordinary
unprotected sessions. The 2026-08-09 chapter mechanism (pending
changes in `<sessions_dir>/chapters.json`, id rotation to
`{live}#{YYYYMMDD-HHMMSS}`) was removed on 2026-08-13 (round 7) —
legacy `#`-suffixed JSONLs stay on disk but are inert: never paged,
never stitched, never listed.

**Transcript paging.** When appending an event would push a session's
JSONL past `rotate_bytes`, the file pages in place instead of
rotating the id:

- `<session-id>.jsonl` — the current page (append target, always the
  newest).
- `<session-id>.1.jsonl` … `<session-id>.N.jsonl` — older pages,
  chronological by number (1 = oldest). Page numbers are discovered
  by scanning the sessions dir for `<session-id>.<n>.jsonl` and
  parsing `<n>` numerically; there is no page index in the metadata
  controller.

Rotation renames `<session-id>.jsonl` → `<session-id>.<n>.jsonl`
(`n = max(existing pages) + 1`) under the append `FileLock` and
appends continue into a fresh `<session-id>.jsonl`; the
`sessions.json` entry and `transcript_file` are untouched.
`SessionStorage::load_events` / `load_normalized` stitch pages 1..N
plus the current page transparently, so every reader (history,
context build, compaction, transcript search) sees one continuous
transcript. `delete_session` removes the current page, every page,
and the `.index.json` / `.context.cache` sidecars; `branch_session`
copies all pages preserving relative names under the new id. A boot
sweep pages any oversized live JSONLs found.

**Transcript search.** `session search` is a case-insensitive
substring scan over the text content of user/assistant message events
in each in-scope session's JSONL (tool-call JSON is not matched).
Hits are `(session_id, role, timestamp, snippet)` with a ~160-char
snippet centered on the match. Subtree (spawned) callers are scoped
to their own subtree; archived sessions are excluded.

**Session path addressing (slugs).** Sessions form trees via
`parent_session_id`; each session may carry a `slug` — a
per-parent-unique path segment (1–64 chars, no `/`, no `:`, no
leading/trailing whitespace) — so a session is addressable as
`/a/b/c` from anywhere inside its own tree. `:` is reserved for raw
session ids (the tree-root shape `root:<dim>:<name>` and the
runtime-extension prefixes `spawn:<uuid>:` / `channel:<id>:`); the
`validate_slug` `:` arm keeps the LLM-facing grammar unambiguous
by construction (slugs cannot look like raw ids).
`title` stays free-form display text; the slug is the machine-stable
segment.

- **Uniqueness is per parent**: two sessions may share a slug iff
  their parents differ. Enforced at every set point (`session
  rename`'s `slug` param, the Agent tool's `name` param at spawn,
  `branch`'s derived `<source-slug>-branch` — uniquified
  `-branch-2`, … — and `move`, which re-checks against the
  DESTINATION's siblings) by scanning siblings in the metadata slice;
  a conflict is a structured error naming the conflicting session id.
- **LLM-facing addressing grammar (sprint 5)** — every tool
  parameter that takes a session reference (`session_key` on the
  `Session` tool incl. `move`'s `new_parent`; `session_key` on the
  Agent tool's `resume`/`compact`) accepts one of three forms via
  the canonical `peko_session::path::resolve_reference` entry point:

  | Form | Example | Resolver branch |
  |------|---------|-----------------|
  | Absolute slug path | `/a/b/c` | `resolve_path` (caller-anchored) |
  | Caller's own session id (UUID) | engine self-reference | Self-reference shortcut |
  | Anything else | non-`/`, non-self | **REFUSED** with structured error |

  - `/` alone anchors at the caller's **topmost ancestor** (the root
    of the caller's tree); each further segment selects the child
    of the current node whose slug equals the segment. An unknown
    segment errors with the available child slugs at the failing
    level.
  - The engine self-reference shortcut returns the caller's own
    UUID verbatim — every `current_session` shape at the engine
    boundary (and nothing else) flows through this arm.
  - **Sprint 6 commit 2 collapsed the three-form grammar to two:**
    the caller-relative slug arm (`resolve_relative`) and the shape
    heuristic (`looks_like_session_id`) are retired now that
    engine-internal ids are opaque UUIDs and the only legitimate
    non-`/` input on the LLM-facing surface is the engine's own
    session id. The engine-internal entrypoint
    (`resolve_id_or_path`) is also retired — engine sites
    canonicalize via `SessionId::from` directly.

  Resolution is cycle-safe and happens in the runtime adapter layer
  before the (unchanged) ownership guards; ids remain the canonical
  key everywhere else, and resolver input is slug-only on the
  LLM-facing surface — raw ids are never accepted there.
- The trunk carries no slug and is addressable only as
  `sess:/` (legacy `/`) from inside its own tree. Listings and prompt context
  return `sess:/`, which round-trips through read tools and list/find scopes.
  Mutation path validation remains unchanged; storage ids and transcripts
  are unchanged (2026-10-07).
- `session list` shows every session the run may reach — the whole
  store for an owner run, its own compartment for any other run
  (ADR-078) — and the `path` parameter narrows to a subtree within that.
  (The earlier subtree default and `scope: "principal"` widening were
  retired with the subtree rule.)
- `session list` entries carry `slug` and a computed absolute `path`:
  ancestors without a slug are skipped as
  intermediate segments; the trunk is `sess:/`, and other slugless targets fall back to their raw
  id as the last segment (e.g. `/memory/550e8400-…`).
- `Agent` `action = "new"` REQUIRES a `name` slug (validated
  upfront via `validate_slug`); standing-child attach by name still
  works — a `name` matching an existing `standing == true`
  session in the caller's subtree re-attaches via the resume path
  instead of minting a fresh session.

The pure resolver/validators live in `peko_session::path`
(`resolve_reference`, `resolve_path`, `compute_path`,
`validate_slug`, `slug_conflict`, `derive_branch_slug`) over
`&[SessionMetadata]` slices — no IO, no locks, mirroring the
ownership-guard style. **Sprint 6 commit 2** retired
`resolve_id_or_path`, `resolve_relative`, and `looks_like_session_id`
(now that engine-internal ids are opaque UUIDs and the LLM-facing
grammar is strict two-form).

**Standing named children (`[children]` in `principal.toml`, Phase
2).** A principal may declare standing first-level children that
persist for the principal's lifetime and are resumed by name:

```toml
[children.memory]
subagent_type = "archivist"        # required, nonempty
description = "Long-term memory"   # optional; becomes the session title
```

**Principal runtime id (2026-09-06).** `principal.toml` carries an
optional top-level `id = "prin_..."` — the principal's stable runtime
`PrincipalId`. Written at create; on load a missing `id` (legacy file)
is generated once and re-persisted. Before this field the id was
regenerated on every load, which forked peer DM channels on each daemon
restart. **Agent name-keyed identities:** the root agent's DID is
resolved through `<identities_dir>/by-name/<name>.json` (a small
`{"did": "..."}` alias) instead of re-minting per turn.

Each table key (`memory`) is the child's slug and must pass
`validate_slug` (1–64 chars, no `/`, no surrounding whitespace); a bad
name or a missing/blank `subagent_type` is a structured load error.

At root-agent run setup the runtime ensures each declared child EXISTS
(`principal::children::ensure_declared_children`): a session whose
`slug == <name>` AND `standing == true` AND `trigger == "spawn"` whose
parent chain roots at the principal's trunk session (looked up via
`peko_session::ownership::find_trunk_session(metas)` — sprint 6
re-anchored from the retired owner root `root:{owner}` in Phase 7
and from the retired `root:self` magic string in sprint 6) is left
untouched; otherwise the child is created — metadata + JSONL
created-event only, **no LLM turn, no run** — with:

| Field | Value |
|-------|-------|
| `trigger` | `"spawn"` |
| `standing` | `true` (prune-exempt) |
| `slug` | the declaration name |
| `parent_session_id` | the trunk session UUID (resolved via `find_trunk_session(metas)`); may DANGLE until the first self-turn creates the trunk — the ownership walks tolerate this by design |
| `title` | `description`, falling back to the name |

The declaration is also recorded as a `system` event
(`"event": "standing_child_declared"`, detail `{name, subagent_type,
description}`) in the child's JSONL so a later attach can recover the
declared `subagent_type` without re-reading the principal config.
Ensure-declared failures (corrupt config, storage errors) warn and
continue — they never crash principal init.

The `Agent` tool's `new` action with a `name` param attaches by name:
when the caller's subtree already contains a session with `slug ==
name` AND `standing == true` AND `trigger == "spawn"`, the run
re-attaches to it via the resume guard stack (the call's `prompt`
drives the turn; `subagent_type` must match the recorded declaration
when one is recoverable). A `name` colliding with a NON-standing
session is a structured refusal — rename semantics live in the
`Session` tool. No `name` → unchanged fresh-UUID spawn.

### 5.9 The Principal Trunk Session (2026-08-15, Phase 3; Phase 7 re-route 2026-08-17; sprint 6 re-anchor 2026-08-20)

A principal has a forever-continuous SELF session — the trunk `/` of
its session tree — anchored as the session with
`parent_session_id = None`, looked up via
`peko_session::ownership::find_trunk_session(metas)`. **Sprint 6
(2026-08-20) retired the legacy `root:self` magic-string id**:
engine-internal ids are now opaque UUIDs, and the trunk anchor is
"the root of the tree" rather than a literal. The trunk is keyed by
no peer: it is the principal's own ongoing working session, keeping
the principal an active actor (supervising children, organizing
memory) instead of a passive request handler.

**Phase 7 (2026-08-17): the trunk is the ONLY root-family session.**
The per-peer root sessions (`root:{peer}`, `root:cron:{peer}`) are
retired — the routing code that minted them is deleted, and those ids
are never created anymore:

- **All external ingress lands in per-peer standing children of the
  trunk** (§5.10). `PrincipalManager::receive` / `receive_streaming`
  and the IPC `principal_send` handler find-or-create the peer's child
  (`ensure_peer_child`) and drive the turn in it via the shared
  `PeerChildTurns` bundle (`SubagentExecutor::resume_streaming` under
  the hood). The permission check (`build_router_context`), DM-channel
  projection (the peer's `dm-<slug>` channel), and the per-peer
  serial queue all survive — the run permit and steering inbox are
  re-keyed to the CHILD session id, and the child run drains that
  shared inbox at iteration boundaries (a concurrent second send
  steers the live child run). `ChannelKind::Cron` arriving at
  `receive`/`receive_streaming` delegates to the trunk path.
- **The `RootRouter` is trunk-only**: `route`/`route_streaming`
  refuse peer channels loudly (a routing bug, not a fallback), and
  `root_session_id_for_channel` resolves both `Trunk` and `Cron` via
  `find_trunk_session`. The retired per-peer memory-recall artifact
  writes are gone; `PrincipalManager::record_peer_recall` (keyed by
  child session id) is the write path.
- **Guards:** the engine-managed refusals (delete/archive/move
  "managed by the engine" in the session tool surface) and the
  idle-prune exemption match exactly the trunk UUID (resolved via
  `find_trunk_session`). Retired-shape ids carry no protection —
  they are plain sessions (and are never created).

- **On disk** it is an ordinary session JSONL
  (`<sessions_dir>/<trunk_uuid>.jsonl`) with the standard paging,
  index, and metadata treatment; nothing about the file format is
  special.
- **Turns** arrive via `PrincipalManager::receive_trunk`, which uses
  the principal's owner as the *proxy subject* for the permission
  check and memory recall (existing gates work unchanged) but always
  runs in the resolved trunk UUID (`ChannelKind::Trunk`). Trunk turns
  skip slash preprocessing and keep the per-session run permit +
  steering fallback (a second tick steers the live run).
- **Conversation record:** trunk turns are NOT projected into any
  per-peer conversation record — the trunk has no peer thread. The
  trunk session JSONL is the durable record (the chat-log store that
  once held per-peer projections was retired in Phase 13).
- **Messenger:** `resolve_peer_via_parent_walk(trunk_uuid, metas)`
  returns `None` — the trunk has no external peer (no `peer_type` /
  `peer_id` stamped on its metadata) and must never resolve as a peer
  literally named "self". The retired `root:{peer}` /
  `root:cron:{peer}` key forms no longer parse as peer keys at all;
  `originating_peer` resolves peer-child sessions from their stamped
  `peer_type`/`peer_id` metadata plus the parent-linkage walk
  (`resolve_peer_via_parent_walk`, sprint 6 commit 1 — replaces the
  deleted `peer_from_session_key`). *Retired by ADR-078:* whom a run
  serves is its `ActingFor` stamp; `originating_peer` is removed.

**Cron targeting (Phase 7).** `CronJobAction::Send`'s optional
`target` field (`#[serde(default, skip_serializing_if =
"Option::is_none")]`, validated on deserialize) has collapsed to a
single destination — the trunk:

| Value | Fire behavior |
|-------|---------------|
| absent / `null` (default) | The turn fires into the trunk UUID via `receive_trunk`; the fired prompt lives in the trunk session JSONL (the `record_cron_input` chat-log projection was dropped in Phase 13), and the outcome is cross-posted as a `⏰ [cron job '<name>' fired] …` note to the OWNER'S PEER CHILD session (found via `find_peer_child` — the retired `root:{owner}` is never addressed) |
| `"trunk"` | Identical to the default — the Phase-3 "trunk skips the note" special case is gone (the note lives in the owner child, not the trunk, so it no longer duplicates anything) |
| anything else | Structured error at JSON load, at `CronScheduler::add_job`, and (defensively) at fire time |

Cron `Notify` jobs (pure delivery, no turn) append their `⏰` note to
the target peer's child session (find-only — a note never provisions
a child) and the `[notify]` self-view line to the trunk's JSONL when
the trunk exists.

Creation surface: the `Cron action create` tool's `target` param (valid only
together with `message`). The legacy `peko cron add / at / every /
add-idle / add-event` CLI subcommands were retired on 2026-08-25
when cron became a fully internal principal tool.

**SpawnTool wake attribution (Phase 3b, 2026-08-15).** `SpawnTool`
jobs take no `target` param; their `wake_on_completion` steer message
posts to the trunk inbox (pre-3b: the owner's conversational
`root:{owner}` inbox). PEKO.md §K requires cron `Send` and SpawnTool
wakes to target the same principal root — the trunk.

**Trunk keepalive floor (Phase 3b; widened in Phase 7).** A `Send`
job with an `Every { every_ms }` schedule is refused at
`CronScheduler::add_job` when `every_ms < 60_000`
(`TRUNK_MIN_INTERVAL_MS`) — every tick is a full LLM turn in the
trunk, so a floorless self-poke loop is a runaway token-burn
anti-pattern. Since Phase 7 every Send job is trunk-bound, so the
floor applies to the default target as well as explicit `"trunk"`.
`At`, `Cron`, `Idle`, and `Event` schedules are exempt
(explicit or event-driven cadence).

### 5.10 Peer-Child Provisioning (sprint 2 Phase 5, 2026-08-17; sprint 6 re-anchored)

Every external ingress peer gets a standing first-level child of the
trunk that acts as that peer's permanent conversation
with the principal. `principal::peer_children::ensure_peer_child`
find-or-creates it (metadata + JSONL created-event only — no LLM turn,
no run); idempotent per peer. The child's slug is derived by
`peer_child_slug`:

| Peer | Slug |
|------|------|
| `user:local` (CLI owner) | `local-user` |
| `user:{id}` | `user-{sanitized}` — lowercase ascii alnum kept, everything else → `-`, repeats collapsed, ends trimmed; empty result falls back to bare `user` |
| `principal:{did}` | `principal-{fragment}` — first 16 lowercase ascii alnum chars of the DID |
| `public` | structured error — public is not a session peer (ADR-039) |

Slugs pass `validate_slug` and are capped at 64 chars. A slug already
held under the trunk by a DIFFERENT session (sanitized collision, e.g.
`user:Foo Bar` vs `user:foo-bar`) retries with `-2`, `-3`, … suffixes;
each peer's lookup walks the same suffix chain so provisioning stays
idempotent per peer.

Created children carry:

| Field | Value |
|-------|-------|
| `trigger` | `"spawn"` |
| `slug` | the derived (possibly suffixed) slug |
| `parent_session_id` | the trunk UUID (resolved via `find_trunk_session(metas)`); may DANGLE until the first self-turn creates the trunk — the ownership walks tolerate this by design |
| `title` | the peer's display string (e.g. `user:alice`) |
| `peer_type` / `peer_id` | the REAL peer (not the `standing_*` placeholder declared children use) |

**Ownership semantics.** Reach comes from the run's acting-for stamp,
not from the child (ADR-078): a run acting for the owner reaches the whole
store, a run acting for anyone else only its own conversation's
compartment. (The `privileged` flag the owner's child used to carry was
retired 2026-09-12.) The self-safety guards — ancestor, self-mutation,
trunk — apply to every run. The ingress re-route that sends peer traffic to these children landed in
Phase 7 (§5.9): `PrincipalManager::receive` / `receive_streaming`, the
IPC
`principal_send` handler, and the group-channel wake all
drive turns in the peer's child via the shared
`principal::child_turns::PeerChildTurns` bundle.

**Peer-facing T1 role (`[routing].peer_agent`, ADR-052 D3 —
2026-09-11).** By default every peer-facing turn (peer-DM ingress
children, passive channel bindings, group wakes) inherits the
principal's ROOT persona. A principal may opt out per-workspace by
naming a role in `principal.toml`:

```toml
[routing]
peer_agent = "channel-comm"   # Optional. Default: None (root persona)
```

The value resolves through the Agent tool's lookup order —
`<workspace>/agents/<name>/AGENT.md` then
`<workspace>/agents/<name>.md` — and that file's Markdown body
replaces the root prompt body for those turns. Only the prompt body
swaps: the agent/session naming stays the root prompt's name (the
cross-path active-run guard keys on it). An unresolvable or
unparseable role logs a warning and falls back to the root persona —
a bad value never breaks peer ingress.

**Recall artifact target (Phase 6, wired in Phase 7 — 2026-08-17).**
The `memory_index.json` `SessionArtifact` shape is unchanged, but the
`session_id` VALUE for peer-chat recall is re-pointed: pre-paradigm
the root router artifacted the peer's `root:{peer}` session;
`PrincipalManager::record_peer_recall` (called by the Phase 7 ingress
wrappers around the child-turn driver) artifacts the peer's standing
child session id instead. The read side
(`PrincipalMemory::find_latest_session_for_peer`) is peer-keyed and
unaffected. No migration: stale `root:{peer}` artifacts simply age
out of use.

---

## 5½. Chat Log — Consumer-Visible Conversation History (RETIRED)

> **RETIRED (sprint 3 Phase 13, 2026-08-19).** The `peko-chat-log`
> crate was deleted from the workspace once its last writer — the
> cron `Send` fired-prompt projection
> (`PrincipalManager::record_cron_input`) — was dropped (sprint 3
> Phase 11 had already moved the peer-conversation projection onto
> the per-peer DM channels, and Phase 12b moved the messenger's
> notes there too). The consumer-visible conversation record is now
> the **peer DM channel** (§5¾): `peko log` and the `principal_log`
> IPC read the DM channel's `Posted` events, and the row type lives
> on as `peko_core::ipc::packet::PrincipalLogMessage` (same wire
> shape as the old `ChatLogMessage`). Pre-Phase-11 shards under
> `<data-dir>/chat_logs/` stay on disk unread; nothing reads or
> writes them anymore. The description below is preserved only as
> historical reference for the on-disk history.

The chat log is the runtime-owned, append-only, **consumer-visible**
record of the text messages an external participant actually
exchanged with a Principal. It is **distinct from the session
JSONL** (§5): the session JSONL is mutable principal-owned working
memory optimized for the agent loop, while the chat log was —
pre-Phase-11 — the only record surfaced by `peko log`, the desktop's
`principal_log` IPC, and any future chat-history surface.

The two stores are intentionally separate so internal execution
detail (prompts, tool calls, thinking blocks, compactions, model
changes, branches) cannot leak into the user-visible chat surface
and so internal session evolution cannot disturb the consumer
contract.

### 5½.1 File Conventions

**Root:** `<data-dir>/chat_logs/` (resolved via
`PathResolver::chat_logs_dir()`).

**Shard layout:** one JSONL file per `(principal, peer)` pair:

```
<root>/<blake3(principal_did)>/<blake3(peer_subject)>.jsonl
```

Both directory and file names are full BLAKE3 hashes so the layout
is self-validating (any mismatch between the hashed path and the
shard's header record is treated as corruption) and so the layout
is safe under arbitrary `ActorRef` and DID strings.

**Per-shard header:** the first line of every shard is a
self-describing record so the file can be validated without
consulting any external index:

```json
{"kind":"thread","schema_version":1,"principal":"did:peko:principal:…","peer":{"kind":"user","id":"alice"}}
```

The header pins the `(principal, peer)` pair the shard claims to
represent. A shard with a header that doesn't match its hashed
path fails `ChatLogStore::read_page` with
`ChatLogError::ThreadMismatch`.

### 5½.2 Record Envelope

Two record kinds appear on a shard; both are JSON objects on a
single line:

```jsonc
// thread header — first line of every shard
{"kind":"thread","schema_version":1,"principal":"…","peer":{…}}

// one consumer-visible message
{
  "kind":"message",
  "schemaVersion":1,
  "id":"chat_<uuid-simple>",
  "sender":<ActorRef>,
  "timestamp":"2026-07-20T12:34:56.789Z",
  "text":"…",
  "correlationId":null|"<id>"
}
```

Fields:

| Field | Type | Notes |
|-------|------|-------|
| `schemaVersion` | u8 | Currently `1`. Bumping it forces a migration. |
| `id` | string | `chat_` prefix + UUID simple-hex. Stable per shard. Used for paging dedupe on the desktop. |
| `sender` | `ActorRef` | `{kind:user,id:…}`, `{kind:principal,id:<did>}`, or `{kind:public}`. Determines direction. |
| `timestamp` | RFC 3339 | UTC. |
| `text` | string | Plain text. The runtime never persists markdown, tool output, or thinking blocks here. |
| `correlationId` | string? | Pairs request/response lines for principal-to-principal exchanges. |

### 5½.3 Append Semantics

Appends follow the same durability contract as the session JSONL
(§5.1) so torn final lines and racing appenders behave the same
way:

- `O_APPEND` open so concurrent writers never interleave bytes
  inside a single line.
- `write_all` then `sync_data` per record, then `sync_dir` on the
  parent directory so the directory entry is durable.
- A `FileLock` keyed on the shard path is acquired before each
  append and held until `sync_data` completes; the lock has a 10 s
  acquire timeout and PID-based stale detection.
- A process-local `tokio::sync::Mutex<()>` per shard serializes
  in-process appenders before they reach the cross-process lock.
- Torn final lines are detected on read (line does not end in
  `\n` after `serde_json::from_slice::<ChatLogRecord>` returns
  `Err`) and silently dropped — same shape as session JSONL.

### 5½.4 Sender Constraints

`ChatLogStore::append_message` rejects senders that aren't one of
the two participants of the thread (`ActorRef::Principal(key.principal)`
or `key.peer`) with `ChatLogError::InvalidSender`. The constraint
is in place so a runtime bug can't sneak an unrelated ActorRef
into a peer's thread and break the privacy contract — every
line in a shard is provably between the two parties the shard
is named after.

### 5½.5 Paging and Cursors

Reads start at EOF and walk backward in bounded 64 KiB chunks,
collecting `limit + 1` messages so the caller can detect
`has_more`. The page is then reversed so the caller sees
oldest-to-newest within each page.

Cursors are **opaque, versioned, thread-bound** base64-url strings
carrying the thread fingerprint (BLAKE3 of `<principal>\0<peer>`)
and the byte offset before the oldest returned line:

```
<version><fingerprint-hash-hex><base64-url(offset, 8 bytes BE)>
```

A cursor issued for thread A is rejected (`ChatLogError::Cursor`) if
reused against thread B, and a cursor pointing past EOF or
mid-line is rejected (`ChatLogError::InvalidOffset`). Cursors
remain valid across subsequent appends because appends only grow
the file monotonically — older byte offsets stay valid.

### 5½.6 Lifetime

Chat-log shards share the lifetime of the principal they belong
to. `PrincipalManager::remove` resolves the principal's stable DID
before evicting in-memory state, then calls
`ChatLogStore::remove_principal` which deletes the principal's
shard directory. Cleanup failures surface as
`PrincipalManagerError::Io` rather than silently retaining
removed data.

There is no retention, no tombstones, no legacy fallback, and no
migration path from session JSONL to chat log. The split is a
clean pre-launch cutover.

### 5½.7 Channel Filter

Only peer-chat channels are logged: `Cli`, `Http`, `Hub`, `A2a`
(covering both `A2a` and `P2p`), and `Webhook`. Automation
inputs (`Cron`, `FileWatch`) are deliberately excluded because
they're not conversations — they don't have a peer, and surfacing
their outputs as chat messages would mislead the consumer.

For principal-to-principal exchanges, **both sides record their
own view** (the recipient via `PrincipalManager::receive` with
`ChannelKind::A2a`; the caller via `SendPeerTool` in
`tunnel::principal_send_tool.rs`). The two views are independent
shards keyed on each principal's DID; deleting one principal
removes only its own view.

---

## 5¾. Channel Store — Multi-Principal Chat Event Log

`peko-channel` (`peko-rs/channel/`) storage. Distinct from the
session JSONL (§5, the principal's private working memory): a
channel's log is an append-only event log shared by its members.
Since Phase 13 it also IS the consumer-visible conversation record
(the chat log, §5½, is retired) — `peko log` reads the peer DM
channels.

### 5¾.1 On-Disk Layout

```text
<runtime_dir>/channels/<channel_id>/
  meta.json       # channel metadata (schema below)
  members.json    # { members: [String], remote_members: [RemoteMember] }
                  # members rows are canonical addresses (`principal:local:prin_x`,
                  # `user:hub:<id>`); rows from before ADR-081 (bare principal IDs,
                  # `kind:<id>`) still read as the same member.
  events.jsonl    # current page: one EventLine per line, append-only (see below)
  events.<n>.jsonl # rotated pages, 1 = oldest (present only after rotation)
  cursors.json    # per-member "last observed line" map (HashMap<PrincipalId, TaskId>)
  read_marks.json # per-session digest read positions (HashMap<session-id, TaskId>)
  attention.json  # per-session waiting lists (HashMap<session-id, [AttentionItem]>)
```

**Event-line provenance envelope (ADR-058 D7).** New `events.jsonl`
lines are a storage-layer envelope, not a bare `ChannelEvent`:

```json
{"origin": "local" | "verified-remote", "event": { ...ChannelEvent... }}
```

`local` covers every local append (`create`, `post`, `invite`,
`leave`, synthetic `Created`); `verified-remote` is written only by
`append_remote_event`, the mirror path the inbound dispatcher feeds
after the D2 envelope signatures verify. Legacy lines (bare
`ChannelEvent` JSON, everything written pre-D7) are read as
`{"origin": "local"}` — the two formats are distinguished by the
presence of a recognized `origin` field, which no `ChannelEvent`
variant carries. The `peko_protocol::channel::ChannelEvent` wire type
is unchanged; the envelope exists only on disk.

The current page rotates aside to `events.<n>.jsonl` when an append
would push it past 8 MiB (`DEFAULT_ROTATE_BYTES`), mirroring
`peko-session`'s `<id>.<n>.jsonl` paging; rotated pages are immutable.
`TaskId`s are **global line numbers across the stitched log** and
never reset on rotation, so cursors, reply `parent` references, and
`peko log` pagination survive page rolls. Pre-paging channels (a lone
`events.jsonl`) are simply a one-page log — no migration needed.

`read_marks.json` is a **sibling of `cursors.json`, not a reuse**: the
subscriber cursor is per-principal and advances on every event the
subscription loop observes (delivery-oriented), so it can never
represent "what has this *session* actually been shown". Read marks
are keyed by session id and advance only when the session's
`{{session_context}}` digest renders (or filters) activity up to a
line, or when its `ChannelRead` returns that line — they are what the
unread summary means by "since you last read". A session's first look
at a channel adopts the newest line as its mark, unless the channel was
opened after the session was created: then it is unread from its first
post. Both files
share the atomic tmp+fsync+rename save convention. Each member's
subscriber writes only its own `cursors.json` entry, merged into the
file under its lock (`ChannelCursors::save_member`), and never moves a
numeric cursor backwards; temp file names carry the pid and a random
suffix. The shape is unchanged.

`attention.json` (ADR-072 D4a) holds, per session, the posts by other
principals the digest has shown but the session has not answered — each an
`AttentionItem {id, author, at, preview}` (line id, author wire form,
RFC 3339 time, truncated body), oldest first, at most 50 per session.
An item leaves when the session's `ChannelRead` returns that line or a post
by the principal lands after it. `read_marks.json` and `attention.json` are
written under a `FileLock` (`<file>.lock`) held across load → save.

Shared-tier channels (PR-3d `pin_to_shared`) use the same layout
under `<shared_dir>/channels/<channel_id>/`.

### 5¾.½ ChannelId Typed Prefixes (sprint 4)

A `ChannelId` (`peko-rs/protocol/src/channel.rs`) is a `String` newtype
whose wire form selects the dispatch branch of `ChannelSend`:

| Wire form             | Kind       | Dispatch                                   |
|-----------------------|------------|--------------------------------------------|
| `chan_<8 base36>` / `dm_<64 hex>` | `Bare` | Conversation post |
| `principal:<ref>`     | `Principal`| Resolve the actor (ADR-073 `principal:local:<id>` / `principal:hub:<UUID>`), provision the pair DM, post, and return (ADR-072 D3: never waits for an answer) |
| `user:<id>` / `visitor:<id>` | `User` | Note to a known human peer (originating-user gate) |
| `group:<slug>`        | `Group`    | Plain fire-and-forget post                 |

`ChannelId::parse` accepts any of the four forms; `ChannelId::kind()`
returns the dispatch kind in O(1). The wire form IS the routing
identity for `principal:<did>` channels (sprint 4 — `peer_dm` sets
`CreateOpts::id` to `ChannelId::for_principal(did)` so `peko log`
consumers and `ChannelSend`'s dispatch agree without translation).

**On-disk path normalization**: typed-prefix ids contain colons
(invalid in directory names on Windows / classic Unix filesystems).
The wire form is human-readable; the storage path uses `.3A.` in
place of every `:` (`peko-rs/channel/src/fs.rs::channel_dir_name`).
Bare `chan_*` ids are unchanged (no colons). `channel_dir_name` and
`channel_dir_name_inverse` are inverses on every valid wire form.

### 5¾.2 `meta.json` Schema

```json
{
  "creator": "prin_alice",
  "name": "design-sync",
  "created_at": "2026-08-05T12:00:00Z",
  "tier": "Runtime",
  "kind": "group",
  "bindings": {
    "prin_alice": { "session": "0b7c…", "wake": "mention", "role": "support" }
  }
}
```

| Field | Type | Required | Notes |
|---|---|---|---|
| `creator` | string | yes | Creator principal id (a mirror keeps the source's creator, display only). |
| `name` | string | yes | Free-form display name. |
| `created_at` | RFC3339 | yes | Snapshot at create time. |
| `tier` | `"Runtime"` \| `"Shared"` | yes | Storage locality (NOT the DM/group split). |
| `kind` | `"dm"` \| `"group"` | yes | ADR-082 D1; immutable. A `dm` has a pair id (`dm_<sha256>`) and exactly its two members for life: invite and leave are refused. |
| `bindings` | object | no | ADR-082 D2: local principal id → `{ session, wake, role? }`. Runtime-local, never on the wire. Omitted when empty. |
| `participants` | string[] | no | A DM's fixed pair (canonical addresses); groups use current membership. |

A binding names the principal's conversation session for the channel
(a session id, or the bare `/slug` path of a DM's peer child) and its
wake policy: `human` (every human root post starts a turn — a DM's fixed
policy), `mention` (a human root post naming `@<name>` — a group's
default), or `off`. Principal-authored posts and replies never start a
turn under any policy (ADR-072). Only the runtime writes a binding, when
it creates that session inside the channel's compartment: a DM's on
first ingress or first `ChannelSend`, a group's on first wake or on
`Channel create` / `wake`. The session side (`SessionEntry.channel_id`)
is authoritative; a stale binding is rewritten when the runtime next
ensures the session. Leaving a group deletes the binding and keeps the
session as history.

`role` (ADR-087 D4, optional, omitted when unset) names the role
(`roles/<id>.md` or `roles/<id>/ROLE.md`) the principal's turns run in
that conversation, on either kind. Unset, a turn runs `routing.peer_agent`
if set, else the seeded `conversation` role, else `root`; a role that does
not resolve falls to the next. Rebinding (a DM's re-ensure, a group's
`bind`) keeps it. Only the principal sets it (`Channel role`); IPC does
not expose it.

Files written before ADR-082 carry no `kind`: a `passive_binding`
marked a DM, and bindings were bare session strings. They are read in
the new shape (`passive_binding` present → `dm`; string bindings get the
kind's default wake) and written that way on the next save.

Cross-runtime mirrors (`ChannelStore::join_remote`) take `kind` from the
signed invite (`peko-channel-invite/3`, which carries `kind` instead of
the creator's session path) and bind the receiver's own peer child for a
DM. An invite whose kind disagrees with an existing mirror, or a `dm`
invite without a pair id, is refused.

IPC exposes the wake policy, never the session: `channel_info { channel }`
(members only, like `channel_peek`) answers `channel_info_result`, and
`channel_set_wake { channel, principal_name, wake }` (operator callers,
the principal's owner, groups only, the principal a member) answers
`channel_wake_set`:

```json
{"type": "channel_info_result", "request_id": 1, "channel": "chan_01qwjy2l",
 "kind": "group", "name": "team",
 "members": [{"kind": "principal", "id": "prin_alice"}, {"kind": "user", "id": "local"}],
 "pekos": [{"name": "alice", "local_id": "prin_alice", "wake": "mention"}]}

{"type": "channel_wake_set", "request_id": 1, "channel": "chan_01qwjy2l",
 "principal_name": "alice", "wake": "off"}
```

`pekos` lists the member principals hosted on this runtime. A DM's is
always `human`; a group member without a binding yet reads as `mention`.
Setting a wake writes the binding first if the principal has none, the
same as the `Channel` tool's `wake` action.

### 5¾.2½ `posted` event `via` attribution (2026-09-12)

`posted` lines in `events.jsonl` may carry an optional `via` field:
the session slug path (e.g. `/user-a/reminder`) of the posting agent
within the author's principal. It is written verbatim and is
**attribution/audit metadata only** — it never feeds membership
checks, the channel responders' trigger rules, or reply matching, and
it is NOT an authority claim (`author` remains the only authorship
signal). The unread summary uses it to skip a session's own posts.

`via` is absent (not `null`) on lines written before this change, on
posts without an agent context (CLI `peko channel post`, peer-authored
inbound projections, messenger notes),
and whenever the posting side could not resolve the calling session's
path — attribution is best-effort and never fails a send. Serde
`default` + `skip_serializing_if` keeps old logs and mixed-version
cross-runtime peers byte-compatible. Producers: the `ChannelSend`
tool (the calling session's path), the group channel responder
(the bound session's path), and `peer_dm::post_peer_dm_reply` (the
bound peer child's path).

### 5¾.2¾ `posted` event `msg_id` / `reply_to` (2026-10-12)

`posted` lines carry two more optional fields:

- `msg_id` — a stable message id (`msg_<uuid>`), minted by the store
  that writes the post (or supplied by the poster, so one message can
  be written to two channels under one id: a same-runtime
  `ChannelSend`'s record and delivery copies). It is
  carried verbatim to every runtime's mirror; line numbers are not.
- `reply_to` — the `msg_id` of the post this one replies to, derived by
  the writing store from `parent`'s line. It is the cross-runtime form
  of `parent`.

Neither field changes who is woken: only humans and cron start runs
(ADR-072). Both fields are absent on lines written before this change; serde
`default` + `skip_serializing_if` keeps old logs readable. Both ride
inside the signed event bytes of cross-runtime envelopes.

### 5¾.2⅞ Cross-runtime delivery files (2026-10-12, ADR-072)

Under the channel runtime dir, `channel_delivery/`:

- `outbox/<seq>-<request_id>.json` — one file per outbound envelope not
  yet confirmed by its recipient runtime: `{seq, request_id,
  recipient_runtime_id, item, created_at, attempts, last_attempt}`,
  where `item` is `{"kind":"event", channel, event, source}` or
  `{"kind":"invite", channel, inviter, creator_did, invitee}` — the
  unsigned parts, re-signed on every attempt. The zero-padded `seq`
  keeps send order. Deleted on receipt; dropped after 30 days.
- `received.jsonl` — `{request_id, at}` per inbound envelope applied,
  so a resend is confirmed again but not applied twice; rows older than
  30 days are pruned on load.

Receipts travel as `{"kind":"received", channel, request_ids, at}`
channel events inside `TunnelChannelEvent` envelopes and are never
written to a channel log.

### 5¾.3 Mirror `members.json` partition (Phase 12a)

`join_remote` re-partitions the invite's source-keyed membership
snapshot to the receiver's view:

- `members` (local) is exactly the receiver's own principal id —
  resolved by the host bootstrap (`AppState::dm_channel_mirror_bootstrap`)
  from the invitee row (the snapshot row with `runtime_id: None`,
  which carries the invited principal's DID). This is what makes the
  mirror visible to `list_for_principal` (the boot sweep) and lets
  the receiver's own `post` pass the membership check — source-side
  id forms never match the receiver's local `prin_<uuid>`.
- `remote_members` is the creator (filed under the envelope's
  `source_runtime_id`) plus every snapshot row carrying a
  `runtime_id`, deduped on the `(runtime_id, principal_id)` pair —
  so the receiver's own posts fan back out to the source runtime.
- Invitee rows (`runtime_id: None`) are dropped once the receiver's
  local principal is resolved; they carry no receiver-local meaning.

The source side, symmetrically, records the invitee as a
`RemoteMember` when the invite fans out
(`TunnelChannelPort::fanout_dm_invite` calls `add_remote_member`
before sending), with `principal_id` = the invitee's bare id/DID
(its `@<runtime>` routing suffix stripped).

### 5¾.3½ Cross-runtime envelope signatures v2 (ADR-058, 2026-09-16)

`TunnelChannelEvent` / `TunnelChannelInvite` envelopes on the tunnel
wire carry TWO signatures, both **compact JWS** (EdDSA / RFC 7515,
embedded payload, header `{"alg":"EdDSA","typ":"JWS"}`,
base64url-no-pad; signing input `b64u(header) || "." ||
b64u(payload)` — canonical by construction, so the hub's
`JSON.parse`→`stringify` relay round-trip cannot break them):

- `signature` — the source **runtime** counter-signature, verified
  against the key derived from `sourceRuntimeId` (`did:key`).
- `authorSignature` — the **author principal**'s signature over the
  SAME payload segment (byte-identical binding), verified against the
  key embedded in a `did:key` `sourcePrincipalDid`. Empty when the
  author has no vault-backed key (legacy runtime-vouched path).

Both JWS embed one JSON payload, versioned `peko-channel-event/2` /
`peko-channel-invite/3`, with snake_case fields: every envelope field
(`request_id`, `source_runtime_id`, `recipient_runtime_id`,
`source_principal_did`, `channel_id`, plus `creator`, `creator_did`,
`name`, `kind` (`dm` / `group`, ADR-082) for invites), the inner payload as
`event_b64u` / `initial_members_b64u` (base64url-no-pad of the
serialized `ChannelEvent` / `InitialMember[]` bytes), and `iat`/`exp`
(Unix seconds; sender mints `exp = iat + 300`). Receivers reject
payloads with `now > exp + 30s` or `iat > now + 60s`, any
envelope↔payload field mismatch, and — for `did:key` authors — a
missing/invalid author signature or (events) a non-member author /
(invites) `creator_did != source_principal_did`. The v1 bespoke
length-prefixed pre-image is deleted (no compat shim, pre-launch).

Principal DIDs of new principals are `did:key` of the principal's own
ed25519 key (D1); the seed lives in the vault under the
`principal-identity` namespace as `{did}#keys-1` (base64-STANDARD
32-byte seed, `ed25519-raw-base64`, `system_owned`).

### 5¾.4 Read/write performance notes (2026-09-03)

The append path is O(1) in the common case: `ChannelStore` keeps a
per-page `(line_count, byte_len)` cache (in-memory only) and trusts a
page's entry only when the file's current byte length matches —
rotated pages are immutable so their entries never go stale, and an
external append to the current page changes its length and forces a
recount. `members.json` read-modify-write cycles
(`invite` / `leave` / `add_remote_member`) hold a `FileLock` on the
members file (distinct from the `events.jsonl` append lock).

Reads come in three shapes on `ChannelPort`:

- `peek` / `peek_with_ids(since)` — forward walk from a line-number
  cursor; pages entirely behind the cursor are skipped via their
  cached line counts without being opened.
- `peek_tail(limit, before)` — the canonical "newest N messages" chat
  read. Only the pages intersecting the returned range are read and
  only the returned `limit` lines are JSON-parsed. Returns
  `TailPage { events, has_more }`; page backward by feeding the
  page's oldest line id back as `before`. `has_more` tracks the raw
  log, so a terminal page can be empty when only
  `Created`/`MemberJoined` lines remain.
- `search(ChannelQuery)` — bounded backward filtered scan over
  `Posted` events (`text` case-insensitive substring, `author` exact,
  `before` anchor, `limit` cap). Walks pages newest→oldest and stops
  at `limit` matches or `SEARCH_MAX_SCAN_LINES` (100k) examined
  lines; `SearchPage.resume_before` + `has_more` continue the scan
  into older history. There is no index by design — the scan is the
  bounded minimum.

The `ChannelPeek` IPC carries `tail`/`before`/`query`/`author` on the
request and `event_ids` (line ids parallel to `events`) + `has_more` +
`resume_before` on the result — all `#[serde(default)]`, so older
peers ignore them. `PrincipalLog` carries the same `query`/`author`
filters for principal threads. The
`ChannelRead` tool returns `{channel, events, has_more, next_cursor}`
with a per-event `id` field, tail-anchored by default, `since` for
forward catch-up, `before` for backward paging, and `query`/`author`
for search.

---

## 6. Principal Snapshot Format (.peko)

A `.peko` file is a runtime-local tar.gz snapshot, inspectable with
`tar -tf snapshot.peko` and `tar -xOf snapshot.peko manifest.toml`.
The former `.agent` package and OCI manifest/layer format are retired (ADR-066 P5).

```text
manifest.toml
config/principal.toml
identity/did.json
identity/keys.enc              # plaintext KeyPairExport JSON
roles/                         # role prompts
sessions/                      # JSONL pages + routing/ownership indexes
cron/                          # authored schedule and run history
plans/                         # plan DAGs
tools/ skills/ mcp/ hooks/ kb/  # workspace files
```

The flat TOML manifest is packaging metadata, never behavior configuration:

```toml
format = "peko-snapshot-v1"
name = "researcher"
did = "did:peko:local:researcher:0123456789abcdef"
created_at = "2026-10-02T08:00:00Z"
peko_version = "0.1.0"
description = "Optional description"
# Optional; omitted when empty in older and current snapshots.
executable_files = ["skills/check/scripts/check.sh"]

[files]
"config/principal.toml" = "sha256:<64 lowercase hex digits>"
"identity/did.json" = "sha256:<64 lowercase hex digits>"
"identity/keys.enc" = "sha256:<64 lowercase hex digits>"
```

Every payload file must appear exactly once in `files`; `manifest.toml` itself is
excluded. Missing files, checksum mismatches, undeclared payload files, unsafe
paths, duplicate archive entries, and non-file entries fail before restore.
Directories in a tar are skipped. `--force` permits overwrites, never corruption.
The optional `executable_files` list contains only declared files under
`tools/`, `skills/`, `mcp/`, `hooks/`, or `kb/`. It is bound by the preview’s
manifest checksum and included in the executable inventory. On Unix these
files restore as `0755`; other workspace files restore as `0644`. Original
ownership and special mode bits are discarded. Missing metadata means no
executable files. Collection rejects links and special files; linked workspace
restore destinations are rejected before writes.
`PrincipalManifest::from_toml` rejects old `[principal]`/`[layers]` manifests with
“re-export from the source runtime” guidance. No layer tarballs, signatures,
TOFU pins, registry blobs, or embedded `.ext` archives are consumed.

Snapshot collection excludes `cache/`, `locks/`, and `memory_index.json`. Import
preserves DID and verbatim boot state when the package carries Local state;
cron jobs are rebound to the imported principal's effective id. A package with
no Local state resets boot inference; a keyless package directs the operator to
`peko create <name> -s <seed.toml>`. Seeds remain plain TOML, grounded with a new DID.

Before local CLI import, the preview prints DID, payload file count, every hook
id and complete `hook.toml` (binds, command, args), every MCP id and complete
server manifest (commands/args included), and skill ids. The optional request
`expected_manifest_checksum` matches the preview's `manifest_checksum` to prevent
changing the displayed manifest before import. Checksums then bind payload bytes.
`principal.snapshot_import` records the same `ExecutableInventory` as a Security
event with caller attribution, before writes. Definitions are retained verbatim,
including malformed ones; preview does not execute them. There is no approval UI.

---

## 7. Team Package Format (.team) (RETIRED)

> **RETIRED.** Teams were removed (ADR-039/041); there is no `.team` package format. Use `.principal` packages instead. The schema below is preserved only as historical reference.

A `.team` file was a gzip-compressed tar archive containing multiple agents plus team metadata.

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

## 8. Extension Package Format (.ext) (RETIRED)

Historical schema only. ADR-066 P5 removed embedded-extension import and the
remaining packaging model. Workspace skills/MCP/hooks travel as plain files in
the `.peko` snapshot; there is no extension archive install path.

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

## 9. Local Registry Store (RETIRED)

ADR-066 P5 deleted runtime registry storage, the remote client, OCI wire
manifests, and push/pull/search. Snapshots are local files (§6); PekoHub
distributes plain seed TOML outside the runtime. Peer tunnel transport remains.

---

## 10. Instance State

Runtime state for a running or stopped instance. Stored in SQLite.

**Location:** `.peko/run/state.db`

This is not a user-editable file. It is the daemon's working state — sessions and instance metadata. It is rebuilt from the filesystem if deleted. The JSONL session files are the source of truth; the SQLite file is a read-optimised index over them.

### 7.1 Tables

#### `instances`

| Column | Type | Description |
|--------|------|-------------|
| `id` | TEXT PK | Instance ID (`inst_...`) |
| `name` | TEXT | Human name |
| `image_ref` | TEXT | Image ref at creation time |
| `image_digest` | TEXT | Pinned image digest |
| `status` | TEXT | Current `InstanceStatus` |
| `team_id` | TEXT NULL | RETIRED — foreign key to `teams.id` (teams removed, ADR-039/041) |
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
| `parent_session_id` | TEXT NULL | Parent session id (set for spawned/branched sessions; rewritten by the `Session` tool's `move` action) |
| `trigger` | TEXT | How the session was started |
| `ended` | INTEGER | Boolean (0/1) |
| `title` | TEXT NULL | Auto-generated or user-set |

#### `teams` (RETIRED)

> **RETIRED.** Teams were removed (ADR-039/041); this table no longer exists. Preserved as historical reference.

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

## 12. Workspace MCP server definitions

**Location:** `<principal-workspace>/mcp/<server-id>/server.json` (ADR-075).
The directory ID is canonical. Registered methods use `mcp__<server>__<method>`;
unsafe, long or ambiguous names receive a stable digest suffix to fit model API
character/64-byte limits. Remote method names remain exact and are bound directly.
Native definitions use `schema_version: 1`;
unknown fields and invalid transport combinations fail explicitly.

```json
{
  "schema_version": 1,
  "transport": { "type": "stdio", "command": "python3", "args": ["server.py"] },
  "cwd": ".",
  "env": { "SERVICE_TOKEN": { "credential": "api_token" } },
  "startup": "auto",
  "startup_timeout_ms": 30000,
  "tool_timeout_ms": null,
  "health_check_interval_secs": 30,
  "max_restarts": 0,
  "reserved_parameters": {
    "principal_id": { "source": "runtime", "field": "principal_id" },
    "session_id": { "source": "runtime", "field": "session_id" }
  }
}
```

| Field | Contract/default |
|---|---|
| `transport` | `stdio` with `command`/`args`; `streamable_http` with `url`; compatibility `sse` with `endpoint` |
| `cwd` | Stdio working directory; relative to the manifest directory; defaults there |
| `env`, `headers` | String values bound as `{ "value": "literal" }` or `{ "credential": "local_slot" }`; env is stdio-only, headers HTTP-only |
| `auth` | Default `{ "type": "none" }`; HTTP bearer `{ "type": "bearer", "credential": "slot" }`; OAuth `{ "type": "oauth" }` with optional client_id/authorization_endpoint/token_endpoint/scopes/resource |
| `startup` | `auto` (default), `eager`, `lazy`; cold catalogs always discover; valid cached auto/lazy catalogs defer connection |
| `startup_timeout_ms` | 30000; discovery also has a bounded aggregate deadline |
| `tool_timeout_ms` | Optional; absent/null means no additional MCP method deadline |
| `health_check_interval_secs` | 30; zero disables periodic health checks |
| `max_restarts` | Zero has no attempt cap; positive values cap automatic retries after the initial attempt; reconnect resets it; failures back off 2–64 seconds |
| `reserved_parameters` | Runtime identity/context, static JSON values, or local credential slots; removed from model schemas and injected at execution |
| `sampling` | Opt-in `{ "model": "optional-model-id", "max_output_tokens": 4096 }`; otherwise uses current principal model and actual owner quota; legacy SSE sampling is unsupported |

Runtime fields are principal_id/principal_name/session_id/agent_id/workspace/run_id/tool_call_id.
Missing runtime values and caller overrides fail. Child processes receive the minimal
runtime process environment plus declared values, with no `${ENV}` substitution.

Credential slots expand only to `mcp:principal:<local-principal-id>:<server-id>`.
OAuth uses slot `oauth_token`, material access token and metadata refresh_token,
expires_at, client_id and endpoint/auth binding. Secrets remain in the encrypted
vault. Workspace snapshots contain definitions/slot names, never these vault entries.

Local metadata cache: `<principal-local-root>/mcp_cache/<server-id>.json`, version 1,
owner, configuration/credential revision digest and inventory (tools/resources/prompts,
server instructions, fetched_at). Reuse expires after 30 minutes and rejects a changed
owner or identity; files are bounded to 8 MiB. Cache is excluded from workspace snapshots.

Legacy unversioned JSON and single-entry `manifest.yaml` with `mcp_servers` normalize
to the same typed definition. Flat JSON/YAML files use their stem as ID. Duplicate
files/IDs, mismatched legacy names, multiple YAML servers, unknown fields, binary
bundles and unscoped raw-secret references require explicit migration. Daemon-global
MCP configuration and reload IPC no longer install servers; move definitions and
credential slots into the selected principal explicitly.

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
    "workspace":   "/home/user/project/.peko/principals/researcher/workspace"
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

## 13½. IPC — `ExecuteTool` Packet (ADR-061 phase 1)

The daemon's IPC protocol is one JSON datagram per message over a unix socket
(`$PEKO_HOME/run/daemon.sock` or `~/.peko/run/daemon.sock`, override
`PEKO_DAEMON_SOCK`), Windows named pipe, or loopback-UDP fallback; packets are
capped at `MAX_PACKET_SIZE = 60 000` bytes. `ExecuteTool` is the synchronous
tool-execution variant — the callback surface for agent-authored workflow
processes (`sdks/python/peko_workflow`), with server-side attribution
(the retired `async_spawn`/`async_cancel`/`async_receipt` variants were
removed 2026-09-27; the CLI never executed tools, ADR-021).

**Request** (`RequestPacket::ExecuteTool`):

```json
{
  "type":        "execute_tool",
  "request_id":  1,
  "tool_name":   "Glob",
  "params":      { "pattern": "*.py" },
  "session_key": "agent:researcher:cli:default",
  "workspace":   "/path/to/workspace",
  "run_token":   "optional — ADR-061 phase 2b"
}
```

The wire carries *where the call lands* (`session_key`), never *who the caller
claims to be* (ADR-057): the daemon parses the key and resolves the owning
principal server-side. ADR-066 P2 deleted the capability gate — there are no
grants to derive; an unknown key simply yields an unattributed call.

**`run_token`** (phase 2b, ADR-061 D6): when present, the token must validate
against the daemon's in-memory `RunTokenRegistry` (unknown or expired fails
closed) AND its recorded `session_key` / `principal_name` must match the
packet's `session_key` — a mismatch fails closed. Validation failure answers
with a transport-level `{"type": "error", ...}` (not a `success: false`
result). The token only *authenticates*: identity still derives from the
session key, never from the token. Tokens are minted by the `Workflow` tool
at spawn (32 random bytes, base64url, TTL = run timeout + 60 s) and injected
into the workflow process as `PEKO_RUN_TOKEN`; the registry is in-memory and
dies with the daemon. When `run_token` is absent, the pre-2b local-transport
trust applies unchanged.

**Caller-awareness** (in-memory record, *not* a wire change): each registry
entry also carries `caller_session_id` — the canonical session UUID of the
tree node the `Workflow` tool was invoked from (`None` for no-context
spawns). On a validated call the handler threads it into
`ToolContext.session_id` in place of the session-key string, so
tree-relative tools (`Agent`, the session layer's ownership guards) classify
the workflow caller as that node — spawns parent under it, ownership guards
see its ancestors. Tokens without a node (and tokenless calls) keep
threading the session-key string — the dangling, fail-closed behavior. A
node id whose session has since been deleted degrades to dangling at the
session layer, which owns that decision. The `Session` builtin itself is
caller-aware on this path (`CallerAwareSessionTool`, registered system-scope
at daemon start): it builds the per-call `SessionManagerRuntime` from the
threaded ctx — `status`/`history`/`list` and the ownership guards see the
calling node; tokenless or no-node calls degrade to dangling exactly as a
session with missing metadata does on the agent path.

**Response** (`ResponsePacket::ToolExecuted`) — the F37 funnel's
`tool_result_from_hook` triplet:

```json
{
  "type":       "tool_executed",
  "request_id": 1,
  "content":    "matches: [\"a.py\"]",
  "result":     { "matches": ["a.py"] },
  "success":    true,
  "truncated":  false
}
```

- `content` — the display string the agentic loop would feed the model.
- `result` — the structured value (`null` when truncated).
- `success` — `false` for tool errors and dispatcher validation failures
  (the fail-closed path surfaces as data, not a transport error; the denial
  text in `content` reads `Error: Tool '<name>' is currently disabled…`).
- `truncated` — when the serialized payload would exceed the datagram budget,
  `result` is dropped to `null` and `content` is clipped until the packet
  fits, suffixed with `[truncated by peko: result exceeded the IPC packet
  budget]`. (ADR-061's spill-to-workspace-file is a phase-2 refinement.)

A funnel-internal failure instead answers `{"type": "error", "request_id": N,
"message": "…"}`.

---

## 13⅝. IPC — `principal_send_stream` packets

A streaming send answers a sequence of packets, all carrying the request's
`request_id`, with `heartbeat` packets every two seconds in between:

| Packet | Fields | When |
|---|---|---|
| `principal_sent_iteration` | `iteration` (from 1) | Each agentic iteration starts. |
| `principal_sent_thinking` | `delta` | Reasoning text. **Owner only.** Deltas; a final full-text event is not repeated after them. |
| `principal_sent_tool_start` | `tool_id`, `name`, `input`? | A tool call starts. |
| `principal_sent_tool_end` | `tool_id`, `name`, `success`, `duration_ms`, `output`? | A tool call finishes. |
| `principal_sent_chunk` | `delta` | Assistant text. |
| `principal_sent_done` | `content` | The final answer (success only). |
| `run_summary` | `iterations`, `usage`, `tool_errors` | Once per run. |
| `error` | `message` | The run failed (`stopped by user` after `principal_stop`). |
| `done` | `success`, `error` | Last. `success: false` with `error: "[queued] …"` means the thread was busy and the message was queued; its reply is posted to the DM channel. |

`input` and `output` are present only when the caller is the peko's owner:
a short summary of the call's arguments and an excerpt of its result, both
redacted by the tool ledger's redactor (ADR-079 D3: known runtime secrets,
sensitive keys, credential shapes) and capped at 1 KiB. Any other caller
sees tool names and status only, and no thinking — a peko's results and
reasoning can quote its owner's private context. `tool_errors` messages are
redacted the same way.

```json
{"type": "principal_sent_tool_start", "request_id": 7, "tool_id": "call_1", "name": "Bash", "input": "{\"command\":\"date\"}"}
{"type": "principal_sent_tool_end", "request_id": 7, "tool_id": "call_1", "name": "Bash", "success": true, "duration_ms": 12, "output": "Tue Oct 27"}
```

peko-desktop parses these packets; their exact shapes are pinned by
`peko-rs/core/src/ipc/fixtures/desktop_stream_responses.json`.

---

## 13¾. `ModelCall` — one-shot inference + the judgment wire (ADR-061 phase 2a)

`ModelCall` is a built-in tool (gated by `tool:ModelCall`; the starter bundle's
`tool:*` covers it) providing **one-shot inference: no session persistence, no
tool-calling loop, no streaming**. Every call is attributed server-side to the
calling principal (from `ToolContext.principal_name`, never from the params),
refuses to run without one, and charges that principal's quota meter from
provider-reported usage after the call.

**Mode selection** — exactly one per call:

- **Completion** — `prompt` present. One non-streaming chat completion with
  `tools: None`.
- **Judgment** — `state` + `questions` present. POSTs to the resolved entry's
  decision API (below). `system` / `max_tokens` / `temperature` are rejected in
  this mode.

```json
{
  "model":       "string? (catalog id; defaults to the principal's preferred_model_id)",
  "prompt":      "string?  (completion mode)",
  "system":      "string?  (completion mode)",
  "max_tokens":  "integer? (completion mode)",
  "temperature": "number?  (completion mode)",
  "state":       "string | object?  (judgment mode)",
  "questions":   "object?  (judgment mode — map of question key → spec)"
}
```

Completion result:

```json
{ "mode": "completion", "text": "…", "model": "<catalog id>",
  "usage": { "input": 12, "output": 40, "total": 52 } }
```

### `ModelSpec.decisions` (models.toml)

Judgment mode is gated on a catalog opt-in flag so an ordinary chat entry can
never be mistaken for a decision API:

```toml
# ~/.peko/models.toml
[entries.jev]
display_name = "Jev"
api_format   = "openai_completions"      # inert for judgment mode
base_url     = "https://api.example.com" # POSTs go to {base_url}/v1/evaluate
model_id     = "jev-1"
requires_key = true
credential_id = "jev"                    # vault credential, as for chat models
enabled      = true

[entries.jev.spec]
decisions = true                         # ← the flag; serde-default false
pricing = { input_per_million = 0.042 }  # judgment APIs bill input only
```

Like the other `ModelSpec` fields, `decisions` is **hand-edit-only** — no
`peko model add` / `peko model edit` flag sets it (`edit` covers `--note`
only). It surfaces read-only via `peko model show` (`spec.decisions`),
`--json`, and the `ModelList` tool. Old `models.toml` files load unchanged
(`#[serde(default)]` ⇒ `false`).

### Judgment wire shape

Request — `POST {base_url}/v1/evaluate` with `Authorization: Bearer <credential>`
(the vault credential resolved through the same chain as chat providers; the
header is omitted when the entry has `requires_key = false`). `questions` is
passed through verbatim:

```json
{
  "model": "jev-1",
  "state": "deploy failed twice",
  "questions": {
    "continueWorking": { "type": "boolean", "instructions": "Is there pending work?" },
    "priority":        { "type": "choice", "options": ["high", "low"], "instructions": "…" },
    "severity":        { "type": "score", "min": 1, "max": 5, "instructions": "…" }
  }
}
```

Response — the runtime requires a top-level `answers` object and passes each
per-question answer through **verbatim** (`probability` rides along when
present); the choice/score response fields are intentionally not pinned.
An optional `usage` block (`input_tokens` / `prompt_tokens` / `input`,
`output_tokens` / `completion_tokens` / `output`) is metered when present:

```json
{
  "answers": {
    "continueWorking": { "answer": true,  "probability": 0.92 },
    "priority":        { "answer": "high", "probability": 0.70 },
    "severity":        { "answer": 4,      "probability": 0.61 }
  },
  "usage": { "input_tokens": 1200 }
}
```

Judgment result returned to the caller:

```json
{ "mode": "judgment", "model": "jev",
  "answers": { "continueWorking": { "answer": true, "probability": 0.92 } },
  "usage": { "input": 1200, "output": 0, "total": 1200 } }
```

(`usage` is omitted from the result when the response carried none; in that
case only the meter's request counter advances — token counts are never
synthesized from request params.)

---

## 13⅞. `Workflow` runner + the `workflows/` directory (ADR-061 phase 2b)

Workflows are **agent-authored Python files at `<workspace>/workflows/*.py`**
(D1) — procedural memory following ADR-050's presence-equals-visibility rule:
a dropped file appears in the per-turn `workflows` prompt catalog (the tail
`<runtime-context>` message) on the next iteration — one
`- {name}: {first docstring line} (workflows/{name}.py)` line per file,
sorted, mtime-keyed cache, 8 KB cap with a list-the-directory pointer on
overflow. No CLI tree, no restart.

The `Workflow` built-in (gated by `tool:Workflow`) runs one as a subprocess:

```json
{ "path": "triage.py", "args": ["--fast"], "timeout_ms": 300000 }
```

- `path` is relative to `<workspace>/workflows/`; it is canonicalized and
  anything escaping that directory (absolute paths, `..`, symlink escapes)
  is refused, as are non-`.py` targets and missing files. The interpreter is
  `python3` on `PATH`.
- `timeout_ms` defaults to 300 000 and is capped at 3 600 000; on timeout or
  abort the child process is killed.
- stdout/stderr are captured as bounded TAILs (16 KiB each); a truncated
  stream is prepended with `[… truncated by peko: showing the last 16 KiB …]`
  and carries `stdout_truncated` / `stderr_truncated: true`.

Result:

```json
{ "workflow": "triage.py", "success": true, "timed_out": false,
  "exit_code": 0, "duration_ms": 812,
  "stdout": "…", "stderr": "",
  "stdout_truncated": false, "stderr_truncated": false }
```

(`exit_code` is `null` when the process was killed by signal/timeout;
`timed_out: true` marks the timeout path, which returns partial output as
data rather than an error.)

**Spawn-time environment** (D6): the child gets a *minimal* env — never the
daemon's (secrets must not leak). Only `PATH` / `HOME` / locale / temp-dir /
Windows platform vars pass through, plus the identity set:

| Variable | Value |
|---|---|
| `PEKO_DAEMON_SOCK` | the daemon's unix socket (`$PEKO_HOME/run/daemon.sock`) |
| `PEKO_WORKSPACE` | the calling principal's workspace path |
| `PEKO_PRINCIPAL_ID` | the calling principal's stable id |
| `PEKO_SESSION_KEY` | the calling session's attribution key (below) |
| `PEKO_RUN_TOKEN` | freshly minted run token (see §13½ `run_token`) |
| `PEKO_WORKFLOW_DEPTH` | nesting depth of the spawned process (agent-spawned = 1) |

`PEKO_SESSION_KEY` is constructed server-side so that
`parse_session_key(key).agent` is the calling principal's name — the segment
`ExecuteTool` attribution resolves from. The agent-loop path (where the tool
sees the session UUID) yields `agent:{principal}:workflow:{session_uuid}`;
a nested workflow→`ExecuteTool` path reuses the parent's key verbatim.

**Recursion guard** (D8): a call at `PEKO_WORKFLOW_DEPTH >= 2` is refused
("a workflow may not spawn itself"). On the `ExecuteTool` path the depth is
server-derived — the handler strips any client-supplied `_workflow_depth`
param and stamps the validated run token's recorded depth instead — so the
guard cannot be spoofed from the wire.

---

## 14. Extension Manifest (RETIRED)

The extension manifest and id model was retired by ADR-066. Roles and skills
are ordinary Markdown files; MCP servers use their workspace manifests; hooks
use `hook.toml` with the six observer points documented above. `.peko` is a
flat local snapshot, described in §6.

---

## 15. Type Reference

Quick-reference table of all primitive types used across formats.

| Type name | Format | Example |
|-----------|--------|---------|
| `Timestamp` | ISO 8601 UTC, ms precision | `2026-03-16T08:00:00.000Z` |
| `InstanceId` | `inst_` + 8 base36 chars | `inst_7k2mxp3q` |
| `SessionId` | `sess_` + 8 base36 chars | `sess_9nrwbf1v` |
| `TeamId` | `team_` + 8 base36 chars (RETIRED — ADR-039/041) | `team_4hsdcl8e` |
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
| `ExtensionType` | Enum | `tool \| skill \| mcp \| session` |
| `BusBackend` | Enum | `in-memory \| redis \| nats` |

---

## 16. Changelog

| Version | Date | Changes |
|---------|------|---------|
| 0.1.0 | 2026-09-11 | ADR-052 D6: the workspace hook manifest (`<workspace>/hooks/<id>/hook.toml`, layout in `docs/architecture/PRINCIPAL_WORKSPACE.md`) gains a `PromptSection` bind point — `binds = [{ point = "PromptSection", section = "<name>", priority = <int?> }]` (new optional `section` / `priority` bind fields; `priority` defaults to 100). The hook command's stdout renders as a `## <name>` section of the per-iteration `<runtime-context>` tail message with the standard change-detection semantics (first injection plain, update notice on edit, one-shot retraction when output stops). Binding a built-in name (`identity` / `agents` / `skills`) augments the built-in catalog via registry aggregation instead of double-dispatching. |
| 0.1.0 | 2026-08-20 | Sprint 6: opaque UUID session ids + peer via parent walk (§5, §5.9, §5.10). `peko_session::SessionId` newtype around `Uuid`; engine-internal `SessionMetadata.session_id` and `parent_session_id` are `SessionId`; the legacy `root:self` magic-string trunk is retired (anchor = `find_trunk_session(metas)`, the session with `parent_session_id = None`); peer routing walks the parent chain via `resolve_peer_via_parent_walk`, reading stamped `peer_type`/`peer_id`; `peer_from_session_key`, `parse_session_key_v2`, `parse_session_key` v1, `base_key_from_overlay`, `trunk_session_id`, `looks_like_session_id`, and `resolve_id_or_path` deleted. Path resolver grammar collapsed from three forms (sprint 5) to two: slug path or caller self-reference. |
| 0.1.0 | 2026-08-19 | Phase 13 (sprint 3): §5½ Chat Log RETIRED — the `peko-chat-log` crate left the workspace; the cron `Send` fired-prompt projection (`record_cron_input`) dropped (fired prompts live in the trunk session JSONL); `PathResolver::chat_logs_dir` removed. The `peko log` row DTO moved to `peko_core::ipc::packet::PrincipalLogMessage` (wire shape unchanged); the peer DM channels (§5¾) are the consumer-visible conversation record. |
| 0.1.0 | 2026-08-17 | Phase 7 ingress re-route (§5.9/§5.10): all external peer ingress lands in per-peer standing children of the trunk; per-peer root sessions (`root:{peer}`, `root:cron:{peer}`) retired (routing deleted); cron `Send` default target is the trunk (`target = "trunk"` accepted, same route); cron notes target the owner's peer child + `[notify]` self-view in the trunk; engine-managed guards + prune exemption narrowed to exactly `root:self`. |
| 0.1.0 | 2026-08-15 | Principal trunk session (§5.9): constant `root:self` self session; `ChannelKind::Trunk`; `CronJobAction::Send.target` (`"trunk"` or absent, validated on load); trunk turns skip chat-log projection and the cron note cross-post. |
| 0.1.0 | 2026-08-13 | Round 7 (§5.8): chapter concept deleted — session ids are stable for life; `chapters.json` removed; oversized JSONLs page in place to `<id>.N.jsonl` with transparent read-path stitching; legacy `#`-suffixed files inert on disk. |
| 0.1.0 | 2026-08-09 | Agent-owned session management (§5.8): `archived`/`compact_requested` on session entries, `PeerInfo.active_session_id` → `Option<String>` with session-id scrubbing on delete, chapter id rotation format (`root:{peer}#<ts>`), `chapters.json` pending-change sidecar, transcript search scope. |
| 0.1.0 | 2026-07-20 | Chat-session separation: added §5½ Chat Log (runtime-owned, append-only, consumer-visible conversation history) distinct from §5 Session JSONL (mutable principal-owned working memory). Sharded by `(principal_did, peer)` with BLAKE3-hashed paths, opaque thread-bound cursors, sender-participant validation, durable append under `FileLock`. Deletion tied to `PrincipalManager::remove`. |
| 0.1.0 | 2026-04-26 | Initial draft. ADR-022: Session compaction — added `compaction` and `model_change` system events (§5.3), context cache file format (§5.5), compaction semantics including dual-threshold triggers, turn boundaries, split-turn handling, and structured summary format (§5.6) |
| 0.1.0 | 2026-05-08 | Packaging restructure (Phases 1–7): `src/image/` merged into `src/portable/`. Clean manifest — `AgentManifest` stripped of `capabilities`, `tools`, `mcp`, `tool_sources`, `memory`. Added `layers` section with content-addressable digests. Added `.agent` build from directory (`AgentBuilder`). Added registry push/pull with mock server. Added team checksum validation and `team.toml` preservation. Added `.ext` export for extensions. `agent.toml` is the single source of truth for agent behaviour. |
| 0.1.0 | 2026-05-11 | Issue 023: Team registry push/pull now decomposes teams into content-addressable layers (`TeamConfig` + per-agent `Config`/`Identity`/`Skills`/etc.) instead of a single opaque blob. Added `TeamAgentIndex` and `AgentLayerRef` types. Added `TeamLayerBuilder` and `TeamLayerReconstructor`. Cross-team agent deduplication works automatically via existing `RegistryClient::check_existing_layers()`. Pull reconstructs agents directly from layers without temporary `.team` files. Documented in §7.4. |

## Tool binding lifetimes (ADR-069)

Tool catalogs are in-memory maps keyed by `(tool_name, PrincipalId)`. The daemon
catalog stores runtime defaults and principal services. A run's private overlay
stores its Agent/Session executor adapters and inherits live parent registrations.
ModelList is a principal service shared by every run. Lookup searches principal
entries across the
layers before system defaults. Wire definitions are sorted and deduplicated;
tool names are exact.

Workflow/IPC callbacks select live run bindings using server-attributed
`(PrincipalId, caller_session_id)` keys and weak runtime references. Expired
bindings fall back to daemon caller-aware adapters. Principal async executors
retain their task registries across turns; each task still records its caller
session for completion routing. These are runtime lifetime changes, with no new
persisted session, task, cron, or workflow fields.

---

*Version: 0.1.0 · Last Updated: 2026-04-26 · Status: Draft*

## ADR-073 dual identities and conversations

`runtime/actors.json` contains actor records with `info` (`address`, `local_id`,
`hub_id`, `kind`, `registration`, `name`, `mention`), an immutable `identity_key`,
and optional private `signing_did`. Private `historical_aliases` preserve address
resolution for past authors and memberships after promotion/unlinking; they are
not accepted by public `resolve_actor` routing. The registry is a routing index:
which principals learned which contacts lives in each principal's contact book
(below), not here. The identity key and signing DID are omitted
from tool profiles. Under ADR-077, `local_id` and `hub_id` are optional, with at
least one present. Local-only actors have a local ID, registered local principals
have both, and remote principals/accounts/visitors have only a Hub ID (`local_id:
null`). Canonical addresses prefer `kind:local:<id>` when a local ID exists,
otherwise `kind:hub:<UUID>`. Remote actors acquire no local proxy alias.
Registry writes use a file lock and atomic replacement. Aliases and immutable
keys cannot belong to distinct actors. Startup reconciles legacy proxies against
the complete hosted-principal inventory, preserving real hosted IDs and recording
retired proxy aliases privately. Failed principal loads skip reconciliation. Local
and Hub scopes remain distinct in authorization checks; every check compares the
registry's immutable keys (ADR-081 D1).

`principal.toml` stores `id`, `actor_key`, signing `did`, `owner`, and `exposure`
(`local_only`, `private`, `public`). It holds no Hub ID: the registry is the only
source of a principal's Hub link, written only by its registration receipt. New
principals are local-only until an explicit registration receipt. Signing keys
are vault-backed.

`runtime/pekohub.toml` stores only the Hub tunnel `url` and optional `tls`; the
runtime is known to Hub by its identity DID (`runtime/identity.toml`). There is no
terminal login file: the terminal is the local user, whose registry record carries
the fixed key `local@<runtime DID>` and, once the runtime is registered, the account
as its Hub alias.

Conversation `SessionEntry.channel_id` is the sole binding. Binding clears
`peer_type`/`peer_id`, and index backfill does not restore actor stamps onto a
channel-bound entry. Per-principal session paths live in channel `meta.json.bindings`;
DM `participants` identify its fixed pair, while group rosters use current membership.
DM IDs hash `peko-dm-v1\0` plus sorted immutable actor keys, each length-prefixed by
its UTF-8 byte length as a big-endian u64. Human peers have membership rows.

Registration announcements add `{localId, identityKey, registrationId}` to the
principal signing proof and deployment metadata. The Hub replies
`principal_registered {registrationId, localId, hubId, signingDid}` or
`principal_registration_rejected {registrationId, message}`. Hub actor UUIDs are
stable across deployment instances; routing instance UUIDs remain separate.
Invites add `hubId`, `identityKey`, and `name` to each member snapshot. Signed posts
use Hub actor addresses on the wire; reads use local addresses for hosted
actors and Hub addresses for remote actors.

Directory responses contain `{name, hubId, localId, identityKey, principalDid,
runtimeId, instanceId, ownerSubject, exposure}`. `principalDid` is a signing credential.
Registered callers use `Authorization: PekoPrincipal <JWS>` with EdDSA claims
`{sub:<signing DID>, aud:"peko-directory-v1", path, iat, exp}`; the proof expires
within 60 seconds and is bound to the exact request path. Routing never grants
private chat permission. These contracts replace the pre-launch identity format.

ADR-073 snapshots use the same vault-backed `did:key` signing credential as live
principals. Import writes the seed to the `principal-identity` vault namespace,
clears the copied Hub ID and exposure projection, and preserves the immutable actor
key unless keys are rotated. An already hosted actor retains its destination
local ID. A remote contact gains a fresh local ID on import, retaining its
immutable actor key, learned relationships, and verified Hub link when installed
by the principal manager. DM pair IDs do not change on promotion.


## ADR-081 portable identity

Local routes name the runtime that issued them. Outside that runtime they are
written `kind:local:<id>@<runtime DID>` and match no local actor; on it they read
as `kind:local:<id>`.

`<data>/principals/<name>/local/contacts.json` — the principal's contact book:
`[{kind, local?: {runtime, id}, hub_id?, identity_key, signing_did?, name?,
registration}]`, keyed by `identity_key`, written under a file lock with atomic
replacement. A contact is reachable when this runtime's registry routes one of its
routes to the same key; otherwise Contacts lists it with `reachable: false`.

`local/channels.json` (written into snapshots, restored on import) —
`[{channel, participants: [{address, hub_id?, kind, identity_key}], binding}]`.

Snapshots (`peko-snapshot-v1`) add `manifest.source_runtime`, `POLICY.md`,
`workflows/`, `plugins/`, `local/contacts.json`, and `local/channels.json`. The
owner, grant subjects and granters, `POLICY.md` `## <actor>` headings, and cron
`created_by.actor` are written by Hub route when the actor has one, else as a
source-qualified local route. Import localizes routes of the destination runtime,
replaces a source-local owner with `user:local` everywhere, rebinds that owner's DM
session to the new pair ID, reuses a former local ID by actor key (never the
snapshot's), and refuses a key already hosted here unless keys are rotated.
A rotated import (`peko import --rotate-keys`) is a new actor: new signing key and
DID, actor key, and local ID, no Hub registration, `LocalOnly` exposure. It keeps
the owner, grants, contacts, and sessions, rebinds the owner's DM session to the
new key's pair ID, and does not restore `local/channels.json`, whose rosters name
the old key.

Cron `created_by` trust is re-derived from the creator's current trust when a job
fires and only lowered.
