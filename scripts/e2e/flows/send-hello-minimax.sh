#!/usr/bin/env bash
# scripts/e2e/flows/send-hello-minimax.sh
#
# End-to-end smoke test against a real MiniMax API:
#   1. Init isolated env (NO_DAEMON — we'll start it after seeding).
#   2. Add the `minimax` template model with the real API key. The key
#      lands in the ISOLATED vault, not the host keychain.
#   3. Create a principal using that model.
#   4. Start the daemon in the background.
#   5. `peko send <principal> "hello world"` — daemon runs the agentic
#      loop, calls MiniMax, returns a response.
#   6. Assert: non-empty response, session JSONL appended, no leak into
#      host `~/.peko`.
#
# Required env:
#   MINIMAX_API_KEY  — the real key. Exported by `peko_iso_init` so the
#                      daemon's resolver bootstrap can use it as a
#                      fallback if the vault lookup fails.

flow_main() {
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "❌ MINIMAX_API_KEY env var not set — refusing to run" >&2
    echo "   export MINIMAX_API_KEY=… before invoking this flow" >&2
    return 64   # EX_USAGE
  fi

  peko_iso_init "send-hello-minimax" || return 1

  # --- step 1: add the minimax template model with the real API key ---
  # Template name is `minimax` (lowercase). Default id becomes
  # `minimax-MiniMax-M3`. The CLI writes both the catalog entry (to the
  # isolated models.toml) AND the credential (to the isolated vault) in
  # this single call.
  peko_iso_run model add \
      --template minimax \
      --model MiniMax-M3 \
      --key "$MINIMAX_API_KEY"
  peko_iso_assert_rc_zero
  peko_iso_assert_contains "Added model 'minimax-MiniMax-M3'"

  # --- step 2: create principal ---
  peko_iso_run principal create hello --model minimax-MiniMax-M3
  peko_iso_assert_rc_zero
  peko_iso_assert_contains "hello"

  # --- step 3: start daemon ---
  peko_iso_start_daemon || return 1

  # --- step 4: send hello world ---
  # Streaming render is the default (and only) mode — the reply
  # prints to stdout as it completes.
  peko_iso_run send hello "hello world"
  peko_iso_assert_rc_zero

  # Assert the response is non-empty AND not just a CLI error message.
  if [[ -z "$_peko_iso_capture_out" ]]; then
    echo "❌ empty response from `peko send`" >&2
    return 1
  fi
  if [[ "$_peko_iso_capture_out" == *"Error"* && "$_peko_iso_capture_out" == *"❌"* ]]; then
    echo "❌ response looks like an error message:" >&2
    echo "$_peko_iso_capture_out" | head -20 >&2
    return 1
  fi

  # --- post-condition: session JSONL landed in the tempdir ---
  #
  # Important quirk: `peko send hello "…"` routes through the daemon's
  # root agent, which is owned by the CLI user (`local` by default),
  # NOT by the target principal. So the session JSONL lands at:
  #
  #   <PEKO_DATA_DIR>/principals/local/local/sessions/root:user:local.jsonl
  #
  # The `hello` principal's own sessions dir remains empty for this
  # one-shot hello-world — it's a model container, not a session owner.
  # See F30 chat-vs-session split for the broader design.
  local sessions_dir="$PEKO_DATA_DIR/principals/local/local/sessions"
  if [[ ! -d "$sessions_dir" ]]; then
    echo "❌ sessions dir missing: $sessions_dir" >&2
    return 1
  fi
  local jsonl_count
  jsonl_count="$(find "$sessions_dir" -name '*.jsonl' 2>/dev/null | wc -l | tr -d ' ')"
  if [[ "$jsonl_count" -lt 1 ]]; then
    echo "❌ no session JSONL written under $sessions_dir" >&2
    return 1
  fi

  # Belt-and-suspenders: confirm the user message + assistant reply
  # actually landed in the JSONL (a stray file from a prior test would
  # otherwise satisfy the count check).
  local session_jsonl
  session_jsonl="$(find "$sessions_dir" -name '*.jsonl' 2>/dev/null | head -1)"
  if ! grep -q '"role":"assistant"' "$session_jsonl" 2>/dev/null; then
    echo "❌ session JSONL has no assistant message" >&2
    return 1
  fi

  # --- post-condition: nothing leaked into host ~/.peko ---
  if [[ -d "$HOME/../.peko" && ! "$HOME" == *peko* ]]; then
    echo "❌ leak: ~/.peko modified by isolated flow" >&2
    return 1
  fi

  # --- print response for human inspection ---
  echo
  echo "───── response ─────"
  echo "$_peko_iso_capture_out"
  echo "─────────────────────"

  echo "✅ flow complete: send-hello-minimax"
  peko_iso_done 0
}
