#!/usr/bin/env bash
# e2e/lib/isolate.sh — environment-isolation library for `peko` CLI flows.

# Tolerate `set -u` from the caller: every optional env var in this lib
# uses `${VAR:-}` form already, but defensively disable nounset inside
# the library itself so a future contributor can't accidentally regress
# it (the failure mode is a stack-trace-less exit halfway through init).
set +u
#
# Source this from a flow script:
#
#   source "$(dirname "$0")/../lib/isolate.sh"
#   peko_iso_init "my-flow"           # creates a fresh tempdir + exports vars
#   peko peko principal create foo …  # any peko subprocess now hits the tempdir
#   peko_iso_done                     # cleanup (kill daemon, remove tempdir)
#
# What gets isolated (precedence low → high matches `peko` itself):
#
#   HOME                     <temp>/home         # dirs::home_dir() on every plat
#   USERPROFILE              <temp>/home         # Windows dirs::home_dir()
#   PEKO_HOME                <temp>/home/.peko   # default_config_dir()
#   PEKO_CONFIG_DIR          <temp>/home/.peko   # clap `env = "PEKO_CONFIG_DIR"`
#   PEKO_DATA_DIR            <temp>/home/.peko/data
#   PEKO_CACHE_DIR           <temp>/home/.peko/cache
#   PEKO_DAEMON_SOCK         <temp>/home/.peko/run/daemon.sock   # Unix IPC
#   PEKO_DAEMON_PIPE         <pipe-name>                          # Win IPC
#   PEKO_MASTER_PASSPHRASE   "peko-test-vault-passphrase"        # vault.enc
#   PEKO_IDENTITY_PASSPHRASE same                                # identity keys
#   CWD                      <temp>/home         # blocks `config init`
#                                                #   writing peko.toml into
#                                                #   the project root
#
# Why both `HOME` AND `PEKO_HOME`?
# --------------------------------
# `peko`'s own config / data / cache helpers all honour `PEKO_HOME`
# (peko-rs/core/src/common/paths.rs:175-214), but the daemon IPC layer
# (peko-rs/core/src/ipc/mod.rs:68-125 + server.rs:253-254) **hard-codes**
# `dirs::home_dir().join(".peko").join("run")`. Setting `HOME` is the only
# way to redirect the daemon socket + PID file in-process; `PEKO_HOME`
# alone isn't enough on the server side. (This matches the project's own
# Rust test harness at peko-rs/core/tests/common/cli.rs.)
#
# What this does NOT isolate:
#
#   - The OS keychain (macOS login / libsecret / DPAPI). Provider keys are
#     stored in the keychain by default; pass `PEKO_TEST_RESOLVER_BOOTSTRAP=1`
#     plus a `*_API_KEY` env var (e.g. `MOCK_LLM_API_KEY=mock-llm-test-key`)
#     to bypass it in CI/headless contexts.
#   - Network listeners on well-known ports. `peko` does not bind any by
#     default — it only opens a Unix datagram socket inside `<peko_home>/run`.

# --- internal state --------------------------------------------------------

_PEKO_ISO_FLOW="${_PEKO_ISO_FLOW:-unnamed}"
_PEKO_ISO_TEMPDIR=""
_PEKO_ISO_PEKO_DIR=""
_PEKO_ISO_SOCK=""
_PEKO_ISO_BIN=""
_PEKO_ISO_VAULT_PP="peko-test-vault-passphrase"
_PEKO_ISO_DAEMON_PID=""

# --- public API ------------------------------------------------------------

