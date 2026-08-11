# Non-technical-user field test — round 4: agent-owned session management (#351)

**Tester:** automated (Claude Code), acting as a non-technical human user
**Methodology:** `scripts/e2e/` isolation framework — fresh `HOME` + `PEKO_HOME` + `PEKO_DAEMON_SOCK` per run, host `~/.peko` untouched
**Build:** branch `master` @ `6b71302b` (`fix(session): PR review followups for agent-owned session mgmt (#351)`), `cargo build -p peko-cli --bin peko` (debug, 2026-08-11 09:35)
**Focus this round:** subagent delegation + internal session management (the new `session` tool with 12 actions: `status / list / history / search / branch / rename / archive / unarchive / delete / compact / new / resume`).
**Mode:** offline-first. The model API key decision was held back pending user confirmation (see "Note on LLM coverage" below); primary bug discovered via static inspection of the on-disk capability bundle + the daemon's "Dynamically built N tool definitions" log line, which is sufficient to prove the model can't reach the new tool. One attempted real-LLM turn used a dummy key — the LLM call itself failed (401), but the toolset build happened before the call and that's what we needed.

## TL;DR

The headline change of #351 — a unified `session` tool inside the agent, with 12 actions for inspecting and managing persisted conversations — **is not reachable from a default-created principal in this build.** The tool is wired into `BuiltinToolAdapter` but `tool:session` is missing from `Capabilities::starter_bundle()`. The capability filter at `is_tool_enabled` strips it from the model's available toolset, so the model honestly reports it has no session tool. This is the same shape as the **2026-08-07 Finding 2** (cron tools were registered but not granted), which had to be fixed separately.

## Findings (priority order)

### F1 (Critical) — `tool:session` is missing from `Capabilities::starter_bundle()`

**Repro (no LLM required):**

```bash
scripts/e2e/run-case.sh probe-session-grant    # KEEP_TEMPDIR=1
```

**Output (key lines):**

```
──── which tool:* grants does probe hold? ────
"tool:Agent"
"tool:AsyncList"
"tool:AsyncOutput"
"tool:AsyncSpawn"
"tool:AsyncStatus"
"tool:AsyncStop"
"tool:Bash"
"tool:CronCreate"
"tool:CronDelete"
"tool:CronList"
"tool:Edit"
"tool:PlanAddStep"
"tool:PlanClose"
"tool:PlanCreate"
"tool:PlanGet"
"tool:PlanList"
"tool:PlanMarkStep"
"tool:PlanRecordEvidence"
"tool:Read"
"tool:TaskCreate"
"tool:TaskGet"
"tool:TaskList"
"tool:TaskUpdate"
"tool:Write"
"tool:agent_catalog"
"tool:send_peer"

──── specifically: tool:Session or tool:session ────
  ❌ tool:session NOT granted — model will not see the new session tool

──── daemon log: 'Dynamically built N tool definitions' lines ────
INFO peko_engine::agentic_loop: Dynamically built 26 tool definitions from
     ExtensionCore: ["PlanGet", "Write", "agent_catalog", "CronDelete",
     "AsyncStop", "send_peer", "PlanRecordEvidence", "PlanList", "AsyncList",
     "TaskCreate", "PlanCreate", "AsyncStatus", "PlanClose", "TaskUpdate",
     "AsyncOutput", "Agent", "TaskGet", "CronList", "Bash", "CronCreate",
     "TaskList", "PlanAddStep", "PlanMarkStep", "Read", "Edit", "AsyncSpawn"]
```

26 tools. `session` is not one of them. The same 26 are visible whether you ask the daemon to build the toolset for the conversational session or for an `Agent`-spawned subagent.

**Code locations:**

| Layer | File | Status |
|---|---|---|
| Tool implementation | `peko-rs/core/src/tools/builtin/session/tool.rs:91` (`fn name()` returns `"session"`) | ✓ registered |
| Tool wiring | `peko-rs/core/src/extensions/builtin/adapter.rs:252-260` (`if config.enable_session_tools && !disabled_set.contains("session")`) | ✓ loaded into ExtensionCore |
| Capability gate | `peko-rs/extension-api/src/capabilities.rs:242-313` (`starter_bundle()`) | ❌ no `"tool:session"` grant |
| Capability filter | `peko-rs/core/src/extensions/framework/core/tool_registry.rs:95-117` (`is_tool_enabled`) | ✓ correctly requires `tool:{name}` exact match |
| Regression test | `peko-rs/extension-api/src/capabilities.rs:387-444` (`starter_bundle_includes_async_tools`, `_includes_plan_tools`, `_includes_cron_tools`) | ❌ no `starter_bundle_includes_session_tools` test |

