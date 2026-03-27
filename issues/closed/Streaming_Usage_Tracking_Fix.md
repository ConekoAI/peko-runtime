# Streaming and Non-Streaming Usage Tracking Fix

## Problem Summary

The e2e test shows `usage: {input_tokens: 0, output_tokens: 0, total_tokens: 0}` for **both** streaming and non-streaming paths. Additionally, `provider` and `model` are empty strings in session JSONL metadata.

**Root causes identified:**

1. **Streaming path**: No `StreamEvent::Usage` variant exists. Provider SSE parsing discards usage data. Engine loop never accumulates usage and passes `None` to session storage.
2. **Non-streaming path**: Code correctly passes usage from provider response, but `provider`/`model` metadata is empty because `UnifiedSession::set_model()` is never called in the production flow.
3. **Session metadata**: `current_provider` and `current_model` on `UnifiedSession` are never set, producing empty strings in JSONL `role_metadata`.

---

## Task 1: Add `StreamEvent::Usage` variant and update `StreamOrchestrator`

**File:** `src/providers/traits.rs`

- Add `Usage { input: u64, output: u64, total: u64 }` variant to `StreamEvent` enum (after `Done`, before `Error`)

**File:** `src/engine/stream_orchestrator.rs`

- Add match arm for `StreamEvent::Usage { .. }` in `process()` returning `vec![]` (usage is handled by engine loop, not orchestrator)
  ```rust
  StreamEvent::Usage { .. } => {
      // Usage events are accumulated by the engine loop, not the orchestrator
      // The orchestrator handles content transformation, not metadata
      vec![]
  }
  ```
- Add unit test for the new variant to ensure it compiles and returns empty vec

---

## Task 2: Extract usage from OpenAI-compatible SSE streams (covers kimi/moonshot)

**File:** `src/providers/adapters/openai.rs`

1. Add `#[serde(default)] usage: Option<OpenAiUsage>` to `OpenAiStreamChunk` struct:
   ```rust
   #[derive(Debug, Deserialize)]
   struct OpenAiStreamChunk {
       choices: Vec<OpenAiStreamChoice>,
       #[serde(default)]
       usage: Option<OpenAiUsage>,  // Final chunk has usage + empty choices
   }
   ```

2. In `parse_sse_event()`: Check for usage BEFORE checking choices. If usage present with empty choices, return `StreamEvent::Usage`:
   ```rust
   fn parse_sse_event(&self, data: &str) -> Result<Option<StreamEvent>> {
       // ... [DONE] check ...
       
       let chunk: OpenAiStreamChunk = serde_json::from_str(data)?;
       
       // Check for usage first (final chunk has usage but empty choices)
       if let Some(usage) = chunk.usage {
           return Ok(Some(StreamEvent::Usage {
               input: usage.prompt_tokens as u64,
               output: usage.completion_tokens as u64,
               total: usage.total_tokens as u64,
           }));
       }
       
       // Continue with existing delta handling...
       let choice = match chunk.choices.into_iter().next() {
           Some(c) => c,
           None => return Ok(None),
       };
       // ... rest of method
   }
   ```

3. In `build_request()`: Add `stream_options` for streaming requests:
   ```rust
   fn build_request(&self, ..., stream: bool) -> Result<(String, Value)> {
       // ... existing body construction ...
       
       if stream {
           body["stream_options"] = json!({"include_usage": true});
       }
       
       Ok(("/chat/completions".to_string(), body))
   }
   ```

4. Add unit test `test_parse_sse_with_usage()`:
   - Test final chunk with usage and empty choices
   - Test that usage is correctly extracted (input: 10, output: 5, total: 15)

---

## Task 3: Extract usage from Anthropic SSE streams (covers kimi-code)

**Design Decision:** Use minimal adapter state to accumulate input tokens from `message_start` and emit a single complete `StreamEvent::Usage` on `message_delta`. This is cleaner than emitting partial usage events with zeros.

**File:** `src/providers/adapters/anthropic.rs`

1. Add state field to `AnthropicAdapter`:
   ```rust
   pub struct AnthropicAdapter {
       model: String,
       base_url: String,
       extra_headers: Vec<(String, String)>,
       /// Accumulates input tokens from message_start for usage tracking
       pending_input_tokens: Arc<tokio::sync::RwLock<Option<u32>>>,
   }
   ```

2. Update constructor to initialize the field:
   ```rust
   pub fn new(model: impl Into<String>) -> Self {
       Self {
           model: model.into(),
           base_url: "https://api.anthropic.com".to_string(),
           extra_headers: vec![("anthropic-version".to_string(), "2023-06-01".to_string())],
           pending_input_tokens: Arc::new(tokio::sync::RwLock::new(None)),
       }
   }
   ```

