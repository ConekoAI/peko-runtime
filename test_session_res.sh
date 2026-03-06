#!/bin/bash
set -e

# Get KIMI_API_KEY from .bashrc
export KIMI_API_KEY=$(grep "export KIMI_API_KEY=" ~/.bashrc | head -1 | sed 's/.*export KIMI_API_KEY="\(.*\)".*/\1/')

echo "========================================"
echo "Testing Session Resumption"
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

# Create agent with kimi provider (using kimi_code alias)
echo "Creating test agent with kimi_code provider..."
./target/debug/pekobot agent create testagent --provider kimi_code --yes
echo ""

# Set API key
echo "Setting API key..."
./target/debug/pekobot auth set kimi "$KIMI_API_KEY"
echo ""

# Test native tool calling
echo "========================================"
echo "Test: Session Resumption"
echo "========================================"
echo "Prompt: 'What's USA's Capital?'"
echo ""
./target/debug/pekobot agent start testagent -M "What's USA's Capital?"
echo "Prompt: 'What about France?'"
echo ""
./target/debug/pekobot agent start testagent -M "What about France?"
echo ""
echo "========================================"
echo "Test completed!"
echo "========================================"
