//! Integration tests for Pekobot
//!
//! These tests verify end-to-end functionality including:
//! - Multi-agent communication
//! - A2A protocol flows
//! - Memory persistence
//! - Provider interactions (mocked)

use pekobot::{
    agent::{Agent, Orchestrator},
    a2a::registry::create_registry,
    config::Config,
};

/// Test complete multi-agent workflow
#[tokio::test]
async fn test_multi_agent_workflow() {
    let (registry, _receiver) = create_registry();
    let mut orchestrator = Orchestrator::with_registry(registry);

    // Create two agents
    let agent1_config = Config::agent("agent-1")
        .with_capabilities(vec!["task".to_string()])
        .with_memory(true)
        .build();

    let agent2_config = Config::agent("agent-2")
        .with_capabilities(vec!["response".to_string()])
        .with_memory(true)
        .build();

    let agent1 = Agent::new(agent1_config).await.unwrap();
    let agent2 = Agent::new(agent2_config).await.unwrap();

    let did1 = agent1.did().to_string();
    let did2 = agent2.did().to_string();

    // Add agents to orchestrator
    orchestrator.add_agent(agent1).await.unwrap();
    orchestrator.add_agent(agent2).await.unwrap();

    // Verify both agents are registered
    let agents = orchestrator.list_agents().await;
    assert_eq!(agents.len(), 2);

    // Verify we can find agents by DID
    let found1 = orchestrator.find_by_did(&did1).await;
    let found2 = orchestrator.find_by_did(&did2).await;

    assert!(found1.is_some());
    assert!(found2.is_some());

    // Start all agents
    orchestrator.start_all().await.unwrap();

    // Execute tasks on each agent
    let agent1 = orchestrator.find_by_did(&did1).await.unwrap();
    {
        let a1 = agent1.lock().await;
        let result = a1.execute("Task from agent 1").await;
        assert!(result.is_ok());
    }

    let agent2 = orchestrator.find_by_did(&did2).await.unwrap();
    {
        let a2 = agent2.lock().await;
        let result = a2.execute("Task from agent 2").await;
        assert!(result.is_ok());
    }

    // Stop all agents
    orchestrator.stop_all().await.unwrap();
}

/// Test agent memory persistence across operations
#[tokio::test]
async fn test_memory_persistence() {
    let config = Config::agent("memory-test")
        .with_memory(true)
        .build();

    let agent = Agent::new(config).await.unwrap();
    agent.start().await.unwrap();

    // Store multiple memories
    let memories = vec![
        ("First memory", serde_json::json!({"order": 1})),
        ("Second memory", serde_json::json!({"order": 2})),
        ("Third memory about cats", serde_json::json!({"order": 3, "topic": "cats"})),
    ];

    for (content, metadata) in memories {
        let result = agent.store_memory(content, Some(metadata));
        assert!(result.is_ok());
    }

    // Search for specific memories
    let cat_results = agent.search_memory("cats", 5).unwrap();
    assert!(!cat_results.is_empty());

    // Search should find "memory" in multiple entries
    let memory_results = agent.search_memory("memory", 10).unwrap();
    assert!(memory_results.len() >= 3);

    agent.stop().await.unwrap();
}

/// Test agent state transitions
#[tokio::test]
async fn test_agent_state_transitions() {
    use pekobot::types::agent::AgentState;

    let config = Config::agent("state-test").build();
    let agent = Agent::new(config).await.unwrap();

    // Initial state
    assert_eq!(agent.state(), AgentState::Idle);

    // Start agent
    agent.start().await.unwrap();
    assert_eq!(agent.state(), AgentState::Idle);

    // Execute (should go Busy then back to Idle)
    let _ = agent.execute("Test").await;
    assert_eq!(agent.state(), AgentState::Idle);

    // Stop agent
    agent.stop().await.unwrap();
}

/// Test identity uniqueness
#[tokio::test]
async fn test_identity_uniqueness() {
    let config1 = Config::agent("unique-test-1").build();
    let config2 = Config::agent("unique-test-2").build();

    let agent1 = Agent::new(config1).await.unwrap();
    let agent2 = Agent::new(config2).await.unwrap();

    // Each agent should have a unique DID
    assert_ne!(agent1.did(), agent2.did());

    // DIDs should be valid
    assert!(agent1.did().starts_with("did:pekobot:"));
    assert!(agent2.did().starts_with("did:pekobot:"));
}

/// Test orchestrator with no agents
#[tokio::test]
async fn test_empty_orchestrator() {
    let (registry, _receiver) = create_registry();
    let orchestrator = Orchestrator::with_registry(registry);

    let agents = orchestrator.list_agents().await;
    assert!(agents.is_empty());

    // Should not error on start/stop with no agents
    orchestrator.start_all().await.unwrap();
    orchestrator.stop_all().await.unwrap();
}

/// Test configuration variations
#[tokio::test]
async fn test_config_variations() {
    // Agent without memory
    let config_no_memory = Config::agent("no-memory").build();
    let agent_no_mem = Agent::new(config_no_memory).await.unwrap();
    
    // Trying to use memory should fail
    let result = agent_no_mem.store_memory("test", None);
    assert!(result.is_err());

    // Agent with memory
    let config_with_memory = Config::agent("with-memory")
        .with_memory(true)
        .build();
    let agent_with_mem = Agent::new(config_with_memory).await.unwrap();
    
    let result = agent_with_mem.store_memory("test", None);
    assert!(result.is_ok());
}
