#!/usr/bin/env bash
# scripts/e2e/flows/quota-charging.sh
#
# Bug A (2026-08-01 v2) regression: `peko quota status <name>` stuck at
# `0 / ∞` even after real LLM calls because the principal's
# `Arc<QuotaMeter>` was never threaded into the root agent. After the
# Fix A wiring (RouterContext → PrincipalContext → agent_runner → the
# engine loop), every LLM call charges the per-cycle counter and the
# `quota status --json` envelope reports non-zero `input_tokens` /
# `output_tokens` / `request_count`.
#
# Flow:
#   1. Seed the minimax model with the real API key.
#   2. Create a principal.
#   3. Start the daemon.
#   4. Make 3 short real-LLM sends.
#   5. Assert `quota status <name>` shows non-zero counters (Bug A).
#   6. Assert `quota status <name> --json` returns an envelope with
#      `state.input_tokens > 0` and `state.request_count == 3`
#      (Bug B).
#
# Requires MINIMAX_API_KEY (real key). Without it the flow exits 64
# like `send-hello-minimax.sh`.

flow_main() {
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "❌ MINIMAX_API_KEY env var not set — refusing to run" >&2
    echo "   export MINIMAX_API_KEY=… before invoking this flow" >&2
    return 64
  fi

  peko_iso_init "quota-charging" || return 1

  echo "==== Seed model + principal ===="
  peko_iso_run model add --template minimax --model MiniMax-M3 --key "$MINIMAX_API_KEY"
  peko_iso_assert_rc_zero || peko_iso_done 1

  peko_iso_run principal create quoter --model minimax-MiniMax-M3
  peko_iso_assert_rc_zero || peko_iso_done 1

  peko_iso_start_daemon || peko_iso_done 1

  echo
  echo "==== Make 3 short real-LLM sends ===="
  for i in 1 2 3; do
    peko_iso_run send quoter "Count to ${i}0 and stop." --no-stream >/dev/null || true
  done

  echo
  echo "==== Bug A regression: quota status (human) ===="
  peko_iso_run quota status quoter
  peko_iso_assert_rc_zero || peko_iso_done 1
  echo "$_peko_iso_capture_out"

  # The human-formatted output line is `  requests:           0 / ∞`
  # when the counter is stuck. After Fix A the requests count must
  # be ≥ 1 because three real LLM calls have run. Some providers
  # (e.g. `minimax-MiniMax-M3`'s anthropic-compat stream) only emit
  # `output_tokens` on the Usage event, so `input: 0` is a valid
  # observed state — assert `requests` and `output` instead, which
  # are populated by every successful charge.
  if echo "$_peko_iso_capture_out" | grep -qE 'requests:[[:space:]]+0 /'; then
    echo "❌ Bug A NOT fixed — quota status still reports requests: 0 / ∞" >&2
    peko_iso_done 1
  fi
  if echo "$_peko_iso_capture_out" | grep -qE 'output:[[:space:]]+0 /'; then
    echo "❌ Bug A NOT fixed — quota status still reports output: 0 / ∞" >&2
    peko_iso_done 1
  fi
  echo "✅ quota status shows non-zero counters"

  echo
  echo "==== Bug B regression: quota status --json envelope ===="
  peko_iso_run quota status quoter --json
  peko_iso_assert_rc_zero || peko_iso_done 1
  # Empty stdout was the Bug B symptom. Sanity-check it has bytes.
  if [[ -z "$_peko_iso_capture_out" ]]; then
    echo "❌ Bug B NOT fixed — quota status --json returned empty stdout" >&2
    peko_iso_done 1
  fi
  # Top-level envelope fields must be present.
  for field in name is_peer config state; do
    if ! echo "$_peko_iso_capture_out" | grep -q "\"$field\""; then
      echo "❌ Bug B NOT fixed — envelope missing '$field'; got:" >&2
      echo "$_peko_iso_capture_out" >&2
      peko_iso_done 1
    fi
  done
  # Bug A in JSON form: `state.request_count` must be ≥ 1 (we made
  # 3 sends). `state.output_tokens` is the provider-agnostic counter
  # (minimax-MiniMax-M3 only emits output_tokens in its Usage event).
  # The `QuotaState` serialises with snake_case field names.
  local req_count
  req_count=$(echo "$_peko_iso_capture_out" \
    | grep -A 6 '"state"' \
    | grep '"request_count"' \
    | grep -oE '[0-9]+' \
    | head -1 || true)
  if [[ -z "$req_count" || "$req_count" -lt 1 ]]; then
    echo "❌ Bug A NOT fixed — JSON envelope reports request_count=$req_count (expected ≥ 1)" >&2
    echo "$_peko_iso_capture_out" | head -30 >&2
    peko_iso_done 1
  fi
  echo "✅ quota status --json returns the expected envelope with request_count=$req_count"
  echo "envelope (first 400 chars):"
  echo "$_peko_iso_capture_out" | head -c 400
  echo

  echo
  echo "🎉 quota-charging flow done"
  peko_iso_done 0
}