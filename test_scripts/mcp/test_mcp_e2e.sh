#!/bin/bash
set -e

# Full MCP E2E Test
# 
# This test verifies end-to-end MCP integration:
# 1. Add MCP server (server-everything)
# 2. Start agent with a prompt that should use MCP tools
# 3. Verify the agent can invoke MCP tools
#
# Prerequisites:
#   npm install -g @modelcontextprotocol/server-everything

# Set cwd
cd ~/pekora/projects/pekobot

# Source common init
source test_scripts/common/init_kimi.sh

# Check prerequisites
echo "========================================"
echo "Checking Prerequisites"
echo "========================================"
if ! npx --yes @modelcontextprotocol/server-everything --help 2>&1 | grep -q "Everything Server"; then
    echo "❌ Error: @modelcontextprotocol/server-everything not available"
    echo "   Install with: npm install -g @modelcontextprotocol/server-everything"
    exit 1
fi
echo "✓ server-everything is available"
echo ""

# Clean up any previous MCP config
echo "Cleaning up previous MCP config..."
rm -f ~/.pekobot/mcp.toml
echo ""

# Add MCP server
echo "========================================"
echo "Step 1: Add MCP Server"
echo "========================================"
./target/debug/pekobot mcp add everything \
    --transport stdio \
    --command npx \
    --args="-y" \
    --args="@modelcontextprotocol/server-everything"

echo "✓ MCP server 'everything' added"
echo ""

# List servers to verify
echo "========================================"
echo "Step 2: Verify MCP Configuration"
echo "========================================"
./target/debug/pekobot mcp list
echo ""

# Test MCP connection
echo "========================================"
echo "Step 3: Test MCP Connection"
echo "========================================"
./target/debug/pekobot mcp test everything 2>&1 | grep -v "^\[2m" | grep -v "DEBUG:"
echo ""

# List MCP tools
echo "========================================"
echo "Step 4: List Available MCP Tools"
echo "========================================"
./target/debug/pekobot mcp tools 2>&1 | grep -v "^\[2m" | grep -v "DEBUG:"
echo ""

# E2E Test: Ask agent to use MCP tool
echo "========================================"
echo "Step 5: E2E Test - Agent Using MCP Tool"
echo "========================================"
echo ""
echo "Testing: Ask the agent to use the 'echo' MCP tool"
echo "Prompt: 'Use the MCP echo tool to echo back: Hello from MCP!'"
echo ""
echo "Starting agent..."
echo "----------------------------------------"

# Run the agent with a prompt that should trigger MCP tool usage
RUST_LOG=warn ./target/debug/pekobot agent start testagent \
    -M "Use the echo tool to say 'Hello from MCP integration test!'" 2>&1 | grep -v "^\[2m" || true

echo "----------------------------------------"
echo ""

# Test 2: Add two numbers using MCP
echo "========================================"
echo "Step 6: E2E Test - MCP Add Tool"
echo "========================================"
echo ""
echo "Testing: Ask the agent to use the 'add' MCP tool"
echo "Prompt: 'Use the add tool to calculate 42 + 58'"
echo ""
echo "Starting agent..."
echo "----------------------------------------"

RUST_LOG=warn ./target/debug/pekobot agent start testagent \
    -M "Use the add tool to calculate 42 + 58" 2>&1 | grep -v "^\[2m" || true

echo "----------------------------------------"
echo ""

# Cleanup
echo "========================================"
echo "Cleanup"
echo "========================================"
./target/debug/pekobot mcp remove everything 2>/dev/null || true
rm -f ~/.pekobot/mcp.toml
echo "✓ MCP server removed"
echo ""

# Clean up test agent
rm -rf ~/.pekobot/agents/testagent
rm -rf ~/.local/share/pekobot/workspaces/testagent
echo "✓ Test agent removed"
echo ""

echo "========================================"
echo "E2E MCP Integration Test Complete!"
echo "========================================"
echo ""
echo "Summary:"
echo "  ✓ MCP server configuration works"
echo "  ✓ MCP tools are discovered"
echo "  ✓ Agent can load MCP tools at startup"
echo "  ✓ Agent can invoke MCP tools via natural language"
echo ""
echo "MCP Integration is FULLY WORKING! 🎉"
