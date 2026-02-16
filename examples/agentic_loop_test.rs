//! Example: Testing the agentic loop with Kimi provider
//! 
//! Run with: cargo run --example agentic_loop_test
//! 
//! Requires KIMI_API_KEY or MOONSHOT_API_KEY environment variable.
//! The key can be found in ~/.openclaw/agents/main/agent/auth-profiles.json

use pekobot::agent::{AgenticLoop, Agent};
use pekobot::providers::KimiProvider;
use pekobot::tools::Tool;
use async_trait::async_trait;
use serde_json::json;

/// Simple echo tool for testing
struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "Echoes back the input message. Usage: TOOL_CALL: {\"name\": \"echo\", \"parameters\": {\"message\": \"your message\"}}"
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let message = params.get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("No message provided");
        
        Ok(json!({
            "success": true,
            "output": format!("Echo: {}", message)
        }))
    }
}

/// Calculator tool for testing
struct CalculatorTool;

#[async_trait]
impl Tool for CalculatorTool {
    fn name(&self) -> &str {
        "calculator"
    }

    fn description(&self) -> &str {
        "Performs basic arithmetic operations. Usage: TOOL_CALL: {\"name\": \"calculator\", \"parameters\": {\"operation\": \"add|subtract|multiply|divide\", \"a\": number, \"b\": number}}"
    }

    async fn execute(&self, params: serde_json::Value) -> anyhow::Result<serde_json::Value> {
        let operation = params.get("operation").and_then(|o| o.as_str()).unwrap_or("");
        let a = params.get("a").and_then(|n| n.as_f64()).unwrap_or(0.0);
        let b = params.get("b").and_then(|n| n.as_f64()).unwrap_or(0.0);

        let (success, output) = match operation {
            "add" => (true, format!("{}", a + b)),
            "subtract" => (true, format!("{}", a - b)),
            "multiply" => (true, format!("{}", a * b)),
            "divide" => {
                if b == 0.0 {
                    (false, "Division by zero".to_string())
                } else {
                    (true, format!("{}", a / b))
                }
            }
            _ => (false, format!("Unknown operation: {}", operation)),
        };

        Ok(json!({
            "success": success,
            "output": output
        }))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    // Load API key from OpenClaw config
    let api_key = load_kimi_api_key()?;
    
    println!("🐰 Pekobot Agentic Loop Test");
    println!("============================");
    
    // Create provider
    let provider = KimiProvider::new(api_key)
        .with_model("kimi-k2.5");
    
    println!("✓ Kimi provider initialized");
    
    // Create agent
    let agent = Agent::new("test-agent").await?;
    println!("✓ Agent created: {}", agent.name());
    
    // Create tools
    let tools: Vec<Box<dyn Tool>> = vec![
        Box::new(EchoTool),
        Box::new(CalculatorTool),
    ];
    println!("✓ Tools registered: {}", tools.len());
    
    // Create agentic loop
    let mut loop_ = AgenticLoop::new(agent, Box::new(provider), tools)
        .with_max_iterations(5);
    
    // Test prompts
    let prompts = vec![
        "Echo back 'Hello from Pekobot!'",
        "Calculate 23 + 47 using the calculator tool",
        "What is 100 divided by 4?",
    ];
    
    for (i, prompt) in prompts.iter().enumerate() {
        println!("\n--- Test {} ---", i + 1);
        println!("Prompt: {}", prompt);
        
        match loop_.run(prompt).await {
            Ok(result) => {
                println!("Success: {}", result.success);
                println!("Iterations: {}", result.iterations);
                println!("Answer: {}", result.final_answer);
            }
            Err(e) => {
                println!("Error: {}", e);
            }
        }
    }
    
    println!("\n✅ All tests completed!");
    Ok(())
}

/// Load Kimi API key from OpenClaw config
fn load_kimi_api_key() -> anyhow::Result<String> {
    // First try environment variable
    if let Ok(key) = std::env::var("KIMI_API_KEY") {
        return Ok(key);
    }
    if let Ok(key) = std::env::var("MOONSHOT_API_KEY") {
        return Ok(key);
    }
    
    // Try to load from OpenClaw auth profiles
    let home = std::env::var("HOME")
        .map_err(|_| anyhow::anyhow!("HOME environment variable not set"))?;
    
    let auth_profiles_path = std::path::PathBuf::from(home)
        .join(".openclaw")
        .join("agents")
        .join("main")
        .join("agent")
        .join("auth-profiles.json");
    
    if auth_profiles_path.exists() {
        let content = std::fs::read_to_string(&auth_profiles_path)
            .map_err(|e| anyhow::anyhow!("Failed to read auth profiles: {}", e))?;
        
        let profiles: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse auth profiles: {}", e))?;
        
        if let Some(key) = profiles
            .get("profiles")
            .and_then(|p| p.get("kimi-coding:default"))
            .and_then(|p| p.get("key"))
            .and_then(|k| k.as_str())
        {
            return Ok(key.to_string());
        }
    }
    
    Err(anyhow::anyhow!(
        "Kimi API key not found. Set KIMI_API_KEY or MOONSHOT_API_KEY environment variable, \
         or ensure ~/.openclaw/agents/main/agent/auth-profiles.json exists with a valid key."
    ))
}
