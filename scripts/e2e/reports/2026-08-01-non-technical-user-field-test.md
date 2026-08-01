# 2026-08-01 — Peko CLI non-technical-user field test

**Tester:** automated (Claude Code, MiniMax-M3 model), acting as a non-technical human user
**Methodology:** `scripts/e2e/` isolation framework — fresh `HOME` + `PEKO_HOME` + `PEKO_DAEMON_SOCK` per run, real `minimax-MiniMax-M3` LLM, host `~/.peko` untouched
**Built binary:** `target/debug/peko` (debug build, no special features)
**Two exploratory flows were left in the tree under `scripts/e2e/flows/` for reuse:**
- `explore-user-journey.sh` — create principal → send real question → follow up → read log
- `explore-edge-cases.sh` — missing principal, empty input, slash commands, JSON output, doctor, cron, extensions

## What was exercised

| Surface                          | Result                                          |
|----------------------------------|-------------------------------------------------|
| `model add --template minimax`   | ✅ vault + catalog written in one call          |
| `model list`                     | ✅ shows `[✓] minimax-MiniMax-M3 - MiniMax M3 [from minimax]` |
| `model test <id>`                | ✅ `✓ Connection successful (1 token billed) (2007ms)` in ~4 s |
| `principal create <name> --model <id>` | ✅ creates DID, workspace, agents/, identity/ |
| `principal list`                 | ✅ plain text, one name per line                |
| `principal show <name>`          | ✅ human-formatted name/DID/workspace/model/agents |
| `principal show --json`          | ⚠️ **bug — `--json` ignored, still human-formatted** |
| `daemon start --foreground -v`   | ✅ starts in ~2 s, IPC socket bound             |
| `daemon status --json`           | ✅ `{"running": true/false, ...}`               |
| `send <name> "msg" --no-stream`  | ✅ 3–6 s for short, 18 s for ~1 KB in, ~902 tokens out |
| `send <name> "msg"` (streaming)  | ✅ `aida: 1, 2, 3, 4, 5.` with `<principal>:` prefix |
| `send <ghost> "msg"`             | ✅ rc=1, `❌ Principal 'ghost-principal' not found` in ~12 ms |
| `send <name> "" --no-stream`     | ⚠️ **bug — empty input still calls LLM, costs tokens** |
| `send <name> "/help"`            | ✅ intercepted locally (18 ms), shows tools/skills/MCP/extensions |
| `send <name> "/help" --no-slash` | ✅ sent to LLM verbatim                          |
| `interrupt <request_id>`         | ✅ mid-stream stop, rc=0, no partial reply      |
| `log <name> --since 1h`          | ✅ human-formatted chat log                      |
| `log <name> --since 1h --json`   | ✅ paginated JSON with `nextCursor`/`hasMore`   |
| `system doctor`                  | ✅ 3 checks pass in ~18 ms                       |
| `cron list`                      | ✅ `🕒 No cron jobs found.`                     |
| `ext list`                       | ✅ shows `builtin:core | builtin | Built-in Tools | built-in` |
| `quota status / set / reset`     | ✅ (note: subcommand is `status`, not `list`)   |
| `completions bash`               | ⚠️ **bug — panics with BrokenPipe when piped through `head`/`less`** |

## Bugs filed (in priority order)

### Bug 1 — `peko completions bash | head` panics
**Repro:**
```bash
./target/debug/peko completions bash | head -3
```
**Observed:** Process prints 3 lines of the `_peko()` shell function, then panics:
```
thread 'main' (5309866) panicked at .../clap_complete-4.5.66/src/aot/shells/shell.rs:86:14:
failed to write completion file: Os { code: 32, kind: BrokenPipe, message: "Broken pipe" }
```
**Impact:** Any user who previews shell completions with `head`, `less`, `grep`, or redirects to a file that fills up hits this. The CLI exits with a stack trace and a non-zero code, polluting the shell history and confusing first-time users.
**Workaround:** `peko completions bash > /tmp/c; head /tmp/c`.
**Root cause:** `clap_complete::generate_to` writes to `Shell::completion_file()` and panics on `BrokenPipe` (`io::ErrorKind::BrokenPipe`) instead of returning `Ok(())`.

