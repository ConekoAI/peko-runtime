//! Kimi Code Stress Test
//! 
//! Comprehensive stress test for Kimi Code provider

use std::env;
use std::time::Instant;

#[tokio::main]
async fn main() {
    println!("🚀 Kimi Code Stress Test");
    println!("========================\n");

    // Load API key from auth profiles
    let auth_file = std::fs::read_to_string(
        format!("{}/.openclaw/agents/main/agent/auth-profiles.json", env::var("HOME").unwrap_or_default())
    );

    let api_key = match auth_file {
        Ok(content) => {
            let json: serde_json::Value = serde_json::from_str(&content).expect("Invalid JSON");
            json["profiles"]["kimi-coding:default"]["key"]
                .as_str()
                .map(|s| s.to_string())
        }
        Err(_) => env::var("KIMI_API_KEY").ok(),
    };

    let api_key = match api_key {
        Some(key) => {
            println!("✅ API key loaded (length: {})\n", key.len());
            key
        }
        None => {
            eprintln!("❌ No API key found!");
            std::process::exit(1);
        }
    };

    // Run stress tests
    let results = vec![
        test_basic_response(&api_key).await,
        test_tool_calling(&api_key).await,
        test_reasoning(&api_key).await,
        test_multi_turn(&api_key).await,
        test_concurrent_requests(&api_key).await,
    ];

    // Summary
    println!("\n📊 Stress Test Summary");
    println!("======================");
    let passed = results.iter().filter(|r| r.0).count();
    let failed = results.len() - passed;
    println!("✅ Passed: {}/{}", passed, results.len());
    println!("❌ Failed: {}/{}", failed, results.len());
    
    for (i, (passed, name, duration)) in results.iter().enumerate() {
        let status = if *passed { "✅" } else { "❌" };
        println!("{} Test {}: {} ({:.2}s)", status, i + 1, name, duration.as_secs_f64());
    }

    if failed > 0 {
        std::process::exit(1);
    }
}

async fn test_basic_response(api_key: &str) -> (bool, &'static str, std::time::Duration) {
    let start = Instant::now();
    println!("📡 Test 1: Basic Response");
    
    let client = reqwest::Client::new();
    let response = client
        .post("https://api.kimi.com/coding/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "model": "k2p5",
            "max_tokens": 100,
            "messages": [{"role": "user", "content": "Say 'Hello from Pekobot!'"}]
        }))
        .send()
        .await;

    match response {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if let Some(text) = json["content"][0]["text"].as_str() {
                    println!("   ✅ Response: {}", text.trim());
                    return (true, "Basic Response", start.elapsed());
                }
            }
        }
        Ok(resp) => println!("   ❌ HTTP {}", resp.status()),
        Err(e) => println!("   ❌ Error: {}", e),
    }
    (false, "Basic Response", start.elapsed())
}

async fn test_tool_calling(api_key: &str) -> (bool, &'static str, std::time::Duration) {
    let start = Instant::now();
    println!("📡 Test 2: Tool Calling");
    
    let client = reqwest::Client::new();
    let response = client
        .post("https://api.kimi.com/coding/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "model": "k2p5",
            "max_tokens": 500,
            "tools": [
                {
                    "name": "calculate",
                    "description": "Calculate a mathematical expression",
                    "input_schema": {
                        "type": "object",
                        "properties": {
                            "expression": {"type": "string"}
                        },
                        "required": ["expression"]
                    }
                }
            ],
            "messages": [{"role": "user", "content": "What is 23 + 47?"}]
        }))
        .send()
        .await;

    match response {
        Ok(resp) if resp.status().is_success() => {
            println!("   ✅ Tool calling works");
            return (true, "Tool Calling", start.elapsed());
        }
        Ok(resp) => println!("   ❌ HTTP {}", resp.status()),
        Err(e) => println!("   ❌ Error: {}", e),
    }
    (false, "Tool Calling", start.elapsed())
}

