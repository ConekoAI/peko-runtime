//! Echo Agent Example
//!
//! A simple agent that echoes back messages with a twist.
//! Demonstrates basic agent setup, memory usage, and task execution.

use pekobot::{Agent, Config};
use std::sync::Arc;
use tokio::sync::Mutex;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    println!("🐰 Starting Echo Agent Example...\n");

    // Create agent configuration
    let config = Config::agent("echo-agent")
        .with_description("A simple echo agent that repeats messages")
        .with_memory(true)
        .build();

    // Create and start the agent
    let agent = Agent::new(config).await?;
    agent.start().await?;

    println!("✅ Agent started with DID: {}", agent.did());
    println!("   Name: {}", agent.name());
    println!("   State: {:?}\n", agent.state());

    // Test some interactions
    let test_messages = vec![
        "Hello, Pekobot!",
        "How are you today?",
        "What's the weather like?",
    ];

    for message in test_messages {
        println!("📤 User: {}", message);
        let response = agent.execute(message).await?;
        println!("📥 Agent: {}\n", response);
    }

    // Demonstrate memory search
    println!("🔍 Searching memory for 'hello'...");
    let memories = agent.search_memory("hello", 5)?;
    println!("   Found {} memories\n", memories.len());

    for (i, mem) in memories.iter().enumerate() {
        println!("   [{}] {}", i + 1, mem.content);
    }

    // Stop the agent
    agent.stop().await?;
    println!("\n👋 Agent stopped. Goodbye!");

    Ok(())
}
