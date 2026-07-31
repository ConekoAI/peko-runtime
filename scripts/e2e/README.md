# Peko CLI e2e — isolation methodology

A small shell framework for running `peko` CLI flows against a fully
isolated home directory, so each run is reproducible and cannot pollute
your real `~/.peko` / `~/.local/share/peko`.

## Quick start

```bash
# Offline flow (no daemon)
scripts/e2e/run-case.sh principal-create-show

# Daemon-backed flow
scripts/e2e/run-case.sh cron-add-list

# Inspect tempdir instead of auto-cleanup
KEEP_TEMPDIR=1 scripts/e2e/run-case.sh principal-create-show
```

## The seam: `PEKO_HOME` + `HOME` + `PEKO_DAEMON_SOCK`

`peko`'s own config / data / cache helpers all honour `PEKO_HOME`
(`peko-rs/core/src/common/paths.rs:175-214`). But the daemon's IPC layer
**hard-codes** `dirs::home_dir().join(".peko").join("run")` for the
socket and PID file (`peko-rs/core/src/ipc/mod.rs:68-125`,
`peko-rs/core/src/ipc/server.rs:253-254`). That means:

| Layer                       | Override                          |
|-----------------------------|-----------------------------------|
| Config / data / cache dirs  | `PEKO_HOME` or `--config-dir`     |
| Daemon Unix socket          | `HOME` (only!) — server ignores PEKO_HOME |
| Daemon PID file             | `HOME` (same reason)              |
| Vault passphrase            | `PEKO_MASTER_PASSPHRASE`          |
| Identity passphrase         | `PEKO_IDENTITY_PASSPHRASE`        |
| Provider keys (headless)    | `MOCK_LLM_API_KEY` + `PEKO_TEST_RESOLVER_BOOTSTRAP=1` |
| Subprocess CWD              | `$HOME` (set by the lib)          |

Resolution order (highest → lowest precedence, matches `from_cli` in
`peko-rs/cli/src/commands/mod.rs:257`):

1. Explicit `--config-dir` / `--data-dir` / `--cache-dir` flags
2. `PEKO_CONFIG_DIR` / `PEKO_DATA_DIR` / `PEKO_CACHE_DIR` env vars
3. `PEKO_HOME` (config_dir = `$PEKO_HOME`, data = `$PEKO_HOME/data`,
   cache = `$PEKO_HOME/cache`)
4. Platform defaults (`~/.peko` + `~/.local/share/peko` on Linux,
   `~/Library/Application Support/peko` on macOS)

## What `lib/isolate.sh` exports

| Variable                   | Value                                                |
|----------------------------|------------------------------------------------------|
| `HOME`                     | `<tempdir>/home`                                     |
| `USERPROFILE`              | same (Windows symmetry)                              |
| `PEKO_HOME`                | `<tempdir>/home/.peko`                               |
| `PEKO_CONFIG_DIR`          | `<tempdir>/home/.peko`                               |
| `PEKO_DATA_DIR`            | `<tempdir>/home/.peko/data`                          |
| `PEKO_CACHE_DIR`           | `<tempdir>/home/.peko/cache`                         |
| `PEKO_DAEMON_SOCK`         | `<tempdir>/home/.peko/run/daemon.sock`               |
| `PEKO_DAEMON_PIPE`         | `""` (Windows-only; set per test if needed)          |
| `PEKO_MASTER_PASSPHRASE`   | `peko-test-vault-passphrase` (deterministic)         |
| `PEKO_IDENTITY_PASSPHRASE` | same                                                 |
| `CWD`                      | `<tempdir>/home`                                     |

## Anatomy of a flow

```bash
#!/usr/bin/env bash
source "$(dirname "$0")/../lib/isolate.sh"

flow_main() {
  peko_iso_init "my-flow-name"          # creates tempdir, exports vars,
                                         # optionally starts the daemon
  peko_iso_run principal create foo --model mock-llm
  peko_iso_assert_rc_zero
  peko_iso_assert_contains "foo"

  # …assertions about on-disk state…

  peko_iso_done 0                        # kills daemon, removes tempdir
}
```

`peko_iso_init` honours these env vars:

