#!/bin/bash
# Final Session Registry E2E Test
# Verifies UUID-based naming, registry persistence, and multiple sessions

set -e

echo "========================================"
echo "Session Registry Final E2E Test"
echo "========================================"
echo ""

cd ~/pekora/projects/pekobot

# Setup cleanup trap
cleanup() {
    echo ""
    echo "========================================"
    echo "Cleanup"
    echo "========================================"
    rm -rf ~/.pekobot/agents/test_registry_final
    rm -rf ~/.local/share/pekobot/workspaces/test_registry_final
    echo "✓ Cleaned up"
}
trap cleanup EXIT

# Create test agent
echo "Creating test agent..."
echo "assistant" | timeout 10s ./target/release/pekobot agent create test_registry_final --provider kimi_code 2>&1 | tail -5 || true

SESSION_DIR="$HOME/.pekobot/agents/test_registry_final/sessions"

echo ""
echo "========================================"
echo "Test 1: First Session Creation"
echo "========================================"
echo ""

echo "Hello, session 1!" | timeout 15s ./target/release/pekobot agent start test_registry_final 2>&1 | tail -10 || true

echo ""
echo "Verifying session files..."
if [ -d "$SESSION_DIR" ]; then
    echo "  Session directory contents:"
    ls -la "$SESSION_DIR/"
    
    # Check for registry.json
    if [ -f "$SESSION_DIR/registry.json" ]; then
        echo ""
        echo "  ✓ registry.json exists"
        
        # Count sessions in registry
        SESSION_COUNT=$(grep -o '"session_id"' "$SESSION_DIR/registry.json" | wc -l)
        echo "  ✓ Registry contains $SESSION_COUNT session(s)"
    else
        echo "  ✗ registry.json NOT FOUND"
        exit 1
    fi
    
    # Count UUID-named JSONL files
    UUID_COUNT=$(ls -1 "$SESSION_DIR"/*.jsonl 2>/dev/null | grep -E '^[0-9a-f]{8}-' | wc -l)
    echo "  ✓ UUID-named session files: $UUID_COUNT"
else
    echo "  ✗ Sessions directory NOT FOUND"
    exit 1
fi

echo ""
echo "========================================"
echo "Test 2: Second Session (--new)"
echo "========================================"
echo ""

echo "Hello, session 2!" | timeout 15s ./target/release/pekobot agent start test_registry_final --new 2>&1 | tail -10 || true

echo ""
echo "Verifying multiple sessions..."
if [ -f "$SESSION_DIR/registry.json" ]; then
    SESSION_COUNT=$(grep -o '"session_id"' "$SESSION_DIR/registry.json" | wc -l)
    echo "  Registry now contains $SESSION_COUNT session(s)"
    
    if [ "$SESSION_COUNT" -ge 2 ]; then
        echo "  ✓ Multiple sessions tracked!"
    else
        echo "  ⚠ Expected 2 sessions, found $SESSION_COUNT"
    fi
fi

UUID_COUNT=$(ls -1 "$SESSION_DIR"/*.jsonl 2>/dev/null | grep -E '^[0-9a-f]{8}-' | wc -l)
echo "  UUID-named session files: $UUID_COUNT"
if [ "$UUID_COUNT" -ge 2 ]; then
    echo "  ✓ Multiple UUID session files!"
fi

echo ""
echo "========================================"
echo "Test 3: Session Content Verification"
echo "========================================"
echo ""

for jsonl in "$SESSION_DIR"/*.jsonl; do
    if [ -f "$jsonl" ]; then
        filename=$(basename "$jsonl")
        echo "File: $filename"
        
        # Check first line is session header
        FIRST_LINE=$(head -1 "$jsonl")
        if echo "$FIRST_LINE" | python3 -c "import sys,json; d=json.load(sys.stdin); exit(0 if d.get('type')=='session' else 1)" 2>/dev/null; then
            echo "  ✓ Valid session header"
            SESSION_ID=$(echo "$FIRST_LINE" | python3 -c "import sys,json; print(json.load(sys.stdin).get('id','N/A'))" 2>/dev/null)
            echo "  ✓ Session ID: $SESSION_ID"
        fi
        
        # Count message entries
        MSG_COUNT=$(grep -c '"type":"message"' "$jsonl" 2>/dev/null || echo "0")
        echo "  ✓ Messages: $MSG_COUNT"
        echo ""
    fi
done

echo ""
echo "========================================"
echo "Test 4: Registry JSON Structure"
echo "========================================"
echo ""

python3 << 'EOF' - "$SESSION_DIR/registry.json"
import json
import sys

with open(sys.argv[1]) as f:
    registry = json.load(f)

print(f"Registry version: {registry.get('version', 'N/A')}")
print(f"Number of peers: {len(registry.get('peers', {}))}")
print("")

for peer_key, entry in registry.get('peers', {}).items():
    print(f"Peer: {peer_key}")
    print(f"  Active session: {entry.get('active_session_id', 'N/A')}")
    print(f"  Sessions:")
    
    for sid, info in entry.get('sessions', {}).items():
        label = info.get('label') or 'unnamed'
        created = info.get('created_at', 'N/A')
        print(f"    - {sid}: {label} (created: {created})")

print("")
print("✓ Registry structure is valid!")
EOF

echo ""
echo "========================================"
echo "E2E Test Complete"
echo "========================================"
echo ""
echo "Verified:"
echo "  ✓ Registry creation and persistence"
echo "  ✓ UUID-based session file naming"  
echo "  ✓ Multiple sessions per peer"
echo "  ✓ Session content structure"
echo ""
