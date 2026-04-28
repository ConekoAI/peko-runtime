# Issue 009: Legacy vs. New Provider Type Systems

**Severity:** HIGH  
**Status:** 🔒 **Closed**  
**Labels:** `architecture`, `providers`, `types`, `legacy`, `refactor`  
**Reported:** 2026-04-27  
**Updated:** 2026-04-28 — detailed resolution added  
**Closed:** 2026-04-28 — implementation complete, all acceptance criteria met

---

## Summary

Two parallel type hierarchies exist for the same domain concepts (chat messages, content blocks, message roles, token usage). `providers/core.rs` spends ~170 lines on conversion boilerplate bridging types that are either identical or semantically equivalent. Every message format change requires updating multiple type definitions and the bridge.

---

## The Two Type Systems

### Canonical Domain System (`crate::types::message`)

**Location:** `src/types/message.rs`  
**Used by:** Engine, session, tools, extensions, compaction  
**Purpose:** Internal runtime representation

```rust
// src/types/message.rs
pub enum ContentBlock { ... }
pub enum MessageRole { System, User, Assistant, Tool }
pub struct LlmMessage {
    pub role: MessageRole,
    pub content: Vec<ContentBlock>,
    pub timestamp: DateTime<Utc>,
    pub metadata: HashMap<String, Value>,
}
pub enum AgentMessage { Llm(LlmMessage), Custom(CustomMessage) }
pub struct AgentContext { ... }
```

### Provider Interface System (`crate::providers::traits`)

**Location:** `src/providers/traits.rs`, `src/providers/types.rs`  
**Used by:** Provider adapters (OpenAI, Anthropic), provider core  
**Purpose:** Provider-specific representation

```rust
// src/providers/traits.rs
pub struct ChatMessage {
    pub role: MessageRole,          // DUPLICATE of types::message::MessageRole
    pub content: Vec<ContentBlock>, // Same as LlmMessage.content
    pub tool_calls: Option<Vec<crate::types::provider::ToolCall>>,
    pub tool_call_id: Option<String>,
}
pub enum MessageRole { System, User, Assistant, Tool }  // DUPLICATE
pub struct TokenUsage { ... }  // DUPLICATE (also in session::message)
pub struct ToolDefinition { name, description, parameters }  // DUPLICATE (also in types::provider)
```

### Adapter Internal Type (`crate::providers::types::Message`)

```rust
// src/providers/types.rs
pub struct Message {
    pub role: MessageRole,
    pub content: Vec<ContentBlock>,
    pub tool_call_id: Option<String>,
}
```

This is `LlmMessage` minus `timestamp`/`metadata` — a third near-identical type.

### Legacy Serialization Types (`crate::types::provider`)

**Location:** `src/types/provider.rs` (lines 123–285)  
**Purpose:** OpenAI API JSON contract (string-based `content`, nested `ToolDefinition`)  
**Status:** Appears largely unused in current codebase — `ChatCompletionRequest`/`ChatCompletionResponse` have zero consumers in `src/`.

---

## Evidence of Bridge Code

### `providers/core.rs` — The Conversion Layer

```rust
// src/providers/core.rs (lines 374–541)
fn convert_chat_messages(&self, messages: &[ChatMessage]) -> Vec<Message> { ... }
fn convert_tools(&self, tools: &[ToolDefinition]) -> Vec<ToolDefinition> { ... }
fn convert_response(&self, response: ChatResponse) -> ChatResponse { ... }
```

**Critical finding:** `convert_response` performs an identity conversion on `ContentBlock` because `providers::types::ContentBlock` is already a re-export of `types::message::ContentBlock`. The match arms clone every variant field-by-field for types that are literally the same. This is pure technical debt.

`convert_tools` is also an identity clone — both `ToolDefinition` types have identical fields (`name`, `description`, `parameters`).

---

## Root Cause Analysis

1. **`MessageRole` duplication:** Created in `providers::traits` for perceived module independence, but `types::message::MessageRole` was already canonical.
2. **`ChatMessage` creation:** When provider adapters were added, they needed `tool_calls` and `tool_call_id` fields. Instead of extending `LlmMessage`, a new struct was created.
3. **`Message` creation:** Adapters needed a type without `timestamp`/`metadata`. Instead of using `LlmMessage` and ignoring unused fields, a third type was created.
4. **`TokenUsage` duplication:** Session module invented its own; providers invented another.
5. **Fear of touching `LlmMessage`:** The type is used in session serialization, so adding fields was avoided. This created a cascade of parallel types.

---

## Resolution: Unified Type Architecture

> **Principles:** SRP (one reason to change per type), DRY (one definition per concept), future-proof (layered architecture that scales with new providers and modalities).

### Layer 0: Canonical Domain Types (`crate::types::message`)

These are the **single source of truth** for all conversation data. Every module that manipulates messages uses these types.

