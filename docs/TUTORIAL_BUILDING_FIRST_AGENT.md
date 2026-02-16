# Tutorial: Building Your First Agent

In this tutorial, you'll build your first Pekobot agent from scratch. By the end, you'll have a working agent that can process tasks and store memories.

## Table of Contents

1. [Prerequisites](#prerequisites)
2. [Step 1: Create a New Project](#step-1-create-a-new-project)
3. [Step 2: Add Dependencies](#step-2-add-dependencies)
4. [Step 3: Write Your Agent](#step-3-write-your-agent)
5. [Step 4: Add Memory](#step-4-add-memory)
6. [Step 5: Handle Multiple Tasks](#step-5-handle-multiple-tasks)
7. [Step 6: Connect to OpenAI](#step-6-connect-to-openai)
8. [Step 7: Build and Run](#step-7-build-and-run)
9. [What's Next?](#whats-next)

---

## Prerequisites

Before starting, ensure you have:

- Rust 1.70+ installed (`rustc --version`)
- An OpenAI API key (get one at [platform.openai.com](https://platform.openai.com))
- Pekobot built from source (see [README](../README.md))

---

## Step 1: Create a New Project

Create a new Rust project for your agent:

```bash
cargo new my-first-agent
cd my-first-agent
```

---

## Step 2: Add Dependencies

Edit `Cargo.toml` to add Pekobot as a dependency:

```toml
[package]
name = "my-first-agent"
version = "0.1.0"
edition = "2021"

[dependencies]
pekobot = { path = "../pekora/projects/pekobot" }
tokio = { version = "1.35", features = ["full"] }
anyhow = "1.0"
tracing-subscriber = "0.3"
serde_json = "1.0"
```

---

## Step 3: Write Your Agent

Replace the contents of `src/main.rs`:

```rust
use pekobot::{Agent, Config};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    println!("🚀 Starting My First Agent!\n");

    // Create agent configuration
    let config = Config::agent("my-agent")
        .with_description("My first Pekobot agent")
        .build();

    // Create and start the agent
    let agent = Agent::new(config).await?;
    agent.start().await?;

    println!("✅ Agent started!");
    println!("   Name: {}", agent.name());
    println!("   DID: {}", agent.did());
    println!("   State: {:?}\n", agent.state());

    // Execute a simple task
    let response = agent.execute("Hello, Pekobot!").await?;
    println!("📝 Response: {}\n", response);

    // Stop the agent
    agent.stop().await?;
    println!("👋 Agent stopped. Goodbye!");

    Ok(())
}
```

### Build and Test

```bash
cargo run
```

You should see:

```
🚀 Starting My First Agent!

✅ Agent started!
   Name: my-agent
   DID: did:pekobot:local:default:...
   State: Idle

📝 Response: Echo: Hello, Pekobot!

👋 Agent stopped. Goodbye!
```

> **Note:** Without an OpenAI API key, the agent echoes back your input. We'll add LLM integration in Step 6.

---

## Step 4: Add Memory

Let's make your agent remember things. Update `src/main.rs`:

```rust
use pekobot::{Agent, Config};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    println!("🚀 Starting Agent with Memory!\n");

    // Create agent with memory enabled
    let config = Config::agent("memory-agent")
        .with_description("An agent that remembers things")
        .with_memory(true)  // Enable SQLite memory
        .build();

    let agent = Agent::new(config).await?;
    agent.start().await?;

    println!("✅ Agent started with memory!");
    println!("   DID: {}\n", agent.did());

    // Store some memories
    println!("💾 Storing memories...");
    
    agent.store_memory(
        "My favorite color is blue",
        Some(serde_json::json!({"topic": "preferences"})),
    )?;
    
    agent.store_memory(
        "I was created in 2026",
        Some(serde_json::json!({"topic": "facts"})),
    )?;
    
    agent.store_memory(
        "Pekobot is a multi-agent runtime",
        Some(serde_json::json!({"topic": "facts"})),
    )?;

    println!("   Stored 3 memories\n");

    // Search memories
    println!("🔍 Searching for 'favorite':");
    let results = agent.search_memory("favorite", 5)?;
    for (i, entry) in results.iter().enumerate() {
        println!("   [{}] {}", i + 1, entry.content);
    }

    println!("\n🔍 Searching for 'Pekobot':");
    let results = agent.search_memory("Pekobot", 5)?;
    for (i, entry) in results.iter().enumerate() {
        println!("   [{}] {}", i + 1, entry.content);
    }

    agent.stop().await?;
    println!("\n👋 Agent stopped!");

    Ok(())
}
```

### Test Memory

```bash
cargo run
```

Output:

```
🚀 Starting Agent with Memory!

✅ Agent started with memory!
   DID: did:pekobot:local:default:...

💾 Storing memories...
   Stored 3 memories

🔍 Searching for 'favorite':
   [1] My favorite color is blue

🔍 Searching for 'Pekobot':
   [1] Pekobot is a multi-agent runtime

👋 Agent stopped!
```

---

## Step 5: Handle Multiple Tasks

Let's make your agent handle multiple tasks in a loop:

```rust
use pekobot::{Agent, Config};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    println!("🚀 Starting Task Processor Agent!\n");

    let config = Config::agent("task-processor")
        .with_description("Processes multiple tasks")
        .with_memory(true)
        .build();

    let agent = Agent::new(config).await?;
    agent.start().await?;

    println!("✅ Agent ready for tasks!\n");

    // Define tasks
    let tasks = vec![
        "Summarize the latest news",
        "Write a poem about Rust",
        "Explain what an agent is",
    ];

    for (i, task) in tasks.iter().enumerate() {
        println!("📋 Task {}: {}", i + 1, task);
        
        let response = agent.execute(task).await?;
        println!("✅ Result: {}\n", response);

        // Small delay between tasks
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    // Show memory usage
    println!("🧠 Memory summary:");
    let all_memories = agent.search_memory("", 100)?;
    println!("   Total entries: {}", all_memories.len());

    agent.stop().await?;
    println!("\n👋 All tasks completed!");

    Ok(())
}
```

---

## Step 6: Connect to OpenAI

Now let's add real AI capabilities. Set your API key and update the config:

```bash
export OPENAI_API_KEY="sk-..."
```

Update `Cargo.toml` to ensure the OpenAI feature is enabled (it is by default):

```rust
use pekobot::{Agent, Config};
use pekobot::types::agent::{AgentConfig, ProviderConfig, ProviderType, MemoryConfig};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    println!("🚀 Starting AI-Powered Agent!\n");

    // Create configuration manually for more control
    let config = AgentConfig {
        name: "ai-agent".to_string(),
        description: Some("An AI-powered agent".to_string()),
        tenant: None,
        capabilities: vec![
            pekobot::types::agent::AgentCapability {
                name: "conversation".to_string(),
                version: "1.0".to_string(),
                description: Some("Can have conversations".to_string()),
                parameters: None,
                required_auth: None,
                estimated_cost: None,
                estimated_duration: None,
            }
        ],
        provider: ProviderConfig {
            provider_type: ProviderType::OpenAI,
            api_key: std::env::var("OPENAI_API_KEY").ok(),
            base_url: Some("https://api.openai.com/v1".to_string()),
            timeout_seconds: Some(30),
            model: Some("gpt-4o-mini".to_string()),
            temperature: Some(0.7),
            max_tokens: Some(1000),
        },
        memory: Some(MemoryConfig {
            enabled: true,
            max_entries: 10000,
            database_path: Some(PathBuf::from("agent_memory.db")),
        }),
        tools: None,
        channels: None,
        auto_accept_trusted: false,
        approval_threshold: Some(100.0),
        default_timeout_seconds: 300,
        coneko: None,
    };

    let agent = Agent::new(config).await?;
    agent.start().await?;

    println!("✅ AI Agent started!");
    println!("   Model: gpt-4o-mini\n");

    // Have a conversation
    let messages = vec![
        "What is the capital of France?",
        "What is 2 + 2?",
        "Tell me a fun fact about space.",
    ];

    for message in messages {
        println!("💬 You: {}", message);
        let response = agent.execute(message).await?;
        println!("🤖 Agent: {}\n", response);
    }

    agent.stop().await?;
    println!("👋 Conversation ended!");

    Ok(())
}
```

---

## Step 7: Build and Run

Build your agent:

```bash
cargo build --release
```

Run it:

```bash
./target/release/my-first-agent
```

---

## What's Next?

Congratulations! You've built your first Pekobot agent. Here are some things to try next:

### 1. Add More Capabilities

Extend your agent with additional capabilities:

```rust
.with_capabilities(vec![
    "messaging".to_string(),
    "task_execution".to_string(),
    "research".to_string(),
])
```

### 2. Use the HTTP Tool

Make your agent fetch web data:

```rust
use pekobot::tools::http::HttpTool;

let http = HttpTool::new();
let response = http.get("https://api.example.com/data").await?;
```

### 3. Connect Multiple Agents

Set up an orchestrator with multiple specialized agents:

```rust
use pekobot::agent::Orchestrator;
use pekobot::a2a::registry::create_registry;

let (registry, _) = create_registry();
let mut orchestrator = Orchestrator::with_registry(registry);

// Add multiple agents...
orchestrator.start_all().await?;
```

### 4. Connect to Coneko

Enable network capabilities:

```rust
use pekobot::coneko::ConekoConfig;

let coneko_config = ConekoConfig {
    enabled: true,
    endpoint: "http://localhost:8080".to_string(),
    auth_token: std::env::var("CONEKO_TOKEN").ok(),
    auto_register: true,
    poll_interval_seconds: 30,
};
```

### 5. Read More

- [User Guide](USERS_GUIDE.md) — Comprehensive guide to Pekobot
- [API Documentation](API.md) — Reference for all APIs
- [CLI Reference](CLI_REFERENCE.md) — Command-line documentation
- [Architecture Guide](ARCHITECTURE.md) — How Pekobot works

---

## Full Example Code

Here's the complete code for a feature-rich agent:

```rust
use pekobot::{Agent, Config};
use pekobot::tools::http::HttpTool;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    println!("🚀 My Advanced Agent\n");

    // Create agent with full configuration
    let config = Config::agent("advanced-agent")
        .with_description("An agent with memory and HTTP capabilities")
        .with_capabilities(vec![
            "messaging".to_string(),
            "task_execution".to_string(),
            "http_requests".to_string(),
        ])
        .with_memory(true)
        .build();

    let agent = Agent::new(config).await?;
    agent.start().await?;

    println!("✅ Agent ready!");
    println!("   DID: {}\n", agent.did());

    // Use HTTP tool
    let http = HttpTool::new();
    
    println!("🌐 Fetching data...");
    match http.get("https://jsonplaceholder.typicode.com/posts/1").await {
        Ok(response) => {
            println!("   Status: {}", response.status);
            agent.store_memory(
                &format!("Fetched: {}", &response.body[..50]),
                None,
            )?;
        }
        Err(e) => println!("   Error: {}", e),
    }

    // Search memory
    println!("\n🔍 Memory search:");
    let memories = agent.search_memory("fetch", 5)?;
    println!("   Found {} memories", memories.len());

    agent.stop().await?;
    println!("\n👋 Done!");

    Ok(())
}
```

---

Happy building! 🐱
