#!/usr/bin/env bash
# scripts/e2e/flows/explore-bash-baseline.sh
#
# Final isolation probe: does Bash work at all for a fresh principal?
# Run a series of escalating commands to find where the failure is:
#   1. echo hello           — does Bash work at all?
#   2. which curl           — is curl in PATH?
#   3. curl -sS file:///etc/hostname  — does curl work on file://?
#   4. curl -sS https://example.com   — does curl work on https://?
#
# If (1) fails the Bash tool is gated. If (1) works but (4) fails, the
# issue is network egress specifically.

flow_main() {
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "❌ MINIMAX_API_KEY env var not set" >&2
    return 64
  fi

  peko_iso_init "bash-baseline" || return 1

  peko_iso_run model add --template minimax --model MiniMax-M3 \
      --key "$MINIMAX_API_KEY" >/dev/null
  peko_iso_assert_rc_zero

  peko_iso_run principal create probe --model minimax-MiniMax-M3 >/dev/null
  peko_iso_assert_rc_zero

  peko_iso_start_daemon || return 1

  local t0 dur

  echo
  echo "─── 1: echo hello (does Bash run at all?) ─────────────"
  t0=$SECONDS
  peko_iso_run send probe \
      "Use the Bash tool to run: echo HELLO_FROM_BASH. Then tell me what you got back, verbatim." \
      --no-stream
  dur=$((SECONDS - t0))
  echo "rc=$_peko_iso_capture_rc, wall=${dur}s"
  echo "$_peko_iso_capture_out"

  echo
  echo "─── 2: which curl (is curl in PATH?) ──────────────────"
  t0=$SECONDS
  peko_iso_run send probe \
      "Use the Bash tool to run: which curl. Tell me what stdout and stderr said." \
      --no-stream
  dur=$((SECONDS - t0))
  echo "rc=$_peko_iso_capture_rc, wall=${dur}s"
  echo "$_peko_iso_capture_out"

  echo
  echo "─── 3: curl -sS https://example.com ────────────────────"
  t0=$SECONDS
  peko_iso_run send probe \
      "Use the Bash tool to run: curl -sS --max-time 8 https://example.com. Show me the first 500 bytes of stdout (or the full error if any)." \
      --no-stream
  dur=$((SECONDS - t0))
  echo "rc=$_peko_iso_capture_rc, wall=${dur}s"
  echo "$_peko_iso_capture_out"

  echo
  echo "─── 4: curl -sS https://api.allorigins.win/raw?url=… ───"
  t0=$SECONDS
  peko_iso_run send probe \
      "Use the Bash tool to run: curl -sS --max-time 8 'https://api.allorigins.win/raw?url=https%3A%2F%2Fen.wikipedia.org%2Fwiki%2FLisbon'. Show me the first 500 bytes of stdout, or the exact error." \
      --no-stream
  dur=$((SECONDS - t0))
  echo "rc=$_peko_iso_capture_rc, wall=${dur}s"
  echo "$_peko_iso_capture_out"

  peko_iso_done 0
}
