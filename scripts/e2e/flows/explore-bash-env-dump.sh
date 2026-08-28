#!/usr/bin/env bash
# scripts/e2e/flows/explore-bash-env-dump.sh
#
# Why does Bash fail with "Failed to execute Bash command" even for
# "echo HELLO_FROM_BASH"? Probe what the Bash tool's tokio Command
# can / can't see.
#
# Strategy: have the model write the result of `id; pwd; env | head;
# ls /tmp; which sh` to a file inside the principal workspace, then
# Read that file back from outside the conversation.

flow_main() {
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "❌ MINIMAX_API_KEY env var not set" >&2
    return 64
  fi

  peko_iso_init "bash-env-dump" || return 1

  peko_iso_run model add --template minimax --model MiniMax-M3 \
      --key "$MINIMAX_API_KEY" >/dev/null
  peko_iso_assert_rc_zero

  peko_iso_run principal create probe --model minimax-MiniMax-M3 >/dev/null
  peko_iso_assert_rc_zero

  peko_iso_start_daemon || return 1

  local probe_home="$PEKO_HOME/principals/probe"
  echo
  echo "─── A: dump bash env into a file in the workspace ─────"
  peko_iso_run send probe \
      "Use the Bash tool to write to the file '${probe_home}/bash_env.txt' the following output (one per line, no commentary): the string 'BEGIN_DUMP'; then the output of: id; pwd; which sh; echo PATH=\$PATH; ls -la /tmp 2>&1 | head -5; echo END_DUMP. Then in 2 lines tell me whether the file got written."
  echo "rc=$_peko_iso_capture_rc"
  echo "$_peko_iso_capture_out"

  echo
  echo "─── B: read that file via Read tool ───────────────────"
  peko_iso_run send probe \
      "Use the Read tool to read the file '${probe_home}/bash_env.txt' and show me the full contents."
  echo "rc=$_peko_iso_capture_rc"
  echo "$_peko_iso_capture_out"

  echo
  echo "─── C: bypass Bash — use Write tool directly ───────────"
  peko_iso_run send probe \
      "Use the Write tool to create the file '${probe_home}/from_write.txt' with the content 'written_via_Write_tool\n'. Then Read it back to confirm."
  echo "rc=$_peko_iso_capture_rc"
  echo "$_peko_iso_capture_out"

  echo
  echo "─── file system ground truth ──────────────────────────"
  ls -la "${probe_home}/" 2>&1 | head -10
  echo
  if [[ -f "${probe_home}/bash_env.txt" ]]; then
    echo "── bash_env.txt contents ──"
    cat "${probe_home}/bash_env.txt"
  else
    echo "── bash_env.txt: NOT WRITTEN ──"
  fi

  peko_iso_done 0
}
