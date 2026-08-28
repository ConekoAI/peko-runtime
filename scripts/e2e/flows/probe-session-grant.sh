#!/usr/bin/env bash
# scripts/e2e/flows/probe-session-grant.sh
#
# Offline probe: does a freshly-created default principal actually have
# access to the new `session` tool? The session tool is wired into
# BuiltinToolAdapter (peko-rs/core/src/extensions/builtin/adapter.rs:252)
# but a `tool:session` grant may or may not be in starter_bundle.
# Reproduces (or refutes) the same shape as the 2026-08-07 Finding 2
# (cron tools described but not bound). No LLM API required.

flow_main() {
  peko_iso_init "probe-session-grant" || return 1

  # Seed a no-op model so the principal can be created. The key isn't
  # used — we never make an LLM call.
  peko_iso_run model add --template anthropic --model claude-sonnet-4-5 --key dummy 2>&1 | head -3
  peko_iso_assert_rc_zero

  # Create a default principal.
  peko_iso_run principal create probe --model anthropic-claude-sonnet-4-5 2>&1
  peko_iso_assert_rc_zero

  # Don't start the daemon — just inspect on-disk capability grants.
  echo
  echo "──── principal toml: capability_grants ────"
  local toml="$PEKO_HOME/principals/probe/principal.toml"
  [[ -f "$toml" ]] || { echo "❌ no principal.toml at $toml"; return 1; }
  grep -A2 'capability_grants\|capabilities' "$toml" || echo "(no capability section found)"

  echo
  echo "──── which tool:* grants does probe hold? ────"
  grep -oE '"tool:[A-Za-z_]+"' "$toml" | sort -u || echo "(no tool:* grants)"

  echo
  echo "──── specifically: tool:Session or tool:session ────"
  if grep -qE '"tool:[Ss]ession"' "$toml"; then
    echo "  ✓ tool:session granted"
  else
    echo "  ❌ tool:session NOT granted — model will not see the new session tool"
  fi

  # Now start the daemon, send a no-op query, and inspect the daemon
  # log for the dynamically-built tool list. This is what the model
  # actually sees in its context.
  echo
  echo "──── starting daemon + sending probe query ────"
  peko_iso_start_daemon || return 1

  # Streaming render is the default. The model will report
  # "no such tool" because session isn't in its toolset.
  peko_iso_run send probe "Use the session tool to list all your sessions. Then use it to compact the current session."
  echo "rc=$_peko_iso_capture_rc"
  echo "──── reply ────"
  echo "$_peko_iso_capture_out" | head -40

  echo
  echo "──── daemon log: 'Dynamically built N tool definitions' lines ────"
  grep -E "Dynamically built|tool definitions|toolset|available" "$_PEKO_ISO_TEMPDIR/daemon.err" 2>/dev/null | head -5

  echo
  echo "──── does 'session' appear in any tool definition in the daemon log? ────"
  if grep -qi '"session"\|name.*session' "$_PEKO_ISO_TEMPDIR/daemon.err"; then
    echo "  ✓ 'session' mentioned in daemon tool building"
    grep -i '"session"\|name.*session' "$_PEKO_ISO_TEMPDIR/daemon.err" | head -3
  else
    echo "  ❌ 'session' tool never built for principal"
  fi

  # TEST 2 deferred to a separate run with `probe-session-grant-fix` —
  # `peko_iso_done` is wired to the EXIT trap and can't be re-entered
  # from inside a single flow. Workaround question (does manual grant
  # fix it?) is interesting but secondary to the main finding; logged
  # here as an open follow-up.
  echo
  echo "──── follow-up noted: does an explicit 'peko capability grant' fix it? ────"
  echo "    (skipped — separate flow needed to avoid EXIT-trap re-entry)"

  peko_iso_done 0
}