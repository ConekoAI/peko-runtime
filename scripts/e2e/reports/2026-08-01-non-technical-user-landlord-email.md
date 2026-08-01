# 2026-08-01 — Peko CLI non-technical-user field test, landlord-email multi-turn scenario

**Tester:** automated (Claude Code, MiniMax-M3 model), acting as a non-technical human user
**Methodology:** `scripts/e2e/` isolation framework — fresh `HOME` + `PEKO_HOME` + `PEKO_DAEMON_SOCK` per run, real `minimax-MiniMax-M3` LLM, host `~/.peko` untouched
**Built binary:** `target/debug/peko` (debug build, no special features)
**Source reports:** `2026-08-01-non-technical-user-field-test.md` (v1), `2026-08-01-non-technical-user-field-test-v2.md` (v2), `2026-08-01-non-technical-user-multi-turn-trip.md` (Lisbon trip)
**Flow:** `scripts/e2e/flows/explore-landlord-email.sh` (left in the tree for reuse)

## Why this report

The prior passes were:
- **v1:** single-shot probes that filed 4 bugs (completions panic, silent principal overwrite, `show --json` ignored, empty-message costs a turn)
- **v2:** persona builder + Python coding helper + UX probes; filed 5 bugs (quota stuck, `quota --json` empty, ANSI in non-TTY, persona-show gap, `principal send` duplicate) — all 5 fixed in commit `16334640`
- **multi-turn-trip:** Lisbon 3-day trip with 3 turns; the model refused to invent restaurant addresses

This pass uses a *different* multi-turn scenario: **help me write a polite-but-firm email to my landlord about a broken dishwasher, then iterate on the tone across 4 turns.** The choice is deliberate:
- The task has a clear structural skeleton (greeting / issue / ask / sign-off) so we can grade prompt faithfulness
- The user pivots on tone 3 times — exercises the principal's ability to honour user steering without re-explaining context
- The conversation history grows every turn, so it exercises the cost-growth side of the LLM
- It is the kind of "I have a real-world problem, help me write to a human" task a non-tech user actually uses ChatGPT for

The goal is to surface friction a non-technical user would hit, **distinct** from the prior reports, and verify the v2 fixes still hold.

## What was exercised

| Surface | Result |
|---|---|
| `principal create comms-helper` (blank principal, no prompt) | ✅ — `displayName` auto-drafted by LLM as "Email Companion" |
| `principal persona set --from "…"` (real LLM, longer description) | ✅ — 9 s, drafted persona with `description`, `goals[]`, `values[]`, `primary.md` body |
| 4-turn real LLM conversation: open ask → firmer + lease clause 4.2 → softer again → short SMS | ✅ — all 4 turns complete, context retained, tone pivoted correctly |
| `principal show --json` (verify persona fields from v2 Bug D fix) | ✅ — includes `persona: { description, goals, values }` block |
| `log --since 1h --json` (verify pagination) | ✅ — 87 lines, paginated envelope |
| `quota status` (verify v2 Bug A fix) | ⚠️ — `output` and `requests` metered; **`input` still 0** |
| `quota status --json` (verify v2 Bug B fix) | ✅ — structured envelope emitted (rc=0, 287 bytes) |
| `system doctor` | ✅ — 3 checks |
| `system doctor --verbose > file` (verify v2 Bug C fix) | ✅ — no ANSI in redirected stdout or stderr |
| `daemon status --json` | ✅ — structured envelope, but surfaces an opaque `tunnel.state: "disabled"` field |
| `ext list` | ✅ — `builtin:core` |
| `cron list` | ✅ — empty |
| `principal persona show` (verify v2 Bug D subcommand gap) | ❌ — `rc=2`, "unrecognized subcommand 'show'" (subcommand was never added) |

## Conversation transcript (with timing)

