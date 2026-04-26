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

This ADR defines a **complete, production-ready session compaction architecture** that closes all gaps and adds user-requested capabilities: pluggable compaction via extensions, per-provider context limits, single-file session storage with an optional derived context cache, and CLI-triggered manual compaction.

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
2. **Minimal Built-In Compaction** — The default compactor auto-triggers using a dual-threshold (ratio + reserved headroom) based on the actual provider/model context window.
3. **Single-File Session Storage with Optional Derived Cache** — One append-only JSONL file is the source of truth; an optional `.context.cache` file provides fast resume and is explicitly discardable.
4. **Manual CLI Trigger** — Users can force compaction early via `pekobot session compact` with optional custom instructions.

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
| `SessionCompaction` | `HookInput::CompactionPreparation { messages_to_summarize, turn_prefix_messages, is_split_turn, previous_summary, file_ops, estimated_tokens, threshold_tokens, model_context_limit, settings }` | `HookOutput::CompactionResult { summary, first_kept_entry_id, tokens_before, details }` → use directly; skip built-in. `HookResult::PassThrough` → run built-in. `HookResult::Cancel` → abort compaction. | **Pre-compaction override** |
| `SessionCompactionPost` | `HookInput::SessionState { messages, summary, tokens_before, tokens_after }` | `HookOutput::MessageVec(msgs)` → replace final message list. `HookResult::PassThrough` → accept as-is. | **Post-compaction augmentation** |
| `SessionContextBuild` | `HookInput::SessionState { history_entries }` | `HookOutput::MessageVec(msgs)` → replace context. | **Context assembly override** |

### Integration in Agentic Loop

```rust
// Pseudo-code inside run_inner(), before LLM call

let estimated_tokens = Compactor::estimate_context_tokens(&messages);
let limit = model_registry.get(provider, model);
let should_compact = should_auto_compact(estimated_tokens, limit, &config);

if should_compact {
    // 1. Build compaction preparation
    let preparation = build_compaction_preparation(&session_entries, &config);

    // 2. Give extensions first shot
    let hook_result = extension_core
        .invoke_hook_compaction(
            HookPoint::SessionCompaction,
            HookInput::CompactionPreparation { preparation }
        )
        .await;

    let compacted = match hook_result {
        HookResult::CompactionResult(custom) => {
            // Extension provided a custom summary
            apply_custom_compaction(&messages, custom)
        }
        HookResult::Cancel => {
            // Extension cancelled compaction
            messages
        }
        HookResult::PassThrough => {
            // Run built-in background compactor
            background_compactor.request_compaction(preparation, &provider_arc).await?
            // ... await result ...
        }
    };

    // 3. Post-compaction hook
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

Following kimi-cli's proven dual-threshold approach, compaction triggers when **either** condition is met:

```rust
/// Returns true if compaction should trigger
fn should_auto_compact(
    estimated_tokens: usize,
    context_window: usize,
    config: &CompactionConfig,
) -> bool {
    if !config.enabled {
        return false;
    }
    // Ratio-based: catches large models early
    let ratio_threshold = (context_window * config.auto_threshold_percent as usize) / 100;
    // Reserved-based: ensures LLM response headroom
    let reserved_threshold = context_window.saturating_sub(config.reserve_tokens);
    estimated_tokens >= ratio_threshold || estimated_tokens >= reserved_threshold
}
```

- **Ratio threshold**: For very large context models (e.g., 1M tokens), 85% may fire before the reserve-based check.
- **Reserve threshold**: For standard models (e.g., 128K), `context_window - reserve_tokens` (e.g., 128K - 16K = 112K) ensures room for the LLM's response.
- The `CompactionConfig` retains `keep_recent_tokens` as a *minimum* floor for how much recent conversation to preserve during compaction.

### Token Estimation

Following pi-mono's hybrid approach for accuracy:

```rust
pub struct ContextUsageEstimate {
    pub tokens: usize,
    pub usage_tokens: usize,
    pub trailing_tokens: usize,
    pub last_usage_index: Option<usize>,
}

