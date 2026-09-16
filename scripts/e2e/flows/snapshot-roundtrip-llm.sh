#!/usr/bin/env bash
# scripts/e2e/flows/snapshot-roundtrip-llm.sh
#
# ADR-056 full-existence snapshot, END-TO-END with a REAL LLM (MiniMax).
# Requires MINIMAX_API_KEY in the environment.
#
# Verifies that `peko export` → `peko remove` →
# `peko import` is cryogenic TRANSPORT of a LIVE principal —
# not cloning (there is exactly one export shape; cloning is
# `principal create -f` with a fresh DID):
#   1. `principal create -f` runs the real genesis turn (ADR-054);
#   2. authored state is accumulated the way the trunk would:
#      a cron job via the CronCreate tool surface, a workspace skill,
#      a kb note, a plan file — plus a hand-stamp to `organized`
#      (files are the contract, ADR-050/054);
#   3. the snapshot archive carries sessions, cron, plans, tooling,
#      and kb — and no derived state (cache/locks/memory_index);
#   4. after remove + import: sessions, the cron schedule (with
#      principal-id rebinding — the package's jobs carry a STALE id),
#      plans, skill, and kb land in their tier directories;
#      boot_state = organized survives verbatim;
#   5. daemon reboot: no genesis re-seed (job set unchanged, no
#      `genesis` job) and the imported principal round-trips a real
#      `peko send` through the model.
#
# Run:  MINIMAX_API_KEY=... scripts/e2e/run-case.sh snapshot-roundtrip-llm
#       (set KEEP_TEMPDIR=1 to keep the artifacts for inspection)

