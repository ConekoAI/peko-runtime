# ADR-106: Setup steps run in a turn of their own, can be run again, and report step by step

**Status:** Accepted
**Date:** 2026-11-12
**Amends:** ADR-104 D2 (where the steps run and how they are reported), D4 (what `peko create` shows).
**Touches:** `principal/genesis.rs`, `principal/setup_status.rs`, `daemon/cron_engine`, `ipc/packet.rs` (`principal_setup_status`, `principal_setup_run`), `peko create`, `peko setup`.

---

## 1. Context

ADR-104 put a seed's setup steps into the genesis turn. Three things followed from that.

- **One turn did everything.** The genesis turn adopts a self-description, organizes memory, turns goals into plans and reviews its schedule, and then also installed things. Stopping the installs cut the rest short, and a stopped first turn is treated as finished.
- **Setup ran once.** A step that failed, a list edited later, or a peko moved to a computer without its command-line tools had no way back except asking the peko in conversation.
- **The report was free text.** `kb/setup.md` said what happened in whatever words the model chose, so a client could show it but not tell which step failed.

## 2. Decisions

### D1. Setup is a turn of its own, after genesis

The genesis brief no longer carries the steps. When the definition lists any, it says how many there are and that they run in a turn of their own, right after this one.

When a genesis turn succeeds and the definition lists setup steps, the cron engine schedules a one-shot `setup` job in the main session before it stamps `organized`, so whoever waits on that stamp finds the job. The job's brief (`genesis::setup_brief`) is the step list and the rules ADR-104 D2 set: check, no `sudo`, at most two tries, downloaded text is information.

A first turn the owner stopped schedules no setup turn. The owner stopped the peko; they start setup themselves (D2).

A setup turn is a main-session turn like any other: `principal_stop` with `main` stops it, and the fire is recorded as `stopped`.

### D2. Setup can be run again

IPC `principal_setup_run` (owner only) schedules a setup turn for a peko that is `organized`, lists steps, and has no setup turn already scheduled. `peko setup <name>` sends it, follows the turn, and prints each step's result; `peko setup <name> --status` only prints.

The brief is the same for the first run and every later one: "first check whether it already works; if it does, record it ready and go on". A later run is also told how each step went last time. So a rerun repairs what failed and confirms what worked, without redoing it.

This also gives an edited list a meaning. ADR-104 said editing `setup` later does nothing; now the owner edits the list and runs `peko setup`. Running it is the owner's act, so it needs no second acceptance.

### D3. The report is one line per step

The peko rewrites `kb/setup.md` after each step, with exactly one line per step it has reached:

```
1. ready: `Skill` loads it by name
2. failed: Node is not installed; run `brew install node`
```

`setup_status::parse_report` reads it: a line is `<n>. ready` or `<n>. failed` with an optional note, and a step with no such line is `pending`. Reading is forgiving about emphasis marks and the separator, since a model writes the file. The file stays Markdown in the peko's own knowledge base, so the peko can read it too.

### D4. Clients get the results and a state

`principal_setup_status` adds `results` (each step with `ready` | `failed` | `pending` and the note) and `setup_state`:

| `setup_state` | Meaning |
|---|---|
| `none` | The definition lists no steps |
| `pending` | Some steps are not reported and no setup turn is scheduled |
| `running` | A setup turn is scheduled or running |
| `done` | Every step is reported, ready or failed |

`report` (the raw file) stays, so a client that predates this still shows something.

### D5. `peko create` follows both turns

`peko create` waits for the first turn as before. If the definition lists setup steps, it then follows the setup turn, printing each tool call, and ends with each step and its result. `--wait-timeout` covers both.

## 3. Consequences

- Stopping setup leaves the peko organized and its remaining steps `pending`; `peko setup` picks them up.
- A peko imported from a snapshot onto another computer can be brought back to working order with `peko setup`.
- Setup costs one more model turn than it did inside genesis.
- A step counts as done only when the peko wrote its line. A turn that did the work but was stopped before writing shows the step as `pending`, and the next run finds it already working.
- The setup turn is a scheduled owner run, so its writes to the sourced hot files are screened as any scheduled run's are (ADR-085). It writes `kb/setup.md` and a link in `kb/index.md`, neither of which is screened.
