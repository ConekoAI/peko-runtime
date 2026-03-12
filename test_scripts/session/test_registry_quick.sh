#!/bin/bash
# Quick test for session registry

set -e

echo "=== Quick Registry Test ==="
echo ""

# Use existing testagent
cd ~/pekora/projects/pekobot

# Clean up any existing test agent
rm -rf ~/.pekobot/agents/test_registry_quick
rm -rf ~/.local/share/pekobot/workspaces/test_registry_quick

echo "1. Creating agent..."
pekobot agent create test_registry_quick --provider kimi_code 2>&1 | tail -5

echo ""
echo "2. Checking initial state..."
SESSION_DIR="$HOME/.pekobot/agents/test_registry_quick/sessions"
ls -la "$SESSION_DIR" 2>/dev/null || echo "  (no sessions dir yet)"

echo ""
echo "3. Starting agent with first message..."
echo "Hello" | timeout 10s ./target/release/pekobot agent start test_registry_quick 2>&1 | grep -v "^\[2m" | grep -v "DEBUG:" | tail -10 || true

echo ""
echo "4. Checking session files..."
if [ -d "$SESSION_DIR" ]; then
    echo "  Session directory contents:"
    ls -la "$SESSION_DIR/"
    
    if [ -f "$SESSION_DIR/registry.json" ]; then
        echo ""
        echo "  Registry contents:"
        cat "$SESSION_DIR/registry.json" | python3 -m json.tool 2>/dev/null || cat "$SESSION_DIR/registry.json"
    fi
    
    echo ""
    echo "  JSONL files:"
    for f in "$SESSION_DIR"/*.jsonl; do
        if [ -f "$f" ]; then
            echo "    $(basename $f)"
        fi
    done
else
    echo "  No sessions directory!"
fi

echo ""
echo "5. Starting agent with --new flag..."
echo "Hello again" | timeout 10s ./target/release/pekobot agent start test_registry_quick --new 2>&1 | grep -v "^\[2m" | grep -v "DEBUG:" | tail -10 || true

echo ""
echo "6. Checking session files after --new..."
if [ -d "$SESSION_DIR" ]; then
    echo "  Session directory contents:"
    ls -la "$SESSION_DIR/"
    
    JSONL_COUNT=$(ls -1 "$SESSION_DIR"/*.jsonl 2>/dev/null | wc -l)
    echo ""
    echo "  JSONL file count: $JSONL_COUNT"
    
    if [ -f "$SESSION_DIR/registry.json" ]; then
        echo ""
        echo "  Registry contents:"
        python3 << 'EOF' - "$SESSION_DIR/registry.json"
import json
import sys
with open(sys.argv[1]) as f:
    data = json.load(f)
print(f"  Peers: {len(data.get('peers', {}))}")
for peer, entry in data.get('peers', {}).items():
    print(f"    Peer: {peer[:50]}...")
    print(f"    Active: {entry.get('active_session_id', 'N/A')[:8]}...")
    print(f"    Sessions: {len(entry.get('sessions', {}))}")
EOF
    fi
else
    echo "  No sessions directory!"
fi

echo ""
echo "7. Cleanup..."
rm -rf ~/.pekobot/agents/test_registry_quick
rm -rf ~/.local/share/pekobot/workspaces/test_registry_quick
echo "  Done"

echo ""
echo "=== Test Complete ==="
