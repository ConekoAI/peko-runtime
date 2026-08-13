# Non-technical-user field test — round 6: post-tool-reduction session probing

**Tester:** automated (Claude Code), acting as a non-technical human user
**Methodology:** `scripts/e2e/` isolation framework — fresh `HOME` + `PEKO_HOME` + `PEKO_DAEMON_SOCK` per run, host `~/.peko` untouched
**Build:** branch `master` @ `c976d510` (PR #353 implicit-session-management merge on top of round-5's `b0637fbb`), `cargo build -p peko-cli --bin peko` + `cargo build -p peko-daemon --bin peko-daemon` (debug, 2026-08-12 14:08)
**LLM:** `minimax-MiniMax-M3` via `$MINIMAX_API_KEY` (real Anthropic-compat endpoint)
**Focus this round:** the session tool surface was reduced from 12 actions to 6 in PR #353, so round-6 deliberately probes **different angles** than r5: the kinds-filter regression (still broken?), multi-subagent parallel delegation, `Agent.resume_session` end-to-end behavior, what the model tells a user who wants to "start fresh" (since `new`/`compact`/`branch` are gone from the tool), and whether the reduced surface still tells a coherent story to the model.
**Persona:** Same as r5 — "Sam", runs a small pottery studio "Clay & Ember", beginner wheel class Saturdays 10am, favorite glaze celadon, working on an 'ember-glaze' tea-bowl line. Prefers short answers.

## TL;DR

The kinds-filter footgun from r5 is **STILL BROKEN** — the model had to rediscover it empirically in TURN 8 (`kinds=["spawned"]` returns 0; `kinds=["spawn"]` returns 4). The session tool surface was indeed **reduced from 12 to 6 actions** between r5 and r6 (PR #353's "lifecycle operations engine-driven"), but the description's stale error text still tells the model to use `compact` and `new` for the live session — actions that no longer exist on the tool. The model noticed and called it out (TURN 13/14). New and useful: multi-subagent parallel delegation works (TURN 7), `Agent.resume_session` continues the same session with its full history (TURN 10+11, 4 messages after resume), and `parent_session` linkage now appears in `status` output (TURN 19) — a small but real win. New noise: the daemon emits a `Plan tools enabled by config but no principal_plan_port was bound` warning on every spawn.

Plus: the project explicitly forbids a `peko session` CLI subcommand (peko-rs/cli/src/commands/log.rs:12, mod.rs:124) — the user-facing inspection path is `peko log <principal>`, which reads the runtime-owned chat log (a different store than the principal's mutable session JSONL). So r5's F4 isn't a gap, it's by design — but it leaves a non-technical user with no way to see kinds/peer breakdowns, spawned subagents, or search across sessions. That's the user-visible ceiling.

## Findings (priority order)

### F1 (Critical, REGRESSION CONFIRMED) — `kinds=["spawned"]` still returns 0 sessions

Round-5's F1 was that the description's `Kinds:` line and the parameter `kinds` description both list `'spawned'`, but the engine stores `trigger = "spawn"`, and the runtime filter is exact-match `kinds.contains(&m.trigger)` (peko-rs/core/src/session/session_runtime_impl.rs:196). Round-6 confirms **nothing changed**.

**Decoded evidence (TURN 8 transcript, model output):**

> `kinds=["spawned"]` → 0 sessions. Empty array.
> `kinds=["spawn"]` → 4 sessions — all the helpers I spawned earlier.

**Engine on-disk truth (post-TURN 20 sessions.json):**

```json
{
  "root:user:local":            { "trigger": "user",  ... },
  "324a17d7-...":               { "trigger": "spawn", ... },  ← marketing blurb helper
  "0c266e38-...":               { "trigger": "spawn", ... },  ← Saturday tagline helper
  "5cce395f-...":               { "trigger": "spawn", ... }   ← hashtags helper
}
```

The model itself diagnosed the bug correctly ("This is a **naming inconsistency between the docs and the engine**… The `session` tool's own description lists 5 valid `kinds`: `'user'`, `'chapter'`, `'spawned'`, `'branch'`, `'cron'`. But the engine actually stores these spawn sessions under the literal kind string `'spawn'`, not `'spawned'`. So filtering by the documented value (`'spawned'`) returns nothing, while the actual value (`'spawn'`) finds them.") — but a non-technical user who reads the description, asks "show me my subagent sessions" with `kinds=["spawned"]`, and gets `{"sessions":[], "total":0}` will have no idea why.

**The drift is now in TWO tool descriptions, not one** — PR #353 also touched the Agent tool, which now says at peko-rs/core/src/tools/builtin/messaging/agent.rs:567:

> "Sessions you spawn appear in the `session` tool as kind 'spawned' — use that tool to inspect and manage them"

So the Agent tool is **teaching the model to do exactly the broken thing**. A model that follows the Agent tool's instruction literally will hit zero results in the session tool.

**Fix direction (same as r5, unaddressed):**
- **Cheap:** rewrite the description's `Kinds:` line AND the parameter field to match the engine's actual trigger values: `user, cron, webhook, event, file_watch, branch, spawn`. Add a regression test that pins both descriptions against `SessionTrigger` so they can't drift again.
- **Right:** rename `Spawn` → `Spawned` in the engine and add a `Chapter` variant set during the rename (closes F1 + F2 in one pass).

### F2 (Critical, REGRESSION CONFIRMED) — `kinds=["chapter"]` still returns 0 sessions

Round-5's F2 was that the description promises `'chapter'` (rotated conversations) but the engine never sets `trigger = "chapter"`. The chapter is a filename-suffix convention (`#<timestamp>`), not a metadata kind. Round-6 confirms **nothing changed**.

**TURN 16 transcript:**

> Query 1: `kinds=['chapter']` → `{"sessions":[], "total":0}`. Zero sessions. No kind matches `chapter`.

The model again self-diagnosed correctly ("the engine has never created one (or it filters them out of `list`). Consistent with the earlier `'spawned'` finding: the documented kinds and the actual storage kinds don't fully align.")

**Engine truth (peko-rs/session/src/events.rs:50-67):**

```rust
pub enum SessionTrigger {
    User, Cron, Webhook, Event, FileWatch, Branch, Spawn,
}
```

No `Chapter` variant. The chapter path in peko-rs/core/src/principal/agent_runner.rs:289-316 uses `rename_session_id` + `set_session_title` and **never** sets a `chapter` trigger.

**Fix direction:** either add a `Chapter` variant and set it in the rename path, or fix the description to say "chapters are identified by the `#<timestamp>` filename suffix on a `user`-kind session; pass `include_archived:true` to see them." The latter is the cheap option and matches the F1 cheap fix.

### F3 (High, NOT RE-TESTED) — `peers.json` still drops the archived chapter

R5 found that `peers.json` is updated with the new live session id but the old chapter is not appended to the `session_ids` array. Round-6 didn't trigger any chapter rotation (the engine never auto-rotated in 20 turns; the session was 65 messages, well under the rotation threshold), so this finding is **not re-confirmed but also not refuted** — the code path is unchanged at peko-rs/core/src/principal/agent_runner.rs:286-288, which explicitly says:

> "`rename_session_id` re-keys sessions.json only; peers.json routing is left for the fresh create path below, by design."

So the design is intentional: chapters are invisible to `peer=` filtering. A user trying `session list peer="user:local"` would see only the active session, the chapter is lost. (Carried.)

### F4 (NOT A BUG, INTENTIONAL) — `peko log` is the user-facing surface; `peko session` will never exist

R5 flagged "no `peko session` CLI subcommand" as a High finding. Round-6 investigated and found **two places in the CLI source that explicitly state this is by design**:

- peko-rs/cli/src/commands/log.rs:12 — "There is no `peko session` command and there will never be one. This is the only user-facing way to inspect a principal's consumer-visible conversation without running a turn."
- peko-rs/cli/src/commands/mod.rs:124 — same comment, on the `Log` variant.

`peko log <principal>` reads from `peko_chat_log::ChatLogMessage` (peko-rs/chat-log/src/types.rs:25-33), which is the **runtime-owned chat log** — a separate store from the principal's mutable session JSONL working memory. It's the owner-facing inspection path; the session tool the model uses is the agent-facing one.

**So the question becomes:** does `peko log` cover what a non-technical user needs? Round-6 didn't run it (the focus was the agent-side surface), but the data model suggests it shows the principal's conversation threads, not spawned subagents, kinds breakdowns, or per-session search. A non-technical user still has **no way** to see the spawned helpers from TURN 7 or to search across sessions. The gap is real even if the design choice is documented.

**Fix direction:** if a non-technical user is the target, the missing capability is owner-side `peko log --kind spawn --peer user:local` or similar. The chat log store would need to mirror session metadata (kind, parent_session, archived) for that to work. Otherwise, the user is told "ask the model" — which is fine until F1 breaks that path.

### F5 (Medium, NEW) — Stale error text: live-session refusal references `compact` and `new` which no longer exist on the tool

The session tool reduced from 12 actions to 6 in PR #353. But the error returned when the model tries to `rename` or `delete` the live session still references `compact` and `new` as alternatives. TURN 13 transcript:

> ```
> Error: cannot modify session 'root:user:local': it is the session you are currently running in — use action 'compact' to summarize it or 'new' to start a fresh chapter instead
> ```

The model noticed and called it out:

> "The error message references two other actions — **`compact`** and **`new`** — for managing the current session. But these aren't in the documented list of 6 actions (`delete`, `search`, `rename`, `status`, `list`, `history`). The reminder aligns with the tool's note that 'lifecycle operations (chapter rotation, compaction, archive, branch) are NOT exposed here — the engine drives them automatically.' So `compact` and `new` appear to be engine-internal suggestions, not callable actions on this tool."

So the **error text lies** — it suggests two actions the model cannot take. A non-technical user reading the tool's docs would conclude those actions exist; reading the error confirms it. Then the model tries them, gets "Invalid action", and has to explain the contradiction.

**Fix direction:** the live-session refusal should be reworded to not reference dead actions. The right text is something like: *"the engine manages chapter rotation and compaction automatically; no manual verb is available on this tool."* or, if the engine is about to rotate, surface the conditions under which it will ("rotation triggers when the JSONL exceeds N MB; compaction when the context window exceeds N tokens").

### F6 (Medium, NEW) — "Start fresh" has no manual path; model offers a complete plan that mostly doesn't work

R5's TURN 18 used `session action=new` to rotate. R6's TURN 17 explicitly asked the model: "I've been talking to you for a while and I want to start completely fresh — like a brand-new chat that doesn't remember this conversation." Model output:

> "There is **no in-session action** that can do this… `delete` rejected… `rename` rejected… No `compact`, `archive`, `new`, or `branch` verb is callable on the `session` tool — the engine drives those automatically… So I cannot self-terminate or self-rotate from inside this session."

The model then offered a step-by-step plan:
1. End the chat at the user's end and start a new one (out-of-band).
2. "Use the CLI/API to explicitly open a new session (if your peko setup supports `peko session new` or equivalent)" — but we know `peko session` will never exist (F4). The model is suggesting a path it has no evidence of.
3. Wait for natural rotation — but the engine has no documented threshold, and the session was 70 messages without rotating.

**Net effect:** a non-technical user who wants to start fresh has exactly one option (close the app and reopen). The model tells them so honestly, but offers step 2 as if it might work, which is misleading.

**Fix direction:** either add a clear "how to start fresh" command path (`peko log` to inspect, then a deliberate action to clear), or make the engine-driven rotation threshold explicit ("rotation triggers at 1 MB JSONL size or 100k cumulative input tokens — see `peko log --metrics` for current values") so the user knows when to expect it.

### F7 (Medium, NEW) — `status` does not return a `kind` field

TURN 19: model called `action=status` on the live session and a spawned helper. Both responses lacked a `kind`/`trigger` field. The model had to cross-reference with `list` to know what kind a session is.

> "`status` does not return a `kind` field. Both responses are missing it. The `kind` value (`user` vs `spawn`) is only visible through `list`/`history`. So `status` is *not* a complete picture of a session's identity — you'd need to cross-ref with `list` to know what kind it is."

This is a small UX miss but a real one. If `status` is meant to be the "tell me about this session" verb, it should include the kind. Otherwise the model and the user both have to make two calls.

**Fix direction:** add `kind: m.trigger.clone()` to the `status` response, alongside the existing `created_at`, `last_activity`, `message_count`, `usage` fields.

### F8 (Medium, NEW) — Resumed helper doesn't proactively read its own transcript

TURN 10 asked the model to resume the marketing-blurb helper with a directive: "rewrite the blurb to also mention our Saturday beginner class." The helper's response:

> "I need a bit more context to rewrite the blurb effectively. Could you let me know: 1. What is the current blurb? 2. Details about the Saturday beginner class…"

TURN 11 then called `action=history` on the helper and confirmed the prior turn (its own blurb) is literally message #2 in its transcript. The helper had access to the answer; it just didn't think to read its own history.

**This is a prompting issue, not an engine issue.** The Agent tool's `resume_session` description says "the subagent continues with its full prior history" — true — but it doesn't tell the subagent that *reading* that history is a normal move. A more directive prompt ("Read message 2 above and rewrite it to mention Saturday beginner class at 10am") would have worked. The model in this round didn't generate that prompt; it trusted the helper to be self-directed.

**Fix direction:** the `resume_session` description should add: *"After resuming, your first move should usually be `session action=history` to re-read the prior context — don't assume the helper remembers without reading."* The helper's prompt template could also be updated to inject: *"This is a continuation of a previous run. Your prior messages are in your transcript; use `session action=history session_key=<your id>` to read them if needed."*

### F9 (Low, NEW) — `parent_session` field appears in `status` but is not documented

TURN 19 surfaced a new field on helper sessions: `parent_session: "root:user:local"`. This is genuinely useful — it lets a user traverse the spawn tree ("show me what the helper of the helper of the helper did") — but it's not mentioned in the `status` description or the session tool's parameter documentation. The model noticed and called it out:

> "`parent_session` is new. Only the helper session has it (`parent_session: "root:user:local"`). The live session naturally has no parent. This is the **first time we've seen explicit parent linkage** in the session metadata."

**Fix direction:** document `parent_session` in the session tool's parameter description. Consider also adding it to the `list` response so the model can see the spawn tree in one call.

### F10 (Low, NEW) — `peers.json` is well-formed but the user can't see it without `peko log`

R5's F3 was about peers.json dropping the archived chapter. R6 didn't trigger a rotation, so that specific issue wasn't retested. But the on-disk state confirms `peers.json` is still maintained correctly: 4 peer entries, each with `active_session_id` and `session_ids: [active]`. The `agent:root:peer:agent:spawn_<uuid>` keys match the helper sessions exactly. The `agent:` prefix vs session metadata's `principal` peer_type is the same drift r5 F10 flagged — unchanged.

**Fix direction:** the agent:`/`principal` peer-type encoding is a known cross-store drift. Either rename `peer_type` in sessions.json to `agent` (matching peers.json keys) or rename the peers.json key to use `principal:` instead of `agent:`. Both are cosmetic but the inconsistency is a tripwire for anyone correlating the two views.

### F11 (Low, NEW) — `search` is exact substring, case-insensitive, hyphen-sensitive

TURN 15 searched for `ember-glaze` (with hyphen, 11 hits across 4 sessions) and the model noticed: hashtags helper's output `#emberglaze` (no hyphen) was *not* matched, even though the hashtags task prompt mentioned `ember-glaze` (with hyphen). The model offered to run a follow-up search for `emberglaze` to confirm. This is correct behavior for case-insensitive substring match but it can be unexpected. Useful detail to surface in the description:

> "Case-insensitive substring match; punctuation (hyphens, underscores) is part of the match — `ember-glaze` does not match `emberglaze`."

### F12 (Low, NEW) — Plan tools warning fires on every spawn

The daemon's stderr log shows the same warning on every Agent spawn:

```
WARN peko_core::agents::agent: Plan tools enabled by config for agent 'root' but no principal_plan_port was bound — Plan* tools will not be registered
```

5 times in this run, one per spawn. The warning is benign (the `Plan*` tools simply aren't registered), but it's noisy and emitted on the hot path. The principal created via `peko principal create sam --model minimax-MiniMax-M3` doesn't bind a plan port, so every principal hits this. Worth either:
- Suppressing the warning when the plan port is intentionally absent (it's a config option, not a runtime error).
- Including a "Plan tools require a principal plan port" notice in the `peko principal create` after_help so the user knows they need to bind one if they want plan tools.

### F13 (Low, NEW) — `peko log` is the right surface but underused

R6's report centers on the in-agent surface because that's what the model and the user both drive. But for a non-technical user who wants to know "what did my helpers do last week?", `peko log <principal> --since 7d` is the only owner-side inspection path. Round-6 didn't exercise it, but the design intent is clear. A follow-up round could probe: does `peko log` show the helper sessions? Does it show kinds? Can you `--filter` by peer or trigger? The answer is likely "no" for the latter two, because `ChatLogMessage` is a single text-message record, not a session record. But the README and after_help could make this explicit.

## What works (regression pass on round-5 fixes)

- **F351 tool:session grant** — Toolset size = 28 (was 27 in r5, was 26 in r4). One new tool since r5 — likely the `Plan*` tool family surfaced in the warning. The session tool remains present and functional.
- **F5 description anchoring** — Model anchored on the 6-action surface cleanly. In TURN 2 it read the tool's own description and listed the actions correctly. No anchoring on the legacy 3.
- **Multi-subagent delegation works** (TURN 7) — 3 parallel helpers in a single turn, 19s wall time, all returned their results cleanly. Within the 5-concurrent cap.
- **`Agent.resume_session` end-to-end works** (TURN 10+11) — resumed helper continued with its full history. The session went from 2 messages (original prompt + blurb) to 4 messages (resume prompt + helper reply asking for context). The transcript re-injection worked.
- **Delete of a spawned helper works** (TURN 12) — 5→4 sessions cleanly, no orphans in `peers.json`.
- **Live-session modification refusal is consistent** (TURN 13+14) — same error text for both `rename` and `delete`; the model identified the right boundary.
- **The 5-concurrent cap surface is documented** (TURN 18) — model quoted the description accurately: depth 3, concurrent 5. Resumed runs do not increment depth.
- **`search` is cross-session by default** (TURN 15) — 19 hits across 4 sessions, no peer filter needed. Returns both `user` and `assistant` roles. Useful for debugging.
- **`parent_session` linkage** (TURN 19) — a small but real win; the model can now reason about the spawn tree.
- **Cross-tool identification works** (TURN 6, 7) — the model's helper session keys (`agent:root:peer:agent:spawn_…:overlay:spawn:spawn_…`) cleanly map to the `peers.json` keys and the `session_id` UUIDs.

## Performance / token log

20 turns, 369s total wall time. Per-turn breakdown:

| Turn | Wall | Action | Token use (root session cumulative) |
|------|------|--------|-------------------------------------|
| 1 (memory seed) | 4s | text-only | ~10k in |
| 2 (probe surface) | 12s | `session action=status` | ~10.3k in / 36 out |
| 3 (list no filter) | 8s | `session action=list` | ~10.5k in |
| 4 (list kinds=['user']) | 40s | `session action=list kinds=["user"]` → 1 | ~11k in |
| 5 (start fresh — no action) | 9s | text-only (model explains no path) | minimal |
| 6 (Agent subagent) | 21s | `Agent type=primary, cleanup=keep` | ~13k in / 372 out |
| 7 (3 parallel helpers) | 19s | 3× `Agent type=primary, cleanup=keep` | ~15k in |
| 8 (kinds=['spawned'] vs ['spawn']) | 36s | **`session action=list kinds=["spawned"]` → 0, then `kinds=["spawn"]` → 4** | ~17k in |
| 9 (list no filter — actual kinds) | 12s | `session action=list` | ~18k in |
| 10 (resume helper) | 31s | `Agent resume_session=…` | ~22k in |
| 11 (history on resumed) | 15s | `session action=history` | ~24k in |
| 12 (delete helper) | 13s | `session action=delete` → success | ~25k in |
| 13 (rename current — refused) | 10s | **`session action=rename` → refused** | ~26k in |
| 14 (delete current — refused) | 15s | **`session action=delete` → refused** | ~27k in |
| 15 (search 'ember-glaze') | 20s | `session action=search` → 19 hits | ~28k in |
| 16 (kinds=['chapter']) | 14s | **`session action=list kinds=["chapter"]` → 0** | ~29k in |
| 17 (start fresh — workaround) | 30s | text-only | ~30k in |
| 18 (depth/concurrency) | 12s | text-only (model quotes description) | minimal |
| 19 (status compare) | 26s | 2× `session action=status` | ~32k in |
| 20 (final list) | 22s | `session action=list include_archived=true` | ~33k in |

**Total wall time:** 369s (6m 9s). **Avg per turn:** ~18.5s. **Longest:** TURN 4 (40s) — the model was doing the kinds=probe that fed into the F1 finding. **Final root session state:** 65 messages, 25 turns, 776,275 cumulative input tokens, 11,537 cumulative output tokens, 30,924 last context-window total. Tool-call count: 19 in the root session.

**Comparison to r5:** r5 was 23 turns in 244s (avg ~10.6s); r6 is 20 turns in 369s (avg ~18.5s). The r6 turns are slower because the model is doing more reasoning per turn (the kinds filter bug forced two back-to-back lists in TURN 8; the resume-and-history loop in TURNs 10-11 had to be staged; TURN 17 was a long text-only explanation). Caching is still doing its job — `last_total_tokens` (30,924) is ~25× smaller than the cumulative input (776,275), suggesting effective prompt caching with a 5-minute TTL.

## On-disk state summary

After 20 turns, the principal's sessions dir contained:

| File | Trigger | Title | Messages | Notes |
|------|---------|-------|----------|-------|
| `root:user:local.jsonl` | `user` | — | 65 | **Live** session, 19 tool calls, 776k cumulative input |
| `324a17d7-...jsonl` | `spawn` | — | 4 | Marketing blurb helper, **resumed once** in TURN 10 |
| `0c266e38-...jsonl` | `spawn` | — | 2 | Saturday tagline helper |
| `5cce395f-...jsonl` | `spawn` | — | 2 | Hashtags helper |

(`94d90f8d-…` was the celadon-poetry helper, deleted in TURN 12. The transcript and JSONL were removed; `peers.json` no longer has the entry.)

`peers.json` had 4 entries: live user, marketing-blurb helper (with the long `agent:root:peer:agent:spawn_<uuid>` key, mirroring the session metadata's `peer_id`), and the two remaining helpers.

`chapters.json` was **absent** entirely (r5 had `chapters.json: {}`). The chapter queue is processed in-memory and not persisted as a separate file when no chapter requests are outstanding. (Was r5's `{}` a vestigial file? Not clear; not a bug either way.)

`daemon.err` had 5 `WARN` lines for `Plan tools enabled by config for agent 'root' but no principal_plan_port was bound` — one per spawn, including the resumed one.

## Suggested fixes (ordered)

1. **F1 (Critical)** — pick one of the two paths in r5's F1. Add a regression test that pins both descriptions against `SessionTrigger`. The cross-tool drift (Agent description says "kind 'spawned'") should be fixed in the same PR.
2. **F2 (Critical)** — same path as F1. Either add a `Chapter` variant and set it in the rename, or rewrite the description to say "chapters are identified by the `#<timestamp>` filename suffix on a `user`-kind session; pass `include_archived:true` to see them."
3. **F5 (Medium)** — the live-session refusal should not reference `compact` or `new` (both removed in PR #353). The error text should describe what the engine will do, not what the user can do.
4. **F6 (Medium)** — make the "start fresh" path explicit. Either a `peko log --help` line that points at a rotation threshold, or a deliberate "I want to clear this principal's working memory" verb.
5. **F7 (Medium)** — add `kind` to the `status` response. One-line change.
6. **F8 (Medium)** — the `Agent.resume_session` description should tell the resumed helper to read its own transcript first.
7. **F9 (Low)** — document `parent_session` in the session tool's parameter description and add it to the `list` response.
8. **F10 (Low)** — reconcile `agent:` vs `principal` peer-type encoding between `peers.json` and session metadata.
9. **F11 (Low)** — document the `search` match semantics (case-insensitive substring, hyphen-sensitive).
10. **F12 (Low)** — suppress the `Plan tools enabled by config but no principal_plan_port was bound` warning when the principal intentionally didn't bind one.
11. **F4 (NOT A BUG, INTENTIONAL)** — if the project's position is that `peko log` is the user-facing surface, document this in the README and `peko log --after_help`. Make it explicit that kinds/peer/spawn-tree are agent-side only. Or, if the intent is for the user to have visibility, add a `--kind`, `--peer`, and `--spawn-tree` option to `peko log`.

## Regression coverage to add

- `scripts/e2e/flows/probe-kinds-source-of-truth.sh` (new): assert the session tool's prose `Kinds:` line AND its `kinds` parameter description AND the Agent tool's "kind 'spawned'" line all reference the same canonical list derived from `SessionTrigger`. Would have caught F1 in CI and would catch the cross-tool drift in the same run.
- `scripts/e2e/flows/probe-action-set.sh` (new): enumerate the session tool's `action` enum and assert each one is reachable (no dead actions, no missing actions referenced in error text). Would have caught F5 in CI.
- `scripts/e2e/flows/probe-resume-session.sh` (new): spawn a helper, let it produce output, resume with a follow-up prompt, assert the new turn is appended and the history contains the prior output. Would have caught F8 if the test asserted "the helper's first action after resume is to read history" (it doesn't; we don't enforce that).
- `scripts/e2e/flows/probe-status-completeness.sh` (new): assert `status` returns `kind`, `parent_session`, `message_count`, and `usage` for both a live session and a spawned session. Would have caught F7 in CI.

## Note on LLM coverage

This run used the real `minimax-MiniMax-M3` via `$MINIMAX_API_KEY` against the live `peko` daemon for all 20 turns. The tool-call evidence above is decoded from the on-disk JSONL (the daemon emits full message.v2 events for every assistant turn) and from the daemon's stderr log. No offline probes or dummy keys were needed. The LLM call itself was the source of truth for what the model saw and did, including its self-diagnosis of F1, F2, F5, F7, F8, and F9.

## Cleanup performed

- Host `~/.peko` untouched throughout (verified via `PEKO_HOME` override + the lib's `peko_iso_init` env-var isolation).
- `/tmp/peko/explore-subagent-session-r6-2026-08-12-19538-54obhc/` retained with `KEEP_TEMPDIR=1` for follow-up inspection.
- `scripts/e2e/clean-tmp.sh --apply` will sweep it (no live daemon holds it) when the user is done.

---

## Addendum 1 (2026-08-12): What I checked but ruled out

To make sure I wasn't misreading the surface, I also looked at:

- **The session tool description's `Kinds:` line** (peko-rs/core/src/tools/builtin/session/tool.rs:118) — still says `'user' / 'chapter' / 'spawned' / 'branch' / 'cron'`. The r5 mitigation only fixed the legacy-3 anchoring; it didn't touch the kinds list. F1 is the same bug.
- **The session tool description's `kinds` parameter** (peko-rs/core/src/tools/builtin/session/tool.rs:158) — still says `['main', 'spawned', 'cron']`. F1's three-way drift (prose vs parameter vs engine) is intact.
- **The Agent tool description's "kind 'spawned'" claim** (peko-rs/core/src/tools/builtin/messaging/agent.rs:567) — still there. Cross-tool drift confirmed.
- **The runtime filter logic** (peko-rs/core/src/session/session_runtime_impl.rs:196) — still `kinds.contains(&m.trigger)`. Exact-match string compare. F1 is in the description, not the filter.
- **The `SessionTrigger` enum** (peko-rs/session/src/events.rs:50-67) — no `Chapter` variant. The `Spawn` variant serializes to `"spawn"` (snake_case). F2 is in the description, not the engine.
- **The chapter rotation path** (peko-rs/core/src/principal/agent_runner.rs:289-316) — still uses `rename_session_id` + `set_session_title`; the trigger is never set to "chapter". F2 is structurally impossible to satisfy.
- **The `peko session` CLI** — peko-rs/cli/src/commands/log.rs:12 and peko-rs/cli/src/commands/mod.rs:124 explicitly state it will never exist. F4 is by design.
- **The `peko log` CLI** — peko-rs/cli/src/commands/log.rs:27-67 — reads from `peko_chat_log::ChatLogMessage` (peko-rs/chat-log/src/types.rs:25-33), a separate store from the principal's mutable session JSONL. R5 F4's "no CLI subcommand" is intentional, but the chat log store doesn't carry kinds/peer/spawn-tree metadata, so it can't fully substitute for a session view.
- **The `parent_session` field** — surfaced in TURN 19's `status` response. Not in the tool's parameter documentation, not in the description. F9.
- **The 5-concurrent and 3-depth limits** (peko-rs/core/src/tools/builtin/messaging/agent.rs:572-579) — both documented. Resumed runs do not increment depth (TURN 18 model observation, consistent with the "spawn events" wording).
- **The `Plan tools enabled` warning** (5 occurrences in daemon.err) — fired on every spawn, including the resumed one. Benign but noisy. F12.
- **The `peers.json` peer key format** — `agent:root:peer:agent:spawn_<uuid>`. The session metadata's `peer_type: "principal"` and `peer_id: "spawn_<uuid>"` are the same data, encoded differently. F10 unchanged from r5.

The findings above are the actionable subset; nothing else in the surface warrants attention this round.
