#!/usr/bin/env bash
# scripts/e2e/flows/probe-session-tool-schema.sh
#
# F5 regression probe (2026-08-11 round-4 addendum 3): does the model
# believe the unified `session` tool supports all 12 actions, or does
# it anchor on the legacy 3 (`status` / `list` / `history`) and refuse
# the lifecycle ops added in PR #351?
#
# Probes against the real LLM with $MINIMAX_API_KEY. The probe fails
# (non-zero exit) if any of the 12 expected action names is missing
# from the model's response.
#
# Usage:
#   MINIMAX_API_KEY=... scripts/e2e/flows/probe-session-tool-schema.sh
#
# Optional env:
#   KEEP_TEMPDIR=1   retain the tempdir for inspection (default: sweep)
#   MODEL=...        override the model (default: minimax-MiniMax-M3)
#
# Exit codes:
#   0  model listed all 12 actions in its response
#   1  at least one action missing from the model's response
#   64 MINIMAX_API_KEY unset
#   *  any peko_iso_* assertion failure

flow_main() {
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "❌ MINIMAX_API_KEY env var not set — refusing to run" >&2
    return 64
  fi

  peko_iso_init "probe-session-tool-schema" || return 1

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

  # ── the probe: ask the model what actions its `session` tool has ──
  echo
  echo "──── probe: asking model about session tool action surface ────"
  local prompt='List every action supported by your `session` tool. \
For each action, give one short sentence. Be exhaustive — do not \
omit any action. Output the actions as a bulleted list, in the order \
you would call them. Do NOT call any tool; just answer from your \
description and schema.'

  local t0 dur
  t0=$SECONDS
  peko_iso_run send probe "$prompt" --no-stream
  dur=$((SECONDS - t0))
  peko_iso_assert_rc_zero
  echo
  echo "wall time: ${dur}s"
  echo
  echo "$_peko_iso_capture_out"
  echo

  # ── assert all 12 actions appear in the response ───────────────────
  echo "──── F5 assertion: all 12 action names in the model response ──"
  local out="$_peko_iso_capture_out"
  local missing=0
  local expected=(
    status
    list
    history
    search
    branch
    rename
    archive
    unarchive
    delete
    compact
    new
    resume
  )
  for action in "${expected[@]}"; do
    # Case-insensitive grep; the model may capitalize.
    if echo "$out" | grep -qiE "(^|[^a-z])${action}([^a-z]|$)"; then
      echo "  ✓ ${action}"
    else
      echo "  ❌ ${action}  ← model omitted this action"
      missing=$((missing + 1))
    fi
  done

  if (( missing > 0 )); then
    echo
    echo "❌ F5 NOT MITIGATED — model is still anchoring on legacy 3;"
    echo "   $missing of 12 actions missing from response."
    peko_iso_done 1
    return 1
  fi

  echo
  echo "✅ F5 MITIGATED — model listed all 12 actions."
  peko_iso_done 0
}