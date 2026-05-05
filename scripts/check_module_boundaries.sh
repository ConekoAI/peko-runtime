#!/bin/bash
# Module Boundary Check Script
# Usage: ./scripts/check_module_boundaries.sh
#
# Enforces the dependency rules from Issue 015:
# 1. src/extension/ must NOT import from src/extensions/
# 2. src/extensions/<type>/ should NOT import from src/extensions/<other_type>/

set -e

cd "$(dirname "$0")/.."

EXIT_CODE=0

echo "=========================================="
echo "Module Boundary Check (Issue 015)"
echo "=========================================="
echo ""

# -----------------------------------------------------------------------------
# Rule 1: src/extension/ must NOT import from src/extensions/
# -----------------------------------------------------------------------------
echo "Rule 1: src/extension/ must NOT import from src/extensions/"
echo ""

VIOLATIONS_1=$(grep -r "use crate::extensions::" src/extension/ --include="*.rs" 2>/dev/null || true)
VIOLATIONS_1B=$(grep -r "crate::extensions::" src/extension/ --include="*.rs" 2>/dev/null | grep -v "use crate::extensions::" || true)

if [ -n "$VIOLATIONS_1" ] || [ -n "$VIOLATIONS_1B" ]; then
    echo "  ❌ FAIL: src/extension/ imports from src/extensions/"
    echo ""
    if [ -n "$VIOLATIONS_1" ]; then
        echo "$VIOLATIONS_1" | while read -r line; do
            echo "     $line"
        done
    fi
    if [ -n "$VIOLATIONS_1B" ]; then
        echo "$VIOLATIONS_1B" | while read -r line; do
            echo "     $line"
        done
    fi
    echo ""
    EXIT_CODE=1
else
    echo "  ✓ PASS: No forbidden imports found"
fi
echo ""

# -----------------------------------------------------------------------------
# Rule 2: src/extensions/<type>/ should NOT import from src/extensions/<other_type>/
# -----------------------------------------------------------------------------
echo "Rule 2: src/extensions/<type>/ should NOT import from src/extensions/<other_type>/"
echo ""

EXTENSION_TYPES=(builtin gateway general mcp skill universal)
RULE2_FAILED=0

for type_dir in "${EXTENSION_TYPES[@]}"; do
    if [ ! -d "src/extensions/$type_dir" ]; then
        continue
    fi
    
    for other_type in "${EXTENSION_TYPES[@]}"; do
        if [ "$type_dir" = "$other_type" ]; then
            continue
        fi
        
        # Check for imports from other extension types
        VIOLATIONS_2=$(grep -r "crate::extensions::$other_type::" "src/extensions/$type_dir/" --include="*.rs" 2>/dev/null || true)
        
        if [ -n "$VIOLATIONS_2" ]; then
            if [ "$RULE2_FAILED" -eq 0 ]; then
                echo "  ❌ FAIL: Cross-extension imports found"
                echo ""
                RULE2_FAILED=1
            fi
            echo "    src/extensions/$type_dir/ → crate::extensions::$other_type::"
            echo "$VIOLATIONS_2" | while read -r line; do
                echo "       $line"
            done
            echo ""
            EXIT_CODE=1
        fi
    done
done

if [ "$RULE2_FAILED" -eq 0 ]; then
    echo "  ✓ PASS: No cross-extension imports found"
fi
echo ""

# -----------------------------------------------------------------------------
# Rule 3: src/extension/core/ must NOT import from src/daemon/ or src/tools/
# -----------------------------------------------------------------------------
echo "Rule 3: src/extension/core/ must NOT import from src/daemon/ or src/tools/"
echo ""

VIOLATIONS_3A=$(grep -r "crate::daemon::" src/extension/core/ --include="*.rs" 2>/dev/null || true)
VIOLATIONS_3B=$(grep -r "crate::tools::" src/extension/core/ --include="*.rs" 2>/dev/null || true)

if [ -n "$VIOLATIONS_3A" ] || [ -n "$VIOLATIONS_3B" ]; then
    echo "  ❌ FAIL: src/extension/core/ imports from forbidden modules"
    echo ""
    if [ -n "$VIOLATIONS_3A" ]; then
        echo "$VIOLATIONS_3A" | while read -r line; do
            echo "     $line"
        done
    fi
    if [ -n "$VIOLATIONS_3B" ]; then
        echo "$VIOLATIONS_3B" | while read -r line; do
            echo "     $line"
        done
    fi
    echo ""
    EXIT_CODE=1
else
    echo "  ✓ PASS: No forbidden imports found"
fi
echo ""

# -----------------------------------------------------------------------------
# Summary
# -----------------------------------------------------------------------------
echo "=========================================="
echo "Summary"
echo "=========================================="

if [ "$EXIT_CODE" -eq 0 ]; then
    echo "✓ All module boundary checks passed"
else
    echo "❌ Module boundary violations detected"
    echo ""
    echo "Fix guidance:"
    echo "  - Framework code (src/extension/) must not depend on extension types"
    echo "  - Extension types should depend on the framework, not each other"
    echo "  - Move shared code to src/extension/ or use trait abstractions"
fi

exit $EXIT_CODE