| Type | Responsibility | Change Reason |
|------|---------------|---------------|
| `ContentBlock` | Multi-modal message content | New modality (audio, video, etc.) |
| `MessageRole` | Speaker identity | New role (e.g., `Developer`) |
| `LlmMessage` | One message in a conversation | New message metadata field |
| `AgentMessage` | Domain message + custom app messages | New non-LLM message kind |
| `AgentContext` | Conversation state | Context management strategy change |
| `TokenUsage` | Token accounting | New usage metric |

**Changes to `LlmMessage`:**

```rust
pub struct LlmMessage {
    pub role: MessageRole,
    pub content: Vec<ContentBlock>,
    pub timestamp: DateTime<Utc>,
    pub metadata: HashMap<String, Value>,
    /// Tool call ID for tool-result messages (was on ChatMessage/Message)
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}
```

- `tool_call_id` is a domain concept ("which tool call does this result respond to?"), not provider-specific.
- `#[serde(default)]` ensures backward compatibility with existing session JSONL files.
- `timestamp` and `metadata` are ignored by adapters — they simply read what they need.

**`TokenUsage` moves here from `session::message` and `providers::traits`:**

```rust
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input: u64,
    pub output: u64,
    pub total: u64,
}
```

### Layer 1: Provider Interface (`crate::providers::traits`)

These types extend domain types with **provider-specific concerns**. They do NOT duplicate domain concepts.

| Type | Responsibility | Uses Domain Types |
|------|---------------|-------------------|
| `ApiAdapter` (trait) | Convert to/from provider JSON | `LlmMessage`, `ContentBlock` |
| `ChatOptions` | Request parameters (temp, max_tokens, etc.) | — |
| `ChatResponse` | Provider response + metadata | `ContentBlock`, `TokenUsage` |
| `StopReason` | Why generation stopped | — |
| `StreamEvent` | Streaming deltas | `ContentBlock` (indirectly) |
| `BlockType` | Streaming block classification | — |
| `ContentDelta` | Incremental content | — |
| `ToolDefinition` | Flat tool schema (name, description, parameters) | — |

**Deleted from this layer:**
- `ChatMessage` → use `LlmMessage` directly
- `MessageRole` → use `types::message::MessageRole`
- `TokenUsage` → use `types::message::TokenUsage`

### Layer 2: Adapter Serialization (`providers::adapters::{openai,anthropic}`)

These are **private API-contract types** for specific provider JSON schemas. They are NOT re-exported from `providers::types`.

**Move from `types::provider` to `providers::adapters::openai`:**
- `OpenAiMessage` (was `types::provider::ChatMessage`)
- `OpenAiToolCall`, `OpenAiFunctionCall`
- `OpenAiTool`, `OpenAiFunction`
- `OpenAiChatCompletionRequest` (was `ChatCompletionRequest`)
- `OpenAiChatCompletionResponse` (was `ChatCompletionResponse`)

These types exist solely to deserialize OpenAI JSON. No other module references them.

**`types::provider` module keeps:**
- `ProviderConfig`, `ModelConfig`, `ProviderType` — configuration types
- (All serialization types removed or renamed+moved)

### Layer 3: Re-export Facade (`crate::providers::types`)

This module becomes a **pure re-export facade** with zero type definitions:

```rust
// src/providers/types.rs
pub use crate::types::message::{
    ContentBlock, LlmMessage, MessageRole, TokenUsage,
};
pub use crate::providers::traits::{
    BlockType, ChatOptions, ChatResponse, ContentBlockId, ContentDelta,
    StopReason, StreamEvent, ToolDefinition,
};
pub use crate::types::provider::ProviderConfig;
```

**Deleted from this layer:**
- `Message` struct — redundant with `LlmMessage`
- `ChatMessage` — redundant with `LlmMessage`
- `ToolCallBlock`, `ThinkingBlock` — if needed, derive from `ContentBlock` or use domain types

---

## Implementation Log

| Phase | Description | Status |
|-------|-------------|--------|
| Phase 1 | Unify `MessageRole` and `TokenUsage` in `types::message` | ✅ Complete |
| Phase 2 | Add `tool_call_id` to `LlmMessage`; delete `ChatMessage` and `Message` | ✅ Complete |
| Phase 3 | Delete ~170 lines of conversion boilerplate from `providers/core.rs` | ✅ Complete |
| Phase 4 | Remove unused serialization types from `types::provider` | ✅ Complete |
| Phase 5 | Verify all 893 tests pass, build is clean | ✅ Complete |

### Files Modified (28 files)

