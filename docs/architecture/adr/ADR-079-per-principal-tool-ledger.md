# ADR-079: A Per-Principal Tool Ledger

**Status:** Accepted (implemented on `feature/adr-079-tool-ledger`)
**Date:** 2026-10-14
**Amends:**
- [ADR-046](ADR-046-trust-and-audit.md) §6–7: implements follow-up 2 (tamper-evident hash chain), for the ledger.
- [ADR-078](ADR-078-acting-for-stamps-and-trust-floor.md) D6 and D7: adds a runtime-only path to the floor; the reviewer's exempt set gains `ToolLog` and loses `Sleep`, which names no production tool.

**Builds on:**
- [ADR-061](ADR-061-agent-authored-workflows.md): workflow callbacks re-enter through `ExecuteTool` with a run token.
- [ADR-063](ADR-063-async-task-delivery-consolidation.md): background tasks and completion delivery.
- [ADR-066](ADR-066-pure-workspace-tooling.md) P3–P6: one dispatcher, one attributed `tool.call` audit per call.
- [ADR-078](ADR-078-acting-for-stamps-and-trust-floor.md): `ActingFor` stamps and compartments.

---

## 1. Context

Peko does not sandbox. Owner and trusted runs keep `Bash` (ADR-078 §2), and workflows are ordinary OS subprocesses. The runtime cannot prevent a run from doing harm, so the remaining controls are the trust floor and **being able to find out afterwards what was done, on whose behalf, and with what result.**

The global audit log already records that every call happened. It cannot answer *what* happened.

Verified on `master` @ `b5d24178`:

| # | Fact | Where |
|---|---|---|
| F1 | Every tool call goes through one place, `ToolDispatcher::execute`. This includes agent loops, subagents, MCP, Workflow callbacks (via IPC `ExecuteTool`), and background task bodies. The dispatcher emits a single `tool.call` audit **after** the call returns. | `core/src/tools/dispatcher.rs:231-251` |
| F2 | `tool.call` records the tool, a declared `action`, agent, principal, session id, the `acting_for` stamp, `success`, and `duration_ms`. For params it records **only a SHA-256 digest**, and it records no result at all. It omits `run_id` and `tool_call_id`, even though `ToolCallSpec` carries both. | `dispatcher.rs:656-719`, `engine/src/tooling.rs:22-23` |
| F3 | There is one log file per UTC day for the whole runtime: `{data_dir}/runtime/audit/audit-YYYY-MM-DD.jsonl`. Files are 0600, each line is fsync'd, and nothing is ever pruned. The only readers are `peko audit tail` / `peko audit list`, which filter by principal name. No tool reads it. | `common/paths.rs:463`, `observability/src/audit.rs`, `cli/src/commands/audit.rs` |
| F4 | Audit write failures are logged at debug level and dropped. The engine-side sink spawns its writes and never awaits them. | `dispatcher.rs:715-717`, `core/src/observability/mod.rs:73` |
| F5 | A foreground call that outlives the router's wait is detached. Its body finishes inside the router's closure, so the `tool.call` row records the **receipt** as a success and the real outcome is never audited. | `extensions/framework/transport/async_router.rs:207-260` |
| F6 | Background bodies are dispatched a second time with `run_id` and `tool_call_id` set to `None`. The result is two rows with no link between them. Task files, the only other record of async results, are deleted after 24 h. | `async_exec/executor/executor.rs:830-845`, `daemon/mod.rs:470` |
| F7 | Workflow callbacks carry the caller's session (`RunTokenEntry.caller_session_id`) but not the `Workflow` call that started them. No transcript records them, so a params digest is the only trace. | `ipc/run_tokens.rs:31-51`, `ipc/handlers/tool.rs:303` |
| F8 | For agent-loop calls, transcripts hold params and results as `message.v2` content blocks. The dedicated `tool.call` / `tool.result` session events are never written in production. A principal can delete or move its transcripts (`Session` delete/move), and `page_limit` retention prunes them. | `session/src/events.rs:405-408`, `session/src/unified.rs:825` |
| F9 | The audit log has no hash chain. Any run with `Bash` writes as the same OS user and can rewrite it, and the rewrite cannot be detected. | ADR-046 §6, `tools/builtin/bash.rs:5` |

