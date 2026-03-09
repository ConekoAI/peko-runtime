# Streaming Architecture

Pekobot supports real-time streaming for progressive output and tool visibility. This document describes the streaming architecture, configuration, and usage.

## Overview

The streaming architecture provides:

- **Progressive Output**: Text appears as it's generated, not all at once
- **Tool Visibility**: See when tools are being executed in real-time
- **Block Chunking**: Coarse-grained output (sentences/paragraphs) rather than token-by-token
- **Provider Support**: Native streaming for major providers, fallback for others

## Architecture

```
User Input
    ↓
Agent::execute_streaming()
    ↓
AgenticLoop::run_streaming()
    ↓
Provider::complete_stream()
    ↓
SSE/NDJSON Stream
    ↓
SseParser / BlockChunker
    ↓
AgenticEvent (delta/final)
    ↓
EventRouter → CLI Display
```

## Configuration

### Per-Agent Configuration

Add streaming settings to your agent config file (`~/.pekobot/agents/{name}.toml`):

```toml
[streaming]
enabled = true              # Enable streaming by default
min_chars = 100             # Minimum chars before emitting a block
max_chars = 2000            # Maximum chars per block
break_preference = "sentence"  # paragraph, sentence, whitespace, hard
show_tools = true           # Show 🔧 tool execution notifications
show_status = true          # Show ⚡ status messages
```

### Configuration Options

| Option | Default | Description |
|--------|---------|-------------|
| `enabled` | `false` | Enable streaming by default for this agent |
| `min_chars` | `100` | Minimum characters before a block is emitted |
| `max_chars` | `2000` | Maximum characters per block (forces break) |
| `break_preference` | `"sentence"` | Where to break: paragraph, sentence, whitespace, hard |
| `show_tools` | `true` | Show tool execution notifications |
| `show_status` | `true` | Show status messages ("Thinking...") |

### Break Preferences

- **paragraph**: Break at double newlines (best for long-form content)
- **sentence**: Break at sentence boundaries (period + space + uppercase)
- **whitespace**: Break at last whitespace before limit
- **hard**: Hard break at max_chars (no natural boundary)

## CLI Usage

### Command Line Flag

Override the agent config with the `--streaming` flag:

```bash
# Use agent config (streaming.enabled from config)
pekobot agent start myagent

# Force enable streaming
pekobot agent start myagent --streaming
pekobot agent start myagent --streaming=true

# Force disable streaming
pekobot agent start myagent --streaming=false
```

## Provider Support

### Native Streaming Providers

These providers have native SSE/NDJSON streaming support:

| Provider | Format | Notes |
|----------|--------|-------|
| OpenAI | SSE | `stream=true` with delta content |
| Kimi/Moonshot | SSE | OpenAI-compatible format |
| Kimi Code | SSE | Anthropic Claude Code backend |
| Anthropic | SSE | `content_block_delta` events |
| Ollama | NDJSON | Newline-delimited JSON |
| Groq | SSE | OpenAI-compatible |
| OpenAICompatible | SSE | Works with Groq, Together, Fireworks, etc. |

### Fallback Providers

These providers use the default fallback implementation (blocking → single event):

- AWS Bedrock
- Cohere
- Perplexity
- xAI
- Venice
- Fireworks (use OpenAICompatible for streaming)
- Together (use OpenAICompatible for streaming)
- OpenRouter
- Reliable

## Events

The streaming system emits these events:

### Lifecycle Events

```rust
AgenticEvent::Lifecycle {
    run_id: String,
    phase: LifecyclePhase,  // Start, Running, End, Error, Aborted
    error: Option<String>,
}
```

### Assistant Events

```rust
AgenticEvent::Assistant {
    run_id: String,
    text: String,           // Content (delta or complete)
    is_delta: bool,         // True if incremental
    is_final: bool,         // True if final chunk
}
```

### Tool Events

```rust
AgenticEvent::ToolStart {
    run_id: String,
    tool_id: String,
    name: String,
    params: Value,          // Tool arguments
}

AgenticEvent::ToolEnd {
    run_id: String,
    tool_id: String,
    result: Value,          // Tool result
    success: bool,
    duration_ms: u64,
}
```

### Status Events