# Locate the `peko` binary. Honors $PEKO_BIN override; otherwise walks up from
# this script's location to find the workspace target/.
peko_iso_resolve_bin() {
  if [[ -n "${PEKO_BIN:-}" && -x "$PEKO_BIN" ]]; then
    _PEKO_ISO_BIN="$PEKO_BIN"
    return 0
  fi
  local script_dir workspace_root target profile
  script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  # scripts/e2e/lib/isolate.sh → repo root is ../../..
  workspace_root="$(cd "$script_dir/../../.." && pwd)"
  profile="$(if [[ -n "${RELEASE:-}" || "${1:-}" == "--release" ]]; then echo release; else echo debug; fi)"
  target="${CARGO_TARGET_DIR:-$workspace_root/target}"
  _PEKO_ISO_BIN="$target/$profile/peko"
  if [[ ! -x "$_PEKO_ISO_BIN" ]]; then
    echo "❌ peko binary not found at $_PEKO_ISO_BIN" >&2
    echo "   Build it first: cargo build -p peko-cli --bin peko" >&2
    echo "   …or set PEKO_BIN=/path/to/peko before sourcing this script." >&2
    return 1
  fi
}

# Initialise a fresh isolated environment for a named flow.
# Args:
#   $1  flow name (used to name the tempdir; default: timestamp)
# Env (optional, override defaults):
#   PEKO_BIN           path to a pre-built `peko` binary
#   PEKO_ISO_ROOT=…    override the tempdir root (default: <workspace>/target/e2e)
#   KEEP_TEMPDIR=1     skip auto-cleanup on exit
#   AUTOSTART_DAEMON=1 start `peko daemon start --foreground` automatically
#                      (DEFAULT: NO — flows control daemon lifecycle so
#                      principals can be seeded before daemon startup.
#                      Set AUTOSTART_DAEMON=1 for flows that don't need
#                      IPC and want a backgrounded daemon for show.)
#   MOCK_LLM_URL=…     seed the catalog with a mock-llm provider entry
#   PEKO_TEST_RESOLVER_BOOTSTRAP=1  honour *_API_KEY env vars (CI/headless)
peko_iso_init() {
  _PEKO_ISO_FLOW="${1:-flow-$(date +%s)}"
  peko_iso_resolve_bin || return 1

  # Use a short tempdir root, NOT `mktemp -t`, because mktemp's default
  # macOS path is /var/folders/.../T/ and the daemon's Unix socket bind
  # fails with `path must be shorter than SUN_LEN` (107 bytes on macOS)
  # when <tempdir>/.peko/run/daemon.sock exceeds that. Even a workspace-
  # relative path like target/e2e/ yields ~109 chars once the rest of
  # the socket path is appended — too long. /tmp/peko is the shortest
  # sane default; PEKO_ISO_ROOT can override for tests that want to
  # keep artifacts under a specific tree.
  local script_dir workspace_root iso_root rand_suffix
  script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  workspace_root="$(cd "$script_dir/../../.." && pwd)"
  iso_root="${PEKO_ISO_ROOT:-/tmp/peko}"
  # Fall back to workspace target if /tmp/peko isn't writable (Linux CI
  # sandboxes, etc.) — that path is too long for macOS but works on Linux
  # where SUN_LEN is also 108 and we're more likely to be sandboxed.
  if ! mkdir -p "$iso_root" 2>/dev/null; then
    iso_root="$workspace_root/target/e2e"
    mkdir -p "$iso_root"
  fi
  rand_suffix="$(LC_ALL=C tr -dc 'a-z0-9' </dev/urandom | head -c 6)"
  _PEKO_ISO_TEMPDIR="$iso_root/${_PEKO_ISO_FLOW}-$$-${rand_suffix}"
  rm -rf "$_PEKO_ISO_TEMPDIR"
  mkdir -p "$_PEKO_ISO_TEMPDIR"
  _PEKO_ISO_PEKO_DIR="$_PEKO_ISO_TEMPDIR/home/.peko"
  _PEKO_ISO_SOCK="$_PEKO_ISO_PEKO_DIR/run/daemon.sock"

  # Pre-create the directory skeleton the daemon + `peko agent create` expect
  # on first touch. Without this the first command errors with "No such file"
  # for paths like `<peko>/runtime/locks`.
  for sub in home home/.peko home/.peko/run home/.peko/data home/.peko/cache \
             home/.peko/runtime/extensions home/.peko/runtime/mcps \
             home/.peko/runtime/registry home/.peko/runtime/locks; do
    mkdir -p "$_PEKO_ISO_TEMPDIR/$sub"
  done

  # Defensive: kill any peko-daemon / peko subprocess that might still be
  # alive from a prior run (KEEP_TEMPDIR=1 + crash, Ctrl-C, etc.). Without
  # this, a stale `peko daemon` bound to the host's default socket would
  # cause `peko_iso_start_daemon`'s status-poll to read "running:true"
  # against the wrong daemon and silently route IPC into a black hole.
  pkill -9 -f 'target/debug/peko' 2>/dev/null || true
  pkill -9 -f 'target/release/peko' 2>/dev/null || true

  # Seed an encrypted vault so the daemon can load provider keys without
  # an OS keychain. Mirrors PekoCli::new() in peko-rs/core/tests/common/cli.rs.
  local vault_path="$_PEKO_ISO_PEKO_DIR/vault.enc"
  python3 - "$vault_path" "$_PEKO_ISO_VAULT_PP" <<'PY' 2>/dev/null || true
import sys, os
# Best-effort: if the vault already exists (second call in the same tempdir)
# leave it alone. We can't construct a valid Argon2id-blake2b vault from
# shell — flows that need vault unlocking set PEKO_MASTER_PASSPHRASE so the
# `peko` subprocess can recreate it on first access.
sys.exit(0)
PY

  # Export every isolation variable. Subprocesses inherit these.
  export HOME="$_PEKO_ISO_TEMPDIR/home"
  export USERPROFILE="$_PEKO_ISO_TEMPDIR/home"   # Windows symmetry
  export PEKO_HOME="$_PEKO_ISO_PEKO_DIR"
  export PEKO_CONFIG_DIR="$_PEKO_ISO_PEKO_DIR"
  export PEKO_DATA_DIR="$_PEKO_ISO_PEKO_DIR/data"
  export PEKO_CACHE_DIR="$_PEKO_ISO_PEKO_DIR/cache"
  export PEKO_DAEMON_SOCK="$_PEKO_ISO_SOCK"
  # PEKO_DAEMON_PIPE is Windows-only; export it to an empty value to be safe.
  export PEKO_DAEMON_PIPE=""
  export PEKO_MASTER_PASSPHRASE="$_PEKO_ISO_VAULT_PP"
  export PEKO_IDENTITY_PASSPHRASE="$_PEKO_ISO_VAULT_PP"
  # Forward real-LLM keys only if they were already set in the parent
  # shell. Daemon's `PEKO_TEST_RESOLVER_BOOTSTRAP=1` reads these when the
  # OS keychain is unavailable. Vault-stored keys (`peko model add --key`)
  # are the preferred path; this is belt-and-suspenders.
  for k in MINIMAX_API_KEY ANTHROPIC_API_KEY KIMI_API_KEY OPENAI_API_KEY; do
    if [[ -n "${!k:-}" ]]; then
      export "$k"
    fi
  done
  # Real-LLM flows should bypass the OS-keychain gate.
  if [[ -n "${MINIMAX_API_KEY:-}${ANTHROPIC_API_KEY:-}${KIMI_API_KEY:-}${OPENAI_API_KEY:-}" ]]; then
    export PEKO_TEST_RESOLVER_BOOTSTRAP=1
  fi
  # Subprocess CWD → the isolated HOME, so `peko config init` (which writes
  # peko.toml relative to CWD) doesn't pollute the project root.
  cd "$HOME" || return 1

  echo "🔒 peko_iso_init: flow='$_PEKO_ISO_FLOW'"
  echo "    HOME              = $HOME"
  echo "    PEKO_HOME         = $PEKO_HOME"
  echo "    PEKO_DAEMON_SOCK  = $PEKO_DAEMON_SOCK"
  echo "    PEKO_BIN          = $_PEKO_ISO_BIN"
  echo "    tempdir           = $_PEKO_ISO_TEMPDIR  (KEEP_TEMPDIR=${KEEP_TEMPDIR:-})"

  # Optional: seed mock-LLM provider if MOCK_LLM_URL is set AND no real
  # key was supplied. Real-key flows (MINIMAX_API_KEY=…, ANTHROPIC_API_KEY=…)
  # take precedence — flows that want to mix both should set MOCK_LLM_URL
  # explicitly.
  if [[ -n "${MOCK_LLM_URL:-}" ]] && [[ -z "${MINIMAX_API_KEY:-}${ANTHROPIC_API_KEY:-}${KIMI_API_KEY:-}${OPENAI_API_KEY:-}" ]]; then
    export PEKO_TEST_RESOLVER_BOOTSTRAP=1
    export MOCK_LLM_API_KEY="${MOCK_LLM_API_KEY:-mock-llm-test-key}"
    peko_iso_seed_mock_provider "$MOCK_LLM_URL"
  fi

  # Optional: spin up the daemon in foreground (backgrounded). Foreground is
  # critical — daemonized daemons orphan and pollute later runs.
  #
  # Default: NO_DAEMON=1. The reason: the daemon loads principals at
  # startup; if a flow creates a principal AFTER the daemon is up, the
  # daemon's principal registry won't see it (peko has no runtime
  # principal-reload verb today). Flows that need IPC-backed ops (cron,
  # send, async tools) call `peko_iso_start_daemon` explicitly AFTER
  # seeding principals.
  if [[ -n "${AUTOSTART_DAEMON:-}" ]]; then
    peko_iso_start_daemon || {
      echo "❌ daemon failed to become ready" >&2
      return 1
    }
  fi

  # Always clean up on shell exit (success or failure) unless KEEP_TEMPDIR=1.
  if [[ -z "${KEEP_TEMPDIR:-}" ]]; then
    trap 'peko_iso_done $?' EXIT INT TERM
  fi
}

