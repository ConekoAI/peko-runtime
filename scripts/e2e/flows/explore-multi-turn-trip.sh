#!/usr/bin/env bash
# scripts/e2e/flows/explore-multi-turn-trip.sh
#
# Multi-turn conversation against a real MiniMax API as a non-technical
# human user. Scenario: planning a 3-day weekend trip to Lisbon and
# following up on what the model says, instead of one giant prompt.
#
# Captures:
#   - wall time per turn
#   - token telemetry from stderr (`[peko] iterations=1 input=N output=M …`)
#   - the raw response text so the human-reader can spot prompt /
#     UX / accuracy issues
#   - assertions on quota counters / JSON envelope behaviour

flow_main() {
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "❌ MINIMAX_API_KEY env var not set — refusing to run" >&2
    return 64
  fi

  peko_iso_init "multi-turn-trip" || return 1

  # --- seed: model + principal ------------------------------------
  peko_iso_run model add \
      --template minimax \
      --model MiniMax-M3 \
      --key "$MINIMAX_API_KEY" >/dev/null
  peko_iso_assert_rc_zero

  peko_iso_run principal create travel-buddy --model minimax-MiniMax-M3 >/dev/null
  peko_iso_assert_rc_zero

  # --- start daemon (persona set goes through IPC → LLM) ----------
  peko_iso_start_daemon || return 1

  # Give the principal a persona (LLM-drafted).
  peko_iso_run principal persona set travel-buddy \
      --from "a friendly, budget-aware travel concierge who gives concrete, well-organised advice for short trips"
  peko_iso_assert_rc_zero

  # --- multi-turn conversation --------------------------------------
  local t0 dur
  echo
  echo "─── TURN 1 ─────────────────────────────────────────────"
  t0=$SECONDS
  peko_iso_run send travel-buddy "Hi! I'm planning a 3-day weekend trip to Lisbon in mid-September. I like walking, local food, and not-too-touristy neighbourhoods. Can you sketch a high-level itinerary? Keep it to ~5 lines."
  dur=$((SECONDS - t0))
  peko_iso_assert_rc_zero
  echo "$_peko_iso_capture_out"
  echo "[turn 1 wall time: ${dur}s]"

  echo
  echo "─── TURN 2 ─────────────────────────────────────────────"
  t0=$SECONDS
  peko_iso_run send travel-buddy "For day 2, can you give me 2-3 specific food spots you'd actually recommend? Nothing fancy — I'm happy with €10-15 mains, tascas and the like. Drop addresses if you can."
  dur=$((SECONDS - t0))
  peko_iso_assert_rc_zero
  echo "$_peko_iso_capture_out"
  echo "[turn 2 wall time: ${dur}s]"

  echo
  echo "─── TURN 3 ─────────────────────────────────────────────"
  t0=$SECONDS
  peko_iso_run send travel-buddy "Actually scratch the day-2 food list — I'm vegetarian. Same vibe (casual tasca, €10-15 mains), but with good vegetarian options. Two or three spots only."
  dur=$((SECONDS - t0))
  peko_iso_assert_rc_zero
  echo "$_peko_iso_capture_out"
  echo "[turn 3 wall time: ${dur}s]"

  echo
  echo "─── TURN 4 — log/--json sanity ─────────────────────────"
  peko_iso_run log travel-buddy --since 1h --json
  peko_iso_assert_rc_zero
  echo "log --json first line: $(echo "$_peko_iso_capture_out" | head -1)"

  echo
  echo "─── TURN 5 — quota probe ───────────────────────────────"
  peko_iso_run quota status travel-buddy
  peko_iso_assert_rc_zero
  echo "$_peko_iso_capture_out"
  peko_iso_run quota status travel-buddy --json
  echo "(quota status --json rc=$_peko_iso_capture_rc, stdout bytes=$(printf %s "$_peko_iso_capture_out" | wc -c | tr -d ' '))"

  echo
  echo "─── TURN 6 — stop sanity (idle thread) ─────────────────"
  # `peko stop` is idempotent: with nothing in flight it prints a
  # friendly "no running turn" notice and exits 0 (scripting-safe).
  peko_iso_run stop travel-buddy
  peko_iso_assert_rc_zero
  peko_iso_assert_contains "No running turn on thread 'owner' with principal 'travel-buddy'"

  peko_iso_done 0
}
