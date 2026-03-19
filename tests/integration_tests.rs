//! Integration tests for Pekobot
//!
//! Run with: cargo test --test integration_tests -- --ignored

use pekobot::{
    agent::Agent,
    types::agent::AgentConfig,
    types::provider::{ModelConfig, ProviderConfig, ProviderType},
};
use std::collections::HashMap;

/// Helper to create test agent config
fn test_agent_config(name: &str) -> AgentConfig {
    let mut models = HashMap::new();
    models.insert(
        "default".to_string(),
        ModelConfig {
            name: "gpt-4o-mini".to_string(),
            max_tokens: 4096,
            temperature: 0.7,
            top_p: 1.0,
            presence_penalty: 0.0,
            frequency_penalty: 0.0,
        },
    );

    AgentConfig {
        version: "1.0".to_string(),
        name: name.to_string(),
        description: Some(format!("Test agent: {}", name)),
        team: None,
        tenant: None,
        capabilities: vec![],
        provider: ProviderConfig {
            provider_type: ProviderType::OpenAI,
            api_key: None,
            api_key_env: Some("OPENAI_API_KEY".to_string()),
            base_url: None,
            default_model: "default".to_string(),
            models,
            timeout_seconds: 60,
            max_retries: 3,
            retry_delay_ms: 1000,
        },
        memory: None,
        tools: None,
        channels: None,
        auto_accept_trusted: false,
        approval_threshold: Some(100.0),
        default_timeout_seconds: 300,
        workspace: None,
        prompt: None,
    }
}

/// Test agent creation
#[tokio::test]
#[ignore = "Integration test - run manually"]
async fn test_agent_creation() {
    let config = test_agent_config("test-agent");
    let agent = Agent::new(config).await;
    assert!(agent.is_ok());
}

/// Test agent start/stop
#[tokio::test]
#[ignore = "Integration test - run manually"]
async fn test_agent_lifecycle() {
    let config = test_agent_config("lifecycle-test");
    let agent = Agent::new(config).await.unwrap();

    // Start agent
    let result = agent.start().await;
    assert!(result.is_ok());

    // Stop agent
    let result = agent.stop().await;
    assert!(result.is_ok());
}

/// Test agent identity (DID)
#[tokio::test]
#[ignore = "Integration test - run manually"]
async fn test_agent_identity() {
    let config = test_agent_config("identity-test");
    let agent = Agent::new(config).await.unwrap();

    let did = agent.did();
    assert!(!did.is_empty());
    assert!(did.starts_with("did:"));
}

/// Test multiple agents have unique DIDs
#[tokio::test]
#[ignore = "Integration test - run manually"]
async fn test_unique_dids() {
    let config1 = test_agent_config("unique-1");
    let config2 = test_agent_config("unique-2");

    let agent1 = Agent::new(config1).await.unwrap();
    let agent2 = Agent::new(config2).await.unwrap();

    assert_ne!(agent1.did(), agent2.did());
}
