//! Brave Search API Test
//!
//! Verifies Brave Search works with the provided API key.
//!
//! Run manually: cargo test --test brave_search_test -- --ignored

use pekobot::tools::{Tool, WebSearchConfig, WebSearchTool};

#[tokio::test]
#[ignore = "Requires BRAVE_API_KEY and network access"]
async fn test_brave_search_api() {
    // Get API key from environment
    let api_key =
        std::env::var("BRAVE_API_KEY").expect("BRAVE_API_KEY environment variable not set");

    println!("\n🔍 Testing Brave Search API...");

    let config = WebSearchConfig {
        api_key: Some(api_key),
        max_urls: 5,
        ..Default::default()
    };

    let tool = WebSearchTool::new(config);

    let result = tool
        .execute(serde_json::json!({
            "query": "Rust programming language",
            "count": 3
        }))
        .await;

    assert!(result.is_ok(), "Brave search failed: {:?}", result);

    let response = result.unwrap();
    let results = response["results"].as_array().unwrap();
    let provider = response["provider"].as_str().unwrap();

    println!("  Provider: {}", provider);
    println!("  Results found: {}", results.len());

    assert_eq!(provider, "brave", "Expected provider to be 'brave'");
    assert!(!results.is_empty(), "Should have search results");

    if !results.is_empty() {
        println!(
            "  First result: {}",
            results[0]["title"].as_str().unwrap_or("N/A")
        );
        println!("  URL: {}", results[0]["url"].as_str().unwrap_or("N/A"));
    }

    println!("✅ Brave Search API test passed!");
}
