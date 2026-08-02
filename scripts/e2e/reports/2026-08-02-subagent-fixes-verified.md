# 2026-08-02 — Subagent fixes verified end-to-end

**Tester:** automated (Claude Code, MiniMax-M3 model), acting as a non-technical human user
**Methodology:** `scripts/e2e/` isolation framework — fresh `HOME` + `PEKO_HOME` + `PEKO_DAEMON_SOCK`, real `minimax-MiniMax-M3` LLM via `$MINIMAX_API_KEY`, host `~/.peko` untouched
**Built binary:** `target/debug/peko` + `target/debug/peko-daemon` (debug, v0.1.0)
**Scenario:** 4-turn probe — one probe per fix from `abstract-foraging-dream.md`. Each probe is a runtime check that the user-visible behavior actually changed.
**Flow:** `/tmp/peko-fixes-verify.sh` (isolated setup, daemon left running)
**Logs:** `/tmp/peko/subagent-fixes-verify-36709-um748q/` (tempdir, cleaned up below)

## Why this report

The earlier 16-turn field test (`2026-08-02-non-technical-user-subagent.md`) found four gaps. We shipped fixes for each in this branch:

| # | Gap | Fix |
|---|---|---|
| 1 | `tool:Async*` described but not granted to fresh principals | Add 5 grants to `Capabilities::starter_bundle()` |
| 2 | `peko send --stream` rejected with tip pointing at `--no-stream` | Add hidden `--stream` synonym field |
| 3 | Depth error JSON opaque (`{error, note, status}` only) | Add `error_type`/`current_depth`/`max_depth` from `SpawnError` |
| 4 | Token footer missing after streaming run | Capture `RunSummary` in streaming branch, emit footer in `Done` arm |

This report is the runtime evidence that each fix actually landed in user-visible behavior — not just compiles + unit tests pass.

## Setup

```
HOME              = /tmp/peko/subagent-fixes-verify-36709-um748q/home
PEKO_HOME         = /tmp/peko/subagent-fixes-verify-36709-um748q/home/.peko
PEKO_DAEMON_SOCK  = …/run/daemon.sock
PEKO_BIN          = /Users/rlsn/workspace/ConekoAI/peko-runtime/target/debug/peko
PEKO_DAEMON_BIN   = /Users/rlsn/workspace/ConekoAI/peko-runtime/target/debug/peko-daemon
model             = minimax-MiniMax-M3
principal         = collab + dbg-depth (created via `peko principal create …`)
```

## Unit tests (all pass)

| Test | Package | Result |
|---|---|---|
| `starter_bundle_includes_async_tools` | peko-extension-api | ok (1/1) |
| `send_parses_stream_flag_as_noop_synonym` | peko-cli | ok (1/1) |
| `test_error_response_formatting` (covers both typed-chain + stringified paths) | peko | ok (1/1) |

The stringified-path sub-tests are what catch the regression described under Fix 3 below — they assert that an error message of the shape `"Subagent failed: Maximum spawn depth exceeded: 4 (max: 3)"` (where the async-exec layer has already stringified the typed error) still reconstructs `error_type: "DepthLimitExceeded"`, `current_depth: 4`, `max_depth: 3`.

## Live runtime probes

### Fix 1: Async tools granted ✅

**Before:** Fresh `collab` principal saw `AsyncSpawn` / `AsyncList` / `AsyncStatus` / `AsyncOutput` / `AsyncStop` in tool descriptions but `is_tool_enabled` filtered them out of the LLM's `available_tools` list — every `AsyncSpawn` call was refused at dispatch with "tool not enabled".

**Probe:** `peko capability list --principal dbg-depth --json` filtered for `tool:Async*`.

**Result:**

```
Async tools granted: 5
  tool:AsyncList
  tool:AsyncOutput
  tool:AsyncSpawn
  tool:AsyncStatus
  tool:AsyncStop
```

