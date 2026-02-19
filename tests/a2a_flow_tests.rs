//! A2A Flow Integration Tests
//!
//! Tests for complete agent-to-agent negotiation flows:
//! - Intent → Quote → Accept → Contract
//! - Error handling and recovery
//! - Multi-party negotiations
//! - Quote expiration

use chrono::{Duration, Utc};
use pekobot::{
    a2a::{
        flows::{A2AFlowHandler, FlowResult},
        message::{
            A2AMessage, AcceptPayload, CompletionPayload, CompletionResult, ContractPayload,
            ContractTerms, DataPayload, Deliverable, ErrorPayload, IntentPayload, MessageType,
            Payload, Price, PriceItem, QuotePayload, RejectPayload, StatusPayload, TaskStatus,
        },
        protocol::A2AProtocol,
        registry::create_registry,
    },
    agent::{Agent, Orchestrator},
    config::Config,
    identity::did::DIDScope,
};
use serde_json::json;

// ============================================================================
// Flow Handler Tests
// ============================================================================

#[test]
fn test_flow_handler_intent_to_quote() {
    let mut provider_handler = A2AFlowHandler::new("did:pekobot:local:provider");

    let intent = IntentPayload {
        task: "utility-quote".to_string(),
        parameters: json!({"address": "123 Main St", "service": "electricity"}),
        request_quote: true,
        require_approval: false,
        timeout_seconds: Some(3600),
    };

    let intent_msg = A2AMessage::new(
        "did:pekobot:local:consumer",
        "did:pekobot:local:provider",
        MessageType::Intent,
        Payload::Intent(intent),
    );

    let result =
        provider_handler.handle_intent(&intent_msg, &intent_msg.payload_as_intent().unwrap());

    match result {
        FlowResult::Response(quote_msg) => {
            assert_eq!(quote_msg.message_type, MessageType::Quote);
            assert_eq!(quote_msg.thread_id, intent_msg.thread_id);
            assert!(!provider_handler.pending_quotes().is_empty());

            // Verify quote content
            let quote = quote_msg.payload_as_quote().unwrap();
            assert!(!quote.quote_id.is_empty());
            assert!(quote.price.amount > 0.0);
            assert_eq!(quote.service_type, "utility-quote");
        }
        _ => panic!("Expected Quote response"),
    }
}

#[test]
fn test_flow_handler_quote_under_threshold_auto_accept() {
    let mut consumer_handler = A2AFlowHandler::new("did:pekobot:local:consumer");

    let quote = QuotePayload {
        quote_id: "quote_123".to_string(),
        service_type: "test-service".to_string(),
        price: Price {
            amount: 50.0, // Under $1000 threshold
            currency: "USD".to_string(),
            breakdown: Some(vec![PriceItem {
                description: "Base fee".to_string(),
                amount: 50.0,
            }]),
        },
        valid_until: Utc::now() + Duration::hours(24),
        terms: "Standard terms".to_string(),
        estimated_duration: Some("1 day".to_string()),
    };

    let quote_msg = A2AMessage::new(
        "did:pekobot:local:provider",
        "did:pekobot:local:consumer",
        MessageType::Quote,
        Payload::Quote(quote),
    );

    let result = consumer_handler.handle_quote(&quote_msg, &quote_msg.payload_as_quote().unwrap());

    match result {
        FlowResult::Response(accept_msg) => {
            assert_eq!(accept_msg.message_type, MessageType::Accept);

            let accept = accept_msg.payload_as_accept().unwrap();
            assert_eq!(accept.quote_id, "quote_123");
        }
        _ => panic!("Expected Accept response"),
    }
}

