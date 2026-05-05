# Session Merger Implementation Plan

## Executive Summary

**Status:** 🚧 Ready for Implementation  
**Priority:** Critical (fixes racing issues)  
**Estimated Effort:** 2-3 days  
**Risk:** Medium (requires coordinated changes across engine and session layers)

## 1. Problem Statement

### 1.1 Racing Issues Between BaseSession and SimpleSession

The codebase currently has **two competing session implementations** that both manage the same underlying storage:

| Aspect | BaseSession | SimpleSession |
|--------|-------------|---------------|
| Location | `src/session/base.rs` | `src/engine/simple_session.rs` |
| Lines of Code | ~751 | ~636 |
| Used By | SessionManager, overlays | AgenticLoopV4, engine |
| Peer Support | Yes (required) | No |
| Session Key | Required | Optional |
| Storage Backend | `SessionStorage` + `SessionIndex` | `SessionStorage` + `SessionIndex` |

**The Racing Problem:**

```rust
// From BaseSession::update_index() - line 289-309:
async fn update_index(&mut self) -> Result<()> {
    if let Some(mut entry) = self.index.get(&self.id).await? {
        // ...
        // Only update message_count if we've actually counted messages
        // (don't overwrite SimpleSession's count with 0 for new sessions)
        if self.message_count > 0 {
            entry.message_count = self.message_count;
        }
        // ...
    }
}
```

This defensive check proves **both sessions are competing for the same index entries**.

### 1.2 Root Causes

1. **Dual Index Updates**: Both sessions call `SessionIndex::insert()` independently
2. **Message Count Conflicts**: Both maintain separate `message_count` counters
3. **Token Tracking Duplication**: Both track and update token usage
4. **File Locking Gaps**: No coordination when both access the same JSONL file
5. **Cache Inconsistency**: `SessionIndex` cache can be stale when accessed from different code paths

### 1.3 Impact

- **Data Loss**: Message counts and token usage get overwritten
- **Inconsistent State**: sessions.json has different values depending on which session type wrote last
- **Race Conditions**: Concurrent operations can corrupt session data
- **Maintenance Burden**: Bug fixes need to be applied to both implementations

## 2. Proposed Solution

### 2.1 Clean Slate Approach: UnifiedSession

Create a **single, authoritative session implementation** that replaces both.

```
src/session/
├── unified.rs      # NEW: Merged implementation
├── base.rs         # DELETE after migration
└── ...

src/engine/
├── simple_session.rs  # DELETE after migration
└── ...
```

### 2.2 Design Principles

1. **Single Source of Truth**: One implementation, one file, one API
2. **Backward Compatible**: Existing sessions continue to work
3. **Atomic Operations**: All index updates happen together
4. **Clear Ownership**: Session lifecycle is explicit and traceable
5. **Minimal API Surface**: Keep only what's needed

### 2.3 UnifiedSession Structure

```rust
/// Unified session - single source of truth for conversation persistence
/// 
/// Replaces both BaseSession and SimpleSession to eliminate racing issues.
pub struct UnifiedSession {
    // Identity
    pub id: String,
    pub agent_name: String,
    pub session_key: String,  // Always present, derived from peer or explicit
    pub peer: Peer,           // Always present, defaults to Peer::User("default")
    
    // Storage (private - controlled access)
    storage: SessionStorage,
    index: SessionIndex,
    
    // State (managed internally)
    last_message_id: Option<String>,
    message_count: usize,
    input_tokens: usize,
    output_tokens: usize,
    current_provider: Option<String>,
    current_model: Option<String>,
}
```

### 2.4 Unified API

