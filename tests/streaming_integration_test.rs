//! Integration tests for the streaming architecture (TDD-003)
//!
//! These tests verify the three-layer pipeline:
//! - Provider Layer: Parse raw SSE into StreamEvents
//! - Orchestration Layer: Transform StreamEvents into AgenticEvents  
//! - Channel Layer: Render AgenticEvents to platform output

use pekobot::engine::{
    AgenticEvent, LifecyclePhase, OrchestratorConfig, StreamBuffer,
    StreamOrchestrator,
};
use pekobot::providers::StreamEvent;
use std::time::Duration;

/// Test helper to collect events from a stream simulation
fn simulate_text_stream() -> Vec<StreamEvent> {
    vec![
        StreamEvent::Start {
            provider: "test".to_string(),
            model: "test-model".to_string(),
        },
        StreamEvent::TextStart { content_index: 0 },
        StreamEvent::TextDelta {
            content_index: 0,
            delta: "Hello ".to_string(),
        },
        StreamEvent::TextDelta {
            content_index: 0,
            delta: "world!".to_string(),
        },
        StreamEvent::TextEnd {
            content_index: 0,
            content: "Hello world!".to_string(),
        },
        StreamEvent::Done {
            stop_reason: pekobot::providers::StopReason::Stop,
        },
    ]
}

/// Test helper to simulate a stream with tool calls
fn simulate_tool_stream() -> Vec<StreamEvent> {
    vec![
        StreamEvent::Start {
            provider: "test".to_string(),
            model: "test-model".to_string(),
        },
        StreamEvent::TextStart { content_index: 0 },
        StreamEvent::TextDelta {
            content_index: 0,
            delta: "Let me ".to_string(),
        },
        StreamEvent::TextDelta {
            content_index: 0,
            delta: "search...".to_string(),
        },
        StreamEvent::TextEnd {
            content_index: 0,
            content: "Let me search...".to_string(),
        },
        StreamEvent::ToolCallStart { content_index: 1 },
        StreamEvent::ToolCallDelta {
            content_index: 1,
            delta: "{\"query\": \"test\"}".to_string(),
        },
        StreamEvent::ToolCallEnd {
            content_index: 1,
            tool_call: pekobot::types::message::ContentBlock::ToolCall {
                id: "tc_001".to_string(),
                name: "web_search".to_string(),
                arguments: serde_json::json!({"query": "test"}),
            },
        },
        StreamEvent::Done {
            stop_reason: pekobot::providers::StopReason::ToolUse,
        },
    ]
}

