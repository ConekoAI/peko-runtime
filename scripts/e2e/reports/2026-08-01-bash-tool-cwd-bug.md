# 2026-08-01 — Peko CLI Bash tool cwd bug (root cause of unhelpful replies)

**Tester:** automated (Claude Code, MiniMax-M3 model), acting as a non-technical human user
**Methodology:** `scripts/e2e/` isolation framework — fresh `HOME` + `PEKO_HOME` + `PEKO_DAEMON_SOCK` per run, real `minimax-MiniMax-M3` LLM
**Built binary:** `target/debug/peko` (debug build)
**Prior reports this builds on:**
- `2026-08-01-non-technical-user-field-test.md` (v1 — surfaced "I'd rather not invent" pattern)
- `2026-08-01-non-technical-user-field-test-v2.md` (v2 — verified fixes; left quota/persona bugs)
- `2026-08-01-non-technical-user-multi-turn-trip.md` (this morning — multi-turn Lisbon trip)

## TL;DR — the structural bug

**The Bash tool is registered in every principal's tool catalog, but it
silently fails for every fresh principal because its default working
directory points to a directory that does not exist.**

The default `cwd` for the `Bash` tool comes from
`PathResolver::agent_workspace(".").parent()`, which resolves to
`<data_dir>/workspaces/.`. That directory is **never created** during
`peko principal create` or during daemon startup — it's only created
lazily when an agent actually initialises its workspace. So the very
first time a fresh principal tries to run a shell command,
`tokio::process::Command::current_dir(<data_dir>/workspaces/.)` fails
because the path doesn't exist, and the tool returns
`Error: Failed to execute Bash command` for *every* invocation — even
`echo HELLO_FROM_BASH`.

That single structural failure is the cause behind:

1. The "I'd rather not invent them" pattern in turns 2/3 of the
   Lisbon-trip conversation (the model *cannot* curl, *cannot* spawn a
   research subagent that has different capabilities, and *cannot* write
   to a file via shell)
2. The "I'd be making them up if I named them confidently" follow-up
   across both turns 2 and 3
3. The model offering to "spin up a sub-agent" twice — even though the
   only registered subagent type is `primary` (same toolset), so the
   offer is structurally empty
4. The model recommending the user "check HappyCow or recent Google
   Maps reviews" — telling the user to look themselves because the
   principal literally cannot

The principal is *registered* as having Bash and Agent tools. It
*appears* to have research capability. It doesn't.

## Reproduction (4 probes, all from `scripts/e2e/flows/`)

Flow files left in the tree for reuse:
- `explore-tool-presence-probe.sh` — A/B/C: explicit Bash + curl,
  explicit Agent spawn, open-ended (no Bash).
- `explore-bash-baseline.sh` — escalating: echo, which curl, https,
  proxy. Every call returns `Error: Failed to execute Bash command`.
- `explore-bash-env-dump.sh` — definitive: tries to write
  `id; pwd; env; ls /tmp` output to a workspace file, then Read it back
  via the Read tool. Bash fails; Read works. Confirms the Bash failure
  is at the cwd layer, not at network egress.

### Probe A — "use Bash with curl"
> "Use the Bash tool with the command: `curl -sS
> 'https://en.wikipedia.org/wiki/Vegetarian_cuisine' | head -c 1500`."

Model reply: *"The Bash tool refused to execute the command — it appears
network access to external URLs (curl to en.wikipedia.org) is blocked or
disabled in this environment."* (Model guess; not the real cause.)

### Probe B — "spawn a research subagent"
> "Use the Agent (subagent) tool to spawn a research subagent that
> returns a one-paragraph answer about vegetarian food in Lisbon."

Model reply: *"The Agent tool call failed because the subagent type
'researcher' isn't registered — only `primary` is available. … If you'd
like, I can retry with `subagent_type: 'primary'`…"*

Daemon log confirms a `primary` subagent ran (`agent=root`); the model
*did* try `researcher` first, fell back to `primary`, and that subagent
produced 1527 chars of training-data-driven output with a
"verify before traveling" caveat. So a "research subagent" is a model
hallucination — there's only `primary`.

### Probe C — open-ended (model on its own)
> "What's a good vegetarian tasca in Lisbon's Príncipe Real
> neighbourhood with €10-15 mains?"

Model reply: *"I don't have reliable, verified information about a
specific vegetarian tasca in Príncipe Real with mains in the €10–15
range. … I'd risk pointing you to a place that has closed, rebranded,
or moved."*

Then tells the user to check HappyCow and Google Maps. The principal
*cannot* do any of this itself.

### Probe D — the smoking gun

| Command attempted via Bash tool | Outcome |
|---|---|
| `echo HELLO_FROM_BASH` | `Error: Failed to execute Bash command` |
| `which curl` | `Error: Failed to execute Bash command` |
| `curl -sS --max-time 8 https://example.com` | `Error: Failed to execute Bash command` |
| `curl -sS --max-time 8 https://api.allorigins.win/raw?url=…` | `Error: Failed to execute Bash command` |
| (Write tool with `content: 'written_via_Write_tool\n'`) | ✅ `from_write.txt` written, 22 bytes on disk |
| (Read tool to read `from_write.txt` back) | ✅ readback worked |

