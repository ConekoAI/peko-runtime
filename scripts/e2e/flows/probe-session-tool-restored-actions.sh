#!/usr/bin/env bash
# scripts/e2e/flows/probe-session-tool-restored-actions.sh
#
# Round-7 regression probe (2026-08-13): PR #353 (WS4) had demoted the
# session tool to 6 actions; round 7 restored the 3 storage-only ones
# (branch / archive / unarchive) and moved the 3 LLM-driving ones
# (new / resume / compact) onto the Agent tool as its `action` enum.
# The Agent tool's `resume_session` parameter is gone — resume now takes
# `session_key`.
#
# This probe verifies, via the model's own schema introspection:
#
#   A. The `session` tool advertises exactly the 9 storage actions
#      (status list history search rename delete branch archive
#      unarchive) and NONE of new/resume/compact.
#   B. The `Agent` tool's `action` enum is exactly [new, resume,
#      compact], it accepts a `session_key` property, and there is NO
#      `resume_session` property.
#
# Successor to probe-session-tool-schema.sh (which pinned the WS4
# 6-action surface; its demoted list is now wrong).
#
# Probes against the real LLM with $MINIMAX_API_KEY. Fails if any
# expected action is missing OR any off-surface action/parameter appears.
#
# Usage:
#   MINIMAX_API_KEY=... scripts/e2e/flows/probe-session-tool-restored-actions.sh
#
# Optional env:
#   KEEP_TEMPDIR=1   retain the tempdir for inspection (default: sweep)
#   MODEL=...        override the model (default: MiniMax-M3)
#
# Exit codes:
#   0  tool surfaces match the round-7 spec exactly
#   1  surface drift (missing or unexpected actions/parameters)
#   64 MINIMAX_API_KEY unset
#   *  any peko_iso_* assertion failure

flow_main() {
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "❌ MINIMAX_API_KEY env var not set — refusing to run" >&2
    return 64
  fi

  peko_iso_init "probe-session-tool-restored-actions" || return 1

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

  # ── PROBE A: session tool action surface ──────────────────────────
  echo
  echo "──── PROBE A: session tool action surface (expect 9) ────"
  peko_iso_run send probe 'Output ONLY a JSON array of action names that your `session` tool \
can be called with. No prose, no markdown, no commentary. Each element \
must be a single lowercase action verb that is a valid value for the \
action parameter. Do NOT call any tool; just answer from your schema.'
  peko_iso_assert_rc_zero
  echo "$_peko_iso_capture_out"
  local session_out="$_peko_iso_capture_out"

  local missing=0 unexpected=0
  local expected=(
    status
    list
    history
    search
    rename
    delete
    branch
    archive
    unarchive
  )
  # LLM-driving verbs live on the Agent tool now — they must NOT show
  # up in the session tool's action enum.
  local moved=(
    new
    resume
    compact
  )
  for action in "${expected[@]}"; do
    # Word-boundary match: 'archive' must not match inside 'unarchive'.
    if echo "$session_out" | grep -qiE "(^|[^a-z])${action}([^a-z]|$)"; then
      echo "  ✓ ${action}"
    else
      echo "  ❌ ${action}  ← model omitted this action"
      missing=$((missing + 1))
    fi
  done
  for action in "${moved[@]}"; do
    if echo "$session_out" | grep -qiE "(^|[^a-z])${action}([^a-z]|$)"; then
      echo "  ❌ ${action}  ← LLM-driving action still surfaces on the session tool"
      unexpected=$((unexpected + 1))
    else
      echo "  ✓ ${action} (moved to Agent tool, not surfaced here)"
    fi
  done

  if (( missing > 0 || unexpected > 0 )); then
    echo
    echo "❌ session tool surface NOT GREEN — missing=$missing, unexpected=$unexpected"
    peko_iso_done 1
    return 1
  fi
  echo
  echo "✓ session tool surface is exactly the 9 storage actions"

  # ── PROBE B: Agent tool action enum + properties ──────────────────
  echo
  echo "──── PROBE B: Agent tool action enum + parameter names ────"
  peko_iso_run send probe 'Output ONLY a JSON object with two keys describing your `Agent` tool: \
"action_enum" — a JSON array of the exact allowed values for its action parameter; \
"properties" — a JSON array of every parameter name the tool accepts. \
No prose, no markdown, no commentary. Do NOT call any tool; answer from your schema.'
  peko_iso_assert_rc_zero
  echo "$_peko_iso_capture_out"
  local agent_out="$_peko_iso_capture_out"

  local agent_missing=0
  for action in new resume compact; do
    # NOTE: 'resume' would also match inside 'resume_session' — the
    # resume_session absence check below guards that false-positive.
    if echo "$agent_out" | grep -qiE "(^|[^a-z])${action}([^a-z]|$)"; then
      echo "  ✓ action enum carries '${action}'"
    else
      echo "  ❌ action enum missing '${action}'"
      agent_missing=$((agent_missing + 1))
    fi
  done

  if echo "$agent_out" | grep -qE '"session_key"'; then
    echo "  ✓ 'session_key' parameter present"
  else
    echo "  ❌ 'session_key' parameter missing"
    agent_missing=$((agent_missing + 1))
  fi

  if echo "$agent_out" | grep -qE '"resume_session"'; then
    echo "  ❌ retired 'resume_session' parameter still on the schema"
    echo "$agent_out" | grep -E '"resume_session"' | head -3
    peko_iso_done 1
    return 1
  fi
  echo "  ✓ no 'resume_session' parameter"

  if (( agent_missing > 0 )); then
    echo
    echo "❌ Agent tool surface NOT GREEN — missing=$agent_missing"
    peko_iso_done 1
    return 1
  fi

  echo
  echo "✅ BOTH SURFACES GREEN — session: 9 storage actions; Agent: new/resume/compact + session_key, no resume_session"
  peko_iso_done 0
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  source "$(dirname "$0")/../lib/isolate.sh"
  flow_main "$@"
fi
