# 2026-08-01 — Peko CLI multi-turn conversation field test (trip planning)

**Tester:** automated (Claude Code, MiniMax-M3 model), acting as a non-technical human user
**Methodology:** `scripts/e2e/` isolation framework — fresh `HOME` + `PEKO_HOME` + `PEKO_DAEMON_SOCK` per run, real `minimax-MiniMax-M3` LLM, host `~/.peko` untouched
**Built binary:** `target/debug/peko` (debug build)
**Scenario:** planning a 3-day weekend trip to Lisbon — open ended question, then a concrete follow-up, then a *correction* (vegetarian), then sanity probes on log / quota / interrupt
**Flow:** `scripts/e2e/flows/explore-multi-turn-trip.sh` (left in the tree for reuse)
**Prior reports:** `2026-08-01-non-technical-user-field-test.md` (v1), `2026-08-01-non-technical-user-field-test-v2.md` (v2)

## Why this report

The two prior passes were single-shot (one `peko send` per scenario). Real
non-technical users *don't* do that — they ask an open-ended question,
read the answer, and follow up. That follow-up is where the LLM either
remembers context, hedges, or pivots. So this pass does a real
3-turn conversation with the principal and watches what surfaces.

The persona was drafted from a single sentence via the new
`principal persona set --from …` builder (the v1 feature wish), then the
conversation ran. Sanity probes on log/quota/interrupt come after.

## Conversation transcript (with timing)

| # | User said (paraphrased) | Wall time | Result |
|---|---|---|---|
| 1 | "3-day Lisbon trip, walking + food, not-too-touristy, high-level itinerary, ~5 lines" | **7 s** | Clean, day-by-day plan with specific landmarks (Miradouro da Senhora do Monte, MAAT, Jerónimos) and a Belém-vs-Cascais choice. Closed with "want me to dig into food or walking?" |
| 2 | "Day 2 — 2-3 specific food spots in €10-15 range, drop addresses if you can" | **8 s** | **Refused to give addresses.** "I'd be making them up if I named them confidently." Listed Taberna da Rua das Flores (no address) + Cervejaria Ramiro (over budget) + Manteigaria. Offered to "spin up a food-focused sub-agent." |
| 3 | "Actually scratch day 2 — I'm vegetarian. 2-3 veg-friendly tascas, €10-15 mains." | **7 s** | **Refused again.** Same honesty disclaimer. Gave a list of *Portuguese vegetarian staples to look for on menus* instead of named spots. Offered the sub-agent research route again. |

Raw text captured in the flow's stdout under TURN 1 / 2 / 3 banners.

## Functional / behavioural findings

### Finding 1 — Quota counters: output ✓, input still ✗

After 3 real sends (~600 output tokens), `peko quota status travel-buddy`
prints:

```
📊 Quota for 'travel-buddy':
  cycle:      daily
  input:               0 / ∞          (unlimited)
  output:            636 / ∞          (unlimited)
  requests:            3 / ∞          (unlimited)
  window:     2026-08-01T00:00:00+00:00 → 2026-08-02T00:00:00+00:00
```

This is a **partial improvement** over v2 Bug A (which had everything
stuck at 0). `output` and `requests` are now metered correctly. But
`input` is still 0, and that's the **more important half of the bill**
for a non-tech user — input tokens are the system prompt + persona +
history that they pay for every turn, and they grow on every follow-up
because the conversation history rides along.

So the regression that v2 flagged (counters stuck at 0/∞) is half-fixed.
The user looking at `quota status` still sees a wildly optimistic "0
input tokens used" picture and has no idea what 3 turns actually cost
them in prompt size. The `[peko] iterations=1 input=N output=M total=K`
line on stderr is the only place the real number surfaces, and that line
is only visible in `--no-stream` mode.

**Impact:** same as v2 Bug A — quota is still not a trustworthy
cost-visibility tool. A non-tech user told to "watch your quota" will
believe they're at 0 because the meter says so.

### Finding 2 — Model offers a "sub-agent" it cannot deliver

Turn 2 ended with *"I can spin up a food-focused sub-agent to dig that
up — want me to?"* and turn 3 ended the same way.

There is no `peko …` subcommand that does what a non-tech user would
hear by "spin up a sub-agent." The closest is the daemon's general
principal send, which the user is *already running*. So the offer is
unfalsifiable: the user can't tell if it would actually be different
from what just happened.