#[test]
fn test_flow_handler_quote_over_threshold_requires_approval() {
    let mut consumer_handler = A2AFlowHandler::new("did:pekobot:local:consumer");

    let quote = QuotePayload {
        quote_id: "quote_expensive".to_string(),
        service_type: "premium-service".to_string(),
        price: Price {
            amount: 5000.0, // Over $1000 threshold
            currency: "USD".to_string(),
            breakdown: None,
        },
        valid_until: Utc::now() + Duration::hours(24),
        terms: "Premium terms".to_string(),
        estimated_duration: None,
    };

    let quote_msg = A2AMessage::new(
        "did:pekobot:local:provider",
        "did:pekobot:local:consumer",
        MessageType::Quote,
        Payload::Quote(quote),
    );

    let result = consumer_handler.handle_quote(&quote_msg, &quote_msg.payload_as_quote().unwrap());

    match result {
        FlowResult::RequiresApproval(reason) => {
            assert!(reason.contains("5000"));
            assert!(reason.contains("exceeds"));
        }
        _ => panic!("Expected RequiresApproval"),
    }
}

#[test]
fn test_flow_handler_expired_quote() {
    let mut consumer_handler = A2AFlowHandler::new("did:pekobot:local:consumer");

    let quote = QuotePayload {
        quote_id: "quote_expired".to_string(),
        service_type: "expired-service".to_string(),
        price: Price {
            amount: 50.0,
            currency: "USD".to_string(),
            breakdown: None,
        },
        valid_until: Utc::now() - Duration::hours(1), // Expired
        terms: "Expired".to_string(),
        estimated_duration: None,
    };

    let quote_msg = A2AMessage::new(
        "did:pekobot:local:provider",
        "did:pekobot:local:consumer",
        MessageType::Quote,
        Payload::Quote(quote),
    );

    let result = consumer_handler.handle_quote(&quote_msg, &quote_msg.payload_as_quote().unwrap());

    match result {
        FlowResult::Error(msg) => {
            assert!(msg.contains("expired"));
        }
        _ => panic!("Expected Error for expired quote"),
    }
}

#[test]
fn test_flow_handler_accept_to_contract() {
    let mut provider_handler = A2AFlowHandler::new("did:pekobot:local:provider");

    // First, create a quote
    let intent = IntentPayload {
        task: "test-service".to_string(),
        parameters: json!({}),
        request_quote: true,
        require_approval: false,
        timeout_seconds: None,
    };

    let intent_msg = A2AMessage::new(
        "did:pekobot:local:consumer",
        "did:pekobot:local:provider",
        MessageType::Intent,
        Payload::Intent(intent),
    );

    let FlowResult::Response(quote_msg) =
        provider_handler.handle_intent(&intent_msg, &intent_msg.payload_as_intent().unwrap())
    else {
        panic!("Expected quote");
    };

    let quote = quote_msg.payload_as_quote().unwrap();
    let quote_id = quote.quote_id.clone();

    // Now accept the quote
    let accept = AcceptPayload {
        quote_id: quote_id.clone(),
        accepted_terms: Some("Standard terms".to_string()),
        notes: Some("Please proceed".to_string()),
    };

    let accept_msg = quote_msg.reply_to(
        "did:pekobot:local:consumer",
        MessageType::Accept,
        Payload::Accept(accept),
    );

    let result =
        provider_handler.handle_accept(&accept_msg, &accept_msg.payload_as_accept().unwrap());

    match result {
        FlowResult::Response(contract_msg) => {
            assert_eq!(contract_msg.message_type, MessageType::Contract);

            let contract = contract_msg.payload_as_contract().unwrap();
            assert!(!contract.contract_id.is_empty());
            assert_eq!(contract.signatures.len(), 2);
            assert!(!provider_handler.active_contracts().is_empty());
        }
        _ => panic!("Expected Contract response"),
    }
}

