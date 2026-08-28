#!/usr/bin/env bash
# scripts/e2e/flows/stop-during-run.sh
#
# Exercises `peko stop` against a live, in-flight run — fully offline,
# no real LLM key needed. A stdlib-Python mock LLM (started via
# peko_iso_start_mock_llm, no pip deps) streams its reply slowly
# (0.6s/word), which keeps the agentic run in flight long enough for
# `peko stop` to land mid-stream.
#
#   1. Init isolated env; start the slow mock LLM on 127.0.0.1.
#   2. Seed the mock-llm provider + principal; start the daemon.
#   3. `peko send` in the background (blocks while the mock streams).
#   4. `peko stop <principal>` → assert the "Stopped run" notice.
#   5. `peko log <principal>` → assert the `⏹ stopped by user` marker.
#   6. Second `peko stop` on the now-idle thread → friendly
#      "No running turn" notice, exit 0 (idempotence).
#
# Prerequisite: python3 on PATH (stdlib only).

flow_main() {
  peko_iso_init "stop-during-run" || return 1

  # --- slow mock LLM: ~24 words × 0.6s keeps the run in flight ~14s ---
  local mock_port
  mock_port="$(peko_iso_start_mock_llm \
      "this mock reply trickles out word by word slowly enough that the run stays in flight and peko stop can cancel it mid stream" \
      0.6)" || return 1

  # --- seed mock-llm provider + principal (same pattern as
  #     daemon-lifecycle.sh) ---
  peko_iso_run model add \
      --custom \
      --id mock-llm \
      --model "${MOCK_LLM_WIRE_ID:-mock-llm-test}" \
      --base-url "http://127.0.0.1:${mock_port}/v1" \
      --api-format openai_completions \
      --key "${MOCK_LLM_API_KEY:-mock-llm-test-key}"
  peko_iso_assert_rc_zero

  peko_iso_run principal create slow-poke --model mock-llm
  peko_iso_assert_rc_zero

  peko_iso_start_daemon || return 1

  # --- background send: blocks until the run finishes or is stopped ---
  # Background the binary DIRECTLY, not the `peko_iso` function — a
  # backgrounded function forks a subshell, so `$!` would be the
  # wrapper's PID, and killing it would orphan the real peko process.
  local send_out="$_PEKO_ISO_TEMPDIR/send.out" send_err="$_PEKO_ISO_TEMPDIR/send.err"
  "$_PEKO_ISO_BIN" send slow-poke "tell me a long story" >"$send_out" 2>"$send_err" &
  local send_pid=$!
  _PEKO_ISO_EXTRA_PIDS+=("$send_pid")

  # Give the daemon a moment to assemble context and open the model
  # stream; the mock then holds the run in flight for ~14s.
  sleep 3

  # --- stop the in-flight run ---
  peko_iso_run stop slow-poke
  peko_iso_assert_rc_zero
  peko_iso_assert_contains "Stopped run on thread 'owner' with principal 'slow-poke'"

  # --- wait for the background send to unwind (bounded) ---
  # Whether the cancel drops the HTTP stream immediately or the loop
  # exits at the next iteration boundary, the send must return well
  # within this window.
  local deadline=$((SECONDS + 45))
  while kill -0 "$send_pid" 2>/dev/null && (( SECONDS < deadline )); do
    sleep 0.5
  done
  wait "$send_pid" 2>/dev/null || true
  if kill -0 "$send_pid" 2>/dev/null; then
    echo "❌ background send did not return within 45s of stop" >&2
    echo "--- send stdout ---" >&2; cat "$send_out" >&2
    echo "--- send stderr ---" >&2; cat "$send_err" >&2
    return 1
  fi

  # --- the thread log shows the stop marker ---
  deadline=$((SECONDS + 30))
  local marker_seen=""
  while (( SECONDS < deadline )); do
    peko_iso_run log slow-poke
    if [[ "$_peko_iso_capture_out" == *"⏹ stopped by user"* ]]; then
      marker_seen=1
      break
    fi
    sleep 1
  done
  if [[ -z "$marker_seen" ]]; then
    echo "❌ log never showed the ⏹ stopped by user marker" >&2
    echo "   last log output: $_peko_iso_capture_out" >&2
    return 1
  fi

  # --- idempotence: stop on the now-idle thread is a friendly no-op ---
  peko_iso_run stop slow-poke
  peko_iso_assert_rc_zero
  peko_iso_assert_contains "No running turn on thread 'owner' with principal 'slow-poke'"

  echo "✅ flow complete: stop-during-run"
  peko_iso_done 0
}