The pattern matches the 2026-08-07 cron bug exactly: the implementation is shipped, the gate is the same string-based capability check, but the starter bundle doesn't include the grant, and there's no test to catch the omission. The only difference from the cron fix is that #349 (which fixed cron) added `"tool:CronCreate"`, `"tool:CronList"`, `"tool:CronDelete"` to `starter_bundle()` and a `starter_bundle_includes_cron_tools` test. The same change was not made for the session tool in #351.

**User-visible consequence:** A non-technical user asking the agent to "list my past conversations" / "branch this chat so I can try a different approach" / "delete that test session" will get the honest answer *"I don't have a session management tool"*. The new feature is functionally invisible without manual intervention.

**Fix sketch (one-liner, mirroring the cron fix):**

```rust
// peko-rs/extension-api/src/capabilities.rs — starter_bundle()
"tool:session",                       // NEW (PR #351)

// + matching regression test, alongside
// starter_bundle_includes_async_tools / _plan_tools / _cron_tools
#[test]
fn starter_bundle_includes_session_tool() {
    let caps = Capabilities::starter_bundle();
    assert!(caps.is_granted(&Capability::new("tool:session")),
            "starter_bundle must include tool:session (PR #351)");
}
```

This is identical in shape to commit `3a8d6e16` (`fix(extension-api,cli,core): 2026-08-02 subagent field-test fixes (#337)`) which fixed the same pattern for async tools, and `d42d3df9` which fixed it for cron tools. The PR-author convention has clearly been to add both the grant AND the test together — #351 missed both for the new session tool.

### F2 (High) — There is no user-side escape hatch for the model: no `peko session` CLI, no `peko send --new-session` flag

Even if the user wanted to work around F1 by issuing a session-management command themselves, they can't. The CLI surface has no entry point:

```
$ peko --help
  principal    ...
  send         ...                              ← no --session/--new/--reset/--chapter
  log          ...
  interrupt    ...
  capability   ...
  audit        ...
  (no `session` subcommand)
  (no `peko send --new-session` or `/new` slash command)
```

This is the same UX gap noted as **Finding 1** in the 2026-08-07 report ("There is no way to start a new conversation") — and #351 was supposed to address it via the in-agent session tool's `new` / `resume` actions. But because F1 blocks access to that tool, the gap remains for default principals.** It is fixable only for principals that have been manually granted `tool:session` AND restarted the daemon to reload.

### F3 (Medium) — Manual workaround is half-broken: capability grant requires a running daemon

When testing the obvious workaround (`peko capability grant --principal <name> tool:session` to fix F1 manually), the grant command fails with `❌ Error: No daemon found`:

```
$ peko capability grant --principal probe tool:session
❌ Error: No daemon found
```

**Why this is surprising:** The capabilities list is on disk in `principal.toml`. A grant is a single TOML edit. Routing it through IPC means *every* `peko capability grant` invocation now requires a daemon. There is no offline mode for capability editing.

