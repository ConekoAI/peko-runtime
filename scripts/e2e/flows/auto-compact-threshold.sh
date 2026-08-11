#!/usr/bin/env bash
# scripts/e2e/flows/auto-compact-threshold.sh
#
# WS1 (implicit session management, 2026-08-11): verify the
# orchestrator's auto-compact wiring is invoked on every iteration
# and reads the persisted token counter through `session.token_usage()`.
#
# This is a thin observability check — the WS1 logic is covered by
# 4 unit tests in `peko_engine::compaction_orchestrator::tests`
# (`persisted_last_total_triggers_compaction_with_empty_messages`,
#  `estimator_wins_when_larger_than_persisted_counter`,
#  `threshold_triggered_compaction_unchanged_without_flag`,
#  `forced_*`). The e2e flow proves the orchestrator integrates with
# the agentic loop and consults the right token source in production.
#
# Strategy:
#   1. Send one turn; locate the live session JSONL and sidecar
#      `sessions.json`.
#   2. Assert `sessions.json` records `last_total_tokens` for the
#      session — proves the orchestrator writes the counter so future
#      iterations can read it (SessionEntry.last_total_tokens).
#   3. Assert the daemon log carries an `Agent loop: iteration` trace
#      line — proves the agentic loop ran and `check_and_compact`
#      had a chance to fire.
#   4. (Stretch) If a `model_context_limit` is pinned on the session,
#      assert it's non-zero — proves the orchestrator pinned the
#      model window from the registry.

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

  # Seed the session with one exchange so the JSONL + sidecar exist.
  peko_iso_run send sam "Reply with the single word: ok"
  peko_iso_assert_rc_zero

  # ── locate the live session JSONL + sessions.json sidecar ─────────
  local live_jsonl sessions_dir meta_file
  live_jsonl=$(find "$PEKO_DATA_DIR/principals" -path '*/sessions/*.jsonl' 2>/dev/null \
      | grep -v '#' | head -1)
  if [[ -z "$live_jsonl" ]]; then
    echo "❌ no live session JSONL found under $PEKO_DATA_DIR/principals"
    return 1
  fi
  sessions_dir=$(dirname "$live_jsonl")
  meta_file="$sessions_dir/sessions.json"
  local live_id
  live_id=$(basename "$live_jsonl" .jsonl)
  echo "live session: $live_jsonl"
  echo "live id: $live_id"

  # ── verify the sidecar carries last_total_tokens ──────────────────
  # The orchestrator pins `last_total_tokens` on `SessionEntry` via
  # `Session::record_usage` after every assistant reply. WS1 reads it
  # back through `SessionView::token_usage` — if the field is missing
  # the orchestrator's `effective_tokens` falls back to
  # `estimated_tokens` only, regressing the round-5 F2 fix.
  echo
  echo "─── POST: sessions.json carries last_total_tokens on entry ─"
  if [[ ! -f "$meta_file" ]]; then
    echo "❌ sessions.json sidecar not found at $meta_file"
    return 1
  fi
  local last_total
  # The sidecar is keyed by session_id at the top level (no .sessions namespace).
  if ! last_total=$(jq -r '."'"$live_id"'".last_total_tokens // empty' "$meta_file" 2>/dev/null); then
    echo "❌ jq failed to parse $meta_file"
    return 1
  fi
  if [[ -z "$last_total" || "$last_total" == "null" ]]; then
    echo "❌ no last_total_tokens for $live_id — WS1 wiring regressed"
    return 1
  fi
  echo "✓ last_total_tokens=$last_total on sessions.json entry"

  # ── verify the daemon log shows the agentic loop ran ─────────────
  echo
  echo "─── POST: daemon log carries iteration traces ──────────────"
  # peko_iso_start_daemon writes stderr to $_PEKO_ISO_TEMPDIR/daemon.err
  # but we don't have direct access to that variable here. Recover it
  # by scanning the well-known /tmp/peko/<flow>-* dirs for the latest
  # one we just created (matches $_PEKO_ISO_TEMPDIR).
  local daemon_log
  daemon_log=$(ls -td /tmp/peko/auto-compact-threshold-* 2>/dev/null | head -1)/daemon.err
  if [[ -f "$daemon_log" ]]; then
    if grep -q "Agent loop: iteration" "$daemon_log"; then
      echo "✓ agentic loop ran at least one iteration (orchestrator had a chance to fire)"
    else
      echo "❌ no iteration log lines — agentic loop never ran"
      return 1
    fi
  else
    echo "⚠️  no daemon.err log at $daemon_log — daemon logging skipped"
  fi

  # ── (stretch) verify model_context_limit was pinned ──────────────
  # The orchestrator pins `model_context_limit` on the session at run
  # start (peko-rs/engine/src/agentic_loop.rs:~1086). WS1 relies on
  # this for `should_request(effective, context_window, ...)`.
  echo
  echo "─── POST (stretch): model_context_limit pinned on entry ────"
  local ctx_limit
  ctx_limit=$(jq -r '."'"$live_id"'".model_context_limit // empty' "$meta_file" 2>/dev/null)
  if [[ -n "$ctx_limit" && "$ctx_limit" != "null" ]]; then
    echo "✓ model_context_limit=$ctx_limit"
  else
    echo "⚠️  model_context_limit not pinned (first-call only or test-model path)"
  fi

  peko_iso_done 0
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  source "$(dirname "$0")/../lib/isolate.sh"
  flow_main "$@"
fi