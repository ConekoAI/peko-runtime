#!/usr/bin/env bash
# scripts/e2e/flows/explore-ux-probes.sh
#
# Probing untested UX surfaces. Goal: surface friction a non-tech user
# would hit when first exploring the CLI after the persona-builder +
# fixes. Touches:
#   - quota surfaces (status / list / --json / --peer)
#   - principal persona surfaces (show / set --help / persona --help)
#   - ext list / ext --help
#   - send --file (attach file-based input)
#   - log windowing
#   - global ergonomics (--version / --help)
#   - system doctor / daemon status
#   - cron list

flow_main() {
  peko_iso_init "explore-ux-probes" || return 1

  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "WARN: MINIMAX_API_KEY not set" >&2
    peko_iso_done 0
    return 0
  fi

  echo "==== Setup: model + principal (real LLM) ===="
  peko_iso_run model add --template minimax --model MiniMax-M3 --key "$MINIMAX_API_KEY"
  peko_iso_assert_rc_zero || peko_iso_done 1
  peko_iso_run principal create probe --model minimax-MiniMax-M3
  peko_iso_assert_rc_zero || peko_iso_done 1
  peko_iso_start_daemon || peko_iso_done 1

  # Make a few real calls so quota should have counts.
  for i in 1 2 3; do
    peko_iso_run send probe "Count to ${i}0 and stop." --no-stream >/dev/null || true
  done

  echo
  echo "--- A. quota surfaces ---"
  echo "A1. quota status probe:"
  peko_iso_run quota status probe
  echo "rc=$_peko_iso_capture_rc"
  echo "$_peko_iso_capture_out"
  echo
  echo "A2. quota list probe (the new alias):"
  peko_iso_run quota list probe
  echo "rc=$_peko_iso_capture_rc"
  echo "$_peko_iso_capture_out"
  echo
  echo "A3. quota status probe --json:"
  peko_iso_run quota status probe --json
  echo "rc=$_peko_iso_capture_rc"
  echo "stdout: [$(echo "$_peko_iso_capture_out" | wc -c) bytes]"
  echo "$_peko_iso_capture_out"
  echo "stderr: [$($_peko_iso_capture_err | wc -c) bytes]"
  echo "$_peko_iso_capture_err"
  echo
  echo "A4. quota status probe --peer:"
  peko_iso_run quota status probe --peer
  echo "rc=$_peko_iso_capture_rc"
  echo "$_peko_iso_capture_out"
  echo
  echo "A5. quota --help:"
  peko_iso_run quota --help
  echo "rc=$_peko_iso_capture_rc"
  echo "$_peko_iso_capture_out"
  echo

  echo "--- B. principal persona surfaces ---"
  echo "B1. does 'principal persona show' exist?"
  peko_iso_run principal persona show probe || true
  echo "rc=$_peko_iso_capture_rc"
  echo "stdout:"
  echo "$_peko_iso_capture_out"
  echo "stderr:"
  echo "$_peko_iso_capture_err"
  echo
  echo "B2. principal persona set --help:"
  peko_iso_run principal persona set --help
  echo "$_peko_iso_capture_out"
  echo
  echo "B3. principal persona --help:"
  peko_iso_run principal persona --help
  echo "rc=$_peko_iso_capture_rc"
  echo "$_peko_iso_capture_out"
  echo
  echo "B4. principal --help:"
  peko_iso_run principal --help
  echo "$_peko_iso_capture_out"
  echo

  echo "--- C. extensions ---"
  echo "C1. ext list:"
  peko_iso_run ext list
  echo "rc=$_peko_iso_capture_rc"
  echo "$_peko_iso_capture_out"
  echo
  echo "C2. ext --help:"
  peko_iso_run ext --help
  echo "$_peko_iso_capture_out"
  echo
  echo "C3. ext --help (subcommands?):"
  peko_iso_run ext list --help
  echo "$_peko_iso_capture_out"
  echo

  echo "--- D. send --file ---"
  echo "D1. send --help:"
  peko_iso_run send --help
  echo "$_peko_iso_capture_out"
  echo
  cat > /tmp/probe-snippet.txt <<'PY'
def foo(items):
    seen = set()
    for x in items:
        if x in seen:
            return False
        seen.add(x)
    return True
PY
  echo "D2. send probe --file /tmp/probe-snippet.txt (real LLM):"
  local t0 t1
  t0=$SECONDS
  peko_iso_run send probe --file /tmp/probe-snippet.txt --no-stream
  t1=$SECONDS
  echo "rc=$_peko_iso_capture_rc  wall=$((t1 - t0))s"
  echo "stdout (first 20 lines):"
  echo "$_peko_iso_capture_out" | head -20
  echo "stderr:"
  echo "$_peko_iso_capture_err"
  echo

  echo "--- E. log windowing ---"
  echo "E1. log probe --since 5m:"
  peko_iso_run log probe --since 5m
  echo "rc=$_peko_iso_capture_rc"
  echo "$_peko_iso_capture_out" | head -40
  echo
  echo "E2. log probe --since 1h --json (paginated):"
  peko_iso_run log probe --since 1h --json
  echo "rc=$_peko_iso_capture_rc"
  echo "$_peko_iso_capture_out" | head -c 400
  echo
  echo

  echo "--- F. global ergonomics ---"
  echo "F1. peko --version:"
  peko_iso_run --version
  echo "$_peko_iso_capture_out"
  echo
  echo "F2. peko --help:"
  peko_iso_run --help
  echo "$_peko_iso_capture_out"
  echo
  echo "F3. system doctor:"
  peko_iso_run system doctor
  echo "rc=$_peko_iso_capture_rc"
  echo "$_peko_iso_capture_out"
  echo
  echo "F4. system doctor --verbose:"
  peko_iso_run system doctor --verbose || true
  echo "rc=$_peko_iso_capture_rc"
  echo "$_peko_iso_capture_out"
  echo
  echo "F5. daemon status:"
  peko_iso_run daemon status
  echo "rc=$_peko_iso_capture_rc"
  echo "$_peko_iso_capture_out"
  echo
  echo "F6. daemon status --json:"
  peko_iso_run daemon status --json
  echo "rc=$_peko_iso_capture_rc"
  echo "$_peko_iso_capture_out"
  echo

  echo "--- G. cron ---"
  echo "G1. cron list:"
  peko_iso_run cron list
  echo "rc=$_peko_iso_capture_rc"
  echo "$_peko_iso_capture_out"
  echo
  echo "G2. cron --help:"
  peko_iso_run cron --help
  echo "$_peko_iso_capture_out"
  echo

  echo "explore-ux-probes flow done"
  peko_iso_done 0
}