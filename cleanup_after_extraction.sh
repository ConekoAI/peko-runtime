#!/bin/bash
# Clean up Pekobot after tool extraction
# Moves tools to tool_bundle and updates Pekobot to reference them

set -e

PEKOBOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
TOOL_BUNDLE_DIR="$PEKOBOT_DIR/../tool_bundle"
BACKUP_DIR="$PEKOBOT_DIR/.backup/tools"

echo "🧹 Pekobot Repository Cleanup"
echo "=============================="
echo ""

cd "$PEKOBOT_DIR"

# Create backup directory
mkdir -p "$BACKUP_DIR"

# Tools to move (on-demand tools, not core)
TOOLS_TO_MOVE=(
    "calendar"
    "document"
    "email"
    "expense"
    "inventory"
    "research"
    "social_media"
)

echo "Step 1: Backing up tool files..."
for tool in "${TOOLS_TO_MOVE[@]}"; do
    if [ -f "src/tools/${tool}.rs" ]; then
        cp "src/tools/${tool}.rs" "$BACKUP_DIR/"
        echo "  ✓ Backed up ${tool}.rs"
    fi
done

echo ""
echo "Step 2: Checking tool_bundle has the tools..."
for tool in "${TOOLS_TO_MOVE[@]}"; do
    if [ -d "$TOOL_BUNDLE_DIR/$tool" ]; then
        echo "  ✓ $tool exists in tool_bundle"
    else
        echo "  ⚠️  $tool MISSING from tool_bundle - copy before removing!"
    fi
done

echo ""
echo "Step 3: Optional - Remove tools from Pekobot core"
echo ""
echo "Tools can be removed completely from Pekobot since they are now:"
echo "  1. In tool_bundle/ for local builds"
echo "  2. Available on Pekohub for downloads"
echo ""
read -p "Remove tool files from Pekobot src/tools/? (y/N) " -n 1 -r
echo

if [[ $REPLY =~ ^[Yy]$ ]]; then
    for tool in "${TOOLS_TO_MOVE[@]}"; do
        if [ -f "src/tools/${tool}.rs" ]; then
            rm "src/tools/${tool}.rs"
            echo "  ✓ Removed src/tools/${tool}.rs"
        fi
    done
    
    # Update mod.rs to remove the optional tool declarations
    echo ""
    echo "Updating src/tools/mod.rs..."
    cat > src/tools/mod.rs <> 'EOF'
//! Tools for agents
//! 
//! Core tools only. On-demand tools are downloaded from Pekohub or installed locally.

pub mod browser;
pub mod filesystem;
pub mod http;
pub mod memory_tool;
pub mod process;
pub mod session_messaging;
pub mod traits;

pub use browser::BrowserTool;
pub use filesystem::FileSystemTool;
pub use http::{HttpMethod, HttpTool};
pub use memory_tool::{MemoryTool, MemoryToolFactory};
pub use process::ProcessTool;
pub use session_messaging::{SessionMessagingTool, SessionRegistry};
pub use traits::Tool;
EOF

    echo "  ✓ Updated mod.rs (core tools only)"
    
    # Update Cargo.toml to remove tool feature flags
    echo ""
    echo "Removing tool feature flags from Cargo.toml..."
    sed -i '/^\[features\]/,/^\[/{ /bundled-tools/d; /calendar/d; /document/d; /email/d; /expense/d; /inventory/d; /research/d; /social_media/d; /full/d; }' Cargo.toml
    sed -i '/^\[features\]$/d' Cargo.toml
    echo "  ✓ Removed feature flags"
    
else
    echo "  Skipped removal (tools remain with feature flags)"
fi

echo ""
echo "Step 4: Cleaning up test files..."
# Move tool-specific tests to tool_bundle
mkdir -p "$TOOL_BUNDLE_DIR/tests"
for tool in "${TOOLS_TO_MOVE[@]}"; do
    if [ -f "tests/${tool}_test.rs" ]; then
        cp "tests/${tool}_test.rs" "$TOOL_BUNDLE_DIR/tests/"
        rm "tests/${tool}_test.rs"
        echo "  ✓ Moved tests/${tool}_test.rs"
    fi
done

echo ""
echo "Step 5: Final check..."
echo ""
echo "Remaining in src/tools/:"
ls -1 src/tools/*.rs 2>/dev/null | xargs -n1 basename || echo "  (none)"

echo ""
echo "✅ Cleanup complete!"
echo ""
echo "Summary:"
echo "  - Tools backed up to: $BACKUP_DIR"
echo "  - Tools available in: $TOOL_BUNDLE_DIR"
echo ""
echo "Next steps:"
echo "  1. Verify Pekobot builds: cargo check"
echo "  2. Build tool_bundle: cd tool_bundle && cargo build --release"
echo "  3. Install tools: ./tool_bundle/install.sh"
echo ""
echo "To restore tools if needed:"
echo "  cp .backup/tools/*.rs src/tools/"
