# ADR-051: Compaction Pages as an Addressable Archive

**Status:** Proposed
**Date:** 2026-09-05
**Author:** rlsn
**Related:** [ADR-042](ADR-042-no-external-session-concept.md) (session
model), [ADR-044](ADR-044-chat-session-separation.md) (agent–session
paradigm), round-7 chapter deletion (`f1f200b5`, `fdd957ad`),
compaction-audit fixes branch `compaction-audit-fixes` (`fd71ce3d` …
`a259573b`).

---

## 1. Context

Peko sessions are **continuous working memory**. Unlike harnesses with
user-bounded threads (e.g. Codex CLI, where a degraded thread is abandoned
and replaced), a peko session is managed by the agent itself or its parent
and may live indefinitely — a peer DM child, a long-running working
session. The only mechanism bounding context size is compaction, so such a
session compacts **as many times as needed**, with no user ever starting a
fresh thread.

Each compaction is a lossy re-encode: the summarizer sees the previous
summary plus recent messages, and produces the next summary. Over many
cycles this chains into summary-of-summary-of-summary, and two failure
modes compound:

- **Information loss** — details that never made it into any summary
  (or were paraphrased away) become unrecoverable *from the agent's
  point of view*, even though the raw events remain on disk.
- **Drift** — goals, constraints, and decisions slowly mutate across
  re-encodings, and the agent has no way to detect or correct the drift
  because the pre-compaction content is invisible to it.

Mitigations already shipped (structured checkpoint format with a pinned
`## Critical Context` section, PRESERVE/ADD/UPDATE update rules, verbatim
user messages preserved under a token budget — `a259573b`) reduce the
rate of loss but cannot eliminate it.

Key facts from the compaction audit (2026-09-04):

- **The archive already exists.** Compaction never truncates storage; it
  appends a `System "compaction"` boundary event carrying the summary,
  message/token counts, a per-session `compaction_number`
  (`8b3bf577`), and structured details. The full pre-compaction
  transcript stays in the stitched JSONL pages forever.
- **Resume honors the boundary** (`fd71ce3d`): `load_history` starts
  from the newest boundary, so the live context is
  `[system prompt, summary, post-boundary events]`.
- **Nothing is addressable.** There is no way for an agent (or a human)
  to ask "what did this session actually say before compaction #3?"
  short of grepping JSONL files by hand.

The lesson of round 7 constrains the shape of any fix: the old
**chapter** concept — where rotation minted a *new session id* per
chapter (`rename_session_id`, `chapters.json`, `#<timestamp>` re-keying)
— was deleted precisely because it coupled storage segmentation to
identity. Every ownership, tooling, and permission rule had to chase the
re-keying. Stable-id paging replaced it. Any page design that re-keys,
renames, or physically splits session storage repeats that mistake.

## 2. Decision

Promote compaction boundaries from an invisible storage detail to an
explicit, agent-visible **page chain** — as a *read model*, not a
storage restructure.

### D1 — Pages are logical segments of the existing append-only log

A **page** is the segment of a session's stitched event stream between
two consecutive compaction boundary events:

- page 1: genesis … first compaction boundary
- page k: boundary k−1 … boundary k
- page N+1 ("live page"): the newest boundary … now

Pages are computed by a pure scan over `load_events` output (which
already stitches rotated `<id>.N.jsonl` files transparently). No new
files, no data duplication, no re-keying, no truncation — the
append-only invariant and every existing reader are untouched. Byte-size
rotation paging (`rotate_bytes`) remains an invisible storage concern,
orthogonal to compaction pages.

### D2 — Page identity is the boundary event; links are derivable

Page k is identified by its terminating boundary event's id
(`compact_<uuid>`) and `compaction_number` (a true per-session sequence
since `8b3bf577`). The chain — "which page precedes this one" — is
derivable by scanning, so no link fields are required. Boundary events
MAY record the previous boundary's id as a convenience for O(1) chain
traversal, but the scan is the source of truth.

### D3 — Agents retrieve pages through tools, deliberately

Two read primitives, exposed as Session-tool actions (or a dedicated
tool), gated by the existing `tool:session` authorization and ownership
rules (a caller reads pages only within the subtree it manages):

- **`read_page`** — render a page's events as transcript text with
  Read-style `offset`/`limit` and a hard token cap per call. Pages are
  potentially huge; retrieval is paginated so reviewing history cannot
  silently re-inflate the context that compaction just shed.
- **`search_pages`** — keyword/regex search across all pages of a
  session, returning capped, page-tagged match snippets with page ids
  suitable for a follow-up `read_page`.

Both are plain text operations over parsed events (per-line parse
already exists). No vector search, no background indexing.

### D4 — Presence is visibility: the page catalog rides the summary message

Following the workspace-capability pattern (ADR-050 §6.6), the agent
must *know* pages exist without any restart or out-of-band signal. The
compaction summary System message gains a catalog footer:

```
<archived-pages>
This session has 3 archived page(s) from earlier compactions:
- page 1 (compaction #1, ~42k tokens): "<first user message excerpt>"
- page 2 (compaction #2, ~38k tokens): "..."
- page 3 (compaction #3, ~51k tokens): "..."
Use read_page / search_pages to retrieve archived content.
</archived-pages>
```

Because the summary message is rewritten at every compaction and
reconstructed from the boundary event on resume (fix `fd71ce3d`), the
catalog is regenerated at exactly the moments the page set changes and
is always current in the live context. No new prompt-renderer plumbing.

### D5 — Token discipline over convenience

Nothing from archived pages is ever auto-injected into context. The
catalog is metadata (one line per page). Content returns only through
explicit, capped tool calls. The design assumes the agent is deliberate:
compaction shed those tokens for a reason.

## 3. Non-goals

- **Physical archiving** — no separate page files, no moving events,
  no splits of the JSONL. (Round-7 lesson.)
- **Semantic/vector search** — keyword search suffices for "find the
  thing I remember mentioning"; revisit only with field evidence.
- **Fixing drift preemptively** — pages make drift *recoverable* (the
  agent can audit the summary against the source), not impossible.
  Summary quality remains the compaction prompt's job.
- **Human-facing surface** — `peko log`-style CLIs may adopt page
  awareness later; this ADR scopes the agent surface only.

## 4. Consequences

- Compaction becomes *lossy compression over a lossless archive* instead
  of lossy compression with no fallback. Long-lived sessions can compact
  indefinitely without irrecoverable amnesia.
- `peko-session` gains a small, pure read module (page enumeration,
  rendering, search) with no new I/O — it consumes `load_events`
  output.
- The Session tool (or a new sibling) gains 2–3 actions; authorization
  and ownership reuse existing gates.
- On-disk format: unchanged, except an optional `prev_boundary_id` in
  the compaction detail (D2) and the catalog footer in the reconstructed
  summary message (derived, not stored).

## 5. Follow-ups (not in this ADR)

- **Summary citations**: teach the compaction prompt to record which
  page key facts came from, so summaries carry references into the
  archive (summary → source navigation).
- **Human surface**: page-aware `peko log`.
- **Cross-session search**: the same primitives generalize, but
  ownership scoping needs design.