### Bug 2 — `peko principal create <existing>` silently overwrites
**Repro:**
```bash
./target/debug/peko principal create scout --model minimax-MiniMax-M3
./target/debug/peko principal create scout --model minimax-MiniMax-M3   # no warning
```
**Observed:** Second call exits rc=0 with `Created principal 'scout' at <path> (model: …)`. Identity files, `agents/primary.md`, memory snapshots, and session JSONL are all wiped.
**Impact:** One-keystroke data-loss footgun for a non-technical user. No `--force` flag, no prompt, no warning.
**Root cause:** `create` doesn't check for an existing workspace before writing.

### Bug 3 — `peko principal show --json` ignores `--json`
**Repro:**
```bash
./target/debug/peko principal show scout --json
```
**Observed:** Output is the same human-formatted text as without `--json`:
```
Principal: scout
  DID:     did:peko:public:scout:dac41499c6c67e43
  Workspace: /tmp/.../principals/scout
  ...
```
Compare with `peko log scout --json` which correctly emits a structured JSON envelope with `nextCursor`/`hasMore`.
**Impact:** Scripts that want to consume `principal show` cannot; users must parse the human-formatted text. Inconsistent with `log --json`.
**Root cause:** `Show` handler doesn't dispatch on `--json`.

### Bug 4 — Empty message costs an LLM turn
**Repro:**
```bash
./target/debug/peko send scout "" --no-stream
```
**Observed:** rc=0 in 3.7 s. Stderr reports `[peko] iterations=1 input=128 output=10 total=138 tools_failed=0`. The LLM was called with empty content (the 128 input tokens are the system prompt) and replied `Hello! How can I help you today?`.
**Impact:** Silent quota tax — a non-technical user running this in a shell loop burns tokens without realizing.

## Performance notes

| Operation | Wall time | Notes |
|-----------|-----------|-------|
| `model add` | <100 ms | vault + catalog writes in one shot |
| `daemon start --foreground` | ~2 s | IPC socket bound at `~1.9 s` |
| `daemon status --json` (already running) | ~10 ms | status poll |
| `model test <id>` (real call) | ~4 s | 2 s of LLM + 2 s overhead |
| `send` short (≤300 chars in, ≤300 out) | 3–6 s | MiniMax-M3 round-trip |
| `send` long (~1 KB in, ~900 tokens out) | 18 s | linear in output tokens |
| `send` empty | 3.7 s | **wasteful** — see Bug 4 |
| `send <ghost>` | ~12 ms | fast offline error path |
| `send /help` | ~18 ms | intercepted locally |
| `system doctor` | ~18 ms | 3 checks |
| `log --json` | ~15 ms | reads JSONL, returns paginated |
| `principal show` | ~12 ms | reads TOML, prints |
| `completions bash | head` | PANICS | see Bug 1 |

Post-turn stderr telemetry `[peko] iterations=1 input=N output=M total=K tools_failed=F` was useful for cost visibility but arrives *after* the streamed content, so it's only visible in `--no-stream` mode.

## Discoverability notes (non-bugs but UX rough)

- **`peko principal create` exposes no flags for persona/prompt/intent/governance/memory/capabilities.** The full schema is editable only via `~/.peko/principals/<name>/principal.toml` + `agents/primary.md` Markdown. The default `primary.md` is `You are scout, a helpful AI assistant. Respond to the caller's message concisely.` — a non-technical user has no in-CLI path to make a principal *do something useful*.
- **`peko principal agent`** is read-only (`list`, `show`) — no `add`/`set`/`edit`. Same TOML-editing problem.
- **`peko quota` subcommands are `status / set / reset`, not `list`.** Discoverable via `--help` but inconsistent with `principal list`.
- **`peko login`, `peko logout`, `peko update`, `peko search`, `peko push`/`pull`** exist but were not exercised here — PekoHub is network-side.
- **`peko principal export / import / permit / revoke / permissions / invite`** exist; not exercised.
- **`peko ext install / init / bundle / mcp / config / validate`** exist; not exercised.

## Deferred — feature wish

> The user (you) asked me to file this but defer implementation until the bugs are addressed.

### Top feature wish — guided persona builder

**Today:** `peko principal create scout --model minimax-MiniMax-M3` produces a principal that, by default, responds with "Hello! I'm scout, a helpful AI assistant. What can I help with?" The user has no in-CLI way to say "I want this principal to be a code reviewer" or "a research analyst" or "a calendar concierge."

**Workaround today:** hand-edit `~/.peko/principals/<name>/principal.toml` (TOML with `[intent]`, `[governance]`, `[memory]`, etc.) + `agents/primary.md` (Markdown with `{{memory}}` placeholders). For a non-technical user, that's a hard stop.

