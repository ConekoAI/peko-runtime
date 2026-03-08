#!/bin/bash
set -e

# Session Stress Test - Concurrent Access
# Tests file locking under load

echo "========================================"
echo "Session Stress Test - Concurrent Access"
echo "========================================"
echo ""

PEKOBOT="./target/debug/pekobot"
TEST_AGENT="stress_test_agent"
TEST_DIR="$HOME/.pekobot/agents/$TEST_AGENT"

cleanup() {
    echo "Cleaning up..."
    rm -rf "$TEST_DIR"
    rm -f "$HOME/.pekobot/agents/${TEST_AGENT}.toml"
}

trap cleanup EXIT

# Build if needed
if [ ! -f "$PEKOBOT" ]; then
    echo "Building Pekobot..."
    source "$HOME/.cargo/env"
    cargo build --bin pekobot 2>&1 | tail -3
fi

echo "Creating test agent..."
$PEKOBOT agent create $TEST_AGENT --provider echo --yes 2>&1 | grep -E "(Created|Using)" || true

echo ""
echo "Running 10 concurrent sessions..."
echo ""

# Launch multiple concurrent sessions
for i in $(seq 1 10); do
    (echo "Message $i from concurrent session" | $PEKOBOT agent start $TEST_AGENT --new 2>&1) &
done

# Wait for all to complete
wait

echo ""
echo "========================================"
echo "Checking results..."
echo "========================================"
echo ""

# Count sessions
SESSION_COUNT=$(ls -1 "$TEST_DIR/sessions/"*.jsonl 2>/dev/null | wc -l)
echo "Sessions created: $SESSION_COUNT"

# Check for lock files (should be cleaned up)
LOCK_COUNT=$(ls -1 "$TEST_DIR/sessions/"*.lock 2>/dev/null | wc -l)
echo "Lock files remaining: $LOCK_COUNT"

if [ "$LOCK_COUNT" -gt 0 ]; then
    echo "⚠️  Warning: $LOCK_COUNT lock files not cleaned up"
    ls -la "$TEST_DIR/sessions/"*.lock
else
    echo "✅ All lock files cleaned up"
fi

# List sessions
echo ""
echo "Session list:"
$PEKOBOT session list --agent $TEST_AGENT 2>&1 | head -15

echo ""
echo "========================================"
echo "Stress test completed!"
echo "========================================"
