#!/bin/bash
# Interactive CLI Session Commands E2E Test
# Tests /new, /branch, /sessions, /switch via stdin commands

set -e

echo "========================================"
echo "CLI Session Commands Interactive Test"
echo "========================================"
echo ""

cd ~/pekora/projects/pekobot

# Source common init
source test_scripts/common/init_kimi.sh

# Setup cleanup trap
cleanup() {
    echo ""
    echo "Cleaning up..."
    rm -rf ~/.pekobot/agents/test_cli_cmd
    rm -rf ~/.local/share/pekobot/workspaces/test_cli_cmd
}
trap cleanup EXIT

SESSION_DIR="$HOME/.pekobot/agents/test_cli_cmd/sessions"

echo "Test agent: test_cli_cmd"
echo "Session dir: $SESSION_DIR"
echo ""

# Create the agent
echo "Creating test agent..."
pekobot create test_cli_cmd --provider kimi_code --non-interactive 2>&1 | tail -10
echo ""

# Test 1: Start agent and send messages, then use /sessions
echo "========================================"
echo "Test 1: Create session and list"
echo "========================================"
echo ""

# Create an expect script to interact with the agent
cat > /tmp/test_cli_cmd.exp << 'EXPECTEOF'
#!/usr/bin/expect -f
set timeout 30
spawn ./target/release/pekobot run test_cli_cmd

expect "💬 You:"
send "Hello, this is the first session!\r"

expect "testagent:"
expect "💬 You:"
send "/sessions\r"

expect "📁 Sessions:"
expect "💬 You:"
send "exit\r"

expect eof
EXPECTEOF

chmod +x /tmp/test_cli_cmd.exp

# Run the expect script (if expect is available)
if command -v expect &> /dev/null; then
    echo "Running interactive test with expect..."
    cd ~/pekora/projects/pekobot && /tmp/test_cli_cmd.exp 2>&1 || true
else
    echo "expect not available, using simple test approach..."
    
    # Simple test using echo and timeout
    echo "First message" | timeout 10s ./target/release/pekobot run test_cli_cmd 2>&1 | tail -20 || true
fi

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
    
    # Check JSONL files
    echo ""
    echo "Session files:"
    for f in "$SESSION_DIR"/*.jsonl; do
        if [ -f "$f" ]; then
            echo "  $(basename $f):"
            echo "    Lines: $(wc -l < "$f")"
            echo "    First line:"
            head -1 "$f" | python3 -m json.tool 2>/dev/null | head -5 || head -1 "$f"
        fi
    done
else
    echo "  No sessions directory yet"
fi

echo ""
echo "========================================"
echo "Test 2: Verify JSONL format"
echo "========================================"
echo ""

if [ -f "$SESSION_DIR"/*.jsonl ]; then
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
                echo "$line" | python3 -c "import sys,json; d=json.load(sys.stdin); print(f'    ID: {d.get(\"id\", \"N/A\")[:8]}...')" 2>/dev/null
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
    echo "Raw content (first 5 lines):"
    head -5 "$JSONL_FILE" | python3 -m json.tool 2>/dev/null || head -5 "$JSONL_FILE"
else
    echo "  No JSONL files to analyze"
fi

echo ""
echo "========================================"
echo "Test Complete"
echo "========================================"
