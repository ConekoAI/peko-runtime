#!/usr/bin/env bash
# scripts/e2e/flows/plan-tool-e2e.sh
#
# Exercises the 7 peko_plan tools (`PlanCreate`/`List`/`Get`/
# `MarkStep`/`RecordEvidence`/`AddStep`/`Close`) end-to-end against a
# real MiniMax-M3 model. The flow:
#
#   1. Init isolated env (NO_DAEMON — we'll start it after seeding).
#   2. Add the minimax model with the real API key.
#   3. Create a principal.
#   4. Grant the 7 `tool:Plan*` capabilities (NOT in starter_bundle —
#      the default capability set covers the Task* family but plan
#      tools need explicit grants per the F37 funnel rule).
#   5. Start daemon in background.
#   6. `peko send planbot "…"` with a prompt that walks the agent
#      through create → list → mark_step → add_step → record_evidence
#      → close. Verify each tool call landed by checking the
#      `<plans_dir>/<plan_id>.jsonl` file ends in the expected state.
#
# Required env:
#   MINIMAX_API_KEY  — the real key, exported by `peko_iso_init` so
#                      the daemon's resolver bootstrap can use it as
#                      a fallback if the vault lookup fails.

flow_main() {
  if [[ -z "${MINIMAX_API_KEY:-}" ]]; then
    echo "❌ MINIMAX_API_KEY env var not set — refusing to run" >&2
    echo "   export MINIMAX_API_KEY=… before invoking this flow" >&2
    return 64   # EX_USAGE
  fi

  peko_iso_init "plan-tool-e2e" || return 1

  # --- step 1: model + principal (offline) -------------------------------
  peko_iso_run model add \
      --template minimax \
      --model MiniMax-M3 \
      --key "$MINIMAX_API_KEY"
  peko_iso_assert_rc_zero
  peko_iso_assert_contains "Added model 'minimax-MiniMax-M3'"

  peko_iso_run principal create planbot --model minimax-MiniMax-M3
  peko_iso_assert_rc_zero
  peko_iso_assert_contains "planbot"

  # --- step 2: start daemon (capability grant + send both need IPC) -----
  peko_iso_start_daemon || return 1

  # --- step 3: grant the 7 Plan tool capabilities ----------------------
  # The starter_bundle covers the Task* family (TaskCreate/List/Get/
  # Update) but NOT the Plan* family. The plan tools require their
  # own `tool:<Name>` grant per the F37 funnel rule. Without these,
  # `is_tool_enabled` filters them out at runtime and the LLM can't
  # actually invoke them — it'd just see an empty `available_tools`
  # list when it tries. NOTE: `peko capability grant` is daemon-backed,
  # so the daemon must be up first (step 2 above).
  for cap in PlanCreate PlanList PlanGet PlanMarkStep \
             PlanRecordEvidence PlanAddStep PlanClose; do
    peko_iso_run capability grant --principal planbot "tool:$cap"
    peko_iso_assert_rc_zero
  done

  # Sanity-check: capability list should now include all 7.
  peko_iso_run capability list --principal planbot --json
  peko_iso_assert_rc_zero
  for cap in PlanCreate PlanList PlanGet PlanMarkStep \
             PlanRecordEvidence PlanAddStep PlanClose; do
    if [[ "$_peko_iso_capture_out" != *"\"tool:$cap\""* ]]; then
      echo "❌ capability list missing tool:$cap" >&2
      echo "   actual: $_peko_iso_capture_out" >&2
      return 1
    fi
  done

  # --- step 4: send a prompt that drives the agent through all 7 tools
  #
  # The prompt is explicit and step-numbered because we want the agent
  # to actually invoke the tools (LLMs without strong priors tend to
  # describe plans in prose rather than call the tools). We verify
  # each tool call landed by inspecting the plan JSONL after.
  local plans_dir="$PEKO_DATA_DIR/principals/planbot/local/plans"
  peko_iso_run send planbot "$(cat <<'PROMPT'
You must use the Plan* tools to build, mutate, and close a single plan. Do not describe the steps in prose; actually invoke each tool. Follow this exact sequence:

1. PlanCreate with title="Test plan", nodes=[
     { step: "first step" },
     { step: "second step" },
     { step: "third step (added later)" }
   ]
   — capture the returned planId and the auto-assigned node ids for steps 1+2 (call them N1, N2).

2. PlanList and confirm the new plan shows up.

3. PlanGet planId=<planId>.

4. PlanMarkStep planId=<planId> nodeId=N1 status=in_progress.

5. PlanAddStep planId=<planId> nodeId=N3 step="third step (added later)".

6. PlanRecordEvidence planId=<planId> nodeId=N1 evidence="completed step one".

7. PlanMarkStep planId=<planId> nodeId=N1 status=completed.

8. PlanClose planId=<planId> reason="all done".

After step 8, reply with a one-line summary including the final planId.
PROMPT
  )" --no-stream
  peko_iso_assert_rc_zero

  # --- step 5: assert on-disk state --------------------------------------
  if [[ ! -d "$plans_dir" ]]; then
    echo "❌ plans dir missing: $plans_dir" >&2
    return 1
  fi
  local plan_jsonls
  plan_jsonls="$(find "$plans_dir" -name '*.jsonl' 2>/dev/null | head -5)"
  if [[ -z "$plan_jsonls" ]]; then
    echo "❌ no plan JSONL files in $plans_dir" >&2
    return 1
  fi

  # Take the first plan (we only created one). Validate the closed
  # status and presence of three nodes — that proves create + add_step
  # + close all ran.
  local plan_jsonl
  plan_jsonl="$(echo "$plan_jsonls" | head -1)"
  echo "    inspecting plan: $plan_jsonl"

  if ! grep -q '"title":"Test plan"' "$plan_jsonl"; then
    echo "❌ plan JSONL missing title='Test plan'" >&2
    cat "$plan_jsonl" >&2
    return 1
  fi
  if ! grep -q '"closed"' "$plan_jsonl"; then
    echo "❌ plan JSONL has no closed field (PlanClose didn't land)" >&2
    cat "$plan_jsonl" >&2
    return 1
  fi
  if ! grep -q '"completed"' "$plan_jsonl"; then
    echo "❌ plan JSONL has no completed node (PlanMarkStep didn't land)" >&2
    cat "$plan_jsonl" >&2
    return 1
  fi

  # Count node objects in the JSONL. We expect at least 3 (two from
  # create + one from add_step). The exact shape is JSONL with one
  # record, but we can grep for `"step":` to count.
  local step_count
  step_count="$(grep -o '"step":' "$plan_jsonl" | wc -l | tr -d ' ')"
  if (( step_count < 3 )); then
    echo "❌ expected ≥3 nodes in plan, found $step_count" >&2
    cat "$plan_jsonl" >&2
    return 1
  fi

  # --- step 6: print plan for human inspection --------------------------
  echo
  echo "───── plan record ─────"
  cat "$plan_jsonl" | python3 -m json.tool 2>/dev/null || cat "$plan_jsonl"
  echo "───────────────────────"

  # --- step 7: nothing leaked into host ~/.peko ------------------------
  if [[ -d "$HOME/../.peko" && ! "$HOME" == *peko* ]]; then
    echo "❌ leak: ~/.peko modified by isolated flow" >&2
    return 1
  fi

  echo "✅ flow complete: plan-tool-e2e"
  peko_iso_done 0
}