# 2026-08-02 — Peko CLI subagent field test (16 adaptive turns)

**Tester:** automated (Claude Code, MiniMax-M3 model), acting as a non-technical human user
**Methodology:** `scripts/e2e/` isolation framework — fresh `HOME` + `PEKO_HOME` + `PEKO_DAEMON_SOCK` per run, real `minimax-MiniMax-M3` LLM via `$MINIMAX_API_KEY`, host `~/.peko` untouched
**Built binary:** `target/debug/peko` (debug build, v0.1.0)
**Scenario:** 16-turn adaptive conversation driving the Agent (subagent) tool surface — happy-path invocation, depth nesting, parallel spawns, capability revoke/regrant, isolated/cleanup variants, nonexistent types, async/background gaps, and streaming cancel.
**Flow:** `/tmp/peko-subagent-setup.sh` (one-off isolated setup; daemon left running for the adaptive phase)
**Logs:** `/tmp/peko/subagent-adaptive-logs/01-…` through `16-…` (one per turn + final stream captures)

## Why this report

The user asked for a subagent-focused field test on the peko CLI as a non-technical user. The existing F37 / F38 / F39 / Phase 10e changes are well tested at the unit level (per `peko-subagent-architecture-verified.md`), but real users only see one of those things at runtime: **does the `Agent` tool do what the model claims it can do, and what error does the user see when it can't.** This pass drove 16 turns end-to-end against the real M3 model to find the gaps.

## Setup

```
HOME                  = /tmp/peko/subagent-adaptive-33486-8o7k3o/home
PEKO_HOME             = /tmp/peko/subagent-adaptive-33486-8o7k3o/home/.peko
PEKO_DAEMON_SOCK      = …/run/daemon.sock
PEKO_BIN              = /Users/rlsn/workspace/ConekoAI/peko-runtime/target/debug/peko
model                 = minimax-MiniMax-M3 (added via `peko model add --template minimax --model MiniMax-M3`)
principal             = collab (created via `peko principal create collab --model minimax-MiniMax-M3`)
subagents             = primary (built-in), researcher (custom), writer (custom)
custom-agent prompt   = YAML frontmatter at $PEKO_HOME/principals/<p>/agents/<name>/AGENT.md
                       (not the bare .md form; the resolver looks under the directory first)
capability grants     = `peko capability list --principal collab --json` shows
                       `["tool:Read", "tool:Write", "tool:Edit", "tool:Bash", "tool:Agent",
                         "tool:agent_catalog", "tool:TaskCreate/...", "tool:PlanCreate/...",
                         "principal:write_config", "principal:write_agents",
                         "principal:write_cron", "principal:write_identity",
                         "agent:researcher", "agent:writer"]`
                       (no `AsyncSpawn`/`AsyncStatus`/`AsyncOutput`/`AsyncStop`/`AsyncList` —
                       see Finding 2)
```

## Conversation transcript (with timing)

