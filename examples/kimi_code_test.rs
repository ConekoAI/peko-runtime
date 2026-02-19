//! Kimi Code API Test
//!
//! Tests the Kimi Code provider using the auth profile key

use std::env;

#[tokio::main]
async fn main() {
    println!("🧪 Kimi Code Provider Test");
    println!("==========================\n");

    // Load API key from auth profiles
    let auth_file = std::fs::read_to_string(format!(
        "{}/.openclaw/agents/main/agent/auth-profiles.json",
        env::var("HOME").unwrap_or_default()
    ));

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
            eprintln!("Set KIMI_API_KEY or update auth-profiles.json");
            std::process::exit(1);
        }
    };

    // Test the provider
    test_kimi_code(&api_key).await;
}

async fn test_kimi_code(api_key: &str) {
    println!("📡 Testing Kimi Code API...\n");

    // Build request (Anthropic format)
    let request_body = serde_json::json!({
        "model": "kimi-k2.5",
        "max_tokens": 1024,
        "temperature": 0.7,
        "messages": [
            {
                "role": "user",
                "content": "Say 'Hello from Pekobot!' and nothing else."
            }
        ]
    });

    let client = reqwest::Client::new();

    // Try different possible endpoints
    let endpoints = [
        "https://api.kimi-code.moonshot.cn/v1/messages",
        "https://api.moonshot.cn/v1/messages",
        "https://api.kimi-code.cn/v1/messages",
    ];

    for endpoint in &endpoints {
        println!("🔍 Trying endpoint: {}", endpoint);

        let response = client
            .post(*endpoint)
            .header("x-api-key", api_key.trim_start_matches("kimi-"))
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&request_body)
            .send()
            .await;

        match response {
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();

                if status.is_success() {
                    println!("✅ SUCCESS! Status: {}", status);

                    // Parse response
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                        if let Some(content) = json["content"][0]["text"].as_str() {
                            println!("\n📝 Response:\n{}\n", content.trim());
                        } else {
                            println!(
                                "\n📄 Raw response:\n{}\n",
                                serde_json::to_string_pretty(&json).unwrap_or_default()
                            );
                        }
                    }
                    return;
                } else {
                    println!("❌ FAILED! Status: {}", status);
                    println!("   Error: {}\n", body.chars().take(200).collect::<String>());
                }
            }
            Err(e) => {
                println!("❌ Connection error: {}\n", e);
            }
        }
    }

    eprintln!("❌ All endpoints failed!");
    std::process::exit(1);
}