# Tear down the isolated environment. Idempotent.
# Args:
#   $1  exit code to propagate (default: 0)
peko_iso_done() {
  local rc="${1:-0}"
  if [[ -n "$_PEKO_ISO_DAEMON_PID" ]]; then
    kill "$_PEKO_ISO_DAEMON_PID" 2>/dev/null || true
    wait "$_PEKO_ISO_DAEMON_PID" 2>/dev/null || true
    _PEKO_ISO_DAEMON_PID=""
  fi
  # Belt-and-suspenders: kill any peko subprocess whose PID file lives
  # under THIS tempdir's run/ dir, in case the daemon forked or escaped
  # _PEKO_ISO_DAEMON_PID (e.g. parent died before trap could attach).
  if [[ -f "$_PEKO_ISO_PEKO_DIR/run/daemon.pid" ]]; then
    local stale
    stale="$(cat "$_PEKO_ISO_PEKO_DIR/run/daemon.pid" 2>/dev/null || true)"
    if [[ -n "$stale" ]] && kill -0 "$stale" 2>/dev/null; then
      kill "$stale" 2>/dev/null || true
      sleep 0.2
      kill -9 "$stale" 2>/dev/null || true
    fi
  fi
  # Belt-and-suspenders: remove the sock file in case the daemon died on a
  # different PID (e.g. the daemon forked). Some flows don't start a daemon
  # at all (NO_DAEMON=1) so this is a no-op in that case.
  [[ -S "$_PEKO_ISO_SOCK" ]] && rm -f "$_PEKO_ISO_SOCK"
  if [[ -z "${KEEP_TEMPDIR:-}" && -n "$_PEKO_ISO_TEMPDIR" && -d "$_PEKO_ISO_TEMPDIR" ]]; then
    rm -rf "$_PEKO_ISO_TEMPDIR"
  elif [[ -n "$_PEKO_ISO_TEMPDIR" ]]; then
    echo "🗂️  KEEP_TEMPDIR=${KEEP_TEMPDIR:-} — leaving $_PEKO_ISO_TEMPDIR on disk"
  fi
  exit "$rc"
}

