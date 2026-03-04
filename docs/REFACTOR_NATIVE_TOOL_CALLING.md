# Pekobot Native Tool Calling Refactor Plan

## Study Summary: pi_agent_rust Architecture

### Key Findings

**1. Native API Tool Calling (Not Prompt-Based)**
- Tools defined as `ToolDef { name, description, parameters: JSON Schema }`
- Provider sends `tools` array in API request body (OpenAI format)
- API returns tool calls in structured `delta.tool_calls` field
- **No parsing JSON from text** - uses native API support

**2. Content Block Architecture**
```rust
enum ContentBlock {
    Text(TextContent),
    Thinking(ThinkingContent),
    Image(ImageContent),
    ToolCall(ToolCall),  // Structured, not parsed
}

struct ToolCall {
    id: String,
    name: String,
    arguments: serde_json::Value,  // Already parsed by provider
}
```

**3. Streaming Event Flow**
```rust
enum StreamEvent {
    ToolCallStart { content_index: usize },
    ToolCallDelta { content_index: usize, delta: String },
    ToolCallEnd { content_index: usize, tool_call: ToolCall },
    // ... TextStart, TextDelta, TextEnd, etc.
}
```

**4. Agent Loop Structure**
```rust
loop {
    // 1. Stream completion from provider
    let assistant = stream_assistant_response().await?;
    
    // 2. Extract tool calls from ContentBlock::ToolCall variants
    let tool_calls = extract_tool_calls(&assistant.content);
    
    // 3. If tool calls exist, execute them
    if !tool_calls.is_empty() {
        let results = execute_tool_calls(&tool_calls).await;
        // Append results as ToolResult messages
        // Loop back for next completion
        continue;
    }
    
    // 4. No tool calls - we're done
    break;
}
```

**5. Unified API Design**
- Single `run(on_event: impl Fn(AgentEvent))` method
- Event callback receives all streaming events
- No separate streaming/non-streaming paths

---

## Current Pekobot vs Target Architecture

