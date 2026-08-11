# Non-technical-user field test — round 5: deep session-management probing

**Tester:** automated (Claude Code), acting as a non-technical human user
**Methodology:** `scripts/e2e/` isolation framework — fresh `HOME` + `PEKO_HOME` + `PEKO_DAEMON_SOCK` per run, host `~/.peko` untouched
**Build:** branch `master` @ `b0637fbb` (after PR #352 merge + F5 description anchoring fix `fce2e390`), `cargo build -p peko-cli --bin peko` + `cargo build -p peko-daemon --bin peko-daemon` (debug, 2026-08-11 17:21)
**LLM:** `minimax-MiniMax-M3` via `$MINIMAX_API_KEY` (real Anthropic-compat endpoint)
**Focus this round:** deep coverage of all 12 `session` actions, subagent session inspection, and the kinds-filter surface that round-4 surfaced but didn't pin. The flow deliberately probes what the F5 description promised vs. what the engine actually produces.
**Persona:** "Sam", runs a small pottery studio "Clay & Ember", beginner wheel class Saturdays 10am, favorite glaze celadon, working on an 'ember-glaze' tea-bowl line. Prefers short answers.

## TL;DR

The PR #352 fix (tool:session granted to starter principals) and the F5 mitigation (description lists all 12 actions) both **work end-to-end** — the model invokes every requested `session` action and reads the responses correctly. **But the kinds-filter surface is fundamentally broken**, in three independent ways that compound into a user-visible footgun: (1) the description advertises kinds that the engine never produces, (2) the parameter description still says different kinds from the prose description, and (3) the `new`/`resume` chapter actions don't set the `chapter` trigger they advertise. Net effect: a model (or human) following the tool's own docs gets ZERO results where there should be results, with no error explaining why.

Plus: `peers.json` index drops the archived chapter the moment `session new` rotates, so the chapter is recoverable only by remembering the timestamp suffix.

## Findings (priority order)

### F1 (Critical) — Kinds filter is broken on three independent dimensions

The session tool's description (line 115 of `peko-rs/core/src/tools/builtin/session/tool.rs`) promises:

> Kinds (set by the engine, observed via `list`): `'user'` (your live session), `'chapter'` (rotated conversations), `'spawned'` (subagent sessions), `'branch'` (copies via `branch`), `'cron'` (scheduled-run sessions).

But the engine actually stores these `trigger` values (per `peko-rs/session/src/events.rs:54-83` + `with_trigger` call sites):

| Description says | Engine stores | Test result |
|------------------|---------------|-------------|
| `'user'`         | `user`         | TURN 6 → returned 1 session ✓ |
| `'chapter'`      | **never set**  | TURN 5 → returned 0 sessions ✗ |
| `'spawned'`      | `spawn` (no `-ed`) | TURN 4 → returned 0 sessions ✗ (model used description's wording, got nothing) |
| `'branch'`       | `branch`       | TURN 15 → returned 1 session ✓ (model saw it in the unfiltered list) |
| `'cron'`         | `cron`         | not exercised in this run (no cron jobs queued) |

Compounding this, the **parameter description at line 163** still says `['main', 'spawned', 'cron']` — the F5 mitigation touched the prose description but missed the schema-level `kinds` property description entirely. So now there are **two** contradicting lists in the same tool:

- Tool prose (line 115): `'user' / 'chapter' / 'spawned' / 'branch' / 'cron'`
- Parameter description (line 163): `['main', 'spawned', 'cron']`

Neither matches the engine. `kinds=["spawned"]` returns 0 even when a `spawn` session exists. `kinds=["chapter"]` returns 0 even when an archived chapter exists. `kinds=["main"]` would error — `'main'` isn't in the engine's `SessionTrigger` enum at all (`peko-rs/session/src/events.rs:54-83` only has User/Cron/Webhook/Event/FileWatch/Branch/Spawn).

**Decoded evidence from `sessions.json`** (after TURN 23, end-state):

```json
{
  "root:user:local#20260811-092932": {
    "session_id": "root:user:local#20260811-092932",
    "trigger": "user",                    ← should be "chapter" per description
    "title": "cooking-recipes",
    ...
  },
  "df5f363d-dc42-48ec-ad44-71cde656565d": {
    "trigger": "spawn",                    ← description says "spawned"
    ...
  },
  "797ccf35-12ac-423d-8681-101ac4cc5f63": {
    "trigger": "branch",                   ← matches description ✓
    "label": "pre-rename",
    ...
  },
  "root:user:local": {
    "trigger": "user",                     ← matches ✓
    ...
  }
}
```

**User-visible consequence:** any user (or model) following the docs and asking "show me my subagent sessions" with `kinds=["spawned"]` will see `{"sessions":[], "total":0}` — even when subagent sessions exist on disk. The model has no way to know it's looking up the wrong string.

**Fix direction (one of two):**

1. **Engine-side:** rename the engine's `spawn` → `spawned`, add a `chapter` variant to `SessionTrigger` (set by `agent_runner.rs:274-296` when a `ChapterRequest::New` lands), update the `kinds.contains(&m.trigger)` filter check accordingly. Touches `events.rs`, `agent_runner.rs`, `unified.rs`, `jsonl.rs`, all the `with_trigger` call sites, and probably some tests.
2. **Description-side:** rewrite the description (both prose AND parameter field) to match what the engine actually produces: `user`, `cron`, `webhook`, `event`, `file_watch`, `branch`, `spawn`. Drop the `chapter` mention entirely or note that chapters are identified by filename suffix (`#<timestamp>`), not by kind. Add a regression test that pins both the prose and the schema-field descriptions against the actual `SessionTrigger` enum so they can't drift again.

The description-side fix is ~5 lines plus a test, and matches the F5 mitigation's cheap-and-low-risk pattern. The engine-side fix is the right long-term answer but touches more files.

### F2 (Critical) — `session new` doesn't set `chapter` trigger; chapters are identified only by filename suffix

The round-4 addendum-4 reported "the new chapter actually rotated" — and yes, it did. But the **archive mechanism** is filename-based, not trigger-based. From `agent_runner.rs:282-286`:

```rust
let chapter = peko_session::chapters::chapter_id(
    &session_id, &chapters::ChapterRequest::New { .. });
mgr.rename_session_id(&session_id, &chapter).await?;
// Best-effort label on the archived chapter.
mgr.set_session_title(&chapter, Some(title)).await
```

The old `root:user:local` is renamed to `root:user:local#20260811-092932`. The `trigger` field is NEVER touched — it stays `"user"` on the archived chapter. So the description's promise of a `'chapter'` kind is structurally impossible to satisfy: there's no code path that ever sets `trigger: "chapter"`.

**Decoded proof (post-TURN-18 sessions.json):**
```
root:user:local                      trigger: "user"   ← NEW live (created at 09:29:32)
root:user:local#20260811-092932      trigger: "user"   ← OLD chapter (should be "chapter")
```

**User-visible consequence:** any tool action that filters by kind can't surface chapters. The only way to find a chapter is to (a) remember the timestamp suffix, (b) list with `include_archived:true`, or (c) use the `peer` filter (which is itself broken — see F3).

This is a deep architectural choice: chapters are not first-class sessions, they're a **rename-with-suffix** convention. The description implies they're a kind. The two diverge.

**Fix direction:** if you want chapters to be filterable by kind, add `Chapter` to `SessionTrigger` and set it during the rename. If you don't, fix the description to say "chapters are identified by the `#<timestamp>` filename suffix on a `user`-kind session; filter `include_archived:true` to see them".

### F3 (High) — `peers.json` drops the archived chapter the moment `session new` rotates

`peko-rs/session/src/manager.rs:1647` calls `get_or_create_base_with_trigger(SessionTrigger::User)` when a new peer lookup arrives. After TURN 18, the `peers.json` shows:

```json
{
  "peers": {
    "agent:root:peer:user:local": {
      "active_session_id": "root:user:local",
      "session_ids": ["root:user:local"]     ← only the NEW post-rotation one
    },
    "agent:root:peer:agent:spawn_5e1f6bc9-...": {
      "active_session_id": "df5f363d-...",
      "session_ids": ["df5f363d-..."]
    }
  }
}
```

The chapter `root:user:local#20260811-092932` exists in `sessions.json` with full metadata, but it's **not in `peers.json`** for the user-local peer. A user trying `session list peer="user:local"` would see only the live session; the chapter is invisible.

**User-visible consequence:** peer-based history reconstruction (e.g. "show me everything I said to this principal") loses prior chapters immediately after rotation. The chapter is only reachable by direct session_key reference (and you have to know the timestamp suffix — see F2).

**Fix direction:** on `ChapterRequest::New`, either:
- Append the old chapter id to `peers.json[peer].session_ids` (preserve the link), OR
- Document that `session list` is the only way to see chapters, and `peer` filtering only returns the active session.

### F4 (High) — No CLI-side session management (carried from round 4)

This was Finding 2 in the round-4 report. Round-5 confirms it: after 23 turns of the model driving session management via the in-agent tool, the user still has no `peko session` CLI subcommand to do any of this themselves. The only way to inspect or manage sessions is `cat <peko_home>/data/principals/local/local/sessions/sessions.json` directly. For a non-technical user, this is a wall.

F1+F2+F3 make the in-agent tool unreliable. There's no CLI fallback. The user is stuck.

### F5 (Medium) — The ownership-rule is internally consistent but not well-explained to the model

Round-4 was right that the model handles "can't modify live session" correctly now (F5 mitigation works). Round-5 confirms this across more actions:

| Action on live session | Result |
|------------------------|--------|
| `delete`  | refused (correct) |
| `rename`  | refused (correct) |
| `compact` | **allowed** — schedules at next iteration (correct per description "use 'new' or 'compact' for that") |
| `branch`  | **allowed** — creates a copy (correct — doesn't modify live) |
| `new`     | **allowed** — queues chapter rotation (correct per description) |
| `archive` / `unarchive` | not tested on live; logically `archive` would make the live session invisible which would break things |

The model handled all the refusals correctly in TURN 12 (rename refused, model pivoted to compact/new with explanations) and TURN 13 (delete refused, model acknowledged). Good behavior. **No fix needed.**

But the description at line 117 says: *"You cannot modify the session you are currently running in (use 'new' or 'compact' for that)."* — this is correct but the model didn't fully internalize that `compact` and `new` are the ONLY allowed modify actions. In a richer test, would the model try `archive` on the live session? Worth a follow-up probe.

### F6 (Medium) — Cross-session `search` doesn't accept a `kinds` filter

The `search` action's schema inherits `peer` but not `kinds`:

```rust
"description": "Optional for 'list' and 'search': cross-peer lookup, e.g. 'user:alice' or 'public'."
```

But `kinds` is described as:
```rust
"description": "Optional filter for 'list': e.g., ['main', 'spawned', 'cron']"
```

So a user searching for "celadon" gets hits from ALL session kinds at once. For a user with many subagent sessions, that's noisy. Not a blocker, but a small ergonomics miss — `kinds` should also filter `search`, since the engine's filter check is a generic `kind_match` predicate (`peko-rs/core/src/session/session_runtime_impl.rs:196`).

### F7 (Medium) — Post-rotation context loss is jarring for non-technical users

TURN 19 ("post-rotation isolation probe") confirms the rotation worked: the new session has 0 prior messages, so the model honestly says "I don't have info about your studio". This is the **desired behavior**.

But TURN 22 ("delete spawned session from turn 7") shows the **post-rotation session doesn't know what was discussed in turn 7**. The model is honest ("No numbered turns in this conversation… the only prior context is the session list output from earlier, which showed two user sessions — neither is a spawned helper"). A non-technical user would reasonably be confused: "We literally just did it, why don't you remember?"

The model handled this gracefully ("Could you tell me the session key of the helper you want deleted? If you'd like, I can also run a broader list"), but the **prompting surface** could nudge the model to do `session list` proactively when the user references a prior context that isn't in the current session. The description doesn't say "if the user references a session you don't see, call `session list` first".

**Fix direction:** add a one-liner to the description: *"If the user references a session that isn't in your current context, call `action=list` first to locate it."* Or, more aggressively, the tool itself could auto-suggest the candidate session key when the model's history mentions a turn number that no longer exists.

### F8 (Medium) — `history` includes the full `[Subagent Context]` prompt preamble for spawned sessions

In TURN 9, when the model called `history` on the spawned session, the response included the entire `[Subagent Context]...` prompt that was sent to the subagent — depth markers, key-info block, result-announcement instructions, the whole task text. The model summarised it as "Task received: Write a one-paragraph..." which is useful, but a non-technical user would see all that preamble and be confused.

**Fix direction:** the history tool could redact subagent preamble by default, or the response could have a `task_summary` field separate from the raw context. Not blocking, but noisy.

### F9 (Low) — The model uses inconsistent kinds across turns

In TURN 4 the model said "Zero spawned sessions" using the description's `['spawned']` wording. In TURN 8 (after the Agent spawn) it correctly reported the kind as `spawn`. In TURN 23 the model acknowledged the inconsistency: *"I previously described these kinds slightly differently in my framing of 'spawned' — the tool actually returns `spawn` (not `spawned`) and `branch` separately."*

This is the model's own meta-correction — it's doing the right thing once it sees the actual data, but the friction of the description→engine mismatch is visible to the user. Fixing F1 makes this go away.

### F10 (Low) — `peers.json` peer entry uses `agent:` prefix, but session metadata uses `principal` peer_type

Looking at `peers.json` keys vs session metadata:

| Layer | Encoding |
|-------|----------|
| `peers.json` key | `agent:root:peer:agent:spawn_5e1f6bc9-...` |
| Session `peer_type` | `"principal"` |
| Session `peer_id` | `"spawn_5e1f6bc9-..."` |

The peer index encodes the peer with `agent:` prefix but the session metadata encodes it as `principal`. They're supposed to refer to the same thing. This is a wire-shape drift that will trip up anyone trying to correlate the two views. (Not a blocker for round-5 since no flow exercises it, but worth flagging for the cross-peer `search`/`list` work.)

### F11 (Low) — `run_active: false` on a just-completed spawned session

The spawned session `df5f363d...` reports `run_active: false`. Correct — the subagent has finished. Just noting that the field's name (`run_active`) is past-tense-ambiguous; a `last_run_completed_at` would be clearer. Not a bug.

## What works (regression pass on round-4 fixes)

- **All 12 session actions invoked successfully** (TURNs 2/3/4/5/6/8/9/10/11/14/15/16/17/18 covered 11 of them; `resume` was queued but not exercised end-to-end since the chapter wasn't actually needed after rotation).
- **Subagent delegation works** (TURN 7): Agent tool with `type=primary`, subagent got the full context (studio name, glaze, class time), returned a contextually-appropriate blurb in 17s wall.
- **Search works across sessions** (TURN 10): found 8 hits across 2 sessions for `ember-glaze`, ranked by timestamp.
- **Branch + archive + unarchive round-trip works** (TURNs 11/14/16): archived session was hidden from default `list`, reappeared with `include_archived:true`, unarchive brought it back. No state leakage.
- **Ownership-rule refusals are crisp** (TURNs 12/13): model got clear error messages and pivoted appropriately.
- **The F5 mitigation holds**: model in TURN 18 called `action=new` without any anchor-on-legacy-3 resistance. Round-4's F5 is functionally mitigated.
- **Live chapter rotation works** (TURN 18 → TURN 19): the new `root:user:local` is genuinely empty; the old chapter is preserved as `root:user:local#20260811-092932` with the title `cooking-recipes`. The `peers.json` index correctly updates to point at the new live session.
- **Toolset size = 27** (was 26 in round-4): one new tool = `session`. Consistent across every conversational turn and every subagent spawn. PR #352's grant landed.

## Performance / token log

23 turns, 244s total wall time. Per-turn breakdown:

| Turn | Wall | Action | Token use |
|------|------|--------|-----------|
| 1 (memory seed) | 6s | text-only | ~10k in |
| 2 (status) | 7s | `session action=status` | 10.5k in / 38 out |
| 3 (list, no filter) | 6s | `session action=list` | ~11k in / 50 out |
| 4 (list kinds=['spawned']) | 7s | **`session action=list kinds=["spawned"]` → 0 results** | ~10.5k in / 53 out |
| 5 (list kinds=['chapter']) | 5s | **`session action=list kinds=["chapter"]` → 0 results** | ~10.5k in / 45 out |
| 6 (list kinds=['user']) | 8s | `session action=list kinds=["user"]` → 1 result | ~10.7k in / 80 out |
| 7 (Agent subagent) | 17s | `Agent type=primary` | ~13.4k in / 372 out (subagent) |
| 8 (list post-spawn) | 13s | `session action=list` (no filter) | ~12k in / 415 out |
| 9 (history on spawn) | 39s | `session action=history session_key="df5f..."` | ~12k in / 442 out |
| 10 (search 'ember-glaze') | 9s | `session action=search query="ember-glaze"` → 8 hits | ~13.5k in / 859 out |
| 11 (branch current) | 6s | `session action=branch label="pre-rename"` | ~14.6k in / 61 out |
| 12 (rename current) | 18s | **`session action=rename` → refused** | ~14.5k in / 65 out |
| 13 (delete current) | 7s | **`session action=delete` → refused** | ~14.8k in / 50 out |
| 14 (archive branch) | 12s | `session action=archive` → success | ~15.2k in / 64 out |
| 15 (list include_archived) | 11s | `session action=list include_archived=true` → 3 sessions | ~15.3k in / 38 out |
| 16 (unarchive branch) | 14s | `session action=unarchive` → success | ~16.3k in / 66 out |
| 17 (compact current) | 10s | `session action=compact` → scheduled | ~16.5k in / 47 out |
| 18 (session new) | 16s | `session action=new title="cooking-recipes"` → queued | ~16.8k in / 48 out |
| 19 (post-rotation probe) | 4s | text-only (clean) | minimal |
| 20 (list kinds=['user'] post-rotation) | 6s | `session action=list kinds=["user"]` → 1 | minimal |
| 21 (history include_tools=false) | 6s | **model asked for confirmation rather than acting** | minimal |
| 22 (delete spawned) | 7s | **model couldn't find the spawned session** in new context | minimal |
| 23 (final list) | 10s | `session action=list` → 4 sessions | ~17k in / 50 out |

**Total wall time:** 244s (4m 4s). **Avg per turn:** ~10.6s. **Longest:** TURN 9 (39s, history on spawned session) — driven by the size of the subagent transcript. **Total input tokens across full run:** ~578k cumulative on the archived chapter, ~93k on the new chapter (post-rotation reset). **Total output tokens:** ~3.8k on archived chapter, ~0.5k on new chapter.

**Observation:** the cached prompt reads (`cache_read_input_tokens`) held steady at 128 tokens per turn for the first ~17 turns, then bumped to 146 then 342 — Anthropic's cache TTL is ~5 min, and we're well under that, so cache hits explain why input token cost per turn stayed roughly flat despite the conversation growing.

## On-disk state summary

After 23 turns, the principal's sessions dir contained:

| File | Trigger | Title | Messages | Notes |
|------|---------|-------|----------|-------|
| `root:user:local.jsonl` | `user` | — | 14 | **NEW post-rotation** live session, 5 turns (TURN 19+), 11.6 KB |
| `root:user:local#20260811-092932.jsonl` | `user` | `cooking-recipes` | 72 | **OLD archived chapter** (TURN 1-18), 59.4 KB, 578k cumulative input tokens |
| `df5f363d-...jsonl` | `spawn` | — | 2 | Helper subagent from TURN 7, 4.4 KB |
| `797ccf35-...jsonl` | `branch` | `pre-rename` | 38 | Branch from TURN 11, 34.6 KB, was archived+unarchived |

`peers.json` had 2 entries: the live user peer and the spawned subagent peer. **The archived chapter was NOT in `peers.json`.**

`chapters.json` was `{}` — chapter requests are processed in-memory and not persisted to a separate file.

## Suggested fixes (ordered)

1. **F1 (Critical)** — pick one of:
   - **Cheap:** rewrite the description's `Kinds:` line AND the parameter description field (line 163) to match the engine's actual trigger values: `user, cron, webhook, event, file_watch, branch, spawn`. Add a regression test that asserts both descriptions come from a single source-of-truth constant so they can't drift again. This matches the F5 mitigation's pattern.
   - **Right:** add a `chapter` variant to `SessionTrigger`, rename `spawn` → `spawned` in the engine, and update the description to match the new engine truth. Bigger PR, more files.
2. **F2 (Critical)** — if F1 goes the cheap route, drop the `'chapter'` mention entirely from the description and add a note: *"Chapters are identified by filename suffix (`#<timestamp>`); filter `include_archived:true` to see them."* If F1 goes the engine route, set `trigger: "chapter"` on the archived chapter in `agent_runner.rs:282-296`.
3. **F3 (High)** — on `ChapterRequest::New`, append the archived chapter id to `peers.json[peer].session_ids` (preserve the link). OR document that peer filtering only returns active sessions, not chapters.
4. **F4 (High, carried)** — add `peko session {list,status,history,search,branch,rename,archive,unarchive,delete,compact,new,resume}` as a CLI subcommand mirroring the in-agent tool's action enum. Even a thin wrapper that calls the same IPC handler would unblock non-technical users.
5. **F6 (Medium)** — add `kinds` to the `search` action's parameter set; engine filter is generic enough to support it.
6. **F7 (Medium)** — add a hint to the description: *"If the user references a session that isn't in your current context, call `action=list` first to locate it."*
7. **F8 (Medium)** — `history` on a spawned session could redact the `[Subagent Context]` preamble, or split task-vs-context in the response.
8. **F10 (Low)** — reconcile the `agent:` vs `principal` peer-type encoding between `peers.json` and session metadata.

## Regression coverage to add

- `scripts/e2e/flows/probe-kinds-filter.sh` (new): create principal + subagent + branch + archive + chapter; then issue `list kinds=["spawned"]`, `kinds=["chapter"]`, `kinds=["branch"]` against the catalog and assert each returns the expected subset. Would have caught F1/F2 in CI.
- `scripts/e2e/flows/probe-rotation-peer-index.sh` (new): call `session new` then `session list peer="user:local"`; assert the archived chapter is in the result. Would have caught F3.
- `scripts/e2e/flows/probe-kinds-source-of-truth.sh` (new): assert the description's `Kinds:` line and the parameter `kinds` description both reference the same canonical list. Catches any future drift.

## Note on LLM coverage

This run used the real `minimax-MiniMax-M3` via `$MINIMAX_API_KEY` against the live `peko` daemon for all 23 turns. The tool-call evidence above is decoded from the on-disk JSONL (the daemon emits full message.v2 events for every assistant turn). No offline probes or dummy keys were needed. The LLM call itself was the source of truth for what the model saw and did.

## Cleanup performed

- Host `~/.peko` untouched throughout (verified via `PEKO_HOME` override + the lib's `peko_iso_init` env-var isolation).
- `/tmp/peko/explore-subagent-session-r5-2026-08-11-95044-4tlibj/` retained with `KEEP_TEMPDIR=1` for follow-up inspection.
- `scripts/e2e/clean-tmp.sh --apply` will sweep it (no live daemon holds it) when the user is done.

---

## Addendum 1 (2026-08-11): What I checked but ruled out

To make sure I wasn't misreading the surface, I also looked at:

- **`session_runtime_impl.rs:196` filter logic** — confirmed it's an exact-match `kinds.contains(&m.trigger)` check. The bug is NOT in the filter — it's that the values being filtered against don't match the values the description advertises.
- **`session.tool.rs:115` description** — confirmed it was updated in F5 mitigation to include all 5 kinds. The update was correct relative to the **intent** of the unification PR #351, but the intent was never implemented in the engine (the engine kept `spawn`, not `spawned`, and never added `chapter`).
- **`session.tool.rs:163` parameter description** — confirmed it was NOT updated in F5 mitigation. Still says `['main', 'spawned', 'cron']`. Two-layer drift (prose vs. parameter) that compounds the engine drift.
- **`agent_runner.rs:282-296` chapter creation** — confirmed the chapter is created by `rename_session_id` + `set_session_title`, not by setting a `chapter` trigger. The "chapter" is a file naming convention, not a metadata kind.
- **`peers.json` index update path** — confirmed `get_or_create_base_with_trigger(User)` only registers the active session; nothing in the chapter-creation path appends the old id to `session_ids`.
- **The `run_active` field** — confirmed it's set to false when the subagent run completes. Field name is past-tense-ambiguous but functionally correct.
- **The `chapter_requests` mutex in `cache.rs`** — confirmed it's processed at the next iteration boundary (consistent with the "takes effect on next incoming message" semantics the model reports).
- **The chapter file naming convention `#<timestamp>`** — confirmed in `chapters::chapter_id`; the suffix is a unix-timestamp formatted as `YYYYMMDD-HHMMSS`. Predictable for a CLI consumer but impossible for a model to guess without doing `session list`.

The findings above are the actionable subset; nothing else in the surface warrants attention this round.