# Run a `peko` subprocess against the isolated environment.
# Args:
#   $@  args to pass to the `peko` binary (e.g. "principal create foo")
# Stdout: command stdout
# Stderr: command stderr (separately captured so callers can assert)
# Returns: the exit code
peko_iso() {
  "$_PEKO_ISO_BIN" "$@"
}

# Same as peko_iso but returns (stdout, stderr, exit_code) on stdout, suitable
# for command-substitution flow scripts:
#   eval "$(peko_iso_capture principal list)"
# Or use the peko_iso_run helper which writes them to globals.
_peko_iso_capture_out=""
_peko_iso_capture_err=""
_peko_iso_capture_rc=0
peko_iso_run() {
  local tmp_out tmp_err
  tmp_out="$(mktemp -t peko-e2e-stdout.XXXXXX)"
  tmp_err="$(mktemp -t peko-e2e-stderr.XXXXXX)"
  "$_PEKO_ISO_BIN" "$@" >"$tmp_out" 2>"$tmp_err"
  _peko_iso_capture_rc=$?
  _peko_iso_capture_out="$(cat "$tmp_out")"
  _peko_iso_capture_err="$(cat "$tmp_err")"
  rm -f "$tmp_out" "$tmp_err"
  return $_peko_iso_capture_rc
}

