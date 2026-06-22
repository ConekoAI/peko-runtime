#!/bin/bash
# Module Boundary Check Script
# Usage: ./scripts/check_module_boundaries.sh
#
# Enforces the dependency rules from Issue 015 and Issue 020:
# 1. src/extensions/framework/ must NOT import from concrete extension types
#    (src/extensions/<type>/ where <type> != framework).
# 2. src/extensions/<type>/ should NOT import from src/extensions/<other_type>/.
# 3. src/extensions/framework/core/ must NOT import from src/daemon/ or src/tools/
#    (except tools::core, the established one-way dep).
# 4. src/commands/ should NOT import from low-level persistence/packaging modules
#    (advisory while the command layer is being cleaned up).
# 5. src/extensions/framework/ must NOT import from src/agents/, src/tunnel/, or
#    src/daemon/.

set -e

cd "$(dirname "$0")/.."

EXIT_CODE=0

EXTENSION_TYPES=(builtin gateway general mcp skill universal)

# ---------------------------------------------------------------------------
# Whether Rule 4 (commands -> persistence/packaging) is a hard gate.
# Set to 0 (advisory) until existing violations are resolved; flip to 1 once
# the command layer no longer reaches past the service boundary.
# ---------------------------------------------------------------------------
RULE4_HARD_GATE=0

echo "=========================================="
echo "Module Boundary Check (Issue 015 / 020)"
echo "=========================================="
echo ""

# -----------------------------------------------------------------------------
# Rule 1: src/extensions/framework/ must NOT import from concrete extension types
# -----------------------------------------------------------------------------
echo "Rule 1: src/extensions/framework/ must NOT import from src/extensions/<type>/"
echo ""

RULE1_FAILED=0

for type_dir in "${EXTENSION_TYPES[@]}"; do
    VIOLATIONS_1=$(grep -rE "^[[:space:]]*use crate::extensions::${type_dir}::" src/extensions/framework/ --include="*.rs" 2>/dev/null || true)

    if [ -n "$VIOLATIONS_1" ]; then
        if [ "$RULE1_FAILED" -eq 0 ]; then
            echo "  ❌ FAIL: src/extensions/framework/ imports from concrete extension types"
            echo ""
            RULE1_FAILED=1
        fi
        echo "    src/extensions/framework/ → crate::extensions::${type_dir}::"
        echo "$VIOLATIONS_1" | while read -r line; do
            echo "       $line"
        done
        echo ""
    fi
done

# Also catch non-use references (e.g. in code) while excluding doc comments.
VIOLATIONS_1B=$(grep -r "crate::extensions::" src/extensions/framework/ --include="*.rs" 2>/dev/null \
    | grep -vE "crate::extensions::framework::" \
    | grep -vE "crate::extensions::\*" \
    | grep -vE ':[[:space:]]*://' \
    | grep -vE ':[[:space:]]*//' \
    | grep -vE ':[[:space:]]*///?' \
    | grep -vE '^[[:space:]]*//' \
    || true)

if [ -n "$VIOLATIONS_1B" ]; then
    if [ "$RULE1_FAILED" -eq 0 ]; then
        echo "  ❌ FAIL: src/extensions/framework/ references concrete extension types"
        echo ""
        RULE1_FAILED=1
    fi
    echo "$VIOLATIONS_1B" | while read -r line; do
        echo "     $line"
    done
    echo ""
fi

if [ "$RULE1_FAILED" -eq 0 ]; then
    echo "  ✓ PASS: No forbidden imports found"
else
    EXIT_CODE=1
fi
echo ""

# -----------------------------------------------------------------------------
# Rule 2: src/extensions/<type>/ should NOT import from src/extensions/<other_type>/
# -----------------------------------------------------------------------------
echo "Rule 2: src/extensions/<type>/ should NOT import from src/extensions/<other_type>/"
echo ""

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
# Rule 3: src/extensions/framework/core/ must NOT import from src/daemon/ or src/tools/
#        (tools::core is the one allowed one-way dep)
# -----------------------------------------------------------------------------
echo "Rule 3: src/extensions/framework/core/ must NOT import from src/daemon/ or src/tools/ (except tools::core)"
echo ""

