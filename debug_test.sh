#!/bin/bash
# Debug test script

source $HOME/.cargo/env
cd /home/ubuntu/pekora/projects/pekobot

echo "Building..."
cargo build --bin pekobot 2>&1 | tail -3

echo ""
echo "Creating test agent..."
rm -rf ~/.pekobot/agents/debugtest ~/.pekobot/agents/debugtest.toml
# Use expect or just create config directly
mkdir -p ~/.pekobot/agents
cat > ~/.pekobot/agents/debugtest.toml << 'EOF'
name = "debugtest"
did = "did:pekobot:local:default:debugtest123"
provider = { provider_type = "kimi_code", model = "kimi-k2.5" }
memory = { enabled = true }
EOF

echo ""
echo "Running test..."
echo "Hello" | RUST_LOG=trace timeout 20 ./target/debug/pekobot agent start debugtest 2>&1 | head -100