# Assert helpers — fail loudly so the trap can surface what broke.
peko_iso_assert_rc_zero() {
  if [[ $_peko_iso_capture_rc -ne 0 ]]; then
    echo "❌ expected rc=0, got rc=$_peko_iso_capture_rc" >&2
    echo "   stdout: $_peko_iso_capture_out" >&2
    echo "   stderr: $_peko_iso_capture_err" >&2
    return 1
  fi
}

peko_iso_assert_contains() {
  local needle="$1" haystack="${2:-$_peko_iso_capture_out}"
  if [[ "$haystack" != *"$needle"* ]]; then
    echo "❌ expected output to contain: $needle" >&2
    echo "   actual: $haystack" >&2
    return 1
  fi
}

# --- internal helpers ------------------------------------------------------

# Seed the runtime model catalog with a mock-llm provider entry so commands
# that take `--model <id>` can resolve it. Mirrors
# `common::agent::seed_mock_provider_in_catalog` in Rust tests.
peko_iso_seed_mock_provider() {
  local url="$1"
  local catalog_dir="$PEKO_HOME/principals"
  mkdir -p "$catalog_dir"
  # The catalog file path is `<PEKO_HOME>/model_catalog.toml`; the daemon
  # rebuilds it from the bundled v3 registry, but for offline tests we want
  # the entry to be present BEFORE the daemon reads it. Easiest way is to
  # write it as a runtime-registered provider via the registry service —
  # but that's only available via IPC. So we rely on `peko model add` which
  # works offline once we set MOCK_LLM_URL.
  echo "    (mock-llm seeding deferred to `peko model add` at flow start)"
}

# Background-spawn the daemon and wait until it accepts IPC.
peko_iso_start_daemon() {
  local debug_out="$_PEKO_ISO_TEMPDIR/daemon.out"
  local debug_err="$_PEKO_ISO_TEMPDIR/daemon.err"
  echo "🚀 starting daemon (logs: $debug_err)"
  # --foreground is required: without it `peko daemon start` double-forks
  # and we lose the child PID. With foreground, $! is the actual daemon.
  "$_PEKO_ISO_BIN" daemon start --foreground -v \
    >"$debug_out" 2>"$debug_err" &
  _PEKO_ISO_DAEMON_PID=$!

  # Poll `peko daemon status --json` until running:true. Mirrors
  # `DaemonGuard::wait_ready` in peko-rs/core/tests/common/daemon.rs.
  local deadline=$((SECONDS + 30))
  while (( SECONDS < deadline )); do
    if peko_iso_run daemon status --json; then
      if [[ "$_peko_iso_capture_out" == *"\"running\": true"* ]]; then
        echo "    daemon ready (pid=$_PEKO_ISO_DAEMON_PID)"
        return 0
      fi
    fi
    sleep 0.5
  done
  echo "❌ daemon did not become ready within 30s" >&2
  echo "--- daemon stderr ---" >&2
  tail -n 50 "$debug_err" >&2
  return 1
}
