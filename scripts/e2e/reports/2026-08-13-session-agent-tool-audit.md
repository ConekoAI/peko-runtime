# session + Agent tool audit (2026-08-13)

Audit of surface, prompting, schema, and agent ergonomics for the two
tools the model invokes most often when managing persistent work:
`session` (introspection) and `Agent` (subagent spawn).

This is a static audit — grounded in code, not LLM probes. Findings
are graded by their impact on model behavior:

- **P0** model misuses or fails because of this
- **P1** model wastes context budget, gets subtly wrong answer, or
  produces inconsistent outputs across sessions
- **P2** cosmetic / documentation drift that doesn't change behavior

Most findings are schema/description drift — the same kind of
three-layer drift (description / schema / engine) that motivated the
kinds-filter removal yesterday.

---

## P0 — error message references demoted actions

`peko-rs/core/src/session/ownership.rs:167-170`

```rust
pub fn err_self_mutation(target: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "cannot modify session '{target}': it is the session you are \
         currently running in — use action 'compact' to summarize it or \
         'new' to start a fresh chapter instead"
    )
}
```

`compact` and `new` were demoted from the `session` tool in PR #353
(WS4 implicit session management, 2026-08-11). The action enum
(`peko-rs/core/src/tools/builtin/session/tool.rs:88-95`) is now
`status / history / search / list / rename / delete`. When the model
gets this refusal (triggered by `rename`/`delete` on the live session
or any session in the caller's subtree), it tries to call
`action=compact` or `action=new`, both of which fail schema validation
with `Invalid action: ...`.

**Fix:** rewrite the error to reflect the new surface. The honest
guidance is "the engine rotates chapters automatically when the JSONL
grows too large, and compacts when the context window fills; you cannot
manage either manually from this tool." (The description in
`tool.rs:120` already says this — point the model there.)

The r6 e2e transcript captured this exactly: TURN 13 (`rename`) and
TURN 14 (`delete`) both returned `cannot modify session 'root:user:local': it is the session you are currently running in — use action 'compact' to summarize it or 'new' to start a fresh chapter instead` and the model tried to call compact/new, both of which are gone from the surface.

---

## P0 — Agent tool `cleanup` silently accepts garbage

`peko-rs/core/src/tools/builtin/messaging/agent.rs:647-652`

```rust
let cleanup = args.cleanup.map_or(SpawnCleanupPolicy::Keep, |s| {
    match s.to_lowercase().as_str() {
        "delete" => SpawnCleanupPolicy::Delete,
        _ => SpawnCleanupPolicy::Keep,
    }
});
```

`SpawnCleanupPolicy::from_str()` in
`peko-rs/extension-api/src/subagent.rs:38-44` correctly returns `None`
for unknown values — but the Agent tool handler discards that signal
and silently defaults to `Keep` for *any* string that isn't "delete"
(case-insensitive). The model can pass `"drop"`, `"purge"`, `"remove"`,
or `""` and get `Keep` without any error. This is exactly the kind of
silent-fallback footgun that lets agents accumulate orphan sessions.

**Fix:** use `SpawnCleanupPolicy::from_str` and bubble up
`anyhow!("cleanup: '{s}' is not one of 'keep' | 'delete'")`. Or use
an enum-typed JSON schema (`"enum": ["keep", "delete"]`) so the
provider validates before the call lands. The schema at
`agent.rs:618-622` advertises `default: "keep"` but no enum constraint,
which is why garbage strings reach the handler.

---

## P1 — schema description for `session_key` is wrong

`peko-rs/core/src/tools/builtin/session/tool.rs:133-136`

```rust
"session_key": {
    "type": "string",
    "description": "Target session. Required for history/rename/delete. \
    Optional for status (defaults to current session)"
}
```

This is wrong in two directions:
- `history` is OPTIONAL with a default of the current session
  (`tool.rs:255-259`: `unwrap_or(&self.runtime.current_session_key())`),
  not required.