async fn test_reasoning(api_key: &str) -> (bool, &'static str, std::time::Duration) {
    let start = Instant::now();
    println!("📡 Test 3: Reasoning");
    
    let client = reqwest::Client::new();
    let response = client
        .post("https://api.kimi.com/coding/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "model": "k2p5",
            "max_tokens": 500,
            "messages": [{"role": "user", "content": "If a train travels 60 km in 30 minutes, what is its average speed in km/h? Show your reasoning."}]
        }))
        .send()
        .await;

    match response {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if let Some(text) = json["content"][0]["text"].as_str() {
                    let has_reasoning = text.to_lowercase().contains("120") || text.to_lowercase().contains("speed");
                    println!("   {} Reasoning: {}", 
                        if has_reasoning { "✅" } else { "⚠️" },
                        text.chars().take(100).collect::<String>()
                    );
                    return (has_reasoning, "Reasoning", start.elapsed());
                }
            }
        }
        Ok(resp) => println!("   ❌ HTTP {}", resp.status()),
        Err(e) => println!("   ❌ Error: {}", e),
    }
    (false, "Reasoning", start.elapsed())
}

async fn test_multi_turn(api_key: &str) -> (bool, &'static str, std::time::Duration) {
    let start = Instant::now();
    println!("📡 Test 4: Multi-turn Conversation");
    
    let client = reqwest::Client::new();
    
    // First turn
    let response1 = client
        .post("https://api.kimi.com/coding/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "model": "k2p5",
            "max_tokens": 100,
            "messages": [{"role": "user", "content": "My name is Pekobot. Remember it."}]
        }))
        .send()
        .await;

    if response1.is_err() || !response1.unwrap().status().is_success() {
        return (false, "Multi-turn", start.elapsed());
    }

    // Second turn - check if it remembers
    let response2 = client
        .post("https://api.kimi.com/coding/v1/messages")
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "model": "k2p5",
            "max_tokens": 100,
            "messages": [
                {"role": "user", "content": "My name is Pekobot. Remember it."},
                {"role": "assistant", "content": "I'll remember that your name is Pekobot."},
                {"role": "user", "content": "What is my name?"}
            ]
        }))
        .send()
        .await;

    match response2 {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if let Some(text) = json["content"][0]["text"].as_str() {
                    let remembers = text.to_lowercase().contains("pekobot");
                    println!("   {} Remembers name: {}", 
                        if remembers { "✅" } else { "⚠️" },
                        text.chars().take(80).collect::<String>()
                    );
                    return (remembers, "Multi-turn", start.elapsed());
                }
            }
        }
        Ok(resp) => println!("   ❌ HTTP {}", resp.status()),
        Err(e) => println!("   ❌ Error: {}", e),
    }
    (false, "Multi-turn", start.elapsed())
}

async fn test_concurrent_requests(api_key: &str) -> (bool, &'static str, std::time::Duration) {
    let start = Instant::now();
    println!("📡 Test 5: Concurrent Requests (3 parallel)");
    
    let client = reqwest::Client::new();
    let mut handles = vec![];
    
    for i in 0..3 {
        let client = client.clone();
        let api_key = api_key.to_string();
        let handle = tokio::spawn(async move {
            client
                .post("https://api.kimi.com/coding/v1/messages")
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01")
                .header("Content-Type", "application/json")
                .json(&serde_json::json!({
                    "model": "k2p5",
                    "max_tokens": 50,
                    "messages": [{"role": "user", "content": format!("Test request {}", i + 1)}]
                }))
                .send()
                .await
        });
        handles.push(handle);
    }
    
    let mut success_count = 0;
    for handle in handles {
        if let Ok(Ok(resp)) = handle.await {
            if resp.status().is_success() {
                success_count += 1;
            }
        }
    }
    
    let all_passed = success_count == 3;
    println!("   {} {}/3 requests succeeded", 
        if all_passed { "✅" } else { "⚠️" },
        success_count
    );
    (all_passed, "Concurrent Requests", start.elapsed())
}
