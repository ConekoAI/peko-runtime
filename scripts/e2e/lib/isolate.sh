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
# Extra background PIDs a flow registers (mock LLM servers, `peko log
# --watch` watchers, background `peko send` processes). peko_iso_done
# kills them all, so cleanup happens even when a flow fails midway.
_PEKO_ISO_EXTRA_PIDS=()

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
  # Kill any extra background processes the flow registered (mock LLM
  # servers, `peko log --watch` watchers, background `peko send`
  # processes) so nothing escapes the flow even on a mid-flow failure.
  # Two registration paths: the _PEKO_ISO_EXTRA_PIDS array (in-shell)
  # and <tempdir>/extra.pids (one PID per line — survives the subshell
  # that `port="$(peko_iso_start_mock_llm …)"` command substitution
  # runs the helper in).
  if [[ ${#_PEKO_ISO_EXTRA_PIDS[@]} -gt 0 ]]; then
    local extra
    for extra in "${_PEKO_ISO_EXTRA_PIDS[@]}"; do
      kill "$extra" 2>/dev/null || true
    done
  fi
  if [[ -n "$_PEKO_ISO_TEMPDIR" && -f "$_PEKO_ISO_TEMPDIR/extra.pids" ]]; then
    local extra
    while read -r extra; do
      [[ -n "$extra" ]] && kill "$extra" 2>/dev/null || true
    done < "$_PEKO_ISO_TEMPDIR/extra.pids"
  fi
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
  # Clear globals so a re-fired EXIT trap (e.g. when `peko_iso_done` is
  # called explicitly and then the shell exits) short-circuits both
  # branches above and prints nothing — the tempdir is already gone.
  unset _PEKO_ISO_TEMPDIR _PEKO_ISO_PEKO_DIR _PEKO_ISO_SOCK _PEKO_ISO_DAEMON_PID
  exit "$rc"
}

# Run a `peko` subprocess against the isolated environment.
# Args:
#   $@  args to pass to the `peko` binary (e.g. "principal create foo")
# Stdout: command stdout
# Stderr: command stderr (separately captured so callers can assert)
# Returns: 0 — exit code is exposed via `_peko_iso_capture_rc` so
#   callers can inspect, fail, or ignore it independently. The function
#   always returns 0 so flows running under `set -euo pipefail`
#   (see scripts/e2e/run-case.sh) don't abort on a probed non-zero
#   rc. Bug G (2026-08-01 v3 field test) was caused by this helper
#   propagating the rc into `return`, which `set -e` then treated as
#   a script failure — silently truncating exploratory flows.
peko_iso() {
  "$_PEKO_ISO_BIN" "$@"
}

# Same as peko_iso but writes (stdout, stderr, exit_code) to globals
# so flow scripts can assert against them without command substitution
# (which swallows non-zero rc under `set -e`):
#   peko_iso_run principal list
#   peko_iso_assert_rc_zero
# The function ALWAYS returns 0 — callers check rc via
# `_peko_iso_capture_rc` or assert helpers.
_peko_iso_capture_out=""
_peko_iso_capture_err=""
_peko_iso_capture_rc=0
peko_iso_run() {
  local tmp_out tmp_err
  tmp_out="$(mktemp -t peko-e2e-stdout.XXXXXX)"
  tmp_err="$(mktemp -t peko-e2e-stderr.XXXXXX)"
  # `|| _peko_iso_capture_rc=$?` is load-bearing: it captures the
  # peko subprocess's rc into `_peko_iso_capture_rc` while keeping
  # the compound command's overall rc = 0. Without this idiom, the
  # binary's non-zero rc would trip `set -e` (set in
  # scripts/e2e/run-case.sh) BEFORE we reach the `$?` assignment,
  # killing the flow. Bug G (2026-08-01 v3 field test) was caused
  # by exactly this — the original lib propagated the rc via
  # `return`, which `set -e` treated as a script failure.
  _peko_iso_capture_rc=0
  "$_PEKO_ISO_BIN" "$@" >"$tmp_out" 2>"$tmp_err" \
    || _peko_iso_capture_rc=$?
  _peko_iso_capture_out="$(cat "$tmp_out")"
  _peko_iso_capture_err="$(cat "$tmp_err")"
  rm -f "$tmp_out" "$tmp_err"
  # Always return 0 — exit code is captured in `_peko_iso_capture_rc`.
  # Callers that want to fail loudly use `peko_iso_assert_rc_zero`;
  # callers that want to ignore can read `_peko_iso_capture_rc`
  # directly.
  return 0
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

# Start a stdlib-only Python mock LLM (OpenAI-compatible SSE server) on
# 127.0.0.1, on an OS-assigned port. Unlike .github/docker/mock-llm/
# (which needs fastapi + uvicorn), this one uses only http.server, so
# it runs anywhere python3 does — no pip installs, no docker.
#
# Every POST gets the same reply: the given text, streamed word-by-word
# in OpenAI `delta.content` chunks with a per-word delay. Set the delay
# high (e.g. 0.5) to keep an agentic run in flight long enough for
# `peko stop` to land mid-stream; keep it tiny (0.01) for fast replies.
#
# Args:
#   $1  response text          (default: "mock reply")
#   $2  per-word delay, secs   (default: 0.01)
# Stdout: the bound port (ONLY the port — logs go to stderr, so callers
#   can do `port="$(peko_iso_start_mock_llm …)"`).
# The server PID is appended to <tempdir>/extra.pids (survives the
# command-substitution subshell), so peko_iso_done kills it on exit.
peko_iso_start_mock_llm() {
  local text="${1:-mock reply}" delay="${2:-0.01}"
  if ! command -v python3 >/dev/null 2>&1; then
    echo "❌ python3 not found — mock LLM unavailable" >&2
    return 1
  fi
  local py="$_PEKO_ISO_TEMPDIR/mock_llm.py"
  local port_file="$_PEKO_ISO_TEMPDIR/mock_llm.port"
  cat >"$py" <<'PY'
import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TEXT = sys.argv[1]
DELAY = float(sys.argv[2])


def chunk(obj):
    return ("data: " + json.dumps(obj) + "\n\n").encode()


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):  # silence per-request logging
        pass

    def do_GET(self):  # health probe
        body = b'{"status":"ok"}'
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):  # any path → chat completion
        length = int(self.headers.get("Content-Length") or 0)
        if length:
            self.rfile.read(length)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()
        try:
            words = TEXT.split(" ")
            for i, word in enumerate(words):
                emit = word if i == len(words) - 1 else word + " "
                self.wfile.write(chunk({
                    "choices": [
                        {"delta": {"content": emit}, "finish_reason": None}
                    ]
                }))
                self.wfile.flush()
                time.sleep(DELAY)
            self.wfile.write(chunk({
                "choices": [],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": len(words),
                    "total_tokens": 10 + len(words),
                },
            }))
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            # Client went away mid-stream (e.g. `peko stop` cancelled the
            # run). Nothing to do — the next request gets a fresh reply.
            pass


srv = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
print(srv.server_address[1], flush=True)  # stdout = port file
srv.serve_forever()
PY
  python3 "$py" "$text" "$delay" \
    >"$port_file" 2>"$_PEKO_ISO_TEMPDIR/mock_llm.err" &
  local pid=$!
  # Record the PID in a file, not the _PEKO_ISO_EXTRA_PIDS array: this
  # helper is normally called via `port="$(peko_iso_start_mock_llm …)"`
  # command substitution, which runs it in a subshell — array appends
  # would be lost. peko_iso_done kills every PID in this file.
  echo "$pid" >> "$_PEKO_ISO_TEMPDIR/extra.pids"

  # Wait for the server to print its OS-assigned port.
  local deadline=$((SECONDS + 10)) port
  while (( SECONDS < deadline )); do
    if [[ -s "$port_file" ]]; then
      port="$(head -1 "$port_file" | tr -d '[:space:]')"
      if [[ -n "$port" ]]; then
        echo "🤖 mock LLM on 127.0.0.1:$port (pid=$pid, delay=${delay}s/word)" >&2
        printf '%s' "$port"
        return 0
      fi
    fi
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.2
  done
  echo "❌ mock LLM did not start" >&2
  cat "$_PEKO_ISO_TEMPDIR/mock_llm.err" >&2 2>/dev/null || true
  return 1
}