### Current (Text-Based Tool Calling)
```
System Prompt: "You have tools: web_search, fetch..."
              ↓
Model Output:  "```json{\"content\":[{\"type\":\"tool_call\"...}]}```"
              ↓
Parse:         Regex/code fence stripping → JSON parsing
              ↓
Execute:       Tool execution
              ↓
Prompt:        "Tool result: {...}"
```

### Target (Native Tool Calling)
```
API Request:   { "tools": [...], "tool_choice": "auto" }
              ↓
Model Output:  Native tool_calls field (structured)
              ↓
Provider:      Emits StreamEvent::ToolCallStart/Delta/End
              ↓
Agent Loop:    ContentBlock::ToolCall extracted
              ↓
Execute:       Tool execution
              ↓
API Request:   { "role": "tool", "content": "..." }
```

---

## Refactor Phases

### Phase 1: Provider Trait Update
**Files:** `src/providers/traits.rs`, `src/providers/openai.rs`, etc.

**Changes:**
```rust
// Add to Provider trait
async fn chat_with_tools(
    &self,
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
    options: &ChatOptions,
) -> Result<ChatResponse>;

async fn stream_with_tools(
    &self,
    messages: &[ChatMessage],
    tools: &[ToolDefinition],
    options: &ChatOptions,
) -> Result<Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>>;
```

**OpenAI Provider Changes:**
- Update `OpenAIRequest` to include `tools: Option<Vec<OpenAITool>>`
- Parse `delta.tool_calls` from SSE stream
- Emit `StreamEvent::ToolCallStart/Delta/End` events
- Map `finish_reason: "tool_calls"` to `StopReason::ToolUse`

### Phase 2: Content Block System
**Files:** `src/types/mod.rs` (new file)

**Create unified content block enum:**
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    Thinking { thinking: String, signature: Option<String> },
    ToolCall { id: String, name: String, arguments: Value },
    ToolResult { tool_call_id: String, content: Vec<ContentBlock> },
}
```

**Update types:**
- `ChatMessage.content` becomes `Vec<ContentBlock>` instead of `String`
- Add `tool_calls: Option<Vec<ToolCall>>` to assistant messages
- Add `tool_call_id: Option<String>` to tool messages

### Phase 3: Agentic Loop Rewrite
**Files:** `src/engine/loop_v4.rs` (new file)

**New loop structure:**
```rust
pub struct AgenticLoopV4 {
    agent: Arc<Agent>,
    provider: Arc<dyn Provider>,
    tools: Vec<Arc<dyn Tool>>,
    max_iterations: usize,
}

impl AgenticLoopV4 {
    pub async fn run(
        &self,
        prompt: &str,
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
    ) -> Result<AgenticResult> {
        // 1. Build messages (system + history + user)
        // 2. Build tool definitions from tool registry
        // 3. Stream with tools
        // 4. Extract ContentBlock::ToolCall from response
        // 5. Execute tools, append results, loop
        // 6. Return final answer
    }
}
```

### Phase 4: Tool Registry Integration
**Files:** `src/tools/mod.rs`

**Add JSON Schema generation:**
```rust
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Value; // JSON Schema
    async fn execute(&self, args: Value) -> Result<ToolOutput>;
}
```

**Derive macro or builder for JSON Schema:**
```rust
#[derive(ToolSchema)]
struct WebSearchArgs {
    #[schema(description = "Search query")]
    query: String,
    #[schema(description = "Number of results", default = 10)]
    limit: usize,
}
```

### Phase 5: Session Storage Update
**Files:** `src/session/simple_session.rs`

**Update to store ContentBlock:**
```rust
pub struct SessionEntry {
    pub role: String,
    pub content: Vec<ContentBlock>,  // Changed from String
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
}
```

**JSONL format v4:**
```json
{"type": "message", "role": "assistant", "content": [{"type": "tool_call", "id": "call_123", "name": "web_search", "arguments": {"query": "..."}}]}
```

### Phase 6: Provider Implementations
**Files:** `src/providers/openai.rs`, `src/providers/anthropic.rs`, etc.

**OpenAI Implementation:**
- Request: Add `tools` field with JSON Schema
- Response: Parse `choices[0].delta.tool_calls`
- Events: Emit `ToolCallStart`, `ToolCallDelta`, `ToolCallEnd`
- Stop reason: Map `"tool_calls"` to `StopReason::ToolUse`

**Anthropic Implementation:**
- Similar pattern with Anthropic's tool format
- Handle `stop_reason: "tool_use"`

**Kimi/KimiCode:**
- OpenAI-compatible, reuse OpenAI provider logic

### Phase 7: Unified Agent API
**Files:** `src/agent/mod.rs`

**Single execute method:**
```rust
impl Agent {
    pub async fn execute(
        &self,
        prompt: &str,
        on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
    ) -> Result<AgenticResult> {
        let loop_ = AgenticLoopV4::new(...);
        loop_.run(prompt, on_event).await
    }
}
```

**Remove:**
- `execute_with_tools()` - replaced by unified `execute()`
- `execute_streaming()` - replaced by callback in `execute()`
- `execute_streaming_v3()` - temporary compatibility

### Phase 8: Channel Updates
**Files:** `src/channels/cli.rs`

**Update to use unified API:**
```rust
let result = agent.execute(prompt, |event| {
    match event {
        AgenticEvent::Assistant { content, .. } => {
            // Handle text/tool display
        }
        AgenticEvent::ToolStart { name, .. } => println!("🔧 {name}..."),
        AgenticEvent::ToolEnd { result, .. } => println!(" ✓"),
        _ => {}
    }
}).await?;
```

---

## Migration Strategy

### Backward Compatibility

**Session Files:**
- Detect v3 format (plain text content) on load
- Convert to v4 format (ContentBlock array)
- Save with new version marker

**Provider Trait:**
- Keep old `complete()` method as default impl
- New `stream_with_tools()` is additional, not replacement
- Gradual migration per-provider

**Agent API:**
- Mark `execute_with_tools()` as `#[deprecated]`
- Keep for 1 release cycle
- Remove in next minor version

### Testing Strategy

**Unit Tests:**
```rust
#[test]
fn test_tool_call_extraction() {
    let content = vec![
        ContentBlock::Text { text: "Let me search...".to_string() },
        ContentBlock::ToolCall { id: "call_1".to_string(), name: "web_search".to_string(), arguments: json!({"query": "test"}) },
    ];
    let calls = extract_tool_calls(&content);
    assert_eq!(calls.len(), 1);
}
```

**Integration Tests:**
- Mock provider that returns tool calls
- Verify loop executes tools and continues
- Verify final answer is extracted

**E2E Tests:**
- Real API calls with safe tools (calculator, echo)
- Verify streaming events
- Verify session persistence

---

## Estimated Timeline

| Phase | Files | Est. Time | Risk |
|-------|-------|-----------|------|
| 1. Provider trait | 2 | 4h | Low |
| 2. Content blocks | 3 | 6h | Medium |
| 3. Agentic loop v4 | 2 | 8h | High |
| 4. Tool registry | 2 | 4h | Low |
| 5. Session storage | 2 | 4h | Medium |
| 6. Provider impls | 4 | 12h | Medium |
| 7. Unified API | 2 | 4h | Low |
| 8. Channel updates | 2 | 4h | Low |
| **Testing** | - | 8h | - |
| **Total** | **19** | **54h** | **~1 week** |

---

## Benefits

1. **Reliability**: No more JSON parsing from text
2. **Performance**: Native API tool calling is faster
3. **Streaming**: Real tool call deltas (partial args)
4. **Simplicity**: Single code path, no parse fallback
5. **Standards**: OpenAI/Anthropic native format
6. **Extensibility**: Easy to add new providers

## Risks

1. **Breaking Change**: Old sessions need migration
2. **Provider Support**: Not all providers support native tools
3. **Complexity**: ContentBlock enum adds complexity
4. **Testing**: Large surface area to test

---

## Decision Needed

**Option A: Full Migration** (recommended)
- Implement all phases
- Remove text-based tool calling
- Clean, maintainable codebase

**Option B: Hybrid Support**
- Keep text-based as fallback
- Try native first, fallback to text parsing
- More complex but backward compatible

**Option C: Status Quo**
- Keep current text-based approach
- Fix parsing bugs only
- Technical debt accumulates

---

*Drafted after studying pi_agent_rust codebase*
*Date: March 4, 2026*
