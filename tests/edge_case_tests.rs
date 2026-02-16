//! Edge Case Tests for Pekobot
//!
//! Tests for boundary conditions, error handling, and unusual inputs.

use pekobot::{
    agent::{Agent, Orchestrator},
    a2a::{
        message::{A2AMessage, AcceptPayload, ContractPayload, IntentPayload, MessageType, Payload, Price, QuotePayload, TaskStatus},
        registry::create_registry,
    },
    config::Config,
    identity::{did::{DIDScope, Identity}, keys::KeyPair},
};
use serde_json::json;

// ============================================================================
// Identity Edge Cases
// ============================================================================

#[test]
fn test_did_parsing_edge_cases() {
    // Empty DID should fail
    let result = Identity::parse_did("");
    assert!(result.is_err());

    // Invalid prefix
    let result = Identity::parse_did("did:invalid:local:test");
    assert!(result.is_err());

    // Missing components
    let result = Identity::parse_did("did:pekobot");
    assert!(result.is_err());

    // Invalid scope
    let result = Identity::parse_did("did:pekobot:invalid:test");
    assert!(result.is_err());

    // Too many components
    let result = Identity::parse_did("did:pekobot:local:tenant:extra:more");
    assert!(result.is_err());

    // Valid minimal DID
    let result = Identity::parse_did("did:pekobot:public:abc123");
    assert!(result.is_ok());
}

#[test]
fn test_identity_with_special_tenant_names() {
    // Tenant with hyphens
    let identity = Identity::generate(DIDScope::Local, Some("my-tenant-name"));
    assert!(identity.is_ok());

    // Tenant with numbers
    let identity = Identity::generate(DIDScope::Local, Some("tenant123"));
    assert!(identity.is_ok());

    // Very long tenant name
    let long_tenant = "a".repeat(100);
    let identity = Identity::generate(DIDScope::Local, Some(&long_tenant));
    assert!(identity.is_ok());

    // Unicode tenant (should work)
    let identity = Identity::generate(DIDScope::Local, Some("テナント"));
    assert!(identity.is_ok());
}

#[test]
fn test_identity_key_operations() {
    // Generate multiple identities - each should be unique
    let identity1 = Identity::generate(DIDScope::Local, Some("test")).unwrap();
    let identity2 = Identity::generate(DIDScope::Local, Some("test")).unwrap();

    // DIDs should be different (different keys)
    assert_ne!(identity1.did, identity2.did);

    // Both should have valid keypairs
    assert!(identity1.keypair.is_some());
    assert!(identity2.keypair.is_some());

    // Document IDs should match their DIDs
    assert_eq!(identity1.document.id, identity1.did);
    assert_eq!(identity2.document.id, identity2.did);
}

// ============================================================================
// A2A Message Edge Cases
// ============================================================================

#[test]
fn test_a2a_message_with_empty_fields() {
    let intent = IntentPayload {
        task: "".to_string(), // Empty task
        parameters: json!({}),
        request_quote: false,
        require_approval: false,
        timeout_seconds: None,
    };

    let msg = A2AMessage::new(
        "did:pekobot:local:sender",
        "did:pekobot:local:recipient",
        MessageType::Intent,
        Payload::Intent(intent),
    );

    assert!(!msg.message_id.is_empty());
    assert!(!msg.thread_id.is_empty());
}

#[test]
fn test_a2a_message_with_large_payload() {
    // Create a large parameters object
    let large_params = json!({
        "data": "x".repeat(10000),
        "nested": {
            "array": (0..1000).collect::<Vec<i32>>(),
        }
    });

    let intent = IntentPayload {
        task: "large-task".to_string(),
        parameters: large_params,
        request_quote: true,
        require_approval: false,
        timeout_seconds: Some(u64::MAX),
    };

    let msg = A2AMessage::new(
        "did:pekobot:local:sender",
        "did:pekobot:local:recipient",
        MessageType::Intent,
        Payload::Intent(intent),
    );

    assert_eq!(msg.a2a_version, "1.0");
}

#[test]
fn test_a2a_reply_preserves_thread_id() {
    let intent = IntentPayload {
        task: "test".to_string(),
        parameters: json!({}),
        request_quote: true,
        require_approval: false,
        timeout_seconds: None,
    };

    let original = A2AMessage::new(
        "did:pekobot:local:buyer",
        "did:pekobot:local:seller",
        MessageType::Intent,
        Payload::Intent(intent),
    );

    let quote = QuotePayload {
        quote_id: "quote_123".to_string(),
        service_type: "test".to_string(),
        price: Price {
            amount: 100.0,
            currency: "USD".to_string(),
            breakdown: None,
        },
        valid_until: chrono::Utc::now() + chrono::Duration::hours(24),
        terms: "Test terms".to_string(),
        estimated_duration: None,
    };

    let reply = original.reply_to(
        "did:pekobot:local:seller",
        MessageType::Quote,
        Payload::Quote(quote),
    );

    // Thread ID should be preserved
    assert_eq!(reply.thread_id, original.thread_id);

    // Sender and recipient should be swapped
    assert_eq!(reply.sender.did, "did:pekobot:local:seller");
    assert_eq!(reply.recipient.did, original.sender.did);

    // Message ID should be different
    assert_ne!(reply.message_id, original.message_id);
}