VIOLATIONS_3A=$(grep -r "crate::daemon::" src/extensions/framework/core/ --include="*.rs" 2>/dev/null || true)
VIOLATIONS_3B=$(grep -rE "crate::tools::(builtin|registry|factory)" src/extensions/framework/core/ --include="*.rs" 2>/dev/null || true)

if [ -n "$VIOLATIONS_3A" ] || [ -n "$VIOLATIONS_3B" ]; then
    echo "  ❌ FAIL: src/extensions/framework/core/ imports from forbidden modules (daemon, tools::builtin, tools::registry, tools::factory)"
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
# Rule 4: src/commands/ should NOT import from low-level persistence/packaging
# -----------------------------------------------------------------------------
if [ "$RULE4_HARD_GATE" -eq 1 ]; then
    echo "Rule 4: src/commands/ must NOT import from persistence/packaging modules"
else
    echo "Rule 4: src/commands/ should NOT import from persistence/packaging modules (advisory)"
fi
echo ""

# Patterns considered low-level persistence/packaging from the command layer.
# The command layer should delegate to services instead.
PERSISTENCE_PATTERNS=(
    "crate::registry::packaging::"
    "crate::common::services::config_authority::"
    "crate::identity::storage::"
    "crate::session::jsonl::"
    "crate::session::metadata_controller::"
)

RULE4_FAILED=0

for pattern in "${PERSISTENCE_PATTERNS[@]}"; do
    # Convert pattern prefix to a grep-safe regex fragment
    regex_pattern="^.*use ${pattern}"
    VIOLATIONS_4=$(grep -rE "$regex_pattern" src/commands/ --include="*.rs" 2>/dev/null || true)

    if [ -n "$VIOLATIONS_4" ]; then
        if [ "$RULE4_FAILED" -eq 0 ]; then
            if [ "$RULE4_HARD_GATE" -eq 1 ]; then
                echo "  ❌ FAIL: Commands import from persistence/packaging modules"
            else
                echo "  ⚠️  WARNING: Commands import from persistence/packaging modules"
            fi
            echo ""
            RULE4_FAILED=1
        fi
        echo "  Pattern: $pattern"
        echo "$VIOLATIONS_4" | while read -r line; do
            echo "     $line"
        done
        echo ""
    fi
done

if [ "$RULE4_FAILED" -eq 1 ]; then
    if [ "$RULE4_HARD_GATE" -eq 1 ]; then
        EXIT_CODE=1
    fi
else
    echo "  ✓ PASS: No persistence/packaging imports found"
fi
echo ""

# -----------------------------------------------------------------------------
# Rule 5: src/extensions/framework/ must NOT import from src/agents/, src/tunnel/, or src/daemon/
# -----------------------------------------------------------------------------
echo "Rule 5: src/extensions/framework/ must NOT import from src/agents/, src/tunnel/, or src/daemon/"
echo ""

VIOLATIONS_5A=$(grep -rE "crate::(agents|tunnel|daemon)::" src/extensions/framework/ --include="*.rs" 2>/dev/null || true)

if [ -n "$VIOLATIONS_5A" ]; then
    echo "  ❌ FAIL: src/extensions/framework/ imports from agents/tunnel/daemon"
    echo ""
    echo "$VIOLATIONS_5A" | while read -r line; do
        echo "     $line"
    done
    echo ""
    EXIT_CODE=1
else
    echo "  ✓ PASS: No forbidden cross-domain imports"
fi
echo ""

# -----------------------------------------------------------------------------
# Summary
# -----------------------------------------------------------------------------
echo "=========================================="
echo "Summary"
echo "=========================================="

if [ "$EXIT_CODE" -eq 0 ]; then
    if [ "$RULE4_FAILED" -eq 1 ]; then
        echo "✓ All hard boundary checks passed; advisory warnings remain"
    else
        echo "✓ All module boundary checks passed"
    fi
else
    echo "❌ Module boundary violations detected"
    echo ""
    echo "Fix guidance:"
    echo "  - Framework code (src/extensions/framework/) must not depend on concrete extension types"
    echo "  - Extension types should depend on the framework, not each other"
    echo "  - src/extensions/framework/core/ must not depend on daemon/ or tools/ (except tools::core)"
    echo "  - Commands should delegate persistence/packaging work to services"
    echo "  - src/extensions/framework/ must not depend on agents/, tunnel/, or daemon/"
fi

exit $EXIT_CODE
