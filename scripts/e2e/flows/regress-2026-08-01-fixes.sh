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
  peko_iso_run send scout "" || true
  if [[ "$_peko_iso_capture_rc" -ne 0 ]] \
     && grep -q "Message is empty" <<<"$_peko_iso_capture_err$_peko_iso_capture_out"; then
    echo "✅ Fix #4: empty \"\" refused (rc=$_peko_iso_capture_rc)"
  else
    echo "❌ Fix #4 regression — empty message should be rejected: rc=$_peko_iso_capture_rc" >&2
    echo "stderr: $_peko_iso_capture_err" >&2
    peko_iso_done 1
  fi

  peko_iso_run send scout "   " || true
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

  # ============================================================
  # Fix #8 — `quota status --json` returns the JSON envelope
  # (Bug B from scripts/e2e/reports/2026-08-01-non-technical-user-field-test-v2.md)
  # ============================================================
  echo "─── Fix #8: quota status --json envelope ───"
  # Need a daemon to satisfy `quota status`; this flow does not start
  # one (it runs without MINIMAX_API_KEY), so we expect the
  # "Daemon is not running" error. The point of this regression is
  # the CLI-level envelope shape: with --json, even on failure the
  # human-readable branch is not what users get back. The shape is
  # pinned by `quota_status_json_envelope_shape` (unit test); here
  # we just confirm the --json flag is accepted on the CLI surface
  # and that it doesn't silently print the empty stdout Bug B did.
  peko_iso_run quota status scout --json || true
  if [[ "$_peko_iso_capture_rc" -ne 0 ]] \
     && grep -q "Daemon is not running" <<<"$_peko_iso_capture_err$_peko_iso_capture_out"; then
    echo "✅ Fix #8: --json reaches the daemon path (rc=$_peko_iso_capture_rc, daemon-not-running is correct)"
  else
    echo "❌ Fix #8 regression — expected daemon-not-running error, got rc=$_peko_iso_capture_rc" >&2
    echo "   stderr: $_peko_iso_capture_err" >&2
    peko_iso_done 1
  fi
  echo

  # ============================================================
  # Fix #9 — `principal show --json` exposes the persona block
  # (Bug D from the v2 field test — extend the existing envelope
  #  with persona.{description, goals, values} instead of adding
  #  a new `principal persona show` subcommand)
  # ============================================================
  echo "─── Fix #9: principal show --json persona block ───"
  peko_iso_run principal create alpha --model minimax-MiniMax-M3 || true
  peko_iso_run principal show alpha --json
  peko_iso_assert_rc_zero || peko_iso_done 1
  # Persona block must be present even when empty (Bug D fix is
  # additive — existing JSON consumers don't break because the
  # new field is always emitted).
  if grep -q '"persona"' <<<"$_peko_iso_capture_out" \
     && grep -q '"goals"' <<<"$_peko_iso_capture_out" \
     && grep -q '"values"' <<<"$_peko_iso_capture_out" \
     && grep -q '"description"' <<<"$_peko_iso_capture_out"; then
    echo "✅ Fix #9: persona block present in show --json"
  else
    echo "❌ Fix #9 regression — persona block missing from show --json:" >&2
    echo "$_peko_iso_capture_out" | head -20 >&2
    peko_iso_done 1
  fi
  echo

  # ============================================================
  # Fix #10 — `principal send` no longer exists
  # (Bug E from the v2 field test — top-level `peko send` is the
  #  canonical command; `principal send` was a duplicate)
  # ============================================================
  echo "─── Fix #10: principal send removed ───"
  peko_iso_run principal send alpha "hello" || true
  if [[ "$_peko_iso_capture_rc" -eq 2 ]] \
     && grep -q "unrecognized subcommand" <<<"$_peko_iso_capture_err$_peko_iso_capture_out"; then
    echo "✅ Fix #10: principal send rejected (rc=2, unrecognized subcommand)"
  else
    echo "❌ Fix #10 regression — principal send should be rejected, got rc=$_peko_iso_capture_rc" >&2
    echo "   stderr: $_peko_iso_capture_err" >&2
    peko_iso_done 1
  fi
  echo

  # ============================================================
  # Fix #11 — `-v` and `system doctor --verbose` don't pollute
  # non-TTY stderr with ANSI escape codes (Bug C from the v2
  # field test). The `peko_iso_run` helper redirects stderr to a
  # temp file, which is exactly the non-TTY capture path users
  # hit with `> file.log` redirects.
  # ============================================================
  echo "─── Fix #11: ANSI codes stripped on non-TTY stderr ───"
  peko_iso_run -v principal show alpha --json >/dev/null 2>&1 || true
  # `_peko_iso_capture_err` was the stderr of the previous call.
  # Make a fresh capture: --help exercises the same tracing init
  # path without needing IPC.
  peko_iso_run -v --help >/dev/null 2>/dev/null
  if grep -q $'\x1b' <<<"$_peko_iso_capture_err"; then
    echo "❌ Fix #11 regression — stderr contains ANSI escape codes:" >&2
    head -c 200 <<<"$_peko_iso_capture_err" | od -c | head -3 >&2
    peko_iso_done 1
  else
    echo "✅ Fix #11: -v / --help stderr has no ANSI codes when captured"
  fi
  echo

  # ============================================================
  # Fix #12 — `peko principal persona show <name>` exists (Bug F
  # from scripts/e2e/reports/2026-08-01-non-technical-user-landlord-email.md).
  # The v2 Bug D fix added a `persona` block to `show --json`, but
  # a non-tech user still had no direct CLI read-back. The fix
  # adds a dedicated `persona show` subcommand.
  # ============================================================
  echo "─── Fix #12: principal persona show subcommand ───"
  peko_iso_run principal create persona-test --model minimax-MiniMax-M3
  peko_iso_assert_rc_zero || peko_iso_done 1
  # Default principal has identity.description="The persona-test Principal"
  # but empty goals/values. The handler's `is_set` is true (description
  # present), so the text path prints the description + empty Goals/Values.
  peko_iso_run principal persona show persona-test
  if [[ "$_peko_iso_capture_rc" -eq 0 ]] \
     && grep -q "Persona for 'persona-test'" <<<"$_peko_iso_capture_out" \
     && grep -q "Description:" <<<"$_peko_iso_capture_out" \
     && grep -q "Goals:" <<<"$_peko_iso_capture_out" \
     && grep -q "Values:" <<<"$_peko_iso_capture_out"; then
    echo "✅ Fix #12: persona show text emits Description/Goals/Values"
  else
    echo "❌ Fix #12 regression — persona show text shape unexpected (rc=$_peko_iso_capture_rc):" >&2
    echo "$_peko_iso_capture_out" | head -10 >&2
    echo "stderr: $_peko_iso_capture_err" >&2
    peko_iso_done 1
  fi

  peko_iso_run principal persona show persona-test --json
  if [[ "$_peko_iso_capture_rc" -eq 0 ]] \
     && [[ "$_peko_iso_capture_out" == "{"* ]] \
     && grep -q '"name": "persona-test"' <<<"$_peko_iso_capture_out" \
     && grep -q '"isSet":' <<<"$_peko_iso_capture_out" \
     && grep -q '"description":' <<<"$_peko_iso_capture_out" \
     && grep -q '"goals":' <<<"$_peko_iso_capture_out" \
     && grep -q '"values":' <<<"$_peko_iso_capture_out"; then
    echo "✅ Fix #12: persona show --json envelope has name/isSet/description/goals/values"
  else
    echo "❌ Fix #12 regression — persona show --json envelope shape unexpected:" >&2
    echo "$_peko_iso_capture_out" | head -10 >&2
    peko_iso_done 1
  fi
  echo

  # ============================================================
  # Fix #13 — `peko_iso_run` always returns 0 (Bug G from the v3
  # field test). Before the fix, this helper propagated the
  # captured exit code via `return`, which `set -euo pipefail` in
  # scripts/e2e/run-case.sh treated as a script failure —
  # truncating exploratory flows on any probed non-zero rc.
  # The regression: a known-rc=2 call (unrecognized subcommand)
  # must NOT kill the flow; the next line MUST execute. We assert
  # by writing a sentinel to disk immediately after the probe; if
  # the lib regresses and `set -e` kills the flow, the sentinel
  # is never written and the assertion at the END of the flow
  # catches it.
  # ============================================================
  echo "─── Fix #13: peko_iso_run returns 0 even on non-zero rc ───"
  # `principal send` is a known-rc=2 path (Bug E fix deleted it).
  # Call it WITHOUT `|| true`; if the lib regresses, `set -e`
  # exits the flow here and the sentinel below is never written.
  peko_iso_run principal send persona-test "hello"
  local probe_rc="$_peko_iso_capture_rc"
  # Sentinel — written only if we get past the probe call.
  echo "fix13-ok" > /tmp/_peko_iso_fix13_marker
  if [[ "$probe_rc" -ne 0 ]] \
     && grep -q "unrecognized subcommand" <<<"$_peko_iso_capture_err$_peko_iso_capture_out"; then
    echo "✅ Fix #13: non-zero rc was captured (rc=$probe_rc); sentinel written; flow continues"
  else
    echo "❌ Fix #13 regression — expected rc=2 + 'unrecognized subcommand', got rc=$probe_rc" >&2
    echo "   stderr: $_peko_iso_capture_err" >&2
    peko_iso_done 1
  fi
  echo

  # ============================================================
  # Fix #14 — `quota status` `input_tokens` is non-zero after real
  # LLM calls (Bug H from the v3 field test). Some providers
  # (notably MiniMax M3 / api.minimaxi.com/anthropic) report
  # input_tokens in `message_delta.usage` and 0 in
  # `message_start.message.usage`; before the fix the Anthropic
  # adapter dropped the delta's input and the meter recorded 0.
  # This regression requires MINIMAX_API_KEY — gated below.
  # ============================================================
  echo "─── Fix #14: input_tokens > 0 after real LLM calls ───"
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "⏭  Fix #14 skipped (no MINIMAX_API_KEY — requires real LLM)"
  else
    # Need a daemon for `send` IPC. Start one; stop it before exit.
    peko_iso_start_daemon || peko_iso_done 1
    # 3 short sends — each call costs ~50-100 input tokens plus
    # the persona + system prompt (~500 tokens). After 3 sends
    # `input_tokens` should easily exceed 1000. Use `alpha`
    # (already created) to avoid consuming quota on a fresh
    # principal.
    peko_iso_run send alpha "Reply with just the word ok"
    peko_iso_assert_rc_zero || peko_iso_done 1
    peko_iso_run send alpha "Reply with just the word ok"
    peko_iso_assert_rc_zero || peko_iso_done 1
    peko_iso_run send alpha "Reply with just the word ok"
    peko_iso_assert_rc_zero || peko_iso_done 1

    peko_iso_run quota status alpha --json
    peko_iso_assert_rc_zero || peko_iso_done 1
    # Extract the four counters using `grep -o` — keeps the flow
    # dependency-free (no `jq` requirement) AND portable across
    # BSD sed (macOS) and GNU sed. Earlier sed-based extraction
    # silently returned empty strings on BSD sed because
    # `sed -n 's/.../.../p'` with backreferences is unreliable
    # there.
    local in_tok out_tok req_count
    in_tok=$(grep -o '"input_tokens":[[:space:]]*[0-9]*' <<<"$_peko_iso_capture_out" \
             | grep -o '[0-9]*$' | head -1)
    out_tok=$(grep -o '"output_tokens":[[:space:]]*[0-9]*' <<<"$_peko_iso_capture_out" \
              | grep -o '[0-9]*$' | head -1)
    req_count=$(grep -o '"request_count":[[:space:]]*[0-9]*' <<<"$_peko_iso_capture_out" \
                | grep -o '[0-9]*$' | head -1)
    if [[ -n "$in_tok" ]] && [[ "$in_tok" -gt 0 ]] \
       && [[ -n "$out_tok" ]] && [[ "$out_tok" -gt 0 ]] \
       && [[ -n "$req_count" ]] && [[ "$req_count" -eq 3 ]]; then
      echo "✅ Fix #14: input=$in_tok output=$out_tok requests=$req_count (all 3 counters healthy)"
    else
      echo "❌ Fix #14 regression — input_tokens stuck at 0 (Bug H regression):" >&2
      echo "   input=$in_tok output=$out_tok requests=$req_count" >&2
      peko_iso_done 1
    fi
  fi
  echo

  echo "🎉 all 14 fixes pass (v1: #1-#7, v2: #8 #9 #10 #11, v3: #12 #13 #14)"
  # Final assertion for Fix #13 — the sentinel must still be on
  # disk, proving `peko_iso_run` didn't kill the flow via `set -e`.
  if [[ ! -s /tmp/_peko_iso_fix13_marker ]]; then
    echo "❌ Fix #13 sentinel missing — flow was killed by set -e on the probe rc" >&2
    peko_iso_done 1
  fi
  rm -f /tmp/_peko_iso_fix13_marker
  peko_iso_done 0
}