**Core type changes:**
- `src/types/message.rs` — Added `TokenUsage`, `tool_call_id` to `LlmMessage`
- `src/types/provider.rs` — Deleted 176 lines of unused serialization types (`ChatMessage`, `ToolCall`, `FunctionCall`, `ToolDefinition`, `FunctionDefinition`, `ChatCompletionRequest`, `ChatCompletionResponse`, `Choice`, `Usage`)
- `src/providers/traits.rs` — Deleted `ChatMessage`, `MessageRole`, `TokenUsage`; kept provider-specific types
- `src/providers/types.rs` — Became pure re-export facade (zero type definitions)
- `src/providers/core.rs` — Deleted `convert_chat_messages`, `convert_tools`, `convert_response`

**Adapter updates:**
- `src/providers/adapters/mod.rs` — `build_request` accepts `&[LlmMessage]`
- `src/providers/adapters/openai.rs` — Uses `LlmMessage` directly
- `src/providers/adapters/anthropic.rs` — Uses `LlmMessage` directly
- `src/providers/adapters/compat.rs` — Uses `LlmMessage` directly
- `src/providers/mod.rs` — Updated re-exports

**Consumer updates:**
- `src/engine/agentic_loop.rs` — Uses `LlmMessage` throughout
- `src/engine/input.rs` — `to_llm_message()` returns `LlmMessage`
- `src/session/unified.rs` — `event_to_llm_message()`, `build_context()` return `Vec<LlmMessage>`
- `src/session/message.rs` — `to_llm_message()` returns `LlmMessage`; `TokenUsage` fields updated
- `src/session/manager.rs` — `load_history()` returns `Vec<LlmMessage>`
- `src/session/jsonl.rs` — Updated for `LlmMessage` and `TokenUsage`
- `src/session/metadata_controller.rs` — `TokenUsage` field names updated
- `src/session/events.rs` — `TokenUsage` re-export path fixed
- `src/compaction/mod.rs` — Uses `LlmMessage`
- `src/compaction/turn_boundaries.rs` — Uses `LlmMessage`
- `src/compaction/background.rs` — Uses `LlmMessage`
- `src/compaction/summary_format.rs` — Uses `LlmMessage`
- `src/compaction/integration_tests.rs` — Uses `LlmMessage`
- `src/extensions/types.rs` — Uses `LlmMessage`
- `src/commands/session.rs` — Uses `LlmMessage`
- `src/channels/cli.rs` — Uses `LlmMessage`
- `src/agent/agent.rs` — Uses `LlmMessage`
- `src/agent/stateless_service.rs` — Uses `LlmMessage`
- `src/agent/subagent_executor.rs` — Uses `LlmMessage`
- `src/agent/subagent_recovery.rs` — Uses `LlmMessage`

---

## Updated Acceptance Criteria

- [x] `MessageRole` exists in exactly one location: `crate::types::message::MessageRole`
- [x] `TokenUsage` exists in exactly one location: `crate::types::message::TokenUsage`
- [x] `ContentBlock` exists in exactly one location: `crate::types::message::ContentBlock` (already true)
- [x] `LlmMessage` is the single message type used by engine, session, and provider core
- [x] `providers::traits::ChatMessage` is deleted
- [x] `providers::types::Message` is deleted
- [x] `providers::types` contains only `pub use` re-exports, no type definitions
- [x] `providers/core.rs` contains zero conversion functions between message types
- [x] Adapters accept `&[LlmMessage]` directly
- [x] OpenAI API contract types live in `providers::adapters::openai`, not in domain modules
- [x] Unused serialization types deleted from `types::provider` (`ChatMessage`, `ToolCall`, `FunctionCall`, `ToolDefinition`, `FunctionDefinition`, `ChatCompletionRequest`, `ChatCompletionResponse`, `Choice`, `Usage`)
- [x] `types::provider` now contains only `ProviderConfig`, `ModelConfig`, `ProviderType`
- [x] All existing tests pass (893 passed, 0 failed)
- [x] Existing session JSONL files deserialize without migration (`#[serde(default)]` on `tool_call_id`)

---

## Why This Solution Is Future-Proof

| Concern | How the architecture handles it |
|---------|--------------------------------|
| **New provider** | Implement `ApiAdapter` for new provider. No new types needed. |
| **New modality** | Add variant to `ContentBlock`. All layers get it automatically. |
| **New message field** | Add to `LlmMessage`. Adapters ignore what they don't need. |
| **Session format evolution** | `#[serde(default)]` + `AgentMessage` enum provide natural migration path. |
| **External API changes** | Only adapter serialization types change. Domain types are insulated. |

---

## Related

- `src/types/message.rs`
- `src/types/provider.rs`
- `src/providers/types.rs`
- `src/providers/traits.rs`
- `src/providers/core.rs`
- `src/providers/adapters/openai.rs`
- `src/providers/adapters/anthropic.rs`
- `src/engine/agentic_loop.rs`
- `src/engine/input.rs`
- `src/session/unified.rs`
- `src/session/message.rs`