/// Estimate context tokens using the last assistant usage when available,
/// plus char/4 heuristic for trailing messages.
pub fn estimate_context_tokens(messages: &[ChatMessage]) -> ContextUsageEstimate {
    // Find last assistant message with valid usage data
    if let Some((usage, index)) = find_last_assistant_usage(messages) {
        let usage_tokens = calculate_context_tokens(usage);
        let trailing_tokens = messages[index + 1..]
            .iter()
            .map(estimate_tokens)
            .sum();
        ContextUsageEstimate {
            tokens: usage_tokens + trailing_tokens,
            usage_tokens,
            trailing_tokens,
            last_usage_index: Some(index),
        }
    } else {
        // No usage available — fall back to heuristic for all messages
        let estimated = messages.iter().map(estimate_tokens).sum();
        ContextUsageEstimate {
            tokens: estimated,
            usage_tokens: 0,
            trailing_tokens: estimated,
            last_usage_index: None,
        }
    }
}
```

---

## 3. Single-File Session Storage with Optional Derived Cache

### Problem

Today, session storage uses a single JSONL file. When compaction runs, we want:
- **Immutable user history** — every user message, assistant response, tool result, permanently recorded for audit and resume.
- **Fast context loading** — the current message list sent to the LLM, which shrinks after compaction, should load quickly on resume.

A single file forces us to either:
- Append compaction metadata but not actually shrink the message list (current broken behavior), or
- Rewrite the entire file to remove compacted messages (expensive, loses history).

### Decision: Single Source of Truth + Derived Cache

Following pi-mono's single-file append-only model and kimi-cli's cache approach:

```
sessions/
└── <session_id>.jsonl           ← Source of truth. Append-only. Every event ever recorded.
└── <session_id>.context.cache   ← Optional derived cache. Current message list for LLM context.
```

#### `*.jsonl` (Source of Truth)

- **Append-only.** Never rewritten.
- Contains: `SessionCreated`, all `MessageV2` (user, assistant, tool, system), `SystemEvent` (model changes, compaction metadata), etc.
- Source of truth for **resuming a session** and for **audit / replay**.
- On session load, `build_context()` walks the entries and applies compaction entries in-memory to produce the current LLM context.

#### `*.context.cache` (Derived, Discardable)

- **Rewritable.** Can be deleted at any time and rebuilt from `*.jsonl`.
- Contains only the messages currently in the LLM context window.
- After compaction, this file is **rewritten** to: `[system_prompt, summary_message, …recent_messages]`.
- On resume, if the cache exists and is valid (matching checksum/sequence from `*.jsonl`), it is loaded directly for fast startup.
- If stale or missing, it is transparently rebuilt from `*.jsonl`.

### Session API Changes

```rust
impl Session {
    /// Append an event to the immutable source-of-truth file
    pub async fn append_event(&mut self, event: &SessionEvent) -> Result<()>;

    /// Build current LLM context from source-of-truth entries.
    /// Applies compaction entries, branch summaries, etc. in-memory.
    /// Called once at session load; result is kept in memory for the run.
    pub async fn build_context(&self) -> Result<Vec<ChatMessage>>;

    /// Load current context via derived cache for fast resume.
    /// Falls back to build_context() if cache is stale or missing.
    pub async fn load_context_fast(&self) -> Result<Vec<ChatMessage>>;

    /// Rewrite the derived cache after compaction
    pub async fn update_context_cache(&self, messages: &[ChatMessage]) -> Result<()>;

    /// Load full history (for audit / replay)
    pub async fn load_history(&self) -> Result<Vec<SessionEvent>>;
}
```

### Context Building with Compaction Entries

Following pi-mono's tree-walk approach:

```rust
/// Build the session context from entries.
/// When a CompactionEntry is encountered, emits the summary first,
/// then only messages from first_kept_entry_id onward.
pub fn build_context(entries: &[SessionEntry]) -> Vec<ChatMessage> {
    let mut messages = Vec::new();
    let mut compaction: Option<&CompactionEntry> = None;

    // Find the latest compaction entry
    for entry in entries.iter().rev() {
        if let SessionEntry::Compaction(c) = entry {
            compaction = Some(c);
            break;
        }
    }

    if let Some(comp) = compaction {
        // Emit summary as a system/compaction message
        messages.push(ChatMessage::System {
            content: format_summary_with_file_ops(&comp.summary, &comp.details),
        });

        // Emit kept messages (from first_kept_entry_id to compaction)
        let mut found_first = false;
        for entry in entries {
            if entry.id() == comp.first_kept_entry_id {
                found_first = true;
            }
            if found_first && entry.id() != comp.id {
                if let Some(msg) = entry.to_chat_message() {
                    messages.push(msg);
                }
            }
        }

        // Emit messages after compaction
        let compaction_passed = entries.iter()
            .position(|e| e.id() == comp.id)
            .map(|i| &entries[i + 1..])
            .unwrap_or(&[]);
        for entry in compaction_passed {
            if let Some(msg) = entry.to_chat_message() {
                messages.push(msg);
            }
        }
    } else {
        // No compaction — emit all messages
        for entry in entries {
            if let Some(msg) = entry.to_chat_message() {
                messages.push(msg);
            }
        }
    }

    messages
}
```

### Compaction Flow

```
1. Session starts → load_context_fast() checks cache validity
   - Cache valid → load cache directly
   - Cache stale/missing → build_context() from *.jsonl, then write cache

