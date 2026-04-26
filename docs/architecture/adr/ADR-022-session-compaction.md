# ADR-022: Full Session Compaction Mechanism

**Status**: Proposed  
**Date**: 2026-04-26  
**Last Updated**: 2026-04-26  
**Author**: Kimi Code CLI  
**Depends On**: ADR-017 (Unified Extension Architecture), ADR-019 (Dynamic Tool and Prompt Updates)  
**Replaces / Supersedes**: Ad-hoc compaction in `src/compaction/mod.rs` and `src/compaction/background.rs`

---

## Context

Pekobot currently has a **partially implemented** session compaction subsystem:

- `src/compaction/mod.rs` — Core `Compactor` with LLM-based summarization, cumulative summary tracking, and token estimation.
- `src/compaction/background.rs` — `BackgroundCompactor` worker with quotas and cooldowns.
- `src/engine/agentic_loop.rs` — Invokes background compaction opportunistically during the agent loop.
- `src/session/unified.rs` — `Session::record_compaction()` and `Session::load_previous_compaction_summary()`.

However, an investigation (see *Gap Analysis* below) reveals that the mechanism is **not fully functional** in production. Key issues include: the `SessionCompaction` extension hook is never invoked; configuration is hard-coded; compacted summaries are not persisted as messages; and the background compactor state is not shared across runs.

This ADR defines a **complete, production-ready session compaction architecture** that closes all gaps and adds user-requested capabilities: pluggable compaction via extensions, per-provider context limits, dual-file session storage, and CLI-triggered manual compaction.

---

## Gap Analysis (Current State)

| # | Gap | Severity | Location |
|---|-----|----------|----------|
| 1 | `SessionCompaction` extension hook is declared but **never invoked**. | High | `src/extensions/core/hook_points.rs`, `src/engine/agentic_loop.rs` |
| 2 | `CompactionConfig` is **hard-coded** (`CompactionConfig::default()`). No TOML integration. | Medium | `src/engine/agentic_loop.rs:303`, `src/types/config.rs` |
| 3 | Compacted summary is **not persisted as a system message** in the session JSONL. On resume, the full un-compacted history is reconstructed. | High | `src/compaction/mod.rs`, `src/session/unified.rs` |
| 4 | `load_previous_compaction_summary` only returns the **latest** compaction record, dropping intermediate context. | Medium | `src/session/unified.rs:693` |
| 5 | `BackgroundCompactor` is recreated per `run_inner` call; quota/cooldown state is **not shared** across user prompts. | Medium | `src/engine/agentic_loop.rs:294` |
| 6 | `SystemEvent { event: "compaction" }` is **not normalized** by `normalize_event`, so `load_normalized` skips it. | Medium | `src/session/jsonl.rs:352` |
| 7 | `test_select_messages` is **disabled** (commented out). | Low | `src/compaction/mod.rs:573` |

---

## Decision

We will redesign session compaction around four pillars:

1. **Extension-Hook Lifecycle** — Expose compaction as first-class extension hooks so users can plug in custom strategies.
2. **Minimal Built-In Compaction** — The default compactor auto-triggers at 80 % of the provider/model context window.
3. **Dual-File Session Storage** — Split session persistence into an immutable **user history file** and a mutable **agent context window file**.
4. **Manual CLI Trigger** — Users can force compaction early via `pekobot session compact`.

---

## 1. Extension-Hook Lifecycle

Compaction becomes a **hook-driven pipeline** inside the agentic loop. The existing `HookPoint::SessionCompaction` (ADR-017) is finally wired up, and two new hook points are added for pre/post processing.

### New & Existing Hook Points

```rust
pub enum HookPoint {
    // ... existing ...

    /// Called BEFORE the built-in compactor runs.
    /// Extensions may return a custom summary to REPLACE the default behavior.
    /// If the hook returns PassThrough, the default compactor proceeds.
    SessionCompaction,

    /// Called AFTER compaction completes (whether by built-in or extension).
    /// Extensions may augment, validate, or log the compacted result.
    SessionCompactionPost,

    /// Called when building the context window from session storage.
    /// Extensions may reorder, filter, or inject messages.
    SessionContextBuild,
}
```

### Hook Semantics

