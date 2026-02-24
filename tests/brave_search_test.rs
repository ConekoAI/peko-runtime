//! Brave Search API Test
//!
//! Verifies Brave Search works with the provided API key.

use pekobot::tools::{WebSearchTool, WebSearchConfig, SearchProvider, Tool};

#[tokio::test]
async fn test_brave_search_api() {
    // Get API key from environment
    let api_key = std::env::var("BRAVE_API_KEY")
        .expect("BRAVE_API_KEY environment variable not set");
    
    println!("\n🔍 Testing Brave Search API...");
    
    let config = WebSearchConfig {
        provider: SearchProvider::Brave,
        api_key: Some(api_key),
        max_results: 5,
        ..Default::default()
    };
    
    let tool = WebSearchTool::new(config);
    
    let result = tool.execute(serde_json::json!({
        "query": "Rust programming language",
        "count": 3
    })).await;
    
    assert!(result.is_ok(), "Brave search failed: {:?}", result);
    
    let response = result.unwrap();
    let results = response["results"].as_array().unwrap();
    let provider = response["provider"].as_str().unwrap();
    
    println!("  Provider: {}", provider);
    println!("  Results found: {}", results.len());
    
    assert_eq!(provider, "brave", "Expected provider to be 'brave'");
    assert!(!results.is_empty(), "Should have search results");
    
    if !results.is_empty() {
        println!("  First result: {}", results[0]["title"].as_str().unwrap_or("N/A"));
        println!("  URL: {}", results[0]["url"].as_str().unwrap_or("N/A"));
    }
    
    println!("✅ Brave Search API test passed!");
}

#[tokio::test]
async fn test_duckduckgo_fallback() {
    println!("\n🔍 Testing DuckDuckGo fallback...");
    
    let config = WebSearchConfig {
        provider: SearchProvider::DuckDuckGo,
        ..Default::default()
    };
    
    let tool = WebSearchTool::new(config);
    
    let result = tool.execute(serde_json::json!({
        "query": "Rust programming",
        "count": 3
    })).await;
    
    assert!(result.is_ok(), "DDG search failed: {:?}", result);
    
    let response = result.unwrap();
    let provider = response["provider"].as_str().unwrap();
    
    println!("  Provider: {}", provider);
    assert_eq!(provider, "duckduckgo", "Expected provider to be 'duckduckgo'");
    
    println!("✅ DuckDuckGo fallback test passed!");
}
