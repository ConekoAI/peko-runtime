#!/bin/bash
# Test agentic loop with fresh environment
# Usage: ./test_agentic_loop.sh [agent_name] [message]

set -e

AGENT_NAME="${1:-testagent}"
MESSAGE="${2:-What is the latest news about Rust programming?}"

echo "=========================================="
echo "🧪 Pekobot Agentic Loop Test"
echo "=========================================="
echo "Agent: $AGENT_NAME"
echo "Message: $MESSAGE"
echo ""

# Load API keys
echo "📦 Loading API keys..."
KIMI_API_KEY=$(grep "export KIMI_API_KEY=" ~/.bashrc 2>/dev/null | head -1 | sed 's/.*export KIMI_API_KEY="\(.*\)".*/\1/')
export KIMI_API_KEY

if [ -z "$KIMI_API_KEY" ]; then
    echo "❌ Error: KIMI_API_KEY not found in ~/.bashrc"
    exit 1
fi
echo "✅ API key found"
echo ""

# Build the binary
echo "🔨 Building Pekobot..."
source "$HOME/.cargo/env"
cd "$(dirname "$0")"
cargo build --bin pekobot 2>&1 | tail -3
echo "✅ Build complete"
echo ""

# Clean up previous test data
echo "🧹 Cleaning up previous test data..."
rm -rf ~/.pekobot/agents/"$AGENT_NAME"* 2>/dev/null || true
echo "✅ Clean complete"
echo ""

# Create agent config directly (skip interactive bootstrap)
echo "🤖 Creating agent config: $AGENT_NAME..."
mkdir -p ~/.pekobot/agents/"$AGENT_NAME"
mkdir -p ~/.pekobot/workspaces/"$AGENT_NAME"

# Create TOML config with env var substitution
TOML_FILE="$HOME/.pekobot/agents/$AGENT_NAME.toml"
cat > "$TOML_FILE" << TOML_EOF
name = "$AGENT_NAME"
description = "Test agent for agentic loop"
capabilities = []
auto_accept_trusted = false
approval_threshold = 100.0
default_timeout_seconds = 300

[provider]
provider_type = "kimi_code"
api_key_env = "KIMI_API_KEY"
default_model = "default"
timeout_seconds = 60
max_retries = 3
retry_delay_ms = 1000

[provider.models.default]
name = "k2p5"
max_tokens = 4096
temperature = 0.7
top_p = 1.0
presence_penalty = 0.0
frequency_penalty = 0.0

[prompt]
mode = "full"

workspace = "~/.pekobot/workspaces/$AGENT_NAME"
TOML_EOF

# Create minimal workspace files
AGENTS_MD="$HOME/.pekobot/workspaces/$AGENT_NAME/AGENTS.md"
cat > "$AGENTS_MD" << 'AGENTS_EOF'
# Test Agent

## Role
You are a helpful AI assistant running in Pekobot.

## Instructions
Think step by step. Use tools when needed.

## Tool Use Format
When you need to use a tool, output JSON with content blocks:
```json
{"content": [{"type": "thinking", "thinking": "Let me search..."}, {"type": "tool_call", "id": "call_1", "name": "web_search", "arguments": {"query": "..."}}]}
```
AGENTS_EOF

TOOLS_MD="$HOME/.pekobot/workspaces/$AGENT_NAME/TOOLS.md"
cat > "$TOOLS_MD" << 'TOOLS_EOF'
# Tools

## web_search
Search the web for information.

## filesystem
Read and write files.

## process
Execute shell commands.

## fetch
Fetch content from URLs.
TOOLS_EOF

echo "✅ Agent created"
echo ""

# Send test message
echo "💬 Sending test message..."
echo "   Message: $MESSAGE"
echo ""
./target/debug/pekobot agent start "$AGENT_NAME" -M "$MESSAGE" 2>&1
echo ""

# Check session file
echo "📁 Checking session file..."
SESSION_FILE=$(ls ~/.pekobot/agents/"$AGENT_NAME"/sessions/*.jsonl 2>/dev/null | head -1)
if [ -n "$SESSION_FILE" ]; then
    echo "✅ Session file created: $SESSION_FILE"
    echo ""
    echo "📊 First 3 entries:"
    head -3 "$SESSION_FILE" | python3 -c "
import sys, json
for line in sys.stdin:
    try:
        d = json.loads(line)
        t = d.get('type', 'unknown')
        id_short = d.get('id', '')[:20] if 'id' in d else ''
        print(f'  - {t:15} {id_short}')
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
