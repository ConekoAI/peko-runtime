#!/usr/bin/env bash
# scripts/e2e/flows/explore-subagent-session-r6-2026-08-12.sh
#
# Real-LLM multi-turn field test (round 6) focused on agent-owned
# session management + subagent delegation — DIFFERENT ANGLES than r5.
#
# Build: master @ c976d510 (after PR #353 implicit-session-management
# merge). r5 (2026-08-11) confirmed F1 (kinds filter broken on 3 axes),
# F2 (chapter is rename-with-suffix, no Chapter trigger), F3
# (peers.json drops old chapter), F4 (no `peko session` CLI). r6 probes:
#
#   - Regression: r5 F1 still broken? (description says "spawned",
#     engine stores "spawn" — Agent tool description reinforces this
#     lie by saying "Sessions you spawn appear in the `session` tool as
#     kind 'spawned'".)
#
#   - The session tool surface was reduced from 12 actions to 6
#     (status/list/history/search/rename/delete). The description now
#     says "Lifecycle operations (chapter rotation, compaction, archive,
#     branch) are NOT exposed here — the engine drives them
#     automatically." So the model can no longer rotate chapters
#     manually. Can it notice, and what does it tell the user?
#
#   - Multi-subagent delegation: ask the model to spawn THREE helpers
#     in one turn. How does it structure the calls (parallel or
#     sequential)? Does the depth/concurrency surface show? Does
#     kinds=["spawned"] filter catch all three?
#
#   - Agent resume_session: spawn a helper with persistent identity,
#     ask the model to come back to it later and continue. Does the
#     model remember the helper's session_id across turns? Does the
#     resume actually work end-to-end?
#
#   - Cross-tool surface drift: model asks `list kinds=["spawned"]`,
#     gets 0 results, even though it spawned helpers a few turns ago.
#     How does the model handle this? Does it switch to an unfiltered
#     list? Does it tell the user about the inconsistency?
#
#   - Engine-driven chapter rotation: ask the model to "start a fresh
#     chapter" / "forget the old context" / "begin a new conversation".
#     Round-5 used session action=new. Now that's gone. Does the
#     engine actually rotate on any signal? Does the model tell the
#     user it can't manually rotate anymore?
#
# Persona: Same as r5 — Sam, Clay & Ember pottery studio.

