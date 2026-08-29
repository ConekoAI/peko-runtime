#!/usr/bin/env bash
# scripts/e2e/flows/explore-subagent-session-r7-2026-08-13.sh
#
# Real-LLM multi-turn field test (round 7) — port of the r6 flow
# (explore-subagent-session-r6-2026-08-12.sh) to the round-7 surface:
#
#   - The chapter concept is DELETED. Session ids are stable for life;
#     oversized JSONLs page in place to `<id>.N.jsonl`; `chapters.json`
#     no longer exists. (Deep assertions: probe-no-chapter-suffix.sh.)
#
#   - The session tool exposes 9 storage actions: status / list /
#     history / search / rename / delete / branch / archive / unarchive.
#     (Schema pin: probe-session-tool-restored-actions.sh.)
#
#   - The Agent tool exposes 3 LLM-driving actions via the `action`
#     param (default "new"): new / resume / compact. `resume_session`
#     is gone — resume takes `session_key`. `cleanup` is validated
#     (keep/delete) with a structured error.
#
# What changed vs r6:
#   - T10 resume now drives Agent action=resume + session_key.
#   - T16 (find chapters by '#' suffix) is INVERTED: assert none exist.
#   - T17 "start fresh" probe stays observational — the root session is
#     continuous and engine-managed.
#   - NEW: branch / archive / unarchive exercises (T12–T14) and an
#     Agent action=compact exercise (T19) with an on-disk
#     compact_requested check.
#
# Persona: Same as r5/r6 — Sam, Clay & Ember pottery studio.