#[test]
fn test_quote_payload_validation() {
    // Zero price
    let quote = QuotePayload {
        quote_id: "quote_zero".to_string(),
        service_type: "free".to_string(),
        price: Price {
            amount: 0.0,
            currency: "USD".to_string(),
            breakdown: None,
        },
        valid_until: chrono::Utc::now() + chrono::Duration::hours(1),
        terms: "Free service".to_string(),
        estimated_duration: None,
    };
    assert_eq!(quote.price.amount, 0.0);

    // Very large price
    let quote = QuotePayload {
        quote_id: "quote_large".to_string(),
        service_type: "expensive".to_string(),
        price: Price {
            amount: f64::MAX,
            currency: "USD".to_string(),
            breakdown: None,
        },
        valid_until: chrono::Utc::now() + chrono::Duration::hours(1),
        terms: "Expensive service".to_string(),
        estimated_duration: None,
    };
    assert_eq!(quote.price.amount, f64::MAX);

    // Expired quote
    let expired_quote = QuotePayload {
        quote_id: "quote_expired".to_string(),
        service_type: "expired".to_string(),
        price: Price {
            amount: 100.0,
            currency: "USD".to_string(),
            breakdown: None,
        },
        valid_until: chrono::Utc::now() - chrono::Duration::hours(1),
        terms: "Expired".to_string(),
        estimated_duration: None,
    };
    assert!(expired_quote.valid_until < chrono::Utc::now());
}

// ============================================================================
// Agent Configuration Edge Cases
// ============================================================================

#[test]
fn test_config_with_empty_name() {
    let config = Config::agent("").build();
    assert_eq!(config.name, "");
}

#[test]
fn test_config_with_very_long_name() {
    let long_name = "a".repeat(1000);
    let config = Config::agent(&long_name).build();
    assert_eq!(config.name, long_name);
}

#[test]
fn test_config_with_unicode_name() {
    let config = Config::agent("エージェント🤖").build();
    assert_eq!(config.name, "エージェント🤖");
}

#[test]
fn test_config_with_many_capabilities() {
    let capabilities: Vec<String> = (0..100).map(|i| format!("cap-{}", i)).collect();
    let config = Config::agent("many-caps")
        .with_capabilities(capabilities.clone())
        .build();
    assert_eq!(config.capabilities.len(), 100);
}

#[test]
fn test_config_builder_chaining() {
    let config = Config::agent("chained")
        .with_description("Desc")
        .with_capabilities(vec!["a".to_string()])
        .with_memory(true)
        .build();

    assert_eq!(config.name, "chained");
    assert_eq!(config.description, Some("Desc".to_string()));
    assert_eq!(config.capabilities.len(), 1);
    assert!(config.memory.is_some());
}

// ============================================================================
// Memory Edge Cases
// ============================================================================

#[tokio::test]
async fn test_agent_memory_with_unicode() {
    let config = Config::agent("unicode-memory")
        .with_memory(true)
        .build();

    let agent = Agent::new(config).await.unwrap();
    agent.start().await.unwrap();

    // Store unicode content
    let unicode_content = "Hello 世界 🌍 Привет мир! مرحبا بالعالم";
    let result = agent.store_memory(unicode_content, None);
    assert!(result.is_ok());

    // Search for unicode
    let results = agent.search_memory("世界", 5).unwrap();
    assert!(!results.is_empty());

    agent.stop().await.unwrap();
}

#[tokio::test]
async fn test_agent_memory_with_special_chars() {
    let config = Config::agent("special-memory")
        .with_memory(true)
        .build();

    let agent = Agent::new(config).await.unwrap();
    agent.start().await.unwrap();

    // Content with special characters that might break SQL
    let special_content = "'; DROP TABLE memory_entries; -- \" OR 1=1; <script>alert('xss')</script>";
    let result = agent.store_memory(special_content, None);
    assert!(result.is_ok());

    // Verify table still exists by searching
    let results = agent.search_memory("DROP", 5).unwrap();
    assert!(!results.is_empty());

    agent.stop().await.unwrap();
}