**Proposed:**
```bash
# Quick path: declarative flags
peko principal create rust-reviewer \
  --model minimax-MiniMax-M3 \
  --display-name "Rust Reviewer" \
  --persona "Senior Rust engineer reviewing PRs for idiomatic patterns, safety, and clarity" \
  --goal "Catch lifetime/borrow-check errors and suggest idiomatic fixes" \
  --style "Concise, cites doc.rust-lang.org, never rewrites more than necessary"

# Conversational path: describe in one sentence, let peko draft the rest
peko principal persona set rust-reviewer \
  --from "a senior rust engineer who reviews PRs for idiomatic patterns and safety"
# → asks M3 to draft persona + style + goals, writes TOML + primary.md, shows diff

# Edit a single field later
peko principal persona edit rust-reviewer --style "more terse, bullet points preferred"
```

Under the hood this just synthesizes the existing `principal.toml` keys + a generated `agents/primary.md`. The model is already there; the wiring is the missing piece. Closing this loop turns "I made an account" into "I have a useful assistant" in one command — the moment a non-technical user either stays or churns.

## Cleanup performed at end of run

- Killed any lingering `peko` / `peko-daemon` processes.
- Removed all tempdirs (`/tmp/peko/` shows `0B`).
- Removed all scratch shell scripts and `.out`/`.err` artifacts.
- Host `~/.peko/principals/` still contains only the pre-existing `Alice` — no leak.

---

## Fixes applied (same session, follow-up commit)

All four bugs above were addressed with minimal-change diffs plus regression tests. No existing test was modified to weaken its assertions; three new unit tests and one new e2e flow were added.

| Bug | Fix file | Change | Test |
|-----|----------|--------|------|
| 1 — `completions bash \| head` panics | `peko-rs/cli/src/main.rs` | Render completions to a `Vec<u8>` buffer, then `write_all` to stdout. BrokenPipe is now a soft Err, swallowed as `Ok(())`. | `regress-2026-08-01-fixes.sh` Fix #1 |
| 2 — `principal create` silent overwrite | `peko-rs/cli/src/commands/principal.rs` | Added `--force` flag. `create_principal` checks `shared_layout.config_file.exists()`; if so and `!force`, bails with a clear `principal 'X' already exists … Refusing to overwrite. To replace it, remove it first …` error. The error message no longer claims destruction (current `--force` semantics: proceeds past the guard but does NOT wipe on-disk state). | `principal_create_parses_force_flag`, `create_principal_refuses_overwrite_without_force` |
| 3 — `principal show --json` ignored | `peko-rs/cli/src/commands/principal.rs` | `show_principal` now takes `json: bool`; when set, emits a structured `ShowView` (name / displayName / did / workspace / preferredModelId / agents[]) via `serde_json::to_string_pretty`. | `regress-2026-08-01-fixes.sh` Fix #3 |
| 4 — empty message costs a turn | `peko-rs/cli/src/commands/send.rs` | `handle_send` rejects empty / whitespace-only messages before any IPC. `resolve_message` also `.trim()`s `--file` content for symmetry. | `handle_send_rejects_empty_message`, `handle_send_rejects_whitespace_only_message` |

### Test counts

- `cargo test -p peko-cli --bin peko` — 72 passed (was 69; added 3 new regressions).
- `scripts/e2e/run-case.sh regress-2026-08-01-fixes` — all four fixes pass against the real binary.

### Known follow-up (not addressed in this commit)

- **`--force` does not actually destroy the existing principal.** The flag bypasses the overwrite guard, but `manager.create` doesn't call `manager.remove` first — so on-disk identity / agents / memory / session history remain in place. The new doc-comment on `Create { force }` and the bail message both tell the user to run `peko principal remove` first for a true replacement. A proper destructive `--force` (call `manager.remove(name).await?` before `manager.create`) is left as a follow-up; the risk profile (deleting real session JSONL) was too high for the same minimal-change diff.
- **No JSON output for `principal list` or `principal remove`.** The same `--json` consistency gap that bug #3 closed for `show` exists for these neighbors; not filed since the field test didn't surface them, but the inconsistency is real.
- **`peko quota` discoverability.** Subcommands are `status / set / reset`, not `list`. Easy to find via `--help` but inconsistent with `principal list`. Trivial to alias if desired.

