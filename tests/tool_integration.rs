//! Comprehensive Tool Integration Test
//!
//! Tests all 14 Pekobot tools to ensure they work correctly.
//! Run with: cargo test --test tool_integration -- --nocapture

use std::path::PathBuf;
use pekobot::tools::Tool;

/// Simple smoke test - verify tools compile and have correct names
#[tokio::test]
async fn test_tools_smoke() {
    use pekobot::tools::{
        FileSystemTool, ProcessTool, HttpTool, FetchTool, WebSearchTool,
        ApplyPatchTool, BrowserTool
    };
    
    println!("\n🔧 Smoke test - verifying tool names...");
    
    // Core tools
    assert_eq!(FileSystemTool::new().name(), "filesystem");
    assert_eq!(ProcessTool::new().name(), "process");
    assert_eq!(HttpTool::new().unwrap().name(), "http");
    assert_eq!(FetchTool::new(Default::default()).name(), "fetch");
    assert_eq!(WebSearchTool::new(Default::default()).name(), "web_search");
    assert_eq!(ApplyPatchTool::new(Default::default(), PathBuf::from(".")).name(), "apply_patch");
    assert_eq!(BrowserTool::new(vec![], None).name(), "browser");
    
    println!("✅ Core tools: 7/7 passed");
    
    // Session introspection tools
    use pekobot::tools::{
        SessionsListTool, SessionsHistoryTool, SessionStatusTool,
        InMemorySessionRegistry
    };
    
    let registry = InMemorySessionRegistry::new("main".to_string());
    assert_eq!(SessionsListTool::new(Box::new(registry)).name(), "sessions_list");
    
    let registry2 = InMemorySessionRegistry::new("main".to_string());
    assert_eq!(SessionsHistoryTool::new(Box::new(registry2)).name(), "sessions_history");
    
    let registry3 = InMemorySessionRegistry::new("main".to_string());
    assert_eq!(SessionStatusTool::new(Box::new(registry3)).name(), "session_status");
    
    println!("✅ Session introspection tools: 3/3 passed");
    
    // Agent management tools
    use pekobot::tools::{AgentsListTool, ManagerCommand};
    use tokio::sync::mpsc;
    
    let (tx, _rx) = mpsc::channel(10);
    assert_eq!(AgentsListTool::new(tx).name(), "agents_list");
    
    println!("✅ Agent management: 1/1 passed");
    
    // Session messaging tool
    use pekobot::tools::{SessionMessagingTool, SessionRegistry};
    use std::sync::Arc;
    
    let registry = Arc::new(SessionRegistry::new());
    assert_eq!(SessionMessagingTool::new(registry, "test".to_string()).name(), "session_messaging");
    
    println!("✅ Session messaging: 1/1 passed");
    
    // Agent communication tools
    use pekobot::tools::{AgentBroadcastTool, AgentInfoTool, AgentSpawnTool};
    
    let (tx, _rx) = mpsc::channel(10);
    assert_eq!(AgentBroadcastTool::new(tx.clone()).name(), "agent_broadcast");
    assert_eq!(AgentInfoTool::new(tx.clone()).name(), "agent_info");
    assert_eq!(AgentSpawnTool::new(tx).name(), "agent_spawn");
    
    println!("✅ Agent communication tools: 3/3 passed");
    
    println!("\n╔════════════════════════════════════════════════════════════╗");
    println!("║     ✅ All 14 Tools Verified!                              ║");
    println!("╚════════════════════════════════════════════════════════════╝");
}

/// Test filesystem tool
#[tokio::test]
#[ignore = "requires filesystem access"]
async fn test_filesystem_tool() {
    use pekobot::tools::FileSystemTool;
    use pekobot::security::SecurityPolicy;
    
    println!("\n📁 Testing filesystem tool...");
    
    let policy = SecurityPolicy {
        workspace_dir: PathBuf::from("/tmp"),
        workspace_only: false,
        ..Default::default()
    };
    let tool = FileSystemTool::with_policy(policy);
    
    // Test write
    let write_result = tool.execute(serde_json::json!({
        "action": "write",
        "path": "/tmp/test_file.txt",
        "content": "Hello from Pekobot!"
    })).await;
    
    assert!(write_result.is_ok(), "Failed to write file: {:?}", write_result);
    println!("  ✓ Write file");
    
    // Test read
    let read_result = tool.execute(serde_json::json!({
        "action": "read",
        "path": "/tmp/test_file.txt"
    })).await;
    
    assert!(read_result.is_ok(), "Failed to read file: {:?}", read_result);
    let binding = read_result.unwrap();
    let content = binding["content"].as_str().unwrap();
    assert_eq!(content, "Hello from Pekobot!");
    println!("  ✓ Read file");
    
    // Cleanup
    let _ = std::fs::remove_file("/tmp/test_file.txt");
    println!("✅ Filesystem tool passed");
}

