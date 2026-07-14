# ADR-043: Sidecar Lifecycle — Desktop Owns the Engine, Daemon Is Invisible

**Status:** Accepted
**Date:** 2026-07-14
**Author:** rlsn
**Depends on:** ADR-021 (Daemon as Central Runtime), ADR-001-desktop (Desktop IPC vs CLI Shell-Out — superseded)
**Related:** ADR-002-desktop (Remote Runtime Support), ADR-040 (Tool Timeout and Async Refactor)

**Note:** This is a clean-slate pre-production design. The end-state described here replaces the current behaviour where `peko-desktop` exposes Start/Stop/Status buttons and depends on the user manually launching the daemon in a separate terminal.

---

## 1. Context

`peko-desktop` is a Tauri GUI for users who do not know — and must not need to know — that the `peko` runtime is implemented as a separate daemon process. The current architecture (a half-built version of which already lives in `peko-desktop/src-tauri/src/daemon/mod.rs`) leaks that boundary into the user-facing surface:

- Settings → Daemon tab exposes **Start / Stop / Restart** buttons. Clicking them shells out to `peko daemon start|stop` as a subprocess.
- The Tauri app's `current_exe()` is not next to a bundled `peko` binary; `find_binary()` falls back to `PATH`. On a fresh machine where the user has only installed the desktop app, the binary is not on `PATH` and `daemon::start()` returns `Err(BinaryNotFound)`.
- The button's error is silently swallowed by the React Query mutation hook (`useDaemonStart` never reads `.error`). The user clicks Start, the button re-enables, and nothing visible changes. T-102 of `MANUAL_TEST_PLAN.md` failed for exactly this reason: *"clicking start, nothing happened"*.
- The manual test plan section §1 ("First run") instructs the tester to *"open another terminal and run `peko daemon start`"* — a step that any non-technical end-user would rightly consider insane.

This ADR records the intended end-state: the desktop owns the engine lifecycle, the engine is invisible to the user, and the test plan no longer asks testers to manage a process boundary.

## 2. Decision

### 2.1 The desktop owns the engine

`peko-desktop` bundles the `peko` binary as a **Tauri sidecar** (`bundle.externalBin` in `tauri.conf.json`). On app startup, the desktop spawns the sidecar, waits for it to bind its IPC socket, and proceeds. On app exit, the desktop shuts the sidecar down cleanly. The user-facing UI never presents "start the engine" or "stop the engine" as an action.

### 2.2 The CLI command surface is preserved

`peko daemon start | stop | status` continues to exist for headless use (servers, scripts, CI). The sidecar path is **additive**, not replacement. A user who runs `peko` from a terminal while the desktop is also running will collide on the IPC socket; that is acceptable, not a bug.

### 2.3 A `--sidecar-mode` flag distinguishes sidecar instances

When the desktop spawns the sidecar, it passes `--sidecar-mode`. In this mode the daemon:

- Uses a per-user lockfile path (`~/.peko/run/<uid>/desktop.lock` or similar) so two desktop instances cannot both claim the IPC socket.
- Writes `PEKO_VERSION=<semver>` to **stderr** at startup, so the desktop can scrape the version without an extra IPC round-trip.
- Refuses to run if the lockfile already holds a live PID (returns a structured "engine already running elsewhere" error rather than silently failing).

The non-sidecar (`peko daemon start` from a terminal) code path is unchanged.

### 2.4 Version mismatch is detected and surfaced

On startup, the desktop reads the sidecar's stderr header (`PEKO_VERSION=...`) and compares it to the version it expected (compiled into the bundle manifest, or read from the bundle's `Info.plist`/`.exe` metadata). On mismatch the desktop shows a banner:

> **A newer Peko engine is available.** Restart the app to update.

The mismatch is **non-blocking** — the running older engine keeps working until the user restarts. This avoids mid-session disruption.

### 2.5 Crash recovery: restart-once, then surface

If the sidecar exits unexpectedly while the desktop is running, the `SidecarSupervisor` restarts it once. If the second instance also exits within 30 seconds, the supervisor gives up and surfaces a banner:

> **Engine keeps stopping.** Please restart the app or contact support.

### 2.6 Power-user diagnostics are hidden

A "Show internal status" toggle in Settings reveals:

- Engine version, PID, uptime, lockfile path, IPC socket path
- Recent log lines (last 200 from a ring buffer the supervisor keeps)
- Manual Restart / Stop buttons (for developers; never user-facing)

The default UI shows only a coloured status badge: green / yellow / red.

### 2.7 Failure messages are jargon-free

Errors surfaced to the user are rewritten at the IPC boundary into plain English:

| Raw error | User-facing message |
|-----------|---------------------|
| `Failed to connect to /tmp/peko.sock` | "Couldn't reach the engine. Retrying…" |
| `binary not found: peko` | "Engine isn't installed. Please reinstall the app." |
| `daemon started but did not write PID file within 10 seconds` | "Engine failed to start. Please restart the app." |
| `engine already running elsewhere` | "Another instance of Peko is already running." |

The raw error stays in the diagnostics panel and the log ring buffer.

## 3. Architecture