The Bash tool fails identically for `echo`, `which`, and `curl`. The
Read and Write tools work fine. The Bash tool is broken specifically;
the workspace tools are fine.

## Where the bug lives

### ToolRuntime registration

`peko-rs/core/src/engine/tool_runtime.rs:127-176` (`register_builtins`):

```rust
let workspace = path_resolver
    .agent_workspace(".")            // <data_dir>/workspaces/./personal
    .parent()                         // <data_dir>/workspaces/.
    .map(PathBuf::from)
    .unwrap_or_else(|| PathBuf::from("."));

let tools: Vec<Arc<dyn Tool>> = vec![
    Arc::new(BashTool::new().with_workspace(workspace.clone())),
    Arc::new(ReadTool::new().with_workspace(workspace.clone())),
    …
];
```

So every built-in is registered with the **same** `workspace` value:
`<data_dir>/workspaces/.` (parent of `agent_workspace(".")`, which is
`<data_dir>/workspaces/./personal`).

### BashTool::execute_command_blocking

`peko-rs/core/src/tools/builtin/bash.rs:155-205`:

```rust
let mut cmd = Command::new(SHELL);
cmd.arg(SHELL_ARG).arg(command);

if let Some(dir) = working_dir {
    cmd.current_dir(dir);          // ← fails here
}

let output_fut = cmd.output();     // ← never gets this far
tokio::pin!(output_fut);

let output = match timeout_ms {
    …
    _ => tokio::select! {
        res = &mut output_fut => res.context("Failed to execute Bash command")?,
        …
    },
};
```

When `working_dir` is Some but doesn't exist, `cmd.current_dir(dir)`
sets the requested CWD but the subsequent `cmd.output()` fails to spawn
the child (chdir fails in the forked child). The error is collapsed
into `"Failed to execute Bash command"` with no indication of the
cwd-missing cause.

### Why Read/Write don't suffer the same fate

`ReadTool`/`WriteTool` don't spawn subprocesses — they use
`tokio::fs::*` which fails gracefully (or auto-creates parent dirs in
the Write path). That's why Probe D's Write call succeeded while
every Bash call failed: the Bash failure is *specifically* the spawn
time `chdir` step.

### Why the workspace dir is missing

`PathResolver::agent_workspaces_root` is only consulted when a
principal's agent actually initialises its workspace (lazy creation in
`Agent::init_builtins_async` or thereabouts — see
`peko-rs/core/src/agents/agent.rs:165`). For a fresh principal that
has never had an `Agent::init_builtins_async` call land on this
specific tool-runtime instance, the directory is absent.

The Bash tool's `with_workspace` value gets baked in once at
`register_builtins` time — which happens *before* any agent runs. So
on the very first principal, the workspace dir doesn't yet exist, and
Bash fails for every command.

## What's *not* the cause

To head off red herrings:

- **Not the capability gate.** The principal's
  `~/.peko/principals/<name>/principal.toml` shows
  `grants = ["tool:Read", "tool:Write", "tool:Edit", "tool:Bash",
  "tool:Agent", …]`. `tool:Bash` is granted. (Verified by reading the
  on-disk principal.toml from a kept tempdir.)
- **Not network egress.** `echo HELLO_FROM_BASH` doesn't make a
  network call. It fails the same way as the curl calls. The model's
  earlier "network access … is blocked" guess was wrong.
- **Not a sandbox profile.** There's no seccomp/App Sandbox/sandbox-exec
  policy in the Bash tool's source; the module's docs explicitly say
  *"No sandboxing, no command blocking, no env filtering"* (ADR-014).
- **Not Bash itself.** `/bin/sh -c 'echo HELLO'` from the shell works
  fine. The daemon's process can spawn shell commands in general.
