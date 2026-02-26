//! End-to-End Test: Agentic Loop with Tool Calling
//!
//! This example tests the core engine functionality:
//! - Agent creation with tools
//! - Agentic loop execution
//! - Real tool calling (filesystem)
//! - Uses Kimi API for real LLM interactions
//!
//! Run with: cargo run --example e2e_tool_test

use pekobot::agent::{Agent, AgenticLoop};
use pekobot::providers::KimiCodeProvider;
use pekobot::security::SecurityPolicy;
use pekobot::tools::FileSystemTool;
use pekobot::types::agent::AgentConfig;
use pekobot::types::provider::{ProviderConfig, ProviderType};
use std::error::Error;
use std::path::PathBuf;
use std::sync::Arc;

/// Load Kimi API key from environment or OpenClaw config
fn load_kimi_api_key() -> anyhow::Result<String> {
    // Try environment variable first
    if let Ok(key) = std::env::var("KIMI_API_KEY") {
        return Ok(key);
    }
    if let Ok(key) = std::env::var("MOONSHOT_API_KEY") {
        return Ok(key);
    }

    // Try to load from OpenClaw config
    let config_path = dirs::home_dir()
        .unwrap_or_default()
        .join(".openclaw/agents/main/agent/auth-profiles.json");

    if let Ok(content) = std::fs::read_to_string(&config_path) {
        if let Ok(profiles) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(profiles_obj) = profiles.as_object() {
                for (_, profile) in profiles_obj {
                    if let Some(api_key) = profile.get("apiKey").and_then(|v| v.as_str()) {
                        return Ok(api_key.to_string());
                    }
                }
            }
        }
    }

    Err(anyhow::anyhow!(
        "Kimi API key not found. Set KIMI_API_KEY env var or configure in OpenClaw."
    ))
}

fn create_fs_tool() -> FileSystemTool {
    // Create a security policy that allows access to /home/ubuntu/pekora
    let policy = SecurityPolicy {
        workspace_dir: PathBuf::from("/home/ubuntu/pekora"),
        workspace_only: false, // Allow access outside workspace
        ..Default::default()
    };

    FileSystemTool::with_policy(policy)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    println!("🐰 Pekobot E2E Tool Calling Test");
    println!("=================================\n");

    // Load API key
    let api_key = load_kimi_api_key()?;
    println!("✓ API key loaded\n");

    // Create provider (Kimi Code uses Anthropic API format)
    let provider = Arc::new(KimiCodeProvider::with_api_key(api_key)?);
    println!("✓ Kimi Code provider initialized\n");

    // Create filesystem tool
    let fs_tool = Arc::new(create_fs_tool());
    println!("✓ Filesystem tool created (access to /home/ubuntu/pekora)\n");

    // Test 1: Simple directory listing
    println!("📁 Test 1: List directory contents");
    println!("-----------------------------------");

    let agent_config = AgentConfig {
        name: "test-agent".to_string(),
        description: Some("Agent for testing tool calling".to_string()),
        provider: ProviderConfig {
            provider_type: ProviderType::Ollama, // Skip provider init - we pass our own
            ..Default::default()
        },
        ..Default::default()
    };
    let agent = Arc::new(Agent::new(agent_config).await?);

    let loop_ = AgenticLoop::new(agent, provider.clone(), vec![fs_tool.clone()]);
    println!("✓ Agentic loop initialized\n");

    let prompt1 =
        "List the files and directories in /home/ubuntu/pekora/projects. Use the filesystem tool.";
    println!("Prompt: {}\n", prompt1);

    match loop_.run(prompt1).await {
        Ok(result) => {
            println!("\n✅ Test 1 PASSED");
            println!("Success: {}", result.success);
            println!("Iterations: {}", result.iterations);
            println!("Tool calls made: {:?}", result.tool_calls);
            println!("Final answer:\n{}\n", result.final_answer);
        }
        Err(e) => {
            println!("\n❌ Test 1 FAILED: {}", e);
            println!("Error chain:");
            let mut current = e.source();
            while let Some(source) = current {
                println!("  → {}", source);
                current = source.source();
            }
            println!();
        }
    }

    // Test 2: Read a file
    println!("📄 Test 2: Read a file");
    println!("----------------------");

    let agent_config2 = AgentConfig {
        name: "test-agent-2".to_string(),
        description: Some("Agent for testing file reading".to_string()),
        provider: ProviderConfig {
            provider_type: ProviderType::Ollama,
            ..Default::default()
        },
        ..Default::default()
    };
    let agent2 = Arc::new(Agent::new(agent_config2).await?);
    let loop_2 = AgenticLoop::new(agent2, provider.clone(), vec![fs_tool.clone()]);

    let prompt2 = "Read the contents of /home/ubuntu/pekora/README.md using the filesystem tool.";
    println!("Prompt: {}\n", prompt2);

    match loop_2.run(prompt2).await {
        Ok(result) => {
            println!("\n✅ Test 2 PASSED");
            println!("Success: {}", result.success);
            println!("Iterations: {}", result.iterations);
            println!("Tool calls: {:?}", result.tool_calls);
            println!(
                "Final answer preview:\n{}...\n",
                if result.final_answer.len() > 200 {
                    &result.final_answer[..200]
                } else {
                    &result.final_answer
                }
            );
        }
        Err(e) => {
            println!("\n❌ Test 2 FAILED: {}\n", e);
        }
    }

    // Test 3: Multi-step task (list + read)
    println!("🔄 Test 3: Multi-step task");
    println!("---------------------------");

    let agent_config3 = AgentConfig {
        name: "test-agent-3".to_string(),
        description: Some("Agent for multi-step tasks".to_string()),
        provider: ProviderConfig {
            provider_type: ProviderType::Ollama,
            ..Default::default()
        },
        ..Default::default()
    };
    let agent3 = Arc::new(Agent::new(agent_config3).await?);
    let loop_3 = AgenticLoop::new(agent3, provider, vec![fs_tool]);

    let prompt3 = "Look at the projects in /home/ubuntu/pekora/projects and tell me what the README says in the pekobot directory.";
    println!("Prompt: {}\n", prompt3);

    match loop_3.run(prompt3).await {
        Ok(result) => {
            println!("\n✅ Test 3 PASSED");
            println!("Success: {}", result.success);
            println!("Iterations: {}", result.iterations);
            println!("Tool calls: {:?}", result.tool_calls);
            println!("Final answer:\n{}\n", result.final_answer);
        }
        Err(e) => {
            println!("\n❌ Test 3 FAILED: {}\n", e);
        }
    }

    println!("=================================");
    println!("E2E Test Complete");

    Ok(())
}
