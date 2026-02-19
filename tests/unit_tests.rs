//! Unit tests for Pekobot core functionality

use pekobot::{
    agent::Agent,
    config::Config,
    identity::{did::DIDScope, Identity},
};

#[tokio::test]
async fn test_agent_creation_basic() {
    let config = Config::agent("test-agent")
        .with_description("A test agent")
        .build();

    let agent = Agent::new(config).await;
    assert!(agent.is_ok());

    let agent = agent.unwrap();
    assert_eq!(agent.name(), "test-agent");
    assert!(agent.did().starts_with("did:pekobot:"));
}

#[tokio::test]
async fn test_agent_lifecycle() {
    let config = Config::agent("lifecycle-test").build();
    let agent = Agent::new(config).await.unwrap();

    // Initial state should be Idle after creation
    assert_eq!(agent.state(), pekobot::types::agent::AgentState::Idle);

    // Start the agent
    agent.start().await.unwrap();
    assert_eq!(agent.state(), pekobot::types::agent::AgentState::Idle);

    // Stop the agent
    agent.stop().await.unwrap();
    // After stop, state may vary depending on implementation
}

#[tokio::test]
async fn test_agent_execution_without_provider() {
    let config = Config::agent("no-provider-test").build();
    let agent = Agent::new(config).await.unwrap();
    agent.start().await.unwrap();

    // Without a provider, should return echo response
    let result = agent.execute("Hello, World!").await;
    assert!(result.is_ok());

    let response = result.unwrap();
    assert!(response.contains("Echo:"));
    assert!(response.contains("Hello, World!"));
}

#[test]
fn test_identity_generation() {
    let identity = Identity::generate(DIDScope::Local, Some("test-tenant"), None);
    assert!(identity.is_ok());

    let identity = identity.unwrap();
    assert!(identity.did.starts_with("did:pekobot:local:test-tenant:"));
    assert_eq!(identity.scope, DIDScope::Local);
}

#[test]
fn test_did_parsing() {
    use pekobot::identity::did::DID;

    let did_str = "did:pekobot:local:acme:myagent";
    let did = DID::parse(did_str);
    assert!(did.is_ok());

    let did = did.unwrap();
    assert_eq!(did.method, "pekobot");
    assert_eq!(did.scope, DIDScope::Local);
    assert_eq!(did.tenant, Some("acme".to_string()));
    assert_eq!(did.identifier, "myagent");
}

#[test]
fn test_did_serialization() {
    use pekobot::identity::did::DID;

    let did = DID {
        method: "pekobot".to_string(),
        scope: DIDScope::Local,
        tenant: Some("test".to_string()),
        identifier: "agent123".to_string(),
    };

    let did_string = did.to_string();
    assert_eq!(did_string, "did:pekobot:local:test:agent123");

    // Roundtrip test
    let parsed = DID::parse(&did_string).unwrap();
    assert_eq!(parsed.method, did.method);
    assert_eq!(parsed.tenant, did.tenant);
    assert_eq!(parsed.identifier, did.identifier);
}

#[tokio::test]
async fn test_memory_operations() {
    let config = Config::agent("memory-test").with_memory(true).build();

    let agent = Agent::new(config).await.unwrap();
    agent.start().await.unwrap();

    // Store a memory
    let content = "This is a test memory";
    let metadata = Some(serde_json::json!({
        "test": true,
        "timestamp": "2024-01-01T00:00:00Z",
    }));

    let result = agent.store_memory(content, metadata);
    assert!(result.is_ok());

    // Search for the memory
    let search_results = agent.search_memory("test memory", 5);
    assert!(search_results.is_ok());

    let results = search_results.unwrap();
    // Should find at least one result
    assert!(!results.is_empty());
}

#[test]
fn test_config_builder() {
    let config = Config::agent("builder-test")
        .with_description("Test description")
        .with_capabilities(vec!["test".to_string(), "demo".to_string()])
        .with_memory(true)
        .build();

    assert_eq!(config.name, "builder-test");
    assert_eq!(config.description, Some("Test description".to_string()));
    assert_eq!(config.capabilities.len(), 2);
    assert!(config.memory.is_some());
}

#[test]
fn test_a2a_message_types() {
    use pekobot::a2a::A2AMessageType;

    // Test message type variants
    let intent = A2AMessageType::Intent {
        action: "test".to_string(),
        parameters: serde_json::json!({}),
    };

    match intent {
        A2AMessageType::Intent { action, .. } => {
            assert_eq!(action, "test");
        }
        _ => panic!("Wrong message type"),
    }
}

#[tokio::test]
async fn test_orchestrator_creation() {
    use pekobot::a2a::registry::create_registry;
    use pekobot::agent::Orchestrator;

    // Empty orchestrator
    let orch = Orchestrator::new();
    assert!(orch.registry().is_none());

    // Orchestrator with registry
    let (registry, _receiver) = create_registry();
    let orch = Orchestrator::with_registry(registry);
    assert!(orch.registry().is_some());
}

#[test]
fn test_provider_config() {
    use pekobot::types::provider::{ProviderConfig, ProviderType};

    let config = ProviderConfig {
        provider_type: ProviderType::OpenAI,
        api_key: Some("test-key".to_string()),
        base_url: Some("https://api.openai.com/v1".to_string()),
        timeout_seconds: 30,
        default_model: None,
        models: vec![],
    };

    assert_eq!(config.provider_type, ProviderType::OpenAI);
    assert_eq!(config.api_key, Some("test-key".to_string()));
}