#[tokio::test]
async fn test_agent_memory_with_large_content() {
    let config = Config::agent("large-memory")
        .with_memory(true)
        .build();

    let agent = Agent::new(config).await.unwrap();
    agent.start().await.unwrap();

    // Large content
    let large_content = "x".repeat(100000);
    let result = agent.store_memory(&large_content, None);
    assert!(result.is_ok());

    agent.stop().await.unwrap();
}

#[tokio::test]
async fn test_agent_without_memory() {
    let config = Config::agent("no-memory").build();
    let agent = Agent::new(config).await.unwrap();

    // Should fail when trying to use memory
    let result = agent.store_memory("test", None);
    assert!(result.is_err());

    // Search should return empty, not error
    let results = agent.search_memory("test", 5).unwrap();
    assert!(results.is_empty());
}

// ============================================================================
// Orchestrator Edge Cases
// ============================================================================

#[tokio::test]
async fn test_orchestrator_with_duplicate_agents() {
    let (registry, _receiver) = create_registry();
    let mut orchestrator = Orchestrator::with_registry(registry);

    // Create two agents with same config name but different DIDs
    let config1 = Config::agent("duplicate").build();
    let config2 = Config::agent("duplicate").build();

    let agent1 = Agent::new(config1).await.unwrap();
    let agent2 = Agent::new(config2).await.unwrap();

    // Should be able to add both (different DIDs)
    orchestrator.add_agent(agent1).await.unwrap();
    orchestrator.add_agent(agent2).await.unwrap();

    // Should have 2 agents
    let agents = orchestrator.list_agents().await;
    assert_eq!(agents.len(), 2);

    // DIDs should be different
    assert_ne!(agents[0].0, agents[1].0);
}

#[tokio::test]
async fn test_orchestrator_find_nonexistent() {
    let (registry, _receiver) = create_registry();
    let orchestrator = Orchestrator::with_registry(registry);

    // Find non-existent agent
    let result = orchestrator.find_by_did("did:pekobot:local:nonexistent").await;
    assert!(result.is_none());

    let result = orchestrator.find_by_name("nonexistent").await;
    assert!(result.is_none());
}

#[tokio::test]
async fn test_orchestrator_empty_operations() {
    let (registry, _receiver) = create_registry();
    let orchestrator = Orchestrator::with_registry(registry);

    // Should not error when empty
    orchestrator.start_all().await.unwrap();
    orchestrator.stop_all().await.unwrap();

    // List should be empty
    let agents = orchestrator.list_agents().await;
    assert!(agents.is_empty());
}

// ============================================================================
// Concurrent Operation Tests
// ============================================================================

#[tokio::test]
async fn test_concurrent_agent_creation() {
    let mut handles = vec![];

    for i in 0..10 {
        let handle = tokio::spawn(async move {
            let config = Config::agent(&format!("concurrent-{}", i))
                .with_memory(true)
                .build();
            Agent::new(config).await
        });
        handles.push(handle);
    }

    // Wait for all to complete
    for handle in handles {
        let result = handle.await.unwrap();
        assert!(result.is_ok());
    }
}

#[tokio::test]
async fn test_concurrent_memory_operations() {
    let config = Config::agent("concurrent-mem")
        .with_memory(true)
        .build();

    let agent = Agent::new(config).await.unwrap();

    // Spawn multiple store operations
    let mut handles = vec![];
    for i in 0..20 {
        let handle = tokio::task::spawn_blocking(move || {
            agent.store_memory(&format!("Memory {}", i), None)
        });
        handles.push(handle);
    }

    // All should complete without error
    for handle in handles {
        let result = handle.await.unwrap();
        assert!(result.is_ok());
    }
}

// ============================================================================
// Error Handling Tests
// ============================================================================

#[tokio::test]
async fn test_agent_execution_without_provider() {
    let config = Config::agent("no-provider").build();
    let agent = Agent::new(config).await.unwrap();
    agent.start().await.unwrap();

    // Should return echo response when no provider
    let result = agent.execute("Hello").await;
    assert!(result.is_ok());
    assert!(result.unwrap().contains("Echo:"));
}

#[tokio::test]
async fn test_agent_double_start() {
    let config = Config::agent("double-start").build();
    let agent = Agent::new(config).await.unwrap();

    // First start
    agent.start().await.unwrap();

    // Second start should be idempotent (not fail)
    agent.start().await.unwrap();

    agent.stop().await.unwrap();
}

#[tokio::test]
async fn test_agent_stop_before_start() {
    let config = Config::agent("stop-first").build();
    let agent = Agent::new(config).await.unwrap();

    // Stop before start - should not error
    agent.stop().await.unwrap();
}
