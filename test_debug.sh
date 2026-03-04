#!/bin/bash
set -e

cd /home/ubuntu/pekora/projects/pekobot

# Get KIMI_API_KEY from .bashrc
export KIMI_API_KEY=$(grep "export KIMI_API_KEY=" ~/.bashrc | head -1 | sed 's/.*export KIMI_API_KEY="\(.*\)".*/\1/')

echo "========================================"
echo "Testing Native Tool Calling (DEBUG)"
echo "========================================"
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

# Create agent with kimi provider (using kimi_code alias)
echo "Creating test agent with kimi_code provider..."
./target/debug/pekobot agent create testagent --provider kimi_code --yes
echo ""

# Set API key
echo "Setting API key..."
./target/debug/pekobot auth set kimi "$KIMI_API_KEY"
echo ""

# Test native tool calling with DEBUG logging
echo "========================================"
echo "Test: Native Tool Calling (DEBUG)"
echo "========================================"
echo "Prompt: 'Feed me some news'"
echo ""
# Use -vvv for trace logging
./target/debug/pekobot agent start testagent -vvv -M "Feed me some news" 2>&1 | tee /tmp/debug_test.log | grep -E "Using tool|Agent:|final_answer|completed|stopped|Max iterations|Messages sent|Adding" | head -30
echo ""

# Show any Anthropic API debug info
echo ""
echo "Debug info (Anthropic API):"
grep -E "Anthropic|tool_use|tool_result" /tmp/debug_test.log | head -20 || echo "No Anthropic debug output found"

echo ""
echo "========================================"
echo "Test completed!"
echo "========================================"
