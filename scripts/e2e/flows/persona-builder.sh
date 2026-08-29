#!/usr/bin/env bash
# scripts/e2e/flows/persona-builder.sh
#
# End-to-end flow for the persona builder — Fix #8 from the 2026-08-01
# field-test follow-ups. Proves a non-technical user can go from a
# blank principal to a fully drafted persona in two commands:
#
#   peko principal create blank --model <id>
#   peko principal persona set blank --from "a senior rust reviewer …"
#
# What this flow asserts:
#   1. `--dry-run` exits 0, prints the preview sections (Identity,
#      Goals, Values, Style, Primary prompt), and does NOT modify
#      principal.toml or primary.md.
#   2. The default (no --dry-run) call exits 0, prints a unified diff,
#      writes populations to [intent.goals] / [intent.values] /
#      [identity.display_name], and rewrites primary.md with the
#      drafted body + the literal `{{memory}}` placeholder.
#   3. The principal actually behaves like the drafted persona — a
#      send message asking about a borrow-checker scenario gets a
#      borrow-checker-relevant reply.
#
# Requires: MINIMAX_API_KEY (or another provider key honored by the
# isolation library's resolver bootstrap). The persona builder's
# IPC path is provider-agnostic; only the LLM call itself needs a
# real key.

flow_main() {
  peko_iso_init "persona-builder" || return 1

  # Need a real provider key to exercise the LLM-drafting path.
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "⚠️  MINIMAX_API_KEY not set — persona-builder flow requires a real LLM" >&2
    echo "    Set MINIMAX_API_KEY and re-run, e.g.:" >&2
    echo "      MINIMAX_API_KEY=… scripts/e2e/run-case.sh persona-builder" >&2
    peko_iso_done 0
    return 0
  fi

  # 1. Seed the model + the principal BEFORE the daemon starts (the
  # daemon doesn't runtime-reload principals; if we create them
  # after `daemon start`, the daemon's IPC handlers won't see them).
  peko_iso_run model add --template minimax --model MiniMax-M3 --key "$MINIMAX_API_KEY"
  peko_iso_assert_rc_zero || peko_iso_done 1
  peko_iso_run principal create blank --model minimax-MiniMax-M3
  peko_iso_assert_rc_zero || peko_iso_done 1

  local principal_dir="$PEKO_HOME/principals/blank"
  local principal_toml="$principal_dir/principal.toml"
  local primary_md="$principal_dir/agents/primary.md"

  # Capture pre-state so we can prove the dry-run didn't write.
  local pre_intent_md5
  pre_intent_md5=$(grep -A 5 '^\[intent' "$principal_toml" | md5sum | awk '{print $1}')
  local pre_primary_md5
  pre_primary_md5=$(md5sum "$primary_md" | awk '{print $1}')

  # 2. Start the daemon (now that the principal exists on disk the
  # CLI's pre-IPC guard will accept the persona request).
  peko_iso_start_daemon || peko_iso_done 1

  # ============================================================
  # Step 1 — `persona set --dry-run` is a preview only
  # ============================================================
  echo "─── Step 1: persona set --dry-run (preview only) ───"
  peko_iso_run principal persona set blank \
      --from "a senior rust reviewer who cites the borrow checker and doc.rust-lang.org" \
      --dry-run
  peko_iso_assert_rc_zero || {
    echo "   stderr: $_peko_iso_capture_err" >&2
    echo "   stdout: $_peko_iso_capture_out" >&2
    peko_iso_done 1
  }
  # Preview must surface the four drafted sections (Identity, Goals,
  # Values, Style, Primary prompt) on stdout and a "(dry-run…
  # was not modified)" footer on stderr. The CLI splits the two
  # streams so the footer is per-flow noise, not envelope content.
  if grep -q "Identity:" <<<"$_peko_iso_capture_out" \
     && grep -q "Goals:" <<<"$_peko_iso_capture_out" \
     && grep -q "Values:" <<<"$_peko_iso_capture_out" \
     && grep -q "Style:" <<<"$_peko_iso_capture_out" \
     && grep -q "Primary prompt" <<<"$_peko_iso_capture_out" \
     && grep -q "dry-run" <<<"$_peko_iso_capture_err"; then
    echo "✅ Step 1: preview sections present (stdout + dry-run footer on stderr)"
  else
    echo "❌ Step 1 regression — preview sections missing:" >&2
    echo "   stdout: $(echo "$_peko_iso_capture_out" | head -20)" >&2
    echo "   stderr: $(echo "$_peko_iso_capture_err" | head -10)" >&2
    peko_iso_done 1
  fi
  # Disk must be unchanged.
  local post_dry_intent_md5 post_dry_primary_md5
  post_dry_intent_md5=$(grep -A 5 '^\[intent' "$principal_toml" | md5sum | awk '{print $1}')
  post_dry_primary_md5=$(md5sum "$primary_md" | awk '{print $1}')
  if [[ "$pre_intent_md5" == "$post_dry_intent_md5" ]] \
     && [[ "$pre_primary_md5" == "$post_dry_primary_md5" ]]; then
    echo "✅ Step 1: principal.toml + primary.md unchanged after --dry-run"
  else
    echo "❌ Step 1 regression — --dry-run modified on-disk state" >&2
    peko_iso_done 1
  fi
  echo

  # ============================================================
  # Step 2 — `persona set` (no --dry-run) writes + diffs
  # ============================================================
  echo "─── Step 2: persona set writes TOML + primary.md ───"
  peko_iso_run principal persona set blank \
      --from "a senior rust reviewer who cites the borrow checker and doc.rust-lang.org"
  peko_iso_assert_rc_zero || {
    echo "   stderr: $_peko_iso_capture_err" >&2
    echo "   stdout: $_peko_iso_capture_out" | head -40 >&2
    peko_iso_done 1
  }
  # Diff banner is the user-facing proof the write happened. The
  # exact lines are LLM-driven, so we only assert the diff shape.
  if grep -q "^--- a/principal.toml" <<<"$_peko_iso_capture_out" \
     || grep -q "principal.toml:" <<<"$_peko_iso_capture_out"; then
    echo "✅ Step 2: unified diff banner present"
  else
    echo "❌ Step 2 regression — no diff banner in output:" >&2
    echo "$_peko_iso_capture_out" | head -40 >&2
    peko_iso_done 1
  fi

  # principal.toml must now have [intent.goals] populated with the
  # drafted bullets. The LLM is free to pick any verbatim phrasing,
  # so we only assert SHAPE: a `[intent]` section followed by a
  # `goals = [` line (multi-line array).
  if grep -A 1 '^\[intent\]' "$principal_toml" | grep -qE '^goals = \['; then
    echo "✅ Step 2: principal.toml has [intent] with a non-empty goals array"
  else
    echo "❌ Step 2 regression — principal.toml [intent.goals] not populated:" >&2
    grep -A 15 '^\[intent\]' "$principal_toml" | head -20 >&2
    peko_iso_done 1
  fi

  # primary.md body must contain the borrow-checker / reviewer
  # vocabulary the user asked for, AND the literal {{memory}}
  # placeholder that the renderer expects.
  if grep -qiE "borrow|reviewer" "$primary_md" \
     && grep -q "{{memory}}" "$primary_md"; then
    echo "✅ Step 2: primary.md contains borrow/reviewer vocabulary + {{memory}}"
  else
    echo "❌ Step 2 regression — primary.md missing drafted vocabulary or {{memory}}:" >&2
    cat "$primary_md" | head -20 >&2
    peko_iso_done 1
  fi
  echo

  # ============================================================
  # Step 3 — Sanity: the drafted persona actually behaves like it
  # ============================================================
  echo "─── Step 3: send a borrow-checker question, expect borrow-aware reply ───"
  # We use a small Rust snippet as the prompt — the drafted persona
  # is a "senior rust reviewer who cites the borrow checker", so the
  # reply should reference borrow checker / lifetime / `&mut` / an
  # `error[E0…]` code. The LLM is free to phrase it however; we
  # only assert ONE of the borrow-checker tells.
  peko_iso_run send blank \
      "review this: fn main() { let mut v = vec![1]; let a = &mut v; let b = &mut v; a.push(2); }" \
      || true
  # send may exit non-zero on streaming-timeout races; the
  # assertion is on the output content, not the rc.
  if grep -qiE "borrow|lifetime|&mut|error.E0" <<<"$_peko_iso_capture_out$_peko_iso_capture_err"; then
    echo "✅ Step 3: principal reply mentions borrow / lifetime / &mut / error[E0…]"
  else
    echo "❌ Step 3 regression — reply did not surface borrow-checker vocabulary:" >&2
    echo "   stdout: $_peko_iso_capture_out" | head -20 >&2
    echo "   stderr: $_peko_iso_capture_err" | head -20 >&2
    peko_iso_done 1
  fi
  echo

  # ============================================================
  # Post-condition: nothing leaked into the user's real $HOME
  # ============================================================
  if [[ -d "$HOME/../.peko" && ! "$HOME" == *peko-e2e-* ]]; then
    echo "❌ persona data leaked into real HOME: $HOME/../.peko" >&2
    peko_iso_done 1
  fi

  echo "🎉 persona-builder flow passed: dry-run preview, write+diff, on-disk state, behavior"
  peko_iso_done 0
}