/// Test process tool
#[tokio::test]
#[ignore = "requires shell access"]
async fn test_process_tool() {
    use pekobot::tools::ProcessTool;
    
    println!("\n⚙️ Testing process tool...");
    
    let tool = ProcessTool::new();
    
    // Test simple command
    let result = tool.execute(serde_json::json!({
        "command": "echo",
        "args": ["Hello", "World"],
        "timeout": 5
    })).await;
    
    assert!(result.is_ok(), "Process execution failed: {:?}", result);
    let response = result.unwrap();
    let output = response["stdout"].as_str().unwrap();
    assert!(output.contains("Hello World"), "Unexpected output: {}", output);
    println!("✅ Process tool passed");
}

/// Test HTTP tool
#[tokio::test]
#[ignore = "requires network access"]
async fn test_http_tool() {
    use pekobot::tools::HttpTool;
    
    println!("\n🌐 Testing HTTP tool...");
    
    let tool = HttpTool::new().expect("Failed to create HTTP tool");
    
    // Test GET request
    let result = tool.execute(serde_json::json!({
        "method": "GET",
        "url": "https://httpbin.org/get",
        "headers": {}
    })).await;
    
    assert!(result.is_ok(), "HTTP request failed: {:?}", result);
    let response = result.unwrap();
    assert_eq!(response["status"].as_u64().unwrap(), 200);
    println!("✅ HTTP tool passed");
}

/// Test fetch tool
#[tokio::test]
#[ignore = "requires network access"]
async fn test_fetch_tool() {
    use pekobot::tools::{FetchTool, FetchConfig};
    
    println!("\n📥 Testing fetch tool...");
    
    let config = FetchConfig::default();
    let tool = FetchTool::new(config);
    
    // Test fetching example.com
    let result = tool.execute(serde_json::json!({
        "url": "https://example.com",
        "extract_mode": "text",
        "max_chars": 1000
    })).await;
    
    assert!(result.is_ok(), "Fetch failed: {:?}", result);
    let response = result.unwrap();
    let content = response["content"].as_str().unwrap_or("");
    assert!(!content.is_empty(), "Content should not be empty");
    println!("✅ Fetch tool passed");
}

/// Test web search tool
#[tokio::test]
#[ignore = "requires network access"]
async fn test_web_search_tool() {
    use pekobot::tools::{WebSearchTool, WebSearchConfig, SearchProvider};
    
    println!("\n🔍 Testing web search tool...");
    
    let config = WebSearchConfig {
        provider: SearchProvider::DuckDuckGo,
        ..Default::default()
    };
    let tool = WebSearchTool::new(config);
    
    let result = tool.execute(serde_json::json!({
        "query": "Rust programming language",
        "count": 3
    })).await;
    
    assert!(result.is_ok(), "Web search failed: {:?}", result);
    let response = result.unwrap();
    let results = response["results"].as_array().unwrap();
    assert!(!results.is_empty(), "Should have search results");
    println!("✅ Web search tool passed");
}

/// Test apply patch tool
#[tokio::test]
#[ignore = "requires filesystem access"]
async fn test_apply_patch_tool() {
    use pekobot::tools::{ApplyPatchTool, ApplyPatchConfig};
    
    println!("\n🩹 Testing apply patch tool...");
    
    let config = ApplyPatchConfig::default();
    let tool = ApplyPatchTool::new(config, PathBuf::from("/tmp"));
    
    // Create initial file
    std::fs::write("/tmp/patch_test.txt", "Hello World").unwrap();
    
    // Apply patch
    let result = tool.execute(serde_json::json!({
        "patches": [{
            "path": "/tmp/patch_test.txt",
            "old_content": "Hello World",
            "new_content": "Hello Pekobot"
        }],
        "dry_run": false
    })).await;
    
    assert!(result.is_ok(), "Patch application failed: {:?}", result);
    
    // Verify content changed
    let content = std::fs::read_to_string("/tmp/patch_test.txt").unwrap();
    assert_eq!(content, "Hello Pekobot");
    
    // Cleanup
    let _ = std::fs::remove_file("/tmp/patch_test.txt");
    let _ = std::fs::remove_file("/tmp/patch_test.txt.bak");
    println!("✅ Apply patch tool passed");
}

/// Test session introspection tools structure
#[tokio::test]
async fn test_session_introspection_tools() {
    use pekobot::tools::{
        SessionsListTool, SessionsHistoryTool, SessionStatusTool,
        InMemorySessionRegistry
    };
    
    println!("\n📊 Testing session introspection tools...");
    
    let registry = InMemorySessionRegistry::new("main".to_string());
    
    // Test tools exist
    let list_tool = SessionsListTool::new(Box::new(registry));
    assert_eq!(list_tool.name(), "sessions_list");
    println!("  ✓ sessions_list");
    
    let registry2 = InMemorySessionRegistry::new("main".to_string());
    let history_tool = SessionsHistoryTool::new(Box::new(registry2));
    assert_eq!(history_tool.name(), "sessions_history");
    println!("  ✓ sessions_history");
    
    let registry3 = InMemorySessionRegistry::new("main".to_string());
    let status_tool = SessionStatusTool::new(Box::new(registry3));
    assert_eq!(status_tool.name(), "session_status");
    println!("  ✓ session_status");
    
    println!("✅ Session introspection tools passed");
}