| Hook | Input | Output | Behavior |
|------|-------|--------|----------|
| `SessionCompaction` | `HookInput::SessionState { messages, estimated_tokens, threshold_tokens, model_context_limit }` | `HookOutput::Text(summary)` → use this summary directly; skip built-in. `HookResult::PassThrough` → run built-in. | **Pre-compaction override** |
| `SessionCompactionPost` | `HookInput::SessionState { messages, summary, tokens_before, tokens_after }` | `HookOutput::MessageVec(msgs)` → replace final message list. `HookResult::PassThrough` → accept as-is. | **Post-compaction augmentation** |
| `SessionContextBuild` | `HookInput::SessionState { history_entries }` | `HookOutput::MessageVec(msgs)` → replace context. | **Context assembly override** |

### Integration in Agentic Loop

```rust
// Pseudo-code inside run_inner(), before LLM call

let estimated_tokens = Compactor::estimate_tokens(&messages);
let threshold = (model_context_limit * 8) / 10; // 80%

if estimated_tokens >= threshold {
    // 1. Give extensions first shot
    let hook_result = extension_core
        .invoke_hook_text(
            HookPoint::SessionCompaction,
            HookInput::SessionState { /* ... */ }
        )
        .await;

    let compacted = match hook_result {
        Some(custom_summary) => {
            // Extension provided a custom summary
            apply_custom_compaction(&messages, custom_summary)
        }
        None => {
            // Run built-in background compactor
            background_compactor.request_compaction(messages.clone(), prev_summary).await?
            // ... await result ...
        }
    };

    // 2. Post-compaction hook
    let final_messages = extension_core
        .invoke_hook_message_vec(
            HookPoint::SessionCompactionPost,
            HookInput::SessionState { messages: compacted, /* ... */ }
        )
        .await
        .unwrap_or(compacted);

    messages = final_messages;
}
```

---

## 2. Minimal Built-In Compaction

### Per-Provider / Per-Model Context Limits

The built-in compactor must know the **actual context window** of the current provider and model. A new registry is introduced:

```rust
/// Registry of known model context windows (tokens)
pub struct ModelContextRegistry {
    /// Fallback when model is unknown
    pub default_limit: usize,
    /// Provider → Model → Limit
    pub limits: HashMap<String, HashMap<String, usize>>,
}

impl ModelContextRegistry {
    pub fn new() -> Self {
        let mut limits = HashMap::new();

        // minimax
        limits.entry("minimax".to_string())
            .or_insert_with(HashMap::new)
            .insert("M2.7".to_string(), 204_800);

        // kimi
        limits.entry("kimi".to_string())
            .or_insert_with(HashMap::new)
            .insert("K2.6".to_string(), 262_144);

        // openai
        limits.entry("openai".to_string())
            .or_insert_with(HashMap::new)
            .insert("gpt-4o".to_string(), 128_000);

        // ... more providers/models ...

        Self {
            default_limit: 128_000,
            limits,
        }
    }

    pub fn get(&self, provider: &str, model: &str) -> usize {
        self.limits
            .get(provider)
            .and_then(|m| m.get(model))
            .copied()
            .unwrap_or(self.default_limit)
    }
}
```

### Auto-Compaction Trigger

```rust
/// Returns true if compaction should trigger
fn should_compact(&self, estimated_tokens: usize, provider: &str, model: &str) -> bool {
    if !self.config.enabled {
        return false;
    }
    let limit = self.registry.get(provider, model);
    let threshold = (limit * 8) / 10; // 80%
    estimated_tokens >= threshold
}
```

- **No reserve / keep-recent subtraction** in the threshold. The 80 % rule is simpler and model-aware.
- The `CompactionConfig` retains `keep_recent_tokens` as a *minimum* floor for how much recent conversation to preserve, but the primary gate is the 80 % rule.

---

## 3. Dual-File Session Storage

### Problem

Today, session storage uses a single JSONL file. When compaction runs, we want:
- **Immutable user history** — every user message, assistant response, tool result, permanently recorded for audit and resume.
- **Mutable agent context** — the current message list sent to the LLM, which shrinks after compaction.

A single file forces us to either:
- Append compaction metadata but not actually shrink the message list (current broken behavior), or
- Rewrite the entire file to remove compacted messages (expensive, loses history).

### Decision: Two Files Per Session

