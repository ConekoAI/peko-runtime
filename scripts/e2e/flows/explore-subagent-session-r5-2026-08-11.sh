#!/usr/bin/env bash
# scripts/e2e/flows/explore-subagent-session-r5-2026-08-11.sh
#
# Real-LLM multi-turn field test (round 5) focused on agent-owned
# session management + subagent delegation.
#
# Builds on the round-4 followups (F1 fix landed in ec220056, F5
# description anchoring landed in fce2e390). Round-5 probes:
#
#   - The kinds filter — description says 'user' / 'chapter' / 'spawned'
#     / 'branch' / 'cron'; parameters() field at line 163 still says
#     ['main', 'spawned', 'cron']; engine actually stores trigger=
#     "spawn" not "spawned". So `kinds=["spawned"]` should return 0
#     sessions even when spawned ones exist.
#
#   - Lifecycle actions that round-4 didn't deeply exercise:
#     branch + rename + archive + unarchive + delete + compact
#
#   - Subagent session inspection: can the model read history of a
#     spawned session? Search across it? Branch it?
#
#   - The cross-action ownership rule: model can't modify the session
#     it's currently running in (delete/compact refused, archive/
#     unarchive/rename still allowed?)
#
# Acts as Sam, a non-technical pottery studio owner. Same persona
# pattern as round-4 for continuity. Real minimax-MiniMax-M3 via
# $MINIMAX_API_KEY.

