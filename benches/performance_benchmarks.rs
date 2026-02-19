//! Performance Benchmarks for Pekobot
//!
//! Run with: cargo bench

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use pekobot::{
    a2a::{
        flows::A2AFlowHandler,
        message::{A2AMessage, IntentPayload, MessageType, Payload, QuotePayload},
        protocol::A2AProtocol,
        registry::create_registry,
    },
    agent::{Agent, Orchestrator},
    config::Config,
    identity::did::{DIDScope, Identity},
    memory::sqlite::SqliteMemory,
};
use serde_json::json;
use std::time::Duration;
use tempfile::TempDir;

// ============================================================================
// Identity Benchmarks
// ============================================================================

fn benchmark_identity_generation(c: &mut Criterion) {
    let mut group = c.benchmark_group("identity_generation");
    group.sample_size(100);

    group.bench_function("generate_local_identity", |b| {
        b.iter(|| Identity::generate(DIDScope::Local, Some("benchmark")).unwrap())
    });

    group.bench_function("generate_public_identity", |b| {
        b.iter(|| Identity::generate(DIDScope::Public, None).unwrap())
    });

    group.bench_function("parse_did", |b| {
        let did = "did:pekobot:local:benchmark:abc123def456";
        b.iter(|| Identity::parse_did(black_box(did)).unwrap())
    });

    group.finish();
}

// ============================================================================
// Memory Benchmarks
// ============================================================================

fn benchmark_memory_operations(c: &mut Criterion) {
    let temp_dir = TempDir::new().unwrap();
    let db_path = temp_dir.path().join("bench.db");
    let memory = SqliteMemory::new(&db_path, "benchmark").unwrap();

    let mut group = c.benchmark_group("memory_operations");
    group.sample_size(100);

    // Store benchmark
    group.bench_function("store_small", |b| {
        let mut counter = 0;
        b.iter(|| {
            counter += 1;
            memory
                .store(
                    &format!("Test content {}", counter),
                    Some(json!({"index": counter})),
                )
                .unwrap()
        })
    });

    // Prepare data for search benchmark
    for i in 0..100 {
        memory
            .store(
                &format!("Searchable content number {}", i),
                Some(json!({"id": i})),
            )
            .unwrap();
    }

    group.bench_function("search", |b| {
        b.iter(|| memory.search(black_box("content"), 10).unwrap())
    });

    group.bench_function("get_by_id", |b| {
        let id = memory.store("Retrievable content", None).unwrap();
        b.iter(|| memory.get(black_box(&id)).unwrap())
    });

    group.bench_function("recent", |b| {
        b.iter(|| memory.recent(black_box(10)).unwrap())
    });

    group.finish();
}

fn benchmark_memory_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_throughput");
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(10));

    group.bench_function("store_1000_entries", |b| {
        b.iter(|| {
            let temp_dir = TempDir::new().unwrap();
            let db_path = temp_dir.path().join("throughput.db");
            let memory = SqliteMemory::new(&db_path, "benchmark").unwrap();

            for i in 0..1000 {
                memory
                    .store(&format!("Bulk content {}", i), Some(json!({"index": i})))
                    .unwrap();
            }
        })
    });

    group.finish();
}

// ============================================================================
// A2A Message Benchmarks
// ============================================================================