```
sessions/
└── <session_id>.history.jsonl   ← Immutable. Every event ever recorded.
└── <session_id>.context.jsonl   ← Mutable. Current message list for LLM context.
```

#### `*.history.jsonl`

- Append-only.
- Contains: `SessionCreated`, all `MessageV2` (user, assistant, tool, system), `SystemEvent` (model changes, compaction metadata), etc.
- Never rewritten.
- Source of truth for **resuming a session** and for **audit / replay**.

#### `*.context.jsonl`

- Rewritable.
- Contains only the messages currently in the LLM context window.
- After compaction, this file is **rewritten** to: `[system_prompt, summary_message, …recent_messages]`.
- Loaded at agent-loop start to build the `messages` vector.

### Session API Changes

```rust
impl Session {
    /// Append an event to the immutable history file
    pub async fn append_history(&mut self, event: &SessionEvent) -> Result<()>;

    /// Rewrite the mutable context file
    pub async fn rewrite_context(&self, messages: &[ChatMessage]) -> Result<()>;

    /// Load current context (for LLM)
    pub async fn load_context(&self) -> Result<Vec<ChatMessage>>;

    /// Load full history (for resume / audit)
    pub async fn load_history(&self) -> Result<Vec<ChatMessage>>;
}
```

### Compaction Flow with Dual Files

```
1. Agent loop starts → load_context() reads *.context.jsonl
2. User message arrives → append to *.history.jsonl AND *.context.jsonl
3. LLM responds → append to *.history.jsonl AND *.context.jsonl
4. Token count hits 80 % → trigger compaction
5. Built-in compactor (or extension) produces summary + kept messages
6. rewrite_context() writes new [summary, kept_msgs] to *.context.jsonl
7. append_history() writes a Compaction metadata event to *.history.jsonl
8. Next iteration → load_context() reads the compacted *.context.jsonl
```

### Backward Compatibility

- Existing single-file sessions (`<session_id>.jsonl`) are treated as **history-only**.
- On first open, if no `.context.jsonl` exists, it is generated by copying `.jsonl` (or deriving from it).
- New sessions created after this ADR get both files from the start.

---

## 4. Manual CLI Trigger

A new CLI command allows users to force compaction immediately:

```bash
# Compact the current session for an agent
pekobot session compact --agent <agent_name> [--team <team>]

# Compact a specific session by ID
pekobot session compact --session <session_id>

# Dry-run: show what would be compacted
pekobot session compact --agent <agent_name> --dry-run
```

### Implementation Sketch

```rust
// src/commands/session.rs
pub async fn handle_session_compact(args: CompactArgs) -> Result<()> {
    let session = open_session(&args).await?;
    let messages = session.load_context().await?;
    let estimated = Compactor::estimate_tokens(&messages);

    let provider = session.current_provider().unwrap_or("default");
    let model = session.current_model().unwrap_or("default");
    let limit = model_registry.get(provider, model);

    if args.dry_run {
        println!("Estimated tokens: {estimated} / {limit} ({}%)",
                 (estimated * 100) / limit);
        println!("Would compact {} messages", messages.len());
        return Ok(());
    }

    let mut compactor = Compactor::new();
    let result = compactor.compact(&messages, &provider_arc).await?;

    session.rewrite_context(&result.messages).await?;
    session.record_compaction(/* ... */).await?;

    println!("Compacted {} messages → summary (saved {} tokens)",
             result.entry.messages_compacted,
             result.entry.tokens_before - result.entry.tokens_after);
    Ok(())
}
```

---

## Configuration

`CompactionConfig` moves into `PekobotConfig` and `AgentConfig`:

```toml
# config.toml
[compaction]
enabled = true
auto_threshold_percent = 80        # trigger at 80% of model limit
keep_recent_tokens = 20_000        # minimum recent conversation to preserve
max_compactions_per_session = 100  # from background quota
cooldown_seconds = 60              # from background quota

# Optional: override model limits
[compaction.model_limits]
minimax.M2.7 = 204800
kimi.K2.6 = 262144
openai.gpt-4o = 128000
```

```rust
// src/types/config.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionConfig {
    pub enabled: bool,
    pub auto_threshold_percent: u8,   // default 80
    pub keep_recent_tokens: usize,    // default 20_000
    pub max_compactions_per_session: usize,
    pub cooldown_seconds: u64,
    pub model_limits: HashMap<String, HashMap<String, usize>>,
}
```

