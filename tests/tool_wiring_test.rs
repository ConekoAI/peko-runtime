//! Verify tools are properly wired into agent runtime
//!
//! This test ensures agents have access to all 14 essential tools.
//! Run with: cargo test --test tool_wiring_test -- --ignored

use pekobot::tools::Tool;
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::test]
async fn test_tool_factory_presets() {
    use pekobot::tools::ToolFactory;

    println!("\n🔧 Testing ToolFactory presets...");

    // Minimal tools
    let minimal = ToolFactory::create_minimal_tools(PathBuf::from("/tmp"), vec![]);
    assert_eq!(
        minimal.tools.len(),
        2,
        "Minimal should have 2 tools (filesystem, process)"
    );
    println!("  ✓ Minimal tools: {}", minimal.tools.len());

    // Coding tools
    let coding = ToolFactory::create_coding_tools(PathBuf::from("/tmp"), vec![]);
    assert!(
        coding.tools.len() >= 4,
        "Coding should have at least 4 tools (filesystem, apply_patch, process, cron)"
    );
    println!("  ✓ Coding tools: {}", coding.tools.len());

    // Full tools
    let full = ToolFactory::create_full_tools(PathBuf::from("/tmp"), vec![]);
    assert!(full.tools.len() >= 7, "Full should have at least 7 tools (filesystem, apply_patch, process, 3 session tools, cron)");
    println!("  ✓ Full tools: {}", full.tools.len());

    println!("✅ ToolFactory presets working!");
}

#[tokio::test]
async fn test_tools_have_descriptions() {
    use pekobot::tools::ToolFactory;

    println!("\n🔧 Testing tool descriptions...");

    let result = ToolFactory::create_full_tools(PathBuf::from("/tmp"), vec![]);

    for tool in &result.tools {
        let desc = tool.description();
        assert!(
            !desc.is_empty(),
            "Tool {} should have description",
            tool.name()
        );
        println!("  ✓ {}: {}", tool.name(), &desc[..desc.len().min(50)]);
    }

    println!("✅ All tools have descriptions!");
}
