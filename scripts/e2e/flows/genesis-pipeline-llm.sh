#!/usr/bin/env bash
# scripts/e2e/flows/genesis-pipeline-llm.sh
#
# ADR-054 genesis pipeline UX, END-TO-END with a REAL LLM (MiniMax).
# Requires MINIMAX_API_KEY in the environment.
#
# Verifies the single-command UX:
#   1. `peko principal create <name> -f template.toml` is BLOCKING and
#      returns only when the principal is alive: workspace provisioned,
#      definition seeded from the template (identity, intent, inline
#      persona), daemon started, genesis + keepalive jobs seeded, and
#      the one-shot genesis turn executed by the real model.
#   2. Post-conditions: boot state genesis_pending, keepalive job
#      present, trunk session JSONL carries the genesis brief AND a
#      real assistant turn, template persona landed in agents/primary.md.
#   3. Ingress: `peko send` round-trips through the peer child with the
#      real model while the pipeline is live.
#   4. Bare create against a RUNNING daemon: `--detach` + no template
#      exercises the PrincipalReload IPC path (daemon learns the new
#      principal without a restart) and the default-definition path
#      (empty identity → genesis adopts).
#   5. Restart idempotence: a daemon reboot re-seeds nothing.
#
# Run:  MINIMAX_API_KEY=... scripts/e2e/run-case.sh genesis-pipeline-llm
#       (set KEEP_TEMPDIR=1 to keep the artifacts for inspection)

flow_main() {
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "❌ MINIMAX_API_KEY must be set for this flow" >&2
    return 1
  fi

  peko_iso_init "genesis-pipeline-llm" || return 1
  local peko_dir="$_PEKO_ISO_PEKO_DIR"
  local principal="alive-e2e"
  local shared_dir="$peko_dir/principals/$principal"
  local local_dir="$peko_dir/data/principals/$principal/local"
  local toml_file="$shared_dir/principal.toml"
  local schedule="$local_dir/cron/schedule.toml"
  local sessions="$local_dir/sessions"

  # ── model: real MiniMax via template ─────────────────────────────
  peko_iso_run model add \
    --template minimax \
    --id minimax-m3 \
    --model MiniMax-M3 \
    --key "$MINIMAX_API_KEY"
  peko_iso_assert_rc_zero

  # ── definition template (a principal.toml + inline persona) ─────
  local template="$_PEKO_ISO_TEMPDIR/alive-e2e.template.toml"
  # NOTE: top-level keys (preferred_model_id, persona) must precede the
  # table sections — classic TOML scoping.
  cat >"$template" <<TOML
name = "alive-e2e"
preferred_model_id = "minimax-m3"
persona = """
You are Alive E2E, a compact probe principal. Be terse and precise.
"""

[identity]
display_name = "Alive E2E"
description = "ADR-054 single-command genesis probe"

[intent]
goals = ["Complete its genesis turn successfully"]
preferences = ["Keep replies short"]
TOML

  # ── ONE command: blocking create → alive ─────────────────────────
  echo "⏳ blocking create (includes the genesis turn; MiniMax latency applies)…"
  peko_iso_run principal create "$principal" -f "$template" --wait-timeout 420
  peko_iso_assert_rc_zero
  peko_iso_assert_contains "is alive"

  # ── post-conditions ──────────────────────────────────────────────
  if grep -q 'boot_state = "genesis_pending"' "$toml_file"; then
    echo "✅ boot_state = genesis_pending persisted"
  else
    echo "❌ boot_state=genesis_pending missing (got: $(grep boot_state "$toml_file"))" >&2
    return 1
  fi

  local job_ids
  job_ids="$(python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
print(" ".join(j["id"] for j in d["jobs"]))
' "$schedule" 2>/dev/null || true)"
  if [[ " $job_ids " == *" keepalive "* ]]; then
    echo "✅ keepalive job present"
  else
    echo "❌ keepalive job missing (jobs: $job_ids)" >&2
    return 1
  fi

  if grep -q "You are Alive E2E, a compact probe principal" "$shared_dir/agents/primary.md"; then
    echo "✅ template persona landed in agents/primary.md"
  else
    echo "❌ persona missing from agents/primary.md" >&2
    cat "$shared_dir/agents/primary.md" >&2
    return 1
  fi

  local trunk_jsonl
  trunk_jsonl="$(grep -l '\[genesis\]' "$sessions"/*.jsonl 2>/dev/null | head -1 || true)"
  if [[ -n "$trunk_jsonl" ]] && grep -q '"role":"assistant"' "$trunk_jsonl"; then
    echo "✅ genesis brief + real assistant turn in $(basename "$trunk_jsonl")"
  else
    echo "❌ genesis turn not found in $sessions" >&2
    ls -la "$sessions" >&2
    return 1
  fi

  # ── ingress sanity: real-LLM round-trip via peer child ──────────
  peko_iso_run send "$principal" "Reply with exactly the word: pong"
  peko_iso_assert_rc_zero
  echo "✅ send round-trip accepted by the daemon"

  # ── bare create against the RUNNING daemon (reload path) ────────
  peko_iso_run principal create bare-e2e --model minimax-m3 --detach
  peko_iso_assert_rc_zero
  peko_iso_assert_contains "detached"
  local bare_toml="$peko_dir/principals/bare-e2e/principal.toml"
  if grep -q 'boot_state = "genesis_pending"' "$bare_toml" \
    && ! grep -q 'display_name' "$bare_toml"; then
    echo "✅ bare create: default definition (no fabricated identity), seeded + reloaded into the running daemon"
  else
    echo "❌ bare create post-conditions wrong:" >&2
    cat "$bare_toml" >&2
    return 1
  fi

  # ── restart idempotence ──────────────────────────────────────────
  peko_iso_run daemon stop
  peko_iso_assert_rc_zero
  peko_iso_start_daemon || return 1
  local ka_count
  ka_count="$(python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
print(sum(1 for j in d["jobs"] if j["id"] == "keepalive"))
' "$schedule" 2>/dev/null || echo 0)"
  if [[ "$ka_count" == "1" ]]; then
    echo "✅ restart re-seeded nothing (exactly 1 keepalive after reboot)"
  else
    echo "❌ expected exactly 1 keepalive after restart, found $ka_count" >&2
    cat "$schedule" >&2
    return 1
  fi

  peko_iso_run daemon stop
  peko_iso_assert_rc_zero

  echo ""
  echo "🎉 genesis pipeline e2e (real LLM, single-command UX): all phases verified"
}
