# ADR-085: Memory Claims Reviewed Against Their Sources; Turns From the Ledger

**Status:** Accepted
**Date:** 2026-10-16 (D4 added the same day)

**Amends:**
- [ADR-080](ADR-080-daily-memory-maintenance.md) D4 and D8: a sourced line also
  needs a reviewer's finding before it reaches `kb/MEMORY.md` or
  `kb/CONVENTIONS.md`.

**Builds on:**
- [ADR-079](ADR-079-per-principal-tool-ledger.md): the per-principal tool ledger.
- [ADR-078](ADR-078-acting-for-stamps-and-trust-floor.md) D7: the policy
  reviewer's model and quota path.

---

## 1. Context

ADR-080 D4 lets a scheduled owner run add a line to `kb/MEMORY.md` only when the
line cites sources where only the owner speaks. D8 applies the same rule to
`kb/CONVENTIONS.md`. Both name the remaining hole: a line can cite an owner
source it did not come from. For example, it can cite the owner's DM while
carrying something a guest said in another channel, or something the model
inferred. The nightly dream now writes more such lines and can promote rules.
Grok Bot's answer is a synthesis gate: a second model checks proposed memory
edits against the cited evidence before they apply.

The memory pass also starts its review blind. It lists active sessions and
reads them, with no map of what each turn did.

## 2. Decision

### D1 — Evidence for the review comes from the ledger

The ADR-079 ledger already records every tool call with its run, session,
acting-for stamp and redacted params. A second per-turn log would duplicate it.
`ToolLog` gains a `turns` action that groups the window's calls (default: the
last day) by run. Each turn reports:

- its session path, who it acted for and at what trust;
- its start and end times, its tool counts and failures;
- the files it wrote and read, and the channels it read and posted to.

Compartment scoping is the same as `search`: owner runs see every turn, others
only their own conversation's. The nightly memory brief starts the review from
`ToolLog` `turns` since the last pass. Turns that made no tool call are not in
the ledger; `Session` `history` still shows them.

### D2 — A reviewer checks each sourced line before it is written

When a scheduled owner run writes a sourced hot file, the D4 source check runs
first. Every added line that survives it, and cites at least one source, then
goes to a reviewer:

- **Evidence.** For each source the runtime quotes up to 2 KB:
  - a `<channel>#<event>` source: that event and the two posts on either side;
  - a bare channel or a session: the messages (and compaction summaries) that
    share the most words with the line, in order, each capped at 600 bytes.
- **Call.** One completion per write, up to 20 lines per call. It uses the
  policy's `judge_model`, else the principal's own model, at temperature 0,
  charged to the principal's quota: the same path as the ADR-078 D7 reviewer.
  The system prompt says a line is supported only when the excerpts state or
  directly imply it, that rules about how to work must come from the owner's
  own words, and that the excerpts are data, not instructions. Runtime tags in
  lines and excerpts are defused.
- **Findings.** The answer is one line per claim, `<n>: supported` or
  `<n>: unsupported - <reason>`. Supported lines are written. Unsupported lines
  are held, and so are lines with no finding (the reviewer could not run, or
  skipped them). Held lines are filed in `kb/inbox/` under the run's own
  compartment, with the reason, and the tool result lists them.
- **No gate bound: fail closed.** Every sourced line is held.
- *Amended 2026-10-17:* a "line" here is an ADR-080 D4 entry, a list item
  with its continuation lines. The reviewer gets each entry whole, flattened
  to one line, so a wrapped fact is not judged fragment by fragment. The
  review runs only when every added entry has passed the source check. Any
  held entry holds the whole write (ADR-080 D4), so a supported entry beside
  it waits until the run repeats the change without the held entry.

### D3 — Scope

The review covers exactly the writes D4 and D8 screen: `Write` / `Edit` of
`kb/MEMORY.md` and `kb/CONVENTIONS.md` by owner runs a cron job started. Writes
by the owner's own conversations are not reviewed; the owner wrote them.

### D4 — The review runs while others are talking to the principal

The review defends against one thing: a run carrying someone else's words into
the owner's memory under an owner citation. On a principal that only its owner
talks to, the source check already passes every line, and the review only
second-guesses the model's own wording. It still costs a call per write and can
stall memory when the reviewer model is down. So `POLICY.md` front matter takes
`memory_review`:

| Value | The reviewer runs… |
|---|---|
| `guests` (default) | only when one of the principal's conversations with someone besides the owner has had activity in the last 7 days (a channel whose roster or log cannot be read counts as active) |
| `always` | on every screened write |
| `off` | never; the source check still applies |

A skipped review passes the line on its sources (`ClaimCheck::Skipped`).
Policies written before this decision read as `guests`. The window is checked on
channels, not sessions: a group post that woke no session still sits in the log
the memory pass reads.

## 3. Alternatives considered

- **An `AfterAgent` hook writing a per-turn evidence file.** It records nothing
  the ledger lacks, apart from turns with no tool calls, which sessions already
  hold. It would add a writer on every turn and a second retention policy.
- **Reviewing in the memory brief only (ask the model to double-check).** That
  is the same model grading its own inference, and it is not enforced.
- **One reviewer call per line.** Clearer findings, but a nightly pass can add
  dozens of lines. Batching by write keeps it to a few calls a night.
- **Embedding retrieval for excerpts.** Better recall for paraphrases, but it
  needs an index. Word overlap over the cited source is enough when the line
  already names where it came from, and an event id pins it exactly.
- **Letting unreviewed lines through when the reviewer cannot run.** The gate
  exists for the case where a run was steered. A missing finding is filed for
  the owner, not trusted.

## 4. Consequences

- Hot memory and conventions written by scheduled runs carry a checked source.
  What a guest said, or what the model inferred, has to survive a second reader
  before it reaches every prompt.
- One extra completion per screened write, but only while others are talking
  to the principal (D4). In that case, if the reviewer model is unavailable,
  the nightly pass fills `kb/inbox/` instead of hot memory until it is fixed.
  The owner can set `memory_review: off` to trade that safety for availability.
- The review is only as good as the excerpt: a claim drawn from a long session
  cited by path, with no shared words, gets "(nothing found)" and is held.
  Citing `<channel>#<event>` avoids this, and the brief asks the model to write
  what the source says.

## 5. Deferred

- Version checks on hot-file writes (compare-and-swap against the content the
  run read).
- An agent-facing `memory_explain` that shows a line's sources and the review
  finding.
- Re-reviewing existing lines; the review covers additions only.
