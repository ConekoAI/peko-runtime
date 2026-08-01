#!/usr/bin/env bash
# scripts/e2e/flows/explore-coding-helper.sh
#
# Non-technical-user exploratory flow. Acts like someone with no
# peko background who wants to spin up a "coding helper" principal
# and use it to do real work via `peko send`.
#
# Steps:
#   1. Seed the minimax model (real LLM key in env).
#   2. Create a blank principal.
#   3. Draft a persona ("a Python helper for small CLI utilities").
#   4. Ask the principal to write a small Python script (real LLM).
#   5. Ask the principal to write tests for that script (real LLM).
#   6. Ask the principal to refactor the script to use pathlib (real LLM).
#   7. Pull up the chat log with --json, verify pagination works.
#
# Goal: surface friction a non-technical user would hit. Things to
# watch for:
#   - Empty/whitespace input cost
#   - Verbose / unhelpful error messages
#   - Cost / token visibility
#   - Latency surprises
#   - Discoverability of the next command

flow_main() {
  peko_iso_init "explore-coding-helper" || return 1

  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "⚠️  MINIMAX_API_KEY not set" >&2
    peko_iso_done 0
    return 0
  fi

  echo "════ Real coding task: write + test + refactor a Python CLI ════"

  # ---- 1. Seed model ----
  peko_iso_run model add --template minimax --model MiniMax-M3 --key "$MINIMAX_API_KEY"
  peko_iso_assert_rc_zero || peko_iso_done 1
  echo

  # ---- 2. Create blank principal ----
  peko_iso_run principal create pyhelper --model minimax-MiniMax-M3
  peko_iso_assert_rc_zero || peko_iso_done 1

  # ---- 3. Draft a persona (the moment a non-tech user stays or churns) ----
  peko_iso_start_daemon || peko_iso_done 1

  echo "─── Step 3: persona set --from (default = write) ───"
  local t0 t1
  t0=$SECONDS
  peko_iso_run principal persona set pyhelper \
    --from "a python helper that writes small CLI utilities, prefers stdlib, writes clean idiomatic code with type hints"
  peko_iso_assert_rc_zero || peko_iso_done 1
  t1=$SECONDS
  echo "persona_draft wall time: $((t1 - t0)) s"
  echo

  # ---- 4. Real coding task: write a small Python CLI ----
  echo "─── Step 4: ask for a small Python CLI utility (--no-stream) ───"
  local prompt1
  prompt1="Write a tiny Python 3.10+ CLI script named wc_tool.py that emulates a subset of `wc`: --lines, --words, --bytes, --chars, and --help. Read from a file path argument or stdin. Include a main() and an if __name__ == '__main__' guard. Use only stdlib. Add type hints. Print the result like GNU wc (lines\\twords\\tbytes filename). Reply with ONLY the script body, no commentary."
  t0=$SECONDS
  peko_iso_run send pyhelper "$prompt1" --no-stream
  peko_iso_assert_rc_zero || peko_iso_done 1
  t1=$SECONDS
  echo "first task wall time: $((t1 - t0)) s"
  echo "─── reply (first 30 lines) ───"
  echo "$_peko_iso_capture_out" | head -30
  echo "─── reply (last 10 lines) ───"
  echo "$_peko_iso_capture_out" | tail -10
  echo "reply length: $(echo "$_peko_iso_capture_out" | wc -l) lines, $(echo "$_peko_iso_capture_out" | wc -c) bytes"
  echo

  # ---- 5. Follow-up: write tests ----
  echo "─── Step 5: follow-up — write tests for it ───"
  local prompt2
  prompt2="Now write test_wc_tool.py using unittest, covering: --lines, --words, --bytes, --chars, stdin mode, and missing-file behavior. Use only stdlib. Reply with ONLY the test file body."
  t0=$SECONDS
  peko_iso_run send pyhelper "$prompt2" --no-stream
  peko_iso_assert_rc_zero || peko_iso_done 1
  t1=$SECONDS
  echo "follow-up wall time: $((t1 - t0)) s"
  echo "─── reply (first 30 lines) ───"
  echo "$_peko_iso_capture_out" | head -30
  echo

  # ---- 6. Refactor ----
  echo "─── Step 6: refactor request — pathlib only ───"
  local prompt3
  prompt3="Refactor wc_tool.py to read input files using pathlib.Path instead of open(). Reply with ONLY the refactored script."
  t0=$SECONDS
  peko_iso_run send pyhelper "$prompt3" --no-stream
  peko_iso_assert_rc_zero || peko_iso_done 1
  t1=$SECONDS
  echo "refactor wall time: $((t1 - t0)) s"
  echo "─── reply (first 30 lines) ───"
  echo "$_peko_iso_capture_out" | head -30
  echo

  # ---- 7. Read the log back via JSON ----
  echo "─── Step 7: log --since 1h --json ───"
  peko_iso_run log pyhelper --since 1h --json
  peko_iso_assert_rc_zero || peko_iso_done 1
  echo "log JSON shape (first 200 chars):"
  echo "$_peko_iso_capture_out" | head -c 200
  echo "..."
  echo

  # ---- 8. Show the principal back ----
  echo "─── Step 8: principal show pyhelper --json (verify drafted persona fields are present) ───"
  peko_iso_run principal show pyhelper --json
  peko_iso_assert_rc_zero || peko_iso_done 1
  echo "$_peko_iso_capture_out" | head -30
  echo

  # ---- 9. quota check ----
  echo "─── Step 9: quota status pyhelper ───"
  peko_iso_run quota status pyhelper
  peko_iso_assert_rc_zero || peko_iso_done 1
  echo "$_peko_iso_capture_out"
  echo

  echo "🎉 explore-coding-helper flow done"
  peko_iso_done 0
}