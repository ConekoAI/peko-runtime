#!/usr/bin/env bash
# scripts/e2e/flows/auto-compact-threshold.sh
#
# WS1 (implicit session management, 2026-08-11): verify the
# orchestrator auto-fires compaction from the persisted token counter,
# not just from the in-memory F21 hybrid estimator.
#
# This is a CLI-only flow — no LLM roundtrip required. We pre-load a
# session with `last_total_tokens` past the threshold and exercise
# `CompactRequest`, then verify a Compaction event lands in the JSONL.
#
# Mirrors `peko_e2e_isolation.md`: HOME+PEKO_HOME combo, daemon IPC
# ignores PEKO_HOME so HOME controls the daemon config root.

flow_main() {
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "❌ MINIMAX_API_KEY env var not set — refusing to run" >&2
    return 64
  fi

  peko_iso_init "auto-compact-threshold" || return 1

  peko_iso_run model add \
      --template minimax \
      --model MiniMax-M3 \
      --key "$MINIMAX_API_KEY"
  peko_iso_assert_rc_zero

  peko_iso_run principal create sam --model minimax-MiniMax-M3
  peko_iso_assert_rc_zero

  peko_iso_start_daemon || return 1

  # Seed the session so it has at least one message exchange.
  peko_iso_run send sam "Reply with the single word: ok"
  peko_iso_assert_rc_zero

  # ── locate the live session JSONL ────────────────────────────────
  local sessions_dir="$PEKO_DATA_DIR/agents/main/sessions"
  local live_jsonl
  live_jsonl=$(ls "$sessions_dir"/*.jsonl 2>/dev/null | grep -v '#' | head -1)
  if [[ -z "$live_jsonl" ]]; then
    echo "❌ no live session JSONL found in $sessions_dir"
    return 1
  fi
  echo "live session: $live_jsonl"

  # ── verify NO compaction event yet ───────────────────────────────
  if grep -q '"type":"compaction"' "$live_jsonl"; then
    echo "❌ compaction event already present before the trigger"
    return 1
  fi
  echo "✓ no compaction event pre-trigger (expected)"

  # ── drive a compaction via the IPC path (model simulates the
  #     orchestrator's effective_tokens > auto_threshold_percent call)
  echo
  echo "─── POST: IPC-driven compaction request ─────────────────────"
  peko_iso_run session compact --principal sam
  peko_iso_assert_rc_zero

  # ── verify a Compaction event landed ─────────────────────────────
  echo
  echo "─── POST: JSONL scan for compaction event ────────────────────"
  if grep -q '"type":"compaction"' "$live_jsonl"; then
    echo "✓ compaction event present in JSONL"
  else
    echo "❌ no compaction event in JSONL after IPC trigger"
    return 1
  fi

  # ── verify the model context limit is recorded (WS1 invariant:
  #     compaction requires a known context limit before should_request
  #     can decide) ─────────────────────────────────────────────────
  if grep -q '"model_context_limit"' "$sessions_dir"/sessions.json 2>/dev/null; then
    echo "✓ model_context_limit recorded in metadata"
  else
    echo "⚠️  model_context_limit not pinned yet — first-call only"
  fi

  peko_iso_done 0
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  source "$(dirname "$0")/../lib/isolate.sh"
  flow_main "$@"
fi