# Unified Agent API Documentation

## Overview

Pekobot now uses a unified callback-based Agent API. This design separates agent logic from presentation concerns, making the code cleaner and more testable.

## The Unified API

### Synchronous-style with Callback

```rust
// Execute with event callback
let result = agent.execute("Search for news", |event| {
    match event {
        AgenticEvent::Assistant { text, is_final, .. } => {
            if is_final {
                println!("🐱 Agent: {}", text);
            } else {
                print!("{}", text);
            }
        }
        AgenticEvent::ToolStart { name, .. } => {
            println!("\n🔧 Using tool: {}", name);
        }
        AgenticEvent::ToolEnd { success, .. } => {
            println!(" {}\n", if success { "✅" } else { "❌" });
        }
        AgenticEvent::Lifecycle { phase, .. } => {
            match phase {
                LifecyclePhase::Start => println!("Starting..."),
                LifecyclePhase::End => println!("Done!"),
                _ => {}
            }
        }
        _ => {}
    }
}).await?;

println!("Success: {}", result.success);
println!("Answer: {}", result.final_answer);
```

### Async Streaming with Channel

```rust
// Get a channel receiver for events
let mut event_rx = agent.execute_streaming("Search for news").await?;

while let Some(event) = event_rx.recv().await {
    // Handle events as they arrive
    println!("{:?}", event);
}
```

## Event Types

```rust
pub enum AgenticEvent {
    /// Lifecycle events (start, running, end, error, aborted)
    Lifecycle { run_id, phase, error },

    /// Assistant text response (streaming deltas or final)
    Assistant { run_id, text, is_delta, is_final },

    /// Tool execution started
    ToolStart { run_id, tool_id, name, arguments },

    /// Tool execution completed
    ToolEnd { run_id, tool_id, success, result, error },

    /// Usage statistics
    Usage { run_id, input_tokens, output_tokens, total_tokens },
}
```

## Migration Guide

### From Old API

**Before (blocking):**
```rust
let result = agent.execute_with_tools(prompt).await?;
println!("{}", result.final_answer);
```

**After (unified):**
```rust
let result = agent.execute(prompt, |_| {}).await?;
println!("{}", result.final_answer);
```

**Before (streaming V3):**
```rust
let mut rx = agent.execute_streaming_v3(prompt).await?;
while let Some(event) = rx.recv().await { ... }
```

**After (unified):**
```rust
let mut rx = agent.execute_streaming(prompt).await?;
while let Some(event) = rx.recv().await { ... }
```

## Deprecated Methods

These methods are deprecated and will be removed in v0.3.0:

- `execute_native()` → Use `execute()`
- `execute_native_streaming()` → Use `execute_streaming()`
- `execute_with_tools()` → Use `execute()`
- `execute_streaming_v3()` → Use `execute_streaming()`
- `execute_streaming_v1_v2()` → Use `execute_streaming()`

## Testing

The unified API makes testing easier:

```rust
#[tokio::test]
async fn test_agent_execution() {
    let agent = create_test_agent().await;
    let mut events = Vec::new();

    let result = agent
        .execute("Test prompt", |e| events.push(e))
        .await
        .unwrap();

    assert!(result.success);
    assert!(!result.final_answer.is_empty());
    assert!(!events.is_empty());
}
```

## Benefits

1. **Single code path** - No duplication between streaming and non-streaming
2. **Caller controls presentation** - CLI prints, tests collect, headless logs
3. **No Send/Sync issues** - Callback runs on agent thread
4. **Easier testing** - Pass `|e| events.push(e)` to capture all events
5. **Native tool calling** - Works with both native and text-based providers
