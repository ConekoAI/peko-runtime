#!/usr/bin/env bash
# scripts/e2e/flows/explore-landlord-email.sh
#
# Non-technical-user exploratory flow. Scenario: a non-tech user asks
# a peko principal to help draft a polite-but-firm email to their
# landlord about a broken dishwasher. They iterate on the tone across
# multiple turns, then probe log / quota / persona surfaces.
#
# Why this is interesting vs the prior v1/v2 + multi-turn-trip reports:
#   - The task has a clear "right answer" structure (greeting, issue,
#     ask, deadline, sign-off) so we can grade prompt faithfulness
#   - The user pivots on tone 3 times — exercises the principal's
#     ability to honour user steering without re-explaining context
#   - The history grows each turn — exercises cost growth on the
#     billing side (input tokens rise turn over turn)
#   - It is the kind of "I have a real-world problem, help me write
#     to a human" task a non-tech user actually uses ChatGPT for
#
# Steps:
#   1. Seed minimax model (real LLM).
#   2. Create a "communication-helper" principal.
#   3. Draft a persona via the v2 builder.
#   4. TURN 1: open ask — "polite, firm, lease-aware" email.
#   5. TURN 2: steer — "actually be firmer, mention lease clause 4.2".
#   6. TURN 3: steer back — "softer, landlord is a friend, but still clear".
#   7. TURN 4: ask for a one-line SMS follow-up.
#   8. Sanity probes (log --json, quota status + --json, doctor,
#      persona show, principal show --json).