| Var                        | Effect                                                    |
|----------------------------|-----------------------------------------------------------|
| `PEKO_BIN=/path/to/peko`   | Skip binary auto-detection                                |
| `NO_DAEMON=1`              | Skip `peko daemon start --foreground`                     |
| `KEEP_TEMPDIR=1`           | Leave tempdir on disk after exit (debug)                 |
| `MOCK_LLM_URL=…`           | Export `PEKO_TEST_RESOLVER_BOOTSTRAP=1` + seed catalog    |
| `MOCK_LLM_API_KEY=…`       | Override the default `mock-llm-test-key`                  |

## Cleanup: sweeping stale tempdirs

`peko_iso_done` traps `EXIT`/`INT`/`TERM` and removes the tempdir it
created — but SIGKILL (or a parent-shell crash mid-`init`) can't be
trapped. `KEEP_TEMPDIR=1` runs also leak by design. The result is
orphan tempdirs accumulating under `/tmp/peko/` (or the
`PEKO_ISO_ROOT` / `<repo>/target/e2e/` fallbacks the lib tries).

`scripts/e2e/clean-tmp.sh` sweeps them. It auto-detects the same
`iso_root`s the lib uses, only removes tempdirs matching the lib's
`<flow>-<pid>-<rand>` naming pattern, and **never touches a tempdir
whose `daemon.pid` resolves to a live process** (so it won't kill a
test you're still inspecting).

```bash
scripts/e2e/clean-tmp.sh                    # dry-run, list candidates
scripts/e2e/clean-tmp.sh --apply            # actually remove
scripts/e2e/clean-tmp.sh --min-age 24       # keep recent runs (debug)
scripts/e2e/clean-tmp.sh --root /tmp/peko   # override iso_root (repeatable)
scripts/e2e/clean-tmp.sh --quiet            # summary only
```

Exit codes:

| Code | Meaning                                                  |
|------|----------------------------------------------------------|
| `0`  | No candidates / dry-run finished / clean succeeded       |
| `1`  | Some candidates skipped because a live peko holds them   |
| `2`  | `--apply` failed (permission, I/O) — see failure list    |

Cron-friendly example — nightly sweep of stale tempdirs older than a
day, swallowed output unless something actually skipped:

```cron
0 3 * * * cd /path/to/peko-runtime && \
    scripts/e2e/clean-tmp.sh --apply --min-age 1440 --quiet \
      || logger -t peko-e2e-cleanup "exit=$?"
```

## Why `--foreground` for the daemon?

`peko daemon start` (without `--foreground`) double-forks. The shell
loses the child PID and `kill` against `$!` kills the wrong process,
leaving the daemon orphaned and bound to a socket that survives the
test. With `--foreground` the daemon is a direct child of the shell so
the `trap peko_iso_done EXIT` in `isolate.sh` reliably cleans up.

## Cross-references

The Rust integration tests in `peko-rs/core/tests/common/cli.rs` use
exactly this combination of env vars. The shell lib is just a
shell-port of that pattern, so a flow you script in shell is
behaviourally identical to the equivalent `#[test]`.

| Rust helper                         | Shell helper                          |
|-------------------------------------|---------------------------------------|
| `PekoCli::new()`                    | `peko_iso_init`                       |
| `PekoCli::cmd()` / `.args([...])`   | `peko_iso_run …`                      |
| `DaemonGuard::spawn()`              | `peko_iso_init` (with daemon default) |
| `Drop for DaemonGuard`              | `peko_iso_done` (trap on EXIT)        |

## Writing a new flow

1. Copy `flows/principal-create-show.sh` to `flows/<your-flow>.sh`.
2. Rename `flow_main` body to your sequence.
3. Add the new file's name to `run-case.sh`'s `Available flows` list
   (auto-discovered — no edit needed; the script globs `flows/*.sh`).
4. Run it: `scripts/e2e/run-case.sh <your-flow>`.

## What this does NOT isolate

- The OS keychain. Provider keys are stored there by default. Set
  `PEKO_TEST_RESOLVER_BOOTSTRAP=1` plus a `*_API_KEY` env var to bypass
  in CI / headless.
- Network ports outside the tempdir. `peko` doesn't bind any by default
  besides the Unix socket inside `<peko_home>/run`.
- Concurrent flows on the same shell. Two flows in the same shell will
  share `_PEKO_ISO_*` globals. Run each flow in its own shell.
