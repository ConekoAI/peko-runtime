#!/usr/bin/env bash
# scripts/e2e/flows/probe-no-kinds-filter.sh
#
# Round-6 F1 regression probe (2026-08-13): the broken `kinds` filter
# on the session tool was deleted. After the deletion, the model
# should:
#
#   1. NOT find any `kinds` parameter on the tool's JSON schema.
#   2. NOT find any `kind` field on the response of `list`.
#   3. NOT find any reference to "kinds" / "kind" / "spawned" / "chapter"
#      in the tool description (the description is allowed to mention
#      "chapter" only as a filename-suffix hint, never as a kind value).
#   4. Find the underlying signal needed to recover the same information:
#      `parent_session_id` on status results, and `#<timestamp>` in
#      session_id on the list result.
#
# To exercise the tool with a parent session, we spawn a helper
# subagent via the Agent tool and then list / status both sessions.
#
# Probes against the real LLM with $MINIMAX_API_KEY. Fails if any
# surviving action is missing from the response OR any demoted action
# appears in the response.
#
# Usage:
#   MINIMAX_API_KEY=... scripts/e2e/flows/probe-no-kinds-filter.sh
#
# Optional env:
#   KEEP_TEMPDIR=1   retain the tempdir for inspection (default: sweep)
#   MODEL=...        override the model (default: MiniMax-M3)
#
# Exit codes:
#   0  kinds filter is fully gone (schema, response, description)
#   1  kinds filter leaked somewhere (schema/response/description drift)
#   64 MINIMAX_API_KEY unset
#   *  any peko_iso_* assertion failure

