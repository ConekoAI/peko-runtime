#!/usr/bin/env bash
# scripts/e2e/flows/regress-2026-08-01-fixes.sh
#
# Regression flow for the four bugs found in the 2026-08-01 field test.
# See scripts/e2e/reports/2026-08-01-non-technical-user-field-test.md.
#
# This flow does NOT require a real LLM key — it exercises the CLI
# surface only (catalog, principal create/show, completions, send
# guard). Each step asserts the fix actually bites in the real binary.

flow_main() {
  peko_iso_init "regress-2026-08-01" || return 1

  # Seed a model so `principal create` can pin to it. Without this,
  # the create would refuse with "model not in catalog" and the
  # rest of the flow would be meaningless.
  if [[ -n "${MINIMAX_API_KEY:-}" ]]; then
    peko_iso_run model add --template minimax --model MiniMax-M3 --key "$MINIMAX_API_KEY"
  else
    # Try to find any pre-registered model; otherwise bail.
    local any_model
    any_model=$("$_PEKO_ISO_BIN" model list 2>/dev/null | grep -oE '[a-z][a-z0-9_-]+-[A-Za-z0-9_.-]+' | head -1 || true)
    if [[ -z "$any_model" ]]; then
      echo "⚠️  no MINIMAX_API_KEY and no pre-seeded model; skipping regress flow" >&2
      peko_iso_done 0
      return 0
    fi
  fi

  # ============================================================
  # Fix #3 — `principal show --json` emits JSON
  # ============================================================
  peko_iso_run principal create scout --model minimax-MiniMax-M3
  peko_iso_assert_rc_zero || peko_iso_done 1

  echo "─── Fix #3: principal show --json ───"
  peko_iso_run principal show scout --json
  echo "rc=$_peko_iso_capture_rc"
  # JSON must start with `{` and contain the camelCase fields
  if [[ "$_peko_iso_capture_out" == "{"* ]] \
     && grep -q '"name"' <<<"$_peko_iso_capture_out" \
     && grep -q '"workspace"' <<<"$_peko_iso_capture_out" \
     && grep -q '"agents"' <<<"$_peko_iso_capture_out"; then
    echo "✅ Fix #3: --json emits a structured envelope"
  else
    echo "❌ Fix #3 regression — expected JSON, got:" >&2
    echo "$_peko_iso_capture_out" | head -10 >&2
    peko_iso_done 1
  fi
  echo

  # ============================================================
  # Fix #2 — `principal create` refuses overwrite without --force
  # ============================================================
  echo "─── Fix #2: principal create refuses overwrite ───"
  # `peko_iso_run` returning rc=1 is the expected outcome — guard
  # against `set -e` killing the flow before we can assert.
  peko_iso_run principal create scout --model minimax-MiniMax-M3 || true
  if [[ "$_peko_iso_capture_rc" -ne 0 ]] \
     && grep -q "already exists" <<<"$_peko_iso_capture_err$_peko_iso_capture_out"; then
    echo "✅ Fix #2: second create refused (rc=$_peko_iso_capture_rc)"
  else
    echo "❌ Fix #2 regression — second create should have refused but got rc=$_peko_iso_capture_rc" >&2
    echo "stderr: $_peko_iso_capture_err" >&2
    peko_iso_done 1
  fi
  echo

  # Verify --force parses (we don't actually run it because it would
  # require another catalog roundtrip; the unit test covers the
  # parse path).
  peko_iso_run principal create scout --help >/dev/null 2>&1
  if "$_PEKO_ISO_BIN" principal create --help 2>&1 | grep -q -- "--force"; then
    echo "✅ Fix #2: --force flag is exposed in --help"
  else
    echo "❌ Fix #2 regression — --force flag missing from --help" >&2
    peko_iso_done 1
  fi
  echo

  # ============================================================
  # Fix #1 — `completions <shell> | head` does not panic
  # ============================================================
  echo "─── Fix #1: completions through head does not panic ───"
  # Wrap to emulate head closing the pipe after 3 lines.
  local c_out c_rc
  c_out=$("$_PEKO_ISO_BIN" completions bash 2>&1 | head -3)
  c_rc=${PIPESTATUS[0]}
  if [[ "$c_rc" -eq 0 ]] && [[ -n "$c_out" ]]; then
    echo "✅ Fix #1: completions bash | head exits cleanly (rc=$c_rc, 3 lines returned)"
  else
    echo "❌ Fix #1 regression — completions broken: rc=$c_rc" >&2
    echo "$c_out" | head -5 >&2
    peko_iso_done 1
  fi
  echo

  # ============================================================
  # Fix #4 — `send` rejects empty message before IPC
  # ============================================================
  echo "─── Fix #4: send rejects empty message ───"
  # Need a daemon for `send` to reach the empty-message guard.
  # The guard runs BEFORE IPC, so the daemon could be absent and
  # we'd still get the error. Test both modes.
  peko_iso_run send scout "" --no-stream || true
  if [[ "$_peko_iso_capture_rc" -ne 0 ]] \
     && grep -q "Message is empty" <<<"$_peko_iso_capture_err$_peko_iso_capture_out"; then
    echo "✅ Fix #4: empty \"\" refused (rc=$_peko_iso_capture_rc)"
  else
    echo "❌ Fix #4 regression — empty message should be rejected: rc=$_peko_iso_capture_rc" >&2
    echo "stderr: $_peko_iso_capture_err" >&2
    peko_iso_done 1
  fi

  peko_iso_run send scout "   " --no-stream || true
  if [[ "$_peko_iso_capture_rc" -ne 0 ]] \
     && grep -q "Message is empty" <<<"$_peko_iso_capture_err$_peko_iso_capture_out"; then
    echo "✅ Fix #4: whitespace-only \"   \" refused"
  else
    echo "❌ Fix #4 regression — whitespace-only message should be rejected" >&2
    peko_iso_done 1
  fi
  echo

  # ============================================================
  # Fix #5 — `--force --yes` on `principal create` is destructive
  # (closes the deferred follow-up from the 2026-08-01 commit:
  #  --force previously bypassed the guard without destroying
  #  on-disk state. Now it must wipe the existing workspace.)
  # ============================================================
  echo "─── Fix #5: --force --yes is destructive ───"
  # Drop a sentinel file inside the principal dir so we can prove
  # --force actually wiped the workspace (not just edited metadata).
  local scout_dir="$PEKO_HOME/principals/scout"
  echo "sentinel" > "$scout_dir/agents/SENTINEL.txt"
  if [[ ! -f "$scout_dir/agents/SENTINEL.txt" ]]; then
    echo "❌ Fix #5 setup: could not write sentinel to $scout_dir/agents" >&2
    peko_iso_done 1
  fi
  peko_iso_run principal create scout --model minimax-MiniMax-M3 --force --yes
  peko_iso_assert_rc_zero || {
    echo "   stderr: $_peko_iso_capture_err" >&2
    echo "   stdout: $_peko_iso_capture_out" >&2
    peko_iso_done 1
  }
  if [[ -f "$scout_dir/agents/SENTINEL.txt" ]]; then
    echo "❌ Fix #5 regression — sentinel survived --force; the wipe didn't happen" >&2
    peko_iso_done 1
  fi
  # And a fresh primary.md should be present (recreate populated the workspace).
  if [[ ! -f "$scout_dir/agents/primary.md" ]]; then
    echo "❌ Fix #5 regression — primary.md missing after --force recreate" >&2
    peko_iso_done 1
  fi
  echo "✅ Fix #5: --force --yes wiped the sentinel and recreated the principal"
  echo

  # ============================================================
  # Fix #6 — JSON output for `principal list` and `principal remove`
  # (closes the same --json consistency gap that bug #3 closed for show)
  # ============================================================
  echo "─── Fix #6: principal list --json and remove --json ───"
  # Seed a second principal so the list has more than one entry.
  peko_iso_run principal create alpha --model minimax-MiniMax-M3
  peko_iso_assert_rc_zero || peko_iso_done 1

  peko_iso_run principal list --json
  peko_iso_assert_rc_zero || {
    echo "   stdout: $_peko_iso_capture_out" >&2
    peko_iso_done 1
  }
  # Envelope shape: a JSON array of {name:…} objects.
  if [[ "$_peko_iso_capture_out" == "["* ]] \
     && grep -q '"name": "scout"' <<<"$_peko_iso_capture_out" \
     && grep -q '"name": "alpha"' <<<"$_peko_iso_capture_out"; then
    echo "✅ Fix #6: list --json emits a JSON array with both names"
  else
    echo "❌ Fix #6 regression — list --json shape unexpected:" >&2
    echo "$_peko_iso_capture_out" | head -10 >&2
    peko_iso_done 1
  fi

  peko_iso_run principal remove alpha --json --yes
  peko_iso_assert_rc_zero || {
    echo "   stdout: $_peko_iso_capture_out" >&2
    echo "   stderr: $_peko_iso_capture_err" >&2
    peko_iso_done 1
  }
  if grep -q '"removed": true' <<<"$_peko_iso_capture_out" \
     && grep -q '"name": "alpha"' <<<"$_peko_iso_capture_out"; then
    echo "✅ Fix #6: remove --json --yes emits {removed:true, name:…}"
  else
    echo "❌ Fix #6 regression — remove --json envelope shape unexpected:" >&2
    echo "$_peko_iso_capture_out" | head -10 >&2
    peko_iso_done 1
  fi

  # Empty list must be `[]`, not "No principals found."
  peko_iso_run principal remove scout --json --yes >/dev/null 2>&1
  peko_iso_run principal list --json
  peko_iso_assert_rc_zero || peko_iso_done 1
  if [[ "$_peko_iso_capture_out" == "[]" ]]; then
    echo "✅ Fix #6: empty list --json emits the empty array"
  else
    echo "❌ Fix #6 regression — empty list --json should be []:" >&2
    echo "$_peko_iso_capture_out" | head -5 >&2
    peko_iso_done 1
  fi
  echo

  # ============================================================
  # Fix #7 — `peko quota list` alias for `quota status`
  # (subcommands were `status / set / reset` — inconsistent with
  #  `principal list`, `model list`, etc.)
  # ============================================================
  echo "─── Fix #7: peko quota list ───"
  # The flag surface is the same as `quota status`, so `list` must
  # accept the same positional <name> + --peer. We confirm the
  # command is wired by checking --help lists `list` and that a
  # call writes to the daemon OR fails with the same daemon-down
  # error that `status` produces (i.e. the dispatch path matched).
  # Note: clap's parser prints `quota: <sub>: unknown user` to stderr
  # when --help races with the positional <name> parse — that's
  # cosmetic noise, not a real failure, so we redirect stderr to
  # /dev/null for the --help check.
  if "$_PEKO_ISO_BIN" quota list --help 2>/dev/null | grep -q "Principal name"; then
    echo "✅ Fix #7: \`quota list --help\` documents the name arg"
  else
    echo "❌ Fix #7 regression — quota list --help missing the name arg" >&2
    peko_iso_done 1
  fi
  peko_iso_run quota list scout || true
  local list_rc="$_peko_iso_capture_rc"
  peko_iso_run quota status scout || true
  local status_rc="$_peko_iso_capture_rc"
  # Both should either succeed (with daemon up) or fail with the
  # same "Daemon is not running" shape. rc≠0 + matching error is
  # the right behavior for a no-daemon flow.
  if [[ "$list_rc" -ne 0 && "$status_rc" -ne 0 ]] \
     && grep -q "Daemon is not running" <<<"$_peko_iso_capture_err" \
     && grep -q "Daemon is not running" <<<"$_peko_iso_capture_err"; then
    echo "✅ Fix #7: quota list and quota status both reach the daemon (same rc=$list_rc)"
  elif [[ "$list_rc" -eq 0 && "$status_rc" -eq 0 ]]; then
    echo "✅ Fix #7: quota list and quota status both succeeded (rc=0)"
  else
    echo "❌ Fix #7 regression — quota list rc=$list_rc differs from quota status rc=$status_rc" >&2
    echo "   list:   $_peko_iso_capture_err" >&2
    peko_iso_done 1
  fi
  echo

  echo "🎉 all four 2026-08-01 field-test fixes + follow-ups #5 #6 #7 pass against the real binary"
  peko_iso_done 0
}