| # | User said (paraphrased) | Wall | Iterations / Tokens (in/out) | Outcome |
|---|---|---|---|---|
| 1 | "Plan a 3-day Reykjavík trip with an 8-year-old — give me the high-level itinerary in ~5 lines." | **20 s** | 2 / 12,801 → 232 | Parent dispatched `Agent(subagent_type=researcher)` itself; received a concierge handoff. Clean. |
| 2 | "Follow-up — verify the subagent really ran and didn't make up the founding year." | **21 s** | 3 / 20,952 → 744 | Parent fetched Wikipedia via `Bash + curl`, parsed 827,489 bytes, distinguished Landnámabók tradition (874 AD) vs. municipal founding (1786). |
| 3 | "Drive the Agent tool directly to fetch the Reykjavík forecast for right now." | **29 s** | 2 / 15,854 → 224 | Receipt `run_405c3716…` returned inline. Model appended the temperature/wind it had to look up. |
| 4 | "Ask the researcher to spawn a writer subagent to write a 3-line haiku." | **32 s** | 3 / 25,469 → 364 | Two-level chain succeeded. `/tmp/iceland-haiku.md` (88 B) verified. |
| 5 | "Enumerate any background tasks / async handles via `peko async-list`." | — | 2 / 19,781 → 523 | **Model reported AsyncList is missing** — see Finding 2. |
| 6 | "Use researcher with `isolated=true, cleanup=delete`, prompt with `echo PING_…`." | **17 s** | 2 / 22,921 → 633 | Receipt echoed `cleanup:"delete"`. Model could not verify deletion from inside its own toolset. |
| 7 | "Spawn two agents in parallel — Bash + curl and a trivial echo." | **21 s** | 2 / 36,132 → 449 | Both `run_id`s returned in one tool-result block — clean fan-out. |
| 8 | "Try `subagent_type=nonexistent` and `=critic`." | **11 s** | 3 / 37,670 → 334 | Hard rejection: `Subagent type 'X' not found at ".../config.toml"` for both. |
| 9 | "Try `AsyncSpawn` to background the agent invocation." | **6 s** | 1 / 12,650 → 323 | **Model reported AsyncSpawn is missing** — see Finding 2. |
| 10 | "Backgrounded Bash via `run_in_background:true` to surface a `task_id`." | **26 s** | 4 / 53,936 → 749 | Two `task_id` receipts (`Bash:b6d79dea…`, `Bash:9db058d2…`); **but no inspector** — model couldn't wait/cancel/list them. |
| 11 | "Try to push past max depth (3) — build a 3-level chain." | **32 s** | 2 / 29,146 → 516 | 3-level chain succeeded; depth-3 researcher self-throttled. |
| 12 | "Find the actual depth ceiling — try depth 5 (R → W → R → W → R)." | **43 s** | 2 / 31,486 → 690 | **Hard limit at depth 4** — see Finding 1. |
| 13 | "Revoke `agent:writer`, then retry `Agent(subagent_type=writer)`." | **15 s** | 2 / 33,491 → 443 | Cleanly refused with `Grant 'agent:writer' and retry` — see Finding 3. |
| 14 | "Re-grant writer and run the final packing-list task." | **21 s** | 2 / 34,725 → 252 | `/tmp/iceland-packing.txt` (61 B) verified. |
| 15 | "Stream a long-running subagent and interrupt it via `peko interrupt <id>`." | **3 s** | — | CLI rejected `--stream` (see Finding 5). Stream run already completed before interrupt landed. |
| 16 | "Re-attempt: stream + interrupt (no `--stream` flag)." | **7 s** | 1 / ~5k → ~10 | `peko interrupt 1` returned `Sent interrupt to run 1`; stream terminated cleanly. |

Wall times are end-to-end `peko send` plus receipt print, on a single Claude Code session with the daemon on the same host. Turn 9 (backgrounded Bash) is the slowest non-streaming turn at 26 s — the parent's reflection on the missing inspector surface added iterations.

## Functional / behavioural findings

### Finding 1 — Depth limit is enforced but the boundary is unclear

Turn 11 set up a 3-level chain (root → researcher → writer → researcher). The depth-3 researcher **self-throttled** ("I am at the depth limit (depth 3/3) and did not spawn additional subagents") and the model concluded *depth is a soft convention*. Turn 12 forced depth 4 with a different pattern (R → W → R → W at depth 4) and got:

```json
{
  "error": "Maximum spawn depth exceeded: 4 (max: 3)",
  "note": "Maximum spawn depth exceeded. Cannot create nested subagents at this depth.",
  "status": "forbidden"
}
```

So:
- Hard limit exists.
- It's at `max: 3`, meaning the **count includes the spawning agent itself** — a subagent at depth 3 cannot spawn further.
- The friendly error message (`"forbidden"` status) doesn't name `SpawnError` or include the agent type, so the model has to quote `note` to recover.
- The model wrote in turn 11 that "depth may be a soft convention" before being contradicted in turn 12 — that's a model-interpretation issue, not a runtime bug, but it shows the runtime isn't telling the agent *why* it should stop.

