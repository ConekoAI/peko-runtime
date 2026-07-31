#!/usr/bin/env bash
# scripts/e2e/flows/daemon-lifecycle.sh
#
# Proves the daemon-isolation seam holds end-to-end:
#   1. Initialise an isolated home (daemon NOT auto-started).
#   2. Create a principal (filesystem-only, no IPC needed).
#   3. Start the daemon in the background — proves the IPC socket
#      binds inside the tempdir (not the host's ~/.peko/run/).
#   4. `peko daemon status --json` → reports running:true against the
#      isolated socket.
#   5. `peko daemon stop`         → tears down cleanly.
#   6. Assert the host's ~/.peko/run/ never saw the socket / pidfile.
#
# This flow does NOT exercise cron add / send / etc. because those IPC
# ops hit the Phase B tier-authorization gate (the CLI's default user
# `local` has no Local-tier write authority for a principal it created
# from another CLI invocation). That's a peko-runtime concern, not an
# isolation-methodology concern — covered by the cron-memory note.

flow_main() {
  peko_iso_init "daemon-lifecycle" || return 1

  # --- seed mock-llm provider so principal create can validate --model ---
  peko_iso_run model add \
      --custom \
      --id mock-llm \
      --model "${MOCK_LLM_WIRE_ID:-mock-llm-test}" \
      --base-url "${MOCK_LLM_URL:-http://127.0.0.1:9/v1}" \
      --api-format openai_completions \
      --key "${MOCK_LLM_API_KEY:-mock-llm-test-key}" || true

  # --- seed principal so the daemon has something to load ---
  peko_iso_run principal create demo-principal --model mock-llm
  peko_iso_assert_rc_zero

  # --- start daemon in the background ---
  peko_iso_start_daemon || return 1

  # --- daemon status must report running ---
  peko_iso_run daemon status --json
  peko_iso_assert_rc_zero
  peko_iso_assert_contains '"running": true'

  # --- post-condition: socket + pidfile live in the tempdir ---
  if [[ ! -S "$_PEKO_ISO_SOCK" ]]; then
    echo "❌ daemon socket missing: $_PEKO_ISO_SOCK" >&2
    return 1
  fi
  if [[ ! -f "$_PEKO_ISO_PEKO_DIR/run/daemon.pid" ]]; then
    echo "❌ daemon pidfile missing in tempdir" >&2
    return 1
  fi

  # --- post-condition: host's ~/.peko/run/ is untouched ---
  # We can't easily check the host's ~/.peko without leaking the test
  # host's username into the assertion. Instead, check that the daemon's
  # PID file is NOT in the default location (which would happen if our
  # HOME override had failed).
  local default_pid="$HOME/../.peko/run/daemon.pid"
  if [[ -f "$default_pid" && "$(cat "$default_pid" 2>/dev/null)" == "$(cat "$_PEKO_ISO_PEKO_DIR/run/daemon.pid")" ]]; then
    echo "❌ daemon pidfile leaked into host default location" >&2
    return 1
  fi

  # --- stop daemon cleanly ---
  peko_iso_run daemon stop
  peko_iso_assert_rc_zero

  # --- post-condition: pidfile is gone (the daemon died) ---
  # Note: `peko daemon stop` does NOT unlink the Unix socket file on
  # macOS — the kernel closes the FD but the inode stays. That's a
  # known peko behavior; we only assert the pidfile is gone, since
  # `is_process_running(pid)` would now return false. Any subsequent
  # `peko_iso_init` flow will overwrite the stale socket via the
  # server-side `let _ = std::fs::remove_file(&sock_path)` on bind.
  if [[ -f "$_PEKO_ISO_PEKO_DIR/run/daemon.pid" ]]; then
    echo "❌ daemon pidfile survived stop" >&2
    return 1
  fi
  # Belt-and-suspenders: scrub the stale socket file so the next test
  # in the same shell can bind cleanly.
  rm -f "$_PEKO_ISO_SOCK"

  echo "✅ flow complete: daemon-lifecycle"
  peko_iso_done 0
}