3. Add helper structs for SSE parsing:
   ```rust
   #[derive(Debug, Deserialize)]
   struct AnthropicMessageStartInfo {
       usage: Option<AnthropicUsage>,
   }
   
   #[derive(Debug, Deserialize)]
   struct AnthropicDeltaUsage {
       output_tokens: u32,
   }
   ```

4. Update `AnthropicSseEvent` struct:
   ```rust
   #[derive(Debug, Deserialize)]
   struct AnthropicSseEvent {
       #[serde(rename = "type")]
       event_type: Option<String>,
       index: Option<u32>,
       #[serde(rename = "content_block")]
       content_block: Option<AnthropicContentBlockInfo>,
       delta: Option<AnthropicDelta>,
       #[serde(rename = "stop_reason")]
       stop_reason: Option<String>,
       // New fields:
       message: Option<AnthropicMessageStartInfo>,
       usage: Option<AnthropicDeltaUsage>,
   }
   ```

5. Update `parse_sse_event()`:
   ```rust
   fn parse_sse_event(&self, data: &str) -> Result<Option<StreamEvent>> {
       let event: AnthropicSseEvent = serde_json::from_str(data)?;
       
       match event.event_type.as_deref() {
           Some("message_start") => {
               // Store input tokens for later combination with output tokens
               if let Some(usage) = event.message.and_then(|m| m.usage) {
                   *self.pending_input_tokens.write() = Some(usage.input_tokens);
               }
               Ok(Some(StreamEvent::Start { ... }))  // existing
           }
           Some("message_delta") => {
               // Check for usage output tokens
               if let Some(delta_usage) = event.usage {
                   let input = self.pending_input_tokens.read().unwrap_or(0);
                   let output = delta_usage.output_tokens;
                   return Ok(Some(StreamEvent::Usage {
                       input: input as u64,
                       output: output as u64,
                       total: (input + output) as u64,
                   }));
               }
               // ... existing stop_reason handling
           }
           // ... other cases
       }
   }
   ```

6. Add unit tests:
   - `test_message_start_usage_extraction()` - verifies input tokens are stored
   - `test_message_delta_usage_extraction()` - verifies combined usage is emitted

---

## Task 4: Accumulate streaming usage in engine loop and persist to session

**File:** `src/engine/loop_v4.rs` - `run_streaming_loop()`

1. Add per-iteration usage tracking (after line 977):
   ```rust
   let mut total_usage = crate::providers::TokenUsage::default();
   
   loop {
       iteration += 1;
       let mut iteration_usage = crate::providers::TokenUsage::default();  // NEW
       
       // ... get stream ...
   ```

2. Handle `StreamEvent::Usage` in the stream processing match (around line 1066):
   ```rust
   match stream_event {
       crate::providers::StreamEvent::ToolCallEnd { tool_call, .. } => {
           tool_calls.push(tool_call);
       }
       crate::providers::StreamEvent::Done { stop_reason: reason } => {
           stop_reason = reason;
       }
       // NEW: Handle usage events
       crate::providers::StreamEvent::Usage { input, output, total } => {
           iteration_usage.input += input;
           iteration_usage.output += output;
           iteration_usage.total += total;
       }
       _ => {}
   }
   ```

3. Accumulate iteration usage after stream processing (after line 1096):
   ```rust
   // Finalize orchestrator and emit remaining events
   let final_events = orchestrator.finalize();
   for event in final_events {
       on_event(event);
   }
   
   // NEW: Accumulate this iteration's usage
   total_usage.input += iteration_usage.input;
   total_usage.output += iteration_usage.output;
   total_usage.total += iteration_usage.total;
   ```

4. **Tool call storage** (line ~1165): Change `None` to `Some(iteration_usage)`:
   ```rust
   s.add_assistant_with_blocks(
       content_blocks,
       Some(tool_call_blocks),
       thinking_block,
       Some(iteration_usage.clone()),  // CHANGED from None
   ).await?;
   ```
   Note: `iteration_usage` should be reset or a new one created for next iteration.

5. **Final answer storage** (line ~1255): Change to pass usage:
   ```rust
   s.add_assistant(
       &accumulated_text,
       None,
       Some(iteration_usage.clone())  // CHANGED from None
   ).await?;
   ```

6. **Usage event emission** (line ~1263): Already reads from `total_usage`, no changes needed - it will now have correct accumulated values.

---

## Task 5: Set provider/model metadata on session

**Note:** This fixes empty `provider`/`model` in session metadata. While related to usage tracking (both affect session JSONL), this is technically a separate issue.

**File:** `src/engine/loop_v4.rs`