```rust
impl UnifiedSession {
    // ===== Creation =====
    pub async fn create(agent_name: &str, peer: &Peer) -> Result<Self>;
    pub async fn create_with_key(agent_name: &str, peer: &Peer, session_key: &str) -> Result<Self>;
    pub async fn open(agent_name: &str, peer: &Peer) -> Result<Option<Self>>;
    pub async fn open_by_key(agent_name: &str, session_key: &str) -> Result<Option<Self>>;
    pub async fn open_by_id(agent_name: &str, session_id: &str) -> Result<Option<Self>>;
    pub async fn get_or_create(agent_name: &str, peer: &Peer) -> Result<Self>;
    
    // ===== Messages =====
    pub async fn add_system(&mut self, content: impl Into<String>) -> Result<()>;
    pub async fn add_user(&mut self, content: impl Into<String>) -> Result<()>;
    pub async fn add_assistant(&mut self, content: impl Into<String>, tool_calls: Option<Vec<ToolCall>>) -> Result<()>;
    pub async fn add_tool_result(&mut self, tool_call_id: impl Into<String>, tool_name: impl Into<String>, result: impl Into<String>) -> Result<()>;
    pub async fn add_thinking(&mut self, thinking: impl Into<String>, signature: Option<String>) -> Result<()>;
    
    // ===== History =====
    pub async fn load_history(&self) -> Result<Vec<ChatMessage>>;
    pub async fn get_context_text(&self, limit: usize) -> String;
    
    // ===== Metadata =====
    pub async fn record_usage(&mut self, input: usize, output: usize) -> Result<()>;
    pub async fn set_model(&mut self, provider: &str, model: &str) -> Result<()>;
    pub fn token_usage(&self) -> (usize, usize, usize);
    
    // ===== Compaction =====
    pub async fn record_compaction(&mut self, summary: &str, messages_compacted: usize, tokens_before: usize, tokens_after: usize, compaction_number: usize) -> Result<()>;
    pub async fn load_previous_compaction_summary(&self) -> Result<Option<String>>;
    
    // ===== Storage =====
    pub fn storage_dir(agent_name: &str, team: Option<&str>) -> PathBuf;
}
```

## 3. Implementation Phases

### Phase 1: Create UnifiedSession (Day 1)

**Files:**
- `src/session/unified.rs` - NEW

**Tasks:**
1. Create `UnifiedSession` struct combining best of both
2. Implement creation methods (from BaseSession)
3. Implement message methods (from both, but unified)
4. Implement history loading (from BaseSession - more complete)
5. Implement metadata tracking (atomic updates)

**Key Decisions:**
- Always require `Peer` (SimpleSession uses `Peer::User("default")`)
- Always require `session_key` (derived from peer if not explicit)
- Single atomic `update_index()` method that updates all fields at once
- Remove defensive checks - no need when there's only one writer

### Phase 2: Update SessionManager (Day 1-2)

**Files:**
- `src/session/manager.rs`

**Changes:**
```rust
// BEFORE:
pub struct HybridSession {
    pub base: Arc<RwLock<BaseSession>>,
    pub overlay: OverlayRef,
}

// AFTER:
pub struct HybridSession {
    pub base: Arc<RwLock<UnifiedSession>>,
    pub overlay: OverlayRef,
}
```

Update all `BaseSession` references to `UnifiedSession`.

### Phase 3: Update Engine (Day 2)

**Files:**
- `src/engine/loop_v4.rs`
- `src/engine/mod.rs`

**Changes:**
1. Replace `SimpleSession` with `UnifiedSession` in imports
2. Update `AgenticLoopV4::run_with_resume()` to use `UnifiedSession`
3. Update `AgenticLoopV4::run_loop()` to use `UnifiedSession`

```rust
// BEFORE:
pub async fn run_with_resume(
    &self,
    prompt: &str,
    on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
    existing_session: Option<SimpleSession>,
    history: Option<Vec<ChatMessage>>,
) -> Result<AgenticResult>;

// AFTER:
pub async fn run_with_resume(
    &self,
    prompt: &str,
    on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
    existing_session: Option<UnifiedSession>,
    history: Option<Vec<ChatMessage>>,
) -> Result<AgenticResult>;
```

### Phase 4: Update SessionContext (Day 2)

**Files:**
- `src/session/context.rs`

**Changes:**
Update `SessionContext` to use `UnifiedSession` instead of `BaseSession`.

### Phase 5: Delete Old Implementations (Day 3)

**Files to Delete:**
- `src/session/base.rs`
- `src/engine/simple_session.rs`

**Files to Update:**
- `src/session/mod.rs` - Remove `base` module, add `unified`
- `src/engine/mod.rs` - Remove `simple_session` module and re-export

### Phase 6: Update All Callers (Day 3)

**Search and Replace:**
```bash
# Find all usages
grep -r "BaseSession\|SimpleSession" --include="*.rs" src/

# Files likely needing updates:
# - src/channels/cli.rs
# - src/agent/agent.rs
# - src/api/routes/sessions.rs
# - src/commands/session.rs
# - tests/
```

### Phase 7: Testing & Verification (Day 3)

**Tests:**
1. Unit tests for UnifiedSession
2. Integration tests for concurrent access
3. E2E tests for session persistence
4. Migration tests for existing sessions

**Verification:**
```bash
# Build
cargo build

# Test
cargo test --lib session::
cargo test --lib engine::

# E2E tests
./test_scripts/session/test_session_overlay_e2e.sh
```

## 4. Migration Strategy