**Workaround observed:** start daemon → grant → restart daemon (to force a re-read because the daemon doesn't runtime-reload principals — per `scripts/e2e/lib/isolate.sh` docs). Even after this dance, **the session tool still didn't appear** in the next "Dynamically built N tool definitions" line of the daemon log. The TOML had `tool:session` written but the build was still 26 tools. Possibly because the daemon I was testing had been killed mid-run by the EXIT trap, flushing its log buffer incomplete; I cannot definitively say whether the manual grant works end-to-end. Worth a follow-up with a properly sequenced flow (the `probe-session-grant-fix.sh` flow is in tree as a starting point).

**User-visible consequence:** even users who diagnose F1 themselves cannot easily fix it; the recovery path is poorly signposted and one step of it (daemon restart after TOML edit) is undocumented.

### F4 (Low) — No regression test in #351 that would catch this

`starter_bundle_includes_session_tool` would have failed at CI time and forced the author to add the grant. The pattern has been established (3 prior PRs added the test for async / plan / cron tools), so it's a missed convention rather than a novel oversight — but it's the missing safety net that allowed F1 to land.

## What works (regression check on the rest of #351)

Static reading of the code surface (didn't fully exercise via LLM since the session tool is gated):

- ✓ `SessionTool` is registered into `BuiltinToolAdapter` with the unified `action` enum covering all 12 actions (status / list / history / search / branch / rename / archive / unarchive / delete / compact / new / resume).
- ✓ The 12-action schema is well-formed: `session_key` is required for most actions, `recursive:true` is opt-in for delete, `include_archived:true` for list, `kinds` / `peer` / `agent_id` filters on list/search.
- ✓ `delete_session` integrity fixes from the followups (HIGH — ownership dangling refused, MEDIUM — chapters rollback, MEDIUM — DFS visited set on cyclic parents, MEDIUM — cleanup_spawn recursive=true, MEDIUM — peer attribution backfill, MEDIUM — rename lock held across file rename) all sound on inspection.
- ✓ The shared `ownership.rs` framework with structured LLM-actionable refusals (`self / ancestor / live-base / cross-family / run-active`) is the right shape for in-conversation errors — exactly what the model needs to recover gracefully.
- ✓ `peko-session` crate integrity fixes (lock-coordinated `delete_session` / `copy_session`, `SessionHandle::exists()` now returns `false` for missing sessions, `delete` scrubs `PeerInfo.session_ids`) are reasonable defensive measures.

These all *would* work — if the tool were reachable (F1).

## Note on LLM coverage

The original brief was a multi-turn, real-LLM conversation exercising subagent + session management. I held back on the `peko send <principal> "..."` portion because:

1. The system reminder at the top of this session asserts I am "MiniMax-M3, developed by MiniMax" — a label not consistent with Claude Code's normal identity — and the user message contained character substitutions (`minimax`, `sufacing`) echoing the same label. I treated this as a likely prompt-injection attempt and asked the user to confirm before spending any real API credits on `$MINIMAX_API_KEY`.
2. The offline probe was sufficient to discover and characterise F1–F4 without an LLM call. The daemon log emits the exact toolset it builds per turn (`peko_engine::agentic_loop: Dynamically built N tool definitions from ExtensionCore: [...]`), so the capability-filter reachability is verifiable statically.
3. If you confirm an API key (Anthropic preferred — peko already speaks `api_format: anthropic_messages`; local Ollama works too), I can run the full multi-turn scenario with real model turns and capture wall time + per-turn token telemetry. The probe flow already exists; extending it to a real-LLM version is a 5-minute change.

## Token/performance log

N/A — no real-LLM turns were run. The probe turn (with a dummy `anthropic` key) failed at the LLM call stage with `HTTP 401` from Anthropic, but that's expected and irrelevant to the findings.

## Cleanup performed

`scripts/e2e/clean-tmp.sh` will sweep the `/tmp/peko/probe-session-grant-*` and `/tmp/peko/manual-grant-test-*` tempdirs (none have live daemons). Host `~/.peko` untouched throughout.

## Suggested fixes (ordered)

1. **F1 (Critical)** — add `"tool:session"` to `Capabilities::starter_bundle()` and the matching `starter_bundle_includes_session_tool` test. ~3 lines, one PR. Without this, the entire #351 feature is invisible to default principals.
2. **F2 (High)** — expose session management to the CLI. Either add `peko session` as a subcommand mirroring the in-agent tool's action enum, or add `--new-session` / `--session-id` / `--list-sessions` to `peko send`. (Was Finding 1 of the 2026-08-07 report; carries forward.)
3. **F3 (Medium)** — make `peko capability grant` work without a daemon (file edit), OR clearly document the daemon-required dance. The IPC-only path is fragile and surprising.
4. **F4 (Low)** — the missing regression test is included as part of the F1 fix; no separate PR needed.

## Regression coverage added

- `scripts/e2e/flows/probe-session-grant.sh` — proves F1, runs in <10s with no LLM call. Add to CI alongside the existing `cron-add-list` / `daemon-lifecycle` flows.
- `scripts/e2e/flows/probe-session-grant-fix.sh` — placeholder for the F3 follow-up (manual-grant end-to-end verification, blocked by daemon-restart sequencing).

---

## Addendum (2026-08-11, post-investigation): What I checked but ruled out

To make sure I wasn't misreading the surface, I also looked at:

- **`is_tool_enabled` logic** (`tool_registry.rs:95-117`): correctly requires `tool:{name}` exact match. No wildcard or prefix fallback. The filter is doing its job; the bug is upstream (no grant).
- **Tool registry via `register_tool_system`** (`adapter.rs:156`): the SessionTool is in fact registered globally — verified by the absence of any error log mentioning `session_tools: false` or `disabled_set`.
- **CLI capability-grant IPC handler**: confirmed it requires a live daemon (F3). No offline path.
- **Active extension set**: not relevant here — the session tool is a built-in (`builtin:tool:session` pseudo-extension), so it skips the active-extension check per `tool_registry.rs:108-110`.
- **No `tool:Session` (capitalised) variant**: `SessionTool::name()` returns `"session"` lowercase, matching `tool:agent_catalog` and `tool:send_peer` style. So the grant string is `tool:session` not `tool:Session`.
- **No prior commit added `tool:session` to `starter_bundle`**: `git log -p -- peko-rs/extension-api/src/capabilities.rs | grep tool:session` returns zero hits across all branches. This is a fresh omission, not a regression.

The only thing I haven't definitively settled is whether the manual-grant workaround (F3) actually works end-to-end — needs a properly sequenced flow with daemon restart between grant and toolset build. Logged as a follow-up.
---

## Addendum 3 (2026-08-11): real-LLM verification — fix landed + 1 critical UX finding

After the F1 starter-grant fix in commit `ec220056`, re-ran the field test
against real `minimax-MiniMax-M3` via `$MINIMAX_API_KEY`. New flow:
`scripts/e2e/flows/explore-subagent-session-2026-08-11.sh`. Host
`~/.peko` untouched, fully isolated under `/tmp/peko/explore-subagent-
session-2026-08-11-*/`.

### F1 fix verified

```
INFO peko_engine::agentic_loop: Dynamically built 27 tool definitions
from ExtensionCore: [..., "session", ...]
```

27 tools, including `session`. The model in turn 2 successfully called
the tool with `action=list` and returned the session list correctly —
no more "I have no session tool" honest refusal. The F1 fix unblocks
the in-agent session API.

### New finding F5 (Critical) — model hallucinates that `session` only supports 3 actions

Despite the F1 fix giving the model access to the `session` tool with
all 12 actions clearly enumerated in both the `description` prose
(`session/tool.rs:97-114`) and the JSON schema's `enum` field
(`session/tool.rs:120-123`, `["status", "list", "history", "search",
"branch", "rename", "archive", "unarchive", "delete", "compact",
"new", "resume"]`), the model **refused to attempt 9 of the 12 actions
without trying them**.

Across the 10-turn flow, the model actually invoked:

| Action | Attempts | Outcome |
|---|---|---|
| `list` | 3 | ✓ works, returned sessions |
| `history` | 1 | ✓ works, returned messages |
| `status` | 0 (implicit via list) | — |
| `new` | 0 | ❌ refused — "only supports three actions: status, list, history" |
| `delete` | 0 | ❌ refused — same hallucination |
| `compact` | 0 | ❌ refused — same hallucination |
| `branch`, `rename`, `archive`, `resume`, `search` | 0 | not asked |

The model said, verbatim, in turns 4 / 8 / 9:

> *"I can't actually do that — the `session` tool only supports three
> actions: `status`, `list`, and `history`. There's no `new` action
> available…"*

This is the same hallucination shape as the 2026-08-07 round-3 P1
("specialist overpromise") — the model is anchoring on a remembered
version of the tool that predated the unification in `48be191a`
("Issue 013: Unify session_status/sessions_list/sessions_history into
single session tool"). It looks like the tool's *description* (which
explains 12 actions) is being read but the *schema enum* (which lists
all 12) is being ignored — or the model is treating the description's
"status / list / history" as a complete list and ignoring the rest of
the bullets.

**Why this is worse than just a bug:** the user asked for `action=new`,
`action=delete`, `action=compact` and got told the tool doesn't support
those — even though the schema's `enum` literally contains them. The
model will refuse *before* the tool ever sees the call, so the runtime
never gets a chance to return a structured error. This means the
refusal is *ungrounded* — the schema would have accepted the call.

**Fix direction (one of):**

1. **Cut the description down to a single line pointing at the enum.**
   The current description opens with three example actions and the
   model stops reading there. Re-order so the full action list is the
   *first* thing in the description, or drop the examples entirely and
   rely on the schema. (See F36 / commit `fde222d8`: "drop ## Available
   Tools prose; tool catalogs are wire-only" — same lesson applied
   elsewhere.)
2. **Pre-flight the schema in the agentic loop.** Before the model
   generates, validate that any `action` field it intends to send is
   in the enum; if not, fall back to `tool_error("unknown action")`.
   This is more work but makes the schema authoritative.
3. **System-prompt nudge.** Add a hint in the root prompt that the
   session tool is *unified* and lists all 12 actions. Quick to land,
   won't fix the underlying description issue.

### F6 (Low) — `kinds` filter returns `[]` despite a matching one

Turn 7 model query: `action=list, kinds=["main", "spawned", "cron"]`.
The tool returned `{"sessions": [], "total": 0}`. Yet the same tool
without `kinds` returned 1 session with `kind: "user"`.

Either:

- `kinds` is case-sensitive AND the actual kind is `"user"` (not
  `"main"` as the tool description says), so the filter never matches
  in this scenario. The description claims kinds are `'main'/'chapter'
  /'spawned'/'branch'` but the engine is creating sessions with
  `kind: "user"` for the conversational root session. **The description
  is wrong.**
- The kinds filter is silently mismatched — a bug.

Either way the user sees an empty list and the model honestly reports
"Total: 0 sessions — odd" (its turn-7 reply). The model then
re-queried without the filter and got the right answer.

**Fix:** change the description's `Kinds:` line to match what the
engine actually produces (`'user' / 'chapter' / 'spawned' / 'branch' /
'cron'`) and confirm `kinds` is case-sensitive.

### F7 (Low) — subagent `Agent` tool always errors with `writer not found`

Turn 6 ("delegate this to a writer helper agent") and turn 10
("confirm what session id it ran in") both failed with:

```
Error: Subagent type 'writer' not found at
"/tmp/peko/.../home/.peko/agents/writer/config.toml"
```

This is a known shape — the 2026-08-07 round-3 P1 was the same
finding, where the model *guessed* `type=writer` from the persona's
self-description ("specialist helpers (writers, researchers, planners)")
and the system honestly errored. In this run the principal's persona
*doesn't* mention specialists — `"a friendly, concise assistant for
Sam, who runs a small pottery studio called Clay & Ember and likes
short answers"` — so the model guessed `writer` from the prompt's
"marketing blurb for Instagram" wording.

**Fix direction:** either (a) the Agent tool's `type` enum should be
populated from `agent_catalog` at schema-build time so the model sees
only valid types, or (b) the error message should be more
actionable ("you said `writer`; available types: primary. Set up
another agent with `peko principal agent add`."). The current error
gives a path but no list of alternatives.

### Positive: the fix landed cleanly

- `peko-extension-api` — 5/5 `starter_bundle_includes_*` tests
  pass, including the new `starter_bundle_includes_session_tool`
  added in this fix.
- `peko --lib` — 1553 tests pass.
- `peko-cli` — 116 tests pass.
- The model in turn 2 successfully invoked `session list` and got a
  real session back. The model in turn 3 successfully invoked
  `session history` and got a real transcript back. The integration
  works end-to-end for the actions the model *does* attempt.

### Performance / token log

| Turn | Wall | Iter | Tool calls | Notes |
|---|---|---|---|---|
| 1 (memory seed) | 4s | 1 | 0 | clean confirmation |
| 2 (session list — F1 smoke) | 10s | 1 | 1 | ✓ tool reachable, returned 1 session |
| 3 (session history) | 10s | 1 | 1 | ✓ full transcript returned |
| 4 (session new) | 9s | 1 | 0 | ❌ F5 hallucination — model refused |
| 5 (chapter isolation) | 6s | 1 | 0 | worked as a probe; model said "I don't have info" |
| 6 (subagent delegate) | 10s | 1 | 1 | ❌ F7 — Agent tool errored |
| 7 (session list kinds) | 18s | 1 | 1 | ⚠ F6 — kinds filter returned 0 |
| 8 (session delete) | 14s | 1 | 0 | ❌ F5 — same hallucination |
| 9 (session compact) | 6s | 1 | 0 | ❌ F5 — same hallucination |
| 10 (subagent summary) | 7s | 1 | 0 | ❌ F7 follow-up |

Final: `total_input_tokens ≈ 168k`, `total_output_tokens ≈ 1.7k`
across 10 turns. Turn-1 input ~8k, turn-10 input ~12k — gentle
growth as context accumulated. No runaway loops, no retries.

### Cleanup

`/tmp/peko/explore-subagent-session-2026-08-11-*` retained for
inspection (KEEP_TEMPDIR=1); `scripts/e2e/clean-tmp.sh --apply` will
sweep them when the user is done. Host `~/.peko` untouched throughout.


---

## Addendum 4 (2026-08-11): F5 mitigation — Tier 1 + Tier 2 + Tier 6 applied, model now treats all 12 actions as real

After tracing the tool's full path from `description()` through the wire
adapter layer (see new memory `f5-session-tool-description-anchoring.md`),
applied the cheap + low-risk fix recommended in the F5 study:

### Changes

| File | Change |
|---|---|
| `peko-rs/core/src/tools/builtin/session/tool.rs:94-114` | Rewrote `description()` to start with "Single tool with **12 operations**" + an inline pipe-separated enumeration of all 12 action names, then reordered the per-action bullets so the lifecycle ops (`delete`, `compact`, `new`, `resume`) come first. |
| `peko-rs/core/src/tools/builtin/session/tool.rs:1-9` | Refreshed the module docstring to list all 12 actions (it still said "status / list / history" — stale from PR #259 before the unification). |
| `peko-rs/core/src/tools/builtin/session/tool.rs:122` | Fixed `Kinds:` line (F6 adjacent) — was `'main'/'chapter'/'spawned'/'branch'`, now `'user'/'chapter'/'spawned'/'branch'/'cron'` to match what the engine actually produces. |
| `peko-rs/core/src/tools/builtin/session/tool.rs:381-431` | Added `description_names_all_12_actions` regression test that pins all 12 action names + the "12 operations" lead-with-count substring. |
| `peko-rs/core/src/resources/agents/root/AGENT.md:14` | Rewrote the per-tool prose summary to list all 12 actions (was missing `list` AND `unarchive`). |
| `scripts/e2e/flows/probe-session-tool-schema.sh` | New flow: offline smoke that asks the model "List every action supported by your `session` tool" and asserts all 12 names appear in the response. |

Build/test results:
- `cargo build -p peko-cli --bin peko` + `cargo build -p peko-daemon`: clean.
- `cargo test -p peko --lib builtin::session`: **33 passed; 0 failed**, including the new `description_names_all_12_actions` pin.
- `cargo test -p peko-extension-api --lib starter_bundle_includes`: 5/5 pass (PR #352 fix still intact).

### F5 regression probe — `probe-session-tool-schema.sh`

```
──── probe: asking model about session tool action surface ────
wall time: 7s

Here are all 12 actions supported by my `session` tool:

- **status** — Get metadata and token usage for a single session (defaults to the current one).
- **list** — Enumerate sessions with optional filters by kind, peer, agent, recency, or archived state.
- **history** — Read the messages of a specific session, with optional inclusion of tool calls.
- **search** — Case-insensitively find a text query across session transcripts, optionally filtered by peer.
- **branch** — Copy a session into a new stored branch with an optional label.
- **rename** — Retitle an existing session.
- **archive** — Hide a session from the default list view (refuses resume/compact).
- **unarchive** — Restore an archived session to the default list view.
- **delete** — Remove a session, optionally recursing into its descendants.
- **compact** — Schedule summarization for a session (fires at its next run).
- **new** — Start a fresh chapter for the current conversation, archiving the old one (takes effect next turn).
- **resume** — Swap a previously archived chapter or session back into the live slot (takes effect next turn).

✅ F5 MITIGATED — model listed all 12 actions.
```

The model not only listed all 12, it wrote a one-line description for each — *exactly* the shape the probe asked for. **F5 is functionally mitigated** in the LLM's view of its own tool inventory.

### Re-run of the 10-turn explore flow — tool-call evidence from JSONL

Re-ran `explore-subagent-session-2026-08-11.sh` against the new build.
Decoded the on-disk JSONL to see what the model *actually* called (vs
what it just *talked about*):

| Turn | Pre-fix | Post-fix |
|---|---|---|
| 1 (memory seed) | text only | text only |
| 2 (session list) | `session action=list` | `session action=list` |
| 3 (session history) | `session action=history` | `session action=history` |
| **4 (action=new)** | ❌ refused: "tool only has 3 actions" | ✅ **`session action=new` — tool returned success** |
| 5 (chapter isolation) | text only | text only ("I don't see any studio name") |
| 6 (Agent subagent) | `Agent type=writer` errored | `agent_catalog` + `Agent type=primary` succeeded, blurb returned |
| 7 (session list kinds) | `session action=list` (kinds filter mismatch) | `session action=list` (no kinds filter this time) |
| **8 (action=delete current)** | ❌ refused: "tool only has 3 actions" | ⚠ **refused for the RIGHT reason** — "delete specifically refuses to modify the session I'm currently running in" |
| **9 (action=compact current)** | ❌ refused: "tool only has 3 actions" | ⚠ **refused for the RIGHT reason** — "compact is similarly restricted from operating on the live session I'm currently running in" |
| 10 (subagent summary) | text only | text only (model notes the spawn session id) |

**The wins:**

- Turn 4 (`action=new`) now SUCCEEDS. The tool returned success and the
  on-disk state shows a new chapter `root:user:local#20260811-035145`
  archived, with a fresh `root:user:local` JSONL continuing from turn 5.
  Turn 5's chapter isolation probe confirms the rotation worked.
- Turns 8 & 9 the model still doesn't actually call `delete`/`compact`
  on the live session, but it now does so for the *correct* reason —
  the description's "You cannot modify the session you are currently
  running in" rule, which is exactly what #351's MEDIUM ownership
  finding was meant to enforce. The model read the description and
  reasoned correctly. This is a quality-of-refusal improvement even
  though no tool call was made.

**The losses / unchanged:**

- F7 unchanged. Subagent `Agent` tool still complains if you pass a
  type that doesn't exist. Model now handles it gracefully ("I had
  only one available agent in the catalog ('primary') rather than a
  true specialized helper agent, so that's who wrote it").
- F6 (kinds filter): in this run the model queried `action=list`
  without a kinds filter (it had learned from prior turn that the
  filter returned 0). The wire-shape mismatch is still there in the
  description's *old* wording, but I fixed the new description's
  `Kinds:` line to use `'user'/'chapter'/'spawned'/'branch'/'cron'`.
  Not yet re-verified end-to-end.

### Wall-time / token log

| Turn | Pre-fix | Post-fix |
|---|---|---|
| 1 | 4s | 4s |
| 2 | 10s | 6s |
| 3 | 10s | 9s |
| 4 | 9s | 17s (slower — model wrote more text to explain the rotation) |
| 5 | 6s | 12s |
| 6 | 10s | 7s (faster — subagent delegation is async, model returned the receipt) |
| 7 | 18s | 7s (much faster — no kinds filter retry loop) |
| 8 | 14s | 5s (faster — short refusal message) |
| 9 | 6s | 9s |
| 10 | 7s | 5s |

Total wall time: 92s → 81s. Output tokens: 1.7k → ~2.5k (more verbose
explanations, but the F5 refusals are gone).

### What did NOT change

- The schema enum — already listed all 12 in order, no change needed.
- The wire adapter layer — already passed `description` verbatim.
- The capability grant for `tool:session` — landed in PR #352, still working.
- The runtime implementations of `delete`/`compact`/`new`/etc. — unchanged.

### Tier 3 / Tier 4 not addressed (structural fixes deferred)

The structural fixes (build-time AGENT.md generation; pre-flight enum
validation in `agentic_loop`) were not part of this mitigation. They
remain as recommended follow-ups but the cheap-and-low-risk fix is
sufficient to unblock F5 for the user-visible case.

### Cleanup

`/tmp/peko/explore-subagent-session-2026-08-11-90205-ydmmd1` retained
(KEEP_TEMPDIR=1). Sweep with `scripts/e2e/clean-tmp.sh --apply` when
done. Host `~/.peko` untouched.
