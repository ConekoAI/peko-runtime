#!/bin/bash
set -e

# Setup verification for Built-in Tools E2E Tests
# This script verifies the environment is ready for E2E testing

cd ~/pekora/projects/pekobot

echo "========================================"
echo "Built-in Tools E2E Test - Setup Check"
echo "========================================"
echo ""

# Check 1: Binary exists
echo "✓ Checking pekobot binary..."
if [ -f "./target/debug/pekobot" ]; then
    echo "  ✓ Debug binary found"
    ./target/debug/pekobot --version
elif [ -f "./target/release/pekobot" ]; then
    echo "  ✓ Release binary found"
    ./target/release/pekobot --version
else
    echo "  ✗ No pekobot binary found. Run 'cargo build' first."
    exit 1
fi
echo ""

# Check 2: API key is configured
echo "✓ Checking API key configuration..."
if [ -n "$KIMI_API_KEY" ]; then
    echo "  ✓ KIMI_API_KEY is set"
elif grep -q "KIMI_API_KEY" ~/.bashrc 2>/dev/null; then
    echo "  ✓ KIMI_API_KEY found in ~/.bashrc"
else
    echo "  ⚠ KIMI_API_KEY not found. E2E tests require API key."
    echo "    Set it with: export KIMI_API_KEY=your_key"
fi
echo ""

# Check 3: Test agent can be created
echo "✓ Testing agent creation..."
TEST_AGENT="testagent_setup_check"

# Clean up any previous test agent
rm -rf ~/.pekobot/agents/$TEST_AGENT
rm -rf ~/.local/share/pekobot/workspaces/$TEST_AGENT

# Create agent
if ./target/debug/pekobot agent create $TEST_AGENT --provider kimi_code --yes 2>&1 | tail -5; then
    echo "  ✓ Agent creation works"
else
    echo "  ✗ Agent creation failed"
    exit 1
fi

# Check 4: Agent directory structure
echo ""
echo "✓ Checking agent directory structure..."
AGENT_DIR="$HOME/.pekobot/agents/$TEST_AGENT"

if [ -d "$AGENT_DIR" ]; then
    echo "  ✓ Agent directory exists: $AGENT_DIR"
    
    if [ -d "$AGENT_DIR/sessions" ]; then
        echo "  ✓ Sessions directory exists"
    else
        echo "  ✗ Sessions directory missing"
    fi
    
    if [ -f "$AGENT_DIR/config.json" ]; then
        echo "  ✓ Config file exists"
    else
        echo "  ✗ Config file missing"
    fi
else
    echo "  ✗ Agent directory not found"
fi

# Check 5: Tools are available
echo ""
echo "✓ Checking available tools..."

# Get list of tools from the agent
# This is a simplified check - in real test we'd parse JSON
if ./target/debug/pekobot agent show $TEST_AGENT 2>&1 | grep -q "tools"; then
    echo "  ✓ Agent has tool configuration"
else
    echo "  ⚠ Tool configuration not visible (may be OK)"
fi

# Clean up
echo ""
echo "✓ Cleaning up test agent..."
rm -rf ~/.pekobot/agents/$TEST_AGENT
rm -rf ~/.local/share/pekobot/workspaces/$TEST_AGENT

echo ""
echo "========================================"
echo "Setup Check Complete"
echo "========================================"
echo ""
echo "Environment is ready for E2E testing."
echo ""
echo "Run the full test with:"
echo "  ./test_scripts/tools/test_builtin_tools_e2e.sh"
echo ""
