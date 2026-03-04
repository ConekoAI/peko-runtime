#!/bin/bash
set -e

# Get KIMI_API_KEY from .bashrc
export KIMI_API_KEY=$(grep "export KIMI_API_KEY=" ~/.bashrc | head -1 | sed 's/.*export KIMI_API_KEY="\(.*\)".*/\1/')

echo "========================================"
echo "Testing Native Tool Calling (Phase 6)"
echo "========================================"
echo ""
echo "KIMI_API_KEY: ${KIMI_API_KEY:0:15}..."
echo ""

# Build
echo "Building Pekobot..."
source "$HOME/.cargo/env" && cargo build --bin pekobot 2>&1 | tail -3
echo ""

# Clean up previous test
echo "Cleaning up previous test agent..."
rm -rf ~/.pekobot/agents/testagent
rm -rf ~/.pekobot/agents/testagent.toml
echo ""

# Create agent with kimi provider
echo "Creating test agent with kimi provider..."
./target/debug/pekobot agent create testagent --provider kimi --model kimi-k2.5 --yes
echo ""

# Test native tool calling
echo "========================================"
echo "Test 1: Native Tool Calling"
echo "========================================"
echo "Prompt: 'Feed me some news'"
echo "Expected: Should use web_search tool via native API"
echo ""
./target/debug/pekobot agent start testagent -M "Feed me some news"
echo ""

echo "========================================"
echo "Test completed!"
echo "========================================"
echo ""
echo "Check the output above for:"
echo "  - '🤔' thinking indicator"
echo "  - '🔧 Using tool: web_search' tool call"
echo "  - '✅ Tool completed' success indicator"
echo "  - Final answer with news headlines"
echo ""
echo "Session log location:"
ls -la ~/.pekobot/agents/testagent/sessions/ 2>/dev/null || echo "No sessions directory"
