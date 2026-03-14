#!/bin/bash
# Code Quality Check Script
# Usage: ./scripts/code_quality_check.sh

set -e

cd "$(dirname "$0")/.."

echo "=========================================="
echo "Code Quality Assessment"
echo "=========================================="
echo ""

# Check test status
echo "1. Running tests..."
cargo test --lib --quiet 2>&1 | tail -3
echo ""

# Count warnings
echo "2. Counting clippy warnings..."
total_warnings=$(cargo clippy --lib 2>&1 | grep -c "^warning:" || true)
unused_async=$(cargo clippy --lib 2>&1 | grep -c "unused_async" || true)
dead_code=$(cargo clippy --lib 2>&1 | grep -c "dead_code" || true)
fixable=$(cargo clippy --lib 2>&1 | grep -oE "[0-9]+ suggestions" | grep -oE "[0-9]+" || true)

echo "   Total warnings: $total_warnings"
echo "   Unused async: $unused_async"
echo "   Dead code: $dead_code"
echo "   Auto-fixable: $fixable"
echo ""

# Files with most warnings
echo "3. Top 10 files with warnings:"
cargo clippy --lib 2>&1 | grep -E "^  --> src/" | sed 's|  --> src/||' | cut -d':' -f1 | sort | uniq -c | sort -rn | head -10
echo ""

# Check formatting
echo "4. Checking formatting..."
if cargo fmt -- --check 2>&1 | grep -q "Diff"; then
    echo "   ⚠ Formatting issues found. Run: cargo fmt"
else
    echo "   ✓ Formatting OK"
fi
echo ""

# Summary
echo "=========================================="
echo "Summary"
echo "=========================================="
echo "Total warnings: $total_warnings"
echo "Critical issues: $((unused_async + dead_code))"
echo ""
echo "Next steps:"
echo "  1. Run: cargo clippy --fix --lib -p pekobot"
echo "  2. Manually fix remaining warnings"
echo "  3. See: issues/REFACTOR-002-code-quality-plan.md"
echo ""