### 4.1 Data Compatibility

**Good News:** Both session types use the same storage format:
- Same JSONL file format
- Same `sessions.json` index format
- Same `peers.json` routing format

**Migration Path:**
1. Existing sessions work immediately (same file format)
2. No data migration needed
3. Old sessions can be opened by UnifiedSession

### 4.2 Backward Compatibility

**API Changes:**
- `BaseSession` → `UnifiedSession` (type alias for gradual migration)
- `SimpleSession` → `UnifiedSession` (type alias for gradual migration)

**Temporary Type Aliases (Phase 1-6):**
```rust
// In src/session/mod.rs
pub use unified::UnifiedSession as BaseSession;

// In src/engine/mod.rs  
pub use crate::session::UnifiedSession as SimpleSession;
```

## 5. File Changes Summary

### New Files
| File | Lines | Purpose |
|------|-------|---------|
| `src/session/unified.rs` | ~600 | Merged implementation |

### Deleted Files
| File | Lines | Reason |
|------|-------|--------|
| `src/session/base.rs` | ~751 | Replaced by unified.rs |
| `src/engine/simple_session.rs` | ~636 | Replaced by unified.rs |

### Modified Files
| File | Changes |
|------|---------|
| `src/session/mod.rs` | Add unified module, remove base |
| `src/session/manager.rs` | BaseSession → UnifiedSession |
| `src/session/context.rs` | BaseSession → UnifiedSession |
| `src/engine/mod.rs` | Remove simple_session module |
| `src/engine/loop_v4.rs` | SimpleSession → UnifiedSession |
| `src/channels/cli.rs` | Update imports |
| `src/agent/agent.rs` | Update imports |
| `src/api/routes/sessions.rs` | Update imports |

**Net Change:** ~1,387 lines removed, ~600 lines added = **~787 lines reduced**

## 6. Testing Plan

### 6.1 Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_unified_session_create() {
        // Test creation with peer
    }
    
    #[tokio::test]
    async fn test_unified_session_messages() {
        // Test message operations
    }
    
    #[tokio::test]
    async fn test_unified_session_concurrent_access() {
        // Test no racing when accessing from multiple tasks
    }
    
    #[tokio::test]
    async fn test_unified_session_backward_compat() {
        // Test can open old BaseSession and SimpleSession files
    }
}
```

### 6.2 Integration Tests

```rust
// Test concurrent operations don't race
#[tokio::test]
async fn test_no_racing_on_index_updates() {
    let session = UnifiedSession::create("test", &Peer::User("alice".to_string())).await.unwrap();
    let session_arc = Arc::new(RwLock::new(session));
    
    // Spawn multiple tasks that update the session
    let mut handles = vec![];
    for i in 0..10 {
        let s = session_arc.clone();
        handles.push(tokio::spawn(async move {
            let mut guard = s.write().await;
            guard.add_user(format!("message {}", i)).await.unwrap();
            guard.record_usage(10, 10).await.unwrap();
        }));
    }
    
    for h in handles {
        h.await.unwrap();
    }
    
    // Verify consistent state
    let guard = session_arc.read().await;
    let (input, output, total) = guard.token_usage();
    assert_eq!(input, 100); // 10 * 10
    assert_eq!(output, 100); // 10 * 10
}
```

### 6.3 E2E Tests

- Session creation and persistence
- Message history across restarts
- Token usage tracking
- Concurrent channel access
- Spawn overlay isolation

## 7. Risk Mitigation

| Risk | Mitigation |
|------|------------|
| Breaking existing sessions | Maintain file format compatibility, test with old data |
| Performance regression | Benchmark before/after, optimize hot paths |
| Missing functionality | Comprehensive test coverage, feature parity checklist |
| Concurrent access bugs | Stress tests with multiple concurrent operations |
| Rollback needed | Keep old code in git, can revert if issues found |

## 8. Success Criteria

- [ ] All existing tests pass
- [ ] New unified session tests pass
- [ ] Concurrent access stress test passes
- [ ] E2E session tests pass
- [ ] No `BaseSession` or `SimpleSession` references remain
- [ ] Code coverage maintained or improved
- [ ] Performance benchmarks show no regression
- [ ] Manual testing confirms no racing issues

## 9. Post-Merge Cleanup

After successful merge and testing:

1. Update documentation to reference `UnifiedSession`
2. Remove type aliases after 1 release cycle
3. Archive this plan document
4. Update CHANGELOG.md

---

**Author:** Generated by Kimi Code  
**Date:** 2026-03-20  
**Status:** Ready for Review
