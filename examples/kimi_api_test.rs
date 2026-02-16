//! Simple test for Kimi API

use reqwest::Client;
use serde_json::json;

fn load_api_key() -> Option<String> {
    // Try environment variables first
    if let Ok(key) = std::env::var("KIMI_API_KEY") {
        return Some(key);
    }
    if let Ok(key) = std::env::var("MOONSHOT_API_KEY") {
        return Some(key);
    }
    
    // Try OpenClaw auth profiles
    let home = std::env::var("HOME").ok()?;
    let path = std::path::PathBuf::from(home)
        .join(".openclaw")
        .join("agents")
        .join("main")
        .join("agent")
        .join("auth-profiles.json");
    let content = std::fs::read_to_string(path).ok()?;
    let profiles: serde_json::Value = serde_json::from_str(&content).ok()?;
    profiles
        .get("profiles")?
        .get("kimi-coding:default")?
        .get("key")?
        .as_str()
        .map(|s| s.to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let api_key = load_api_key()
        .expect("API key not found. Set KIMI_API_KEY or check auth-profiles.json");

    println!("Testing Kimi API...");
    println!("API Key: {}...", &api_key[..20.min(api_key.len())]);

    let client = Client::new();
    let body = json!({
        "model": "kimi-k2.5",
        "messages": [
            {"role": "user", "content": "Say 'Hello from Pekobot!'"}
        ],
        "temperature": 0.7,
        "stream": false
    });

    println!("\nSending request...");
    let response = client
        .post("https://api.moonshot.cn/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await?;

    let status = response.status();
    println!("Status: {}", status);

    let text = response.text().await?;
    
    if status.is_success() {
        let result: serde_json::Value = serde_json::from_str(&text)?;
        println!("Response JSON: {}", serde_json::to_string_pretty(&result)?);
        
        if let Some(content) = result
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
        {
            println!("\n✅ SUCCESS! Kimi API is working.");
            println!("Content: {}", content);
        }
    } else {
        println!("\n❌ FAILED! API returned error status.");
        println!("Response: {}", text);
    }

    Ok(())
}
