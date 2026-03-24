# TDD-003: True Streaming Architecture

**Status:** 📝 **Draft / Planning**

**Author:** @kimi-code

**Date:** 2026-03-24

**Target Implementation:** 2026-03-31

---

## 1. Executive Summary

This document designs a true streaming architecture for Pekobot that enables real-time token-by-token delivery from LLM providers to users. The design enforces strict **SRP** (Single Responsibility Principle) and **DRY** (Don't Repeat Yourself) principles while maintaining interface-agnostic event delivery (works for CLI, HTTP API, WebSocket, Discord, etc.).

**Key Innovation:** A three-layer pipeline where each layer has a single, well-defined responsibility:
1. **Provider Layer:** Parses raw SSE into structured `StreamEvent`s
2. **Orchestration Layer:** Transforms `StreamEvent`s into presentation-ready `AgenticEvent`s
3. **Channel Layer:** Renders `AgenticEvent`s to platform-specific output

---

## 2. Goals & Non-Goals

### Goals

- [ ] True streaming: Token-by-token delivery from provider to user
- [ ] Support both streaming and block modes (configurable per-channel)
- [ ] Interface-agnostic: Same event flow for CLI, HTTP, WebSocket, Discord
- [ ] Chunking/coalescing for optimal UX (sentence/paragraph boundaries)
- [ ] Tool call streaming (incremental JSON parsing)
- [ ] Thinking/reasoning streaming (for Claude, o1, etc.)
- [ ] Backpressure handling (slow consumers don't block)
- [ ] Graceful fallback to block mode if streaming unsupported

### Non-Goals

- Voice/audio streaming (out of scope)
- Video streaming (out of scope)
- Bidirectional streaming (user→agent voice, future work)
- Multi-modal streaming (images, files)
- Stream resumption after disconnection

---

## 3. Research: OpenClaw Streaming Architecture

### 3.1 OpenClaw Design Patterns (Analyzed)

OpenClaw implements a sophisticated **block streaming** system with these key components:

#### Pattern 1: Draft Stream Loop (`draft-stream-loop.ts`)
```typescript
// Throttles updates to prevent flooding the channel
const loop = createDraftStreamLoop({
  throttleMs: 350,           // Minimum time between updates
  sendOrEditStreamMessage: async (text) => { /* send to channel */ }
});

// Usage: loop.update(newText) → throttled delivery
```

**Key Insight:** Throttling prevents API rate limits while maintaining responsive feel.

#### Pattern 2: Block Streaming Config (`block-streaming.ts`)
```typescript
type BlockStreamingConfig = {
  chunking: {
    minChars: 800,           // Don't send until we have 800 chars
    maxChars: 1200,          // Force send at 1200 chars
    breakPreference: "paragraph" | "newline" | "sentence"
  },
  coalescing: {
    minChars: 800,
    maxChars: 1200,
    idleMs: 1000,            // Flush after 1s of no new content
    joiner: "\n\n"           // Join chunks with paragraph break
  }
};
```

**Key Insight:** Coarse-grained blocks (not token-by-token) for better UX and fewer API calls.

#### Pattern 3: Block Reply Pipeline (`block-reply-pipeline.ts`)
```typescript
// Pipeline with deduplication and coalescing
const pipeline = createBlockReplyPipeline({
  onBlockReply: async (payload) => { /* send */ },
  coalescing: { /* config */ },
  buffer: { /* media buffering */ }
});

// Usage: pipeline.enqueue(payload) → coalesced delivery
```

**Key Insight:** Deduplication prevents duplicate messages when final payload matches streamed content.

#### Pattern 4: ACP Stream Settings (`acp-stream-settings.ts`)
```typescript
type AcpDeliveryMode = "live" | "final_only";
type AcpHiddenBoundarySeparator = "none" | "space" | "newline" | "paragraph";

// Live mode: immediate delivery
// Final_only: buffer until complete
```

**Key Insight:** Different channels need different delivery modes (Discord vs CLI).

### 3.2 OpenClaw Lessons Applied to Pekobot

| OpenClaw Pattern | Pekobot Equivalent | Notes |
|------------------|-------------------|-------|
| `DraftStreamLoop` | `StreamOrchestrator` with throttling | Generalized for all channels |
| `BlockStreamingConfig` | `StreamingConfig` + `BlockChunker` | Reuse existing `BlockChunker` |
| `BlockReplyPipeline` | `EventProcessor` + channel adapters | Interface-agnostic design |
| `AcpDeliveryMode` | `DeliveryMode` enum per-channel | CLI=live, Discord=block |

---

## 4. Proposed Architecture

### 4.1 Three-Layer Pipeline

```
┌─────────────────────────────────────────────────────────────────────────────┐
│  LAYER 1: PROVIDER (Parse raw SSE → StreamEvent)                            │
│  Responsibility: Provider-specific JSON parsing, generic SSE handling        │
│                                                                              │
│  Input: HTTP SSE stream (bytes)                                              │
│  Output: Stream<Item = StreamEvent>                                          │
│                                                                              │
│  Components:                                                                 │
│  - SseParser (exists): bytes → SseEvent                                      │
│  - ApiAdapter::parse_sse_event(): SseEvent → StreamEvent (NEW)               │
└─────────────────────────────────────────────────────────────────────────────┘
                                       │
                                       ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│  LAYER 2: ORCHESTRATOR (Transform StreamEvent → AgenticEvent)               │
│  Responsibility: Chunking, coalescing, tool accumulation, state machine      │
│                                                                              │
│  Input: Stream<Item = StreamEvent>                                           │
│  Output: Stream<Item = AgenticEvent>                                         │
│                                                                              │
│  Components:                                                                 │
│  - StreamOrchestrator (NEW): State machine, event transformation             │
│  - BlockChunker (exists): Text → Chunks                                      │
│  - ToolCallStreamParser (exists): Partial JSON → ToolCall                    │
│  - StreamBuffer (NEW): Throttling, coalescing, backpressure                  │
└─────────────────────────────────────────────────────────────────────────────┘
                                       │
                                       ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│  LAYER 3: CHANNEL (Render AgenticEvent → Platform Output)                   │
│  Responsibility: Platform-specific rendering, user experience                │
│                                                                              │
│  Input: Stream<Item = AgenticEvent>                                          │
│  Output: CLI print / HTTP SSE / WebSocket frame / Discord post               │
│                                                                              │
│  Components:                                                                 │
│  - EventProcessor (exists): AgenticEvent → ChannelAction                     │
│  - Channel implementations: Execute ChannelAction                            │
│  - Delivery adapters: Platform-specific flushing                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 4.2 Component Details

#### 4.2.1 StreamOrchestrator (NEW)

```rust
/// Transforms low-level StreamEvents into presentation-ready AgenticEvents
/// 
/// Responsibilities:
/// - Accumulate text deltas and emit chunked blocks
/// - Parse incremental tool calls
/// - Manage interstitial vs final state
/// - Apply throttling/coalescing
pub struct StreamOrchestrator {
    config: OrchestratorConfig,
    state: OrchestratorState,
    chunker: BlockChunker,
    tool_parser: ToolCallStreamParser,
    buffer: StreamBuffer,
    sequence: usize,
}

#[derive(Debug, Clone)]
pub struct OrchestratorConfig {
    /// Delivery mode: live (immediate) or block (coalesced)
    pub delivery_mode: DeliveryMode,
    /// Chunking configuration
    pub chunking: ChunkerConfig,
    /// Coalescing configuration (for block mode)
    pub coalescing: CoalesceConfig,
    /// Throttle between emits (for live mode)
    pub throttle_ms: u64,
}

pub enum DeliveryMode {
    /// Emit every delta immediately (for CLI, TUI)
    Live,
    /// Coalesce into blocks (for Discord, HTTP)
    Block,
    /// Buffer until complete (for non-streaming channels)
    FinalOnly,
}

impl StreamOrchestrator {
    /// Process a StreamEvent and return AgenticEvents to emit
    pub fn process(&mut self, event: StreamEvent) -> Vec<AgenticEvent> {
        match event {
            StreamEvent::TextDelta { delta, .. } => {
                self.handle_text_delta(delta)
            }
            StreamEvent::ToolCallDelta { .. } => {
                self.handle_tool_delta(event)
            }
            StreamEvent::ToolCallEnd { tool_call } => {
                self.handle_tool_complete(tool_call)
            }
            StreamEvent::ThinkingDelta { delta, .. } => {
                self.handle_thinking_delta(delta)
            }
            StreamEvent::Done { stop_reason } => {
                self.handle_done(stop_reason)
            }
            _ => vec![],
        }
    }

    fn handle_text_delta(&mut self, delta: String) -> Vec<AgenticEvent> {
        match self.config.delivery_mode {
            DeliveryMode::Live => {
                // Emit immediately (with optional throttling)
                vec![AgenticEvent::AssistantDelta { 
                    text: delta, 
                    sequence: self.next_seq(),
                    .. 
                }]
            }
            DeliveryMode::Block => {
                // Accumulate and emit chunks
                let mut events = vec![];
                for chunk in self.chunker.feed(&delta) {
                    events.push(AgenticEvent::AssistantDelta {
                        text: chunk,
                        sequence: self.next_seq(),
                        ..
                    });
                }
                events
            }
            DeliveryMode::FinalOnly => {
                // Buffer everything
                self.buffer.push(delta);
                vec![]
            }
        }
    }
}
```

#### 4.2.2 StreamBuffer (NEW)

```rust
/// Buffers and coalesces events with idle timeout
/// 
/// Similar to OpenClaw's DraftStreamLoop but generalized
/// for AgenticEvents instead of text strings.
pub struct StreamBuffer {
    pending: Vec<AgenticEvent>,
    last_emit: Instant,
    idle_timeout: Duration,
    min_chars: usize,
    max_chars: usize,
    joiner: String,
}

impl StreamBuffer {
    pub fn push(&mut self, event: AgenticEvent) {
        self.pending.push(event);
    }

    /// Returns events that are ready to emit (idle timeout or max size)
    pub fn try_flush(&mut self) -> Vec<AgenticEvent> {
        if self.should_flush() {
            self.flush()
        } else {
            vec![]
        }
    }

    fn should_flush(&self) -> bool {
        if self.pending.is_empty() {
            return false;
        }
        let total_chars: usize = self.pending.iter()
            .map(|e| match e {
                AgenticEvent::AssistantDelta { text, .. } => text.len(),
                _ => 0,
            })
            .sum();
        
        total_chars >= self.max_chars 
            || (total_chars >= self.min_chars 
                && self.last_emit.elapsed() >= self.idle_timeout)
    }
}
```

#### 4.2.3 Enhanced AgenticEvent

```rust
pub enum AgenticEvent {
    // ... existing events ...

    /// Text streaming delta (for true streaming mode)
    /// 
    /// Unlike AssistantText (complete block), this is a raw delta
    /// that channels can render immediately or buffer.
    AssistantDelta {
        run_id: RunId,
        text: String,
        sequence: usize,
        is_interstitial: bool,
    },

    /// Tool call streaming (partial for UI preview)
    /// 
    /// Emitted while tool call JSON is being accumulated.
    /// Channels can show "Running {name}..." with progress.
    ToolCallStreaming {
        run_id: RunId,
        tool_call_id: ToolCallId,
        name: Option<String>,       // Partial name
        arguments_preview: String,  // Partial JSON
        progress: Option<u8>,       // 0-100 (if determinable)
    },

    /// Flush request (internal)
    /// 
    /// Signals that buffered content should be emitted immediately.
    /// Used when tool calls start (end interstitial text).
    Flush,
}
```

#### 4.2.4 Streaming Agentic Loop

```rust
impl AgenticLoopV4 {
    /// Run with true streaming support
    pub async fn run_streaming(
        &self,
        prompt: &str,
        session: Arc<RwLock<UnifiedSession>>,
        on_event: impl Fn(AgenticEvent),
        config: StreamingConfig,
    ) -> Result<AgenticResult> {
        // 1. Create orchestrator with channel-specific config
        let mut orchestrator = StreamOrchestrator::new(config);
        
        // 2. Get streaming provider
        let stream = self.provider
            .stream_with_tools(&messages, &tools, &options)
            .await?;

        // 3. Process each StreamEvent
        pin_mut!(stream);
        while let Some(result) = stream.next().await {
            match result {
                Ok(event) => {
                    let events = orchestrator.process(event);
                    for event in events {
                        on_event(event);
                    }
                }
                Err(e) => {
                    on_event(AgenticEvent::Lifecycle {
                        phase: LifecyclePhase::Error,
                        error: Some(e.to_string()),
                        ..
                    });
                    return Err(e);
                }
            }
        }

        // 4. Final flush
        for event in orchestrator.finalize() {
            on_event(event);
        }

        // 5. Handle tool execution and recursion
        // (similar to existing run_loop but with streaming)
    }
}
```

---

## 5. Implementation Plan

### Phase 1: Provider Layer (Week 1)

| Task | File(s) | Notes |
|------|---------|-------|
| Implement `parse_sse_event()` in adapters | `providers/adapters/*.rs` | Provider-specific JSON parsing |
| Enhance `StreamEvent` coverage | `providers/traits.rs` | All event types |
| Test streaming with real providers | `tests/streaming.rs` | Integration tests |

### Phase 2: Orchestration Layer (Week 2)

| Task | File(s) | Notes |
|------|---------|-------|
| Create `StreamOrchestrator` | `engine/stream_orchestrator.rs` | Core transformation logic |
| Create `StreamBuffer` | `engine/stream_buffer.rs` | Throttling/coalescing |
| Enhance `AgenticEvent` | `engine/events.rs` | Add delta events |
| Unit tests | `engine/stream_orchestrator_test.rs` | Mock provider streams |

### Phase 3: Engine Integration (Week 3)

| Task | File(s) | Notes |
|------|---------|-------|
| Implement `run_streaming()` | `engine/loop_v4.rs` | New loop method |
| Tool execution mid-stream | `engine/loop_v4.rs` | Pause/resume stream |
| History management | `engine/loop_v4.rs` | Correct message ordering |
| Fallback to block mode | `engine/runner.rs` | If streaming unsupported |

### Phase 4: Channel Integration (Week 4)

| Task | File(s) | Notes |
|------|---------|-------|
| Update `EventProcessor` | `engine/event_processor.rs` | Handle delta events |
| CLI streaming mode | `channels/cli.rs` | Immediate render |
| HTTP SSE endpoint | `api/routes/chat.rs` | Server-sent events |
| Discord block mode | `channels/discord.rs` | Coalesced delivery |

---

## 6. Architecture Compliance

| Principle | Implementation |
|-----------|----------------|
| **SRP** | Provider parses, Orchestrator transforms, Channel renders |
| **DRY** | `BlockChunker`, `ToolCallStreamParser` reused; single `EventProcessor` |
| **Interface-agnostic** | `AgenticEvent` is universal interface |
| **Open/Closed** | New channels extend `EventProcessor`; new providers implement `parse_sse_event()` |
| **Testability** | Each layer unit-testable with mocks |

---

## 7. Configuration Schema

```toml
# pekobot.toml
[streaming]
# Global default
default_mode = "block"  # "live" | "block" | "final_only"

[streaming.chunking]
min_chars = 100
max_chars = 2000
break_preference = "sentence"  # "paragraph" | "sentence" | "whitespace" | "hard"

[streaming.coalescing]
min_chars = 1500
max_chars = 3000
idle_ms = 500

[channels.cli.streaming]
mode = "live"
throttle_ms = 50  # CLI feels responsive with 50ms

[channels.discord.streaming]
mode = "block"
coalescing.idle_ms = 1000  # Discord rate limits

[channels.api.streaming]
mode = "live"
chunking.break_preference = "paragraph"
```

---

## 8. Migration Strategy

1. **Backward Compatibility:** Existing `chat_with_tools()` continues to work
2. **Feature Flag:** `streaming.enabled = true` to opt-in
3. **Graceful Degradation:** If provider doesn't support streaming, fall back to block mode
4. **Channel Defaults:** CLI defaults to live, Discord to block, API configurable

---

## 9. Success Metrics

- [ ] CLI: First token visible within 500ms of LLM start
- [ ] Discord: No rate limit errors with block streaming
- [ ] HTTP API: SSE events received every 100ms minimum
- [ ] Memory: No unbounded growth during long streams
- [ ] Tool calls: Streaming preview shown before execution

---

## 10. References

- OpenClaw `draft-stream-loop.ts`: Throttling pattern
- OpenClaw `block-streaming.ts`: Chunking/coalescing config
- OpenClaw `block-reply-pipeline.ts`: Deduplication and delivery
- Pekobot `BlockChunker`: Existing chunking implementation
- Pekobot `ToolCallStreamParser`: Existing incremental JSON parser
