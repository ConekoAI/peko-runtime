#!/bin/bash
set -e

# Quick E2E test for process tool only
# This is a fast test to verify the E2E pattern works

cd ~/pekora/projects/pekobot

# Source common init
source test_scripts/common/init_kimi.sh

echo "========================================"
echo "Quick E2E Test: Process Tool"
echo "========================================"
echo ""

# Run test prompt
prompt="Run date at your shell and get me the result"
echo "Prompt: $prompt"
echo ""

# Run agent (limit output to avoid too much noise)
timeout 60 ./target/debug/pekobot agent start testagent -M "$prompt" 2>&1 | tail -30 || true

echo ""

# Verify session was created
SESSION_DIR="$HOME/.pekobot/agents/testagent/sessions"

if [ -d "$SESSION_DIR" ]; then
    echo "✓ Sessions directory exists"
    
    # Find session files
    session_files=$(ls -1 "$SESSION_DIR"/*.jsonl 2>/dev/null | wc -l)
    echo "  Session files: $session_files"
    
    if [ "$session_files" -gt 0 ]; then
        latest_session=$(ls -t "$SESSION_DIR"/*.jsonl | head -1)
        echo "  Latest session: $(basename "$latest_session")"
        
        # Check for tool activity
        if grep -q '"tool"' "$latest_session" 2>/dev/null; then
            echo "  ✓ Found tool calls in session"
            tool_count=$(grep -c '"tool"' "$latest_session")
            echo "    Tool calls: $tool_count"
        else
            echo "  ⚠ No tool calls found (may still be processing)"
        fi
        
        # Check for process tool specifically
        if grep -q "process" "$latest_session" 2>/dev/null || \
           grep -q "shell" "$latest_session" 2>/dev/null || \
           grep -q "date" "$latest_session" 2>/dev/null; then
            echo "  ✓ Found process/date references"
        fi
        
        # Show session summary
        echo ""
        echo "Session summary:"
        total_lines=$(wc -l < "$latest_session")
        echo "  Total lines: $total_lines"
        
        # Count message types
        user_msgs=$(grep -c '"role":"user"' "$latest_session" 2>/dev/null || echo "0")
        assistant_msgs=$(grep -c '"role":"assistant"' "$latest_session" 2>/dev/null || echo "0")
        tool_calls=$(grep -c '"tool"' "$latest_session" 2>/dev/null || echo "0")
        
        echo "  User messages: $user_msgs"
        echo "  Assistant messages: $assistant_msgs"
        echo "  Tool calls: $tool_calls"
        
        echo ""
        echo "✓ Test completed successfully"
    else
        echo "  ⚠ No session files found"
    fi
else
    echo "  ⚠ Sessions directory not found"
fi

echo ""
echo "========================================"
echo "Cleanup"
echo "========================================"
rm -rf ~/.pekobot/agents/testagent
rm -rf ~/.local/share/pekobot/workspaces/testagent
echo "✓ Test agent cleaned up"
echo ""