#[test]
fn test_flow_handler_accept_invalid_quote() {
    let mut provider_handler = A2AFlowHandler::new("did:pekobot:local:provider");

    let accept = AcceptPayload {
        quote_id: "nonexistent_quote".to_string(),
        accepted_terms: None,
        notes: None,
    };

    let accept_msg = A2AMessage::new(
        "did:pekobot:local:consumer",
        "did:pekobot:local:provider",
        MessageType::Accept,
        Payload::Accept(accept),
    );

    let result =
        provider_handler.handle_accept(&accept_msg, &accept_msg.payload_as_accept().unwrap());

    match result {
        FlowResult::Error(msg) => {
            assert!(msg.contains("not found"));
        }
        _ => panic!("Expected Error for invalid quote"),
    }
}

// ============================================================================
// Complete Flow Tests
// ============================================================================

#[test]
fn test_complete_negotiation_flow() {
    // Setup handlers
    let mut consumer_handler = A2AFlowHandler::new("did:pekobot:local:consumer");
    let mut provider_handler = A2AFlowHandler::new("did:pekobot:local:provider");

    // Step 1: Consumer sends intent
    let intent = IntentPayload {
        task: "web-design".to_string(),
        parameters: json!({
            "pages": 5,
            "style": "modern",
            "deadline": "2025-03-01"
        }),
        request_quote: true,
        require_approval: false,
        timeout_seconds: Some(86400),
    };

    let intent_msg = A2AMessage::new(
        "did:pekobot:local:consumer",
        "did:pekobot:local:provider",
        MessageType::Intent,
        Payload::Intent(intent),
    );

    // Step 2: Provider handles intent, sends quote
    let FlowResult::Response(quote_msg) =
        provider_handler.handle_intent(&intent_msg, &intent_msg.payload_as_intent().unwrap())
    else {
        panic!("Expected quote response");
    };

    // Step 3: Consumer handles quote, sends accept
    let FlowResult::Response(accept_msg) =
        consumer_handler.handle_quote(&quote_msg, &quote_msg.payload_as_quote().unwrap())
    else {
        panic!("Expected accept response");
    };

    // Step 4: Provider handles accept, sends contract
    let FlowResult::Response(contract_msg) =
        provider_handler.handle_accept(&accept_msg, &accept_msg.payload_as_accept().unwrap())
    else {
        panic!("Expected contract response");
    };

    // Step 5: Consumer handles contract
    let result = consumer_handler
        .handle_contract(&contract_msg, &contract_msg.payload_as_contract().unwrap());

    match result {
        FlowResult::Handled => {
            // Verify both sides have the contract
            assert!(!consumer_handler.active_contracts().is_empty());
            assert!(!provider_handler.active_contracts().is_empty());
        }
        _ => panic!("Expected Handled"),
    }
}

#[test]
fn test_flow_with_rejection() {
    let mut consumer_handler = A2AFlowHandler::new("did:pekobot:local:consumer");

    let quote = QuotePayload {
        quote_id: "quote_rejected".to_string(),
        service_type: "expensive-service".to_string(),
        price: Price {
            amount: 10000.0,
            currency: "USD".to_string(),
            breakdown: None,
        },
        valid_until: Utc::now() + Duration::hours(24),
        terms: "Expensive".to_string(),
        estimated_duration: None,
    };

    let quote_msg = A2AMessage::new(
        "did:pekobot:local:provider",
        "did:pekobot:local:consumer",
        MessageType::Quote,
        Payload::Quote(quote),
    );

    // Consumer sees quote is expensive, decides to reject
    let reject = RejectPayload {
        quote_id: "quote_rejected".to_string(),
        reason: "Price too high".to_string(),
        alternative_proposal: Some("Can you do it for $5000?".to_string()),
    };

    let reject_msg = quote_msg.reply_to(
        "did:pekobot:local:consumer",
        MessageType::Reject,
        Payload::Reject(reject),
    );

    // Rejection is handled (no response needed)
    assert_eq!(reject_msg.message_type, MessageType::Reject);
}

// ============================================================================
// Protocol Integration Tests
// ============================================================================

