#!/bin/bash
# Pekobot Stress Test Suite for Kimi API
# Run with: ./stress_test_kimi.sh YOUR_API_KEY

set -e

API_KEY=${1:-$KIMI_API_KEY}

if [ -z "$API_KEY" ]; then
    echo "❌ Error: No API key provided"
    echo "Usage: ./stress_test_kimi.sh YOUR_API_KEY"
    echo "   or: export KIMI_API_KEY=YOUR_API_KEY && ./stress_test_kimi.sh"
    exit 1
fi

echo "🐰 Pekobot Kimi Stress Test Suite"
echo "=================================="
echo "API Key: ${API_KEY:0:20}..."
echo ""

# Set the key for all tests
export KIMI_API_KEY="$API_KEY"
export MOONSHOT_API_KEY="$API_KEY"

cd /home/ubuntu/pekora/projects/pekobot

echo "Test 1: Basic API Connectivity"
echo "-------------------------------"
cargo run --example kimi_api_test 2>/dev/null && echo "✅ PASSED" || echo "❌ FAILED"
echo ""

echo "Test 2: Agentic Loop with Tool Calling"
echo "---------------------------------------"
cargo run --example agentic_loop_test 2>/dev/null && echo "✅ PASSED" || echo "❌ FAILED"
echo ""

echo "Test 3: Provider Direct Test (if available)"
echo "--------------------------------------------"
# This would test the provider directly
cargo test --lib kimi:: 2>/dev/null && echo "✅ PASSED" || echo "❌ FAILED (or no tests)"
echo ""

echo "=================================="
echo "Stress test complete!"