**Impact:** this is the worst kind of UX friction for a non-tech user
because:
- it sounds like a feature the model has but isn't surfacing
- the user has no verb to accept ("yes, spin up the sub-agent")
- it makes the prior answer feel like a *placeholder*

If the model genuinely wants to keep a tool-use / search path behind a
command, `peko` should expose `peko ask <principal> --research "<query>"`
so the model can flag when it would have used it and the user can opt in
explicitly. See the **feature wish** at the bottom.

### Finding 3 — "I'd rather not invent them" is over-honest

In both turns 2 and 3 the user asked for **specific, named** food spots
and the model pivoted to "I don't have reliable current info on
addresses." That's factually defensible — the training cut-off is real,
restaurants close — but it underplays what the user actually asked for:

- Specific, named tascas with addresses (turn 2) → got 3 names but no
  addresses; one over budget; one already mentioned in turn 1 (Manteigaria)
- Specific, named vegetarian tascas (turn 3) → got zero names; just
  "look for these dishes on menus" guidance

A non-tech user planning a trip will *often* prefer the model to give
its best-guess answer with a "verify before you go" footer than to
refuse. The current system prompt / persona seems to over-weight the
"don't hallucinate" axis at the expense of the "be useful" axis.

This is a *prompting* finding, not a peko bug — but it's the most
visible UX issue for the persona-buddy use case, and it shows up
because of the *persona*, not the model. So `peko principal persona set`
is taking a default-system-prompt personality and amplifying it. Worth
noting when shipping the persona builder further.

### Finding 4 — Turn 1 was excellent (positive)

For contrast: turn 1 — open-ended, "high-level, ~5 lines" — got a
near-perfect answer. Day-by-day structure, specific landmarks, a
Belém-vs-Cascais branch with a "pick whichever suits your energy"
closer. Closed with an offer to dig deeper (which turn 2 then fumbled).

This is what the principal does *well*: structured, well-organised
travel advice with concrete points-of-interest. The same persona that
got turn 1 right refused to commit on turn 2. The difference is the
request type (open itinerary vs. specific addresses), not the model.

### Finding 5 — Persona builder wire-up is intact

`principal persona set --from "…"` ran against the live daemon and
produced a structured persona. The conversation afterwards read like
a budget-aware travel concierge (compact paragraphs, structured bullets,
offered to dig deeper). The persona worked end-to-end.

The dry-run path (`principal persona set … --dry-run`) was removed from
this flow because v1's run-case still has `set -euo pipefail` and the
dry-run path also goes through IPC → the daemon route, which works fine
on its own — it was a flow-script ordering issue, not a peko bug.

### Finding 6 — `log --json` envelope works (positive)

`peko log travel-buddy --since 1h --json` emitted a structured JSON
envelope (first character was `{`). Consistent with v2.

### Finding 7 — `quota status --json` returns 286 bytes (partial positive)

`peko quota status travel-buddy --json` returned rc=0 with 286 bytes
of stdout — the v2 Bug B "empty stdout" is fixed. We didn't `jq` it
in this flow (no jq available in the isolate env), but it's no longer
empty. Worth a follow-up to confirm the JSON shape is parseable.

### Finding 8 — Performance is solid

| Operation | Wall time | Notes |
|---|---|---|
| `model add` | <100 ms | (seed step) |
| `principal create` | <100 ms | (seed step) |
| `principal persona set` (with LLM) | <1 s in this flow | did not isolate its own timing |
| `daemon start --foreground` | <2 s | already-running daemon is instant |
| `send` turn 1 (≤300 chars in, ~250 out) | **7 s** | MiniMax-M3 + persona + history |
| `send` turn 2 (concrete follow-up, ~300 out) | **8 s** | history now ~600 tokens in |
| `send` turn 3 (correction, ~350 out) | **7 s** | history ~900 tokens in — flat latency |
| `log --since 1h --json` | <50 ms | reads JSONL, returns paginated |
| `quota status` | <50 ms | file read |
| `quota status --json` | <50 ms | 286 bytes back |

The flat 7-8 s across all three turns despite the input prompt growing
on each turn is reassuring — MiniMax-M3 is not rate-bound for a 1 KB
prompt, and the agentic loop in peko isn't doing obvious quadratic work
on the growing history. Good.

