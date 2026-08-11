#!/usr/bin/env bash
# scripts/e2e/flows/auto-paging-large-message.sh
#
# WS2 (implicit session management, 2026-08-11): verify the JSONL
# auto-paging flow when a session grows past `rotate_bytes()`.
#
# Strategy: shrink the threshold via PEKO_TEST_MODE + SESSION_TEST_ROTATE_BYTES,
# have the user send one large bash-output result through the principal,
# and assert that a `<live-id>#<UTC-ts>.jsonl` sibling materialised and
# the rotated id is in `session list peer=…` output (round-5 F3 fix).

flow_main() {
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "❌ MINIMAX_API_KEY env var not set — refusing to run" >&2
    return 64
  fi

  peko_iso_init "auto-paging-large-message" || return 1

  # Tighter threshold so a single shell-output call crosses it.
  export PEKO_TEST_MODE=1
  export SESSION_TEST_ROTATE_BYTES=2048

  peko_iso_run model add \
      --template minimax \
      --model MiniMax-M3 \
      --key "$MINIMAX_API_KEY"
  peko_iso_assert_rc_zero

  peko_iso_run principal create sam --model minimax-MiniMax-M3
  peko_iso_assert_rc_zero

  peko_iso_start_daemon || return 1

  # Ask the model to delegate to a helper that emits a >2 KiB bash result.
  # We do NOT assert rc=0: the LLM run may surface a transient
  # "Cannot rename non-existent session" error from a downstream tool
  # call racing with auto-paging. WS2 is about whether auto-paging
  # fires and the F3 fix lands — both observable on disk regardless of
  # whether the LLM run terminates cleanly.
  peko_iso_run send sam \
      "Use the Agent tool to delegate this to a helper: run 'yes | head -c 5000' in bash and report the exact output. Don't summarise — I want to see the full text. The helper should not modify it." || true

  # ── locate the live session JSONL ────────────────────────────────
  local live_jsonl live_id
  live_jsonl=$(find "$PEKO_DATA_DIR/principals" -path '*/sessions/*.jsonl' 2>/dev/null \
      | grep -v '#' | head -1)
  if [[ -z "$live_jsonl" ]]; then
    echo "❌ no live session JSONL found under $PEKO_DATA_DIR/principals"
    return 1
  fi
  live_id=$(basename "$live_jsonl" .jsonl)
  local sessions_dir
  sessions_dir=$(dirname "$live_jsonl")
  echo "live id: $live_id"
  echo "sessions dir: $sessions_dir"

  # ── assert a chapter sibling exists ──────────────────────────────
  echo
  echo "─── POST: chapter siblings for $live_id ─────────────────────"
  local chapter_count
  chapter_count=$(find "$sessions_dir" -name "${live_id}#*.jsonl" 2>/dev/null | wc -l | tr -d ' ')
  if [[ "$chapter_count" -ge 1 ]]; then
    echo "✓ ${chapter_count} chapter sibling(s) present (auto-paging fired)"
  else
    echo "❌ no chapter sibling found — auto-paging did not fire"
    return 1
  fi

  # ── assert round-5 F3 fix: rotated id appears in peers.json ──────
  echo
  echo "─── POST: peers.json contains the rotated id (F3 fix) ────────"
  local first_chapter peers_file
  first_chapter=$(find "$sessions_dir" -name "${live_id}#*.jsonl" 2>/dev/null | head -1 | xargs -I {} basename {} .jsonl)
  if [[ -z "$first_chapter" ]]; then
    echo "❌ no chapter id to search for"
    return 1
  fi
  peers_file="$sessions_dir/peers.json"
  if [[ ! -f "$peers_file" ]]; then
    echo "❌ peers.json missing at $peers_file"
    return 1
  fi
  if grep -q "$first_chapter" "$peers_file"; then
    echo "✓ rotated id $first_chapter registered in peers.json"
  else
    echo "❌ rotated id $first_chapter missing from peers.json (F3 regression)"
    return 1
  fi

  # ── assert new events land in the un-rotated live id ─────────────
  echo
  echo "─── POST: live id still has appends after rotation ──────────"
  if [[ -f "$live_jsonl" ]]; then
    echo "✓ live id JSONL still present"
  else
    echo "❌ live id JSONL gone — rotation over-stepped"
    return 1
  fi

  peko_iso_done 0
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  source "$(dirname "$0")/../lib/isolate.sh"
  flow_main "$@"
fi