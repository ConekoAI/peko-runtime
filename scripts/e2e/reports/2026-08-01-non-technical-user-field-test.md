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

---

## Follow-up commit — closing the deferred items

The four follow-up items above (the three bullets plus the top feature wish from §Deferred) were addressed in a single follow-up commit. The plan was approved up front (`/Users/rlsn/.claude/plans/warm-wishing-pixel.md`) and followed minimal-change discipline: each fix touches one or two files, every fix has a focused unit test, and the regress e2e flow gains one section per closed item.

### Fix A — destructive `--force` on `principal create`

**Today:** `peko principal create scout --model <id> --force` skips the overwrite guard but does not call `manager.remove` first — the old workspace, `agents/`, `memory/`, and session JSONL were all left in place. The doc-comment claimed destruction; the code did not.

**Change:** `peko-rs/cli/src/commands/principal.rs` — `create_principal` now, when `force==true`, branches on TTY + `--yes`:

- **TTY + no `--yes`:** prompts `Type the principal name to confirm: …`. Two-step confirmation, mirrors the existing `peko principal remove --yes` pattern.
- **TTY + `--yes` OR non-TTY:** calls `manager.remove(name).await?` synchronously, then proceeds with the existing create.
- Bail message inside the create path was reworded from "Refusing to overwrite. To replace it, remove it first" to "Removed and recreating" (only reached when `force` is true and the remove itself succeeded).

`--yes` was added as a sibling to `--force` so CI / scripted flows can run `peko principal create <name> --model <id> --force --yes`. `--yes` has no effect without `--force`.

**Tests added:**
- `peko-rs/cli`: `create_principal_force_yes_calls_remove_then_create` (write sentinel → `create --force --yes` → assert sentinel gone + new principal.toml present).
- `scripts/e2e/flows/regress-2026-08-01-fixes.sh` Fix #5 — drops a sentinel file in `agents/`, runs `peko_iso_run principal create scout --model <id> --force --yes`, asserts the sentinel is gone and a fresh `primary.md` is present.

### Fix B — JSON output for `list` and `remove`

**Today:** `peko principal list` and `peko principal remove` ignored `--json`. Users had to parse plain-text output, breaking the consistent envelope contract that bug #3 closed for `show`.

**Change:** `peko-rs/cli/src/commands/principal.rs` — `handle_principal` now forwards `json` to the `List` and `Remove` arms. Both emit envelopes that mirror the existing `log --json` / `show --json` patterns:

- `peko principal list --json` → `[{"name":"scout"},{"name":"alpha"}]` (empty list emits `[]`, not "No principals found.")
- `peko principal remove scout --json --yes` → `{"name":"scout","removed":true}` (the `--yes` is required even with `--json` — the destructive confirmation is a separate gate, not a JSON formatting concern)

**Tests added:**
- `peko-rs/cli`: `list_principals_json_emits_envelope`, `list_principals_json_empty_dir_emits_empty_array`, `remove_principal_json_emits_envelope`.
- `scripts/e2e/flows/regress-2026-08-01-fixes.sh` Fix #6 — seeds two principals, asserts the JSON array contains both names, runs `remove --json --yes`, asserts the `{removed:true, name:…}` envelope, removes the second, asserts the empty list emits `[]`.

### Fix C — `peko quota list` alias

**Today:** `peko quota` exposed `status / set / reset`. Discoverable via `--help` but inconsistent with `principal list`, `model list`, etc. — a user reaching for `list` got a clap parse error.

**Change:** `peko-rs/cli/src/commands/quota.rs` — added `List { name: String, #[arg(long)] peer: bool }` to the `QuotaCommands` enum. The dispatch arm delegates to the existing `status(name, peer, json)` helper, so no new logic. The dispatch preserves `status` so existing scripts (`peko quota status <name>`) keep working.