| # | User asked | Wall time | Output bytes | Result |
|---|---|---|---|---|
| 0 | persona draft (`--from` description, 154 chars) | **9 s** | (LLM drafted persona) | displayName=Email Companion; goals include "Ask exactly one focused clarifying question whenever the email's context or intent is unclear" |
| 1 | "polite but firm, under 180 words, don't sign with my name" | **8 s** | 1444 | Clean 4-paragraph email, 160 words, acknowledged 4-year relationship, asked for repair date, offered to coordinate access. Added useful "Tone notes" explainer. |
| 2 | "actually firmer, reference lease clause 4.2, under 200 words" | **7 s** | 1746 | Kept warmth, added the lease clause 4.2 reference, framed as "written notice", re-affirmed 4-year relationship. Subject line changed to "Clause 4.2". |
| 3 | "scratch the lease clause, soften, landlords are stretched thin, keep the relationship good" | **9 s** | 1496 | Removed the clause, switched from "specific repair date" to "rough timeline" / "ballpark window", acknowledged the landlord's bandwidth. 140 words. |
| 4 | "now a 2-sentence iMessage variant, casual, one emoji max" | **5 s** | 596 | Casual one-paragraph text with 🙏 emoji, same context (4-year relationship, 10 days), ready to paste. |

**Multi-turn context retention: excellent.** The principal remembered across all 4 turns:
- The 4-year-tenure + decent relationship framing (turns 1, 2, 3)
- The 10-days-broken detail and the two text reports (turns 1, 2, 3, 4)
- The `7 days` repair deadline (turn 1, 2) → softened to "rough timeline" (turn 3) → still a "rough ballpark" in the SMS (turn 4)
- The lease clause 4.2 (turn 2) → removed (turns 3, 4) without re-explaining why

**Tone pivots worked.** The user flipped "polite-but-firm" → "firmer + legal" → "softer" in three turns. The model correctly held context while shifting register. A non-tech user who pivots like this in real life would have a smooth experience.

**Bonus: the model volunteered "Tone notes"** after every email — e.g. turn 2's "I kept the warmth … but the request is now tied to a contractual obligation rather than a favor." This is a non-tech-user-friendly addition the model produces unsolicited. It's good UX even if the user didn't ask for it.

**One thing the persona *did not* do:** the drafted persona's goals include "Ask exactly one focused clarifying question whenever the email's context or intent is unclear." Across 4 turns, the user gave clear context every time and the model did NOT ask any clarifying question. This is the right behavior (no friction) but the persona's "ask one clarifying question" rule is silently dropped when context is unambiguous. Worth noting as a prompting design point — the goal is honored by silence when not applicable, which is correct, but a non-tech user reading the persona would expect the principal to push back if asked something ambiguous. (We didn't test that path.)

## Performance