---

## File Changes

### New Files

| File | Purpose |
|------|---------|
| `src/compaction/registry.rs` | `ModelContextRegistry` — provider/model limit lookups |
| `src/compaction/hooks.rs` | Helper to invoke `SessionCompaction` / `SessionCompactionPost` hooks |

### Modified Files

| File | Changes |
|------|---------|
| `src/compaction/mod.rs` | Use `ModelContextRegistry`; remove hard-coded defaults; fix `select_messages` test |
| `src/compaction/background.rs` | Accept `ModelContextRegistry`; share state per session |
| `src/engine/agentic_loop.rs` | Invoke `SessionCompaction` hook; use dual-file session APIs; load config from agent |
| `src/session/unified.rs` | Add `rewrite_context()`, `load_context()`, `append_history()`; dual-file support |
| `src/session/jsonl.rs` | Add `SessionStorage::rewrite_context()`, `load_context()`; normalize compaction events |
| `src/types/config.rs` | Add `CompactionConfig` to `PekobotConfig` / `AgentConfig` |
| `src/extensions/core/hook_points.rs` | Add `SessionCompactionPost` hook point |
| `src/commands/session.rs` | Add `session compact` subcommand |
| `config.example.toml` | Add `[compaction]` section |

### Deleted Files

None. Existing compaction code is refactored, not removed.

---

## Migration Path

| Phase | Task | Effort |
|-------|------|--------|
| 1 | Add `ModelContextRegistry` and `CompactionConfig` to config types | 1 day |
| 2 | Implement dual-file session storage (`*.history.jsonl` + `*.context.jsonl`) | 2 days |
| 3 | Wire `SessionCompaction` and `SessionCompactionPost` hooks in agentic loop | 1 day |
| 4 | Update built-in compactor to use registry + config; fix normalization | 1 day |
| 5 | Add `pekobot session compact` CLI command | 0.5 day |
| 6 | Tests: unit tests for dual-file, hook integration, CLI dry-run | 1 day |
| 7 | Documentation: update `DATA_MODEL.md` §5 for dual-file format | 0.5 day |

---

## Consequences

### Positive

- **Pluggable compaction**: Extensions can replace or augment the built-in summarizer (e.g., semantic clustering, RAG-based retrieval, external summarization API).
- **Model-aware triggers**: No more guessing context windows; each provider/model has a known limit.
- **Immutable audit trail**: `*.history.jsonl` preserves every message forever, even after compaction.
- **Fast context loading**: `*.context.jsonl` is always small; no need to parse a giant history file on every LLM call.
- **User control**: Manual compaction via CLI for power users.

### Negative / Risks

| Risk | Mitigation |
|------|------------|
| Dual files may drift (history has N messages, context has M < N) | Document the contract: history is append-only, context is derived. Add integrity check on session open. |
| Backward compatibility for old single-file sessions | Auto-generate `.context.jsonl` on first open. Keep `.jsonl` as history. |
| Extension hooks add latency to every compaction | Hooks are async but run in the same task. Document that custom compaction should be fast or use background tasks. |
| CLI `compact` needs provider/model info from session | Store provider/model in session metadata (already partially done via `record_model_change`). |

---

## Success Criteria

- [ ] `SessionCompaction` hook is invoked and can override built-in compaction.
- [ ] `SessionCompactionPost` hook is invoked and can modify the result.
- [ ] Built-in compactor triggers at 80 % of the **actual** model context limit.
- [ ] New sessions create both `.history.jsonl` and `.context.jsonl`.
- [ ] Old single-file sessions auto-migrate on open.
- [ ] `pekobot session compact --agent <name>` works and rewrites context.
- [ ] All existing tests pass; new tests cover dual-file and hook integration.

---

## References

- ADR-017: Unified Extension Architecture
- ADR-019: Dynamic Tool and Prompt Updates
- `src/compaction/mod.rs` — existing compactor implementation
- `src/compaction/background.rs` — existing background worker
- `src/engine/agentic_loop.rs` — agent loop integration point
- `src/session/unified.rs` — session persistence API
- `src/session/jsonl.rs` — JSONL storage backend