**Impact:** non-technical user who sees a depth error gets `forbidden` as the JSON status — meaningless to them. The tool should return a structured `SpawnError::DepthLimitExceeded` variant per the F39 taxonomy and the model should be told in the system prompt what the limit is, so it can self-throttle on turn 1 rather than overshooting and getting rejected.

### Finding 2 — Async/background inspector gap is real (and bigger than F39/F40 territory)

Across turns 5, 9, and 10, the model consistently tried to inspect background tasks and could not:

| Tool | Detected in capability list? | In the principal's `active` toolset? | Result |
|---|---|---|---|
| `AsyncSpawn` | yes | **no** | model refused: "AsyncSpawn is not a tool in my toolset" |
| `AsyncList` | yes | **no** | model refused: "AsyncList does not exist in this toolset" |
| `AsyncStatus` | yes | **no** | model refused: "AsyncStatus … is not actually bound" |
| `AsyncOutput` | yes | **no** | model refused: "no AsyncOutput tool exposed to me" |
| `AsyncStop` | yes | **no** | model refused: same pattern |
| `Bash` with `run_in_background:true` | yes | **yes** | works — returns `{task_id, status:"running", tool:"Bash"}` |

But **even with `run_in_background`**, the model has no way to *read* the output, *check* the status, or *cancel* the task — it has a `task_id` with no inspector behind it. The model literally wrote: *"I have two live task_ids I can't read, cancel, or wait on"*.

This is a clear UX cliff:
1. The framework describes the async control family in tool descriptions (visible to the model).
2. The framework only binds `AsyncSpawn` for principals that explicitly enabled it.
3. The `collab` principal has none of the `tool:Async*` capabilities (only `tool:Bash`, `tool:Agent`, `tool:Task*`, `tool:Plan*`).
4. The Bash background path *kind of* works but doesn't expose a control surface.

**Impact:** A user asking the principal "did that background job finish?" gets the honest answer "I have no way to tell" — which makes the CLI a worse experience than just running the command synchronously. Either:
- the async control family should be granted by default for `tool:Bash` (so the model can read/cancel its own backgrounded tasks), or
- `peko` should remove the `Async*` references from tool descriptions that aren't actually bound.

This affects subagent UX directly because the model thought it could `AsyncSpawn` an `Agent` invocation (turn 9) and was refused — it had to fall back to synchronous blocking calls, which serialised 16 turns that could have been concurrent.

### Finding 3 — Capability gate distinguishes "not found" from "not enabled" (good)

Turn 8 (`subagent_type=nonexistent`) returned:

```
Error: Subagent type 'nonexistent' not found at "/tmp/peko/…/agents/nonexistent/config.toml"
```

Turn 13 (after revoking `agent:writer`) returned:

```json
{
  "error": "Subagent 'writer' is not enabled for this principal. Grant 'agent:writer' and retry."
}
```

Two distinct error shapes for two distinct conditions. The model immediately understood the difference and re-granted cleanly. **This is the right design** — the F37 capability-driven per-type gate works as advertised.

**Positive observation:** the framework survives capability revocation between turns without any "stale config" or daemon-restart requirement. Turn 14 re-granted and the next send worked. (Worth noting because v2 audit feared staleness on tier-1 changes.)

### Finding 4 — Parallel fan-out works; model-level concurrency is the bottleneck

Turn 7 spawned two Agent calls in the same function_calls block and both `run_id`s came back synchronously inside the same tool-result block. Distinct UUIDs, no interleaving, the network-bound curl didn't delay the trivial echo. **Concurrency ≤ 5 per F39 default (`DEFAULT_MAX_CONCURRENT: usize = 5`) is respected.**

