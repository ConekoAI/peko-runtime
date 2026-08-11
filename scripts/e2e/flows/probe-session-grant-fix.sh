#!/usr/bin/env bash
# scripts/e2e/flows/probe-session-grant-fix.sh
#
# Follow-up to probe-session-grant.sh: does an explicit
# `peko capability grant --principal <name> tool:session` bring the
# session tool back into the model's toolset?
#
# Separate flow because peko_iso_done is wired to the EXIT trap and
# can't be re-entered inside a single flow.

flow_main() {
  peko_iso_init "probe-session-grant-fix" || return 1

  peko_iso_run model add --template anthropic --model claude-sonnet-4-5 --key dummy 2>&1 | tail -1
  peko_iso_assert_rc_zero

  peko_iso_run principal create probe --model anthropic-claude-sonnet-4-5 2>&1 | tail -1
  peko_iso_assert_rc_zero

  peko_iso_start_daemon || return 1

  peko_iso_run capability grant --principal probe tool:session
  peko_iso_assert_rc_zero

  echo "──── principal grants after manual grant ────"
  grep -oE 'tool:[A-Za-z_]+' "$PEKO_HOME/principals/probe/principal.toml" | sort -u | tr '\n' ' '
  echo

  # Restart daemon so it re-reads the principal's TOML (no runtime reload).
  # We do this by killing it from peko_iso_done, then re-init.
  echo "──── peko_iso_done + restart for fresh daemon read ────"
  peko_iso_done 0
}

# Wrap peko_iso_done restart in a separate flow: this script's flow_main
# does the half that needs the daemon (the grant). Then we call the
# second half inline using a fresh isolation tempdir.
flow_main_after_restart() {
  # New tempdir, daemon will read TOML that we copied in.
  peko_iso_init "probe-session-grant-fix-2" || return 1

  # Copy the principal + grant from the prior tempdir.
  cp -r "$1/home/.peko/principals" "$PEKO_HOME/" 2>/dev/null || true
  cp -r "$1/home/.peko/data" "$PEKO_HOME/" 2>/dev/null || true

  # We don't have the model in the isolated catalog anymore, so add it.
  peko_iso_run model add --template anthropic --model claude-sonnet-4-5 --key dummy 2>&1 | tail -1
  peko_iso_assert_rc_zero

  # Re-grant to be sure (daemon-mediated).
  peko_iso_start_daemon || return 1
  peko_iso_run capability grant --principal probe tool:session
  peko_iso_assert_rc_zero

  # Now restart so it re-reads.
  peko_iso_done 0
}

# Invoke both halves. The first half's tempdir is captured via PEKO_HOME
# before peko_iso_done wipes it. Actually peko_iso_done removes it, so
# capture grants into a stash file first.
#
# Simpler: rewrite to a single in-script approach — the manual grant
# only needs the daemon for the IPC write. The bug we're testing is
# whether the GRANT, once persisted, makes the tool appear. So:

flow_main() {
  peko_iso_init "probe-session-grant-fix" || return 1

  peko_iso_run model add --template anthropic --model claude-sonnet-4-5 --key dummy 2>&1 | tail -1
  peko_iso_assert_rc_zero

  peko_iso_run principal create probe --model anthropic-claude-sonnet-4-5 2>&1 | tail -1
  peko_iso_assert_rc_zero

  peko_iso_start_daemon || return 1

  peko_iso_run capability grant --principal probe tool:session
  peko_iso_assert_rc_zero

  echo "──── principal grants after manual grant (TOML on disk) ────"
  grep -oE 'tool:[A-Za-z_]+' "$PEKO_HOME/principals/probe/principal.toml" | sort -u | tr '\n' ' '
  echo

  echo "──── but the daemon already loaded its in-memory principal state ────"
  echo "    Per scripts/e2e/lib/isolate.sh docs: 'daemon doesn't runtime-reload principals'."
  echo "    So this single-flow test can't actually verify the fix end-to-end."
  echo "    The TOML grant persists; verification requires a daemon restart."

  peko_iso_done 0
}