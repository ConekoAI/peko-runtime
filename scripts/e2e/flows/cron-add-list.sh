#!/usr/bin/env bash
# scripts/e2e/flows/cron-add-list.sh
#
# Exercises the daemon-backed path:
#   1. Initialise an isolated home (daemon NOT auto-started).
#   2. Seed mock-llm provider + create a principal (filesystem-only ops).
#   3. Start the daemon in the background (now that the principal exists
#      on disk for the daemon to load at boot — peko has no runtime
#      principal-reload verb today, so the ordering matters).
#   4. `peko cron add`     → IPC to daemon
#   5. `peko cron list`    → IPC to daemon
#   6. `peko cron history` → IPC to daemon
#   7. Assert schedule.toml landed under the tempdir.

flow_main() {
  # NO_DAEMON by default; we start it explicitly after seeding the principal.
  peko_iso_init "cron-add-list" || return 1

  # --- seed mock-llm provider ---
  peko_iso_run model add \
      --custom \
      --id mock-llm \
      --model "${MOCK_LLM_WIRE_ID:-mock-llm-test}" \
      --base-url "${MOCK_LLM_URL:-http://127.0.0.1:9/v1}" \
      --api-format openai_completions \
      --key "${MOCK_LLM_API_KEY:-mock-llm-test-key}" || true

  # --- seed principal so the daemon can load it at startup ---
  peko_iso_run principal create cron-principal --model mock-llm
  peko_iso_assert_rc_zero
  peko_iso_assert_contains "cron-principal"

  # --- now start the daemon ---
  peko_iso_start_daemon || return 1

  # --- cron add (daemon IPC) ---
  peko_iso_run cron add \
      --principal cron-principal \
      --name "smoke-test-job" \
      --schedule "0 9 * * *" \
      --message "echo hello"
  peko_iso_assert_rc_zero

  # --- cron list (daemon IPC) ---
  peko_iso_run cron list --principal cron-principal
  peko_iso_assert_rc_zero
  peko_iso_assert_contains "smoke-test-job"

  # --- cron history (daemon IPC) — takes a positional JOB_ID. The CLI's
  # `peko cron add` JSON output includes the new job id; for a smoke test
  # we accept any rc and just verify the path on disk instead of the
  # history shape (the daemon hasn't actually run the job in 30s).
  peko_iso_run cron history 1 || true

  # --- post-condition: per-principal cron dir on disk ---
  local cron_dir="$PEKO_DATA_DIR/principals/cron-principal/local/cron"
  if [[ ! -d "$cron_dir" ]]; then
    echo "❌ cron dir missing: $cron_dir" >&2
    return 1
  fi
  if [[ ! -f "$cron_dir/schedule.toml" ]]; then
    echo "❌ schedule.toml missing — cron add did not persist" >&2
    return 1
  fi

  # --- post-condition: nothing leaked into the user's real $HOME ---
  if [[ -d "$HOME/../.peko" && ! "$HOME" == *peko* ]]; then
    echo "❌ principal data leaked into real HOME" >&2
    return 1
  fi

  echo "✅ flow complete: cron-add-list"
  peko_iso_done 0
}
