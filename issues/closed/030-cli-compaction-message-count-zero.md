# Issue 030: `peko session compact --dry-run --json` Reports `message_count: 0` Despite Populated JSONL

**Status:** ✅ **Closed / Resolved**
**Priority:** P2
**Area:** Session / Compaction / CLI Integration Testing
**Related:** `src/session/unified.rs`, `src/session/jsonl.rs`, `src/compaction/cli.rs`, `src/commands/session.rs`, `src/ipc/packet.rs`, `src/ipc/server.rs`
**Blocks:** the 5 deferred tests in `tests/cli_compaction.rs` (T1-full, T2, T3, T4, T5, T6 + extension) which would migrate the 6 sub-tests of `e2e_tests/compaction_cli.ps1` + `e2e_tests/compaction_extension.ps1` to Rust.
**Origin:** Discovered during `be34a2e` ("feat(session): add --dry-run --json + cli_compaction smoke test"). The CLI fix in that commit landed; the 5 deeper scenarios were deferred pending this issue.
**Resolved:** 2026-06-15

---

## 0. Resolution

### 0.1 Root Cause (Re-investigated)

The issue's own "Suggested Investigation Path" was a red herring. The bug is **not** in the `load_normalized → normalize_event → from_entries` pipeline; that path was correctly producing a non-empty `Vec<LlmMessage>`.

The smoking gun: the example output reports `estimated_tokens: 622`. If `load_context_fast` had returned an empty `Vec`, `Compactor::estimate_tokens` would have returned 0. The messages were reaching the compactor; they were getting lost on the way out.

The actual bug was a wire-format overload across two layers:

