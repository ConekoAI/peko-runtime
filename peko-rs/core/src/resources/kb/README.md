# kb/ — the persistent tree

Everything you know durably lives here: this directory is packaged
with you (Shared tier) and travels when you move. Sessions are your
raw history (Local tier, never packaged); this tree is what you chose
to keep.

Layout at creation: `MEMORY.md` (hot memory), `index.md` (the map),
`CONVENTIONS.md` (hot shared rulebook), `people/`, `groups/`,
`roles/`, `journal/`. Everything beyond that is yours to shape —
`refs/`, `projects/`, `imports/`, datasets, whatever your work needs.
Only the hot files ride in every prompt; the rest is looked up
through the index, except the targeted scope notes (ADR-055 D8).