2. Within the run, messages are kept in memory (_history: Vec<ChatMessage>)

3. User message arrives → append to *.jsonl AND in-memory _history

4. LLM responds → append to *.jsonl AND in-memory _history

5. Token count hits threshold → trigger compaction

6. Built-in compactor (or extension) produces summary + kept messages

7. update_context_cache() writes new [summary, kept_msgs] to *.context.cache

8. Append CompactionEntry to *.jsonl (source of truth)

9. Next iteration → in-memory _history already compacted; on next resume,
   load_context_fast() will use the updated cache or rebuild from *.jsonl
```

### Backward Compatibility

- Existing single-file sessions (`<session_id>.jsonl`) are treated as **source-of-truth** files.
- On first open, if no `.context.cache` exists, it is generated by calling `build_context()` on `*.jsonl`.
- New sessions created after this ADR get both files from the start.

---

## 4. Message Selection and Turn Boundaries

Following pi-mono's proven approach for preserving coherent conversation structure:

### Cut Point Rules

Valid cut points (where compaction may split history from kept messages):
- User messages
- Assistant messages
- Bash execution messages
- Custom messages

**Never cut at tool results** — they must stay paired with their tool call.

### Split Turn Handling

When a single turn exceeds `keep_recent_tokens`, the cut point may land mid-turn at an assistant message. This is a "split turn":

```
Before compaction:

  entry:  0     1     2      3     4      5      6     7      8
        ┌─────┬─────┬─────┬──────┬─────┬──────┬──────┬─────┬──────┐
        │ hdr │ usr │ ass │ tool │ ass │ tool │ tool │ ass │ tool │
        └─────┴─────┴─────┴──────┴─────┴──────┴──────┴─────┴──────┘
                ↑                                     ↑
         turn_start = 1                    first_kept = 7
                │                                     │
                └──── turn_prefix (1-6) ──────────────┘     kept (7-8)

  is_split_turn = true
  messages_to_summarize = []  (no complete turns before)
  turn_prefix_messages = [usr, ass, tool, ass, tool, tool]
```

For split turns, generate **two summaries** and merge them:
1. **History summary**: Previous context (if any)
2. **Turn prefix summary**: The early part of the split turn

```rust
pub async fn compact(preparation: &CompactionPreparation, ...) -> CompactionResult {
    let summary = if preparation.is_split_turn && !preparation.turn_prefix_messages.is_empty() {
        let (history, prefix) = tokio::join!(
            generate_summary(&preparation.messages_to_summarize, ...),
            generate_turn_prefix_summary(&preparation.turn_prefix_messages, ...),
        );
        format!("{}\n\n---\n\n**Turn Context (split turn):**\n\n{}", history, prefix)
    } else {
        generate_summary(&preparation.messages_to_summarize, ...).await
    };

    // Append cumulative file operations
    let (read_files, modified_files) = compute_file_lists(&preparation.file_ops);
    summary += format_file_operations(read_files, modified_files);

    CompactionResult {
        summary,
        first_kept_entry_id: preparation.first_kept_entry_id,
        tokens_before: preparation.tokens_before,
        details: CompactionDetails { read_files, modified_files },
    }
}
```

---

## 5. Structured Summary Format

The built-in compactor uses a structured summary format (inspired by pi-mono) that LLMs can continue from effectively:

```markdown
## Goal
[What the user is trying to accomplish]