**Option A - In run_loop() and run_streaming_loop():**
At the start of each method (before the main loop), set the model once:
```rust
// Set provider/model metadata on session (do this once at start)
{
    let provider_name = self.provider.name();
    let model_name = self.agent.config.model.as_deref()
        .unwrap_or_else(|| self.provider.default_model());
    
    let mut s = session.write().await;
    s.set_model(provider_name, model_name);
}
```

**Option B - Check initialization code:**
Consider setting this in `src/agent/stateless_service.rs` or where the session is first created, rather than in the loop. This is cleaner as it happens once during session setup.

**Decision:** Use Option A for minimal changes, but investigate Option B if there's a clear session initialization point.

---

## Task 6: Update e2e test to verify usage tracking

**File:** `e2e_tests/session/session_usage.ps1`

Add assertions after getting session JSONL (after line 68):

```powershell
# Verify usage tracking for both sessions
Write-Host "`nVerifying usage tracking..." -ForegroundColor Cyan

foreach ($sessionId in @($sessionId1, $sessionId2)) {
    $jsonlPath = "$env:USERPROFILE/AppData/Roaming/pekobot/sessions/default/$agentName/$sessionId.jsonl"
    $content = Get-Content $jsonlPath -Raw
    $events = $content -split "`n" | Where-Object { $_ } | ForEach-Object { $_ | ConvertFrom-Json }
    
    # Find assistant message
    $assistantMsg = $events | Where-Object { 
        $_.type -eq "message.v2" -and $_.role -eq "assistant" 
    } | Select-Object -First 1
    
    if (-not $assistantMsg) {
        Write-Error "No assistant message found in session $sessionId"
        exit 1
    }
    
    # Verify usage is non-zero
    $usage = $assistantMsg.role_metadata.Assistant.usage
    if ($usage.total_tokens -eq 0) {
        Write-Error "Usage tracking failed for $sessionId`: total_tokens is 0"
        exit 1
    }
    Write-Host "  ✓ Usage: input=$($usage.input_tokens), output=$($usage.output_tokens), total=$($usage.total_tokens)" -ForegroundColor Green
    
    # Verify provider and model are non-empty
    $provider = $assistantMsg.role_metadata.Assistant.provider
    $model = $assistantMsg.role_metadata.Assistant.model
    
    if ([string]::IsNullOrWhiteSpace($provider)) {
        Write-Error "Provider is empty for $sessionId"
        exit 1
    }
    if ([string]::IsNullOrWhiteSpace($model)) {
        Write-Error "Model is empty for $sessionId"
        exit 1
    }
    Write-Host "  ✓ Provider: $provider, Model: $model" -ForegroundColor Green
}
```

---

## Task 7: Run unit tests and e2e test

1. Run unit tests for modified modules:
   ```bash
   cargo test -p pekobot providers::adapters::openai
   cargo test -p pekobot providers::adapters::anthropic
   cargo test -p pekobot engine::stream_orchestrator
   cargo test -p pekobot engine::loop_v4
   ```

2. Run full test suite:
   ```bash
   cargo test
   ```

3. Run e2e test:
   ```powershell
   cd e2e_tests/session
   ./session_usage.ps1 -Provider kimi
   ```

---

## Key Design Decisions

| Decision | Rationale |
|----------|-----------|
| **Anthropic state management** | Using `Arc<RwLock<Option<u32>>>` for `pending_input_tokens` keeps the adapter stateless across clones while allowing accumulation across SSE events. This is cleaner than emitting partial usage events with zeros, which would confuse the accumulation logic and future developers. |
| **Orchestrator passes through Usage** | The `StreamOrchestrator` handles content transformation (text chunking, tool call parsing), not metadata. Usage events pass through as `vec![]` because they're handled by the engine loop which manages the iteration lifecycle. |
| **Per-iteration usage accumulation** | Each streaming iteration (potentially multiple with tool calls) tracks its own `iteration_usage`, then adds to `total_usage`. This matches the non-streaming path where each `chat_with_tools` call returns usage for that iteration. |
| **Backward compatible** | All changes are additive. If a provider doesn't send usage, values remain 0 (existing behavior). No breaking changes to public APIs. |
| **stream_options placement** | Added only when `stream: true` in OpenAI adapter. This follows OpenAI's documented API for requesting usage in streaming mode. |

---

## Testing Checklist

- [ ] Unit test: OpenAI adapter parses SSE with usage
- [ ] Unit test: Anthropic adapter accumulates input + output tokens
- [ ] Unit test: StreamOrchestrator handles Usage event (returns empty vec)
- [ ] Unit test: Engine loop accumulates usage across iterations
- [ ] E2E test: Streaming session shows non-zero usage
- [ ] E2E test: Non-streaming session shows non-zero usage
- [ ] E2E test: Both sessions show non-empty provider/model
- [ ] Regression test: All 917+ existing tests pass
