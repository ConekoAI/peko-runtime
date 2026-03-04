# Modernization Migration Plan

## Goal
Transform Pekobot to use native JSON tool calling (like OpenClaw) with streaming thinking support.

## Phase 1: Core Type Updates

### 1.1 Update ContentBlock Types
```rust
pub enum ContentBlock {
    Text { text: String },
    Thinking { thinking: String, signature: Option<String> },
    ToolCall { id: String, name: String, arguments: Value },
    ToolResult { tool_call_id: String, name: String, content: Vec<ContentBlock>, is_error: bool },
    Image { source: ImageSource, mime_type: String },
}
```

### 1.2 Update System Prompt
Remove `TOOL_CALL:` format instruction, replace with native tool_calls in content.

### 1.3 Update Provider Interface
Support content blocks in messages instead of just text.

## Phase 2: Agentic Loop v4

### 2.1 Parse Content Blocks
Parse assistant response as JSON with content blocks array.

### 2.2 Extract Tool Calls
Extract tool_calls from content blocks (support multiple per message).

### 2.3 Stream Thinking
When assistant sends `type: "thinking"`, stream it with 🤔 prefix.

### 2.4 Execute Tools
Execute all tool calls in parallel, collect results.

### 2.5 Send Tool Results
Send tool results back as tool_result content blocks.

## Phase 3: Session Format

### 3.1 JSONL Format
Match OpenClaw's session format:
```jsonl
{"type":"session","version":3,"id":"...","timestamp":"..."}
{"type":"message","id":"...","message":{"role":"user","content":[{"type":"text","text":"..."}]}}
{"type":"message","id":"...","message":{"role":"assistant","content":[{"type":"thinking","thinking":"..."},{type":"toolCall","id":"...","name":"...","arguments":{}}]}}
{"type":"toolResult","toolCallId":"...","toolName":"...","content":[{"type":"text","text":"..."}]}
```

### 3.2 Transcript Storage
Update TranscriptEntry to support content blocks.

## Phase 4: Agent Recreation

### 4.1 Delete test-agent
Remove existing test-agent config and workspace.

### 4.2 Create New test-agent
Create fresh test-agent with new prompt format.

### 4.3 Test
Verify tool calling works with new format.

## Implementation Order

1. ✅ Update ContentBlock types
2. ✅ Update system prompt (remove TOOL_CALL)
3. ✅ Update v3 loop to parse content blocks
4. ✅ Add thinking streaming
5. ⏳ Update session format (JSONL)
6. ⏳ Recreate test-agent
7. ⏳ Test and verify