**Tests added:**
- `peko-rs/cli`: `cli_quota_list_parses_with_name_and_peer_flag` (parses `peko quota list scout --peer`).
- `scripts/e2e/flows/regress-2026-08-01-fixes.sh` Fix #7 — confirms `quota list --help` documents the `<NAME>` argument, then runs `peko_iso_run quota list scout` and `peko quota status scout` back-to-back. Both must fail with the same `Daemon is not running` error (or both succeed); the rc + stderr match proves the dispatch arm wires `list` to the same code path as `status`.

### Fix D — `peko principal persona set <name> --from "…"` (top feature wish)

**Today:** `peko principal create <name> --model <id>` produces a principal whose default prompt is `You are <name>, a helpful AI assistant. Respond to the caller's message concisely.` A non-technical user has no in-CLI way to specify a role, goals, style, or values — they must hand-edit `principal.toml` + `agents/primary.md`. This is the moment a non-technical user either stays or churns.

**Change:** A new one-shot persona-draft surface that the user reaches with one command:

```bash
# Preview only — no on-disk changes
peko principal persona set <name> \
    --from "a senior rust reviewer who cites the borrow checker and doc.rust-lang.org" \
    --dry-run

# Draft + write + diff (default behavior)
peko principal persona set <name> --from "…"
```

Default behavior (one shot, matches the filed wish literally):
1. Resolve the model — prefer explicit `--model <id>`, else fall back to the principal's existing `preferred_model_id`, else error in non-TTY.
2. Connect to the daemon (`DaemonClient::connect()`).
3. `client.persona_draft(model_id, from)` — sends a `PersonaDraft` packet, awaits `PersonaDrafted` by `request_id`.
4. Parse the LLM's JSON into a `PersonaDraft` struct.
5. Write the persona fields into `principal.toml` (mutate `[identity]`, populate `[intent.goals]`, `[intent.values]`, etc.) and `agents/primary.md` (frontmatter + drafted body with `{{memory}}` placeholder). The write path starts from the *existing* file and patches the persona sections rather than overwriting wholesale — a real principal may already have capabilities, quota, etc.
6. Print a unified diff of what changed and exit 0. If the diff is empty (drafted values identical to existing), exit 0 with `Persona unchanged.`

With `--dry-run`: draft only, render the preview (Identity / Goals / Values / Style / Primary prompt sections), exit 0 without writing.

**Wire protocol** — new `PersonaDraft` / `PersonaDrafted` IPC variants in `peko-rs/core/src/ipc/packet.rs`. The handler is a thin pass-through to `Provider::chat_with_system` guided by a hardcoded system prompt (~40 lines) that instructs the LLM to emit *only* a JSON object matching the schema:

```json
{
  "display_name": "…",
  "description": "…",
  "goals": ["…"],
  "values": ["…"],
  "style": "…",
  "primary_md_body": "<markdown including the literal {{memory}} marker>"
}
```

If the model returns non-JSON, the handler returns `Ok(PersonaDrafted { content, parse_ok: false })` and the CLI surfaces a "Could not parse structured persona; falling back to free text" preview — the LLM's bytes still reach the user.

**Boundary rules (F6):** `peko-rs/core/src/ipc/handlers/persona.rs` defines the `PersonaHost` trait (3-trait surface: `draft_persona(model_id, system, from) -> String`). `daemon/state.rs` implements it on `AppState`, reaching the `LlmResolver` and `Provider` directly. The handler module must not import any other `ipc::handlers::*` module — handlers are independent domains.

