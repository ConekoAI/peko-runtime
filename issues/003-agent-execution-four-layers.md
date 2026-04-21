# Issue 003: Agent Execution — Four Layers of Delegation

**Severity:** HIGH  
**Status:** Open  
**Labels:** `architecture`, `agent-execution`, `orchestration`, `refactor`, `adr-016`  
**Reported:** 2026-04-21  

---

## Summary

Agent execution involves four layers of delegation for the core operation. `Agent` has four execute methods that all delegate to `AgentExecutor`. `AgentExecutor` has the same four methods, delegating to `AgenticLoop`. Then `StatelessAgentService` cold-starts an `Agent` and calls `execute_with_session()`. This excessive layering adds no value, complicates state management, and makes the execution flow difficult to trace.

---

## Delegation Chain

```
StatelessAgentService::execute_message()
  → StatelessAgentService::execute()
    → StatelessAgentService::execute_inner()
      → Agent::new(config)
      → Agent::execute_with_session(prompt, session, history, on_event)
        → AgentExecutor::execute_with_session(prompt, session, history, on_event)
          → AgenticLoop::new(agent, provider, extension_core)
          → AgenticLoop::run_with_resume(prompt, on_event, session, history)
            → AgenticLoop::run_streaming_with_resume(...)
              → AgenticLoop::run_inner(...)
```

**Total: 4 orchestrator layers** (`StatelessAgentService`, `Agent`, `AgentExecutor`, `AgenticLoop`) for a single operation.

---

## Evidence

### Layer 1: `Agent` — Thin wrappers

**`src/agent/agent.rs` (lines 300–380):**
```rust
pub async fn execute(&self, prompt: &str, on_event: impl Fn(AgenticEvent) + Send + Sync + 'static) -> Result<AgenticResult> {
    let Some(provider) = self.provider_arc() else { return Err(...); };
    let executor = AgentExecutor::new(
        Arc::new(self.as_executor_agent()),
        provider,
        self.extension_core(),
    );
    executor.execute(prompt, on_event).await
}

pub async fn execute_with_session(&self, prompt: &str, session: Arc<RwLock<Session>>, history: Option<Vec<ChatMessage>>, on_event: impl Fn(AgenticEvent) + Send + Sync + 'static) -> Result<AgenticResult> {
    let Some(provider) = self.provider_arc() else { return Err(...); };
    let executor = AgentExecutor::new(
        Arc::new(self.as_executor_agent()),
        provider,
        self.extension_core(),
    );
    executor.execute_with_session(prompt, session, history, on_event).await
}

pub async fn execute_streaming(&self, prompt: &str) -> Result<mpsc::Receiver<AgenticEvent>> { ... }

pub async fn execute_streaming_with_session<F>(&self, prompt: &str, session: Arc<RwLock<Session>>, history: Option<Vec<ChatMessage>>, on_event: F) -> Result<AgenticResult>
where F: Fn(AgenticEvent) + Send + Sync + 'static { ... }
```

All four methods:
1. Check `provider_arc()`
2. Call `as_executor_agent()` (shallow clone)
3. Construct `AgentExecutor`
4. Delegate to the matching `AgentExecutor` method

### `as_executor_agent()` — Shallow clone workaround

**`src/agent/agent.rs` (lines 382–413):**
```rust
fn as_executor_agent(&self) -> Self {
    Self {
        config: self.config.clone(),
        state: Arc::clone(&self.state),
        identity: Identity { did: self.identity.did.clone(), document: self.identity.document.clone(), keypair: None },
        provider: None, // Provider passed separately to avoid double-Arc
        session_manager: Arc::clone(&self.session_manager),
        session_router: SessionRouter::new(Arc::clone(&self.session_manager), ...),
        extension_core: Arc::clone(&self.extension_core),
        current_session_id: Arc::clone(&self.current_session_id),
    }
}
```

This exists solely because `AgentExecutor` takes `Arc<Agent>`, and the caller already holds `&self`.

### Layer 2: `AgentExecutor` — More thin wrappers

**`src/agent/executor.rs` (lines 22–267):**
```rust
pub struct AgentExecutor {
    agent: Arc<Agent>,
    provider: Arc<Provider>,
    extension_core: Arc<ExtensionCore>,
}

impl AgentExecutor {
    pub async fn execute(&self, prompt: &str, on_event: impl Fn(AgenticEvent) + Send + Sync + 'static) -> Result<AgenticResult> {
        self.agent.set_state(AgentState::Busy);
        let loop_ = AgenticLoop::new(Arc::clone(&self.agent), Arc::clone(&self.provider), Arc::clone(&self.extension_core)).await;
        let result = loop_.run(prompt, on_event).await;
        self.agent.set_state(AgentState::Idle);
        result
    }

    pub async fn execute_with_session(&self, prompt: &str, session: Arc<RwLock<Session>>, history: Option<Vec<ChatMessage>>, on_event: impl Fn(AgenticEvent) + Send + Sync + 'static) -> Result<AgenticResult> {
        // Same pattern: set_busy → create_loop → run → set_idle
    }

    pub async fn execute_streaming(&self, prompt: &str) -> Result<mpsc::Receiver<AgenticEvent>> { ... }

    pub async fn execute_streaming_with_session<F>(&self, prompt: &str, session: Arc<RwLock<Session>>, history: Option<Vec<ChatMessage>>, on_event: F) -> Result<AgenticResult> { ... }
}
```

