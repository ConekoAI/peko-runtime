#!/usr/bin/env bash
# scripts/e2e/flows/probe-session-tool-schema.sh
#
# WS4 regression probe (2026-08-11 implicit session management):
# after WS4 demoted the 6 lifecycle actions, verify the model
# advertises exactly the 6 surviving actions and does NOT mention the
# demoted ones (`new`/`resume`/`branch`/`archive`/`unarchive`/`compact`).
#
# Probes against the real LLM with $MINIMAX_API_KEY. Fails if any
# surviving action is missing from the response OR any demoted action
# appears in the response.
#
# Usage:
#   MINIMAX_API_KEY=... scripts/e2e/flows/probe-session-tool-schema.sh
#
# Optional env:
#   KEEP_TEMPDIR=1   retain the tempdir for inspection (default: sweep)
#   MODEL=...        override the model (default: MiniMax-M3)
#
# Exit codes:
#   0  tool surface is exactly the 6 surviving actions
#   1  surface drift (missing or unexpected actions in the model's response)
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
  local prompt='Output ONLY a JSON array of action names that your `session` tool \
can be called with. No prose, no markdown, no commentary. Each element \
must be a single lowercase action verb that is a valid value for the \
action parameter. Do NOT call any tool; just answer from your schema.'

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

  # ── assert WS4 demote: 6 surviving actions appear, 6 demoted don't ─
  # WS4 (implicit session management, 2026-08-11) demoted
  # `new`/`resume`/`branch`/`archive`/`unarchive`/`compact` from the
  # tool surface — those ops are now engine-internal. The 6 surviving
  # actions must appear; the 6 demoted must NOT (the model would
  # otherwise try to invoke them and get a schema-validation error).
  echo
  echo "──── WS4 assertion: 6 surviving actions, 6 demoted removed ───"
  local out="$_peko_iso_capture_out"
  local missing=0
  local unexpected=0
  local expected=(
    status
    list
    history
    search
    rename
    delete
  )
  local demoted=(
    new
    resume
    branch
    archive
    unarchive
    compact
  )
  for action in "${expected[@]}"; do
    if echo "$out" | grep -qiE "(^|[^a-z])${action}([^a-z]|$)"; then
      echo "  ✓ ${action}"
    else
      echo "  ❌ ${action}  ← model omitted this action"
      missing=$((missing + 1))
    fi
  done
  for action in "${demoted[@]}"; do
    # Demoted actions should NOT appear in the model's description. A
    # stray mention (e.g. in a list of "things you cannot do") still
    # fails — proves the model isn't confused about which ops it owns.
    if echo "$out" | grep -qiE "(^|[^a-z])${action}([^a-z]|$)"; then
      echo "  ❌ ${action}  ← demoted action still surfaces (WS4 regressed)"
      unexpected=$((unexpected + 1))
    else
      echo "  ✓ ${action} (demoted, not surfaced)"
    fi
  done

  if (( missing > 0 || unexpected > 0 )); then
    echo
    echo "❌ WS4 NOT GREEN — missing=$missing, unexpected=$unexpected"
    peko_iso_done 1
    return 1
  fi

  echo
  echo "✅ WS4 GREEN — tool surface is the 6 surviving actions."
  peko_iso_done 0
}
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  source "$(dirname "$0")/../lib/isolate.sh"
  flow_main "$@"
fi
