#!/usr/bin/env bash
# scripts/e2e/flows/subagent-result-persisted.sh
#
# WS3 (implicit session management, 2026-08-11): when a helper agent
# completes, its output lands in the parent's transcript as a
# first-class user-role message tagged `source: agent`. Stop the
# daemon, restart it, and verify the persisted line survives — proving
# it's a JSONL write, not just an in-memory injection.

flow_main() {
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "❌ MINIMAX_API_KEY env var not set — refusing to run" >&2
    return 64
  fi

  peko_iso_init "subagent-result-persisted" || return 1

  peko_iso_run model add \
      --template minimax \
      --model MiniMax-M3 \
      --key "$MINIMAX_API_KEY"
  peko_iso_assert_rc_zero

  peko_iso_run principal create sam --model minimax-MiniMax-M3
  peko_iso_assert_rc_zero

  peko_iso_start_daemon || return 1

  # Ask the model to delegate so a 'Helper: Agent' line gets written.
  # The prompt names the subagent_type explicitly ('primary') so the
  # tool call succeeds in a single iteration — otherwise the model
  # wastes turns on agent_catalog lookups and the run ends before the
  # helper's completion reaches the inbox.
  peko_iso_run send sam \
      "Use the Agent (subagent) tool with subagent_type='primary'. Pass this exact task: 'Write the one-word answer to: what colour is the sky on a clear day, and return just that word.' Don't answer the question yourself — only call the Agent tool with that prompt and report what the helper returned."
  peko_iso_assert_rc_zero

  # ── locate the live session JSONL ────────────────────────────────
  local live_jsonl
  live_jsonl=$(find "$PEKO_DATA_DIR/principals" -path '*/sessions/*.jsonl' 2>/dev/null \
      | grep -v '#' | head -1)
  if [[ -z "$live_jsonl" ]]; then
    echo "❌ no live session JSONL found under $PEKO_DATA_DIR/principals"
    return 1
  fi
  echo "live session: $live_jsonl"

  # ── assert a Helper line + source:agent tag exist ────────────────
  echo
  echo "─── POST: parent JSONL contains 📨 [Helper: Agent] + source ─"
  if grep -q "📨 \[Helper: Agent\]" "$live_jsonl"; then
    echo "✓ Helper line present in parent JSONL"
  else
    echo "❌ no Helper line in parent JSONL — WS3 didn't persist"
    return 1
  fi
  if grep -q '"source":"agent"' "$live_jsonl"; then
    echo "✓ source:agent tag present"
  else
    echo "❌ source:agent tag missing — WS3 missing the source field"
    return 1
  fi

  # ── stop + restart daemon; verify the line survives ──────────────
  echo
  echo "─── POST: stop+restart daemon, re-scan ───────────────────────"
  peko_iso_run daemon stop
  peko_iso_assert_rc_zero
  peko_iso_start_daemon || return 1
  if grep -q "📨 \[Helper: Agent\]" "$live_jsonl"; then
    echo "✓ Helper line persisted across restart (proves JSONL write, not in-memory)"
  else
    echo "❌ Helper line vanished after restart — WS3 was in-memory only"
    return 1
  fi

  peko_iso_done 0
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  source "$(dirname "$0")/../lib/isolate.sh"
  flow_main "$@"
fi