Taken together: an owner cannot answer "what did my principal run in `/user-x` last Tuesday, and what came back?" The principal cannot answer it either, about itself.

---

## 2. Decision

### D1: Each principal gets an append-only tool ledger

`{data_dir}/principals/{name}/local/ledger/ledger-YYYY-MM-DD.jsonl` (`LocalLayout.ledger_dir`) holds one file per UTC day. Files are mode 0600, and each line is fsync'd.

- **Only the dispatcher writes it.** It writes every call, under one `Ledger` per principal, bound with the principal's services (`install_principal_services`, beside its sessions).
- **Every tool is recorded**, including read-only ones. Filtering happens when the ledger is read (D6), not when it is written.
- **The global audit log keeps its current role.** It stays an operator trail across all principals, with params as a digest only. Its `tool.call` event gains `call_id`, `run_id`, and `tool_call_id`, so it can be joined to the ledger. The ledger records the same `params_digest`, computed over the unredacted params, so either log can check the other.

### D2: Two records per call, sharing a `call_id`

```jsonc
// start — written and fsync'd BEFORE the call executes (write-ahead)
{ "v": 1, "seq": 812, "prev": "sha256:…", "hash": "sha256:…",
  "kind": "start", "ts": "2026-10-14T09:12:03.114Z",
  "call_id": "call_0192f…",                    // minted by the dispatcher (UUIDv7)
  "run_id": "…", "tool_call_id": "toolu_…",    // the engine's identifiers, when present
  "parent": { "kind": "task", "id": "Bash:…", "call_id": "call_0192e…" },   // D4
  "principal_id": "…", "agent_id": "did:peko:…",
  "session_id": "…",                            // a path is resolved when read (D6)
  "acting_for": { "actor": "user:hub:…", "trust": "untrusted",
                  "compartment": { "conversation": { "channel": "chan_…" } },
                  "origin": { "ingress": { "channel": "chan_…" } } },
  "tool": "Bash", "action": null, "source": "builtin",          // or "mcp:<server>"
  "params": { "command": "curl -H 'Authorization: [redacted]' …" },  // D3
  "params_digest": "sha256:…" }

// end
{ "v": 1, "seq": 815, "prev": "…", "hash": "…",
  "kind": "end", "ts": "…", "call_id": "call_0192f…",
  "outcome": "ok",          // ok | error | cancelled | floor_denied | policy_denied
                            // | awaiting_owner | proposed | detached
  "duration_ms": 1840,
  "exit_code": 0,           // Bash only
  "task_id": null,          // set when outcome = detached (F5)
  "result": { "excerpt": "…", "bytes": 48211, "truncated": true } }
```

- **The start record is written before the call runs**, ahead of the floor and the policy gate. A call that hangs, kills the daemon, or is refused still leaves a record. Refusals end with `floor_denied`, `policy_denied`, or `awaiting_owner`.
- **A detached call** (F5) ends with `outcome: "detached"` and the receipt's `task_id`. Its real outcome is recorded when the body finishes, as an extra `end` record under the same `call_id`, marked `"late": true`.
- **`error` includes tool-reported failures.** A result with `"success": false` (a non-zero Bash exit) is an `error` in the ledger, though the call itself succeeded for the agent.
- **A protected write filed as a proposal** (ADR-078 D6) ends with `proposed`.
- **`call_id` is always minted by the dispatcher.** Model-issued `tool_call_id`s are not unique across sessions, so they are kept only as metadata.

### D3: Redaction and size caps

The ledger stores params and a result excerpt in plaintext. Before writing, both go through one redactor:

1. **Values the runtime knows are secret** are replaced with `[secret:<label>]`. These are every vault credential's material (provider keys, registry tokens, identity keys; `Vault::secret_materials`) and live workflow run tokens, of 8 or more characters, matched exactly. This is the strongest rule, because the runtime knows the actual values. Known secrets are replaced before the patterns run, so a pattern never re-redacts a marker.
2. **Values under sensitive keys** are replaced with `[redacted]`. A key is sensitive if, case-insensitively, it matches `*key`, `*token`, `*secret`, `password`, `authorization`, or `cookie`.
3. **Common credential shapes** in free text are replaced: `Authorization:` / `Bearer` headers, `sk-…`, `ghp_…`, `xox[bp]-…`, and PEM private-key blocks.

