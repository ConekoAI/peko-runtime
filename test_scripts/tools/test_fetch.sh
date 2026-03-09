#!/bin/bash
set -e

# Set cwd
cd ~/pekora/projects/pekobot
./test_scripts/common/init_kimi.sh

# Test fetch
echo "========================================"
echo "Test: Fetch"
echo "========================================"
prompt='Summarize this page: https://docs.openclaw.ai/tools/apply-patch'
echo "Prompt: $prompt'"
echo ""
./target/debug/pekobot agent start testagent -M "$prompt"
echo ""

echo "========================================"
echo "Test completed!"
echo "========================================"