fn benchmark_a2a_message_creation(c: &mut Criterion) {
    let mut group = c.benchmark_group("a2a_message");
    group.sample_size(100);

    group.bench_function("create_intent", |b| {
        b.iter(|| {
            let intent = IntentPayload {
                task: "benchmark-task".to_string(),
                parameters: json!({"key": "value", "nested": {"data": [1, 2, 3]}}),
                request_quote: true,
                require_approval: false,
                timeout_seconds: Some(3600),
            };

            A2AMessage::new(
                "did:pekobot:local:sender",
                "did:pekobot:local:recipient",
                MessageType::Intent,
                Payload::Intent(intent),
            )
        })
    });

    group.bench_function("create_large_intent", |b| {
        let large_params = json!({
            "data": "x".repeat(10000),
            "array": (0..1000).collect::<Vec<i32>>(),
        });

        b.iter(|| {
            let intent = IntentPayload {
                task: "large-task".to_string(),
                parameters: large_params.clone(),
                request_quote: true,
                require_approval: false,
                timeout_seconds: Some(3600),
            };

            A2AMessage::new(
                "did:pekobot:local:sender",
                "did:pekobot:local:recipient",
                MessageType::Intent,
                Payload::Intent(intent),
            )
        })
    });

    // Pre-create message for reply benchmark
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

    group.bench_function("create_reply", |b| {
        let quote = QuotePayload {
            quote_id: "quote_123".to_string(),
            service_type: "test".to_string(),
            price: pekobot::a2a::message::Price {
                amount: 100.0,
                currency: "USD".to_string(),
                breakdown: None,
            },
            valid_until: chrono::Utc::now() + chrono::Duration::hours(24),
            terms: "Test".to_string(),
            estimated_duration: None,
        };

        b.iter(|| {
            original.reply_to(
                "did:pekobot:local:seller",
                MessageType::Quote,
                Payload::Quote(quote.clone()),
            )
        })
    });

    group.bench_function("serialize_intent", |b| {
        let intent = IntentPayload {
            task: "serialize-test".to_string(),
            parameters: json!({"data": "value"}),
            request_quote: true,
            require_approval: false,
            timeout_seconds: None,
        };
        let msg = A2AMessage::new(
            "did:pekobot:local:sender",
            "did:pekobot:local:recipient",
            MessageType::Intent,
            Payload::Intent(intent),
        );

        b.iter(|| serde_json::to_string(black_box(&msg)).unwrap())
    });

    group.finish();
}

// ============================================================================
// Flow Handler Benchmarks
// ============================================================================

fn benchmark_flow_handler(c: &mut Criterion) {
    let mut group = c.benchmark_group("flow_handler");
    group.sample_size(100);

    group.bench_function("handle_intent", |b| {
        let mut handler = A2AFlowHandler::new("did:pekobot:local:provider");
        let intent = IntentPayload {
            task: "benchmark".to_string(),
            parameters: json!({"key": "value"}),
            request_quote: true,
            require_approval: false,
            timeout_seconds: None,
        };
        let msg = A2AMessage::new(
            "did:pekobot:local:consumer",
            "did:pekobot:local:provider",
            MessageType::Intent,
            Payload::Intent(intent),
        );

        b.iter(|| {
            handler.handle_intent(
                black_box(&msg),
                black_box(&msg.payload_as_intent().unwrap()),
            )
        })
    });

    group.bench_function("handle_quote_auto_accept", |b| {
        let mut handler = A2AFlowHandler::new("did:pekobot:local:consumer");
        let quote = QuotePayload {
            quote_id: "quote_bench".to_string(),
            service_type: "test".to_string(),
            price: pekobot::a2a::message::Price {
                amount: 50.0,
                currency: "USD".to_string(),
                breakdown: None,
            },
            valid_until: chrono::Utc::now() + chrono::Duration::hours(24),
            terms: "Test".to_string(),
            estimated_duration: None,
        };
        let msg = A2AMessage::new(
            "did:pekobot:local:provider",
            "did:pekobot:local:consumer",
            MessageType::Quote,
            Payload::Quote(quote),
        );

        b.iter(|| {
            handler.handle_quote(black_box(&msg), black_box(&msg.payload_as_quote().unwrap()))
        })
    });

    group.finish();
}

// ============================================================================
// Agent Lifecycle Benchmarks
// ============================================================================

async fn create_agent(name: &str) -> Agent {
    let config = Config::agent(name).build();
    Agent::new(config).await.unwrap()
}

fn benchmark_agent_lifecycle(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let mut group = c.benchmark_group("agent_lifecycle");
    group.sample_size(50);

    group.bench_function("create_agent", |b| {
        b.to_async(&rt).iter(|| async {
            let config = Config::agent("bench-agent").build();
            Agent::new(config).await.unwrap()
        })
    });

    group.bench_function("create_agent_with_memory", |b| {
        b.to_async(&rt).iter(|| async {
            let config = Config::agent("bench-agent-mem").with_memory(true).build();
            Agent::new(config).await.unwrap()
        })
    });

    group.bench_function("agent_start_stop", |b| {
        b.to_async(&rt).iter(|| async {
            let agent = create_agent("lifecycle").await;
            agent.start().await.unwrap();
            agent.stop().await.unwrap();
        })
    });

    group.bench_function("agent_execute_echo", |b| {
        b.to_async(&rt).iter(|| async {
            let agent = create_agent("execute").await;
            agent.start().await.unwrap();
            agent.execute("Hello, benchmark!").await.unwrap();
            agent.stop().await.unwrap();
        })
    });

    group.finish();
}