**Files touched:**
- `peko-rs/core/src/ipc/packet.rs` — `PersonaDraft` (request) + `PersonaDrafted` (response) variants with roundtrip tests.
- `peko-rs/core/src/ipc/handlers/persona.rs` (new) — `PersonaHandler` + `PersonaHost` trait + `DRAFT_SYSTEM_PROMPT` constant. 3 unit tests (matches-only, draft envelope round-trip with parse_ok=true, draft envelope carries parse_ok=false for fallback).
- `peko-rs/core/src/ipc/handlers/mod.rs` — registered the handler (dispatcher array 17 → 18 entries).
- `peko-rs/core/src/daemon/state.rs` — `PersonaHost` impl on `AppState` (validates model_id against catalog, calls `resolver.build_provider` + `provider.chat_with_system(temp=0.7)`).
- `peko-rs/core/src/ipc/client.rs` — `DaemonClient::persona_draft(model_id, from) -> ResponsePacket` next to `principal_create`.
- `peko-rs/cli/src/commands/principal.rs` — `PrincipalSubcommand::Persona(PersonaCommands)` + `PersonaCommands::Set { name, from, dry_run, model, json }`. Helpers: `PersonaDraft` struct, `render_persona_preview`, `apply_persona_to_disk`, `diff_preview`, `handle_persona_set`. 5 unit tests.

**Test counts:**
- `cargo test -p peko-cli --bin peko` → 81 passed (was 72; added 9 new tests).
- `cargo test -p peko --lib --features test-utils` → 1396 passed (was 1393; added 3 new — 2 packet round-trip + 1 persona handler).
- `scripts/e2e/run-case.sh regress-2026-08-01-fixes` → 9 assertions, all pass (Fix #1/2/3/4 unchanged + new Fix #5/6/7).
- `scripts/e2e/run-case.sh persona-builder` (new) → 3 steps, all pass:
  - **Step 1 — `--dry-run` is preview only.** Calls `peko principal persona set blank --from "…" --dry-run`, asserts the stdout preview contains all four sections (`Identity:`, `Goals:`, `Values:`, `Style:`, `Primary prompt`), asserts the stderr footer reads `(dry-run: principal 'blank' was not modified)`, asserts the on-disk `principal.toml` and `primary.md` MD5s are unchanged before/after.
  - **Step 2 — default writes + diffs.** Calls `peko principal persona set blank --from "…"` (no `--dry-run`), asserts the unified-diff banner is present, asserts `principal.toml` now has `[intent.goals]` populated with a multi-line array, asserts `primary.md` contains the borrow-checker / reviewer vocabulary *and* the literal `{{memory}}` placeholder.
  - **Step 3 — drafted persona behaves.** Sends a borrow-checker scenario (`fn main() { let mut v = vec![1]; let a = &mut v; let b = &mut v; a.push(2); }`) to the drafted principal, asserts the reply contains one of `borrow | lifetime | &mut | error[E0…]`.

### Non-trivial side bug found during e2e

The `persona-builder` flow failed on the first run with `Failed to parse request packet: unknown variant persona_draft` — the daemon IPC server rejected the new packet. Root cause: `target/debug/peko-daemon` (a separate workspace binary) was stale relative to the freshly-rebuilt `target/debug/peko` CLI. The CLI's packet enum had `PersonaDraft` but the daemon's enum did not. Fix: `cargo build -p peko-daemon --bin peko-daemon`. Worth a CI guard rail so a CLI-only rebuild doesn't silently leave the daemon binary out of sync.

### Test + clippy status

- `cargo test -p peko-cli --bin peko` → 81 passed, 0 failed
- `cargo test -p peko --lib --features test-utils` → 1396 passed, 0 failed, 1 ignored
- `cargo clippy -p peko -p peko-cli --all-targets -- -D warnings` → my new code is clean. The only blocker is a pre-existing `field_reassign_with_default` / `unnecessary_map_or` lint in `peko-provider-api/src/retry_config.rs` and `peko-provider-api/src/retry_after.rs` that exists on `master` (verified by `git stash` + re-run); out of scope for this follow-up.

### Out of scope (deferred a second time)

- Declarative flags on `peko principal create` (`--display-name`, `--persona`, `--goal`, `--style`). The LLM-drafting path is the harder piece; flags are a separate, smaller follow-up.
- `peko principal persona edit <name> --style "…"` (single-field edit). The same drafting path could be reused with `--from "more terse, bullet points preferred"`.
- `peko principal persona show <name>`. `peko principal show --json` exposes everything; a dedicated persona view is sugar.

