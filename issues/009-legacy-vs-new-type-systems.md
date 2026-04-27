# Issue 009: Legacy vs. New Provider Type Systems

**Severity:** HIGH  
**Status:** 🟡 **Open**  
**Labels:** `architecture`, `providers`, `types`, `legacy`, `refactor`  
**Reported:** 2026-04-27  

---

## Summary

Two parallel type hierarchies exist for the same domain concepts (chat messages, content blocks, message roles):
- **Legacy:** `crate::types::message::{ChatMessage, ContentBlock, MessageRole}`
- **New:** `providers::types::{ChatMessage, ContentBlock, MessageRole}`

`providers/core.rs` spends ~170 lines on conversion boilerplate (`convert_chat_messages`, `convert_tools`, `convert_response`) bridging the two systems. Every message format change requires updating both type systems and the bridge.

---

## The Two Type Systems

### Legacy System (`crate::types::message`)

**Location:** `src/types/message.rs`  
**Used by:** Engine, session, tools, extensions  
**Purpose:** Internal runtime representation

```rust
// src/types/message.rs
pub struct ChatMessage {
    pub role: MessageRole,
    pub content: String,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
}

pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

pub enum ContentBlock {
    Text(String),
    Image { ... },
    ToolCall { ... },
    ToolResult { ... },
}
```

---

### New System (`providers::types`)

**Location:** `src/providers/types.rs`, `src/providers/traits.rs`  
**Used by:** Provider adapters (OpenAI, Anthropic)  
**Purpose:** Provider-specific representation

```rust
// src/providers/traits.rs
pub struct ChatMessage {
    pub role: MessageRole,
    pub content: Vec<ContentBlock>,  // Note: Vec<ContentBlock> vs String
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
}

pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

pub enum ContentBlock {
    Text(String),
    Image { ... },
    ToolCall { ... },
    ToolResult { ... },
}
```

**Key difference:** `providers::ChatMessage` uses `Vec<ContentBlock>` for content, while `crate::types::ChatMessage` uses `String`. This is a meaningful semantic difference (multi-modal support), but the two systems are 90% identical.

---

## Evidence of Bridge Code

### `providers/core.rs` — The Conversion Layer

```rust
// src/providers/core.rs (lines 374–541)
fn convert_chat_messages(
    messages: Vec<crate::types::message::ChatMessage>,
) -> Vec<providers::types::ChatMessage> {
    messages.into_iter().map(|m| {
        providers::types::ChatMessage {
            role: convert_role(m.role),
            content: vec![ContentBlock::Text(m.content)],  // Wrap String in Vec
            tool_calls: m.tool_calls.map(convert_tool_calls),
            tool_call_id: m.tool_call_id,
        }
    }).collect()
}

fn convert_tools(
    tools: Vec<crate::types::message::ToolDefinition>,
) -> Vec<providers::types::ToolDefinition> { ... }

fn convert_response(
    response: providers::types::ChatResponse,
) -> crate::types::message::ChatResponse { ... }
```

### Provider Adapters Use New Types Internally

```rust
// src/providers/adapters/openai.rs
fn build_request_body(&self, messages: Vec<providers::types::ChatMessage>, ...) -> Value { ... }
fn parse_response(&self, json: Value) -> Result<providers::types::ChatResponse> { ... }
```

But the `Provider` struct in `core.rs` accepts legacy types and converts them:

```rust
// src/providers/core.rs
pub async fn chat_with_tools(
    &self,
    messages: Vec<crate::types::message::ChatMessage>,  // Legacy type
    tools: Vec<crate::types::message::ToolDefinition>,   // Legacy type
) -> Result<crate::types::message::ChatResponse> {      // Legacy type
    let converted_messages = convert_chat_messages(messages);
    let converted_tools = convert_tools(tools);
    let provider_response = adapter.chat(converted_messages, converted_tools).await?;
    Ok(convert_response(provider_response))
}
```

---

## Impact

1. **Maintenance burden:** Adding a new field to `ChatMessage` requires updating both type definitions and the conversion functions.
2. **Runtime overhead:** Every provider call allocates a new `Vec<ChatMessage>` and converts every message, even when the content is a simple string.
3. **Type confusion:** Developers must know which `ChatMessage` type to use in which context. The same name in different modules is error-prone.
4. **Legacy debt:** The `crate::types::message` module is used throughout the engine, session, and tools modules. Migrating everything to `providers::types` is a large refactor.

---

## Root Cause

- The original `crate::types::message` system was built for simple text-only chat.
- When provider adapters were added, they needed multi-modal support (`Vec<ContentBlock>`), so a new type system was created in `providers::types`.
- The two systems were never unified because `crate::types::message` is deeply embedded in the engine and session modules.

---

## Proposed Resolution

**Option A: Eliminate legacy types, migrate engine/session to `providers::types` (Recommended long-term)**

1. **Audit all usages of `crate::types::message`** in engine, session, and tools.
2. **Migrate `engine/agentic_loop.rs`** to use `providers::types::ChatMessage`.
3. **Migrate `session/unified.rs`** to store `Vec<ContentBlock>` instead of `String` for message content.
4. **Delete `crate::types::message`** and rename `providers::types` to `crate::types::message` (or a new canonical module).
5. **Delete conversion functions** in `providers/core.rs`.

This is a large refactor but eliminates the duplication permanently.

**Option B: Auto-derive conversions with a macro**

Create a `#[derive(MessageConvert)]` procedural macro that generates `convert_chat_messages`, `convert_tools`, and `convert_response` automatically. This reduces boilerplate but does not eliminate the root problem.

**Option C: Make `providers::types` a thin wrapper**

Define `providers::ChatMessage` as a re-export or newtype of `crate::types::ChatMessage` with additional methods for multi-modal content. This minimizes duplication but may not fully satisfy provider adapter needs.

---

## Acceptance Criteria

- [ ] There is exactly **one** `ChatMessage` / `ContentBlock` / `MessageRole` type hierarchy in the codebase.
- [ ] No manual conversion functions exist between legacy and new types.
- [ ] Provider adapters, engine, and session modules all use the same types.
- [ ] Multi-modal content (`Vec<ContentBlock>`) is supported end-to-end.
- [ ] All existing tests pass.

---

## Related

- `src/types/message.rs`
- `src/providers/types.rs`
- `src/providers/traits.rs`
- `src/providers/core.rs`
- `src/providers/adapters/openai.rs`
- `src/providers/adapters/anthropic.rs`
- `src/engine/agentic_loop.rs`
- `src/session/unified.rs`