## Prompting observations (for `principal persona set` follow-up work)

- The drafted persona worked at the *style* level (compact bullets,
  structured headers, "want me to dig deeper?" closes).
- The drafted persona did **not** work at the *commitment* level — the
  persona "friendly, budget-aware travel concierge" implies the model
  should give concrete recommendations. But the underlying model refused
  to commit. Either the persona draft needs to include an explicit
  "give your best recommendation even with caveats" line, or the
  principal's system prompt needs to inject one when the persona is set.

  Hypothesis for a follow-up: append a single line to drafted personas:
  *"When the user asks for specific recommendations, give your best
  picks and add a one-line 'verify before you go' footer."*

- The model's offer to "spin up a sub-agent" leaks system architecture
  into the conversation. The persona builder should probably suppress
  references to sub-agents / internal model mechanics in the drafted
  persona — non-tech users don't have a sub-agent button to push.

## Discoverability notes (non-bugs)

- `peko ask` doesn't exist (would solve Finding 2 — see feature wish).
- `peko note` / `peko save` don't exist either (also see feature wish).
- `peko send <name> --research <query>` doesn't exist; would also close
  Finding 2 if it triggered the principal's tool-using mode.
- `peko principal persona` has `set` but no `show`/`edit`/`list` — once
  you draft a persona you can't easily review what was written without
  opening `agents/primary.md` and `principal.toml` directly. v2 Bug E
  flagged `persona show` as missing; still missing here.

## Top feature wish — `peko ask <principal> --research --save`

**Today:** a non-tech user gets a chat reply. The reply is good, but
it's ephemeral — they finish the conversation, close the terminal, and
the trip plan evaporates. If they want to *do* anything with the
research (verify addresses, share with a travel partner, revisit it on
the flight), they have to copy-paste from the terminal into Notes /
email / wherever. And when the model refuses (Finding 3), they have no
way to say "look it up and come back."

**Proposed:**
```bash
# Today: chat mode
peko send travel-buddy "2-3 veg-friendly tascas in Príncipe Real?"

# Proposed: research + persist
peko ask travel-buddy \
    --research "2-3 vegetarian tascas in Príncipe Real / Lisbon, open Sep 12-14, €10-15 mains, with addresses" \
    --save

# → triggers the principal's tool-using / sub-agent / search path,
#   streams (or blocks for) the result,
#   writes it to <PEKO_DATA_DIR>/notes/<principal>/2026-08-01-veg-tascas-lisbon.md
#   with a YAML frontmatter (date, principal, model, query, sources).
```

**Why this is the *top* wish, not just another nice-to-have:**

1. It collapses the gap between "I asked" and "I have it" — which is the
   moment a non-tech user either stays or churns.
2. It turns the model from a chat partner into a small research
   assistant with a built-in notebook. That's the experience non-tech
   users describe when they say "I want a personal assistant."
3. It solves Finding 2 (the unfalsifiable "sub-agent" offer) — when the
   model wants to dig, the user has a verb (`peko ask --research`) to
   green-light it.
4. It composes with `peko log`: today, `peko log <principal>` is a
   chat replay. After this lands, it can be a chat replay **plus** a
   linkable index of saved notes (`peko log --since 1d` shows the
   message thread, `--notes` shows the saved research).
5. It's a small surface area: one new top-level command, one new flag,
   one new on-disk convention. No protocol changes.

**Under the hood:** just synthesizes a `peko send` with a system-prompt
prefix that enables tool use and a markdown dump on completion. The
plumbing for both already exists (the daemon's tool routing + the
principal's log writer).

## Deferred / out of scope

- Didn't try `peko send --stream` (the prior tests covered streaming).
- Didn't try interruption of a *real* stream (only the offline
  not-running probe).
- Didn't try cron / extensions / mcp / plans / quota / capabilities —
  none of those are touched by a trip-planning flow.

## Cleanup performed

- The `explore-multi-turn-trip.sh` flow script is left in
  `scripts/e2e/flows/` (matches the convention from v1/v2).
- The tempdir was removed by `peko_iso_done` on exit (verified —
  `/tmp/peko/` is empty afterwards).
- No peko / peko-daemon processes survived (verified via `pgrep -fa
  peko` — only transient pgrep matches).
- This report is the only file added under `scripts/e2e/reports/`.
