//! MCP Integration Tests
//!
//! These tests verify MCP server discovery, connection, and tool loading.

use std::path::PathBuf;

/// Test that ToolFactory can create tools with MCP disabled
#[tokio::test]
async fn test_tool_factory_core_only() {
    use pekobot::tools::{ToolFactory, ToolFactoryConfig};

    let config = ToolFactoryConfig {
        workspace_dir: PathBuf::from("."),
        mcp: pekobot::tools::McpFactoryConfig {
            enabled: false,
            ..Default::default()
        },
        ..Default::default()
    };

    let (tools, discovery) = ToolFactory::create_tools_async(&config).await.unwrap();

    // Should have core tools
    assert!(!tools.is_empty(), "Should have core tools");
    
    // MCP should not be used
    assert!(!discovery.has_mcp_tools());
    assert_eq!(discovery.servers_found, 0);
}

/// Test that ToolFactory attempts MCP discovery when enabled
#[tokio::test]
async fn test_tool_factory_mcp_enabled_no_config() {
    use pekobot::tools::{ToolFactory, ToolFactoryConfig};

    let temp_dir = tempfile::TempDir::new().unwrap();

    let config = ToolFactoryConfig {
        workspace_dir: temp_dir.path().to_path_buf(),
        mcp: pekobot::tools::McpFactoryConfig {
            enabled: true,
            ..Default::default()
        },
        ..Default::default()
    };

    // Should not fail even without MCP config
    let (tools, discovery) = ToolFactory::create_tools_async(&config).await.unwrap();

    // Should still have core tools
    assert!(!tools.is_empty(), "Should have core tools even without MCP");
    
    // No MCP servers should be found (no config)
    assert_eq!(discovery.servers_found, 0);
}

/// Test MCP config path helper
#[test]
fn test_mcp_config_path() {
    let path = pekobot::tools::ToolFactory::mcp_config_path();
    assert!(path.ends_with("mcp.toml"));
}

/// Test should_use_mcp helper
#[tokio::test]
async fn test_should_use_mcp() {
    // This may be true or false depending on environment
    // Just verify it doesn't panic
    let _result = pekobot::tools::ToolFactory::should_use_mcp().await;
}

/// Test McpDiscoveryResult helper methods
#[test]
fn test_mcp_discovery_result_helpers() {
    use pekobot::tools::factory::McpDiscoveryResult;

    let result = McpDiscoveryResult {
        servers_found: 0,
        tools_available: 0,
        failed_servers: vec![],
        auto_install_attempted: false,
    };
    assert!(!result.has_mcp_tools());

    let result_with_tools = McpDiscoveryResult {
        servers_found: 1,
        tools_available: 5,
        failed_servers: vec![],
        auto_install_attempted: false,
    };
    assert!(result_with_tools.has_mcp_tools());
}

/// Test that create_minimal_tools works
#[test]
fn test_create_minimal_tools() {
    use pekobot::tools::ToolFactory;

    let tools = ToolFactory::create_minimal_tools(PathBuf::from("."));
    
    // Minimal tools should only have filesystem and process
    let tool_names: Vec<_> = tools.iter().map(|t| t.name().to_string()).collect();
    
    // Should have filesystem
    assert!(tool_names.iter().any(|n| n.contains("filesystem") || n == "fs" || n == "read_file" || n == "FileSystem"),
        "Should have filesystem tool, got: {:?}", tool_names);
    
    // Should have process  
    assert!(tool_names.iter().any(|n| n.contains("process") || n.contains("shell") || n.contains("bash")),
        "Should have process tool, got: {:?}", tool_names);
}

/// Test that create_coding_tools works
#[test]
fn test_create_coding_tools() {
    use pekobot::tools::ToolFactory;

    let tools = ToolFactory::create_coding_tools(PathBuf::from("."));
    
    // Should have more tools than minimal
    assert!(!tools.is_empty(), "Should have coding tools");
}
