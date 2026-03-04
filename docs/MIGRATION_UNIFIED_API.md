# Unified Agent API Migration Plan

## Overview

Migrate Pekobot's agent execution API from separate `run()`/`run_streaming()` methods to a unified callback-based approach (like `pi_agent_rust`). This achieves clean separation between agent logic and presentation concerns.

**Branch:** `feature/unified-agent-api`  
**Target Merge:** After review and testing  
**Risk Level:** Medium (touches core execution path)

---

## Current State

```rust
// Non-streaming - blocks, returns final result
let result = agent.execute_with_tools(prompt).await?;

// Streaming - returns channel receiver
let mut rx = agent.execute_streaming(prompt).await?;
while let Some(event) = rx.recv().await { ... }

// V3 internal - two methods with duplication
impl AgenticLoopV3 {
    pub async fn run(&self, prompt: &str) -> Result<AgenticResult>;
    pub async fn run_streaming(&self, prompt: &str, event_tx: Sender<AgenticEvent>) -> Result<AgenticResult>;
}
```

**Problems:**
- Two entry points = two code paths to maintain
- `run()` internally spawns `run_streaming()` and collects - wasteful
- Channels require `spawn_local()` due to non-Send types
- External code (CLI) handles channels explicitly

---

## Target State

```rust
// Single unified API - callback for events, return for result
let result = agent.execute(prompt, |event| {
    match event {
        AgenticEvent::Assistant { text, is_final, .. } => {
            if is_final { println!("🐱 {}", text); }
            else { print!("{}", text); }
        }
        AgenticEvent::ToolStart { name, .. } => println!("🔧 {}...", name),
        AgenticEvent::ToolEnd { result, .. } => println!(" ✓"),
        _ => {}
    }
}).await?;

// Internal - single method
impl AgenticLoopV3 {
    pub async fn run(
        &self, 
        prompt: &str, 
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static
    ) -> Result<AgenticResult>;
}
```

**Benefits:**
- One code path, zero duplication
- Caller controls presentation (CLI = print, test = collect, headless = log)
- No Send/Sync issues - callback runs on agent thread
- Easier testing - pass `|e| events.push(e)`

---

## Migration Steps

### Phase 1: Core Loop Changes
**Files:** `src/engine/loop_v3.rs`

1. **Remove** `run_streaming()` method
2. **Modify** `run()` signature:
   ```rust
   pub async fn run(
       &self,
       prompt: &str,
       on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
   ) -> Result<AgenticResult>
   ```
3. **Replace** all `event_tx.send().await` with `on_event(event)`
4. **Update** tests to use callback pattern

### Phase 2: Agent Facade Changes
**Files:** `src/agent/mod.rs`

1. **Replace** `execute_with_tools()` and `execute_streaming()` with single `execute()`:
   ```rust
   pub async fn execute(
       &self,
       prompt: &str,
       on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
   ) -> Result<AgenticResult>
   ```
2. **Remove** channel-based `execute_streaming()` entirely
3. **Update** `execute_streaming_v3()` to wrap callback (temporary compat)

### Phase 3: Channel Adapter Layer
**Files:** `src/channels/cli.rs`, `src/runner.rs`

1. **Create** `EventChannel` adapter for code that needs channels:
   ```rust
   pub fn channel_adapter<F>(on_event: F) -> (Sender<AgenticEvent>, impl Future)
   where F: Fn(AgenticEvent) + ...
   ```
2. **Update** CLI interactive mode to use callback directly
3. **Update** `AgentRunner` to use unified API

### Phase 4: Test Updates
**Files:** `tests/`, `examples/`

1. **Update** `test_agentic_loop.sh` if it tests specific output format
2. **Update** examples to use new API
3. **Add** unit test for event sequence verification

### Phase 5: Documentation
**Files:** `docs/STREAMING.md`, `README.md`

1. **Update** streaming architecture docs
2. **Update** API examples in README
3. **Add** migration guide for custom channels

---

## Implementation Details

### Event Callback Design

```rust
/// Event callback trait for flexibility
pub trait EventHandler: Send + Sync {
    fn on_event(&self, event: AgenticEvent);
}

impl<F> EventHandler for F 
where F: Fn(AgenticEvent) + Send + Sync + 'static 
{
    fn on_event(&self, event: AgenticEvent) {
        (self)(event)
    }
}

// In AgenticLoopV3::run:
for block in &content_blocks {
    match block {
        ContentBlock::Thinking { text, .. } => {
            on_event(AgenticEvent::Assistant { 
                text: text.clone(), 
                is_delta: true, 
                is_final: false,
                run_id: run_id.clone(),
            });
        }
        // ...
    }
}
```

### Backpressure Handling

For slow consumers (like network channels), provide a bounded adapter:

```rust
/// Wraps a channel sender to use as callback
pub fn channel_sender(tx: Sender<AgenticEvent>) -> impl Fn(AgenticEvent) + Send + Sync {
    move |event| {
        // Try_send to avoid blocking agent loop
        match tx.try_send(event) {
            Ok(_) => {}
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                warn!("Event channel full, dropping event");
            }
            Err(_) => {} // Channel closed, ignore
        }
    }
}
```

---

## Testing Strategy

### Unit Tests
```rust
#[tokio::test]
async fn test_event_sequence() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_clone = Arc::clone(&events);
    
    let result = agent.execute("test", move |e| {
        events_clone.lock().unwrap().push(e);
    }).await.unwrap();
    
    let collected = events.lock().unwrap();
    assert!(matches!(collected[0], AgenticEvent::Lifecycle { phase: Start, .. }));
    assert!(matches!(collected.last(), AgenticEvent::Lifecycle { phase: End, .. }));
}
```

### Integration Tests
1. CLI interactive mode - verify progress bars and tool display
2. Single message mode - verify streaming output
3. Error handling - verify error events propagate

### Manual Tests
```bash
# Interactive mode
pekobot agent start test-agent --interactive

# Single message
pekobot agent run test-agent "What time is it?"

# With tools
pekobot agent run test-agent "Search for Rust async patterns"
```

---

## Rollback Plan

If issues found:

1. **Revert branch**: `git revert HEAD~N..HEAD`
2. **Restore old API**: `execute_with_tools()` and `execute_streaming()` remain available
3. **Deprecation window**: Mark old API deprecated but functional for 1 release cycle

---

## Timeline

| Phase | Est. Time | Owner |
|-------|-----------|-------|
| Phase 1: Core Loop | 2h | Pekora |
| Phase 2: Agent Facade | 1h | Pekora |
| Phase 3: Channel Adapter | 2h | Pekora |
| Phase 4: Test Updates | 1h | Pekora |
| Phase 5: Documentation | 1h | Pekora |
| Review & Testing | 2h | Miz |
| **Total** | **9h** | |

---

## Checklist

- [ ] Phase 1 complete - `loop_v3.rs` uses unified callback
- [ ] Phase 2 complete - `Agent::execute()` is single entry point
- [ ] Phase 3 complete - CLI uses callback, no channel receivers
- [ ] Phase 4 complete - All tests pass
- [ ] Phase 5 complete - Documentation updated
- [ ] `cargo test` passes
- [ ] `cargo clippy` clean
- [ ] Manual CLI test passes
- [ ] PR reviewed and approved

---

## References

- pi-mono agentic loop pattern (reference implementation)
- OpenClaw streaming architecture (`docs/STREAMING.md`)
- Current V3 loop implementation (`src/engine/loop_v3.rs`)
