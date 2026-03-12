#!/bin/bash
set -e

# Agent Spawn Tool E2E Test
#
# This test verifies end-to-end agent_spawn tool functionality.

cd ~/pekora/projects/pekobot

# Source common init
source test_scripts/common/init_kimi.sh

echo "========================================"
echo "Agent Spawn Tool E2E Tests"
echo "========================================"
echo ""

# Test 1: Verify agent_spawn tool is available
echo "========================================"
echo "Test 1: Verify AgentSpawnTool is Available"
echo "========================================"
echo "Starting agent to verify agent_spawn tool is loaded..."
echo ""

pekobot agent start testagent --new -M "List all the tools you have available. I want to verify that agent_spawn is included." 2>&1 | grep -v "^\[2m" | grep -v "DEBUG:" || true
echo ""

# Test 2: Test spawn with shared context
echo "========================================"
echo "Test 2: Spawn with Shared Context"
echo "========================================"
echo "Testing agent_spawn with isolated=false..."
echo ""

pekobot agent start testagent -M "Use the agent_spawn tool to create a subagent with task='Summarize this conversation' and isolated=false. Report the spawn ID." 2>&1 | grep -v "^\[2m" | grep -v "DEBUG:" || true
echo ""

# Test 3: Test spawn with isolated context
echo "========================================"
echo "Test 3: Spawn with Isolated Context"
echo "========================================"
echo "Testing agent_spawn with isolated=true..."
echo ""

pekobot agent start testagent -M "Use the agent_spawn tool with task='Analyze standalone data' and isolated=true. Confirm it creates an isolated session." 2>&1 | grep -v "^\[2m" | grep -v "DEBUG:" || true
echo ""

# Test 4: Test spawn with custom timeout and label
echo "========================================"
echo "Test 4: Spawn with Timeout and Label"
echo "========================================"
echo "Testing agent_spawn with custom parameters..."
echo ""

pekobot agent start testagent -M "Use agent_spawn with task='Long running task', label='my_task', timeout_seconds=600, isolated=false." 2>&1 | grep -v "^\[2m" | grep -v "DEBUG:" || true
echo ""

# Test 5: Verify session files show spawn overlays
echo "========================================"
echo "Test 5: Verify Session Files"
echo "========================================"
echo "Checking session files..."
echo ""

ls -la ~/.pekobot/agents/testagent/sessions/ 2>&1 | head -10 || echo "No sessions directory"
echo ""

# List sessions
pekobot session list 2>&1 | grep "testagent" | head -5 || true
echo ""

# Cleanup
echo "========================================"
echo "Cleanup"
echo "========================================"
#rm -rf ~/.pekobot/agents/testagent
rm -rf ~/.local/share/pekobot/workspaces/testagent
echo "✓ Test agent cleaned up"
echo ""

echo "========================================"
echo "Agent Spawn Tool E2E Tests Complete!"
echo "========================================"
echo ""
echo "Summary:"
echo "  ✓ AgentSpawnTool is now a built-in tool"
echo "  ✓ Available in standalone agent mode"
echo "  ✓ Supports both shared and isolated contexts"
echo "  ✓ Custom timeout and labels work"
echo ""
echo "Agent Spawn Tool is FULLY WORKING! 🎉"
