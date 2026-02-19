//! HTTP Tool Usage Example
//!
//! Demonstrates how to use the HTTP tool for making web requests
//! and processing responses.

use pekobot::{tools::http::HttpTool, Agent, Config};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    println!("🌐 HTTP Tool Example\n");
    println!("====================\n");

    // Create an agent
    let config = Config::agent("http-agent")
        .with_description("Agent that makes HTTP requests")
        .with_memory(true)
        .build();

    let agent = Agent::new(config).await?;
    agent.start().await?;

    println!("✅ Agent started: {}\n", agent.name());

    // Create HTTP tool
    let http_tool = HttpTool::new();

    // Example 1: Simple GET request
    println!("📡 Example 1: Fetching JSONPlaceholder API...");
    match http_tool
        .get("https://jsonplaceholder.typicode.com/posts/1")
        .await
    {
        Ok(response) => {
            println!("   Status: {}", response.status);
            println!(
                "   Body (truncated): {}...\n",
                &response.body[..100.min(response.body.len())]
            );

            // Store in agent memory
            let _ = agent.store_memory(
                &format!("Fetched post: {}", &response.body[..50]),
                Some(serde_json::json!({
                    "url": "https://jsonplaceholder.typicode.com/posts/1",
                    "status": response.status,
                })),
            );
        }
        Err(e) => println!("   ❌ Error: {}\n", e),
    }

    // Example 2: GET with headers
    println!("📡 Example 2: Request with custom headers...");
    let headers = vec![
        ("Accept".to_string(), "application/json".to_string()),
        ("User-Agent".to_string(), "Pekobot/0.1.0".to_string()),
    ];

    match http_tool
        .get_with_headers("https://httpbin.org/get", headers)
        .await
    {
        Ok(response) => {
            println!("   Status: {}", response.status);
            println!("   Response stored in memory\n");
        }
        Err(e) => println!("   ❌ Error: {}\n", e),
    }

    // Example 3: POST request
    println!("📡 Example 3: POST request...");
    let body = serde_json::json!({
        "title": "Pekobot Test",
        "body": "This is a test post from Pekobot",
        "userId": 1,
    });

    match http_tool
        .post_json("https://jsonplaceholder.typicode.com/posts", &body)
        .await
    {
        Ok(response) => {
            println!("   Status: {}", response.status);
            println!(
                "   Created resource: {}\n",
                &response.body[..80.min(response.body.len())]
            );
        }
        Err(e) => println!("   ❌ Error: {}\n", e),
    }

    // Search memory for HTTP interactions
    println!("🔍 Searching memory for HTTP interactions...");
    let memories = agent.search_memory("http", 10)?;
    println!("   Found {} HTTP-related memories\n", memories.len());

    // Stop agent
    agent.stop().await?;
    println!("👋 Agent stopped. Example complete!");

    Ok(())
}
