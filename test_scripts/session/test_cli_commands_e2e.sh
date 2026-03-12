#!/bin/bash
# CLI Session Commands E2E Test
# Tests session registry with actual agent runs

set -e

echo "========================================"
echo "CLI Session Commands E2E Test"
echo "========================================"
echo ""

cd ~/pekora/projects/pekobot

# Source common init
source test_scripts/common/init_kimi.sh

# Setup cleanup trap
cleanup() {
    echo ""
    echo "========================================"
    echo "Cleanup"
    echo "========================================"
    rm -rf ~/.pekobot/agents/test_cli_cmd
    rm -rf ~/.local/share/pekobot/workspaces/test_cli_cmd
    echo "✓ Test agent cleaned up"
}
trap cleanup EXIT

SESSION_DIR="$HOME/.pekobot/agents/test_cli_cmd/sessions"

echo "Test agent: test_cli_cmd"
echo "Session dir: $SESSION_DIR"
echo ""

# Create the agent
echo "========================================"
echo "Creating test agent..."
echo "========================================"
pekobot agent create test_cli_cmd --provider kimi_code --non-interactive 2>&1 | tail -10
echo ""

# Test 1: Start agent with first message
echo "========================================"
echo "Test 1: Create first session"
echo "========================================"
echo "Sending: 'Hello, this is session 1!'"
echo ""

echo "Hello, this is session 1!" | timeout 15s ./target/release/pekobot agent start test_cli_cmd --new 2>&1 | grep -v "^\[2m" | grep -v "^DEBUG:" || true

echo ""
echo "Checking session files..."
if [ -d "$SESSION_DIR" ]; then
    ls -la "$SESSION_DIR/"
    
    if [ -f "$SESSION_DIR/registry.json" ]; then
        echo ""
        echo "Registry contents:"
        python3 << 'EOF' - "$SESSION_DIR/registry.json"
import json
import sys

with open(sys.argv[1]) as f:
    registry = json.load(f)

print(f"  Peers: {len(registry.get('peers', {}))}")
for peer_key, entry in registry.get('peers', {}).items():
    print(f"\n  Peer: {peer_key}")
    print(f"    Active: {entry.get('active_session_id', 'N/A')[:8]}...")
    print(f"    Sessions: {len(entry.get('sessions', {}))}")
    for sid, info in entry.get('sessions', {}).items():
        label = info.get('label') or 'unnamed'
        print(f"      - {sid[:8]}...: {label}")
EOF
    fi
else
    echo "  No sessions directory yet"
fi

echo ""

# Test 2: Analyze JSONL format
echo "========================================"
echo "Test 2: Verify JSONL format"
echo "========================================"
echo ""