| Operation | Wall time | Notes |
|---|---|---|
| `model add --template minimax` | <100 ms | vault + catalog writes |
| `principal create comms-helper` | ~50 ms | creates DID + workspace + agents/ + identity/ |
| `daemon start --foreground` | ~2 s | IPC socket bound at ~1.9 s |
| `principal persona set --from "…"` (real LLM) | **9 s** | persona_draft; 154-char description (longer than v2's 6 s with a shorter description — output-size bound) |
| Turn 1: 180-word email | **8 s** | 1444 B output, 4 paragraphs |
| Turn 2: 200-word email + lease clause | **7 s** | 1746 B output |
| Turn 3: 140-word softer email | **9 s** | 1496 B output |
| Turn 4: 2-sentence SMS | **5 s** | 596 B output |
| `principal show --json` | <15 ms | file read |
| `log --since 1h --json` | <20 ms | JSONL scan |
| `quota status` | <20 ms | file read (returns 0/1176/4) |
| `quota status --json` | <20 ms | 287 B JSON envelope |
| `system doctor` | ~30 ms | 3 checks |
| `system doctor --verbose` | ~30 ms | no ANSI leak |
| `daemon status --json` | <10 ms | IPC poll |

Latency is consistent with v2: 5–9 s for a focused 500 B–1.7 KB output. The persona draft at 9 s is the only call where the wait is non-trivial — the description was 154 chars (longer than v2's 6 s with a ~70 char description), and the LLM had to draft a full persona with `displayName`, `description`, 5 `goals[]`, 5 `values[]`, plus a `primary.md` body. Output-size bound, not a peko issue.

A non-tech user waiting 5–9 s for a turn in `--no-stream` mode has **no progress indicator** (v2 noted the same). The persona draft at 9 s feels longer than it is because there's no feedback at all. A spinner / "drafting persona…" / "thinking…" line would close this.

## Bugs filed (in priority order)

### Bug F — `principal persona show <name>` still missing (revisit of v2 Bug D)

**Repro:**
```bash
peko principal create comms-helper --model minimax-MiniMax-M3
peko principal persona set comms-helper --from "a calm, careful writing helper…"
peko principal persona show comms-helper
```

**Observed:** rc=2, stderr `error: unrecognized subcommand 'show'`. `principal persona --help` lists only `set` and `help`. The v2 report's Bug D fix landed at commit `16334640` and added `persona: {description, goals, values}` to the `principal show --json` envelope — but **did not add a read-back subcommand.**

**Impact:** A non-tech user who ran `peko principal persona set --from "…"` has no CLI way to see what was written. The only paths to "what did peko write?" are:
- `peko principal show <name> --json` (works — but the envelope is 200+ lines of metadata, not the persona-specific fields surfaced cleanly)
- Reading `~/.peko/principals/<name>/principal.toml` + `agents/primary.md` directly (hard stop for non-tech users)

The v2 follow-up explicitly deferred a `persona show` subcommand as "sugar." This v3 pass surfaces it as a real gap, because after running the persona builder (the v1 feature wish) the natural next question is "what did you actually write?" — and the CLI has no answer.

**Workaround today:** `peko principal show <name> --json | jq .persona` (works, but `jq` is not a non-tech tool).

**Likely root cause:** v2 Bug D fix added the JSON envelope field; no follow-up added the read-back subcommand. The `principal persona` subcommand tree has only `set` and `help`.

### Bug G — `set -euo pipefail` in `run-case.sh:16` aborts any flow on a non-zero `peko` rc

**Repro (any exploratory flow that probes a missing/rc!=0 command):**
```bash
# In any flow script:
peko_iso_run principal persona show comms-helper     # rc=2 because subcommand missing
echo "this line never prints"                         # script exits before reaching here
```

**Observed:** `run-case.sh` line 16 is `set -euo pipefail`. When `peko_iso_run` returns 2 (for example, because the user is testing what happens with a non-existent subcommand), the script exits via the EXIT trap, the `peko_iso_done $?` cleanup runs, and **all subsequent probes in the flow are skipped**. The bash tool reports rc=2, which looks like a hang / test failure, not a graceful "this command isn't supported" signal.

**This is not a peko bug** — it's a methodology gap in the e2e framework. But it has real consequences:
- A flow author who wants to probe a `rc=2` command (to confirm a bug is still present, or to test error UX) cannot without writing `|| true` after every call
- A flow author who calls a subcommand that was *just* added and happens to fail with rc=1 will silently lose the rest of the run
- The exploratory flows in `scripts/e2e/flows/` (which exist to surface new bugs) are exactly the ones most likely to hit this

**Workaround today:** wrap every probe in `|| true` or `if peko_iso_run …; then … fi`. The `explore-landlord-email.sh` flow had to be patched with `peko_iso_run principal persona show comms-helper || true` to get past the v2 Bug D probe. (The patch is in the current flow file.)

**Recommended fix (in `scripts/e2e/lib/isolate.sh` or `scripts/e2e/run-case.sh`):** either (a) make `peko_iso_run` always return 0 to the caller and expose the rc via `_peko_iso_capture_rc` only, or (b) drop `set -e` and rely on explicit `peko_iso_assert_rc_zero` calls for the cases that should fail. Option (a) is closer to the spirit of an isolation library: capture the rc, don't propagate it as a script terminator.

### Bug H (regression) — `quota status` input still stuck at 0

**Repro:**
```bash
peko quota status comms-helper
# After 4 real LLM sends (output totals 1176 tokens)
```

**Observed:**
```
📊 Quota for 'comms-helper':
  cycle:      daily
  input:               0 / ∞          (unlimited)
  output:           1176 / ∞          (unlimited)
  requests:            4 / ∞          (unlimited)
  window:     2026-08-01T00:00:00+00:00 → 2026-08-02T00:00:00+00:00
```

`output` and `requests` are metered correctly (verified — same finding as the multi-turn-trip report). `input` is still 0. The v2 follow-up (commit `16334640`) fixed the input-metering bug for *principal* sends per that report's `quota-charging` regression test, but the input still reads 0 here for a real LLM principal.

**Impact:** `input` is the **more important half of the bill** for a non-tech user — input tokens are the system prompt + persona + history that they pay for every turn, and they grow on every follow-up because the conversation history rides along. A non-tech user told to "watch your quota" sees `input: 0/∞` and concludes the LLM is free for input — which is wrong. (For a 4-turn conversation, the input tokens can easily exceed the output tokens.)

**Workaround:** none today. The `[peko] iterations=1 input=N output=M total=K` line on stderr is the only place the real number surfaces, and that line is only visible in `--no-stream` mode.

**Likely root cause:** the meter thread for `output` and `requests` is plumbed correctly, but the `input` increment is on a different path that either doesn't fire or is keyed off something the IPC layer doesn't supply. (Same hypothesis as the v2 + multi-turn-trip reports — but the v2 follow-up was supposed to close this. Either the fix regressed or it was scoped narrowly to specific call patterns.)

## UX / discoverability / prompting notes (not bugs, but rough edges)

- **`daemon status --json` surfaces an opaque `tunnel` field.** The envelope includes:
  ```json
  "tunnel": {
    "degraded": false,
    "last_error": null,
    "reconnect_attempts": 0,
    "state": "disabled"
  }
  ```
  A non-tech user has no idea what "tunnel" means in this context. The doc is silent. Either the field needs a one-line gloss ("tunnel: external connection for remote daemon features (currently disabled)") or it should be hidden when `state == "disabled"`.

- **Persona draft is non-cancellable.** `peko principal persona set --from "…"` opens an IPC request to the daemon that the user cannot interrupt (no `interrupt` works for `persona_draft` — `interrupt` is keyed off `request_id` from `send` runs). A 9-s wait with no escape hatch is annoying; a 60-s wait is painful. This was flagged in v2 and remains.

- **No cost visibility in `--no-stream` mode.** Each turn produces ~600-1700 B of output in 5-9 s. The user has no idea what that cost them. The `[peko] iterations=1 input=N output=M total=K` line lands on stderr after the response — easy to miss when stdout is the focus. v2 noted the same.

- **`peko_iso_run` swallows stderr.** The e2e lib's `peko_iso_run` helper captures stderr into `_peko_iso_capture_err` but the existing flow templates don't surface it. The `[peko]` telemetry line is therefore invisible to flows that read the capture. The new `explore-landlord-email.sh` flow has the same gap. A future flow revision should `echo "$_peko_iso_capture_err" | grep '^\[peko\]'` after each `send` to surface the cost line.

- **Persona draft produces a `displayName` that varies run-to-run.** Across 5 runs of the same `--from` description, the LLM produced: "Email Polisher", "Email Writing Helper", "Email Composition Assistant", "Considered Email Helper", "Email Companion". All are reasonable, but a non-tech user running `principal create foo` 3 times in a row will get a different `displayName` each time and may not understand why. (This is LLM behavior, not a peko bug — but it's a surprising shape for a deterministic CLI.)

- **The model volunteers helpful "Tone notes"** after every email draft. This is unsolicited but genuinely useful — a non-tech user learning email etiquette gets free tutoring. The persona draft didn't *ask* for it, but the model produces it because the persona is "calm, careful writing helper for personal emails" and the model interpreted that as a request to explain tone choices. Worth surfacing in `peko chat <name>` (the v2 REPL wish) as a default behavior.

- **Multi-turn context retention is a strong positive finding.** Across 4 turns, the principal correctly remembered the 4-year relationship, 10-day duration, two text reports, 7-day deadline, and the lease clause 4.2 (added in turn 2, removed in turn 3). This is the kind of back-and-forth a non-tech user does naturally, and peko handles it well. (No bug here — just a positive finding worth preserving.)

- **`log --since 1h --json` emits 87 lines for 4 turns.** Each turn produces ~22 lines of JSONL (one per message: user, agent, system). The envelope is paginated. A non-tech user who runs `peko log <name>` (non-JSON) will get the full message bodies — fine for a 4-turn conversation, but a 40-turn conversation becomes unreadable. `--summary` (one line per turn) would be a useful addition.

## Deferred — top feature wish (re-affirmed from v2)

### `peko chat <name>` — an interactive REPL for principals

This scenario is **the perfect demonstration of why the v2 REPL wish is the #1 feature for non-tech adoption.** The user made 4 `peko send` invocations, each one a separate process: shell-parse → IPC → daemon → LLM → stream back → exit. The 4 turns in this run took 8+7+9+5 = 29 s of LLM work, plus ~5 s of overhead per invocation (process startup, IPC handshake, daemon routing). Total wall time for the 4-turn conversation: ~50 s of *user-visible* wait time, plus the cognitive cost of "I have to re-invoke the command every time."

With `peko chat <name>`:
```bash
peko chat comms-helper
# opens an interactive prompt:
# comms-helper › write a polite-but-firm email to my landlord about a broken dishwasher
# comms-helper › actually make it firmer and reference lease clause 4.2
# comms-helper › scratch that, make it softer
# comms-helper › now give me an iMessage version
# comms-helper › /help
# comms-helper › /quit
```

The user types 4 lines, sees 4 streaming responses, and the session continuity is **visible** — same window, same prompt, same persona. The history is the same JSONL that `peko log` already reads back, so `peko log comms-helper --since 1h` shows the same conversation. No new persistence layer.

This v3 pass adds a data point: **the persona model the v1 wish was supposed to enable is exactly the persona a non-tech user wants for this kind of multi-turn task.** A "calm, careful writing helper" with `goals = [draft concise emails, explain tone choices, ask one clarifying question]` is only useful if the user can do back-and-forth with it. The CLI's one-shot `send` invocation makes the persona feel like a single-use tool. The REPL makes it feel like a collaborator.

Why it's still the #1 wish:
- Every chat-shaped product a non-tech user has ever used (ChatGPT, claude.ai, Gemini, Copilot) presents as a chat, not a CLI
- Peko's CLI is precise and scriptable, but it's also where non-tech users bounce — and the persona builder (the v1 wish) is wasted if users bounce at the next step
- The infrastructure is already there: IPC, session persistence, JSONL log, persona + system prompt — `peko chat` is a 100-200 line addition on top of `peko send`'s handler
- v2 + this v3 pass both show the persona is a "one-shot" tool right now; the REPL is the missing piece

## Cleanup performed at end of run

- The single test run's tempdir (`/tmp/peko/explore-landlord-email-22647-67hcjq`) is preserved for inspection (KEEP_TEMPDIR=1).
- All scratch probe scripts in `/tmp/probe*.sh`, `/tmp/probe*.out`, and `/tmp/landlord-flow-*.out` removed at run end.
- No peko/daemon processes remain.
- Host `~/.peko/principals/` was not touched.
- The new exploratory flow `scripts/e2e/flows/explore-landlord-email.sh` is left in the tree for reuse.

## Open follow-ups vs the v2 report

| Item | Status this pass |
|---|---|
| Quota `output` metered | ✅ verified — 1176 / ∞ |
| Quota `requests` metered | ✅ verified — 4 / ∞ |
| Quota `input` metered | ❌ **regression — still 0 / ∞** (Bug H above) |
| `quota status --json` envelope | ✅ verified — 287 B JSON envelope, structured |
| `system doctor --verbose` no ANSI on non-TTY | ✅ verified — no `\x1b[` in redirected output |
| `principal show --json` includes persona fields | ✅ verified — `persona: { description, goals, values }` block emitted |
| `principal persona show <name>` subcommand | ❌ **still missing** (Bug F above) |
| `principal send` duplicate removed | ✅ verified — not in help output |
| Multi-turn context retention | ✅ verified across 4 turns (positive finding) |

## New findings vs the v2 report

- **Bug F — `principal persona show` subcommand gap** (revisit of v2 Bug D)
- **Bug G — `set -euo pipefail` aborts flow on rc=2** (e2e methodology issue)
- **Bug H (regression) — `quota status` input still 0** (regression of v2 Bug A fix)
- **UX note — opaque `tunnel` field in `daemon status --json`** (new)
- **UX note — `peko_iso_run` swallows stderr telemetry** (methodology gap)
- **UX note — `displayName` varies run-to-run** (LLM behavior, not a bug)
- **UX note — model volunteers "Tone notes"** (positive finding worth preserving)
- **Performance — 9 s persona draft** for 154-char description (slower than v2's 6 s for shorter description; output-size bound)

## Test + clippy status (no code changes in this pass)

- `cargo test -p peko-cli --bin peko` → not re-run (no code changes)
- `cargo test -p peko --lib --features test-utils` → not re-run (no code changes)
- `scripts/e2e/run-case.sh explore-landlord-email` → 4 turns + 11 probes, all reach a terminal state once the `|| true` patch is in place
- Reused v2 regressions: `quota-charging`, `regress-2026-08-01-fixes` → not re-run this pass (already verified in v2)

## Fixed in (post-report patches)

A subsequent pass landed fixes for **Bug F**, **Bug G**, and **Bug H**:

- **Bug F** — added `peko principal persona show <name>` subcommand (`peko-rs/cli/src/commands/principal.rs`). Mirrors the read-back shape used by `principal show --json`. Both text (`Description:`/`Goals:`/`Values:`) and JSON (`{name, isSet, description, goals, values}`) forms. Adds 3 unit tests; rc=0 for both forms.
- **Bug G** — changed `peko_iso_run` (`scripts/e2e/lib/isolate.sh`) to always return 0; the rc is now exclusively exposed via `$_peko_iso_capture_rc`. Existing flows using bare `peko_iso_run` followed by `peko_iso_assert_rc_zero` continue to work; exploratory flows that needed `|| true` to survive the probe rc no longer need it. New regression step #13 asserts the post-`peko_iso_run` line runs even when rc≠0.
- **Bug H** — added `input_tokens: Option<u32>` to `AnthropicDeltaUsage` (`peko-rs/providers/src/adapters/anthropic.rs`) and updated the `message_delta` handler to prefer the delta's value over the cached `message_start.message.usage.input_tokens`. Some Anthropic-compat providers (notably MiniMax M3 / `https://api.minimaxi.com/anthropic`) report the real `input_tokens` only in the delta and `0` in the start; before the fix the start value won and the meter recorded `input_tokens: 0` after every send. Adds 2 unit tests (`test_message_delta_input_tokens_overrides_start_zero`, `test_message_delta_without_input_tokens_uses_start`). New regression step #14 runs 3 real sends via MiniMax M3 and asserts `state.input_tokens > 0`, `state.output_tokens > 0`, `request_count == 3`. Observed after fix: `input=15451 output=6 requests=3` (15451 input tokens across 3 short sends, dominated by persona + system prompt).

All 14 regression steps (`scripts/e2e/flows/regress-2026-08-01-fixes.sh`) pass with the new fixes in place; `scripts/e2e/run-case.sh persona-builder` and `scripts/e2e/run-case.sh quota-charging` also re-passed with no regressions. No new clippy warnings introduced.

## Files left in tree for reuse

- `scripts/e2e/flows/explore-landlord-email.sh` — the new exploratory flow
- `scripts/e2e/flows/explore-multi-turn-trip.sh` — prior multi-turn scenario
- `scripts/e2e/flows/explore-coding-helper.sh` — v2 coding scenario
- `scripts/e2e/flows/explore-ux-probes.sh` — v2 UX probe set
- `scripts/e2e/reports/2026-08-01-non-technical-user-field-test.md` — v1
- `scripts/e2e/reports/2026-08-01-non-technical-user-field-test-v2.md` — v2
- `scripts/e2e/reports/2026-08-01-non-technical-user-multi-turn-trip.md` — Lisbon trip
- `scripts/e2e/reports/2026-08-01-non-technical-user-landlord-email.md` — this report