// ============================================================================
// Registry Benchmarks
// ============================================================================

fn benchmark_registry_operations(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let mut group = c.benchmark_group("registry");
    group.sample_size(50);

    group.bench_function("register_single_agent", |b| {
        b.to_async(&rt).iter(|| async {
            let (registry, _receiver) = create_registry();
            let config = Config::agent("reg-agent").build();
            let agent = Agent::new(config).await.unwrap();
            registry.register(agent).await.unwrap();
        })
    });

    group.bench_function("list_10_agents", |b| {
        b.to_async(&rt).iter(|| async {
            let (registry, _receiver) = create_registry();

            for i in 0..10 {
                let config = Config::agent(&format!("agent-{}", i)).build();
                let agent = Agent::new(config).await.unwrap();
                registry.register(agent).await.unwrap();
            }

            registry.list_agents().await;
        })
    });

    group.bench_function("find_by_did", |b| {
        b.to_async(&rt).iter_with_setup(
            || {
                rt.block_on(async {
                    let (registry, _receiver) = create_registry();
                    let config = Config::agent("find-test").build();
                    let agent = Agent::new(config).await.unwrap();
                    let did = agent.did().to_string();
                    registry.register(agent).await.unwrap();
                    (registry, did)
                })
            },
            |(registry, did)| async move {
                registry.get_by_did(&did).await;
            },
        )
    });

    group.finish();
}

// ============================================================================
// Protocol Throughput Benchmarks
// ============================================================================

fn benchmark_protocol_throughput(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let mut group = c.benchmark_group("protocol_throughput");
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(10));

    group.bench_function("send_100_intents", |b| {
        b.to_async(&rt).iter(|| async {
            let (registry, _receiver) = create_registry();
            let protocol = A2AProtocol::new(registry);

            for i in 0..100 {
                protocol
                    .send_intent(
                        "did:pekobot:local:sender",
                        "did:pekobot:local:recipient",
                        &format!("task-{}", i),
                        json!({"index": i}),
                        true,
                    )
                    .await
                    .unwrap();
            }
        })
    });

    group.finish();
}

// ============================================================================
// Complete Flow Benchmarks
// ============================================================================

fn benchmark_complete_flow(c: &mut Criterion) {
    use pekobot::a2a::flows::FlowResult;

    let mut group = c.benchmark_group("complete_flow");
    group.sample_size(50);

    group.bench_function("intent_to_contract", |b| {
        b.iter(|| {
            let mut consumer = A2AFlowHandler::new("did:pekobot:local:consumer");
            let mut provider = A2AFlowHandler::new("did:pekobot:local:provider");

            // Intent
            let intent = IntentPayload {
                task: "test".to_string(),
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
                provider.handle_intent(&intent_msg, &intent_msg.payload_as_intent().unwrap())
            else {
                panic!("Expected quote")
            };

            let FlowResult::Response(accept_msg) =
                consumer.handle_quote(&quote_msg, &quote_msg.payload_as_quote().unwrap())
            else {
                panic!("Expected accept")
            };

            let FlowResult::Response(_contract_msg) =
                provider.handle_accept(&accept_msg, &accept_msg.payload_as_accept().unwrap())
            else {
                panic!("Expected contract")
            };
        })
    });

    group.finish();
}

// ============================================================================
// Criterion Groups
// ============================================================================

criterion_group!(
    benches,
    benchmark_identity_generation,
    benchmark_memory_operations,
    benchmark_memory_throughput,
    benchmark_a2a_message_creation,
    benchmark_flow_handler,
    benchmark_agent_lifecycle,
    benchmark_registry_operations,
    benchmark_protocol_throughput,
    benchmark_complete_flow
);

criterion_main!(benches);