if ls "$SESSION_DIR"/*.jsonl 1> /dev/null 2>&1; then
    JSONL_FILE=$(ls -t "$SESSION_DIR"/*.jsonl | head -1)
    echo "Analyzing: $(basename $JSONL_FILE)"
    echo ""
    
    echo "Line-by-line breakdown:"
    line_num=0
    while IFS= read -r line; do
        line_num=$((line_num + 1))
        line_type=$(echo "$line" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('type', 'unknown'))" 2>/dev/null || echo "invalid")
        
        case $line_type in
            session)
                echo "  Line $line_num: session header"
                echo "$line" | python3 -c "import sys,json; d=json.load(sys.stdin); print(f'    ID: {d.get(\"id\", \"N/A\")}')" 2>/dev/null
                ;;
            message)
                role=$(echo "$line" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('data', {}).get('role', 'unknown'))" 2>/dev/null || echo "unknown")
                echo "  Line $line_num: message ($role)"
                ;;
            compaction)
                echo "  Line $line_num: compaction marker"
                ;;
            *)
                echo "  Line $line_num: $line_type"
                ;;
        esac
    done < "$JSONL_FILE"
    
    echo ""
    echo "Raw content (formatted):"
    head -10 "$JSONL_FILE" | while read -r line; do
        echo "$line" | python3 -m json.tool 2>/dev/null || echo "$line"
        echo "---"
    done
else
    echo "  No JSONL files to analyze"
fi

echo ""

# Test 3: Start with --new flag (should create new session)
echo "========================================"
echo "Test 3: Create second session (--new)"
echo "========================================"
echo "Sending: 'This is session 2!'"
echo ""

echo "This is session 2!" | timeout 15s ./target/release/pekobot agent start test_cli_cmd --new 2>&1 | grep -v "^\[2m" | grep -v "^DEBUG:" || true

echo ""
echo "Verifying multiple sessions..."
if [ -f "$SESSION_DIR/registry.json" ]; then
    python3 << 'EOF' - "$SESSION_DIR/registry.json"
import json
import sys

with open(sys.argv[1]) as f:
    registry = json.load(f)

for peer_key, entry in registry.get('peers', {}).items():
    session_count = len(entry.get('sessions', {}))
    print(f"  Peer {peer_key[:50]}... has {session_count} session(s)")
    
    if session_count >= 2:
        print("  ✓ Multiple sessions tracked correctly!")
        for sid, info in entry.get('sessions', {}).items():
            label = info.get('label') or 'unnamed'
            print(f"      - {sid}: {label}")
    elif session_count == 1:
        print("  ⚠ Only 1 session (expected 2 with --new)")
        print("  (This may indicate --new overwrites existing sessions)")
    else:
        print("  ✗ No sessions found")
EOF
fi

echo ""
echo "Session files:"
ls -la "$SESSION_DIR/"*.jsonl 2>/dev/null || echo "  No JSONL files"

echo ""

# Test 4: Check JSONL content isolation
echo "========================================"
echo "Test 4: Session Content Isolation"
echo "========================================"
echo ""

JSONL_COUNT=$(ls -1 "$SESSION_DIR"/*.jsonl 2>/dev/null | wc -l)
echo "Total JSONL files: $JSONL_COUNT"

if [ "$JSONL_COUNT" -ge 2 ]; then
    echo ""
    echo "Comparing session content:"
    
    # Get first two files
    FILE1=$(ls -t "$SESSION_DIR"/*.jsonl | head -1)
    FILE2=$(ls -t "$SESSION_DIR"/*.jsonl | sed -n '2p')
    
    echo "  File 1: $(basename $FILE1)"
    echo "  File 2: $(basename $FILE2)"
    echo ""
    
    # Extract session IDs
    ID1=$(head -1 "$FILE1" | python3 -c "import sys,json; print(json.load(sys.stdin).get('id', 'N/A'))" 2>/dev/null)
    ID2=$(head -1 "$FILE2" | python3 -c "import sys,json; print(json.load(sys.stdin).get('id', 'N/A'))" 2>/dev/null)
    
    echo "  Session 1 ID: $ID1"
    echo "  Session 2 ID: $ID2"
    
    if [ "$ID1" != "$ID2" ]; then
        echo "  ✓ Different session IDs (proper isolation)"
    else
        echo "  ✗ Same session ID (unexpected)"
    fi
    
    # Count messages
    MSG1=$(grep -c '"type":"message"' "$FILE1" 2>/dev/null || echo "0")
    MSG2=$(grep -c '"type":"message"' "$FILE2" 2>/dev/null || echo "0")
    
    echo ""
    echo "  File 1 messages: $MSG1"
    echo "  File 2 messages: $MSG2"
    
    # Check content
    CONTENT1=$(grep -o 'session 1' "$FILE1" 2>/dev/null | head -1 || echo "")
    CONTENT2=$(grep -o 'session 2' "$FILE2" 2>/dev/null | head -1 || echo "")
    
    if [ -n "$CONTENT1" ]; then
        echo "  ✓ File 1 contains 'session 1'"
    fi
    if [ -n "$CONTENT2" ]; then
        echo "  ✓ File 2 contains 'session 2'"
    fi
else
    echo "  Need at least 2 sessions for isolation test"
fi

echo ""

# Test 5: UUID format verification
echo "========================================"
echo "Test 5: UUID Format Verification"
echo "========================================"
echo ""

UUID_PATTERN='^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\.jsonl$'
VALID_COUNT=0

for f in "$SESSION_DIR"/*.jsonl; do
    if [ -f "$f" ]; then
        filename=$(basename "$f")
        if echo "$filename" | grep -qE "$UUID_PATTERN"; then
            echo "  ✓ $filename (valid UUID)"
            VALID_COUNT=$((VALID_COUNT + 1))
        else
            echo "  ⚠ $filename (not UUID format)"
        fi
    fi
done

echo ""
echo "Summary: $VALID_COUNT valid UUID-named files"

echo ""
echo "========================================"
echo "E2E Test Complete"
echo "========================================"
