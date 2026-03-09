#!/bin/bash
set -e
# Set cwd
cd ~/pekora/projects/pekobot
./test_scripts/common/init_kimi.sh


# Get BRAVE_API_KEY from .bashrc
export BRAVE_API_KEY=$(grep "export BRAVE_API_KEY=" ~/.bashrc | head -1 | sed 's/.*export BRAVE_API_KEY="\(.*\)".*/\1/')

echo "BRAVE_API_KEY: ${BRAVE_API_KEY:0:10}..."


# Test web search
echo "========================================"
echo "Test: web search"
echo "========================================"
prompt='use web search to get me some news'
echo "Prompt: '$prompt'"
echo ""
pekobot agent start testagent -M "$prompt"
echo ""

echo "========================================"
echo "Test completed!"
echo "========================================"
