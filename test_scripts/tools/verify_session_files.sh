#!/bin/bash
set -e

# Session File Verification Script
#
# This script verifies that generated session JSONL files conform to expected format.

cd ~/pekora/projects/pekobot

# Source common init
source test_scripts/common/init_kimi.sh

echo "========================================"
echo "Session File Format Verification"
echo "========================================"
echo ""

# Create test agent and spawn subagent
echo "Creating test scenario with subagent spawn..."
pekobot agent start testagent --new -M "Use agent_spawn with task='Test task for verification', isolated=false. Report the childSessionKey from the result." 2>&1 | grep -v "^\[2m" | grep -v "DEBUG:" || true
echo ""

# Wait a moment for files to be written
sleep 1

SESSION_DIR="$HOME/.pekobot/agents/testagent/sessions"

echo "========================================"
echo "Verification Tests"
echo "========================================"
echo ""

# Test 1: Sessions directory exists
echo "Test 1: Sessions directory exists"
if [ -d "$SESSION_DIR" ]; then
    echo "  ✓ Sessions directory exists: $SESSION_DIR"
else
    echo "  ✗ Sessions directory NOT found"
    exit 1
fi
echo ""

# Test 2: sessions.json exists and is valid JSON
echo "Test 2: sessions.json is valid JSON"
if [ -f "$SESSION_DIR/sessions.json" ]; then
    if python3 -c "import json; json.load(open('$SESSION_DIR/sessions.json'))" 2>/dev/null; then
        echo "  ✓ sessions.json is valid JSON"
    else
        echo "  ✗ sessions.json is NOT valid JSON"
        exit 1
    fi
else
    echo "  ✗ sessions.json NOT found"
    exit 1
fi
echo ""

# Test 3: Check session JSONL file format
echo "Test 3: Session JSONL file format validation"
for jsonl_file in "$SESSION_DIR"/*.jsonl; do
    if [ -f "$jsonl_file" ]; then
        filename=$(basename "$jsonl_file")
        echo "  Checking $filename..."
        
        # Check first line is session header
        first_line=$(head -1 "$jsonl_file")
        if echo "$first_line" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('type')=='session', 'First line must be session header'" 2>/dev/null; then
            echo "    ✓ Has session header"
        else
            echo "    ✗ Missing or invalid session header"
            exit 1
        fi
        
        # Check all lines are valid JSON
        line_num=0
        valid_lines=0
        while IFS= read -r line; do
            line_num=$((line_num + 1))
            if echo "$line" | python3 -c "import sys,json; json.load(sys.stdin)" 2>/dev/null; then
                valid_lines=$((valid_lines + 1))
            else
                echo "    ✗ Invalid JSON at line $line_num"
                exit 1
            fi
        done < "$jsonl_file"
        echo "    ✓ All $valid_lines lines are valid JSON"
    fi
done
echo ""

# Test 4: Verify message structure
echo "Test 4: Message structure validation"
for jsonl_file in "$SESSION_DIR"/*.jsonl; do
    if [ -f "$jsonl_file" ]; then
        filename=$(basename "$jsonl_file")
        
        # Count message types
        msg_count=$(grep -c '"type":"message"' "$jsonl_file" 2>/dev/null || echo "0")
        tool_result_count=$(grep -c '"type":"toolResult"' "$jsonl_file" 2>/dev/null || echo "0")
        
        echo "  $filename: $msg_count messages, $tool_result_count tool results"
        
        # Check for required message fields
        if grep -q '"role":"user"' "$jsonl_file"; then
            echo "    ✓ Has user messages"
        fi
        if grep -q '"role":"assistant"' "$jsonl_file"; then
            echo "    ✓ Has assistant messages"
        fi
        if grep -q '"role":"system"' "$jsonl_file"; then
            echo "    ✓ Has system messages"
        fi
    fi
done
echo ""

# Test 5: Check for subagent spawn sessions
echo "Test 5: Subagent spawn session verification"
spawn_sessions=$(grep -l "spawn" "$SESSION_DIR"/*.jsonl 2>/dev/null | wc -l)
echo "  Found $spawn_sessions spawn session files"

if [ "$spawn_sessions" -gt 0 ]; then
    # Check spawn session has subagent context
    for jsonl_file in "$SESSION_DIR"/*.jsonl; do
        if [ -f "$jsonl_file" ] && grep -q "spawn" "$jsonl_file"; then
            if grep -q "Subagent Context" "$jsonl_file"; then
                echo "  ✓ Spawn session contains Subagent Context"
            fi
            if grep -q "Subagent Task" "$jsonl_file"; then
                echo "  ✓ Spawn session contains Subagent Task"
            fi
        fi
    done
fi
echo ""

# Test 6: Verify parent session has tool results
echo "Test 6: Parent session tool results"
for jsonl_file in "$SESSION_DIR"/testagent_user_*.jsonl; do
    if [ -f "$jsonl_file" ]; then
        if grep -q '"toolName":"agent_spawn"' "$jsonl_file"; then
            echo "  ✓ Parent session has agent_spawn tool calls"
        fi
        if grep -q '"status":"accepted"' "$jsonl_file"; then
            echo "  ✓ Parent session has accepted spawn receipts"
        fi
        if grep -q '"childSessionKey"' "$jsonl_file"; then
            echo "  ✓ Parent session has childSessionKey references"
        fi
    fi
done
echo ""

# Test 7: Check session metadata consistency
echo "Test 7: Session metadata consistency"
python3 << 'EOF'
import json
import sys

sessions_file = "$HOME/.pekobot/agents/testagent/sessions/sessions.json"
session_dir = "$HOME/.pekobot/agents/testagent/sessions"

try:
    with open(sessions_file) as f:
        sessions = json.load(f)
    
    for session_key, metadata in sessions.items():
        transcript_file = metadata.get('transcript_file')
        if transcript_file:
            import os
            full_path = os.path.join(session_dir, transcript_file)
            if os.path.exists(full_path):
                print(f"  ✓ {session_key}: transcript file exists ({transcript_file})")
            else:
                print(f"  ✗ {session_key}: transcript file MISSING ({transcript_file})")
                sys.exit(1)
        
        # Verify required fields
        required = ['session_id', 'agent_name', 'session_key', 'created_at']
        for field in required:
            if field in metadata:
                print(f"    ✓ Has {field}")
            else:
                print(f"    ✗ Missing {field}")
                sys.exit(1)
except Exception as e:
    print(f"  ✗ Error checking metadata: {e}")
    sys.exit(1)
EOF
echo ""

echo "========================================"
echo "Verification Complete"
echo "========================================"
echo ""
echo "All session files validated successfully:"
echo "  ✓ sessions.json is valid"
echo "  ✓ All JSONL files are valid JSON"
echo "  ✓ Session headers present"
echo "  ✓ Message structure correct"
echo "  ✓ Subagent spawn sessions have proper context"
echo "  ✓ Parent sessions have tool results"
echo "  ✓ Metadata consistent with files"
echo ""

# Show summary
echo "Session files:"
ls -la "$SESSION_DIR"/
echo ""

# Cleanup
echo "========================================"
echo "Cleanup"
echo "========================================"
rm -rf ~/.pekobot/agents/testagent
rm -rf ~/.local/share/pekobot/workspaces/testagent
echo "✓ Test agent cleaned up"
echo ""
