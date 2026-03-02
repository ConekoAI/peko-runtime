# Streaming Architecture Design

## Overview

Implement OpenClaw-style streaming for Pekobot agent interactions.

## Goals

1. **Event-driven streaming** - Emit events for assistant text, tool execution, lifecycle
2. **Block-based streaming** - Coarse-grained chunks (not token-by-token)
3. **Tool visibility** - Show tool execution progress in real-time
4. **Provider SSE support** - Server-Sent Events for streaming providers

## Architecture

### Core Components

```
┌─────────────────────────────────────────────────────────────┐
│                        CLI Channel                          │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐  │
│  │ Event Router │  │ Block Chunker│  │ Status Display   │  │
│  └──────────────┘  └──────────────┘  └──────────────────┘  │
└──────────────────────────┬──────────────────────────────────┘
                           │ AgenticEvent stream
┌──────────────────────────┴──────────────────────────────────┐
│                     Agentic Loop                            │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐  │
│  │ Event Source │  │ Tool Exec    │  │ Response Parser  │  │
│  └──────────────┘  └──────────────┘  └──────────────────┘  │
└──────────────────────────┬──────────────────────────────────┘
                           │ SSE / Stream
┌──────────────────────────┴──────────────────────────────────┐
│                     Provider Layer                          │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────────┐  │
│  │ OpenAI SSE   │  │ Kimi SSE     │  │ Ollama Stream    │  │
│  └──────────────┘  └──────────────┘  └──────────────────┘  │
└─────────────────────────────────────────────────────────────┘
```

### Event Types

```rust
pub enum AgenticEvent {
    // Lifecycle
    Lifecycle { phase: LifecyclePhase, run_id: String },
    
    // Assistant text (streaming blocks)
    Assistant { text: String, is_delta: bool, is_final: bool },
    
    // Tool execution
    ToolStart { id: String, name: String, params: Value },
    ToolUpdate { id: String, output: String },
    ToolEnd { id: String, result: Value, success: bool },
    
    // Status
    Status { message: String, typing: bool },
}
```

## Implementation Phases

### Phase 1: Event System Foundation
- [ ] Define `AgenticEvent` enum
- [ ] Create `EventSource` trait
- [ ] Update `AgenticLoop` to emit events
- [ ] Update CLI channel to receive events

### Phase 2: Provider Streaming
- [ ] Add `Provider::complete_stream()` method
- [ ] Implement SSE parsing for OpenAI
- [ ] Implement SSE parsing for Kimi
- [ ] Implement streaming for Ollama

### Phase 3: Block Chunking
- [ ] Implement `BlockChunker`
- [ ] Configurable min/max chars
- [ ] Break preference (paragraph/sentence/whitespace)

### Phase 4: Tool Visibility
- [ ] Tool start/end events
- [ ] Tool progress updates (for long-running tools)
- [ ] Tool cards in CLI

### Phase 5: Integration
- [ ] Update `execute_with_tools()` to use streaming
- [ ] CLI status line updates
- [ ] Backward compatibility (blocking mode)

## Configuration

```toml
[streaming]
enabled = true
mode = "block"  # "block" | "message"
min_chars = 100
max_chars = 2000
break_preference = "sentence"  # "paragraph" | "sentence" | "whitespace"
coalesce = { min_chars = 150, idle_ms = 500 }

[streaming.tool_visibility]
show_start = true
show_progress = true
show_result = true
```

## Provider-Specific Notes

### OpenAI
- Endpoint: `POST /v1/chat/completions`
- Header: `Accept: text/event-stream`
- Body: `{ "stream": true }`
- Events: `data: {...}` lines

### Kimi (Moonshot)
- Endpoint: `POST /v1/chat/completions`  
- Header: `Accept: text/event-stream`
- Body: `{ "stream": true }`
- Events: Similar to OpenAI

### Ollama
- Endpoint: `POST /api/generate`
- Body: `{ "stream": true }`
- Events: NDJSON (newline-delimited JSON)

## Backward Compatibility

- Blocking mode remains default
- Streaming opt-in via config
- `complete()` method remains (calls `complete_stream().collect()`)

## Testing

- Unit tests for SSE parsing
- Integration tests for event flow
- Mock provider for testing events

## References

- OpenClaw streaming docs: `/docs/concepts/streaming.md`
- OpenClaw agent loop: `/docs/concepts/agent-loop.md`
- OpenAI streaming: https://platform.openai.com/docs/api-reference/chat/create#chat-create-stream
