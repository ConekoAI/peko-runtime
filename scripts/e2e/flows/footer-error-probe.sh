#!/usr/bin/env bash
# scripts/e2e/flows/footer-error-probe.sh
#
# Force a tool-call error to verify Fix D surfaces `tools_failed > 0` in
# the `--no-stream` footer. Asks the LLM to call PePlanCreate with two
# nodes sharing the same explicit nodeId — Fix B rejects this with
# `PlanError::InvalidNodeId`, which the daemon captures and forwards
# on the `RunSummary` packet.

flow_main() {
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "❌ MINIMAX_API_KEY env var not set — refusing to run" >&2
    return 64
  fi

  peko_iso_init "footer-error-probe" || return 1

  peko_iso_run model add --template minimax --model MiniMax-M3 --key "$MINIMAX_API_KEY"
  peko_iso_assert_rc_zero
  peko_iso_run principal create probe --model minimax-MiniMax-M3
  peko_iso_assert_rc_zero
  peko_iso_start_daemon || return 1

  for cap in PePlanCreate PePlanList PePlanGet PePlanMarkStep \
             PePlanRecordEvidence PePlanAddStep PePlanClose; do
    peko_iso_run capability grant --principal probe "tool:$cap"
    peko_iso_assert_rc_zero
  done

  # Capture both stdout (assistant text) and stderr (footer) directly.
  echo "=== stdout (assistant text) ==="
  peko_iso_run send probe 'Call PePlanCreate with title="Dup" and nodes=[
    {"nodeId":"node_same","step":"first"},
    {"nodeId":"node_same","step":"second"}
  ]. Then reply with the tool error message verbatim.' --no-stream
  peko_iso_assert_rc_zero

  echo "=== stderr (footer — tools_failed should be >= 1) ==="
  echo "$_peko_iso_capture_err"

  # Footer assertion: must include `tools_failed=N` where N >= 1.
  if [[ "$_peko_iso_capture_err" != *"tools_failed="* ]]; then
    echo "❌ footer missing tools_failed=" >&2
    return 1
  fi

  # Extract the tools_failed count from the footer and assert it's >= 1.
  local tools_failed
  tools_failed="$(echo "$_peko_iso_capture_err" | grep -oE 'tools_failed=[0-9]+' | head -1 | cut -d= -f2)"
  if (( tools_failed < 1 )); then
    echo "❌ expected tools_failed >= 1, got $tools_failed" >&2
    return 1
  fi

  echo "✅ footer-error-probe: tools_failed=$tools_failed"
  peko_iso_done 0
}