Caps:
- Params: string leaves are cut to 2 KiB and the whole record to 8 KiB.
- Result: a 4 KiB excerpt — for process output (`stdout` / `stderr`) the **tail** of each stream (2 KiB each, matching their own truncation), otherwise the head — plus the full byte length.

Storing params at all is a deliberate change from the global log's digest-only rule (F2). The transcript already holds the same params in plaintext, in the same tier, at the same mode. So the ledger adds very little exposure, but it closes the gaps where no transcript exists (F6, F7) and where transcripts can be changed (F8). Redaction is best-effort. The ledger is sensitive data and is treated that way (D7).

### D4: Every call names its parent

| Call | `parent` | Carried by |
|---|---|---|
| Agent-loop call | none (the session and `run_id` place it) | — |
| Background body | `{ kind: "task", id: <task_id>, call_id: <spawning call> }` | the executor puts the spawning call's id on the `ToolCallSpec`, which today sets it to `None` (F6) |
| Body of a detached call | `{ kind: "task", … }`; recorded as the parent call's late `end` | the router passes `call_id` into its closure |
| Workflow callback | `{ kind: "workflow", call_id: <Workflow call> }` | `RunTokenEntry.parent_call_id`, set at spawn |
| Approved replay | `{ kind: "approval", id: <request_id>, call_id: <original call> }` | the approval record stores the original `call_id` |
| Subagent call | none; the child session's path places it under the `Agent` call's session | — |

`ToolCallSpec` gains `parent: Option<CallParent>`. This is a new field on a struct that is often built by literal, so every literal site is updated in the same PR.

### D5: Tampering is detectable, not preventable

- **Hash chain.** Each line's `hash` is SHA-256 over its canonical bytes with `hash` removed, and `prev` is the previous line's `hash`. The chain continues across days, so the first line of a day links to the last line of the day before. A principal's first line is a `genesis` record. After an import (D7) the genesis records the snapshot.
- **Checkpoints.** After a call when an hour has passed, on the hourly daemon janitor tick, and at shutdown, the dispatcher writes a `ledger.checkpoint` event (`principal_id`, `seq`, head `hash`) to the **global** audit log when the head has moved. A rewrite then has to forge two files consistently.
- **Verification.** `peko audit verify <peko>` walks the chain, checks it against the checkpoints, and checks each record's `params_digest` and `call_id` against the global `tool.call` rows. It reports the first broken link, missing ranges, and calls that appear in one log but not the other.
- **Fail-closed writes.** If the ledger cannot be written — or could not be opened when the principal was bound — owner runs proceed and a `ledger.write_failed` Security event goes to the global log. An owner must not be locked out of the principal they need to repair. Every other run's call is refused with `tool ledger unavailable`.
- **The floor protects the ledger directory.** `Write` and `Edit` refuse `local/ledger/` for every run, whatever its trust. This is the same rule as for `kb/inbox/`. `Read`, `Glob`, and `Grep` keep their ADR-078 reach.

**This does not stop the same OS user.** A run with `Bash` can rewrite the ledger, the global log, and the checkpoints together. What actually anchors the chain is a copy outside that user's reach. That is follow-up F-1: posting checkpoints to PekoHub, signed with the principal's DID key, when the tunnel is registered.

### D6: Reading the ledger

**For the principal: a new builtin, `ToolLog`** (read-only, ADR-071 actions):

| Action | Params | Returns |
|---|---|---|
| `search` | `tool`, `tool_action` (the logged call's action; `action` is `ToolLog`'s own discriminator), `session` (slug path or own session id) + `subtree`, `actor`, `outcome` (or `in_flight`), `since` (default 7 days) / `until`, `limit` (default 50, max 200), `cursor` | one compact row per call: start time, tool, action, session path, actor, trust, outcome, duration, exit code, parent. No excerpts. |
| `get` | `call_id` | the full start and end records, plus the call's parent and children |

