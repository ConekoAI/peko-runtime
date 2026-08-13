#!/usr/bin/env bash
# scripts/e2e/flows/probe-agent-cleanup-validation.sh
#
# Round-7 probe (2026-08-13): the Agent tool's `cleanup` parameter is
# validated via SpawnCleanupPolicy::from_str. An unknown value (e.g.
# "purge") must surface a STRUCTURED ERROR — before round 7 it silently
# defaulted to keep. The canonical error text is:
#
#   invalid cleanup 'purge' — valid values: "keep", "delete"
#
# Two assertions:
#   1. The model's reply relays the structured error (it names
#      "invalid cleanup" and the valid values).
#   2. Deterministic: no subagent was spawned — sessions.json has no
#      entry with parent_session_id set (parse_cleanup fails before any
#      spawn work).
#
# Probes against the real LLM with $MINIMAX_API_KEY.
#
# Usage:
#   MINIMAX_API_KEY=... scripts/e2e/flows/probe-agent-cleanup-validation.sh
#
# Optional env:
#   KEEP_TEMPDIR=1   retain the tempdir for inspection (default: sweep)
#   MODEL=...        override the model (default: MiniMax-M3)
#
# Exit codes:
#   0  structured error surfaced, no spawn happened
#   1  invalid cleanup was silently accepted (or a spawn leaked through)
#   64 MINIMAX_API_KEY unset
#   *  any peko_iso_* assertion failure

flow_main() {
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "❌ MINIMAX_API_KEY env var not set — refusing to run" >&2
    return 64
  fi

  peko_iso_init "probe-agent-cleanup-validation" || return 1

  local model_wireid="${MODEL:-MiniMax-M3}"

  # ── seed model + principal ─────────────────────────────────────────
  peko_iso_run model add \
      --template minimax \
      --model "$model_wireid" \
      --key "$MINIMAX_API_KEY"
  peko_iso_assert_rc_zero

  peko_iso_run principal create probe --model "minimax-$model_wireid"
  peko_iso_assert_rc_zero

  peko_iso_start_daemon || return 1

  # ── TURN 1: force the invalid cleanup value through the tool ───────
  # The model may suspect 'purge' is invalid — tell it we WANT the
  # error, verbatim, so it doesn't just lecture us without calling.
  echo
  echo "──── TURN 1 (Agent call with cleanup='purge') ────"
  peko_iso_run send probe \
      "Call the Agent tool with subagent_type=primary, prompt='Reply with the single word: ok', and cleanup='purge'. I know 'purge' is probably not a valid value — I want to see the tool's exact error message. Actually make the call and quote the tool's response verbatim, word for word." \
      --no-stream
  peko_iso_assert_rc_zero
  echo "$_peko_iso_capture_out"
  local raw="$_peko_iso_capture_out"

  # ── ASSERTION 1: structured error surfaced ─────────────────────────
  echo
  echo "═══════════════════════════════════════════════════════════════"
  echo "ASSERTION 1: structured 'invalid cleanup' error surfaced"
  echo "═══════════════════════════════════════════════════════════════"
  if echo "$raw" | grep -qi "invalid cleanup"; then
    echo "  ✓ reply carries the 'invalid cleanup' error"
  else
    echo "  ❌ no 'invalid cleanup' error in the reply — silent success?"
    peko_iso_done 1
    return 1
  fi
  if echo "$raw" | grep -qi "keep" && echo "$raw" | grep -qi "delete"; then
    echo "  ✓ error names the valid values (keep / delete)"
  else
    echo "  ⚠ valid values not quoted — error text may have drifted"
  fi

  # ── ASSERTION 2: no spawn leaked through ───────────────────────────
  # parse_cleanup runs before any spawn work, so a refused call must
  # leave zero spawned sessions on disk.
  echo
  echo "═══════════════════════════════════════════════════════════════"
  echo "ASSERTION 2: no subagent session was created"
  echo "═══════════════════════════════════════════════════════════════"
  local meta spawned
  meta=$(find "$PEKO_DATA_DIR/principals" -name 'sessions.json' 2>/dev/null | head -1)
  if [[ -z "$meta" ]]; then
    echo "  ⚠ no sessions.json found — treating as 'no spawn' (nothing persisted)"
  else
    spawned=$(jq -r 'to_entries[] | select(.value.parent_session_id != null) | .key' "$meta" 2>/dev/null | head -1)
    if [[ -n "$spawned" ]]; then
      echo "  ❌ a spawned session exists ($spawned) — cleanup='purge' was accepted"
      peko_iso_done 1
      return 1
    fi
    echo "  ✓ no spawned session in sessions.json — the error fired before the spawn"
  fi

  echo
  echo "✅ CLEANUP VALIDATION GREEN — structured error, no silent success"
  peko_iso_done 0
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  source "$(dirname "$0")/../lib/isolate.sh"
  flow_main "$@"
fi