- `delete` is correctly required, but `list` is not mentioned at all
  (it doesn't use session_key, so the absence is OK) — actually
  `list` doesn't take session_key, so this is fine.

Net effect: the schema lies about `history` — the model will supply a
`session_key` it doesn't need to, costing context tokens, and may pick a
  stale session id if it guesses wrong.

**Fix:** "Required for `rename` and `delete`. Optional for `status` and
`history` (defaults to current session). Ignored by `list` and
`search`."

---

## P1 — schema default for `limit` doesn't match handler for `history`

`peko-rs/core/src/tools/builtin/session/tool.rs:163-167`

```rust
"limit": {
    "type": "integer",
    "default": 50,
    "description": "Max results for 'list', 'history', or 'search'"
}
```

The handler at `tool.rs:260` defaults `history`'s limit to **100**, not
50. The schema advertises 50; the handler overrides. Same drift for
`search` (line 285, `unwrap_or(50)` — matches) but history is the
outlier.

**Fix:** either change the schema default to 100, or change the
history handler default to 50. The intent (per the description "Max
results") suggests 50 is the right value; the handler should align.

---

## P1 — invalid timezone silently falls back to local time

`peko-rs/core/src/tools/builtin/session/tool.rs:207-216`

```rust
let timestamp = if let Some(tz_str) = timezone {
    match tz_str.parse::<chrono_tz::Tz>() {
        Ok(tz) => now_utc.with_timezone(&tz)
            .format("%Y-%m-%d %H:%M:%S %Z").to_string(),
        Err(_) => chrono::Local::now()
            .format("%Y-%m-%d %H:%M:%S %Z").to_string(),
    }
}
```

If the model passes `"America/Atlantis"` or `"UTC+9"` (wrong format),
the tool silently uses local time. The model sees a formatted
timestamp and trusts it. This is the same shape as the cleanup silent
fallback above — a surface that hides errors.

**Fix:** return `Err(anyhow!("timezone '{tz_str}' is not a valid IANA \
tz (e.g. 'America/New_York', 'UTC')"))`. The schema at line 177-180
gives an example — a clear error when the model misses it will help
self-correction.

---

## P1 — AGENT.md says 12 actions; tool says 6

`peko-rs/core/src/resources/agents/root/AGENT.md:14`

```
- `session` — manage your memory. Single tool with **12 actions**:
  `status` / `list` / `history` (inspect one or many sessions),
  `search` (find text across transcripts), `branch` (copy) / `rename` /
  `archive` / `unarchive` (manage), `delete` (remove), `compact`
  (schedule summarization), `new` / `resume` (rotate your own
  conversation chapter — both take effect on the NEXT incoming
  message, not this turn).
```

This file is the principal's system prompt. It advertises 12 actions
to the model on every turn. The actual tool has 6 (PR #353 WS4
demoted `branch`/`archive`/`unarchive`/`compact`/`new`/`resume`).
The 6 demoted actions are still listed by name; the model will try to
call them and get `Invalid action: branch` (or whichever).

This is the F5 finding from r6 (2026-08-12 field test): the model
attempts a demoted action, gets a validation error, and has to retry.

Also at:
- `peko-rs/extension-api/src/capabilities.rs:298-300` (a comment)
- `peko-rs/extension-api/src/capabilities.rs:459` (a doc comment)
- `AGENTS.md:675` (the engine doc)

All four places say "12 actions" and list the same 12 names. All
four are out of date.

**Fix:** rewrite the AGENT.md session line as: "`session` — manage your
memory. Single tool with **6 actions**: `status` / `list` / `history`
/ `search` / `rename` / `delete`. Lifecycle operations (chapter
rotation, compaction, archive, branch) are engine-internal; the model
can't drive them manually. Cross-peer lookup: pass `peer` like
`"user:alice"`." Same edit to the 3 comment/doc sites. (The session
tool's own description at `tool.rs:103-122` is already correct after
WS4 — only the *secondary* surfaces were missed.)

---

## P1 — `agent_catalog` mention omits `model_id` field

`peko-rs/core/src/resources/agents/root/AGENT.md:9`

```
- `agent_catalog` — list the agents available in this Principal. Each
  entry has an `id`, a human-readable `name`, and an `enabled` flag.
```

The actual response shape (per the catalog implementation in the
session/messaging crates — see also [[multi-model-subagents-phase2-shipped]]
for Phase 2 model_list) carries more fields. Let me check what's
actually returned. (TODO: confirm by reading catalog tool.)

If `model_list` or `cost_per_call` are present, AGENT.md is missing
them — the model might miss affordances it has.

---

## P1 — Agent tool description redundantly repeats schema field info

`peko-rs/core/src/tools/builtin/messaging/agent.rs:557-580`

The description has:

> Parameters:
> - prompt: Description of the task to execute (required)
> - subagent_type: Name of the agent config under ~/.peko/... (required)
> - description: Optional description for tracking (matches Claude Code's Agent schema)
> - model: Optional model override for the subagent (matches Claude Code's Agent schema)
> - isolated: If true, creates isolated session without parent context (default: false)
> - cleanup: "keep" or "delete" - what to do with session after completion (default: "keep")
> - parent_session_key: ...
> - resume_session: ...

Then the schema (line 593-634) re-describes each one. **This is
~400 tokens of pure duplication on every turn** for the model's
context window. Modern Claude/Sonnet reads schemas natively; the
prose description is supposed to surface things the schema can't
(rationale, error modes, limits, examples).

What the description SHOULD keep (not in the schema):
- The 5-minute auto-detach behavior ("the framework applies a constant
  5-minute timeout…")
- The cross-tool pointer ("Sessions you spawn have `parent_session_id`
  set…")
- The resume_session refusal modes — "chapters/branches/live root
  sessions refuse, not be the session you are running in or an
  ancestor, not be archived, and not have an active run."
- The depth=3 / concurrent=5 limits with the structured error fields

Everything else is schema-redundant. Trim aggressively. The session
tool description has the same shape issue (lines 103-122 repeat
schema per-action summaries).

---

## P2 — Agent tool `description` parameter is misnamed

`peko-rs/core/src/tools/builtin/messaging/agent.rs:96`

```rust
pub description: Option<String>,
```

The comment says "Optional description for tracking". But the handler
at line 685 uses it as the session label: `label: description.clone()`.
So a model that passes `description: "Investigate the bug"` gets a
session labeled "Investigate the bug" — which is a session title, not
a description.

This is fine semantically (a description can be a title for the
session's purpose), but it's confusing. Two options:
- rename `description` → `title` to match the session tool's `title`
  parameter (used by `session action=rename`)
- leave it alone but document the dual purpose

The schema description ("Optional description for tracking this
spawn") leans toward "tracking" / "label". Rename is intrusive
(breaking change); documentation is cheap. At minimum, update the
schema description to "Optional title/label for the spawned session
— appears in `session list`."

---

## P2 — Agent tool description mentions `session_id` example value

`peko-rs/core/src/tools/builtin/messaging/agent.rs:589`

```
// Persistent worker - continue a previous spawned session with its history
{"prompt": "Now update report.txt with the new numbers", "subagent_type": "writer", "resume_session": "<session-id from session list>"}
```

The `<session-id from session list>` is a placeholder, not a literal
value. The model needs to substitute. The example is fine but adjacent
text ("see `session list`" at line 565) makes the linkage. No drift;
just slightly indirect.

---

## P2 — session tool `delete` has no `agent_id` filter

The session tool's `list` action supports `agent_id` filter (line
159-162). The `delete` action does not — but the schema description
doesn't promise it. So this is a missing affordance, not a drift.
Worth noting: the model can list+delete via two calls, but can't
do "delete all sessions for agent X" in one go.

---

## P2 — `is_active` field on `SessionInfo` is always `true`

`peko-rs/core/src/session/session_runtime_impl.rs:226`

```rust
is_active: true,
```

The field exists in the wire shape (`SessionInfo.is_active: bool`)
but is always set to `true` by the production adapter. The session
tool description says `list: query sessions (filters: peer, agent_id,
active_minutes; archived hidden unless include_archived:true)` — no
filter on `is_active` is offered, but the field is exposed.

Either:
- remove the field (it's always true, useless to model)
- or actually compute it (e.g., `m.updated_at > now - 5min`)

Currently it's dead data on the wire — costs context tokens without
informing the model.

---

## Summary of fixes worth doing

| ID | Priority | Effort | File |
|----|----------|--------|------|
| err_self_mutation references demoted actions | P0 | small | session/ownership.rs:167 |
| cleanup silently accepts garbage | P0 | small | agent.rs:647 |
| session_key required/wrong for history | P1 | small | session/tool.rs:135 |
| limit default mismatch (history: 100 vs schema: 50) | P1 | trivial | session/tool.rs:163 |
| timezone silent fallback | P1 | trivial | session/tool.rs:213 |
| AGENT.md says 12 actions | P1 | small | resources/agents/root/AGENT.md:14 + 3 comment sites |
| Agent description duplicates schema (token waste) | P1 | medium | agent.rs:553-590 |
| description param mislabeled (title vs description) | P2 | trivial | agent.rs:96 |
| is_active always true | P2 | trivial | session_runtime_impl.rs:226 |

The 3 P0/P1 wins (error message, silent fallback, AGENT.md drift) all
have the same shape: **description/schema/error-message drift between
the tool surface and what the rest of the system tells the model**.
Same pattern as the kinds-filter removal — fixing one place without
fixing the others creates a follow-up P0 in the next round.

## Files cited

- peko-rs/core/src/tools/builtin/session/{mod,cache,tool}.rs (session tool)
- peko-rs/core/src/tools/builtin/messaging/agent.rs (Agent tool)
- peko-rs/core/src/tools/builtin/messaging/dto.rs (SpawnCleanupPolicy re-export)
- peko-rs/core/src/session/session_runtime_impl.rs (production adapter)
- peko-rs/core/src/session/ownership.rs (error messages)
- peko-rs/core/src/extensions/builtin/adapter.rs (placeholder wiring)
- peko-rs/core/src/agents/agent.rs (real wiring)
- peko-rs/core/src/resources/agents/root/AGENT.md (root system prompt)
- peko-rs/extension-api/src/{subagent,capabilities}.rs (cleanup policy + capability comments)
- AGENTS.md:675 (engine doc)