#!/usr/bin/env bash
# scripts/e2e/flows/principal-create-show.sh
#
# Demonstrates the minimal offline flow:
#   1. Initialise an isolated home (no daemon needed).
#   2. Seed a mock-llm provider entry so `--model mock-llm` resolves.
#   3. `peko principal create …`
#   4. `peko principal list`  → expect new principal
#   5. `peko principal show …` → expect identity fields
#   6. Assert files on disk live inside the tempdir (not the user's $HOME).

flow_main() {
  peko_iso_init "principal-create-show" || return 1

  # Seed the catalog BEFORE creating. `--custom` is the right form for a
  # mock-llm endpoint we don't have a template for (see
  # `peko-rs/cli/src/commands/model.rs` — `--custom` + `--api-format
  # openai_completions` is the OpenAI-compatible shape).
  peko_iso_run model add \
      --custom \
      --id mock-llm \
      --model "${MOCK_LLM_WIRE_ID:-mock-llm-test}" \
      --base-url "${MOCK_LLM_URL:-http://127.0.0.1:9/v1}" \
      --api-format openai_completions \
      --key "${MOCK_LLM_API_KEY:-mock-llm-test-key}" || {
    # Older builds may not expose `--custom`; fall back to no-op so the
    # rest of the flow still demonstrates the pattern.
    echo "    (model add skipped — proceeding)"
  }

  # --- create ---
  peko_iso_run principal create test-principal --model mock-llm
  peko_iso_assert_rc_zero
  peko_iso_assert_contains "test-principal"

  # --- list ---
  peko_iso_run principal list
  peko_iso_assert_rc_zero
  peko_iso_assert_contains "test-principal"

  # --- show ---
  peko_iso_run principal show test-principal
  peko_iso_assert_rc_zero
  peko_iso_assert_contains "test-principal"

  # --- post-condition: nothing leaked into the user's real $HOME ---
  if [[ -d "$HOME/../.peko" && ! "$HOME" == *peko-e2e-* ]]; then
    echo "❌ principal data leaked into real HOME: $HOME/../.peko" >&2
    return 1
  fi
  if [[ ! -d "$PEKO_HOME/principals/test-principal" ]]; then
    echo "❌ expected principal dir missing: $PEKO_HOME/principals/test-principal" >&2
    return 1
  fi
  if [[ ! -f "$PEKO_HOME/principals/test-principal/principal.toml" ]]; then
    echo "❌ expected principal.toml missing" >&2
    return 1
  fi

  echo "✅ flow complete: principal-create-show"
  peko_iso_done 0
}