1. **Daemon (root cause)** — [`src/ipc/server.rs:2224`](../../pekobot/peko-runtime/src/ipc/server.rs#L2224) (pre-fix) hard-coded `messages_compacted: 0` in the `SessionCompacted` response for the dry-run path, throwing away `report.message_count` and `report.messages_to_compact`:
   ```rust
   let response = ResponsePacket::SessionCompacted {
       …
       messages_compacted: 0,                    // hard-coded — bug
       tokens_saved: report.estimated_tokens,    // forwarded
       tokens_before: report.context_window,     // forwarded
       tokens_after: report.context_window.saturating_sub(report.estimated_tokens),
   };
   ```

2. **CLI (compound bug)** — [`src/commands/session.rs:306-307`](../../pekobot/peko-runtime/src/commands/session.rs#L306-L307) (pre-fix) reused the same `messages_compacted` field for **both** `message_count` and `messages_to_compact` in the JSON output:
   ```rust
   message_count: messages_compacted,        // always 0 for dry-run
   messages_to_compact: messages_compacted,  // always 0 for dry-run
   ```

`SessionCompacted.messages_compacted` semantically means "messages folded into the summary" — a value only meaningful *after* real compaction. Reusing it for the dry-run response is a category error, and the CLI's reuse of that single field for two output fields doubled the visible damage.

### 0.2 Decision

**Introduce a dedicated `ResponsePacket::SessionCompactDryRun` variant** carrying the full `DryRunReport` fields, rather than overloading `SessionCompacted`. This separates the two concerns at the wire-format layer so the entire class of bug is impossible to repeat.

### 0.3 What Changed

| File | Change |
|---|---|
| [`src/ipc/packet.rs`](../../pekobot/peko-runtime/src/ipc/packet.rs) | New `ResponsePacket::SessionCompactDryRun` variant + registered in `request_id()` exhaustive match + new `test_session_compact_dry_run_response_roundtrip` |
| [`src/ipc/server.rs:2218-2240`](../../pekobot/peko-runtime/src/ipc/server.rs#L2218-L2240) | Send `SessionCompactDryRun` for dry-run, forwarding all 5 `DryRunReport` fields directly |
| [`src/commands/session.rs:30-49`](../../pekobot/peko-runtime/src/commands/session.rs#L30-L49) | New `render_dry_run_json` helper extracted from the match arm for testability |
| [`src/commands/session.rs:286-323`](../../pekobot/peko-runtime/src/commands/session.rs#L286-L323) | Match the new variant; render JSON via the helper; drop the synthetic `DryRunReport` reconstruction that was overloading `messages_compacted` |
| [`src/commands/session.rs:577-660`](../../pekobot/peko-runtime/src/commands/session.rs#L577-L660) | 3 new unit tests: field-name contract, distinct `message_count` vs `messages_to_compact`, no leakage of `messages_compacted` into the dry-run JSON |
| [`tests/cli_compaction.rs:244-360`](../../pekobot/peko-runtime/tests/cli_compaction.rs#L244-L360) | New `cli_compact_dry_run_json_reports_message_counts_after_multi_turn` integration test (6-turn setup, asserts `message_count >= 6` and `messages_to_compact >= 1`; gated on `MOCK_LLM_URL` like the existing smoke test) |

### 0.4 Why This Is Future-Proof

- **No more category error.** The dry-run wire format and the real-compaction wire format are siblings, not aliases. Adding fields to `DryRunReport` (e.g., `oldest_message_age`, `compaction_estimate_eur`) can be added to the new variant without touching `SessionCompacted`.
- **Backward compatible.** Existing real-compaction callers that parse `SessionCompacted` see no change. Existing dry-run JSON consumers see *additional correct values* for `message_count` and `messages_to_compact` (which were `0` and meaningless pre-fix).
- **Testable without a daemon.** The new `render_dry_run_json` helper is unit-testable in isolation; the field-name contract is enforced by `test_render_dry_run_json_preserves_message_counts`, `test_render_dry_run_json_separates_message_count_from_messages_to_compact`, and `test_dry_run_json_no_messages_compacted_field`.

### 0.5 Out of Scope (Per Original Issue)

- The 5 deferred PS1-T2..T6 + extension tests from the original issue are still deferred; the 6-turn integration test in this fix is the foundation for that follow-up PR (5-line restore from `be34a2e~1`).
- `e2e_tests/compaction/{cli,extension}.ps1` cleanup (Phase E) is untouched.

---

## 1. Problem Summary

After 6 mock-LLM-driven `peko send` rounds through the agent loop, the session's JSONL clearly contains 18+ events (6 user prompts, 6 assistant tool_calls, 6 tool results, 6 assistant text sentinels, plus session.created and model_change events). `peko session list <agent> --json` returns the correct `session_id`.

But `peko session compact <agent> --dry-run --json` reports:

```json
{
  "success": true,
  "dry_run": true,
  "session_id": "d1d3a360-...",
  "context_window": 128000,
  "estimated_tokens": 622,
  "percent": 0,
  "message_count": 0,
  "messages_to_compact": 0
}
```

`message_count: 0` (and `messages_to_compact: 0`) is the bug. The `session_id` in the response is the same UUID as the agent's `instance_id` in the JSONL's `session.created` event, so the CLI client and daemon agree on which session to inspect.

**Impact:** Cannot write the 5 deferred `tests/cli_compaction.rs` tests (and the corresponding real `e2e_tests/compaction/*.ps1` migration is blocked on the Rust tests). All 5 depend on the compactor's `load_context_fast` returning a non-empty `Vec<LlmMessage>` after multi-turn setup.

---

## 2. Diagnostic Data (Verified)

The bug reproduces deterministically with the smoke test in `tests/cli_compaction.rs` extended to do 6 setup turns. The JSONL after 6 turns (dumped via `read_session_jsonl`) shows the expected event sequence — `message.v2` events with `role: "user"`, `role: "assistant"` (text + tool_call), and `role: "tool"` (tool_result), all with `instance_id` matching the dry-run's `session_id`.

A representative excerpt of the JSONL (full output: ~50 lines, all 18 events confirmed present):

```json
{"type":"session.created","id":"evt_...","ts":"...","instance_id":"d1d3a360-a5a6-45ca-9ea8-c158e9f7ac4d","image_digest":"","trigger":"user"}
{"type":"message.v2","id":"evt_...","ts":"...","message_id":"msg_...","role":"user","content":[{"type":"text","text":"Use your write_file tool to create 'compaction_setup_t01.txt' ... needle 'cli-compact-dryjson-p4d7'."}],"timestamp":"...","role_metadata":{"User":{"source":"user"}}}
{"type":"system","id":"model_...","ts":"...","event":"model_change","detail":{"model_id":"default","provider":"openai_compatible"}}
{"type":"message.v2","id":"evt_...","ts":"...","message_id":"msg_...","role":"assistant","content":[{"type":"tool_call","id":"call_mock_155","name":"write_file","arguments":{"content":"COMPACTION_SETUP_T01_CONTENT","path":"compaction_setup_t01.txt"}}],"timestamp":"...","role_metadata":{"Assistant":{"provider":"openai_compatible","model":"default",...}}}
{"type":"message.v2","id":"evt_...","ts":"...","message_id":"msg_...","role":"tool","content":[{"type":"tool_result","tool_call_id":"call_mock_155","name":"write_file","content":[{"type":"text","text":"{\"bytes_written\":28,...}"}],"is_error":false}],"timestamp":"...","tool_call_id":"call_mock_155","role_metadata":{"Tool":{...}}}
{"type":"message.v2","id":"evt_...","ts":"...","message_id":"msg_...","role":"assistant","content":[{"type":"text","text":"SETUP_T01_DONE"}],"timestamp":"...","role_metadata":{"Assistant":{...}}}
... (5 more turns, each producing the same 5-event pattern) ...
```

All 18+ events are in the JSONL, all in the same session, all using the Event Format (new format), all parseable as `SessionEvent` (verified by `from_str` on a sample line). The agent loop is working correctly.

---

## 3. Reproduction

The simplest reproducer: take the existing `tests/cli_compaction.rs::cli_compact_dry_run_json_reports_metadata` and change `n_turns=1` to `n_turns=6` (the trim-and-dry-run smoke test only does 1 turn to keep the test green; the prior 6-turn version is what surfaced the bug). The diagnostic JSONL dump code is in git history at the commit immediately before `be34a2e`'s trim of the 5 deferred tests (search for `eprintln!("--- DIAGNOSTIC: session JSONL")` in `tests/cli_compaction.rs` history).

Manual reproducer (10s, no Rust):

```bash
# In peko-runtime/, with MOCK_LLM_URL=http://localhost:8080 (the mock-llm container):
mkdir /tmp/probe
PEKO_HOME=/tmp/probe/.peko peko agent create probe --provider mock 2>&1 >/dev/null
PEKO_HOME=/tmp/probe/.peko peko daemon start --foreground 2>/dev/null &
DAEMON_PID=$!
sleep 3

# Configure the mock to script a tool_call turn
curl -s -X POST $MOCK_LLM_URL/_test/configure \
  -H 'Content-Type: application/json' \
  -d '{"MOCK_LLM_SCRIPT": "{\"probe\":[{\"tool_call\":{\"name\":\"write_file\",\"arguments\":\"...\"}}, \"PROBE\"]}"}}'

# Send 1 turn
PEKO_HOME=/tmp/probe/.peko peko send probe "Use the needle 'probe' to write probe.txt and respond PROBE." --no-stream

# Inspect JSONL: should be populated
ls /tmp/probe/.peko/data/sessions/probe/default/*.jsonl | head -1 | xargs cat | grep -c '"type":"message.v2"'
# (returns >= 3, confirming the events are there)

# Run dry-run: should report message_count > 0
PEKO_HOME=/tmp/probe/.peko peko session compact probe --dry-run --json
# (returns message_count: 0 — the bug)
```

---

## 4. Code Paths to Investigate

### 4.1 `load_context_fast` → `build_context` → `load_normalized` pipeline

`src/session/unified.rs::load_context_fast` (line 772) is the entry point. It either returns a cached `Vec<LlmMessage>` or falls back to `build_context`, which loads from JSONL. The fallback is the only path on the first call (no cache yet). The relevant pieces:

- `src/session/unified.rs:163-207` — `from_entries` counts `UserMessage` / `AssistantMessage` from `NormalizedEntry`, and constructs a `Session` struct. **`message_count: 0` likely originates here** if `NormalizedEntry` doesn't have any `UserMessage` / `AssistantMessage` variants for our events.
- `src/session/unified.rs:772-799` — `load_context_fast` calls `self.storage.compute_jsonl_checksum`, `count_jsonl_entries`, and falls through to `build_context` on cache miss.
- `src/session/unified.rs:172-186` — the counter loop: only `NormalizedEntry::UserMessage` and `NormalizedEntry::AssistantMessage` are counted; tool calls and tool results are intentionally ignored (as comments say "Runtime-injected system messages … are treated as conversation" but tool messages don't increment the count).

### 4.2 `load_normalized` → `normalize_event` mapping

`src/session/jsonl.rs:326-385` — `load_normalized` reads the JSONL line-by-line, parses as `SessionEvent`, and calls `normalize_event` to map to `NormalizedEntry`. The `message.v2` events with `role: "user"` and `role: "assistant"` should map to `NormalizedEntry::UserMessage` and `NormalizedEntry::AssistantMessage` respectively. The diagnostic JSONL shows these events ARE present, so either:

- The parsing is silently failing (e.g., the `event.as_message()` check at line 368 returning `None` for our event shape), OR
- The role mapping at line 372-385 isn't matching (`msg.role()` returning the wrong variant), OR
- The counter at line 172-186 is correct but `message_count` isn't being returned / used in the dry-run path, OR
- The dry-run is using a `Session` opened via a different code path (e.g., one that re-reads the JSONL into a different in-memory state) and the count is being initialized to 0 somewhere else.

### 4.3 `SessionCompactor::dry_run` → `load_context_fast`

`src/compaction/cli.rs:50-72` — the dry-run is straightforward: call `session.load_context_fast()`, then `Compactor::estimate_tokens(&messages)`, then return `DryRunReport { message_count: messages.len(), ... }`. So `message_count: 0` ⇒ `messages.len() == 0` ⇒ `load_context_fast` returned an empty Vec.

The `Session` instance the dry-run uses is opened via the IPC handler `handle_session_compact` (`src/ipc/server.rs:2203-2216`) calling `service.open_session(agent, team, session_id, "default")`. The `session_id` it passes is the one the CLI client resolved via `resolve_session_id` (commands/session.rs:270-272). Both sides agree on the UUID (verified: dry-run's `session_id` matches JSONL's `instance_id`).

---

## 5. What "Fixed" Looks Like

After the fix, the smoke test extended to 6 setup turns (the prior 6-turn variant, recoverable from git history before `be34a2e`) should produce:

```json
{
  "success": true,
  "dry_run": true,
  "session_id": "d1d3a360-...",
  "estimated_tokens": ">0",
  "context_window": 128000,
  "percent": 0,
  "message_count": ">= 6",
  "messages_to_compact": ">= 1"
}
```

The 5 deferred tests in `tests/cli_compaction.rs` will then start passing:
- `cli_compact_actual_records_compaction_in_jsonl` (compaction_cli.ps1 T2)
- `cli_compact_updates_context_cache` (compaction_cli.ps1 T3)
- `cli_compact_session_usable_after_compaction` (compaction_cli.ps1 T4)
- `cli_compact_custom_instruction_in_summary` (compaction_cli.ps1 T5)
- `cli_compact_incremental_compaction_numbers` (compaction_cli.ps1 T6)
- `cli_compact_with_compaction_extension_installed` (compaction_extension.ps1 T1+T3)

The test file already has scaffolding for all 6 (just commented out / `replace_all`'d to a smaller scope in `be34a2e`); restoring them is a 5-line PR once the underlying bug is fixed.

---

## 6. Why This Bug Wasn't Caught Earlier

- The `compaction_cli.ps1` and `compaction_extension.ps1` PS scripts are NOT run in CI (they're in `e2e_tests/`, which the test pipeline doesn't execute). They were broken against the current code anyway (the `--dry-run --json` CLI fix is also in `be34a2e`).
- `compaction/integration_tests.rs` (in-process unit tests) doesn't cover the CLI path; it tests `SessionCompactor::compact` directly with hand-crafted `Session` objects that DO have `message_count > 0`.
- The CLI smoke test (the one that landed) only does 1 setup turn, so the message count is irrelevant to the assertion (it just checks the JSON shape fields exist).

---

## 7. What I Tried That Did NOT Fix It

(So the next person doesn't repeat the work)

- ✅ Verified the JSONL is populated — events are present, parseable, with the right `instance_id`.
- ✅ Verified the agent loop runs end-to-end — the file lands at `<peko_dir>/data/workspaces/<filename>` with the right content.
- ✅ Verified the mock-LLM sequence mechanism works — using a single shared needle + flat `[tc_1, sent_1, tc_2, sent_2, …]` script (12 elements for 6 turns) successfully drives the agent loop; the parent receives the expected sentinels (verified in stdout).
- ✅ Verified the dry-run returns the correct `session_id` — the UUID matches the agent's `instance_id` in the JSONL.
- ❌ Did NOT identify which specific step in the `load_normalized → normalize_event → from_entries` pipeline drops the events. That's where to focus.

---

## 8. Suggested Investigation Path

1. Add an `eprintln!` in `src/session/jsonl.rs::normalize_event` (or `load_normalized`) that prints the count of each `NormalizedEntry` variant returned across the JSONL lines. Run the smoke test and see what variants the events map to.
2. If the events map correctly to `UserMessage` / `AssistantMessage`, the bug is in `from_entries` (likely the role match or the counter logic). If they don't map (e.g., they fall through to `Some(None)` or return early), the bug is in `event.as_message()` or the `match msg.role()`.
3. If the events DO map but `message_count` is still 0, the bug is in the path from `load_context_fast` to the daemon's response — possibly the daemon is reading a stale cached context (the cache is a separate file `.context.cache` next to the JSONL; see `src/session/jsonl.rs` `write_context_cache` / `load_context_cache`).
4. If none of the above, compare the sessions-dir the agent loop writes to with the sessions-dir the daemon's `handle_session_compact` reads from. Both should resolve via `service.get_sessions_dir(agent, team)`, but a peer-based or path-component difference (e.g., `default` peer vs `user` peer) could mean they're different on-disk locations.

---

## 9. Files Involved

| File | Likely touched |
|---|---|
| `src/session/unified.rs` | `from_entries` (L163-207) — count + Session construction. The most likely fix location if the events ARE mapping correctly. |
| `src/session/jsonl.rs` | `load_normalized` (L326) and `normalize_event` (L363) — if events aren't mapping. |
| `src/session/jsonl.rs` | `compute_jsonl_checksum` / `count_jsonl_entries` / `write_context_cache` / `load_context_cache` — if a stale cache is being returned. |
| `src/compaction/cli.rs` | `SessionCompactor::dry_run` (L50-72) — unlikely; this is a thin wrapper around `load_context_fast`. |
| `tests/cli_compaction.rs` | After the fix, restore the 5 deferred tests (5-10 min PR). The helpers `setup_multi_turn_session` and `build_flat_script` and the 5 test functions are in the git history (commit before `be34a2e`'s trim). |

---

## 10. Hand-off Notes

- The mock-LLM-side scaffolding is solid. The deferred tests use a single shared needle + flat `[tc, sent, tc, sent, …]` script sequence; the per-substring counter (keyed on the needle) advances on every LLM call regardless of message-history contents. This part works.
- The `tests/cli_compaction.rs` file is small (~150 lines in its smoke-only state) and the deferred tests can be restored from git history (look at the file as of commit `be34a2e~1` or earlier).
- The smoke test at `cli_compact_dry_run_json_reports_metadata` (with 1 setup turn) is GREEN. Don't break it; extend it (or add a new test) to do 6 turns and assert `message_count >= 6` to lock in the fix.

---

## 11. Definition of Done

- The reproducer (manual or test) returns `message_count >= 6` after 6 setup turns.
- The 5 deferred tests in `tests/cli_compaction.rs` pass locally and in CI.
- `make test-cli-compaction` runs all 6 tests green.
- The `e2e_tests/compaction/{cli,extension}.ps1` PS scripts can be deleted (Phase E cleanup, deferrable).
- TESTING.md §7 coverage gap section can be updated to mark the deferred scenarios as migrated.