## Constraints & Preferences
- [Requirements mentioned by user]

## Progress
### Done
- [x] [Completed tasks]

### In Progress
- [ ] [Current work]

### Blocked
- [Issues, if any]

## Key Decisions
- **[Decision]**: [Rationale]

## Next Steps
1. [What should happen next]

## Critical Context
- [Data needed to continue]

<read-files>
path/to/file1.rs
path/to/file2.rs
</read-files>

<modified-files>
path/to/changed.rs
</modified-files>
```

### Iterative Summary Updates

When a previous compaction summary exists, use an update prompt instead of generating from scratch:

```
Update the existing structured summary with new information. RULES:
- PRESERVE all existing information from the previous summary
- ADD new progress, decisions, and context from the new messages
- UPDATE the Progress section: move items from "In Progress" to "Done" when completed
- UPDATE "Next Steps" based on what was accomplished
- PRESERVE exact file paths, function names, and error messages
```

### File Operation Tracking

Cumulative file operations are tracked across compactions in `CompactionEntry.details`:

```rust
pub struct CompactionDetails {
    pub read_files: Vec<String>,
    pub modified_files: Vec<String>,
}
```

When generating a summary, extract file operations from:
- Tool calls in the messages being summarized
- Previous compaction's `details` (if pi-generated, i.e., `from_hook == false`)

---

## 6. Manual CLI Trigger

A new CLI command allows users to force compaction immediately with optional custom instructions:

```bash
# Compact the current session for an agent
pekobot session compact --agent <agent_name> [--team <team>]

# Compact a specific session by ID
pekobot session compact --session <session_id>

# Dry-run: show what would be compacted
pekobot session compact --agent <agent_name> --dry-run

