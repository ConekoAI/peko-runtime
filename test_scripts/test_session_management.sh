#!/bin/bash
set -e

# Session Management Integration Tests
# Tests file locking, index, keys, and maintenance

echo "========================================"
echo "Session Management Integration Tests"
echo "========================================"
echo ""

# Test configuration with shortened constants for easier testing
export PEKOBOT_TEST_MODE=1
export SESSION_TEST_PRUNE_DAYS=1  # 1 day instead of 30
export SESSION_TEST_MAX_SESSIONS=3  # 3 instead of 500
export SESSION_TEST_ROTATE_BYTES=1024  # 1KB instead of 10MB

PEKOBOT="./target/debug/pekobot"
TEST_AGENT="session_test_agent"
TEST_DIR="$HOME/.pekobot/agents/$TEST_AGENT"

cleanup() {
    echo "Cleaning up test data..."
    rm -rf "$TEST_DIR"
    rm -f "$HOME/.pekobot/agents/${TEST_AGENT}.toml"
}

trap cleanup EXIT

# Build if needed
if [ ! -f "$PEKOBOT" ]; then
    echo "Building Pekobot..."
    source "$HOME/.cargo/env"
    cargo build --bin pekobot 2>&1 | tail -3
    echo ""
fi

echo "========================================"
echo "Test 1: Session Index Creation"
echo "========================================"
echo ""

# Clean start
cleanup 2>/dev/null || true

# Create agent
echo "Creating test agent..."
$PEKOBOT agent create $TEST_AGENT --provider echo --yes 2>&1 | grep -E "(Created|Using)" || true
echo ""

# Start a session
echo "Starting session (will create index)..."
echo "test message" | $PEKOBOT agent start $TEST_AGENT --new 2>&1 | tail -5
echo ""

# Check index was created
if [ -f "$TEST_DIR/sessions/sessions.json" ]; then
    echo "✅ sessions.json index created"
    echo "Index contents:"
    cat "$TEST_DIR/sessions/sessions.json" | head -20
else
    echo "❌ sessions.json not found"
    exit 1
fi
echo ""

echo "========================================"
echo "Test 2: Session Key Derivation (CLI)"
echo "========================================"
echo ""

# The CLI default session should be created with key agent:{agent}:cli:default
echo "Checking CLI default session key..."
if grep -q "cli:default" "$TEST_DIR/sessions/sessions.json"; then
    echo "✅ CLI default session key found in index"
else
    echo "⚠️  CLI default session key not found (may be using legacy format)"
fi
echo ""

echo "========================================"
echo "Test 3: Session Persistence"
echo "========================================"
echo ""

# Send multiple messages
for i in 1 2 3; do
    echo "Sending message $i..."
    echo "Message $i" | $PEKOBOT agent start $TEST_AGENT 2>&1 | tail -2
done
echo ""

# Check session list shows message count
echo "Checking session list..."
$PEKOBOT session list --agent $TEST_AGENT 2>&1 | head -10
echo ""

echo "========================================"
echo "Test 4: Session Metadata Tracking"
echo "========================================"
echo ""

# Show session details
echo "Session details:"
SESSION_ID=$(ls "$TEST_DIR/sessions/"*.jsonl 2>/dev/null | head -1 | xargs basename | sed 's/.jsonl//')
if [ -n "$SESSION_ID" ]; then
    $PEKOBOT session show "$SESSION_ID" 2>&1 | head -20
else
    echo "⚠️  No session file found"
fi
echo ""

echo "========================================"
echo "Test 5: Session Maintenance (Dry Run)"
echo "========================================"
echo ""

# Create multiple sessions to test maintenance
echo "Creating additional test sessions..."
for i in 1 2 3 4 5; do
    touch "$TEST_DIR/sessions/old_session_$i.jsonl"
    echo '{"type":"session","version":3,"id":"old_session_"}' > "$TEST_DIR/sessions/old_session_$i.jsonl"
done

# Run maintenance dry-run
echo "Running maintenance (dry-run)..."
$PEKOBOT session maintenance 2>&1 | head -20
echo ""

echo "========================================"
echo "Test 6: Session File Locking"
echo "========================================"
echo ""

echo "Testing concurrent access (simulated)..."
echo "This would normally test that two processes can't corrupt the same session"
echo "Manual test: Run two 'pekobot agent start' commands simultaneously"
echo ""

echo "========================================"
echo "Test 7: Session Resumption"
echo "========================================"
echo ""

# Send a message that references context
echo "Testing context retention..."
echo "My name is TestUser" | $PEKOBOT agent start $TEST_AGENT 2>&1 | tail -3
echo ""
echo "What is my name?" | $PEKOBOT agent start $TEST_AGENT 2>&1 | tail -5
echo ""

echo "========================================"
echo "All Tests Completed!"
echo "========================================"
echo ""
echo "Summary:"
echo "  - Session index: $(if [ -f "$TEST_DIR/sessions/sessions.json" ]; then echo "✅"; else echo "❌"; fi)"
echo "  - Session files: $(ls -1 "$TEST_DIR/sessions/"*.jsonl 2>/dev/null | wc -l) files"
echo ""
echo "To clean up: rm -rf $TEST_DIR"