**Cross-check via daemon log:** the dynamic tool-list construction at `agentic_loop` step "Dynamically built 22 tool definitions from ExtensionCore" includes `"AsyncSpawn", "AsyncList", "AsyncStatus", "AsyncOutput", "AsyncStop"` — confirming the model can actually see them in its prompt.

---

### Fix 2: `--stream` parses ✅

**Before:** `peko send collab "hi" --stream` errored with `unexpected argument '--stream' found. tip: 'send --no-stream' exists`. The CLI streams by default; users who try to force streaming got rejected.

**Probe:** `peko send dbg-depth --stream 'Reply with just: hi'`.

**Result:**

```
[peko] request_id=1 (run `peko interrupt 1` to stop)

dbg-depth: hi
[peko] iterations=1 input=15625 output=2 total=15627 tools_failed=0
exit=0
```

The flag parses, the run streams normally, and the response text appears. No "tip" message.

---

### Fix 3: Depth error JSON shape ✅

**Before:** When the depth limit fired, the model got `{"error":"Maximum spawn depth exceeded: 4 (max: 3)","note":"Maximum spawn depth exceeded. Cannot create nested subagents at this depth.","status":"forbidden"}` — three string fields, no agent type, no numeric fields. The model had to quote the `note` string to recover.

**Probe:** 5-step chain `dbg-depth → Agent(primary) → Agent(primary) → Agent(primary) → Agent(primary)`; the 4th spawn should hit the depth limit. Asked the model to copy the failing tool_result JSON verbatim.

**Result:**

```json
{"current_depth":4,"error":"Maximum spawn depth exceeded: 4 (max: 3)","error_type":"DepthLimitExceeded","max_depth":3,"note":"Maximum spawn depth exceeded. Cannot create nested subagents at this depth.","status":"forbidden"}
```

All six expected fields present: `status`, `error_type`, `current_depth`, `max_depth`, `error`, `note`. Numeric values are `4` and `3` (not strings).

**Note on the verification journey:** the first live attempt after shipping still showed the old shape — same 3-field envelope. Tracing revealed the string-parsing fallback `parse_two_u32s` had a leading-whitespace bug: the second number's capture window started with `" 3)"`, but `find(|c| !c.is_ascii_digit())` returned the leading space as a non-digit, making the parse return `None`. Unit test caught this once `error.to_string()` was asserted end-to-end (e.g. `"Subagent failed: Maximum spawn depth exceeded: 4 (max: 3)"`). Fix: `trim_start()` the `after_sep` slice before scanning digits. Re-verified end-to-end and the new shape landed. Final test passes; debug eprintlns removed.

---

### Fix 4: Token footer after streaming ✅

**Before:** The `[peko] iterations=N input=X output=Y total=Z tools_failed=F` line only emitted in the `--no-stream` branch. After a normal streaming run, users had no idea what they paid. The earlier 16-turn field test billed ~395k input vs ~7.5k output (50:1) and no surface made that visible.

**Probe:** `peko send dbg-depth --stream 'Just say: hello'` with stdout/stderr captured separately.

**Result:**

```
--- stdout ---
dbg-depth: hello
--- stderr (footer goes here) ---
[peko] request_id=1 (run `peko interrupt 1` to stop)
[peko] iterations=1 input=16532 output=2 total=16534 tools_failed=0
```

The footer lands on stderr (so `peko send … | grep -i 'response'` still works) right after the streaming response completes — same shape as `--no-stream`.

## Summary

All four gaps from the 16-turn field test now have user-visible fixes:

| Gap | Status |
|---|---|
| Async tools callable | ✅ |
| `--stream` parses | ✅ |
| Depth error JSON typed | ✅ |
| Token line visible | ✅ |

Bonus fix shipped as part of Fix 3 verification: `parse_two_u32s` leading-whitespace bug — the unit test caught it once I re-ran after the live probe failed.

## Cleanup

The tempdir at `/tmp/peko/subagent-fixes-verify-36709-um748q` is removed; host `~/.peko` was never touched (per `peko-e2e-isolation` memory).