# Pi-Mono Agent Migration Plan

## Overview
The pi-mono Rust port (`pi_agent_rust`) is a sophisticated ~1.7M line codebase with a full agent runtime. Rather than copying line-by-line, we'll extract the core agentic loop patterns and adapt them to Pekobot's architecture.

## Key Components to Migrate

### 1. Core Agent Loop (`agent.rs`)
**What it does:**
- Main `run_loop()` function that orchestrates the conversation
- Handles user input → LLM call → tool execution → repeat
- Supports steering messages (interrupts) and follow-up messages
- Manages max iteration limits and abort signals

**Integration approach:**
- Extract the loop structure but use Pekobot's providers
- Adapt message types to use Pekobot's `AgentMessage` abstraction
- Keep Pekobot's tool execution (already working)

### 2. Message Types (`model.rs`)
**What it does:**
- Rich message hierarchy: UserMessage, AssistantMessage, ToolResultMessage
- Content blocks: Text, Image, ToolCall, ToolResult
- Streaming events: StreamEvent with deltas

**Integration approach:**
- Pekobot already has similar types in `types/message.rs`
- Enhance with any missing variants from pi-mono
- Ensure compatibility with pi-mono's content block model

### 3. Session Management (`session.rs`)
**What it does:**
- Persists conversation history
- Handles session lifecycle (create, load, save)
- Compaction (pruning old messages)
- SQLite backend

**Integration approach:**
- Pekobot has basic SQLite memory - extend it
- Add pi-mono's session persistence patterns
- Keep Pekobot's simpler memory model as default

### 4. Provider Interface (`provider.rs`)
**What it does:**
- Abstract interface for LLM providers
- Streaming support with StreamEvent
- Tool definition format

**Integration approach:**
- Pekobot's providers are working - keep them
- Adapt pi-mono's streaming event format if beneficial
- Don't replace the entire provider stack

## Simplified Migration Strategy

### Phase 1: Core Loop (Immediate)
Extract just the essential loop logic from `agent.rs`:

```rust
// Simplified pattern from pi-mono:
loop {
    // 1. Get user message + steering messages
    // 2. Stream LLM response
    // 3. If tool calls: execute tools, add results, continue
    // 4. If final answer: return
}
```

**Key fixes for Pekobot's v2 loop:**
1. Tool results need to be properly formatted in the prompt
2. Stop after tool execution completes and LLM gives final answer
3. Don't loop infinitely on the same tool call

### Phase 2: Message Abstraction (Week 1)
Enhance Pekobot's `AgentMessage` with pi-mono patterns:
- Proper content block hierarchy
- Tool call / tool result relationship
- Message metadata

### Phase 3: Session Persistence (Week 2)
Add proper session management:
- Load/save conversations
- Session indexing
- Compaction for long conversations

## Files to Reference

From `pi_agent_rust/src/`:
- `agent.rs:574-667` - Main run functions
- `agent.rs:667-900` - Core run_loop implementation
- `model.rs` - Message types
- `session.rs` - Session management
- `provider.rs` - Provider interface

## Integration Path

Instead of copying, create a new `AgenticLoopV3` that:
1. Uses pi-mono's loop structure
2. Adapts to Pekobot's providers/tools
3. Integrates with existing event streaming
4. Keeps Pekobot's simpler deployment model

## Critical Fix for Current V2 Loop

The immediate issue: v2 loop doesn't properly signal when to stop after tool execution.

From pi-mono pattern (`agent.rs:800-850`):
- After tool execution, check if assistant message has stop_reason == StopReason::Stop
- If tool calls were made, continue the loop
- If no tool calls and stop reason is Stop, return final answer

Pekobot v2 is missing this check - it always continues.