- **Session paths resolve when read.** Records store the session id; the reader maps it to the current slug path, so `Session move` never leaves a stale path. `session` filters resolve from the calling session's tree.
- **Scope follows ADR-078 D4.** Owner runs see the whole ledger. Any other run sees only calls whose stamp is in its own compartment, and everything else reads as missing. An unattributed run sees nothing.
- **The reviewer skips it.** `ToolLog` joins the ADR-078 D7 exempt set, like `Session` and `ChannelRead`, because a non-owner run only ever reads its own compartment.
- **Ledger text is untrusted.** Excerpts hold tool output: web pages, peer text, command output. `ToolLog` returns them as data, with `<acting-for>` / `<runtime-context>` tags defused by `neutralize_runtime_tags`.
- **The intended use is self-review:** for example a cron-driven "summarize yesterday's Bash calls and anything that failed", or a principal checking what a subagent or workflow actually did.

**For humans: `peko audit ledger <peko>`**, with `--since`, `--tool`, `--session`, `--actor`, `--outcome`, `--call`, `--json`, and `--follow`. Like `peko audit tail`, it reads the files directly and works with the daemon stopped.

### D7: Retention and portability

- **Retention.** The default is 90 days, set by `[governance] ledger_retention_days` in `principal.toml` (it replaces the never-read `[governance] audit` level). The hourly daemon janitor prunes days older than that. Each prune writes a `ledger.pruned` event (last pruned `seq` and `hash`) to the global log, and verification treats that as the chain's new starting point.
- **Size.** Caps keep each line under about 16 KiB.
- **Snapshots exclude it.** A `.peko` export packs only the listed Local-tier directories (`sessions/`, `cron/`, `plans/`), so `ledger/` stays out without any change. The ledger is this runtime's record of the principal, not part of the principal. An imported principal starts a new chain at its first bind.

### D8: Where it lives

- **`peko-observability`**: a generic `ChainedJsonlSink` (daily files, hash chain, fsync, `verify`). It shares rotation code with `JsonlSink` and gains no new workspace dependency.
- **`core/src/security/ledger/`**:
  - `mod.rs`: `Ledger`, the record shapes, outcomes, excerpts
  - `redact.rs`: the redactor and the `SecretSource` the daemon binds
  - `read.rs`: grouping, compartment scoping, filters, session paths, and verification
- **The dispatcher**: start before `execute_inner`, end after it (including refusals and detaches), and the checkpoint tick.
- **`core/src/tools/builtin/tool_log.rs`**: the `ToolLog` tool.
- **`cli/src/commands/audit.rs`** + **`audit/ledger.rs`**: `ledger` and `verify`.
- **Doc updates**: `DATA_MODEL.md` (record shape, layout), `API_SURFACE.md` (`ToolCallSpec.parent`), `builtin-tools.md` (`ToolLog`).

---

## 3. Phasing

1. **Linkage.** Mint `call_id`, add `ToolCallSpec.parent`, wire D4, and add `call_id` / `run_id` / `tool_call_id` to the global `tool.call` event. This fixes F5–F7 for the global log as well.
2. **Ledger write path.** Records, redaction, write-ahead start, fail-closed writes, the floor rule for `local/ledger/`, and retention.
3. **Readers.** `ToolLog` and `peko audit ledger`.
4. **Integrity.** Hash chain, checkpoints, and `peko audit verify`.

Each phase can merge on its own. Phase 1 is worth having even if the rest is deferred.

---

## 4. Alternatives considered

- **Put full params and results in the global log.** This would mix every principal's content into one file that no principal can be given access to. The global log should stay small, cross-principal, and free of content.
- **Rely on transcripts.** They miss workflow callbacks and background bodies (F6, F7), the principal can delete or move them, and retention prunes them (F8). They are conversation memory, not a record.
- **Bring back the `tool.call` / `tool.result` session events.** Same problems as transcripts, and it splits the record across thousands of session files.
- **Per-tool record schemas** (Bash command / exit / cwd, Write path / bytes, and so on). Better to read, but each new tool needs its own schema. One generic record with redaction and caps covers every tool now. Per-tool summaries can be added later as an optional `summary` field.
- **Record digests only, as today.** A digest can only confirm a call you already suspect. It cannot answer "what ran".
- **Sandboxing.** It is the real fix for the effects of `Bash` and workflows (§5), and it is still the long-term direction (ADR-078 §5). A ledger is useful with or without it.