- **Not the `subagent_type` model.** Subagent spawn actually works
  (Probe B's `primary` subagent ran and produced 1527 chars of output).
  The "researcher" subagent type the model asked for doesn't exist —
  only `primary` is registered — but the user-facing problem is that
  even *with* a subagent, the same cwd bug means the subagent has no
  shell access either.

## Impact

| User-facing symptom | Underlying cause |
|---|---|
| Model says "I'd rather not invent them" for any current-info question | No way to look anything up: Bash is dead + no built-in `web_search` / `fetch` / `http` tool (`peko-rs/core/src/tools/mod.rs:48` confirms heavy tools are MCP-only) |
| Model offers to "spin up a sub-agent" but the offer is empty | Only `primary` is registered; subagents inherit the same broken Bash |
| Model recommends "check HappyCow or Google Maps" | Honest fallback because the principal has zero research capability |
| `tool:Bash` listed in capability grants + tool catalog | A lie — the tool is in the catalog but every invocation errors |
| Per-tool descriptions advertise "Full system access" / "execute any shell command" | False — only `echo`-style commands that don't touch the cwd would work, and only if cwd happens to exist |

This isn't a "prompting" or "persona" issue. The model is being
*correct* when it refuses to invent addresses: it has been told (via
the tool catalog) that it has a Bash tool with full shell access, and
it tries to use it, and every attempt silently fails with a generic
error. The model has no way to distinguish "Bash is broken" from
"Bash is correctly rejecting this command," so it falls back on its
training-data instinct: don't invent specifics.

## Suggested fix (one of)

Any one of these would unblock Bash for fresh principals:

1. **Auto-create the workspace dir at registration time** —
   `ToolRuntime::register_builtins` should
   `tokio::fs::create_dir_all(&workspace).await` before constructing
   `BashTool::new().with_workspace(workspace)`. Smallest diff; matches
   the existing `mkdir -p` discipline in
   `scripts/e2e/lib/isolate.sh:91-95`.

2. **Use a path that's guaranteed to exist** — set the Bash tool's
   default cwd to `<data_dir>` (the root data dir) or
   `<data_dir>/principals/<name>/` (the principal workspace, which
   `principal create` definitely created). Both already exist by the
   time the daemon serves the first request.

3. **Skip `current_dir` if the dir doesn't exist** — in
   `execute_command_blocking`, check `dir.exists()` first; if not, log
   a warning and fall back to the daemon's CWD. Less clean because
   it papers over the actual root cause, but the smallest behavioral
   change.

4. **Add a `web_search` / `http_get` built-in tool** — solves the
   bigger gap (no research path at all) but doesn't fix Bash.

The first two are clearly correct; the fourth is the larger product
gap (Finding 2 of the earlier report — *"the model offers a sub-agent
it cannot deliver"*).

## Recommended follow-ups

- A regression test that exercises Bash on a freshly-created principal
  end-to-end (`peko principal create foo`, `daemon start`, `send foo
  "echo hello"` → expect `hello` in stdout, not `Error: Failed to
  execute Bash command`). This is the kind of test that would have
  caught the bug the moment `register_builtins` started passing a
  never-created dir to the Bash tool.
- A separate test for `peko system doctor` that calls Bash with a
  probe command. Today's `doctor` checks daemon reachability, not tool
  reachability.
- A user-facing error: when Bash fails due to cwd-missing, the error
  should say *"Bash workspace `<path>` does not exist — run `peko
  system doctor` or recreate the principal"* rather than the generic
  *"Failed to execute Bash command"*. The current generic message
  drove the model (and the user) to chase the wrong cause.

## Performance side note

While running these probes I also confirmed the v2 quota-meter fix:

```
peko quota status probe (after 4 real MiniMax-M3 sends)
  input:               0 / ∞          (still 0 — see v2 Bug A follow-up)
  output:            636 / ∞          (correctly metered)
  requests:            3 / ∞          (correctly metered)
```

So the v2 partial fix held: output and requests meter; input still
shows 0 (which is the v2 follow-up bug, narrower than the cwd bug but
still real). Out of scope for this report — captured here only to
confirm the v2 fixes didn't regress.

## Cleanup

- `explore-tool-presence-probe.sh`, `explore-bash-baseline.sh`,
  `explore-bash-env-dump.sh` left in `scripts/e2e/flows/` for reuse
  (matches the convention from earlier reports).
- Kept tempdirs under `/tmp/peko/` removed by the next
  `scripts/e2e/clean-tmp.sh` sweep (or manually — see cleanup at end
  of session).
- No peko / peko-daemon processes survived the runs.
- This report is the only file added under `scripts/e2e/reports/`.

## Status (later this session)

Fixed by adding 3 lines to `peko-rs/core/src/engine/tool_runtime.rs:137-143` in
`register_builtins` (best-effort `tokio::fs::create_dir_all(&workspace).await`,
matching the daemon-init convention at `daemon/state.rs:771`). End-to-end
verification with the same `explore-bash-baseline.sh` flow:

| Probe | Before | After |
|---|---|---|
| `echo HELLO_FROM_BASH` | `Error: Failed to execute Bash command` | `HELLO_FROM_BASH` (rc=0) |
| `which curl` | `Error: Failed to execute Bash command` | `/opt/homebrew/bin/curl` (rc=0) |
| `curl -sS --max-time 8 https://example.com` | `Error: Failed to execute Bash command` | real `<!doctype html>…` (rc=0) |
| `curl -sS --max-time 8 https://api.allorigins.win/raw?url=…` | `Error: Failed to execute Bash command` | `curl: (28) Operation timed out` (rc=28, expected — proxy, not bash) |

Workspace dir was materialised on first call at
`<data_dir>/workspaces/.` confirming the fix path. Out-of-scope follow-ups
(deeper resolve_cwd defense, error-message clarity, regression test) deferred
per user's minimal-scope choice.
