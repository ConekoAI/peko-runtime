# 2026-08-01 (v2) — Peko CLI non-technical-user field test, follow-up pass

**Tester:** automated (Claude Code, MiniMax-M3 model), acting as a non-technical human user
**Methodology:** `scripts/e2e/` isolation framework — fresh `HOME` + `PEKO_HOME` + `PEKO_DAEMON_SOCK` per run, real `minimax-MiniMax-M3` LLM, host `~/.peko` untouched
**Built binary:** `target/debug/peko` (debug build, no special features)
**Source report:** `scripts/e2e/reports/2026-08-01-non-technical-user-field-test.md` (the v1 pass that filed 4 bugs + 4 follow-ups + 1 feature wish — all of which are now landed in the source tree)

## Why this pass exists

v1 surfaced four bugs (completions panic, principal silent overwrite, `show --json` ignored, empty message costs a turn) and a top feature wish (guided persona builder). All four were fixed in commit `249693ce` and the feature shipped in the follow-up commit described at the bottom of the v1 report. This v2 pass verifies those fixes still hold, exercises surfaces the v1 pass didn't touch (quota counters, persona `show` surface, `send --file`, the actual coding-helper workflow a non-tech user would run), and files new findings.

## What was exercised

| Surface | Result |
|---|---|
| `regress-2026-08-01-fixes` (4+3 fixes) | ✅ all 7 fixes pass against the live binary |
| `persona-builder` flow (real LLM) | ✅ dry-run + write + diff + behavior |
| `persona create` + `persona set` for a "Python CLI helper" persona | ✅ displayName, intent.goals, primary.md all drafted correctly |
| Real coding task: write a `wc_tool.py` CLI | ✅ 21 s, 3,152 bytes, valid `python3 -m py_compile` shape, follows instructions |
| Real coding task: write `test_wc_tool.py` | ⚠️ 59 s — 3× slower than task #1 (output-size bound) |
| Real coding task: refactor to `pathlib` | ✅ 10 s, valid replacement |
| `principal show --json` | ✅ emits envelope with `displayName: "Python CLI Helper"` |
| `log --since 1h --json` | ✅ paginated `{nextCursor, hasMore, messages[]}` |
| `send --file /tmp/probe-snippet.txt` | ✅ real LLM, 6 s, returns explanation + examples |
| `system doctor` | ✅ 3/3 checks |
| `daemon status --json` | ✅ structured envelope |
| `ext list` | ✅ shows `builtin:core` |
| `cron list` | ✅ empty |
| `quota status / list` (alias) | ⚠️ **bug — counters stuck at 0/∞ after 3 real calls** |
| `quota status --json` | ❌ **bug — rc=0, stdout empty, no envelope** |
| `system doctor --verbose` | ⚠️ **bug — emits ANSI escape codes to non-TTY capture** |
| `principal persona show` | ❌ **missing — rc=2 "unrecognized subcommand 'show'"** |

Two new exploratory flow files were left under `scripts/e2e/flows/` for reuse:
- `explore-coding-helper.sh` — create principal → draft persona → write + test + refactor a real Python CLI utility
- `explore-ux-probes.sh` — probe quota, persona, ext, send --file, log, doctor, daemon, cron

## Bugs filed (in priority order)

### Bug A — `quota status` counters never move
**Repro:**
```bash
peko model add --template minimax --model MiniMax-M3 --key "$MINIMAX_API_KEY"
peko principal create probe --model minimax-MiniMax-M3
peko daemon start --foreground &     # any 3 sends below

for i in 1 2 3; do
  peko send probe "Count to ${i}0 and stop." --no-stream
done

peko quota status probe
```
**Observed:** Every call hits the real LLM (each ~3 s of MiniMax-M3 work + 128/240-token reply content visible in `[peko] iterations=1 input=128 output=240 …` stderr). `peko quota status probe` then prints:
```
📊 Quota for 'probe':
  cycle:      daily
  input:               0 / ∞          (unlimited)
  output:              0 / ∞          (unlimited)
  requests:            0 / ∞          (unlimited)
```
The same output appears for `quota list`, `quota status --peer`, and after every call — the counters never increment.