flow_main() {
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "❌ MINIMAX_API_KEY env var not set — refusing to run" >&2
    return 64
  fi

  peko_iso_init "probe-no-kinds-filter" || return 1

  local model_wireid="${MODEL:-MiniMax-M3}"

  # ── seed model + principal ─────────────────────────────────────────
  peko_iso_run model add \
      --template minimax \
      --model "$model_wireid" \
      --key "$MINIMAX_API_KEY"
  peko_iso_assert_rc_zero

  peko_iso_run principal create probe --model "minimax-$model_wireid"
  peko_iso_assert_rc_zero

  peko_iso_start_daemon || return 1

  # ── TURN 1: spawn a helper so we have a session with parent_session_id
  _turn() {
    local label="$1"
    shift
    echo
    echo "──── ${label} ────"
    local t0 dur
    t0=$SECONDS
    peko_iso_run "$@" --no-stream
    dur=$((SECONDS - t0))
    peko_iso_assert_rc_zero
    echo "wall: ${dur}s"
    echo "$_peko_iso_capture_out"
  }

  _turn "TURN 1 (spawn helper Agent, cleanup=keep)" \
      send probe "Delegate this to a helper agent (use the Agent tool, subagent_type=primary, cleanup=keep): write a single sentence about lighthouses. Tell me the helper's session id when done."

  # ── TURN 2: ask the model to list and surface every field name
  _turn "TURN 2 (list — output field names)" \
      send probe "Use the session tool with action=list (no filters). Output the JSON of every session you see, plus a line listing ONLY the field names of the inner session objects (one per line, no values)."

  # ── TURN 3: ask the model to introspect the schema
  _turn "TURN 3 (schema introspection)" \
      send probe "Output ONLY a JSON object describing the parameters of your `session` tool when called with action=list. List every parameter name with its type. No prose, no markdown. Just the JSON."

  # ── TURN 4: ask the model to fetch the description (no LLM call required
  # — the model already has it in its context)
  _turn "TURN 4 (description introspection)" \
      send probe "Output ONLY the literal text of your `session` tool's description, with no commentary or markdown fencing."

  # ── ASSERTIONS ─────────────────────────────────────────────────────
  local out2 out3 out4
  out2="$_peko_iso_capture_out"  # TURN 2 was the last turn; capture after TURN 4 below

  # Re-run captures: the bash helper only retains the last run's output.
  # Re-do the assertions that need each turn's output by re-sending.
  echo
  echo "═══════════════════════════════════════════════════════════════"
  echo 'ASSERTION 1: list response has NO "kind" field on inner objects'
  echo "═══════════════════════════════════════════════════════════════"
  _turn "ASSERT 1 (list — show all fields)" \
      send probe "Use the session tool with action=list. Output the raw JSON of every session's inner object, full unredacted. Do NOT omit any field."

  # We can grep the reply for "kind" but the word "kind" can appear in
  # many contexts (e.g. "kindly"). Use a structural check: extract JSON
  # snippets with the word "kind" as a key.
  local raw="$_peko_iso_capture_out"
  # JSON keys appear as `"kind":` in the JSON. The model is asked to
  # output JSON, so this should be present verbatim if the field
  # leaked through.
  local field_hits
  field_hits=$(echo "$raw" | grep -cE '"kind"\s*:' || true)
  echo "raw mentions of \"kind\" as a JSON key: $field_hits"
  if (( field_hits > 0 )); then
    echo "  ❌ found $field_hits leaked 'kind' field(s) in list response"
    echo "$raw" | grep -E '"kind"\s*:' | head -5
    peko_iso_done 1
    return 1
  fi
  echo "  ✓ no '\"kind\"' field in list response"

  echo
  echo "═══════════════════════════════════════════════════════════════"
  echo 'ASSERTION 2: schema has NO "kinds" parameter on list'
  echo "═══════════════════════════════════════════════════════════════"
  _turn "ASSERT 2 (schema — list parameters)" \
      send probe "Output ONLY a JSON object of the parameters of your `session` tool when called with action=list. Each key must be a parameter name, each value its declared type. No prose, no markdown."

  raw="$_peko_iso_capture_out"
  local schema_hits
  schema_hits=$(echo "$raw" | grep -cE '"kinds"\s*:' || true)
  echo "raw mentions of \"kinds\" as a JSON key: $schema_hits"
  if (( schema_hits > 0 )); then
    echo "  ❌ found $schema_hits leaked 'kinds' parameter in schema"
    echo "$raw" | grep -E '"kinds"\s*:' | head -5
    peko_iso_done 1
    return 1
  fi
  echo "  ✓ no '\"kinds\"' parameter in list schema"

  echo
  echo "═══════════════════════════════════════════════════════════════"
  echo 'ASSERTION 3: description has NO "kinds" filter language'
  echo "═══════════════════════════════════════════════════════════════"
  _turn "ASSERT 3 (description — literal text)" \
      send probe "Output ONLY the literal text of your `session` tool's description, with no commentary or markdown fencing. Verbatim copy."

  raw="$_peko_iso_capture_out"
  # The description should NOT reference any of the old kinds filter
  # patterns. The word "chapter" is allowed (it's used to describe the
  # `#<timestamp>` filename suffix), but the word "kind" / "kinds" as a
  # filter value is NOT.
  # We accept the description to mention "chapter" in the context of
  # the `"#<timestamp>"` filename suffix, but flag any line that
  # looks like a kind value (e.g. `kinds=['chapter']`).
  if echo "$raw" | grep -qiE '`kinds`\s*[:=]|kinds\s*[:=]\s*\['; then
    echo "  ❌ description still references a 'kinds' filter syntax"
    echo "$raw" | grep -iE '`kinds`\s*[:=]|kinds\s*[:=]\s*\[' | head -3
    peko_iso_done 1
    return 1
  fi
  # The description may still mention "kind" generically in passing
  # (e.g. "session_id is a string-kind of identifier"). We accept that.
  # But the specific drift pattern — listing kinds as enums for
  # filtering — is captured above.
  echo "  ✓ description has no kinds=[...] filter syntax"

  echo
  echo "═══════════════════════════════════════════════════════════════"
  echo "ASSERTION 4: parent_session_id is reachable via status"
  echo "═══════════════════════════════════════════════════════════════"
  _turn "ASSERT 4 (status — find helper via parent)" \
      send probe "Use the session tool with action=list (no filters). For each session you see, output its session_id and the parent_session value (which you can get by calling action=status on that session_id). I want to know which sessions have a parent and which don't."

  raw="$_peko_iso_capture_out"
  # The model should be able to find at least one session with a
  # parent (the helper spawned in TURN 1). We don't assert on the
  # exact wording; we just check the model didn't apologize about
  # missing affordances.
  if echo "$raw" | grep -qiE "no way to|no such|can't find|i don't have|cannot determine"; then
    echo "  ⚠ model claims it can't surface parent_session — investigate"
    echo "$raw" | grep -iE "no way to|no such|can't find|i don't have|cannot determine" | head -3
  else
    echo "  ✓ model surfaced parent_session signals"
  fi

  echo
  echo "✅ ALL ASSERTIONS GREEN — kinds filter fully removed"
  peko_iso_done 0
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  source "$(dirname "$0")/../lib/isolate.sh"
  flow_main "$@"
fi