# Compact with custom instructions (focus the summary)
pekobot session compact --agent <agent_name> --instruction "preserve all API design decisions"
```

### Implementation Sketch

```rust
// src/commands/session.rs
pub async fn handle_session_compact(args: CompactArgs) -> Result<()> {
    let session = open_session(&args).await?;
    let messages = session.load_context_fast().await?;
    let estimated = Compactor::estimate_context_tokens(&messages);

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
    let preparation = build_compaction_preparation(&session.entries, &config)?;
    let result = compactor.compact(&preparation, &provider_arc, args.instruction.as_deref()).await?;

    session.update_context_cache(&result.messages).await?;
    session.append_event(&SessionEvent::Compaction(result.entry)).await?;

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
auto_threshold_percent = 85       # trigger at 85% of model limit
reserve_tokens = 16384            # tokens to reserve for LLM response
keep_recent_tokens = 20_000       # minimum recent conversation to preserve
max_compactions_per_session = 100 # from background quota
cooldown_seconds = 60             # from background quota

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
    pub auto_threshold_percent: u8,   // default 85
    pub reserve_tokens: usize,        // default 16_384
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
| `src/compaction/turn_boundaries.rs` | Cut point detection, split-turn handling |
| `src/compaction/summary_format.rs` | Structured summary format, file operation tracking |

### Modified Files

| File | Changes |
|------|---------|
| `src/compaction/mod.rs` | Use `ModelContextRegistry`; hybrid token estimation; structured summaries; fix `select_messages` test |
| `src/compaction/background.rs` | Accept `ModelContextRegistry`; share state per session; dual-threshold trigger |
| `src/engine/agentic_loop.rs` | Invoke `SessionCompaction` hook; use single-file + cache APIs; load config from agent |
| `src/session/unified.rs` | Add `build_context()`, `load_context_fast()`, `update_context_cache()`, `append_event()`; single-file + cache support |
| `src/session/jsonl.rs` | Add cache read/write with checksum validation; normalize compaction events |
| `src/types/config.rs` | Add `CompactionConfig` to `PekobotConfig` / `AgentConfig` |
| `src/extensions/core/hook_points.rs` | Add `SessionCompactionPost` hook point; enrich `HookInput::CompactionPreparation` |
| `src/commands/session.rs` | Add `session compact` subcommand with `--instruction` support |
| `config.example.toml` | Add `[compaction]` section |

### Deleted Files

None. Existing compaction code is refactored, not removed.

---

## Migration Path

| Phase | Task | Effort |
|-------|------|--------|
| 1 | Add `ModelContextRegistry` and `CompactionConfig` to config types | 1 day |
| 2 | Implement single-file + derived cache storage (`*.jsonl` + `*.context.cache`) | 2 days |
| 3 | Wire `SessionCompaction` and `SessionCompactionPost` hooks in agentic loop | 1 day |
| 4 | Update built-in compactor: dual-threshold trigger, hybrid estimation, structured summaries, turn boundaries | 2 days |
| 5 | Add `pekobot session compact` CLI command with `--instruction` | 0.5 day |
| 6 | Tests: unit tests for context building, turn boundaries, hook integration, CLI dry-run | 1.5 day |
| 7 | Documentation: update `DATA_MODEL.md` for single-file + cache format | 0.5 day |

---

## Consequences

### Positive

- **Pluggable compaction**: Extensions can replace or augment the built-in summarizer (e.g., semantic clustering, RAG-based retrieval, external summarization API). Hooks can also cancel compaction.
- **Model-aware dual-threshold triggers**: Ratio threshold catches large models early; reserve threshold ensures response headroom for all models.
- **Immutable audit trail**: `*.jsonl` preserves every message forever, even after compaction.
- **Fast context loading**: `*.context.cache` provides fast resume; can be rebuilt from source of truth at any time.
- **No drift risk**: Cache is explicitly derived and discardable; source of truth is a single append-only file.
- **Structured summaries**: Proven format with goal, progress, decisions, file operations — LLMs continue work more effectively.
- **Turn-boundary preservation**: Never cuts mid-tool-call; split-turn handling with dual summaries prevents context loss.
- **User control**: Manual compaction via CLI with custom instructions for power users.
- **Future branching ready**: Single-file tree structure (id/parentId) enables `/fork` and branch summarization.

### Negative / Risks

| Risk | Mitigation |
|------|------------|
| Cache may become stale if `*.jsonl` is modified externally | Include checksum/sequence number in cache header; validate on load; rebuild if mismatch |
| Backward compatibility for old single-file sessions | Auto-generate `.context.cache` on first open. Keep `.jsonl` as source of truth |
| Extension hooks add latency to every compaction | Hooks are async but run in the same task. Document that custom compaction should be fast or use background tasks |
| CLI `compact` needs provider/model info from session | Store provider/model in session metadata (already partially done via `record_model_change`) |
| First resume without cache parses full history | Acceptable for typical sessions; cache is written after first load |

---

## Success Criteria

- [ ] `SessionCompaction` hook is invoked and can override built-in compaction.
- [ ] `SessionCompaction` hook can cancel compaction via `HookResult::Cancel`.
- [ ] `SessionCompactionPost` hook is invoked and can modify the result.
- [ ] Built-in compactor triggers using dual-threshold (ratio OR reserved headroom) with actual model context limit.
- [ ] New sessions create `*.jsonl` source of truth and `*.context.cache` derived cache.
- [ ] Old single-file sessions auto-migrate on open (cache generated from `*.jsonl`).
- [ ] `pekobot session compact --agent <name>` works and rewrites cache.
- [ ] `pekobot session compact --instruction "..."` passes custom focus to summarizer.
- [ ] Turn boundaries are respected: never cuts at tool results.
- [ ] Split-turn scenario produces merged history + turn-prefix summary.
- [ ] Structured summary format includes Goal, Progress, Decisions, Next Steps, File Operations.
- [ ] All existing tests pass; new tests cover context building, turn boundaries, hook integration, and cache validation.

---

## References

- ADR-017: Unified Extension Architecture
- ADR-019: Dynamic Tool and Prompt Updates
- `src/compaction/mod.rs` — existing compactor implementation
- `src/compaction/background.rs` — existing background worker
- `src/engine/agentic_loop.rs` — agent loop integration point
- `src/session/unified.rs` — session persistence API
- `src/session/jsonl.rs` — JSONL storage backend
- pi-mono: `packages/coding-agent/src/core/compaction/compaction.ts` — turn boundaries, split-turn handling, structured summaries
- pi-mono: `packages/coding-agent/src/core/session-manager.ts` — single-file tree storage, `buildSessionContext()`
- kimi-cli: `src/kimi_cli/soul/compaction.py` — dual-threshold trigger, protocol-based compaction
- kimi-cli: `src/kimi_cli/soul/context.py` — derived cache pattern with `clear()` and rewrite
