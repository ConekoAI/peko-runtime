#!/usr/bin/env bash
# scripts/e2e/flows/log-watch.sh
#
# Exercises `peko log <principal> --watch` — fully offline, no real LLM
# key needed. A stdlib-Python mock LLM (peko_iso_start_mock_llm) replies
# instantly with a fixed keyword, so the exchange is deterministic.
#
#   1. Init isolated env; start the fast mock LLM on 127.0.0.1.
#   2. Seed the mock-llm provider + principal; start the daemon.
#   3. Send a first message — `log --watch` refuses to attach before a
#      conversation thread exists.
#   4. Background `peko log <principal> --watch` writing to a file.
#   5. `peko send` a second message; mock replies fast.
#   6. Assert the watch output file shows BOTH the user message and the
#      assistant reply (live tail, not just replay).
#   7. Kill the watcher; cleanup runs on all exits (the watcher PID is
#      registered in _PEKO_ISO_EXTRA_PIDS, killed by peko_iso_done).
#
# Prerequisite: python3 on PATH (stdlib only).

flow_main() {
  peko_iso_init "log-watch" || return 1

  # --- fast mock LLM: fixed reply keyword ---
  local mock_port
  mock_port="$(peko_iso_start_mock_llm "WATCH_PONG from the mock model" 0.01)" || return 1

  # --- seed mock-llm provider + principal ---
  peko_iso_run model add \
      --custom \
      --id mock-llm \
      --model "${MOCK_LLM_WIRE_ID:-mock-llm-test}" \
      --base-url "http://127.0.0.1:${mock_port}/v1" \
      --api-format openai_completions \
      --key "${MOCK_LLM_API_KEY:-mock-llm-test-key}"
  peko_iso_assert_rc_zero

  peko_iso_run principal create watch-me --model mock-llm
  peko_iso_assert_rc_zero

  peko_iso_start_daemon || return 1

  # --- seed the thread: `log --watch` refuses to attach before any
  #     conversation exists ("no conversation thread … yet"), so a
  #     first exchange must land before the watcher starts ---
  peko_iso_run send watch-me "seed the thread"
  peko_iso_assert_rc_zero

  # --- background `peko log --watch`, writing to a file ---
  # Note: background the binary DIRECTLY, not the `peko_iso` function —
  # a backgrounded function forks a subshell, so `$!` would be the
  # wrapper's PID and `kill $!` would orphan the real peko process.
  local watch_out="$_PEKO_ISO_TEMPDIR/watch.out" watch_err="$_PEKO_ISO_TEMPDIR/watch.err"
  "$_PEKO_ISO_BIN" log watch-me --watch >"$watch_out" 2>"$watch_err" &
  local watch_pid=$!
  _PEKO_ISO_EXTRA_PIDS+=("$watch_pid")

  # Let the watch stream connect (replay phase completes, live tail
  # begins) before we post anything.
  sleep 2

  # --- send a second message; mock replies fast ---
  peko_iso_run send watch-me "ping the watch flow"
  peko_iso_assert_rc_zero

  # --- poll the watch output for the exchange (generous timeout) ---
  local deadline=$((SECONDS + 30)) seen=""
  while (( SECONDS < deadline )); do
    if grep -q "ping the watch flow" "$watch_out" 2>/dev/null \
       && grep -q "WATCH_PONG" "$watch_out" 2>/dev/null; then
      seen=1
      break
    fi
    sleep 1
  done

  # --- kill the watcher explicitly (peko_iso_done would too, via
  #     _PEKO_ISO_EXTRA_PIDS, but be tidy on the success path) ---
  kill "$watch_pid" 2>/dev/null || true
  wait "$watch_pid" 2>/dev/null || true

  if [[ -z "$seen" ]]; then
    echo "❌ watch output missing the exchange" >&2
    echo "--- watch stdout ---" >&2; cat "$watch_out" >&2
    echo "--- watch stderr ---" >&2; cat "$watch_err" >&2
    return 1
  fi

  echo "--- watch output ---"
  cat "$watch_out"
  echo "--------------------"

  echo "✅ flow complete: log-watch"
  peko_iso_done 0
}