flow_main() {
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "❌ MINIMAX_API_KEY env var not set — refusing to run" >&2
    return 64
  fi

  peko_iso_init "explore-landlord-email" || return 1

  # ---- 1. Seed model ----
  peko_iso_run model add \
      --template minimax \
      --model MiniMax-M3 \
      --key "$MINIMAX_API_KEY" >/dev/null
  peko_iso_assert_rc_zero
  echo

  # ---- 2. Create blank principal ----
  peko_iso_run principal create comms-helper --model minimax-MiniMax-M3 >/dev/null
  peko_iso_assert_rc_zero
  echo

  # ---- 3. Draft persona ----
  peko_iso_start_daemon || return 1
  echo "─── Step 3: persona set --from ───"
  local t0 t1 dur
  t0=$SECONDS
  peko_iso_run principal persona set comms-helper \
      --from "a calm, careful writing helper for personal emails — prefers short, well-organised prose, explains tone choices, and asks one clarifying question if the situation is unclear"
  peko_iso_assert_rc_zero
  dur=$((SECONDS - t0))
  echo "persona_draft wall time: ${dur}s"
  echo

  # ---- 4. TURN 1: open ask ----
  echo "═══ TURN 1 — open ask, polite-but-firm, lease-aware ═══"
  t0=$SECONDS
  peko_iso_run send comms-helper \
      "Help me write a short email to my landlord. The dishwasher in my apartment has been broken for 10 days. I've reported it twice via text. I'd like a polite but firm email that asks for a specific repair date within the next 7 days. Keep it under 180 words. Use a professional but warm tone — I've lived here 4 years and we have a decent relationship. Don't sign the email with my name; just put [Your Name]." \
      --no-stream
  peko_iso_assert_rc_zero
  dur=$((SECONDS - t0))
  echo
  echo "─── Turn 1 reply ───"
  echo "$_peko_iso_capture_out"
  echo "─── end reply ───"
  echo "[turn 1 wall time: ${dur}s, output bytes: $(printf %s "$_peko_iso_capture_out" | wc -c | tr -d ' ')]"
  echo

  # ---- 5. TURN 2: steer firmer + lease clause ----
  echo "═══ TURN 2 — steer firmer, mention lease clause 4.2 ═══"
  t0=$SECONDS
  peko_iso_run send comms-helper \
      "Same situation, but make it firmer — I just discovered my lease says 'landlord shall maintain all major appliances in working order within 7 days of written notice'. Reference that as clause 4.2. Keep it under 200 words. Still warm, not legalistic." \
      --no-stream
  peko_iso_assert_rc_zero
  dur=$((SECONDS - t0))
  echo
  echo "─── Turn 2 reply ───"
  echo "$_peko_iso_capture_out"
  echo "─── end reply ───"
  echo "[turn 2 wall time: ${dur}s, output bytes: $(printf %s "$_peko_iso_capture_out" | wc -c | tr -d ' ')]"
  echo

  # ---- 6. TURN 3: steer back softer ----
  echo "═══ TURN 3 — steer back softer, but still clear ═══"
  t0=$SECONDS
  peko_iso_run send comms-helper \
      "Actually scratch the lease clause — I don't want to escalate. Make it softer. Landlords in my city are stretched thin and I want to keep the relationship good. Just ask nicely, mention the 10 days, and request a rough timeline. Same word count." \
      --no-stream
  peko_iso_assert_rc_zero
  dur=$((SECONDS - t0))
  echo
  echo "─── Turn 3 reply ───"
  echo "$_peko_iso_capture_out"
  echo "─── end reply ───"
  echo "[turn 3 wall time: ${dur}s, output bytes: $(printf %s "$_peko_iso_capture_out" | wc -c | tr -d ' ')]"
  echo

  # ---- 7. TURN 4: one-line SMS follow-up ----
  echo "═══ TURN 4 — short SMS follow-up variant ═══"
  t0=$SECONDS
  peko_iso_run send comms-helper \
      "Now give me a 2-sentence text-message version of the same ask, ready to paste into iMessage. Casual, one emoji max." \
      --no-stream
  peko_iso_assert_rc_zero
  dur=$((SECONDS - t0))
  echo
  echo "─── Turn 4 reply ───"
  echo "$_peko_iso_capture_out"
  echo "─── end reply ───"
  echo "[turn 4 wall time: ${dur}s, output bytes: $(printf %s "$_peko_iso_capture_out" | wc -c | tr -d ' ')]"
  echo

  # ---- 8. Sanity probes ----
  echo "═══ PROBES — log / quota / persona / doctor ═══"

  echo "─── log comms-helper --since 1h --json ───"
  peko_iso_run log comms-helper --since 1h --json
  peko_iso_assert_rc_zero
  echo "log --json first line:"
  echo "$_peko_iso_capture_out" | head -1
  echo "(log --json total lines: $(printf %s "$_peko_iso_capture_out" | wc -l | tr -d ' '))"
  echo

  echo "─── quota status comms-helper ───"
  peko_iso_run quota status comms-helper
  peko_iso_assert_rc_zero
  echo "$_peko_iso_capture_out"
  echo

  echo "─── quota status comms-helper --json ───"
  peko_iso_run quota status comms-helper --json
  echo "(rc=$_peko_iso_capture_rc, stdout bytes=$(printf %s "$_peko_iso_capture_out" | wc -c | tr -d ' '))"
  if [[ "$(printf %s "$_peko_iso_capture_out" | wc -c | tr -d ' ')" -gt 0 ]]; then
    echo "first 250 chars of --json:"
    printf %s "$_peko_iso_capture_out" | head -c 250
    echo
  fi
  echo

  echo "─── principal show comms-helper --json (verify persona shows) ───"
  peko_iso_run principal show comms-helper --json
  peko_iso_assert_rc_zero
  echo "first 400 chars:"
  printf %s "$_peko_iso_capture_out" | head -c 400
  echo
  echo

  echo "─── principal persona show comms-helper (was Bug D in v2, expected rc=2) ───"
  peko_iso_run principal persona show comms-helper || true
  echo "(rc=$_peko_iso_capture_rc — subcommand missing is expected; v2 fix only added persona fields to 'show --json' envelope)"
  echo "stderr (first 200): $(printf %s "$_peko_iso_capture_err" | head -c 200)"
  echo

  echo "─── system doctor (3 checks) ───"
  peko_iso_run system doctor
  echo

  echo "─── system doctor --verbose > /tmp/doctor.out  (Bug C repro) ───"
  local doctor_out="/tmp/doctor-verbose-probe.out"
  peko_iso_run system doctor --verbose
  if [[ "$_peko_iso_capture_err" == *$(printf '\x1b')* ]]; then
    echo "❌ ANSI codes leaked to stderr (Bug C regressed?)"
  else
    echo "stderr appears clean of ANSI codes (good)"
  fi
  # Also test redirected stdout
  "$_PEKO_ISO_BIN" system doctor --verbose >"$doctor_out" 2>&1
  if grep -q $'\x1b' "$doctor_out"; then
    echo "❌ ANSI codes leaked to redirected stdout (Bug C regressed?)"
    echo "first 200 bytes of file:"
    head -c 200 "$doctor_out" | od -c | head -8
  else
    echo "redirected stdout appears clean of ANSI codes (good)"
  fi
  rm -f "$doctor_out"
  echo

  echo "─── daemon status --json ───"
  peko_iso_run daemon status --json
  peko_iso_assert_rc_zero
  printf %s "$_peko_iso_capture_out" | head -c 200
  echo
  echo

  echo "─── ext list (built-in extensions) ───"
  peko_iso_run ext list
  peko_iso_assert_rc_zero
  echo "$_peko_iso_capture_out"
  echo

  echo "─── cron list (should be empty) ───"
  peko_iso_run cron list
  peko_iso_assert_rc_zero
  echo "$_peko_iso_capture_out"

  peko_iso_done 0
}
