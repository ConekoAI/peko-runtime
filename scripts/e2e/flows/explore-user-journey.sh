#!/usr/bin/env bash
# scripts/e2e/flows/explore-user-journey.sh
#
# Exploratory flow: a non-technical human user tries peko as if for the
# first time. Each step is a real `peko` invocation; assertions are
# deliberately loose — we want to SEE the output, not gate on it.
#
# Required env:
#   MINIMAX_API_KEY  — the real key.

# Args:
#   $1  principal name (default: scout)

flow_main() {
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "❌ MINIMAX_API_KEY env var not set — refusing to run" >&2
    return 64
  fi

  local principal_name="${1:-scout}"
  peko_iso_init "explore-user-journey" || return 1

  # ----- add model -----
  peko_iso_run model add \
      --template minimax \
      --model MiniMax-M3 \
      --key "$MINIMAX_API_KEY"
  peko_iso_assert_rc_zero

  # ----- explore the model catalog -----
  echo
  echo "=========================================="
  echo "STEP 1: List models (what does user see?)"
  echo "=========================================="
  peko_iso_run model list
  echo "rc=$_peko_iso_capture_rc"
  echo "STDOUT:"
  echo "$_peko_iso_capture_out"
  echo "STDERR:"
  echo "$_peko_iso_capture_err"

  # ----- create principal -----
  echo
  echo "=========================================="
  echo "STEP 2: Create principal '$principal_name'"
  echo "=========================================="
  peko_iso_run principal create "$principal_name" --model minimax-MiniMax-M3
  echo "rc=$_peko_iso_capture_rc"
  echo "STDOUT:"
  echo "$_peko_iso_capture_out"
  echo "STDERR:"
  echo "$_peko_iso_capture_err"

  # ----- list principals -----
  echo
  echo "=========================================="
  echo "STEP 3: List principals"
  echo "=========================================="
  peko_iso_run principal list
  echo "STDOUT:"
  echo "$_peko_iso_capture_out"

  # ----- show principal -----
  echo
  echo "=========================================="
  echo "STEP 4: Show principal '$principal_name'"
  echo "=========================================="
  peko_iso_run principal show "$principal_name"
  echo "STDOUT:"
  echo "$_peko_iso_capture_out" | head -60

  # ----- start daemon -----
  peko_iso_start_daemon || return 1

  # ----- send a REAL question (not just hello) -----
  echo
  echo "=========================================="
  echo "STEP 5: Send a real question (no-stream)"
  echo "=========================================="
  local q="In one short paragraph, what is the rust borrow checker and why does it matter?"
  local t0
  t0=$(date +%s%N)
  peko_iso_run send "$principal_name" "$q" --no-stream
  local rc=$_peko_iso_capture_rc
  local t1
  t1=$(date +%s%N)
  local latency_ms=$(( (t1 - t0) / 1000000 ))
  echo "rc=$rc  latency_ms=$latency_ms"
  echo "STDOUT:"
  echo "$_peko_iso_capture_out"
  if [[ -n "$_peko_iso_capture_err" ]]; then
    echo "STDERR:"
    echo "$_peko_iso_capture_err" | head -40
  fi

  # ----- check log -----
  echo
  echo "=========================================="
  echo "STEP 6: Read log"
  echo "=========================================="
  peko_iso_run log "$principal_name" --since 10m
  echo "STDOUT:"
  echo "$_peko_iso_capture_out" | head -100
  if [[ -n "$_peko_iso_capture_err" ]]; then
    echo "STDERR:"
    echo "$_peko_iso_capture_err" | head -20
  fi

  # ----- multi-turn follow-up -----
  echo
  echo "=========================================="
  echo "STEP 7: Multi-turn follow-up"
  echo "=========================================="
  peko_iso_run send "$principal_name" "Can you give me a concrete example of a borrow-check error and the fix?" --no-stream
  echo "rc=$_peko_iso_capture_rc"
  echo "STDOUT:"
  echo "$_peko_iso_capture_out"

  peko_iso_done 0
}