flow_main() {
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "❌ MINIMAX_API_KEY must be set for this flow" >&2
    return 1
  fi

  peko_iso_init "snapshot-roundtrip-llm" || return 1
  local peko_dir="$_PEKO_ISO_PEKO_DIR"
  local principal="snap-e2e"
  local shared_dir="$peko_dir/principals/$principal"
  local local_dir="$peko_dir/data/principals/$principal/local"
  local toml_file="$shared_dir/principal.toml"
  local schedule="$local_dir/cron/schedule.toml"
  local sessions="$local_dir/sessions"
  local tempdir="$_PEKO_ISO_TEMPDIR"

  # ── model: real MiniMax via template ─────────────────────────────
  peko_iso_run model add \
    --template minimax \
    --id minimax-m3 \
    --model MiniMax-M3 \
    --key "$MINIMAX_API_KEY"
  peko_iso_assert_rc_zero

  # ── ONE command: blocking create → alive (real genesis turn) ─────
  local template="$tempdir/snap-e2e.template.toml"
  cat >"$template" <<TOML
name = "snap-e2e"
preferred_model_id = "minimax-m3"
persona = """
You are Snap E2E, a compact probe principal. Be terse and precise.
"""

[identity]
display_name = "Snap E2E"
description = "ADR-056 full-existence snapshot probe"

[intent]
goals = ["Complete its genesis turn successfully"]
preferences = ["Keep replies short"]
TOML

  echo "⏳ blocking create (includes the genesis turn; MiniMax latency applies)…"
  peko_iso_run principal create "$principal" -f "$template" --wait-timeout 420
  peko_iso_assert_rc_zero
  peko_iso_assert_contains "is alive"
  echo "✅ principal created and alive (genesis turn done)"

  # ── author state the way the trunk would ─────────────────────────
  # 1. A trunk-authored cron job via the REAL CronCreate tool surface —
  #    genuine self-organization, not a hand-seeded row.
  peko_iso_run send "$principal" \
    "Use your CronCreate tool to create a RECURRING job: label 'snapshot-authored-job', cron expression '0 9 * * *', message 'authored by the trunk before the snapshot'. Create nothing else. When the tool call succeeds, reply with exactly: job-created"
  peko_iso_assert_rc_zero

  if grep -q 'snapshot-authored-job' "$schedule"; then
    echo "✅ trunk authored a cron job via CronCreate (real self-organization)"
  else
    # Fallback: clone the seeded keepalive job (guaranteed-valid
    # CronJob shape) so the packaging assertions stay deterministic
    # even if the model skipped the tool call.
    echo "    (trunk did not author the job via CronCreate; cloning the keepalive job instead)"
    python3 - "$schedule" <<'PY' || return 1
import json, sys
path = sys.argv[1]
db = json.load(open(path))
ka = next(j for j in db["jobs"] if j["id"] == "keepalive")
job = json.loads(json.dumps(ka))
job["id"] = "snapshot-authored-job"
job["name"] = "snapshot-authored-job"
db["jobs"].append(job)
json.dump(db, open(path, "w"), indent=2)
PY
    grep -q 'snapshot-authored-job' "$schedule" || {
      echo "❌ fallback job planting failed" >&2
      return 1
    }
    echo "✅ authored cron job planted (fallback path)"
  fi

  # 2. A workspace skill (Shared tier tooling).
  mkdir -p "$shared_dir/skills/e2e-skill"
  cat >"$shared_dir/skills/e2e-skill/SKILL.md" <<'MD'
---
name: e2e-skill
description: Probe skill planted before the ADR-056 snapshot export.
---

# E2E Skill

If you can read this, the skill layer survived the snapshot.
MD
  # 3. A kb note (ADR-055 hot set).
  printf '\nsnapshot marker: KB-MEM-042\n' >>"$shared_dir/kb/MEMORY.md"
  # 4. A plan file (Local tier, authored).
  mkdir -p "$local_dir/plans"
  printf '{"plan_id":"snap-plan","step":"exists"}\n' >"$local_dir/plans/snap-plan.jsonl"
  # 5. Hand-stamp organized (monotone advance; simulates P4 — the
  #    trunk owns its rhythm and the runtime stops touching its
  #    schedule at boot).
  if [[ "$(uname)" == "Darwin" ]]; then
    sed -i '' 's/boot_state = "genesis_pending"/boot_state = "organized"/' "$toml_file"
  else
    sed -i 's/boot_state = "genesis_pending"/boot_state = "organized"/' "$toml_file"
  fi
  grep -q 'boot_state = "organized"' "$toml_file" || {
    echo "❌ failed to stamp boot_state=organized before export" >&2
    grep boot_state "$toml_file" >&2
    return 1
  }
  echo "✅ authored state planted (skill, kb note, plan, organized stamp)"

  # Remember the principal's runtime id — the imported config carries
  # it verbatim, so after import the schedule must be rebound TO it.
  local principal_id
  principal_id="$(grep -E '^id = ' "$toml_file" | head -1 | sed -E 's/^id = "//; s/"$//')"
  [[ -n "$principal_id" ]] || {
    echo "❌ could not read the principal runtime id from $toml_file" >&2
    return 1
  }
  echo "    runtime id: $principal_id"

  # ── sabotage the schedule's ids with a STALE value ───────────────
  # This simulates the rebinding scenario (source ids that no longer
  # match the imported principal's effective id — e.g. after key
  # rotation or a host whose id scheme changed). The import must
  # rebind every job back to the effective runtime id.
  if [[ "$(uname)" == "Darwin" ]]; then
    sed -i '' "s/\"principal_id\": \"[^\"]*\"/\"principal_id\": \"prin_stale_snapshot\"/g" "$schedule"
  else
    sed -i "s/\"principal_id\": \"[^\"]*\"/\"principal_id\": \"prin_stale_snapshot\"/g" "$schedule"
  fi
  grep -q 'prin_stale_snapshot' "$schedule" || {
    echo "❌ failed to plant stale principal ids in $schedule" >&2
    cat "$schedule" >&2
    return 1
  }
  echo "✅ stale principal ids planted (rebinding will be exercised)"

  # ── reboot so the daemon re-reads the edited files ───────────────
  # The running daemon caches the config in memory; the hand-stamped
  # organized state and the edited schedule only exist on disk until
  # it reloads. This reboot doubles as the D4 assertion: an organized
  # principal's schedule must survive boot untouched (no re-seed).
  peko_iso_run daemon stop
  peko_iso_assert_rc_zero
  peko_iso_start_daemon || return 1
  grep -q 'boot_state = "organized"' "$toml_file" || {
    echo "❌ daemon rewrote boot_state on boot (organized must be sticky)" >&2
    grep boot_state "$toml_file" >&2
    return 1
  }
  local job_count
  job_count="$(python3 -c '
import json, sys
d = json.load(open(sys.argv[1]))
print(len(d["jobs"]))
' "$schedule" 2>/dev/null || echo 0)"
  if [[ "$job_count" == "2" ]]; then
    echo "✅ organized reboot re-seeded nothing (exactly 2 jobs, untouched)"
  else
    echo "❌ expected exactly 2 jobs after organized reboot, found $job_count" >&2
    cat "$schedule" >&2
    return 1
  fi

  # ── export the snapshot and inspect the archive ──────────────────
  local full_pkg="$tempdir/snap-full.peko"

  peko_iso_run principal export "$principal" -o "$full_pkg"
  peko_iso_assert_rc_zero

  # Full snapshot: all of it, and no declared export mode on the
  # manifest (there is exactly one shape).
  for layer in 'sessions/' 'cron/schedule.toml' 'plans/' 'skills/e2e-skill/' 'kb/MEMORY.md'; do
    if ! tar -tzf "$full_pkg" | grep -q "^$layer"; then
      echo "❌ snapshot export missing layer: $layer" >&2
      tar -tzf "$full_pkg" >&2
      return 1
    fi
  done
  tar -xzOf "$full_pkg" manifest.toml | grep -q 'export_mode' && {
    echo "❌ manifest must not declare an export mode (single shape)" >&2
    tar -xzOf "$full_pkg" manifest.toml >&2
    return 1
  }
  # Derived state never travels.
  if tar -tzf "$full_pkg" | grep -Eq '^(cache/|locks/|.*memory_index\.json)'; then
    echo "❌ full-snapshot export must not carry derived state (cache/locks/memory_index)" >&2
    tar -tzf "$full_pkg" | grep -E '^(cache/|locks/|.*memory_index\.json)' >&2
    return 1
  fi
  echo "✅ snapshot export carries sessions+cron+plans+tooling+kb, no derived state"

  # ── remove the principal entirely ────────────────────────────────
  peko_iso_run principal remove "$principal" --yes
  peko_iso_assert_rc_zero
  if [[ -d "$shared_dir" || -d "$local_dir" ]]; then
    echo "❌ remove left data behind: $shared_dir / $local_dir" >&2
    return 1
  fi
  echo "✅ principal removed (all data gone)"

  # ── import the snapshot ──────────────────────────────────────────
  peko_iso_run principal import "$full_pkg" --yes
  peko_iso_assert_rc_zero
  echo "✅ snapshot import accepted"

  # ── file-level post-conditions (daemon not yet restarted) ────────
  local imp_toml="$shared_dir/principal.toml"
  [[ -f "$imp_toml" ]] || { echo "❌ imported config missing: $imp_toml" >&2; return 1; }

  grep -q 'boot_state = "organized"' "$imp_toml" || {
    echo "❌ organized boot state did not survive the snapshot (no genesis re-seed would be abstained)" >&2
    grep boot_state "$imp_toml" >&2
    return 1
  }
  echo "✅ boot_state = organized carried verbatim"

  local imp_id
  imp_id="$(grep -E '^id = ' "$imp_toml" | head -1 | sed -E 's/^id = "//; s/"$//')"
  if [[ "$imp_id" != "$principal_id" ]]; then
    echo "❌ imported runtime id changed: was $principal_id, now $imp_id" >&2
    return 1
  fi
  echo "✅ runtime id preserved ($imp_id)"

  # Sessions restored with the real genesis turn.
  local trunk_jsonl
  trunk_jsonl="$(grep -l '\[genesis\]' "$sessions"/*.jsonl 2>/dev/null | head -1 || true)"
  if [[ -n "$trunk_jsonl" ]] && grep -q '"role":"assistant"' "$trunk_jsonl"; then
    echo "✅ sessions restored (genesis brief + real assistant turn)"
  else
    echo "❌ sessions not restored into $sessions" >&2
    ls -la "$sessions" 2>&2 || true
    return 1
  fi

  # Cron schedule restored AND rebound away from the stale ids.
  grep -q 'snapshot-authored-job' "$schedule" || {
    echo "❌ authored cron job missing from the restored schedule" >&2
    cat "$schedule" >&2
    return 1
  }
  grep -q 'keepalive' "$schedule" || {
    echo "❌ keepalive job missing from the restored schedule" >&2
    cat "$schedule" >&2
    return 1
  }
  if grep -q 'prin_stale_snapshot' "$schedule"; then
    echo "❌ stale principal ids survived the import (rebinding did not run)" >&2
    cat "$schedule" >&2
    return 1
  fi
  local stale_count
  stale_count="$(grep -c "\"principal_id\": \"$principal_id\"" "$schedule" || true)"
  if [[ "${stale_count:-0}" -ge 2 ]]; then
    echo "✅ cron schedule restored and rebound to the effective runtime id ($stale_count jobs)"
  else
    echo "❌ expected ≥2 jobs rebound to $principal_id, found $stale_count" >&2
    cat "$schedule" >&2
    return 1
  fi

  # Plans, skill, kb restored to their tier directories.
  [[ -f "$local_dir/plans/snap-plan.jsonl" ]] || {
    echo "❌ plan file not restored" >&2
    return 1
  }
  grep -q 'snapshot marker: KB-MEM-042' "$shared_dir/kb/MEMORY.md" || {
    echo "❌ kb note not restored" >&2
    return 1
  }
  grep -q 'survived the snapshot' "$shared_dir/skills/e2e-skill/SKILL.md" || {
    echo "❌ skill not restored" >&2
    return 1
  }
  echo "✅ plans + skill + kb restored to their tier directories"

  # ── reboot: organized → the runtime abstains from re-seeding ─────
  peko_iso_run daemon stop
  peko_iso_assert_rc_zero
  peko_iso_start_daemon || return 1

  # (No `peko cron` CLI tree — the schedule file IS the surface.)
  if grep -q 'snapshot-authored-job' "$schedule" && grep -q 'keepalive' "$schedule" \
    && ! grep -q '"id": "genesis"' "$schedule"; then
    echo "✅ reboot re-seeded nothing (organized schedule intact, no genesis job)"
  else
    echo "❌ unexpected schedule after reboot (re-seed leaked?)" >&2
    cat "$schedule" >&2
    return 1
  fi

  # ── the imported principal is ALIVE: real-LLM round-trip ─────────
  peko_iso_run send "$principal" "Reply with exactly the word: reimported-ok"
  peko_iso_assert_rc_zero
  echo "✅ imported principal round-trips a real send"

  peko_iso_run daemon stop
  peko_iso_assert_rc_zero

  echo ""
  echo "🎉 ADR-056 full-existence snapshot e2e (real LLM): all phases verified"
}
