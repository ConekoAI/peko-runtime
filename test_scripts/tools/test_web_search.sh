#!/bin/bash
set -e
# Set cwd
cd ~/pekora/projects/pekobot

# Get KIMI_API_KEY from .bashrc
export KIMI_API_KEY=$(grep "export KIMI_API_KEY=" ~/.bashrc | head -1 | sed 's/.*export KIMI_API_KEY="\(.*\)".*/\1/')
# Get BRAVE_API_KEY from .bashrc
export BRAVE_API_KEY=$(grep "export BRAVE_API_KEY=" ~/.bashrc | head -1 | sed 's/.*export BRAVE_API_KEY="\(.*\)".*/\1/')

echo "========================================"
echo "Testing Native Tool Calling"
echo "========================================"
echo ""
echo "KIMI_API_KEY: ${KIMI_API_KEY:0:15}..."
echo "BRAVE_API_KEY: ${BRAVE_API_KEY:0:10}..."
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
echo "Test: Native Tool Calling"
echo "========================================"
echo "Prompt: 'use web search to get me some news'"
echo ""
./target/debug/pekobot agent start testagent -M "use web search to get me some news"
echo ""

echo "========================================"
echo "Test completed!"
echo "========================================"