```rust
AgenticEvent::Status {
    run_id: String,
    message: String,        // "Thinking...", etc.
    typing: bool,
}
```

## Implementation Details

### Block Chunking

The `BlockChunker` accumulates text and emits blocks based on configuration:

```rust
let config = ChunkerConfig {
    min_chars: 100,
    max_chars: 2000,
    break_preference: BreakPreference::Sentence,
    emit_partial: true,
};

let mut chunker = BlockChunker::with_config(config);

// Feed text as it arrives
let blocks = chunker.feed("Some text from the stream...");
for block in blocks {
    // Emit each complete block
    emit_block(block);
}

// Flush remaining at end
let final_blocks = chunker.flush();
```

### SSE Parsing

The `SseParser` handles Server-Sent Events:

```rust
let mut parser = SseParser::new();
let events = parser.feed("data: {...}\n\ndata: {...}\n\n");
```

### Provider Implementation

Providers implement `complete_stream()`:

```rust
async fn complete_stream(
    &self,
    prompt: &str,
    event_tx: Sender<AgenticEvent>,
    run_id: String,
) -> Result<()> {
    // 1. Emit Start event
    // 2. Make streaming HTTP request
    // 3. Parse SSE/NDJSON chunks
    // 4. Emit Assistant deltas
    // 5. Emit End event
}
```

## Example Session

With streaming enabled, a typical session looks like:

```
🐱 Agent 'peko' is ready (streaming mode)! Type 'exit' or 'quit' to stop.

💬 You: Search for the latest Rust news and summarize it

⚡ Thinking...
🔧 Using tool: web_search
✅ Tool 'web_search' completed

🐱 Agent: Here are the latest Rust news highlights:

1. Rust 1.76 released with improved const generics
2. New cargo features for faster builds
3. Rust Foundation announces new members

[response continues...]
```

## Adding Streaming to a Provider

To add native streaming to a provider:

1. Implement `complete_stream()` in the provider
2. Emit `Lifecycle`, `Assistant`, and error events
3. Handle SSE or NDJSON streaming format
4. Update this documentation

Example pattern (OpenAI-compatible):

```rust
async fn complete_stream(&self, prompt, event_tx, run_id) -> Result<()> {
    use crate::engine::{AgenticEvent, LifecyclePhase};
    use crate::providers::SseParser;
    use futures::StreamExt;
    
    // Emit start
    event_tx.send(AgenticEvent::Lifecycle {
        run_id: run_id.clone(),
        phase: LifecyclePhase::Start,
        error: None,
    }).await?;
    
    // Make streaming request
    let response = self.client.post(...)
        .header("Accept", "text/event-stream")
        .json(&request)
        .send().await?;
    
    // Stream chunks
    let mut stream = response.bytes_stream();
    let mut parser = SseParser::new();
    
    while let Some(chunk) = stream.next().await {
        let text = String::from_utf8_lossy(&chunk?);
        for event in parser.feed(&text) {
            if let Some(content) = parse_delta(&event) {
                event_tx.send(AgenticEvent::Assistant {
                    run_id: run_id.clone(),
                    text: content,
                    is_delta: true,
                    is_final: false,
                }).await?;
            }
        }
    }
    
    // Emit end
    event_tx.send(AgenticEvent::Lifecycle {
        run_id,
        phase: LifecyclePhase::End,
        error: None,
    }).await?;
    
    Ok(())
}
```

## Troubleshooting

### Streaming Not Working

1. Check provider supports streaming (see table above)
2. Verify `streaming.enabled = true` in agent config
3. Try `--streaming` flag to override
4. Check API key and network connectivity

### Too Many Small Chunks

Increase `min_chars` in config:

```toml
[streaming]
min_chars = 200  # Wait for 200 chars before emitting
```

### Chunks Too Large

Decrease `max_chars` or change `break_preference`:

```toml
[streaming]
max_chars = 1000
break_preference = "sentence"
```

### Tool Notifications Not Showing

Ensure `show_tools = true`:

```toml
[streaming]
show_tools = true
show_status = true
```

## See Also

- `src/engine/events.rs` - Event definitions
- `src/engine/chunker.rs` - Block chunking implementation
- `src/providers/sse.rs` - SSE parsing utilities
- `src/providers/traits.rs` - Provider trait with `complete_stream()`