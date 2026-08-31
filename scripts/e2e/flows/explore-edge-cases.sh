#!/usr/bin/env bash
# scripts/e2e/flows/explore-edge-cases.sh
#
# Exploratory flow: stress-test edge cases and advanced features.
#
# Required env:
#   MINIMAX_API_KEY

flow_main() {
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "❌ MINIMAX_API_KEY env var not set" >&2
    return 64
  fi

  peko_iso_init "explore-edge-cases" || return 1

  peko_iso_run model add --template minimax --model MiniMax-M3 --key "$MINIMAX_API_KEY"
  peko_iso_assert_rc_zero

  peko_iso_run principal create scout --model minimax-MiniMax-M3
  peko_iso_assert_rc_zero

  peko_iso_start_daemon || return 1

  # ----- 1. Try to send to a non-existent principal -----
  echo
  echo "=========================================="
  echo "EDGE 1: Send to non-existent principal"
  echo "=========================================="
  peko_iso_run send ghost-principal "hi"
  echo "rc=$_peko_iso_capture_rc"
  echo "STDOUT:"
  echo "$_peko_iso_capture_out"
  echo "STDERR:"
  echo "$_peko_iso_capture_err"

  # ----- 2. Empty message -----
  echo
  echo "=========================================="
  echo "EDGE 2: Send empty message"
  echo "=========================================="
  peko_iso_run send scout ""
  echo "rc=$_peko_iso_capture_rc"
  echo "STDOUT:"
  echo "$_peko_iso_capture_out"
  echo "STDERR:"
  echo "$_peko_iso_capture_err"

  # ----- 3. JSON log output -----
  echo
  echo "=========================================="
  echo "EDGE 3: peko log --json"
  echo "=========================================="
  peko_iso_run log scout --since 1h --json
  echo "rc=$_peko_iso_capture_rc"
  echo "STDOUT (first 30 lines):"
  echo "$_peko_iso_capture_out" | head -30

  # ----- 4. Cron list (should be empty) -----
  echo
  echo "=========================================="
  echo "EDGE 4: peko cron list"
  echo "=========================================="
  peko_iso_run cron list
  echo "rc=$_peko_iso_capture_rc"
  echo "STDOUT:"
  echo "$_peko_iso_capture_out"

  # ----- 5. Protocol view via --json -----
  echo
  echo "=========================================="
  echo "EDGE 5: peko send --json (protocol view)"
  echo "=========================================="
  peko_iso_run send scout "say ok" --json
  echo "rc=$_peko_iso_capture_rc"
  echo "STDOUT (first 30 lines):"
  echo "$_peko_iso_capture_out" | head -30

  # ----- 6. system doctor -----
  echo
  echo "=========================================="
  echo "EDGE 6: peko system doctor"
  echo "=========================================="
  peko_iso_run system doctor
  echo "rc=$_peko_iso_capture_rc"
  echo "STDOUT (first 60 lines):"
  echo "$_peko_iso_capture_out" | head -60
  echo "STDERR:"
  echo "$_peko_iso_capture_err"

  # ----- 7. ext list (extensions) -----
  echo
  echo "=========================================="
  echo "EDGE 7: peko ext list"
  echo "=========================================="
  peko_iso_run ext list
  echo "rc=$_peko_iso_capture_rc"
  echo "STDOUT:"
  echo "$_peko_iso_capture_out"

  peko_iso_done 0
}