**Impact:** Quota is the **only** built-in cost-visibility tool a non-technical user has (F18/F19 wired it but didn't wire a readable meter). With counters stuck at 0, the user has no signal they're spending tokens, and the F19 "centralized metering" promise is hollow. The `[peko] iterations=1 input=N output=M total=K` line on stderr is informative but only visible in `--no-stream` mode; `quota` is supposed to be the canonical answer.

**Workaround:** none today. The user has to scrape stderr lines from `peko send` invocations and sum them by hand.

**Likely root cause:** `quota status` reads from a counter file the daemon is supposed to update on every send (F19 [[f19-centralized-quota-metering]]). Either the daemon never writes the counter, the counter is keyed off something the IPC path doesn't supply (peer-id mismatch? request-id keyed? principal-id keyed to a different principal?), or the read path looks at the wrong file. `quota --peer` produces the same stuck value, which rules out a simple peer-key mismatch.

### Bug B — `quota status --json` returns empty stdout
**Repro:**
```bash
peko quota status probe --json
```
**Observed:** rc=0, stdout is 1 byte (newline), stderr is empty. No JSON envelope, no error message, no indication that `--json` was rejected or ignored.

**Impact:** The `--json` flag is exposed in `peko quota --help` (line 79 of the help output) and was wired in the v1 follow-ups, but the `status` dispatch arm doesn't honor it — the flag is parsed and discarded. Scripts reaching for `peko quota status --json | jq .input` get nothing.

**Workaround:** wrap the human-formatted text and parse it (gross).

**Likely root cause:** `handle_quota_status` accepts `json: bool` but never branches on it; the `display_name` / `quota` fields are written straight to `println!` regardless.

### Bug C — `system doctor --verbose` pollutes non-TTY capture with ANSI codes
**Repro:**
```bash
peko system doctor --verbose > /tmp/doctor.out
```
**Observed:**
```
[2m2026-08-01T09:15:36.146740Z[0m [32m INFO[0m [2mpeko[0m[2m:[0m Auto-detecting async transport for CLI mode
🏥 Running health check...
  ✓ daemon_ready: Daemon is ready to serve requests
  ✓ not_degraded: Daemon is operating normally
  ✓ uptime: Daemon uptime: 21 seconds

  Results: 3 passed, 0 failed, 0 warnings
```
The first line (`[2m2026-…`) is literal escape codes (`ESC[2m`, `ESC[32m`, `ESC[0m`) — the `tracing` formatter was applied without a `MakeWriter` filter on `stdout().is_terminal()`. A non-tech user who redirects the output to a file (or who pipes it to `grep` / `less`) gets literal control characters in their log.

**Impact:** Any non-tech user who runs `peko system doctor --verbose > health.log` or pipes it to anything gets garbage. The same bug almost certainly affects `peko -v …` and `peko daemon -v` output to redirected streams — these need a `is_terminal()` check on the tracing subscriber.

**Workaround:** strip with `sed 's/\x1b\[[0-9;]*m//g'` on the user's end.

### Bug D — `principal persona show <name>` doesn't exist
**Repro:**
```bash
peko principal persona show probe
```
**Observed:** rc=2, stderr `error: unrecognized subcommand 'show'`. `principal persona --help` lists only `set` and `help`. The `persona set --from` flow writes a drafted persona to `principal.toml` + `agents/primary.md`, but the user has no in-CLI way to read it back.

**Impact:** A non-tech user who ran `peko principal persona set pyhelper --from "…"` has no way to confirm what got written. The only paths to "what did peko write?" are:
- `peko principal show pyhelper --json` (works, but shows the full envelope, not the persona-specific fields)
- Read `~/.peko/principals/pyhelper/principal.toml` and `agents/primary.md` directly (hard stop for non-tech users)

The v1 follow-up explicitly deferred `persona show` ("sugar"). This v2 pass surfaces it as a real gap: after the persona builder runs, there is no discoverable read-back command.

### Bug E — `principal send` and `send` are duplicate commands
**Repro:**
```bash
peko --help            # → `send` listed as top-level command
peko principal --help  # → `send` listed under principal too
```
**Observed:** Two commands routing to the same handler. A non-tech user who finds `peko send` via `--help` will be confused when `peko principal send` also exists, and vice versa.

**Impact:** Discoverability + 2× docs surface area. Not a bug per se but a consistency smell.

## Performance notes

| Operation | Wall time | Notes |
|---|---|---|
| `model add --template minimax` | <100 ms | vault + catalog writes |
| `principal create <name> --model <id>` | ~50 ms | creates DID + workspace + agents/ + identity/ |
| `daemon start --foreground` | ~2 s | IPC socket bound at ~1.9 s |
| `principal persona set --from "…"` (real LLM) | **10 s** | persona_draft is one LLM call |
| `send` first turn (write `wc_tool.py`, ~3 KB out) | **21 s** | output-size bound |
| `send` second turn (write `test_wc_tool.py`, larger out) | **59 s** | output-size bound — 3× the first turn, ~3× the output size |
| `send` third turn (refactor to `pathlib`) | **10 s** | output similar to first turn |
| `send "Count to N"` (3 calls, short out) | ~3 s each | round-trip floor |
| `send --file <path>` | 6 s | file content shipped in the user message |
| `model test <id>` | ~4 s | 2 s LLM + 2 s overhead |
| `principal show --json` | <15 ms | file read |
| `log --since 1h --json` | <20 ms | JSONL scan |
| `quota status / list` | <20 ms | but returns zeros (Bug A) |
| `system doctor` | ~30 ms | 3 checks |
| `daemon status --json` | <10 ms | status poll |

Latency has a clear output-size slope: ~10 s for ≤1 KB, ~20 s for 3 KB, ~60 s for ~6 KB. The follow-up turn (test file) took **59 s** because the LLM was producing a large unittest module. For a non-tech user iterating on a feature, this means each revision costs ~30–60 s of wall time. There's no progress indicator in `--no-stream` mode during that wait — the user is staring at a blank terminal.

## Discoverability + UX notes (not bugs, but rough edges)

- **No streaming output is buffered to disk.** A user who runs `peko send foo "long task" --no-stream` and then closes the terminal loses the reply. `--tee <file>` would be a tiny addition.
- **Persona draft is non-cancellable.** `peko principal persona set … --from "…"` opens an IPC request to the daemon that the user cannot interrupt (no `interrupt` works for persona_draft — `interrupt` is keyed off `request_id` from `send` runs). A 10-s wait with no escape hatch is annoying; a 60-s wait is painful.
- **Latency hint missing.** After the persona_draft and send round-trips, the only cost/latency visibility is the post-hoc stderr line. A `--quiet`-friendly cost footer would help budget-conscious non-tech users.
- **No `peko chat <name>` interactive REPL.** Every turn requires a new `peko send <name> "…"` invocation. A REPL mode (`peko chat pyhelper`, prompt loops, slash-commands work, history scrolls) would match what every other chat UI does and is the obvious next step for non-tech adoption.
- **`system doctor --verbose` doesn't actually go verbose** — it just prepends one INFO log line and otherwise prints the same three checks. Useless.
- **The post-turn telemetry line `[peko] iterations=N input=I output=O total=T tools_failed=F` is the only cost signal**, but it lands on stderr *after* the response in `--no-stream`, so a user piping stdout misses it. Same content is invisible when stdout is captured.
- **`log --since 5m` prints full message bodies.** For a long conversation, `--summary` (or pagination through `--json` only) is the only path. A non-tech user would prefer a one-page summary view.

## Deferred — top feature wish

### `peko chat <name>` — an interactive REPL for principals

**Today:** the CLI's only path to back-and-forth with a principal is `peko send <name> "…"`. Each invocation is a separate process: shell-parse → IPC → daemon → LLM → stream back → exit. A non-tech user has no equivalent of typing into ChatGPT or claude.ai and pressing Enter to continue the conversation in the same window.

**Workaround today:** open a terminal, type `peko send pyhelper "…"`; repeat. There is no session continuity visible to the user (sessions are persisted in `data/principals/local/local/sessions/…jsonl` but you have to call `peko log` to see them). Every turn is its own process — the only persistent state is the JSONL.

**Proposed:**
```bash
peko chat pyhelper
# opens an interactive prompt:
# pyhelper › write a Python CLI that emulates wc
# pyhelper › ... (streaming reply)
# pyhelper › add a --chars flag
# pyhelper › write tests for it
# pyhelper › /help
# pyhelper › /interrupt
# pyhelper › /quit
```

Behaviour matches `python` or `node -i` REPL conventions:
- `rustyline`-based line editor with history (↑/↓, Ctrl-A/E, Ctrl-C interrupt)
- Each non-slash line is a `peko send` with `--stream` semantics; the reply streams token-by-token beneath the prompt
- Slash commands route through the existing local intercept (`/help`, `/clear`, `/interrupt <request_id>`)
- `--model <id>` overrides the principal's model for the session
- `--no-stream` for non-tech users who want full-response-then-prompt (e.g. terminal-without-color)
- Persistent history via `~/.peko/repl_history` (one file, append-only JSONL) — `peko log pyhelper` shows the same conversation

This is a 100–200 line addition on top of `peko send`'s existing handler (the IPC + session plumbing is already there). It closes the "I made a useful principal but using it is friction" gap that the v1 persona builder left open — the moment a non-tech user is supposed to *use* what they just configured.

Why it's the #1 wish: every chat-shaped product a non-tech user has ever used (ChatGPT, claude.ai, Gemini, Copilot) presents as a chat, not a CLI. Peko's CLI is precise and scriptable, but it's also where non-tech users bounce. A REPL keeps the CLI's scriptability for power users *and* gives non-tech users the entry point they expect.

## Cleanup performed at end of run

- `scripts/e2e/clean-tmp.sh` reports 0 candidates; nothing leaked.
- Two new exploratory flow files were left in `scripts/e2e/flows/` for future runs:
  - `explore-coding-helper.sh`
  - `explore-ux-probes.sh`
- No peko/daemon processes remain (verified by `pkill -9 -f 'target/debug/peko'` at the end of each flow).
- Host `~/.peko/principals/` was not touched.
- All scratch files (`/tmp/probe-snippet.txt`, `/tmp/probe.out`) removed at run end.

## Open follow-ups vs the v1 report

The v1 follow-up section noted three deferred items. This v2 pass:

| Deferred item | Status this pass |
|---|---|
| `--force` actually destructive | ✅ verified working — sentinel file in `agents/` was wiped by `principal create --force --yes` |
| `--json` for `principal list` / `remove` | ✅ verified — list emits array, remove emits `{removed:true, name:…}`, empty list emits `[]` |
| `peko quota list` alias | ✅ verified — `quota list scout` and `quota status scout` produce identical output |
| Guided persona builder | ✅ verified end-to-end — drafted `displayName: "Python CLI Helper"`, `intent.goals`, `primary.md` body with `{{memory}}` placeholder |

New findings in this pass are filed as Bugs A–E above. Top feature wish is `peko chat <name>` (the interactive REPL).

---

## Test + clippy status (no code changes in this pass)

- `cargo test -p peko-cli --bin peko` → 81 passed (unchanged)
- `cargo test -p peko --lib --features test-utils` → 1396 passed (unchanged)
- `scripts/e2e/run-case.sh regress-2026-08-01-fixes` → 7 fixes, all pass
- `scripts/e2e/run-case.sh persona-builder` → 3 steps, all pass
- `scripts/e2e/run-case.sh explore-coding-helper` → 4 turns of real LLM, all complete (write + test + refactor + log readback)
- `scripts/e2e/run-case.sh explore-ux-probes` → 24 sub-probes, all reach a terminal state (rc=0, rc=2 for missing `persona show`)

---

## Fixes applied (commit ranges)

The 5 bugs above (A–E) were all fixed in a single change-set landing
after this report. Summary of changes + verification:

| Bug | Files touched | Verification |
|---|---|---|
| A — quota counters stuck at 0 | `peko-rs/core/src/principal/{router.rs,manager.rs,routers/root.rs,context.rs,agent_runner.rs}` + `peko-rs/quota/src/{meter.rs,scope.rs}` (consumed only). Meter threaded `Principal.quota_meter` → `RouterContext` → `PrincipalContext` → `AgenticLoop.quota_meter`. | `scripts/e2e/run-case.sh quota-charging` (NEW) — 3 real LLM sends, asserts `quota status` reports `requests: 3 / ∞` and `output: > 0 / ∞`. JSON envelope asserts `state.request_count == 3`. |
| B — `quota status --json` empty | `peko-rs/cli/src/commands/quota.rs` (status() now builds the same `serde_json::json!({…})` envelope as `set`/`reset`). | New unit test `quota_status_json_envelope_shape` + regression step #8. |
| C — ANSI codes on non-TTY | `peko-rs/cli/src/commands/mod.rs:268-289` + `peko-rs/peko-daemon/src/main.rs:130-141`. Added `.with_ansi(std::io::stderr().is_terminal())` to both tracing subscribers. | New regression step #11 (asserts no `\x1b[` in `-v` stderr captured to file). |
| D — overstated persona-show gap | `peko-rs/cli/src/commands/principal.rs:534-589` — added `persona: {description, goals, values}` block to the `ShowView` JSON envelope. No new command. | New unit test `show_principal_json_envelope_has_persona_fields` + regression step #9. |
| E — `principal send` duplicate | `peko-rs/cli/src/commands/principal.rs` — deleted `Send` enum variant, dispatch arm, and `send_to_principal` function. Cleaned up unused `ChannelContext`/`ChannelKind` imports. | New unit test `principal_no_send_subcommand` + regression step #10 (asserts `Cli::try_parse_from(["peko","principal","send","x","y"])` fails with "unrecognized subcommand"). |

Final test status (post-fix):

- `cargo test -p peko-cli --bin peko` → **84 passed** (was 81; +3 new tests)
- `cargo test -p peko --lib --features test-utils` → **1396 passed** (unchanged)
- `scripts/e2e/run-case.sh regress-2026-08-01-fixes` → **11 fixes pass** (was 7)
- `scripts/e2e/run-case.sh quota-charging` → **new flow passes** (Bug A + Bug B verified end-to-end with real LLM)
- `scripts/e2e/run-case.sh explore-coding-helper` → still passes; now also reports `output: 4670 / ∞, requests: 3 / ∞` (Bug A bonus — the original v2 pass reported `0 / ∞` for the same flow)

Peer metering (F20) is deferred to a follow-up — the plan called it out as "out of scope" because it requires plumbing the `PeerRegistry` into `RootRouter::build_context`, which is a wider surface than this fix-set should touch. The principal meter alone closes Bug A for non-tech users, who care about *total spend per principal*, not per-peer attribution.