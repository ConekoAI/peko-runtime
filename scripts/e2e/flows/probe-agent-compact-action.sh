#!/usr/bin/env bash
# scripts/e2e/flows/probe-agent-compact-action.sh
#
# Round-7 probe (2026-08-13): the Agent tool's `action="compact"` flags
# a session for engine-driven compaction at its next run. It returns
# immediately with a success payload ({session_id, message}) and sets
# `compact_requested: true` on the target's sessions.json index entry
# (SessionManager::set_compact_requested).
#
# Flow:
#   1. Spawn a helper (action=new, cleanup=keep) — one turn.
#   2. Resume it (action=resume + session_key) — second turn, so the
#      helper has a multi-turn conversation.
#   3. Call Agent with action=compact + session_key on the helper.
#   4. Assert the model reports the scheduling message, then assert on
#      disk that sessions.json carries compact_requested:true for the
#      helper's entry (the deterministic half of the probe).
#
# Probes against the real LLM with $MINIMAX_API_KEY.
#
# Usage:
#   MINIMAX_API_KEY=... scripts/e2e/flows/probe-agent-compact-action.sh
#
# Optional env:
#   KEEP_TEMPDIR=1   retain the tempdir for inspection (default: sweep)
#   MODEL=...        override the model (default: MiniMax-M3)
#
# Exit codes:
#   0  compact returned success and compact_requested:true is persisted
#   1  compact errored, or the flag is missing from sessions.json
#   64 MINIMAX_API_KEY unset
#   *  any peko_iso_* assertion failure

flow_main() {
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "❌ MINIMAX_API_KEY env var not set — refusing to run" >&2
    return 64
  fi

  peko_iso_init "probe-agent-compact-action" || return 1

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

  # ── TURN 1: spawn a persistent helper ─────────────────────────────
  echo
  echo "──── TURN 1 (spawn helper, cleanup=keep) ────"
  peko_iso_run send probe \
      "Delegate this to a helper agent (use the Agent tool, subagent_type=primary, cleanup=keep): write a single sentence about lighthouses. Tell me the helper's session id when done." \
      --no-stream
  peko_iso_assert_rc_zero
  echo "$_peko_iso_capture_out"

  # ── TURN 2: resume the helper → multi-turn conversation ────────────
  echo
  echo "──── TURN 2 (resume helper via action=resume) ────"
  peko_iso_run send probe \
      "Use the Agent tool with action=resume, session_key=<the helper session id from your previous turn>, subagent_type=primary, and prompt: 'Now add a second sentence about foghorns.' Tell me what the helper returned." \
      --no-stream
  peko_iso_assert_rc_zero
  echo "$_peko_iso_capture_out"

  # ── locate the helper session id on disk (deterministic) ──────────
  # Spawned sessions carry parent_session_id in sessions.json.
  local meta helper_id
  meta=$(find "$PEKO_DATA_DIR/principals" -name 'sessions.json' 2>/dev/null | head -1)
  if [[ -z "$meta" ]]; then
    echo "❌ sessions.json not found under $PEKO_DATA_DIR/principals"
    peko_iso_done 1
    return 1
  fi
  helper_id=$(jq -r 'to_entries[] | select(.value.parent_session_id != null) | .key' "$meta" 2>/dev/null | head -1)
  if [[ -z "$helper_id" ]]; then
    echo "❌ no spawned session (parent_session_id set) found in $meta"
    peko_iso_done 1
    return 1
  fi
  echo
  echo "helper session id (from sessions.json): $helper_id"

  # ── TURN 3: compact the helper via the Agent tool ──────────────────
  echo
  echo "──── TURN 3 (Agent action=compact on the helper) ────"
  peko_iso_run send probe \
      "Use the Agent tool with action=compact and session_key='$helper_id'. No prompt or subagent_type needed. Tell me exactly what the tool returned." \
      --no-stream
  peko_iso_assert_rc_zero
  echo "$_peko_iso_capture_out"

  # The success payload's message is "Compaction scheduled — the engine
  # summarizes the session at its next run…". The model was asked to
  # relay the tool result, so some form of 'scheduled' must surface.
  local raw="$_peko_iso_capture_out"
  if echo "$raw" | grep -qiE "schedul|flagged"; then
    echo "  ✓ model reports the compact request was accepted"
  else
    echo "  ❌ no success signal in the reply — compact may have been refused:"
    echo "$raw" | grep -iE "error|refuse|cannot|not found" | head -3
    peko_iso_done 1
    return 1
  fi
  if echo "$raw" | grep -qiE "invalid cleanup|unknown action|requires 'session_key'|cannot compact"; then
    echo "  ❌ reply carries a structured refusal"
    peko_iso_done 1
    return 1
  fi

  # ── POST: sessions.json carries compact_requested:true ─────────────
  echo
  echo "─── POST: index entry for $helper_id has compact_requested:true ─"
  local flag
  flag=$(jq -r '."'"$helper_id"'".compact_requested // empty' "$meta" 2>/dev/null)
  if [[ "$flag" == "true" ]]; then
    echo "  ✓ compact_requested:true persisted on the helper's index entry"
  else
    echo "  ❌ compact_requested is '${flag:-<missing>}' — flag not persisted"
    echo "     entry: $(jq -c '."'"$helper_id"'"' "$meta" 2>/dev/null)"
    peko_iso_done 1
    return 1
  fi

  echo
  echo "✅ COMPACT ACTION GREEN — success response + persisted flag"
  peko_iso_done 0
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  source "$(dirname "$0")/../lib/isolate.sh"
  flow_main "$@"
fi
