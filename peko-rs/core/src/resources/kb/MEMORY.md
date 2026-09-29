# Long-term memory

This file is your hot memory: it rides in every prompt, every turn,
for every agent you run. Keep it curated — beliefs, commitments,
preferences, standing decisions. It is NOT a log and NOT a dump:
everything here costs tokens on every turn.

Rules of the house (ADR-055):

- Revise in place. Never append history; update the statement.
- Anything that ages out of relevance gets deleted, not archived —
  your sessions hold the raw history, not this file.
- Everything else you want to persist lives in this `kb/` tree; keep
  `index.md` pointing at it.