flow_main() {
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "❌ MINIMAX_API_KEY env var not set — refusing to run" >&2
    return 64
  fi

  peko_iso_init "explore-subagent-session-r6-2026-08-12" || return 1

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
  _r6_turn() {
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
  _r6_turn "TURN 1 (memory seed)" send sam "Remember: my studio is called Clay & Ember, I teach a beginner wheel class every Saturday at 10am, my favorite glaze is celadon, and I'm working on a new tea-bowl line called 'ember-glaze'. Reply with a one-line confirmation."

  # ── TURN 2: probe the surface — what actions does session have now? ─
  _r6_turn "TURN 2 (probe session tool surface)" send sam "Use the session tool with action=status on the current session. Then read the tool's description and list every action it accepts. Tell me what you find."

  # ── TURN 3: list sessions, no filter ─────────────────────────────
  _r6_turn "TURN 3 (list — no filter, baseline)" send sam "Use the session tool with action=list (no filters) and tell me how many sessions exist."

  # ── TURN 4: post-kinds-removal — ask for the live session ────
  # Round-6 F1 fix (2026-08-13): the kinds filter was deleted. The
  # model now derives "live session" from parent_session_id is None
  # and session_id doesn't contain '#'. Ask directly.
  _r6_turn "TURN 4 (find the live session — post-F1-fix)" send sam "Show me my live user session (the one I just started talking to). Use the session tool however you need to."

  # ── TURN 5: ask the model to start fresh — observe the missing action ─
  _r6_turn "TURN 5 (start fresh chapter — observe surface)" send sam "I want to start a fresh conversation and forget the current one. How would you do that with the session tool? Don't actually do anything yet, just tell me what action you'd call."

  # ── TURN 6: delegate to a persistent helper subagent ─────────────
  _r6_turn "TURN 6 (Agent subagent — persistent worker)" send sam "Delegate this to a helper agent (use the Agent tool, subagent_type=primary, cleanup=keep): write a one-paragraph marketing blurb for an 'ember-glaze' tea-bowl line, suitable for our Instagram. Don't do it yourself. Tell me the helper's session id."

  # ── TURN 7: multi-subagent delegation — 3 helpers in parallel ────
  _r6_turn "TURN 7 (3 parallel helpers)" send sam "Delegate three small tasks in parallel to helper agents (subagent_type=primary, cleanup=keep): (a) one helper writes 3 social-media hashtags for the 'ember-glaze' line, (b) one writes a 1-sentence tagline for our Saturday beginner class, (c) one writes 1 line of poetry about celadon glaze. Use multiple Agent tool calls in the same turn if you can. Tell me what you got from each helper."

  # ── TURN 8: post-F1-fix — find subagent sessions ──────────────
  # Round-6 F1 fix (2026-08-13): the kinds filter was deleted. The
  # model now derives "spawned" from parent_session_id is not None.
  _r6_turn "TURN 8 (find subagent sessions — post-F1-fix)" send sam "Show me all the helper sessions you spawned this conversation. Use the session tool however you need to. I expect at least 4 (one from TURN 6, three from TURN 7)."

  # ── TURN 9: list without filter — see parent_session_id for each ─
  _r6_turn "TURN 9 (list no filter — see parent_session_id)" send sam "Now use the session tool with action=list (no filters). For each session, tell me its session_id and parent_session (you can get parent_session via action=status on each id)."

  # ── TURN 10: resume a spawned session via Agent ───────────────────
  _r6_turn "TURN 10 (resume_session on helper from TURN 6)" send sam "Find the helper session you spawned in turn 6 (the marketing blurb one). Use the Agent tool with subagent_type=primary and resume_session=<that id> to ask the helper: 'Can you rewrite the blurb to also mention our Saturday beginner class?' Tell me what the helper returned."

  # ── TURN 11: history on the resumed helper — verify history grew ──
  _r6_turn "TURN 11 (history on resumed helper)" send sam "Use the session tool with action=history on the same helper session id. How many messages are in it now? Did the resume add a new turn?"

  # ── TURN 12: delete a spawned helper — should work ───────────────
  _r6_turn "TURN 12 (delete a helper from TURN 7)" send sam "Pick any one helper session from turn 7 (hashtags, tagline, or poetry) and use the session tool with action=delete on it. Then confirm with another action=list that it's gone."

  # ── TURN 13: rename the live session — should refuse (per #351) ──
  _r6_turn "TURN 13 (rename current session — should refuse)" send sam "Use the session tool with action=rename on the current session. Title it 'r6-test-2026-08-12'. Tell me exactly what the tool returned."

  # ── TURN 14: delete current session — should refuse ──────────────
  _r6_turn "TURN 14 (delete current — should refuse)" send sam "Use the session tool with action=delete on the current session. Tell me exactly what the tool returns."

  # ── TURN 15: search across all sessions ──────────────────────────
  _r6_turn "TURN 15 (search query='ember-glaze')" send sam "Use the session tool with action=search and query='ember-glaze'. Show me the hits."

  # ── TURN 16: post-F2-fix — find chapters by session_id suffix ───
  # Round-6 F2 fix (2026-08-13): a "chapter" is identified by a
  # `#<timestamp>` filename suffix on session_id, not by a kind value.
  _r6_turn "TURN 16 (find chapters — post-F2-fix)" send sam "Show me any chapters (a chapter is a rotated user session whose id contains '#'). Use the session tool however you need to — likely include_archived=true."

  # ── TURN 17: probe — what happens if I ask to forget everything ──
  _r6_turn "TURN 17 (model workaround probe for chapter rotation)" send sam "I've been talking to you for a while and I want to start completely fresh — like a brand-new chat that doesn't remember this conversation. Walk me through what you'd actually DO step by step. Don't take any action yet."

  # ── TURN 18: probe Agent's depth/concurrency surface ─────────────
  _r6_turn "TURN 18 (probe depth/concurrency in description)" send sam "Read the Agent tool's description carefully and tell me: what is the maximum spawn depth allowed, and how many concurrent subagents are allowed? Quote the exact phrases from the description."

  # ── TURN 19: status of current + a spawned session ────────────────
  _r6_turn "TURN 19 (status — current vs spawned)" send sam "Use the session tool with action=status on (a) the current session, and (b) any one helper session from turn 7 that you can find. Compare: id, parent_session, message count, last activity."

  # ── TURN 20: final list to see end state ─────────────────────────
  _r6_turn "TURN 20 (final list — end state)" send sam "Final check: use the session tool with action=list (no filter, include_archived=true). Tell me by parent_session_id which sessions are live, subagent, or chapter."

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
    echo
    echo "peers.json (if any):"
    cat "$sessions_dir/peers.json" 2>/dev/null
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