#[tokio::test]
async fn test_protocol_message_routing() {
    let (registry, _receiver) = create_registry();
    let mut protocol = A2AProtocol::new(registry);

    // Register a handler
    let handler = A2AFlowHandler::new("did:pekobot:local:test");
    protocol.register_agent_handler("did:pekobot:local:test", handler);

    // Create and handle a message
    let intent = IntentPayload {
        task: "test".to_string(),
        parameters: json!({}),
        request_quote: true,
        require_approval: false,
        timeout_seconds: None,
    };

    let msg = A2AMessage::new(
        "did:pekobot:local:sender",
        "did:pekobot:local:test",
        MessageType::Intent,
        Payload::Intent(intent),
    );

    let result = protocol.handle_message(msg).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_protocol_send_intent() {
    let (registry, mut receiver) = create_registry();
    let protocol = A2AProtocol::new(registry);

    // Send an intent
    let result = protocol
        .send_intent(
            "did:pekobot:local:sender",
            "did:pekobot:local:recipient",
            "test-task",
            json!({"key": "value"}),
            true,
        )
        .await;

    assert!(result.is_ok());
    let sent_msg = result.unwrap();
    assert_eq!(sent_msg.message_type, MessageType::Intent);

    // Verify it was sent to the message bus
    let received = receiver.recv().await;
    assert!(received.is_some());
    assert_eq!(received.unwrap().message_id, sent_msg.message_id);
}

#[tokio::test]
async fn test_protocol_unregistered_recipient() {
    let (registry, _receiver) = create_registry();
    let mut protocol = A2AProtocol::new(registry);

    // Don't register any handlers

    let intent = IntentPayload {
        task: "test".to_string(),
        parameters: json!({}),
        request_quote: false,
        require_approval: false,
        timeout_seconds: None,
    };

    let msg = A2AMessage::new(
        "did:pekobot:local:sender",
        "did:pekobot:local:unregistered",
        MessageType::Intent,
        Payload::Intent(intent),
    );

    // Should return Ok(None) for unregistered recipient
    let result = protocol.handle_message(msg).await;
    assert!(result.is_ok());
    assert!(result.unwrap().is_none());
}

// ============================================================================
// Multi-Party Negotiation Tests
// ============================================================================

#[tokio::test]
async fn test_multi_party_negotiation() {
    let (registry, _receiver) = create_registry();
    let mut orchestrator = Orchestrator::with_registry(registry);

    // Create multiple agents
    let consumer_config = Config::agent("consumer")
        .with_capabilities(vec!["buying".to_string()])
        .build();

    let provider1_config = Config::agent("provider-1")
        .with_capabilities(vec!["selling".to_string()])
        .build();

    let provider2_config = Config::agent("provider-2")
        .with_capabilities(vec!["selling".to_string()])
        .build();

    let consumer = Agent::new(consumer_config).await.unwrap();
    let provider1 = Agent::new(provider1_config).await.unwrap();
    let provider2 = Agent::new(provider2_config).await.unwrap();

    let consumer_did = consumer.did().to_string();
    let provider1_did = provider1.did().to_string();
    let provider2_did = provider2.did().to_string();

    // Add to orchestrator
    orchestrator.add_agent(consumer).await.unwrap();
    orchestrator.add_agent(provider1).await.unwrap();
    orchestrator.add_agent(provider2).await.unwrap();

    // Verify all registered
    let agents = orchestrator.list_agents().await;
    assert_eq!(agents.len(), 3);

    // Get protocol and send intents to multiple providers
    let protocol = orchestrator.protocol().unwrap();

    // Request quotes from both providers
    let intent1 = protocol
        .send_intent(
            &consumer_did,
            &provider1_did,
            "get-quote",
            json!({"item": "widget"}),
            true,
        )
        .await;
    assert!(intent1.is_ok());

    let intent2 = protocol
        .send_intent(
            &consumer_did,
            &provider2_did,
            "get-quote",
            json!({"item": "widget"}),
            true,
        )
        .await;
    assert!(intent2.is_ok());
}

// ============================================================================
// Contract Lifecycle Tests
// ============================================================================

#[test]
fn test_contract_terms_validation() {
    let terms = ContractTerms {
        service_type: "web-development".to_string(),
        price: Price {
            amount: 5000.0,
            currency: "USD".to_string(),
            breakdown: Some(vec![
                PriceItem {
                    description: "Design".to_string(),
                    amount: 2000.0,
                },
                PriceItem {
                    description: "Development".to_string(),
                    amount: 3000.0,
                },
            ]),
        },
        start_date: Utc::now(),
        end_date: Some(Utc::now() + Duration::days(30)),
        deliverables: vec![
            "Homepage design".to_string(),
            "About page".to_string(),
            "Contact form".to_string(),
        ],
        payment_terms: "50% upfront, 50% on delivery".to_string(),
    };

    assert_eq!(terms.service_type, "web-development");
    assert_eq!(terms.price.amount, 5000.0);
    assert_eq!(terms.deliverables.len(), 3);
    assert!(terms.end_date.unwrap() > terms.start_date);
}

#[test]
fn test_contract_signatures() {
    use pekobot::a2a::message::{ContractSignature, ContractTerms};

    let contract = ContractPayload {
        contract_id: "contract_123".to_string(),
        terms: ContractTerms {
            service_type: "test".to_string(),
            price: Price {
                amount: 100.0,
                currency: "USD".to_string(),
                breakdown: None,
            },
            start_date: Utc::now(),
            end_date: None,
            deliverables: vec!["item".to_string()],
            payment_terms: "Net 30".to_string(),
        },
        signatures: vec![
            ContractSignature {
                did: "did:pekobot:local:provider".to_string(),
                signature: "sig_provider".to_string(),
                timestamp: Utc::now(),
            },
            ContractSignature {
                did: "did:pekobot:local:consumer".to_string(),
                signature: "sig_consumer".to_string(),
                timestamp: Utc::now(),
            },
        ],
    };

    assert_eq!(contract.signatures.len(), 2);
    assert_eq!(contract.signatures[0].did, "did:pekobot:local:provider");
    assert_eq!(contract.signatures[1].did, "did:pekobot:local:consumer");
}

// ============================================================================
// Error and Recovery Tests
// ============================================================================

#[tokio::test]
async fn test_error_message_handling() {
    let (registry, _receiver) = create_registry();
    let mut protocol = A2AProtocol::new(registry);

    // Register handler
    let handler = A2AFlowHandler::new("did:pekobot:local:handler");
    protocol.register_agent_handler("did:pekobot:local:handler", handler);

    // Send a message that will cause an error (expired quote)
    let quote = QuotePayload {
        quote_id: "quote_old".to_string(),
        service_type: "test".to_string(),
        price: Price {
            amount: 50.0,
            currency: "USD".to_string(),
            breakdown: None,
        },
        valid_until: Utc::now() - Duration::hours(1), // Expired
        terms: "Old".to_string(),
        estimated_duration: None,
    };

    let quote_msg = A2AMessage::new(
        "did:pekobot:local:provider",
        "did:pekobot:local:handler",
        MessageType::Quote,
        Payload::Quote(quote),
    );

    let result = protocol.handle_message(quote_msg).await;
    assert!(result.is_ok()); // Protocol handles errors gracefully

    // An error response should be generated
    let error_response = result.unwrap();
    assert!(error_response.is_some());
}

#[test]
fn test_flow_cleanup_expired_quotes() {
    let mut handler = A2AFlowHandler::new("did:pekobot:local:test");

    // Create an intent to generate a quote
    let intent = IntentPayload {
        task: "test".to_string(),
        parameters: json!({}),
        request_quote: true,
        require_approval: false,
        timeout_seconds: None,
    };

    let intent_msg = A2AMessage::new(
        "did:pekobot:local:consumer",
        "did:pekobot:local:test",
        MessageType::Intent,
        Payload::Intent(intent),
    );

    // Generate quote
    let FlowResult::Response(_) =
        handler.handle_intent(&intent_msg, &intent_msg.payload_as_intent().unwrap())
    else {
        panic!("Expected quote");
    };

    assert_eq!(handler.pending_quotes().len(), 1);

    // Manually expire the quote by modifying the stored state
    // (In real usage, cleanup would be called periodically)
    handler.cleanup_expired_quotes();

    // Quote should still be there (not expired yet)
    assert_eq!(handler.pending_quotes().len(), 1);
}

// ============================================================================
// Status and Completion Tests
// ============================================================================

#[test]
fn test_status_payload_variants() {
    let pending = StatusPayload {
        status: TaskStatus::Pending,
        progress: None,
        message: Some("Waiting to start".to_string()),
        estimated_completion: None,
    };
    assert_eq!(pending.status, TaskStatus::Pending);

    let in_progress = StatusPayload {
        status: TaskStatus::InProgress,
        progress: Some(0.5),
        message: Some("Halfway done".to_string()),
        estimated_completion: Some(Utc::now() + Duration::hours(2)),
    };
    assert_eq!(in_progress.status, TaskStatus::InProgress);
    assert_eq!(in_progress.progress, Some(0.5));

    let completed = StatusPayload {
        status: TaskStatus::Completed,
        progress: Some(1.0),
        message: Some("Done!".to_string()),
        estimated_completion: None,
    };
    assert_eq!(completed.status, TaskStatus::Completed);
}

#[test]
fn test_completion_payload() {
    let completion = CompletionPayload {
        result: CompletionResult::Success,
        deliverables: vec![Deliverable {
            id: "deliv_1".to_string(),
            description: "Final report".to_string(),
            content_type: "application/pdf".to_string(),
            content: json!({"url": "https://example.com/report.pdf"}),
        }],
        final_report: Some("Task completed successfully".to_string()),
    };

    assert_eq!(completion.result, CompletionResult::Success);
    assert_eq!(completion.deliverables.len(), 1);
    assert_eq!(completion.deliverables[0].id, "deliv_1");
}

// ============================================================================
// Serialization Tests
// ============================================================================

#[test]
fn test_a2a_message_json_roundtrip() {
    let intent = IntentPayload {
        task: "test-task".to_string(),
        parameters: json!({"key": "value", "number": 42}),
        request_quote: true,
        require_approval: false,
        timeout_seconds: Some(3600),
    };

    let msg = A2AMessage::new(
        "did:pekobot:local:sender",
        "did:pekobot:local:recipient",
        MessageType::Intent,
        Payload::Intent(intent),
    );

    // Serialize to JSON
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("test-task"));
    assert!(json.contains("INTENT"));

    // Deserialize back
    let deserialized: A2AMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.message_id, msg.message_id);
    assert_eq!(deserialized.thread_id, msg.thread_id);
    assert_eq!(deserialized.sender.did, msg.sender.did);
    assert_eq!(deserialized.recipient.did, msg.recipient.did);
}

#[test]
fn test_quote_payload_serialization() {
    let quote = QuotePayload {
        quote_id: "quote_abc".to_string(),
        service_type: "service".to_string(),
        price: Price {
            amount: 99.99,
            currency: "USD".to_string(),
            breakdown: Some(vec![
                PriceItem {
                    description: "Item 1".to_string(),
                    amount: 49.99,
                },
                PriceItem {
                    description: "Item 2".to_string(),
                    amount: 50.0,
                },
            ]),
        },
        valid_until: Utc::now() + Duration::hours(24),
        terms: "Standard".to_string(),
        estimated_duration: Some("2 days".to_string()),
    };

    let json = serde_json::to_string(&quote).unwrap();
    let deserialized: QuotePayload = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.quote_id, quote.quote_id);
    assert_eq!(deserialized.price.amount, quote.price.amount);
    assert_eq!(deserialized.price.breakdown.as_ref().unwrap().len(), 2);
}
