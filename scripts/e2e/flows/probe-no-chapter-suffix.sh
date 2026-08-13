#!/usr/bin/env bash
# scripts/e2e/flows/probe-no-chapter-suffix.sh
#
# Round-7 regression probe (2026-08-13): the chapter concept is DELETED.
# Session ids are stable for life; when a session JSONL crosses
# `rotate_bytes` it pages in place — the current page is renamed to
# `<id>.N.jsonl` (N chronological, 1 = oldest) and appends continue into
# a fresh `<id>.jsonl`. Readers stitch pages transparently. This probe
# succeeds auto-paging-large-message.sh, which asserted the retired
# `<id>#<timestamp>.jsonl` chapter-sibling behavior.
#
# Strategy:
#   1. Shrink the threshold via PEKO_TEST_MODE + SESSION_TEST_ROTATE_BYTES
#      (the same knob auto-paging-large-message.sh used; the daemon
#      inherits the env from this shell).
#   2. Delegate a >2 KiB bash result to a helper subagent so a session
#      JSONL pages at least once.
#   3. Assert on disk:
#      - no `chapters.json` anywhere under $PEKO_DATA_DIR
#      - no `#` in any session-dir filename or sessions.json key
#      - the paged session has `<id>.N.jsonl` page(s) alongside `<id>.jsonl`
#   4. Assert the paged session still answers status/history through the
#      session tool (the stitched read path): the model must see the
#      pre-paging task text in the history.
#
# Probes against the real LLM with $MINIMAX_API_KEY.
#
# Usage:
#   MINIMAX_API_KEY=... scripts/e2e/flows/probe-no-chapter-suffix.sh
#
# Optional env:
#   KEEP_TEMPDIR=1   retain the tempdir for inspection (default: sweep)
#   MODEL=...        override the model (default: MiniMax-M3)
#
# Exit codes:
#   0  no chapter residue; paging + stitched reads verified
#   1  chapter residue found, or paging/stitching broken
#   64 MINIMAX_API_KEY unset
#   *  any peko_iso_* assertion failure