flow_main() {
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "❌ MINIMAX_API_KEY env var not set — refusing to run" >&2
    return 64
  fi

  peko_iso_init "explore-subagent-session-r5-2026-08-11" || return 1

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
  _r5_turn() {
    local label="$1"
    shift
    echo
    echo "─── ${label} ───"
    t0=$SECONDS
    peko_iso_run "$@" --no-stream
    dur=$((SECONDS - t0))
    peko_iso_assert_rc_zero
    echo "wall: ${dur}s"
    echo "$_peko_iso_capture_out"
  }

  # ── TURN 1: memory seed ───────────────────────────────────────────
  _r5_turn "TURN 1 (memory seed)" send sam "Remember: my studio is called Clay & Ember, I teach a beginner wheel class every Saturday at 10am, my favorite glaze is celadon, and I'm working on a new tea-bowl line called 'ember-glaze'. Reply with a one-line confirmation."

  # ── TURN 2: session status — get current metadata + token usage ──
  _r5_turn "TURN 2 (status — current session)" send sam "Use the session tool with action=status (no session_key) and tell me the current session's id, kind, and token usage."

  # ── TURN 3: list sessions, no filter ─────────────────────────────
  _r5_turn "TURN 3 (list — no filter)" send sam "Use the session tool with action=list (no filters) and tell me how many sessions exist right now."

  # ── TURN 4: post-kinds-removal — ask for subagent sessions ─────
  # Round-6 F1 fix (2026-08-13): the kinds filter was deleted. The
  # model now derives "is this a subagent session?" from
  # `parent_session_id` on the status result. Ask for it directly.
  _r5_turn "TURN 4 (find subagent sessions — model uses parent_session_id)" send sam "Show me only my subagent sessions (the ones I haven't talked to directly). Use the session tool however you need to."

  # ── TURN 5: post-kinds-removal — ask for chapters ──────────────
  # Round-6 F2 fix: a "chapter" is identified by a `#<timestamp>`
  # filename suffix on session_id, not by a trigger value. The model
  # should call list with include_archived=true and grep for `#`.
  _r5_turn "TURN 5 (find chapters — model uses session_id contains '#')" send sam "Show me any chapters (a chapter is a rotated user session whose id contains '#'). Use the session tool however you need to."

  # ── TURN 6: post-kinds-removal — ask for the live session only ──
  _r5_turn "TURN 6 (find the live session — model filters on parent_session_id=None)" send sam "Show me my live (non-subagent, non-chapter) session. Use the session tool however you need to."

  # ── TURN 7: delegate to subagent to seed a 'spawned' session ────
  _r5_turn "TURN 7 (Agent subagent — should produce a 'spawn' session)" send sam "Delegate this to a helper agent (use the Agent tool with type=primary): write a one-paragraph marketing blurb for an 'ember-glaze' tea-bowl line, suitable for our Instagram. Don't do it yourself."

  # ── TURN 8: list sessions again — should now have a parent_session_id ─
  _r5_turn "TURN 8 (list — expect a spawned entry now)" send sam "Use the session tool with action=list (no filters) and show me the parent_session_id of every session. I want to see if the helper created a new one with a parent."

  # ── TURN 9: history of the spawned session ───────────────────────
  _r5_turn "TURN 9 (history on spawned session)" send sam "From the list above, find the spawned session's key and call action=history on it. Summarise what the helper produced."

  # ── TURN 10: search across transcripts for 'ember-glaze' ────────
  _r5_turn "TURN 10 (search query='ember-glaze')" send sam "Use the session tool with action=search and query='ember-glaze'. Show me the hits."

  # ── TURN 11: branch the current session with a label ─────────────
  _r5_turn "TURN 11 (branch — current session, label='pre-rename')" send sam "Use the session tool with action=branch on the current session, label='pre-rename'. Tell me the new session key."

  # ── TURN 12: rename the current session ──────────────────────────
  _r5_turn "TURN 12 (rename — current session)" send sam "Use the session tool with action=rename on the current session. Title it 'pottery-chat-2026-08-11'. Tell me the new title."

  # ── TURN 13: try to delete the live session — should refuse ──────
  _r5_turn "TURN 13 (delete current — should refuse per #351)" send sam "Use the session tool with action=delete on the current session. Tell me exactly what the tool returns."

  # ── TURN 14: archive the branch we just made ─────────────────────
  _r5_turn "TURN 14 (archive the branch from TURN 11)" send sam "Use the session tool with action=archive on the branch session you created in turn 11. Confirm what happened."

  # ── TURN 15: list with include_archived=true ─────────────────────
  _r5_turn "TURN 15 (list include_archived=true)" send sam "Use the session tool with action=list and include_archived=true. Tell me how many sessions are now in the list vs before."

  # ── TURN 16: unarchive the branch back ───────────────────────────
  _r5_turn "TURN 16 (unarchive the branch)" send sam "Use the session tool with action=unarchive on the same branch session from turn 14. Confirm it came back."

  # ── TURN 17: compact the live session — should refuse (per #351) ─
  _r5_turn "TURN 17 (compact current — should refuse)" send sam "Use the session tool with action=compact on the current session. Tell me exactly what the tool returns."

  # ── TURN 18: start a fresh chapter ───────────────────────────────
  _r5_turn "TURN 18 (session new — start a fresh chapter)" send sam "Let's start a fresh chapter. Use the session tool with action=new (optional title 'cooking-recipes'). Confirm what the tool returned and that the old chapter is archived."

  # ── TURN 19: probe — does the rotation actually take effect? ─────
  _r5_turn "TURN 19 (post-rotation isolation probe)" send sam "Without looking at any earlier context, what's my studio's name and what line of ceramics am I working on? Reply based only on what's in front of you right now."

  # ── TURN 20: list — find the live session after rotation ──────
  _r5_turn "TURN 20 (list — find the live session post-rotation)" send sam "Use the session tool with action=list (no filters). Tell me which session is now the live one (the one with no parent_session_id and no '#' in session_id)."

  # ── TURN 21: history with include_tools=false on a small one ────
  _r5_turn "TURN 21 (history include_tools=false)" send sam "Use the session tool with action=history on the current session with include_tools=false. Show me just the conversation text, no tool calls."

  # ── TURN 22: cleanup — delete the spawned session ────────────────
  _r5_turn "TURN 22 (delete spawned session — recursive=false)" send sam "Find the spawned helper session from turn 7 and use action=delete on it (recursive=false, just the leaf). Confirm it's gone from the next list."

  # ── TURN 23: final list to see end state ─────────────────────────
  _r5_turn "TURN 23 (final list — end state)" send sam "Final check: use the session tool with action=list (no filter) and tell me by parent_session_id which sessions are live, subagent, or chapter."

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
    # Round 7 (2026-08-13) deleted the chapter concept: chapters.json no
    # longer exists and session ids never carry a '#' suffix — oversized
    # JSONLs page in place to <id>.N.jsonl. See probe-no-chapter-suffix.sh.
    echo "in-place pages <id>.N.jsonl (if any):"
    find "$sessions_dir" -name '*.jsonl' 2>/dev/null | grep -E '\.[0-9]+\.jsonl$' | sort
  else
    echo "❌ sessions dir missing"
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