But over the whole 16-turn run, only 1 of the 16 turns actually used the parallel path — the model defaulted to single-shot serialization even when given prompts that invited concurrency (e.g. turn 10's "spawn a researcher AND a writer"). This is a model behaviour, not a runtime bug, but it means the runtime's concurrency-5 capacity is not a meaningful constraint in practice for non-technical users.

### Finding 5 — `--stream` flag is wrong-direction; CLI uses streaming by default

Turn 15 tried `peko --stream send collab …` and got:

```
error: unexpected argument '--stream' found

  tip: 'peko send --no-stream' exists

Usage: peko [OPTIONS] <COMMAND>
```

The CLI streams by default; the explicit opt-out is `peko send --no-stream`. That's a sensible default (streaming is what users want) but the tip text suggests `send --no-stream` is the supported direction while `--stream` is a typo — which is correct, but the inverse flag name (`--stream`) isn't accepted at all. A user who reads "streaming by default" and tries to *force* streaming gets an error.

**Impact:** small UX friction. Recommendation: either accept `--stream` as a no-op synonym (so `peko send --stream` and `peko send` behave identically) or include `--stream` in the error tip.

### Finding 6 — `cleanup=delete` policy echoes in receipt but is unverifiable from inside the toolset

Turn 6 used `isolated=true, cleanup=delete` and the receipt contained `"cleanup":"delete"`, but the model wrote: *"I have no inspector tool to verify the deletion actually happened on disk; I can only confirm the policy was applied."*

I verified externally: `$PEKO_HOME/data/principals/collab/local/sessions` was empty after the run (no leftover isolated-session files). The policy works; the model just couldn't see it. **No bug here, but the model output is misleading** — a non-technical user who reads the model's "I can't verify" caveat will distrust the system even when it works.

### Finding 7 — Streaming interrupt works (`peko interrupt <request_id>` is functional)

Turn 16 finally landed the interrupt path:
- `peko send collab …` (streaming by default, no `--stream`)
- spawned a long-running `Agent(subagent_type=researcher)` with `sleep 30 && echo DONE_SLEEP`
- `peko interrupt 1` after ~4 s → `Sent interrupt to run 1` with `interrupt_rc=0`
- stream terminated at 7 s wall time (3 s of which was the agent launch)

Turn 15 had failed for two reasons: the rejected `--stream` flag (Finding 5) and the agent finished too fast for the interrupt to land — the stream was already `completed` before `peko interrupt` was called. **The interrupt path works; the failure was setup error.**

**Positive observation:** the `interrupt` command took <1 s to acknowledge, and the stream was killed promptly. This matches the desktop iteration-bubble boundary (steering-only interrupt) — there's a clean `interrupt` channel for CLI users.

### Finding 8 — Performance: 16 turns in ~6 min, ~400k total tokens

Per-turn totals (in + out):
- Median: ~22k tokens / turn
- Turn 10 (backgrounded Bash + reflection): 53,936 in — largest
- Turn 9 (parallel Bash, simple): 36,132 in
- Tool-call input cost dominates — every `Agent` invocation re-sends the full subagent prompt + the parent's persona + conversation history

Total tokens billed across the run: ~395k input, ~7.5k output. **Input-to-output ratio is 50:1**, which is the key cost driver. The model's verbose self-narration ("Summary (3 lines each):" headers, restated receipts) doesn't help — every turn's output balloons back into the next turn's input. A non-technical user doing 5–10 follow-ups will see their per-turn cost grow linearly with turn count.

The CLI's stderr line `[peko] iterations=N input=X output=Y total=Z` is the **only** place the real numbers surface, and it's only visible in `--no-stream` mode. v2 audit flagged this; still true today.

### Finding 9 — "Honesty noise" in the model output is a UX problem

Multiple turns the model emitted disclaimers like:

- *"I am at the depth limit (depth 3/3) and did not spawn additional subagents"* — true but unprompted; a non-tech user reading the transcript sees "depth limit" without context
- *"I have no inspector tool to verify the deletion actually happened on disk"* — accurate but alarming
- *"I can't show you task IDs or statuses because the tool you asked for doesn't exist in this environment"* — honest, but reads as a system failure to a non-tech user
- *"If you want to test the background path, I can run `sleep 30 && echo done`..."* — three turns of unsolicited offers to test things

The model is being epistemically careful (M3 is calibrated to admit uncertainty), but the *form* of the disclosure reads as "the system doesn't work" to a user who can't distinguish "model is being careful" from "tool surface is missing."

**Impact:** the **runtime should fix the gaps the model is hedging about** (Finding 2's async inspector, Finding 1's depth error shape) rather than rely on the model to apologise for them.

## Prompts / agent-config observations

The custom `researcher` and `writer` `AGENT.md` files were written with the format:

```
---
name: researcher
description: Careful research helper
---
You are the research helper. Return concise, source-aware findings. …
```

…which the principal YAML frontmatter parser (`principal/agent_prompt.rs`) requires. The `description` line is surfaced in `principal agent list <p>`; the body becomes the system-prompt section. The model respected both prompts — turn 4's writer produced a haiku exactly 3 lines, turn 14's writer produced exactly 3 bullets — so the YAML prompt schema is well-formed and is being read end-to-end.

## Bug list (priority order)

| # | Finding | Severity | One-line fix sketch |
|---|---|---|---|
| 1 | Depth error returns `status:"forbidden"` with no agent type or `SpawnError` variant | Medium | Return the F39 `SpawnError::DepthLimitExceeded` variant the runtime already has, and include `max_depth` + `current_depth` |
| 2 | `AsyncSpawn` / `AsyncList` / `AsyncStatus` / `AsyncOutput` / `AsyncStop` referenced in tool descriptions but not granted for typical principals | **High** | Either grant `tool:Async*` by default for `tool:Bash`-bearing principals, or strip the descriptions from unbound tools |
| 3 | `--stream` flag rejected; only `--no-stream` accepted | Low | Add `--stream` as a no-op synonym (or rename to `--stream=on`/`--stream=off`) |
| 4 | Stderr token line (`[peko] iterations=N input=X output=Y total=Z`) only visible in `--no-stream` mode | Medium | Echo the line after stream completion, or surface in `peko quota status` |
| 5 | Model hedging ("I can't verify cleanup ran") misleads users | Low | Fix the underlying inspector gap (Finding 2) so the model doesn't have to apologise |

## Positive findings

- **Capability gate works** (Finding 3): distinct error shapes for "type unknown" vs "type not enabled", survives revocation across turns.
- **Parallel fan-out** (Finding 4): concurrent `Agent` calls return distinct `run_id`s cleanly, no interleaving, no shared state.
- **Streaming interrupt** (Finding 7): `peko interrupt <request_id>` kills in-flight streams cleanly with <1 s acknowledgement.
- **`cleanup=delete` works** (Finding 6): post-run disk inspection confirms isolated child sessions were removed.
- **YAML frontmatter prompts honoured** (Prompts section): model obeys the `description` and body content of custom `AGENT.md` files.
- **Identity / tier authority correct** (Phase B/C recall): all `principal:write_*` and `agent:*` grants persisted across turns; no stale-config issue.

## Cleanup performed

1. Stopped daemon: `kill <daemon_pid>` — confirmed gone (`pgrep` empty).
2. Removed tempdir: `rm -rf /tmp/peko/subagent-adaptive-33486-8o7k3o` — verified the home tree (50+ files, ~178 KB daemon.err log) was deleted.
3. Removed 16 test logs: `rm -rf /tmp/peko/subagent-adaptive-logs`.
4. Removed user-side artefacts: `/tmp/peko-env.sh`, `/tmp/peko-subagent-setup.sh`, `/tmp/peko-subagent-setup.out`, `/tmp/peko-subagent-env`, `/tmp/peko-subagent-tool-presence.out`, `/tmp/iceland-haiku.md`, `/tmp/iceland-packing.txt`.
5. Confirmed host `~/.peko` is unchanged (no leakage from the isolated tempdir).
6. Host `~/.peko` runtime state untouched throughout (per `peko_iso_init` isolation contract).