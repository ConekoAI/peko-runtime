#!/usr/bin/env bash
# scripts/e2e/run-case.sh — entry point for executing a single flow.
#
# Usage:
#   scripts/e2e/run-case.sh <flow-name> [extra-args...]
#
# Examples:
#   scripts/e2e/run-case.sh principal-create-show
#   scripts/e2e/run-case.sh cron-add-list KEEP_TEMPDIR=1
#   MOCK_LLM_URL=http://127.0.0.1:9999/v1 scripts/e2e/run-case.sh mock-llm-ping
#
# The runner sources `lib/isolate.sh`, then `flows/<flow-name>.sh`. Each
# flow is a tiny shell script that calls `peko_iso_init` followed by a
# sequence of `peko_iso_run` calls and assertions.

set -euo pipefail

E2E_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [[ $# -lt 1 ]]; then
  echo "usage: $0 <flow-name> [extra-args...]" >&2
  echo >&2
  echo "Available flows:" >&2
  for f in "$E2E_DIR"/flows/*.sh; do
    [[ -e "$f" ]] || continue
    echo "  $(basename "$f" .sh)" >&2
  done
  exit 64   # EX_USAGE
fi

FLOW_NAME="$1"
shift

FLOW_SCRIPT="$E2E_DIR/flows/${FLOW_NAME}.sh"
if [[ ! -f "$FLOW_SCRIPT" ]]; then
  echo "❌ flow not found: $FLOW_NAME" >&2
  echo "   expected: $FLOW_SCRIPT" >&2
  exit 64
fi

# Forward env-overrides (KEEP_TEMPDIR=, MOCK_LLM_URL=, NO_DAEMON=) to the
# flow script verbatim. Anything else passed on the command line after the
# flow name is exported as PEKO_FLOW_ARGS so the flow can read it.
export PEKO_FLOW_NAME="$FLOW_NAME"
PEKO_FLOW_ARGS=("$@")

# Source the flow. It must define `flow_main` (called by us) which is the
# one place that calls `peko_iso_init` and runs the sequence.
source "$E2E_DIR/lib/isolate.sh"
source "$FLOW_SCRIPT"

flow_main "$@"