```
┌─────────────────────────────────────────────────────────┐
│  peko-desktop (Tauri)                                    │
│  ┌───────────────┐                                       │
│  │ React UI      │                                       │
│  │  - status badge│  (green / yellow / red)              │
│  │  - no buttons  │                                       │
│  │  - error banners│  (jargon-free)                      │
│  └──────┬────────┘                                       │
│         │ Tauri commands (ipc)                           │
│  ┌──────▼────────┐                                       │
│  │ SidecarSupervisor│                                    │
│  │  - spawn       │                                       │
│  │  - watch PID   │                                       │
│  │  - restart once│                                       │
│  │  - shutdown    │                                       │
│  │  - parse stderr│  PEKO_VERSION=x.y.z                   │
│  └──────┬────────┘                                       │
│         │ Command::new_sidecar("peko")                   │
│         │ args: ["daemon", "start", "--sidecar-mode"]    │
└─────────┼───────────────────────────────────────────────┘
          │
          ▼
┌─────────────────────────────────────────────────────────┐
│  peko sidecar process (bundled binary)                   │
│  - stderr: PEKO_VERSION=x.y.z\n…                        │
│  - listens on ~/.peko/run/<uid>/desktop.sock             │
│  - lockfile at ~/.peko/run/<uid>/desktop.lock            │
│  - refuses if lockfile already held                      │
└─────────────────────────────────────────────────────────┘
```

For **headless users** (servers, CI), `peko daemon start` from a terminal still works unchanged — the same `peko` binary, just without `--sidecar-mode`. The lockfile path defaults to the existing `~/.peko/run/daemon.pid`.

## 4. Implementation plan

The work splits into six PRs. Each is independently mergeable; D–F are gated.

| PR | Repo | Scope | Gated on |
|----|------|-------|----------|
| **A** | peko-runtime | This ADR (docs only) | — |
| **B** | peko-runtime | `peko version` subcommand, `--sidecar-mode` daemon flag, `PEKO_VERSION=x.y.z` on stderr at startup, per-user lockfile path in sidecar mode | A |
| **C** | peko-desktop | `tauri.conf.json` `bundle.externalBin` for `peko`; build script that copies the freshly-built `peko` binary from `peko-runtime/target/release/` into the desktop sidecar location; CI verification | B |
| **D** | peko-desktop | New `SidecarSupervisor` module: spawn on `setup()`, watch child PID, parse stderr, auto-restart-once, graceful shutdown on `cleanup()`; wire into `main.rs` | C |
| **E** | peko-desktop | Remove Start/Stop/Status buttons from the main Settings surface; add engine status badge to header; add version-mismatch banner; add hidden diagnostics toggle | D |
| **F** | peko-runtime | Rewrite `MANUAL_TEST_PLAN.md` §1 (First run) and §T-101..T-105: drop the Start/Stop tests; add engine-status verification (T-101), auto-restart verification (T-102), version-mismatch banner (T-103) | E |

PR A can merge independently. PRs B and C can be developed in parallel; C depends on B's `--sidecar-mode` flag to function but the build-pipeline change itself compiles without it.

## 5. Consequences

**Positive:**

- Non-technical users never encounter the word "daemon" or the concept of a separate process. The UX matches every other consumer desktop app that owns its engine (Docker Desktop, Slack, Discord).
- T-102 of the manual test plan becomes un-blockable by design: there is no button to click.
- Version mismatch is handled gracefully — users always know when they need to update, but never mid-session.
- Crash recovery (restart-once) eliminates the "engine died and now nothing works" failure mode.
- The CLI daemon commands stay intact for server use; no migration tax for headless users.

**Negative:**

- The desktop and runtime are coupled at build time (the desktop's bundle manifest declares the expected `peko` version). If a user has `peko` on `PATH` at a different version, the desktop uses its bundled binary, not the user's. This is intentional — predictable behaviour beats accidental compatibility.
- Two PEKO daemons on the same machine (desktop sidecar + a manually-started terminal daemon) cannot share an IPC socket. The lockfile prevents corruption but not the user confusion. The diagnostics panel surfaces the running instance and its lockfile path.
- The `pnpm-lock.yaml` / build script coupling between `peko-desktop` and `peko-runtime` adds a step to the release process. Acceptable: the alternative is shipping a desktop that doesn't know what version of the engine it's running.

## 6. Out of scope

- **Remote runtime** — ADR-002-desktop already covers this. Sidecar is for the local engine only.
- **Multi-engine** — running more than one sidecar engine on a single machine. Possible future work; the lockfile lays the groundwork.
- **Auto-update of the bundled `peko` binary** — the version-mismatch banner prompts the user to restart the desktop app, which loads the new bundle. In-place sidecar hot-swap is future work.
- **Replacing `peko daemon start`** — the CLI command stays as the headless entry point.

## 7. References

- `peko-desktop/src-tauri/src/daemon/mod.rs` — current half-built daemon module (`start`, `stop`, `restart`, `ensure_running`, `status`).
- `peko-desktop/src-tauri/src/commands/daemon.rs` — current Tauri command handlers (`daemon_start`, etc.).
- `peko-desktop/src/hooks/useDaemon.ts` — current React Query hooks (silent-error bug fixed in PR #26).
- `peko-runtime/docs/testing/MANUAL_TEST_PLAN.md` T-101..T-105 — the manual test plan steps this ADR will obsolete.
- ADR-021 (peko-runtime) — Daemon as Central Runtime: defines the IPC protocol the sidecar continues to speak.
- ADR-001-desktop — Desktop IPC vs CLI Shell-Out: superseded by the IPC-primary model; this ADR inherits the Phase 3 outcome (desktop speaks IPC to a local daemon) and changes the daemon's *owner*.
- ADR-002-desktop — Desktop Remote Runtime Support: the remote path is unaffected; this ADR is about the local sidecar.