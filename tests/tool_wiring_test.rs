//! Verify tools are properly wired into agent runtime
//!
//! This test ensures agents have access to all 14 essential tools.
//! Run with: cargo test --test tool_wiring_test -- --ignored

use pekobot::manager::AgentManager;
use pekobot::tools::Tool;
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::test]
#[ignore = "Integration test - run manually"]
async fn test_agent_manager_creates_all_tools() {
    println!("\n🔧 Testing AgentManager tool creation...");

    // Create agent manager
    let (manager, _receiver) = AgentManager::new()
        .await
        .expect("Failed to create agent manager");

    // Create all tools for an agent
    let tools = manager.create_all_tools("did:peko:test-agent");

    // Verify we have the expected number of tools
    println!("  Created {} tools", tools.len());
    assert!(
        tools.len() >= 14,
        "Expected at least 14 tools, got {}",
        tools.len()
    );

    // Verify tool names
    let tool_names: Vec<String> = tools
        .iter()
        .map(|t: &Arc<dyn Tool>| t.name().to_string())
        .collect();

    println!("  Tool names: {:?}", tool_names);

    // Core tools
    assert!(
        tool_names.contains(&"filesystem".to_string()),
        "Missing filesystem tool"
    );
    assert!(
        tool_names.contains(&"process".to_string()),
        "Missing process tool"
    );
    assert!(
        tool_names.contains(&"http".to_string()),
        "Missing http tool"
    );
    assert!(
        tool_names.contains(&"fetch".to_string()),
        "Missing fetch tool"
    );
    assert!(
        tool_names.contains(&"web_search".to_string()),
        "Missing web_search tool"
    );
    assert!(
        tool_names.contains(&"apply_patch".to_string()),
        "Missing apply_patch tool"
    );
    assert!(
        tool_names.contains(&"browser".to_string()),
        "Missing browser tool"
    );

    // Session introspection
    assert!(
        tool_names.contains(&"sessions_list".to_string()),
        "Missing sessions_list tool"
    );
    assert!(
        tool_names.contains(&"sessions_history".to_string()),
        "Missing sessions_history tool"
    );
    assert!(
        tool_names.contains(&"session_status".to_string()),
        "Missing session_status tool"
    );
    assert!(
        tool_names.contains(&"session_messaging".to_string()),
        "Missing session_messaging tool"
    );

    // Agent management
    assert!(
        tool_names.contains(&"agents_list".to_string()),
        "Missing agents_list tool"
    );
    assert!(
        tool_names.contains(&"agent_info".to_string()),
        "Missing agent_info tool"
    );
    assert!(
        tool_names.contains(&"agent_spawn".to_string()),
        "Missing agent_spawn tool"
    );
    assert!(
        tool_names.contains(&"agent_broadcast".to_string()),
        "Missing agent_broadcast tool"
    );

    println!("✅ All 14 tools verified in AgentManager!");
}

#[tokio::test]
async fn test_tool_factory_presets() {
    use pekobot::tools::ToolFactory;

    println!("\n🔧 Testing ToolFactory presets...");

    // Minimal tools
    let minimal = ToolFactory::create_minimal_tools(PathBuf::from("/tmp"));
    assert_eq!(
        minimal.len(),
        2,
        "Minimal should have 2 tools (filesystem, process)"
    );
    println!("  ✓ Minimal tools: {}", minimal.len());

    // Coding tools
    let coding = ToolFactory::create_coding_tools(PathBuf::from("/tmp"));
    assert!(coding.len() >= 5, "Coding should have at least 5 tools");
    println!("  ✓ Coding tools: {}", coding.len());

    // Full tools
    let full = ToolFactory::create_full_tools(PathBuf::from("/tmp"));
    assert!(full.len() >= 10, "Full should have at least 10 tools");
    println!("  ✓ Full tools: {}", full.len());

    println!("✅ ToolFactory presets working!");
}

#[tokio::test]
async fn test_tools_have_descriptions() {
    use pekobot::tools::ToolFactory;

    println!("\n🔧 Testing tool descriptions...");

    let tools = ToolFactory::create_full_tools(PathBuf::from("/tmp"));

    for tool in &tools {
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
