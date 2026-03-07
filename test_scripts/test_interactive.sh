#!/bin/bash
set -e

# Get KIMI_API_KEY from .bashrc
export KIMI_API_KEY=$(grep "export KIMI_API_KEY=" ~/.bashrc | head -1 | sed 's/.*export KIMI_API_KEY="\(.*\)".*/\1/')

echo "========================================"
echo "Testing Interactive Session Commands"
echo "========================================"
echo ""

# Build
echo "Building Pekobot..."
source "$HOME/.cargo/env" && cargo build --bin pekobot 2>&1 | tail -3
echo ""

# Use existing testagent
echo "Using existing testagent..."
echo ""

echo "========================================"
echo "Test: Interactive mode with /sessions"
echo "========================================"
echo "Commands to type:"
echo "  1. /sessions  (list sessions)"
echo "  2. Hello      (first message)"
echo "  3. /sessions  (see updated list)"
echo "  4. /new       (reset session)"
echo "  5. Hello again (fresh session)"
echo "  6. quit       (exit)"
echo ""

./target/debug/pekobot agent start testagent

echo ""
echo "========================================"
echo "Test completed!"
echo "========================================"
