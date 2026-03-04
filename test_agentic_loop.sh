#!/bin/bash
# Test agentic loop with fresh environment
# Usage: ./test_agentic_loop.sh [agent_name] [message]

set -e

AGENT_NAME="${1:-testagent}"
MESSAGE="${2:-What's the latest news about Rust programming?}"

echo "=========================================="
echo "🧪 Pekobot Agentic Loop Test"
echo "=========================================="
echo "Agent: $AGENT_NAME"
echo "Message: $MESSAGE"
echo ""

# Source bashrc to get API keys
echo "📦 Sourcing ~/.bashrc for API keys..."
source ~/.bashrc 2>/dev/null || true

# Verify API key is set
if [ -z "$KIMI_API_KEY" ]; then
    echo "❌ Error: KIMI_API_KEY not found in environment"
    echo "   Please add 'export KIMI_API_KEY=...' to ~/.bashrc"
    exit 1
fi
echo "✅ API key found"
echo ""

# Build the binary
echo "🔨 Building Pekobot..."
source "$HOME/.cargo/env"
cd "$(dirname "$0")"
cargo build --bin pekobot 2>&1 | tail -5
echo "✅ Build complete"
echo ""

# Clean up previous test data
echo "🧹 Cleaning up previous test data..."
rm -rf ~/.pekobot/agents/"$AGENT_NAME"* 2>/dev/null || true
echo "✅ Clean complete"
echo ""

# Create new agent
echo "🤖 Creating new agent: $AGENT_NAME..."
./target/debug/pekobot agent create "$AGENT_NAME" --yes
echo "✅ Agent created"
echo ""

# Send test message
echo "💬 Sending test message..."
echo "   Message: $MESSAGE"
echo ""
./target/debug/pekobot agent start "$AGENT_NAME" -M "$MESSAGE" -v 2>>1
echo ""

# Check session file
echo "📁 Checking session file..."
SESSION_FILE=$(ls ~/.pekobot/agents/"$AGENT_NAME"/sessions/*.jsonl 2>/dev/null | head -1)
if [ -n "$SESSION_FILE" ]; then
    echo "✅ Session file created: $SESSION_FILE"
    echo ""
    echo "📊 Session entries:"
    head -3 "$SESSION_FILE" | python3 -c "
import sys, json
for line in sys.stdin:
    try:
        d = json.loads(line)
        print(f\"  - {d.get('type', 'unknown'):12} {d.get('id', '')[:30] if 'id' in d else ''}\")
    except:
        pass
" 2>/dev/null || head -3 "$SESSION_FILE"
else
    echo "⚠️  No session file found"
fi

echo ""
echo "=========================================="
echo "✅ Test complete!"
echo "=========================================="
