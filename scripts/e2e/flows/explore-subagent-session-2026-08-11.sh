#!/usr/bin/env bash
# scripts/e2e/flows/explore-subagent-session-2026-08-11.sh
#
# Real-LLM multi-turn field test focused on the agent-owned session
# management work in #351 + the F1 starter-grant fix.
#
# Acts as a non-technical user ("Sam", small pottery studio owner) who
# wants to:
#   - chat for several turns to build context
#   - ask the assistant to "start a fresh conversation" (session new)
#   - delegate a task to a subagent (Agent tool)
#   - inspect what sessions exist (session list)
#   - try to delete a spawned subagent session (should work)
#   - try to delete the currently-running session (should be refused)
#
# Captures per turn:
#   - wall time
#   - stderr token telemetry ("[peko] iterations=N input=N output=N")
#   - response text
#   - on-disk session JSONL count and trigger stamps
#   - whether the model actually called the session tool or claimed
#     it didn't exist (the F1 smoke check)

flow_main() {
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "❌ MINIMAX_API_KEY env var not set — refusing to run" >&2
    return 64
  fi

  peko_iso_init "explore-subagent-session-2026-08-11" || return 1

  # ── seed model + principal + persona ───────────────────────────────
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

  local t0 dur sessions_dir

  # ── TURN 1: seed context ───────────────────────────────────────────
  echo
  echo "─── TURN 1 (memory seed) ─────────────────────────────────────"
  t0=$SECONDS
  peko_iso_run send sam "Remember: my studio is called Clay & Ember, I teach a beginner wheel class every Saturday at 10am, and my favorite glaze is celadon. Reply with a one-line confirmation." --no-stream
  dur=$((SECONDS - t0))
  peko_iso_assert_rc_zero
  echo "wall time: ${dur}s"
  echo "$_peko_iso_capture_out"
  echo

  # ── TURN 2: ask the model to list sessions ─────────────────────────
  # This is the F1 smoke check — after the fix, the model should call
  # the `session` tool with action=list. Pre-fix it would have said
  # "I don't have a session tool".
  echo "─── TURN 2 (session list — F1 smoke check) ────────────────────"
  t0=$SECONDS
  peko_iso_run send sam "Can you see what sessions exist right now? Use the session tool to list them and tell me what you see." --no-stream
  dur=$((SECONDS - t0))
  peko_iso_assert_rc_zero
  echo "wall time: ${dur}s"
  echo "$_peko_iso_capture_out"
  echo

  # ── TURN 3: ask for session history ───────────────────────────────
  echo "─── TURN 3 (session history on current) ───────────────────────"
  t0=$SECONDS
  peko_iso_run send sam "Show me the history of this current session — what we have said so far." --no-stream
  dur=$((SECONDS - t0))
  peko_iso_assert_rc_zero
  echo "wall time: ${dur}s"
  echo "$_peko_iso_capture_out"
  echo

  # ── TURN 4: rotate to a fresh chapter (session new) ───────────────
  echo "─── TURN 4 (start a fresh conversation — session new) ──────────"
  t0=$SECONDS
  peko_iso_run send sam "Let's start a fresh chapter in our conversation. I want to ask you something unrelated — use the session tool with action=new to rotate this conversation into a new chapter so the old topic doesn't clutter the new one." --no-stream
  dur=$((SECONDS - t0))
  peko_iso_assert_rc_zero
  echo "wall time: ${dur}s"
  echo "$_peko_iso_capture_out"
  echo

  # ── TURN 5: prove the new chapter is clean ────────────────────────
  # Per #351 description: "new" rotates the live id at the next run
  # start, so the model should NOT remember "Clay & Ember" in this
  # turn. (It might re-read it from the in-agent memory store, but the
  # session JSONL won't have it.)
  echo "─── TURN 5 (chapter isolation probe) ──────────────────────────"
  t0=$SECONDS
  peko_iso_run send sam "Without looking at any earlier context, what's my studio's name? Reply based only on what's in front of you right now." --no-stream
  dur=$((SECONDS - t0))
  peko_iso_assert_rc_zero
  echo "wall time: ${dur}s"
  echo "$_peko_iso_capture_out"
  echo

  # ── TURN 6: delegate to a subagent ────────────────────────────────
  echo "─── TURN 6 (Agent subagent delegation) ────────────────────────"
  t0=$SECONDS
  peko_iso_run send sam "Delegate this to a helper agent: write a one-paragraph marketing blurb for a Saturday wheel class, suitable for posting on Instagram. Don't do it yourself." --no-stream
  dur=$((SECONDS - t0))
  peko_iso_assert_rc_zero
  echo "wall time: ${dur}s"
  echo "$_peko_iso_capture_out"
  echo

  # ── TURN 7: list sessions — expect spawned kind ───────────────────
  echo "─── TURN 7 (session list — looking for spawned kind) ──────────"
  t0=$SECONDS
  peko_iso_run send sam "List ALL of your sessions now, including spawned ones from the helper. Show me the kinds." --no-stream
  dur=$((SECONDS - t0))
  peko_iso_assert_rc_zero
  echo "wall time: ${dur}s"
  echo "$_peko_iso_capture_out"
  echo

  # ── TURN 8: try to delete the running session — should refuse ────
  echo "─── TURN 8 (delete current — should refuse per #351 ownership) "
  t0=$SECONDS
  peko_iso_run send sam "Use the session tool with action=delete to delete this current session. Show me what the tool returns." --no-stream
  dur=$((SECONDS - t0))
  peko_iso_assert_rc_zero
  echo "wall time: ${dur}s"
  echo "$_peko_iso_capture_out"
  echo

  # ── TURN 9: compact (non-destructive safety check) ───────────────
  echo "─── TURN 9 (compact current session — fire-and-forget flag) ───"
  t0=$SECONDS
  peko_iso_run send sam "Use the session tool with action=compact to schedule a compaction of this current session. Don't worry about the result, just call it." --no-stream
  dur=$((SECONDS - t0))
  peko_iso_assert_rc_zero
  echo "wall time: ${dur}s"
  echo "$_peko_iso_capture_out"
  echo

  # ── TURN 10: subagent cleanup via the Agent tool ─────────────────
  echo "─── TURN 10 (subagent session should be re-routable via list) ─"
  t0=$SECONDS
  peko_iso_run send sam "Show me a one-line summary of what the helper agent produced in the previous turn, and confirm what session id it ran in." --no-stream
  dur=$((SECONDS - t0))
  peko_iso_assert_rc_zero
  echo "wall time: ${dur}s"
  echo "$_peko_iso_capture_out"
  echo

  # ── POST: inspect on-disk state ───────────────────────────────────
  echo
  echo "─── POST: on-disk session state ────────────────────────────────"
  sessions_dir="$PEKO_DATA_DIR/principals/local/local/sessions"
  echo "sessions dir: $sessions_dir"
  if [[ -d "$sessions_dir" ]]; then
    echo "JSONL files:"
    ls -la "$sessions_dir"/*.jsonl 2>/dev/null
    echo
    echo "session metadata:"
    cat "$sessions_dir/sessions.json" 2>/dev/null | head -100
    echo
    echo "chapters file (if any):"
    cat "$sessions_dir/chapters.json" 2>/dev/null
  else
    echo "❌ sessions dir missing"
  fi

  echo
  echo "─── POST: spawned-sessions subdir ──────────────────────────────"
  # Subagent sessions may live elsewhere — check the principal dir
  find "$PEKO_DATA_DIR" -type d -name 'spawn*' 2>/dev/null
  echo
  echo "JSONL count by directory:"
  find "$PEKO_DATA_DIR" -name '*.jsonl' 2>/dev/null | xargs -I {} dirname {} | sort -u | while read d; do
    n=$(find "$d" -maxdepth 1 -name '*.jsonl' 2>/dev/null | wc -l | tr -d ' ')
    echo "  $d: $n jsonl files"
  done

  echo
  echo "─── POST: trigger stamping on JSONL headers ───────────────────"
  for f in $(find "$PEKO_DATA_DIR" -name '*.jsonl' 2>/dev/null); do
    head -1 "$f" 2>/dev/null | grep -oE '"trigger":"[a-z_]+"' | head -1
  done | sort -u

  peko_iso_done 0
}