flow_main() {
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "❌ MINIMAX_API_KEY env var not set — refusing to run" >&2
    return 64
  fi

  peko_iso_init "probe-no-chapter-suffix" || return 1

  # Tighter threshold so a single shell-output call crosses it. Must be
  # exported BEFORE peko_iso_start_daemon so the daemon inherits it.
  export PEKO_TEST_MODE=1
  export SESSION_TEST_ROTATE_BYTES=2048

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

  # ── TURN 1: delegate a >2 KiB bash result to a helper ─────────────
  # We do NOT hard-assert rc=0: the LLM run may surface a transient
  # downstream error; the round-7 paging behavior is observable on disk
  # regardless of whether the LLM run terminates cleanly (same posture
  # auto-paging-large-message.sh took).
  echo
  echo "──── TURN 1 (spawn helper with a >2 KiB bash result) ────"
  peko_iso_run send probe \
      "Use the Agent tool (subagent_type=primary, cleanup=keep) to delegate this exact task: run 'yes | head -c 5000' in bash and report the exact output verbatim. Don't summarise — I want the full text. Tell me the helper's session id when done." \
      --no-stream
  if [[ $_peko_iso_capture_rc -ne 0 ]]; then
    echo "⚠ send returned rc=$_peko_iso_capture_rc — continuing (disk assertions are the point)"
    echo "   stderr: $_peko_iso_capture_err"
  fi
  echo "$_peko_iso_capture_out"

  # ── locate the sessions dir ────────────────────────────────────────
  local any_jsonl sessions_dir
  any_jsonl=$(find "$PEKO_DATA_DIR/principals" -path '*/sessions/*.jsonl' 2>/dev/null | head -1)
  if [[ -z "$any_jsonl" ]]; then
    echo "❌ no session JSONL found under $PEKO_DATA_DIR/principals"
    peko_iso_done 1
    return 1
  fi
  sessions_dir=$(dirname "$any_jsonl")
  echo
  echo "sessions dir: $sessions_dir"
  ls -la "$sessions_dir"/*.jsonl 2>/dev/null

  # ── ASSERTION 1: no chapters.json anywhere ─────────────────────────
  echo
  echo "═══════════════════════════════════════════════════════════════"
  echo "ASSERTION 1: chapters.json does not exist anywhere"
  echo "═══════════════════════════════════════════════════════════════"
  local stray
  stray=$(find "$PEKO_DATA_DIR" -name 'chapters.json' 2>/dev/null | head -1)
  if [[ -n "$stray" ]]; then
    echo "  ❌ chapters.json found at $stray — chapter concept not fully deleted"
    peko_iso_done 1
    return 1
  fi
  echo "  ✓ no chapters.json under $PEKO_DATA_DIR"

  # ── ASSERTION 2: no '#' in any session id ──────────────────────────
  echo
  echo "═══════════════════════════════════════════════════════════════"
  echo "ASSERTION 2: no session id carries a '#' suffix"
  echo "═══════════════════════════════════════════════════════════════"
  local hash_names
  hash_names=$(find "$PEKO_DATA_DIR/principals" -path '*/sessions/*' -name '*#*' 2>/dev/null)
  if [[ -n "$hash_names" ]]; then
    echo "  ❌ '#'-suffixed entries in the sessions dir:"
    echo "$hash_names" | head -5
    peko_iso_done 1
    return 1
  fi
  echo "  ✓ no '#' in any sessions-dir filename"
  local meta hash_keys
  meta=$(find "$PEKO_DATA_DIR/principals" -name 'sessions.json' 2>/dev/null | head -1)
  if [[ -n "$meta" ]]; then
    hash_keys=$(jq -r 'keys[]' "$meta" 2>/dev/null | grep '#' || true)
    if [[ -n "$hash_keys" ]]; then
      echo "  ❌ '#'-suffixed session ids in sessions.json:"
      echo "$hash_keys" | head -5
      peko_iso_done 1
      return 1
    fi
    echo "  ✓ no '#' in any sessions.json key"
  else
    echo "  ⚠ no sessions.json found — key check skipped"
  fi

  # ── ASSERTION 3: paged session has <id>.N.jsonl alongside <id>.jsonl
  echo
  echo "═══════════════════════════════════════════════════════════════"
  echo "ASSERTION 3: oversized JSONL paged in place (<id>.N.jsonl)"
  echo "═══════════════════════════════════════════════════════════════"
  local page_file paged_id
  page_file=$(find "$PEKO_DATA_DIR/principals" -path '*/sessions/*.jsonl' 2>/dev/null \
      | grep -E '\.[0-9]+\.jsonl$' | sort | head -1)
  if [[ -z "$page_file" ]]; then
    echo "  ❌ no <id>.N.jsonl page found — in-place paging did not fire"
    echo "     (SESSION_TEST_ROTATE_BYTES=$SESSION_TEST_ROTATE_BYTES, threshold knob ignored?)"
    peko_iso_done 1
    return 1
  fi
  paged_id=$(basename "$page_file" | sed -E 's/\.[0-9]+\.jsonl$//')
  echo "  paged session id: $paged_id"
  echo "  pages:"
  find "$sessions_dir" -name "${paged_id}.[0-9]*.jsonl" 2>/dev/null | sort
  if [[ -f "$sessions_dir/$paged_id.jsonl" ]]; then
    echo "  ✓ current page $paged_id.jsonl exists alongside the numbered page(s)"
  else
    echo "  ❌ current page $paged_id.jsonl missing — paging lost the append target"
    peko_iso_done 1
    return 1
  fi

  # ── ASSERTION 4: paged session still answers status/history ────────
  # History reads stitch pages 1..N + the current page inside
  # peko-session's load chokepoint, so the pre-paging task text must be
  # visible to the model.
  echo
  echo "═══════════════════════════════════════════════════════════════"
  echo "ASSERTION 4: paged session still responds to status/history (stitched)"
  echo "═══════════════════════════════════════════════════════════════"
  peko_iso_run send probe \
      "Use the session tool with action=status on session_key='$paged_id', then action=history on the same session_key (include_tools=true). Tell me: (a) the total message count, and (b) whether you can see the helper's original task in the history — it mentions the command 'yes | head -c 5000'. Answer both explicitly." \
      --no-stream
  peko_iso_assert_rc_zero
  echo "$_peko_iso_capture_out"
  local raw="$_peko_iso_capture_out"
  if echo "$raw" | grep -qiE "not found|no such session|does not exist|couldn't find|cannot find"; then
    echo "  ❌ paged session no longer resolves through the session tool"
    peko_iso_done 1
    return 1
  fi
  echo "  ✓ status/history on the paged session did not error"
  if echo "$raw" | grep -qi "head -c 5000"; then
    echo "  ✓ history shows the pre-paging task text (pages stitched)"
  else
    echo "  ❌ model could not see the pre-paging task in history — stitching broken?"
    peko_iso_done 1
    return 1
  fi

  echo
  echo "✅ ALL ASSERTIONS GREEN — chapters gone, stable-id paging works"
  peko_iso_done 0
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  source "$(dirname "$0")/../lib/isolate.sh"
  flow_main "$@"
fi