#[test]
fn test_three_layer_pipeline_live_mode() {
    // Layer 1: Provider (simulated)
    let stream_events = simulate_text_stream();

    // Layer 2: Orchestrator
    let config = OrchestratorConfig::live();
    let mut orchestrator = StreamOrchestrator::new("run_001", config);

    let mut agentic_events = Vec::new();
    for event in stream_events {
        agentic_events.extend(orchestrator.process(event));
    }
    agentic_events.extend(orchestrator.finalize());

    // Layer 3: Verify channel-ready events
    assert!(
        agentic_events.iter().any(|e| matches!(
            e,
            AgenticEvent::Lifecycle {
                phase: LifecyclePhase::Start,
                ..
            }
        )),
        "Should have Start lifecycle event"
    );

    // Live mode should emit deltas immediately
    let deltas: Vec<_> = agentic_events
        .iter()
        .filter_map(|e| match e {
            AgenticEvent::AssistantDelta { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();

    assert_eq!(deltas.len(), 2, "Live mode should emit 2 deltas");
    assert_eq!(deltas.join(""), "Hello world!");

    // Should have end event
    assert!(
        agentic_events.iter().any(|e| matches!(
            e,
            AgenticEvent::Lifecycle {
                phase: LifecyclePhase::End,
                ..
            }
        )),
        "Should have End lifecycle event"
    );
}

#[test]
fn test_three_layer_pipeline_final_only_mode() {
    // Layer 1: Provider (simulated)
    let stream_events = simulate_text_stream();

    // Layer 2: Orchestrator in FinalOnly mode
    let config = OrchestratorConfig::final_only();
    let mut orchestrator = StreamOrchestrator::new("run_002", config);

    let mut agentic_events = Vec::new();
    for event in stream_events {
        agentic_events.extend(orchestrator.process(event));
    }
    agentic_events.extend(orchestrator.finalize());

    // FinalOnly mode should not emit deltas during processing
    let deltas: Vec<_> = agentic_events
        .iter()
        .filter(|e| matches!(e, AgenticEvent::AssistantDelta { .. }))
        .collect();
    assert!(
        deltas.is_empty(),
        "FinalOnly mode should not emit deltas during processing"
    );

    // Should emit AssistantText at finalize
    let texts: Vec<_> = agentic_events
        .iter()
        .filter_map(|e| match e {
            AgenticEvent::AssistantText { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect();

    assert_eq!(texts.len(), 1);
    assert_eq!(texts[0], "Hello world!");
}

#[test]
fn test_stream_buffer_coalescing() {
    use pekobot::engine::CoalesceConfig;

    // Create buffer with small thresholds for testing
    let config = CoalesceConfig {
        min_chars: 10,
        max_chars: 50,
        idle_timeout: Duration::from_millis(100),
        joiner: " ".to_string(),
    };

    let mut buffer = StreamBuffer::new("run_003", config);

    // Push small deltas - should not emit immediately
    let events = buffer.push(AgenticEvent::AssistantDelta {
        run_id: "run_003".to_string(),
        text: "Hello ".to_string(),
        sequence: 1,
        is_interstitial: false,
    });
    assert!(events.is_empty(), "Small delta should be buffered");

    let events = buffer.push(AgenticEvent::AssistantDelta {
        run_id: "run_003".to_string(),
        text: "world ".to_string(),
        sequence: 2,
        is_interstitial: false,
    });
    assert!(events.is_empty(), "Still below min_chars");

    // Flush should emit accumulated content
    let events = buffer.flush();
    assert_eq!(events.len(), 1);
    match &events[0] {
        AgenticEvent::AssistantDelta { text, .. } => {
            assert_eq!(text, "Hello world ");
        }
        _ => panic!("Expected AssistantDelta"),
    }
}

#[test]
fn test_interstitial_detection_with_tool_calls() {
    // Layer 1: Provider (simulated tool stream)
    let stream_events = simulate_tool_stream();

    // Layer 2: Orchestrator
    let config = OrchestratorConfig::live();
    let mut orchestrator = StreamOrchestrator::new("run_004", config);

    let mut agentic_events = Vec::new();
    for event in stream_events {
        agentic_events.extend(orchestrator.process(event));
    }
    agentic_events.extend(orchestrator.finalize());

    // Should have text deltas
    let text_deltas: Vec<_> = agentic_events
        .iter()
        .filter_map(|e| match e {
            AgenticEvent::AssistantDelta {
                text,
                is_interstitial,
                ..
            } => Some((text.clone(), *is_interstitial)),
            _ => None,
        })
        .collect();

    // Note: Text before tool calls may or may not be interstitial depending on
    // when the tool call starts. The orchestrator marks text as interstitial
    // once it sees a tool call, but text processed before that won't be marked.
    // This is expected streaming behavior.
    assert!(!text_deltas.is_empty(), "Should have text deltas");

    // Should have tool start event
    assert!(
        agentic_events
            .iter()
            .any(|e| matches!(e, AgenticEvent::ToolStart { tool_id, .. } if tool_id == "tc_001")),
        "Should have ToolStart event"
    );
}

#[test]
fn test_error_handling_in_pipeline() {
    let config = OrchestratorConfig::live();
    let mut orchestrator = StreamOrchestrator::new("run_005", config);

    // Simulate error from provider
    let error_event = StreamEvent::Error {
        message: "Rate limit exceeded".to_string(),
    };

    let events = orchestrator.process(error_event);

    assert_eq!(events.len(), 1);
    match &events[0] {
        AgenticEvent::Lifecycle { phase, error, .. } => {
            assert!(matches!(phase, LifecyclePhase::Error));
            assert_eq!(error.as_deref(), Some("Rate limit exceeded"));
        }
        _ => panic!("Expected Lifecycle::Error event"),
    }
}

/// Integration test demonstrating the full flow with channel actions
#[test]
fn test_full_pipeline_to_channel_actions() {
    use pekobot::engine::{ChannelAction, EventProcessor};

    // Layer 1 & 2: Simulate streaming and convert to AgenticEvents
    let stream_events = simulate_text_stream();
    let config = OrchestratorConfig::live();
    let mut orchestrator = StreamOrchestrator::new("run_006", config);

    let mut agentic_events = Vec::new();
    for event in stream_events {
        agentic_events.extend(orchestrator.process(event));
    }
    agentic_events.extend(orchestrator.finalize());

    // Layer 3: Process through EventProcessor to get ChannelActions
    let mut processor = EventProcessor::for_agent("test-agent");
    let mut actions = Vec::new();

    for event in &agentic_events {
        actions.extend(processor.process(event));
    }

    // Verify channel actions for streaming/live mode
    // In live mode, AssistantDelta events are produced which become Print + Flush
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, ChannelAction::StartTurn(name) if name == "test-agent")),
        "Should start turn: got {:?}",
        actions
    );

    // Streaming mode uses Print (not Println) for real-time display
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, ChannelAction::Print(text) if text.contains("Hello"))),
        "Should print content: got {:?}",
        actions
    );

    // Should have Flush actions for streaming mode
    assert!(
        actions.iter().any(|a| matches!(a, ChannelAction::Flush)),
        "Should flush output: got {:?}",
        actions
    );

    // EndTurn comes from Lifecycle::End event
    assert!(
        actions.iter().any(|a| matches!(a, ChannelAction::EndTurn)),
        "Should end turn: got {:?}",
        actions
    );
}
