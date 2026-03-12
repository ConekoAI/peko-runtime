#!/bin/bash
# Final E2E Test for Session Registry

set -e

echo "========================================"
echo "Session Registry Final E2E Test"
echo "========================================"
echo ""

cd ~/pekora/projects/pekobot

# Cleanup previous test
rm -rf ~/.pekobot/agents/test_e2e_final
rm -rf ~/.local/share/pekobot/workspaces/test_e2e_final

# Create test agent
echo "Creating test agent..."
echo "assistant" | timeout 10s ./target/release/pekobot agent create test_e2e_final --provider kimi_code 2>&1 | tail -5 || true

SESSION_DIR="$HOME/.pekobot/agents/test_e2e_final/sessions"

echo ""
echo "========================================"
echo "Test 1: First Session"
echo "========================================"
echo ""

echo "Hello session 1" | timeout 15s ./target/release/pekobot agent start test_e2e_final 2>&1 | tail -8 || true

echo ""
echo "Files after first session:"
ls -la "$SESSION_DIR/" 2>/dev/null || echo "  (no sessions dir)"

echo ""
echo "========================================"
echo "Test 2: Second Session (--new)"
echo "========================================"
echo ""

echo "Hello session 2" | timeout 15s ./target/release/pekobot agent start test_e2e_final --new 2>&1 | tail -8 || true

echo ""
echo "Files after second session:"
ls -la "$SESSION_DIR/" 2>/dev/null || echo "  (no sessions dir)"

echo ""
echo "========================================"
echo "Test 3: Verify Registry"
echo "========================================"
echo ""

if [ -f "$SESSION_DIR/registry.json" ]; then
    echo "Registry contents:"
    python3 << 'EOF' - "$SESSION_DIR/registry.json"
import json
import sys
with open(sys.argv[1]) as f:
    data = json.load(f)
print(f"  Peers: {len(data.get('peers', {}))}")
for peer, entry in data.get('peers', {}).items():
    print(f"  Peer: {peer}")
    print(f"    Active: {entry.get('active_session_id', 'N/A')}")
    print(f"    Sessions: {len(entry.get('sessions', {}))}")
    for sid, info in entry.get('sessions', {}).items():
        print(f"      - {sid}")
EOF
else
    echo "  ✗ registry.json not found"
fi

echo ""
echo "========================================"
echo "Test 4: JSONL Files"
echo "========================================"
echo ""

UUID_COUNT=$(ls -1 "$SESSION_DIR"/*.jsonl 2>/dev/null | grep -E '[0-9a-f]{8}-' | wc -l)
echo "UUID-named JSONL files: $UUID_COUNT"

for f in "$SESSION_DIR"/*.jsonl; do
    if [ -f "$f" ]; then
        filename=$(basename "$f")
        if echo "$filename" | grep -qE '^[0-9a-f]{8}-'; then
            echo "  ✓ $filename (valid UUID)"
        fi
    fi
done

echo ""
echo "========================================"
echo "Cleanup"
echo "========================================"
rm -rf ~/.pekobot/agents/test_e2e_final
rm -rf ~/.local/share/pekobot/workspaces/test_e2e_final
echo "✓ Cleaned up"

echo ""
echo "========================================"
echo "Test Summary"
echo "========================================"
echo ""
if [ "$UUID_COUNT" -ge 1 ]; then
    echo "✓ UUID-based session files created"
else
    echo "✗ No UUID-based session files found"
fi

if [ -f "$SESSION_DIR/registry.json" ]; then
    SESSION_COUNT=$(grep -o '"session_id"' "$SESSION_DIR/registry.json" 2>/dev/null | wc -l)
    if [ "$SESSION_COUNT" -ge 1 ]; then
        echo "✓ Registry tracking sessions ($SESSION_COUNT session(s))"
    else
        echo "✗ Registry exists but no sessions tracked"
    fi
else
    echo "✗ Registry not created"
fi

echo ""
echo "E2E Test Complete!"
