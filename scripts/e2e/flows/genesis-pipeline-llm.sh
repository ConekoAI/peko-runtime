#!/usr/bin/env bash
# scripts/e2e/flows/genesis-pipeline-llm.sh
#
# ADR-054 genesis pipeline, END-TO-END with a REAL LLM (MiniMax).
# Requires MINIMAX_API_KEY in the environment.
#
# Verifies the whole procedure the ADR structuralizes:
#   P0  `peko principal create` stamps `boot_state = provisioned`
#       (bare create: no fabricated identity).
#   P1  `peko principal define` records identity/intent and stamps
#       `boot_state = defined`.
#   P2  daemon boot seeds the genesis + keepalive cron jobs; the
#       one-shot genesis turn FIRES ~60s later and the trunk session
#       JSONL shows a real LLM self-turn (user brief + assistant
#       reply + tool activity if the model chooses any).
#   P4  a second daemon boot re-seeds nothing (idempotence: still
#       exactly 2 jobs) and the state stays `genesis_pending` (the
#       engine-side `organized` flip is ADR-054 §5 deferred work).
#   Ingress sanity: `peko send` round-trips through the per-peer
#       child with the real model while the pipeline is live.
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
  local principal="genesis-e2e"
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

  # ── P0: bare create stamps provisioned ───────────────────────────
  peko_iso_run principal create "$principal" --model minimax-m3
  peko_iso_assert_rc_zero
  peko_iso_assert_contains "Boot state: Provisioned"
  if grep -q 'boot_state = "provisioned"' "$toml_file"; then
    echo "✅ P0: boot_state = provisioned persisted"
  else
    echo "❌ P0: boot_state=provisioned missing from $toml_file" >&2
    cat "$toml_file" >&2
    return 1
  fi

  # ── P1: definition stamps defined ────────────────────────────────
  peko_iso_run principal define "$principal" \
    --display-name "Genesis E2E" \
    --description "ADR-054 genesis pipeline live probe" \
    --goal "Complete its genesis turn successfully" \
    --preference "Keep replies short"
  peko_iso_assert_rc_zero
  peko_iso_assert_contains "boot state: Defined"
  if grep -q 'boot_state = "defined"' "$toml_file"; then
    echo "✅ P1: boot_state = defined persisted"
  else
    echo "❌ P1: boot_state=defined missing from $toml_file" >&2
    return 1
  fi

  # ── P2: daemon boot seeds the pipeline ───────────────────────────
  peko_iso_start_daemon || return 1
  if grep -q 'genesis: ' "$_PEKO_ISO_TEMPDIR/daemon.err" 2>/dev/null; then
    grep 'genesis: ' "$_PEKO_ISO_TEMPDIR/daemon.err" | head -3
    echo "✅ P2: boot seeding logged"
  else
    echo "❌ P2: no genesis seeding line in daemon log" >&2
    tail -n 30 "$_PEKO_ISO_TEMPDIR/daemon.err" >&2
    return 1
  fi

  local job_ids
  job_ids="$(python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
print(" ".join(j["id"] for j in d["jobs"]))
' "$schedule" 2>/dev/null || true)"
  for job in genesis keepalive; do
    if [[ " $job_ids " == *" $job "* ]]; then
      echo "✅ P2: '$job' job present in schedule"
    else
      echo "❌ P2: '$job' job missing from $schedule (jobs: $job_ids)" >&2
      cat "$schedule" 2>/dev/null >&2
      return 1
    fi
  done
  if grep -q 'boot_state = "genesis_pending"' "$toml_file"; then
    echo "✅ P2: boot_state = genesis_pending persisted"
  else
    echo "❌ P2: boot_state=genesis_pending missing (got: $(grep boot_state "$toml_file"))" >&2
    return 1
  fi

  # ── P2: the genesis turn actually fires (real LLM) ──────────────
  echo "⏳ waiting up to 300s for the genesis turn (fires ~60s after boot; MiniMax latency applies)…"
  local deadline=$((SECONDS + 300))
  local trunk_jsonl=""
  while (( SECONDS < deadline )); do
    # The trunk's JSONL is whichever session file contains the genesis
    # brief (session ids are opaque UUIDs; do not hardcode).
    trunk_jsonl="$(grep -l '\[genesis\]' "$sessions"/*.jsonl 2>/dev/null | head -1 || true)"
    if [[ -n "$trunk_jsonl" ]] && grep -q '"role":"assistant"\|"role": "assistant"\|Assistant' "$trunk_jsonl" 2>/dev/null; then
      break
    fi
    sleep 5
  done
  if [[ -z "$trunk_jsonl" ]]; then
    echo "❌ P2: genesis turn never landed in $sessions" >&2
    ls -la "$sessions" >&2
    tail -n 20 "$_PEKO_ISO_TEMPDIR/daemon.err" >&2
    return 1
  fi
  echo "✅ P2: genesis brief recorded in $(basename "$trunk_jsonl")"
  # The trunk must have produced a real assistant turn (LLM replied).
  if grep -q '"role":"assistant"\|"role": "assistant"\|Assistant' "$trunk_jsonl"; then
    echo "✅ P2: trunk produced an assistant (LLM) turn"
  else
    echo "❌ P2: genesis brief present but no assistant turn yet" >&2
    tail -c 2000 "$trunk_jsonl" >&2
    return 1
  fi

  # ── ingress sanity: real-LLM round-trip via peer child ──────────
  peko_iso_run send "$principal" "Reply with exactly the word: pong"
  peko_iso_assert_rc_zero

  # ── P4/idempotence: restart the daemon, nothing re-seeds ────────
  peko_iso_run daemon stop
  peko_iso_assert_rc_zero
  peko_iso_start_daemon || return 1
  # The schedule file is JSON (despite the .toml name): count job
  # records via the jobs array, ignoring the runs log below it.
  local job_count
  job_count="$(python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
print(len(d["jobs"]))
' "$schedule" 2>/dev/null || echo 0)"
  if [[ "$job_count" == "2" ]]; then
    echo "✅ P4: restart re-seeded nothing (2 jobs after reboot)"
  else
    echo "❌ P4: expected 2 jobs after restart, found $job_count" >&2
    cat "$schedule" >&2
    return 1
  fi
  if grep -q 'boot_state = "genesis_pending"' "$toml_file"; then
    echo "✅ P4: boot_state still genesis_pending (engine flip is ADR-054 §5 deferred)"
  else
    echo "ℹ️  P4: boot_state advanced: $(grep boot_state "$toml_file")"
  fi

  peko_iso_run daemon stop
  peko_iso_assert_rc_zero

  echo ""
  echo "🎉 genesis pipeline e2e (real LLM): all phases verified"
}

# Keep the flow file importable by run-case.sh; helpers come from isolate.sh.
