#!/bin/bash
set -e

# Set cwd
cd ~/pekora/projects/pekobot
./test_scripts/common/init_kimi.sh

# Test fetch
echo "========================================"
echo "Test: Process"
echo "========================================"
prompt='Run date at your shell and get me the result'
echo "Prompt: $prompt'"
echo ""
./target/debug/pekobot agent start testagent -M "$prompt"
echo ""

echo "========================================"
echo "Test completed!"
echo "========================================"