flow_main() {
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "❌ MINIMAX_API_KEY env var not set — refusing to run" >&2
    return 64
  fi

  peko_iso_init "explore-subagent-session-r7-2026-08-13" || return 1

  # ── seed model + principal + persona ──────────────────────────────
  peko_iso_run model add \
      --template minimax \
      --model MiniMax-M3 \
      --key "$MINIMAX_API_KEY"
  peko_iso_assert_rc_zero

  peko_iso_run principal create sam --model minimax-MiniMax-M3
  peko_iso_assert_rc_zero

  peko_iso_start_daemon || return 1

  peko_iso_run principal persona set sam \
      --from "a friendly, concise assistant for Sam, who runs a small pottery studio called Clay & Ember and likes short answers"
  peko_iso_assert_rc_zero

  local t0 dur
  _r7_turn() {
    local label="$1"
    shift
    echo
    echo "─── ${label} ───"
    t0=$SECONDS
    peko_iso_run "$@"
    dur=$((SECONDS - t0))
    peko_iso_assert_rc_zero
    echo "wall: ${dur}s"
    echo "$_peko_iso_capture_out"
  }

  # ── TURN 1: memory seed ───────────────────────────────────────────
  _r7_turn "TURN 1 (memory seed)" send sam "Remember: my studio is called Clay & Ember, I teach a beginner wheel class every Saturday at 10am, my favorite glaze is celadon, and I'm working on a new tea-bowl line called 'ember-glaze'. Reply with a one-line confirmation."

  # ── TURN 2: probe the surface — expect the 9 storage actions ──────
  _r7_turn "TURN 2 (probe session tool surface)" send sam "Use the session tool with action=status on the current session. Then read the tool's description and list every action it accepts. Tell me what you find."

  # ── TURN 3: list sessions, no filter ─────────────────────────────
  _r7_turn "TURN 3 (list — no filter, baseline)" send sam "Use the session tool with action=list (no filters) and tell me how many sessions exist."

  # ── TURN 4: find the live session (parent_session_id is empty) ────
  _r7_turn "TURN 4 (find the live session)" send sam "Show me my live user session (the one I just started talking to). Use the session tool however you need to."

  # ── TURN 5: start-fresh probe — root session is engine-managed ────
  _r7_turn "TURN 5 (start fresh — observe surface)" send sam "I want to start a fresh conversation and forget the current one. How would you do that? Don't actually do anything yet, just tell me what you'd call."

  # ── TURN 6: delegate to a persistent helper subagent ─────────────
  _r7_turn "TURN 6 (Agent action=new — persistent worker)" send sam "Delegate this to a helper agent (use the Agent tool, subagent_type=primary, cleanup=keep): write a one-paragraph marketing blurb for an 'ember-glaze' tea-bowl line, suitable for our Instagram. Don't do it yourself. Tell me the helper's session id."

  # ── TURN 7: multi-subagent delegation — 3 helpers in parallel ────
  _r7_turn "TURN 7 (3 parallel helpers)" send sam "Delegate three small tasks in parallel to helper agents (subagent_type=primary, cleanup=keep): (a) one helper writes 3 social-media hashtags for the 'ember-glaze' line, (b) one writes a 1-sentence tagline for our Saturday beginner class, (c) one writes 1 line of poetry about celadon glaze. Use multiple Agent tool calls in the same turn if you can. Tell me what you got from each helper."

  # ── TURN 8: find subagent sessions via parent_session_id ─────────
  _r7_turn "TURN 8 (find subagent sessions)" send sam "Show me all the helper sessions you spawned this conversation. Use the session tool however you need to. I expect at least 4 (one from TURN 6, three from TURN 7)."

  # ── TURN 9: list without filter — see parent_session_id for each ─
  _r7_turn "TURN 9 (list no filter — see parent_session_id)" send sam "Now use the session tool with action=list (no filters). For each session, tell me its session_id and parent_session (you can get parent_session via action=status on each id)."

  # ── TURN 10: resume a spawned session via Agent action=resume ─────
  # Round-7: `resume_session` is gone; resume takes session_key.
  _r7_turn "TURN 10 (Agent action=resume on helper from TURN 6)" send sam "Find the helper session you spawned in turn 6 (the marketing blurb one). Use the Agent tool with action=resume, session_key=<that id>, subagent_type=primary, and prompt: 'Can you rewrite the blurb to also mention our Saturday beginner class?' Tell me what the helper returned."

  # ── TURN 11: history on the resumed helper — verify history grew ──
  _r7_turn "TURN 11 (history on resumed helper)" send sam "Use the session tool with action=history on the same helper session id. How many messages are in it now? Did the resume add a new turn?"

  # ── TURN 12: branch a helper session (restored storage action) ────
  _r7_turn "TURN 12 (branch a helper from TURN 7)" send sam "Pick any one helper session from turn 7 and use the session tool with action=branch on it (give it the label 'r7-branch-test'). Then use action=list and confirm the branched copy shows up as its own session. Tell me both ids."

  # ── TURN 13: archive a helper — hidden from default list ─────────
  _r7_turn "TURN 13 (archive a helper from TURN 7)" send sam "Pick a different helper session from turn 7 and use the session tool with action=archive on it. Then action=list (no filters) — is it hidden? Then action=list with include_archived=true — does it show up again? Tell me what you see."

  # ── TURN 14: unarchive it — visible again ─────────────────────────
  _r7_turn "TURN 14 (unarchive the same helper)" send sam "Use the session tool with action=unarchive on the session you just archived. Then action=list (no filters) and confirm it's visible again."

  # ── TURN 15: delete a spawned helper — should work ───────────────
  _r7_turn "TURN 15 (delete the third helper from TURN 7)" send sam "Pick the one remaining helper session from turn 7 that you haven't branched or archived, and use the session tool with action=delete on it. Then confirm with another action=list that it's gone."

  # ── TURN 16: rename the live session — should refuse ─────────────
  _r7_turn "TURN 16 (rename current session — should refuse)" send sam "Use the session tool with action=rename on the current session. Title it 'r7-test-2026-08-13'. Tell me exactly what the tool returned."

  # ── TURN 17: delete current session — should refuse ──────────────
  _r7_turn "TURN 17 (delete current — should refuse)" send sam "Use the session tool with action=delete on the current session. Tell me exactly what the tool returns."

  # ── TURN 18: search across all sessions ──────────────────────────
  _r7_turn "TURN 18 (search query='ember-glaze')" send sam "Use the session tool with action=search and query='ember-glaze'. Show me the hits."

  # ── TURN 19: compact the helper from TURN 6 via Agent ─────────────
  _r7_turn "TURN 19 (Agent action=compact on helper from TURN 6)" send sam "Use the Agent tool with action=compact and session_key=<the marketing-blurb helper's session id from turn 6>. Tell me exactly what the tool returned."

  # ── TURN 20: probe Agent's depth/concurrency surface ─────────────
  _r7_turn "TURN 20 (probe depth/concurrency in description)" send sam "Read the Agent tool's description carefully and tell me: what is the maximum spawn depth allowed, and how many concurrent subagents are allowed? Quote the exact phrases from the description."

  # ── TURN 21: status of current + a spawned session ────────────────
  _r7_turn "TURN 21 (status — current vs spawned)" send sam "Use the session tool with action=status on (a) the current session, and (b) the helper session from turn 6. Compare: id, parent_session, message count, last activity."

  # ── TURN 22: final list to see end state ─────────────────────────
  _r7_turn "TURN 22 (final list — end state)" send sam "Final check: use the session tool with action=list (no filter, include_archived=true). Tell me by parent_session_id which sessions are live or subagent-spawned."

  # ── POST: inspect on-disk state ───────────────────────────────────
  echo
  echo "─── POST: on-disk session state ────────────────────────────────"
  local sessions_dir
  sessions_dir="$PEKO_DATA_DIR/principals/local/local/sessions"
  echo "sessions dir: $sessions_dir"
  if [[ -d "$sessions_dir" ]]; then
    echo "JSONL files:"
    ls -la "$sessions_dir"/*.jsonl 2>/dev/null
    echo
    echo "sessions.json:"
    cat "$sessions_dir/sessions.json" 2>/dev/null | head -200
    echo
    echo "peers.json (if any):"
    cat "$sessions_dir/peers.json" 2>/dev/null
  else
    echo "❌ sessions dir missing"
  fi

  # ── POST: round-7 negations — no chapters.json, no '#' ids ───────
  # (The assertion-grade version of these checks lives in
  # probe-no-chapter-suffix.sh; here they gate the flow's exit code.)
  echo
  echo "─── POST: round-7 chapter-concept negations ────────────────────"
  local stray
  stray=$(find "$PEKO_DATA_DIR" -name 'chapters.json' 2>/dev/null | head -1)
  if [[ -n "$stray" ]]; then
    echo "❌ chapters.json found at $stray — chapter concept not fully deleted"
    peko_iso_done 1
    return 1
  fi
  echo "✓ no chapters.json anywhere"
  local hash_names
  hash_names=$(find "$PEKO_DATA_DIR/principals" -path '*/sessions/*' -name '*#*' 2>/dev/null)
  if [[ -n "$hash_names" ]]; then
    echo "❌ '#'-suffixed session entries found:"
    echo "$hash_names" | head -5
    peko_iso_done 1
    return 1
  fi
  echo "✓ no '#' in any session id on disk"

  # ── POST: compact flag persisted for the TURN-19 target ──────────
  echo
  echo "─── POST: compact_requested persisted on a spawned entry ───────"
  local meta flagged
  meta="$sessions_dir/sessions.json"
  if [[ -f "$meta" ]]; then
    flagged=$(jq -r 'to_entries[] | select(.value.compact_requested == true) | .key' "$meta" 2>/dev/null | head -1)
    if [[ -n "$flagged" ]]; then
      echo "✓ compact_requested:true on $flagged (TURN 19 landed)"
    else
      echo "⚠ no entry with compact_requested:true — did TURN 19's compact call succeed?"
    fi
  else
    echo "⚠ sessions.json missing — compact flag check skipped"
  fi

  echo
  echo "─── POST: all trigger stamps observed across JSONL ─────────────"
  for f in $(find "$PEKO_DATA_DIR" -name '*.jsonl' 2>/dev/null); do
    head -1 "$f" 2>/dev/null | grep -oE '"trigger":"[a-z_]+"' | head -1
  done | sort -u

  echo
  echo "─── POST: tool-call count from session JSONLs ─────────────────"
  for f in $(find "$PEKO_DATA_DIR" -name '*.jsonl' 2>/dev/null); do
    n=$(grep -c '"tool_call"' "$f" 2>/dev/null || echo 0)
    base=$(basename "$f")
    echo "  $base: $n tool_call lines"
  done

  peko_iso_done 0
}

# Run if executed directly (vs sourced for testing).
if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  source "$(dirname "$0")/../lib/isolate.sh"
  flow_main "$@"
fi