---

## 5. Consequences and residual risk

- **What is recorded is the attempt, not its effects.** The ledger shows the command and its output, not every file a script touched or every connection it opened. Without a sandbox or syscall tracing, nothing more is visible.
- **Self-review is a feature, not a control.** A prompt-injected principal is the adversary in that threat model, and it can rewrite its own ledger with `Bash`. The security value comes from the owner reviewing the ledger and from tampering being detectable (D5). Real prevention needs the off-host anchor (F-1).
- **Redaction is best-effort.** A secret the runtime does not know, in a format it does not recognize, will be stored. The ledger is 0600, outside the workspace, and out of snapshots, but on disk it is as sensitive as the transcripts.
- **Cost.** Every call gets two fsync'd lines plus a hash. That is small next to an LLM turn, but `Read` / `Grep`-heavy runs will pay it. If it shows up in profiles, the end record's fsync can be batched; the write-ahead start record stays synchronous.
- **Non-owner runs depend on the ledger being writable.** A full disk stops guests' tools. Owners keep working and are told (D5).

---

## 6. Retired along the way

Found while wiring the ledger, and removed because nothing used them or the
ledger supersedes them:

- **Async task files** (`{data_dir}/async_tasks/<task>.json`, `TaskFileWriter`):
  a write-only, cross-principal record holding plaintext params, deleted after
  24 h. Also the receipts' `task_file` field and `CompletionEvent.output_path`,
  which pointed at them.
- **`SessionService`**: built into `AppState` but never called; with it the
  `peko_session::session_info` DTOs (`HistoryEvent`, `HistorySummary`, …),
  whose `tool_calls` count was always 0.
- **`tool.call` / `tool.result` session events**: never written; tool calls
  ride `message.v2` content blocks.
- **`[governance] audit = all|commands|none`**: never read, yet it suggested
  auditing could be switched off.

## 7. Follow-ups

- **F-1:** Signed checkpoints posted to PekoHub, for an anchor outside the OS user's reach.
- **F-2:** An optional per-tool `summary` field (Bash `cwd`, Write path + bytes, `ChannelSend` target).
- **F-3:** Ledger-driven alerts, e.g. a workspace hook or a cron recipe that DMs the owner on `floor_denied` bursts or destructive Bash patterns.
- **F-4:** Give the ADR-078 policy reviewer a recent-history digest from the ledger, still without the conversation.

---

## 8. Tests pinning the ledger

- `security::ledger::dispatch_tests` (through the real dispatcher):
  - `the_start_is_durable_before_the_call_runs_and_the_end_follows` — a blocking tool shows an in-flight call; the global `tool.call` shares `call_id` and digest
  - `refusals_unknown_tools_and_bad_arguments_are_recorded`
  - `a_failing_command_is_an_error_with_its_exit_code`
  - `a_detached_call_ends_twice_under_one_call_id`
  - `a_background_body_names_its_task_and_spawning_call`
  - `known_secrets_never_reach_the_ledger`
  - `an_unopenable_ledger_refuses_everyone_but_the_owner`
  - `a_harness_ledger_verifies_against_the_global_log`
- `security::dispatch_tests::the_gate_allows_denies_or_asks_the_owner` — `ok` / `policy_denied` / `awaiting_owner`, and the gated call carries its `call_id`.
- `principal::manager::tests::the_policy_gate_reviews_and_the_owner_decides` — an approved replay names its request and the approved call.
- `ipc::handlers::tool::tests::workflow_callbacks_name_their_workflow_call_in_the_ledger`.
- `peko_observability::chain` — edited, deleted, torn, and truncated lines; checkpoint mismatches; pruning anchors; 0600/0700 modes.
- `security::ledger::{redact, read}` and `tools::builtin::tool_log` — redaction and caps, grouping and filters, compartment scope, defused excerpts, paging, parent/children, cross-log verification.
- `security::floor::tests::no_run_writes_a_tool_ledger`; `principal::config` retention defaults and the ignored legacy `audit` level.