`AgentExecutor` adds:
- State management (`set_state(Busy/Idle)`)
- `prepare_execution()` (calls `init_builtins_async()` + `AgentInit` hook)
- But otherwise just constructs `AgenticLoop` and delegates.

### Layer 3: `StatelessAgentService` — Cold-start wrapper

**`src/agent/stateless_service.rs` (lines 474–639):**
```rust
async fn execute_inner(&self, request: ExecutionRequest) -> Result<ExecutionResult> {
    // 1. Load config
    let config_entry = self.load_config_fresh(&request.agent_name).await?;
    // 2. Open session
    let session = session_manager.open_session(&request.session_id).await?;
    // 3. Load history
    let history = self.load_session_history(session.clone()).await?;
    // 4. Cold-start agent
    let agent = Agent::new(config_entry.config.clone()).await?;
    // 5. Execute
    let execute_result = agent.execute_with_session(&request.message, session.clone(), Some(history), |_event| {}).await;
    // 6. Agent dropped here
}
```

### Layer 4: `AgenticLoop` — The actual work

**`src/engine/agentic_loop.rs` (lines 55–893):**
```rust
pub struct AgenticLoop {
    agent: Arc<Agent>,
    provider: Arc<Provider>,
    max_iterations: usize,
    system_prompt: String,
    extension_core: Arc<ExtensionCore>,
}
```

This is where the actual LLM streaming, tool execution, and session management happen.

---

## Impact

1. **Cognitive overhead:** To understand a single execution, a developer must read 4 files and ~600 lines of delegation code.
2. **State management bugs:** `Agent::state` is an `Arc<RwLock<AgentState>>` shared between the original `Agent` and the `as_executor_agent()` clone. State transitions happen in `AgentExecutor`, but the original `Agent` is the one exposed to callers. Race conditions are possible.
3. **Redundant cloning:** `as_executor_agent()` clones `config`, `session_manager`, `session_router`, `extension_core`, `current_session_id` on every execution.
4. **Provider double-Arc:** The provider is stored in `Agent` but passed separately to `AgentExecutor` to avoid `Arc<Arc<Provider>>`.
5. **Testing difficulty:** Unit tests must mock or construct 4 layers to test one execution path.
6. **ADR-016 incomplete:** The stateless service architecture was intended to simplify execution, but it added another layer instead of collapsing existing ones.

---

## Root Cause

- `AgentExecutor` was extracted from `Agent` to eliminate `clone_for_loop()`, but the extraction was shallow — `Agent` still has the same methods delegating to `AgentExecutor`.
- `StatelessAgentService` was added for ADR-016 (stateless cold-start) without removing or simplifying the existing `Agent`/`AgentExecutor` pair.
- `AgenticLoop` was introduced as the "unified streaming core" but the old layers were preserved for backward compatibility.

---

## Proposed Resolution

### Option A: Collapse `Agent` + `AgentExecutor` into one layer (Recommended)

1. **Move `AgentExecutor`'s logic into `Agent` directly.** `Agent` should hold `provider: Arc<Provider>` and `extension_core: Arc<ExtensionCore>` and execute directly via `AgenticLoop`.
2. **Delete `AgentExecutor`.**
3. **Delete `as_executor_agent()`.**
4. `Agent` becomes the sole orchestrator for owned/long-lived agents.

### Option B: Make `Agent` a pure data/config object

1. **Strip all execute methods from `Agent`.** `Agent` holds only config, identity, and state.
2. **`AgentExecutor` becomes the sole execution entry point.** It takes `Arc<Agent>` + `Arc<Provider>` + `Arc<ExtensionCore>`.
3. **`StatelessAgentService` constructs `Agent` + `AgentExecutor` per request.**
4. This is cleaner but requires more call-site changes.

### Option C: Introduce `AgentRunner` facade

1. Create a single `AgentRunner` that owns `Agent`, `Provider`, and `ExtensionCore`.
2. `AgentRunner::execute*()` is the only public API.
3. `StatelessAgentService` creates an `AgentRunner` per request.
4. `Agent` and `AgentExecutor` become private implementation details.

---

## Acceptance Criteria

- [ ] The execution stack has at most 2 layers between the caller and `AgenticLoop`.
- [ ] `as_executor_agent()` is removed.
- [ ] There is a single, clear entry point for agent execution (either `Agent::execute*`, `AgentExecutor::execute*`, or a new `AgentRunner`).
- [ ] `StatelessAgentService` does not bypass layers or create redundant `Agent` clones.
- [ ] All existing tests pass without increasing mock complexity.

---

## Related

- `src/agent/agent.rs`
- `src/agent/executor.rs`
- `src/agent/stateless_service.rs`
- `src/engine/agentic_loop.rs`
- ADR-016: Stateless Agent Service
- ADR-020: Daemon-Based Async Execution
