#!/usr/bin/env bash
# scripts/e2e/flows/explore-tool-presence-probe.sh
#
# Root-cause probe: does a fresh principal actually have Bash / curl /
# Agent (subagent) in its tool catalog? Did the travel-buddy principal
# simply not reach for them in turns 2/3, or are they gated?
#
# Run two prompts back-to-back:
#   A. "use the Bash tool with curl https://api.allorigins.win/raw?url=…
#       to fetch the Wikipedia intro to vegetarian cuisine in Portugal"
#       — if Bash works, the principal COULD have researched turn 2/3
#       topics and just didn't.
#   B. "spawn a sub-agent (use the Agent tool) to research veg-friendly
#       tascas in Lisbon" — if Agent works, the model accepted the
#       earlier "spin up a sub-agent" offer in turns 2/3 but the user
#       had no verb for it.

flow_main() {
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "❌ MINIMAX_API_KEY env var not set" >&2
    return 64
  fi

  peko_iso_init "tool-presence-probe" || return 1

  peko_iso_run model add --template minimax --model MiniMax-M3 \
      --key "$MINIMAX_API_KEY" >/dev/null
  peko_iso_assert_rc_zero

  peko_iso_run principal create probe --model minimax-MiniMax-M3 >/dev/null
  peko_iso_assert_rc_zero

  peko_iso_start_daemon || return 1

  local t0 dur

  echo
  echo "─── A: explicit Bash + curl request ─────────────────────"
  t0=$SECONDS
  peko_iso_run send probe \
      "Use the Bash tool with the command: curl -sS 'https://en.wikipedia.org/wiki/Vegetarian_cuisine' | head -c 1500. Then in 2 lines tell me what you actually fetched." \
      --no-stream
  dur=$((SECONDS - t0))
  echo "rc=$_peko_iso_capture_rc, wall=${dur}s"
  echo "$_peko_iso_capture_out"

  echo
  echo "─── B: explicit Agent (subagent) request ───────────────"
  t0=$SECONDS
  peko_iso_run send probe \
      "Use the Agent (subagent) tool to spawn a research subagent that returns a one-paragraph answer about vegetarian food in Lisbon. Then summarise what the subagent said in 3 lines." \
      --no-stream
  dur=$((SECONDS - t0))
  echo "rc=$_peko_iso_capture_rc, wall=${dur}s"
  echo "$_peko_iso_capture_out"

  echo
  echo "─── C: open-ended (does model reach for Bash on its own?) ─"
  t0=$SECONDS
  peko_iso_run send probe \
      "What's a good vegetarian tasca in Lisbon's Príncipe Real neighbourhood with €10-15 mains?" \
      --no-stream
  dur=$((SECONDS - t0))
  echo "rc=$_peko_iso_capture_rc, wall=${dur}s"
  echo "$_peko_iso_capture_out"

  peko_iso_done 0
}
