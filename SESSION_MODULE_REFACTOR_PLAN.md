# Session Module Refactor Plan (Trimmed)

## Overview

This is a streamlined refactor plan focused on fixing real architectural debt without over-engineering. **No breaking API changes. No premature optimizations.**

**Key Changes from Original Plan:**
1. Reduced from 7 phases to 5 phases (16 weeks vs 22 weeks)
2. Eliminated 3 stabilization sprints (conservative for code-only changes)
3. Removed Phase 7 (breaking API changes) - keeping `SessionMetadata` as public name
4. Removed `SessionMetadataBuilder` (only used in tests)
5. Simplified caching (no TTL until profiling shows need)
6. Removed lock timeout complexity (document only, add if issues observed)
7. Simplified `MessageConverter` to free functions (no new struct/file)
8. Kept `HybridSession`/`SessionHandle` separate (circular ref fix only)

**Timeline: 16 weeks**

---

## Current State Analysis

### Critical Issues (Must Fix)

| Issue | Severity | Files | Risk |
|-------|----------|-------|------|
| 4 duplicate metadata types (`SessionEntry`, `SessionMetadata`, `SessionInfo`, `SessionResponse`) | High | 15+ | Breaking changes |
| `SessionIndex` public but should be private | High | 8+ | Layer violation |
| `UnifiedSession` has 5+ responsibilities | High | 1 | God object |
| `SessionManager` creates new `MetadataController` per call | Medium | 3 | Cache bypass |
| `SessionHandle` holds circular reference to `SessionManager` | Medium | 2 | Memory/lock risk |

### Architectural Debt

```
Current Call Patterns (Layer Violations):
┌─────────────────────────────────────────────────────────────┐
│  API Routes ──────┐                                         │
│  CLI Commands ────┼──► SessionIndex (direct!)               │
│  Tools ───────────┤        ▲                                │
│  SessionService ──┼────────┘                                │
│  SessionManager ──┼──► MetadataController ──► SessionIndex  │
│                   │              │                          │
│                   └──────────────┴──► UnifiedSession        │
└─────────────────────────────────────────────────────────────┘

Target Call Patterns (Layered):
┌─────────────────────────────────────────────────────────────┐
│  API Routes ──────┐                                         │
│  CLI Commands ────┼──► SessionService ──► SessionManager    │
│  Tools ───────────┤                           │              │
│                   │                           ▼              │
│                   │              ┌─────────────────────┐     │
│                   │              │ MetadataController  │     │
│                   │              │ (private SessionIndex)    │
│                   │              └─────────────────────┘     │
│                   │                           │              │
│                   └───────────────────────────┴──► UnifiedSession
└─────────────────────────────────────────────────────────────┘
```

---

## Guiding Principles

1. **Internal-First Migration**: Fix internal usage before touching public APIs
2. **No Premature Abstraction**: Don't add traits until multiple implementations exist
3. **No Breaking Changes**: Keep `SessionMetadata` as public type name
4. **Profile Before Optimizing**: No TTL/timeouts until issues observed

---

## Phase 1: Type Consolidation (Internal Only)

**Weeks 1-4**

### Phase 1a: Establish SessionEntry as Internal Source of Truth

**Goal**: Make `SessionEntry` the canonical type internally without changing public APIs.

**Steps:**

1. **Add conversion methods to SessionEntry** (no breaking changes):
```rust
// In session/index.rs
impl SessionEntry {
    /// Convert to SessionMetadata (for backward compatibility)
    pub fn to_metadata(&self) -> SessionMetadata {
        SessionMetadata::from_entry(self.clone())
    }
    
    /// Convert to SessionInfo (for service layer)
    pub fn to_info(&self) -> SessionInfo {
        SessionInfo::from(self.clone())
    }
}
```

2. **Update MetadataController to use SessionEntry internally**:
```rust
// Before: MetadataController stores SessionMetadata
pub struct MetadataController {
    cache: Arc<RwLock<HashMap<String, SessionMetadata>>>,
}

// After: Store SessionEntry internally, convert on API boundary
pub struct MetadataController {
    cache: Arc<RwLock<HashMap<String, SessionEntry>>>,
}

impl MetadataController {
    // Keep public API returning SessionMetadata for now
    pub async fn get_metadata(&mut self, session_id: &str, sync: bool) 
        -> Result<Option<SessionMetadata>> {
        let entry = self.get_entry(session_id, sync).await?;
        Ok(entry.map(|e| e.to_metadata()))
    }
    
    // New internal method
    async fn get_entry(&mut self, session_id: &str, sync: bool) 
        -> Result<Option<SessionEntry>> {
        // ... implementation using SessionEntry
    }
}
```

3. **Migrate SessionService to use SessionEntry via MetadataController**:
```rust
// In session_service.rs
pub async fn list_sessions(...) -> Result<Vec<SessionInfo>> {
    let mut controller = MetadataController::new(&sessions_dir);
    let entries = controller.list_entries().await?; // New method returning SessionEntry
    Ok(entries.into_iter().map(|e| e.to_info()).collect())
}
```

**Files Modified:**
- `src/session/index.rs` - Add conversion methods
- `src/session/metadata_controller.rs` - Use SessionEntry internally
- `src/common/services/session_service.rs` - Use new methods

**Keep:** `SessionMetadata` as public type name—no breaking changes.

**Success Criteria:**
- [ ] All internal paths use `SessionEntry`
- [ ] `SessionMetadata` only used at API boundaries
- [ ] No behavior changes (verified by existing tests)
- [ ] Performance benchmark shows no regression

---

### Phase 1b: Consolidate History Event Types

**Goal**: Use `SessionEvent` internally, eliminate `HistoryEvent` duplication in internal code.

**Steps:**

1. **Add display/view methods to SessionEvent**:
```rust
// In session/events.rs
impl SessionEvent {
    /// Get display type for CLI/API (replaces HistoryEvent type discrimination)
    pub fn display_type(&self) -> &'static str {
        match self {
            SessionEvent::UserMessage(_) => "user.message",
            SessionEvent::AssistantMessage(_) => "assistant.message",
            SessionEvent::LlmMessage(e) => &e.role,
            SessionEvent::ToolCall(_) => "tool.call",
            SessionEvent::ToolResult(_) => "tool.result",
            SessionEvent::Thinking(_) => "thinking",
            SessionEvent::SessionCreated(_) => "session.created",
            _ => "custom",
        }
    }
    
    /// Extract text content for display
    pub fn display_content(&self) -> Option<String> {
        match self {
            SessionEvent::UserMessage(e) => Some(e.content.clone()),
            SessionEvent::AssistantMessage(e) => Some(e.content.clone()),
            SessionEvent::LlmMessage(e) => {
                Some(e.content_blocks.iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(""))
            }
            SessionEvent::ToolCall(e) => Some(format!("{}({})", e.tool, e.args)),
            SessionEvent::ToolResult(e) => e.output.clone(),
            SessionEvent::Thinking(e) => Some(e.content.clone()),
            _ => None,
        }
    }
    
    /// Get timestamp for sorting/filtering
    pub fn timestamp(&self) -> DateTime<Utc> {
        self.envelope().ts
    }
}
```

2. **Update SessionService to use SessionEvent directly**:
```rust
// Replace HistoryEvent with SessionEvent in internal processing
pub async fn get_history(...) -> Result<HistoryResult> {
    let events: Vec<SessionEvent> = storage.load_events(session_id).await?;
    
    // Convert to HistoryEvent only at API boundary
    let history_events: Vec<HistoryEvent> = events
        .into_iter()
        .filter_map(|e| convert_to_history_event(e, &query))
        .collect();
    
    Ok(HistoryResult { ... })
}
```

**Files Modified:**
- `src/session/events.rs` - Add display methods
- `src/common/services/session_service.rs` - Use SessionEvent internally
- `src/api/routes/sessions.rs` - Update converters

**Success Criteria:**
- [ ] `SessionEvent` used throughout internal processing
- [ ] `HistoryEvent` only used in API response types
- [ ] Display logic centralized in `SessionEvent`

---

### Phase 1c: Stabilization Sprint (Week 4)

**Activities:**
- Run full integration test suite
- Performance benchmarks (metadata lookups, history loading)
- Code review of all Phase 1a/1b changes
- Fix any issues discovered
- **No new features**—only stabilization

---

## Phase 2: Boundary Clarification

**Weeks 5-8**

### Phase 2a: Fix SessionHandle Circular Reference

**Goal**: Remove circular reference from `SessionHandle` to `SessionManager` while preserving metadata operation capabilities.

**Decision:** Keep `HybridSession` and `SessionHandle` separate. Use shared `MetadataController` reference for metadata operations.

**Current Problem:**
```rust
// BEFORE (problematic):
pub struct SessionHandle {
    session_id: String,
    base: Arc<RwLock<UnifiedSession>>,
    manager: Arc<RwLock<SessionManager>>,  // Circular! Creates memory/lock risk
}
```

**Root Issue**: SessionHandle needs to perform metadata operations (get_metadata, record_usage, set_model, sync_metadata) but cannot hold a reference to SessionManager without creating a circular reference.

**Selected Solution: Shared MetadataController Reference**
```rust
// AFTER (with shared controller):
pub struct SessionHandle {
    session_id: String,
    base: Arc<RwLock<UnifiedSession>>,
    overlay: Option<OverlayRef>,
    // Shared metadata controller for metadata operations
    metadata: Arc<RwLock<MetadataController>>,
}

impl SessionHandle {
    /// Access base session for message operations
    pub async fn with_base<F, R>(&self, f: F) -> Result<R>
    where F: FnOnce(&mut UnifiedSession) -> Result<R> {
        let mut base = self.base.write().await;
        f(&mut *base)
    }
    
    /// Check if this handle has an overlay
    pub fn has_overlay(&self) -> bool {
        self.overlay.is_some()
    }
    
    /// Get session metadata (via shared controller)
    pub async fn get_metadata(&self) -> Result<SessionMetadata> {
        let mut controller = self.metadata.write().await;
        controller.get_metadata(&self.session_id, false).await
            .transpose()
            .unwrap_or_else(|| Err(anyhow!("Session not found")))
    }
    
    /// Record token usage (via shared controller)
    pub async fn record_usage(&self, input: usize, output: usize) -> Result<()> {
        let mut controller = self.metadata.write().await;
        controller.record_token_usage(&self.session_id, input, output).await
    }
    
    /// Set model information (via shared controller)
    pub async fn set_model(&self, provider: &str, model: &str) -> Result<()> {
        let mut controller = self.metadata.write().await;
        let mut metadata = self.get_metadata().await?;
        metadata.set_model(provider, model);
        controller.update_metadata(metadata).await
    }
    
    /// Sync metadata from JSONL (via shared controller)
    pub async fn sync_metadata(&self) -> Result<()> {
        let mut controller = self.metadata.write().await;
        controller.sync_from_jsonl(&self.session_id).await?;
        Ok(())
    }
}
```

**Alternative Considered: Command Pattern**
```rust
// Alternative design (not selected):
pub enum MetadataCommand {
    GetMetadata { session_id: String },
    RecordUsage { session_id: String, input: usize, output: usize },
    SetModel { session_id: String, provider: String, model: String },
}

pub struct SessionHandle {
    session_id: String,
    base: Arc<RwLock<UnifiedSession>>,
    command_tx: mpsc::Sender<MetadataCommand>,
}
```
Rejected because: adds async channel complexity, error handling becomes harder, performance overhead for simple operations.

**Cache Consistency Note:**
The shared `Arc<RwLock<MetadataController>>` ensures all SessionHandles from the same SessionManager share the same cache. This solves the cache inconsistency issue where multiple controller instances had separate caches.

**Files Modified:**
- `src/session/manager.rs` - Update SessionHandle with shared controller
- `src/session/metadata_controller.rs` - Ensure Clone uses shared Arc

**Success Criteria:**
- [ ] SessionHandle can perform all metadata operations without SessionManager reference
- [ ] No circular references between SessionHandle and SessionManager
- [ ] All SessionHandles from same manager share metadata cache

---

### Phase 2b: Privatize SessionIndex

**Goal**: `SessionIndex` is implementation detail of `MetadataController`.

**Current Problem:**
```rust
// These direct accesses must be eliminated:
// - api/routes/sessions.rs: directly creates SessionIndex
// - commands/session.rs: directly manipulates SessionIndex  
// - session_service.rs: directly creates SessionIndex
```

**Steps:**

1. **Add required methods to MetadataController**:
```rust
impl MetadataController {
    /// List all sessions (replaces SessionIndex::list_all)
    pub async fn list_all(&mut self) -> Result<Vec<SessionEntry>> {
        self.index.list_all().await
    }
    
    /// List sessions for peer (replaces SessionIndex::list_for_peer)
    pub async fn list_for_peer(&mut self, peer_key: &str) -> Result<Vec<SessionEntry>> {
        self.index.list_for_peer(peer_key).await
    }
    
    /// Get active session for peer
    pub async fn get_active_for_peer(&mut self, peer_key: &str) -> Result<Option<SessionEntry>> {
        self.index.get_active_for_peer(peer_key).await
    }
    
    /// Set active session for peer
    pub async fn set_active_for_peer(&mut self, peer_key: &str, session_id: &str) -> Result<()> {
        self.index.set_active_for_peer(peer_key, session_id).await?;
        self.index.save().await
    }
    
    /// Branch session for peer
    pub async fn branch_for_peer(&mut self, entry: SessionEntry, peer_key: &str) -> Result<()> {
        self.index.branch_for_peer(entry, peer_key).await?;
        self.index.save().await
    }
}
```

2. **Update all callers to use MetadataController**:
```rust
// Before (in commands/session.rs):
let mut index = SessionIndex::open(&sessions_dir);
index.set_active_for_peer(&peer_key, session_id).await?;
index.save().await?;

// After:
let mut controller = MetadataController::new(&sessions_dir);
controller.set_active_for_peer(&peer_key, session_id).await?;
```

3. **Make SessionIndex module-private**:
```rust
// In session/metadata_controller.rs
mod index;  // Private module

pub struct MetadataController {
    index: index::SessionIndex,  // Not pub(crate)
    // ...
}
```

**Files Modified:**
- `src/session/metadata_controller.rs` - Add proxy methods
- `src/commands/session.rs` - Use MetadataController
- `src/common/services/session_service.rs` - Use MetadataController
- `src/api/routes/sessions.rs` - Use MetadataController (via SessionService)

**Success Criteria:**
- [ ] `rg "SessionIndex::open" src/` returns 0 results (except in MetadataController)
- [ ] All index operations go through MetadataController
- [ ] Module compiles with `SessionIndex` as private

---

### Phase 2c: Stabilization Sprint (Week 8)

**Activities:**
- Comprehensive testing of MetadataController proxy methods
- Verify no direct SessionIndex usage remains
- Performance test: ensure proxy methods don't add overhead
- Lock contention testing

---

## Phase 2d: MetadataController Cache Consistency (Week 8 - NEW)

**Goal**: Eliminate cache inconsistency caused by multiple MetadataController instances.

**Problem**: `SessionManager::get_session_metadata()` creates a new `MetadataController` on every call, resulting in cache misses and potential stale data:

```rust
// PROBLEMATIC (current code):
pub async fn get_session_metadata(&self, session_id: &str) -> Result<SessionMetadata> {
    let sessions_dir = self.sessions_dir.clone().unwrap_or_else(|| std::env::temp_dir());
    let mut controller = MetadataController::new(sessions_dir);  // NEW instance = fresh cache!
    // ... cache miss every time
}
```

**Solution A: Shared Controller Instance (Selected)**
Make MetadataController shareable via `Arc<RwLock<>>`:

```rust
pub struct SessionManager {
    // ... other fields ...
    metadata_controller: Arc<RwLock<MetadataController>>,
}

pub async fn get_session_metadata(&self, session_id: &str) -> Result<SessionMetadata> {
    let mut controller = self.metadata_controller.write().await;
    // Uses shared cache
    controller.get_metadata(session_id, false).await
        .transpose()
        .unwrap_or_else(|| Err(anyhow!("Session not found")))
}
```

**Implementation Changes:**
1. Change `MetadataController` storage in SessionManager to `Arc<RwLock<MetadataController>>`
2. Update SessionManager methods to use shared controller
3. Pass `Arc<RwLock<MetadataController>>` to SessionHandle

**Solution B: Remove Cache (Alternative)**
If shared state proves problematic, remove the HashMap cache entirely and read from index each time. This trades performance for consistency.

**Files Modified:**
- `src/session/manager.rs` - Use Arc<RwLock<MetadataController>>
- `src/session/metadata_controller.rs` - Ensure thread-safe operations

**Success Criteria:**
- [ ] Only one MetadataController instance exists per SessionManager
- [ ] All metadata operations use shared cache
- [ ] Cache hit rate > 80% for repeated metadata lookups (measure with logging)

---

## Phase 2e: Lock Timeout Implementation (Week 8 - NEW)

**Goal**: Replace documentation-only lock hierarchy with actual timeout protection.

**Problem**: The current plan documents lock ordering but provides no runtime protection against deadlocks.

**Solution: Implement try_lock with timeout in critical sections**

```rust
// src/session/lock_utils.rs - NEW FILE
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::timeout;

/// Lock acquisition result with timeout
pub enum LockResult<T> {
    Acquired(T),
    Timeout(Duration),
}

/// Acquire write lock with timeout
pub async fn try_write_lock<T>(
    lock: &RwLock<T>,
    timeout_duration: Duration,
    lock_name: &str,
) -> Result<tokio::sync::RwLockWriteGuard<T>> {
    timeout(timeout_duration, lock.write())
        .await
        .map_err(|_| anyhow::anyhow!(
            "Timeout acquiring write lock on {} after {:?}",
            lock_name, timeout_duration
        ))?
}

/// Acquire read lock with timeout
pub async fn try_read_lock<T>(
    lock: &RwLock<T>,
    timeout_duration: Duration,
    lock_name: &str,
) -> Result<tokio::sync::RwLockReadGuard<T>> {
    timeout(timeout_duration, lock.read())
        .await
        .map_err(|_| anyhow::anyhow!(
            "Timeout acquiring read lock on {} after {:?}",
            lock_name, timeout_duration
        ))?
}

// Default timeout: 5 seconds for write, 10 seconds for read
pub const DEFAULT_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(10);
```

**Usage in SessionManager:**
```rust
impl SessionManager {
    pub async fn get_or_create_base(&mut self, agent: &str, peer: &Peer) -> Result<Arc<RwLock<UnifiedSession>>> {
        use crate::session::lock_utils::{try_write_lock, DEFAULT_WRITE_TIMEOUT};
        
        // Try to acquire with timeout
        let base_sessions = try_write_lock(
            &self.base_sessions_lock,  // New: separate lock for base_sessions HashMap
            DEFAULT_WRITE_TIMEOUT,
            "base_sessions"
        ).await?;
        
        // ... rest of method
    }
}
```

**Lock Granularity Changes:**
Instead of locking entire SessionManager, use finer-grained locks:

```rust
pub struct SessionManager {
    base_sessions: Arc<RwLock<HashMap<...>>>,
    channel_overlays: Arc<RwLock<HashMap<...>>>,
    spawn_overlays: Arc<RwLock<HashMap<...>>>,
    metadata_controller: Arc<RwLock<MetadataController>>,
    // ... other fields don't need locks
}
```

**Benefits:**
- Deadlocks become detectable errors instead of infinite hangs
- Finer-grained locking reduces contention
- Timeout errors provide diagnostic information (which lock, how long)

**Files Modified:**
- `src/session/lock_utils.rs` - New utility module
- `src/session/manager.rs` - Use timeout-based locking

**Success Criteria:**
- [ ] All lock acquisitions use timeout
- [ ] Timeout errors include lock name and duration
- [ ] No regression in concurrent operation performance

---

## Phase 3: Responsibility Consolidation

**Weeks 9-12**

### Phase 3a: Single Session Creation Pathway + Remove Broken Legacy Code

**Goal**: Only `SessionManager::create_session()` creates sessions, and remove broken legacy fallback code.

### Critical Finding from Investigation
The `open_by_key()` method is a **stub returning `Ok(None)`**, breaking session resumption in the legacy fallback path:

```rust
// unified.rs:265-270 - STUB IMPLEMENTATION
pub async fn open_by_key(agent_name: &str, session_key: &str) -> Result<Option<Self>> {
    // This method requires SessionManager to look up the session ID from the index
    // UnifiedSession no longer accesses the index directly
    let _ = (agent_name, session_key);
    Ok(None)  // ← ALWAYS RETURNS NONE!
}
```

This causes `get_or_create_base()` to **always create new sessions** when the index isn't initialized (lines 1167-1189 in manager.rs).

**Since legacy support is NOT needed**, we remove the broken fallback entirely.

### Current Violations:**
```rust
// These all create sessions:
UnifiedSession::create()           // Lines 127-132 in unified.rs
UnifiedSession::create_with_key()  // Lines 162-207
UnifiedSession::create_with_path() // Lines 213-249
SessionManager::create_session()   // Lines 712-779
SessionManager::get_or_create_base() // Lines 1110-1190 (creates via index)
SessionIndex::create_for_peer()    // Creates index entry only
```

**Steps:**

1. **Remove `UnifiedSession::create*` methods**, move logic to SessionManager:
```rust
// In unified.rs - remove these:
// pub(crate) async fn create(...)  // REMOVE
// pub(crate) async fn create_with_key(...)  // REMOVE
// pub(crate) async fn create_with_path(...)  // REMOVE

// Keep only:
pub(crate) async fn open_by_id(...)  // For loading existing
```

2. **Centralize creation in SessionManager**:
```rust
impl SessionManager {
    /// THE ONLY way to create a session
    pub async fn create_session(
        &mut self,
        agent: &str,
        peer: &Peer,
        options: SessionCreateOptions,
    ) -> Result<SessionHandle> {
        let sessions_dir = self.sessions_dir.as_ref()
            .ok_or_else(|| anyhow!("Sessions directory not set"))?;
        
        // 1. Generate session ID
        let session_id = options.session_id
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        
        // 2. Create JSONL file directly (UnifiedSession no longer creates)
        let storage = SessionStorage::new(sessions_dir.clone());
        storage.create_session(&session_id, options.cwd.clone()).await?;
        
        // 3. Create metadata entry
        let entry = SessionEntry::with_peer(
            session_id.clone(),
            agent.to_string(),
            format!("{}.jsonl", session_id),
            peer.peer_type(),
            peer.id(),
        );
        
        // 4. Store via MetadataController
        self.metadata_controller.create_entry(entry).await?;
        
        // 5. Set as active for peer
        let peer_key = derive_base_session_key(agent, peer);
        self.metadata_controller.set_active_for_peer(&peer_key, &session_id).await?;
        
        // 6. Load and cache
        let session = UnifiedSession::open_by_id(agent, &session_id, sessions_dir, Some(peer))
            .await?
            .ok_or_else(|| anyhow!("Failed to open created session"))?;
        
        let arc = Arc::new(RwLock::new(session));
        self.base_sessions.insert((agent.to_string(), peer.clone()), arc.clone());
        
        Ok(SessionHandle::new(session_id, arc, None))
    }
}
```

3. **Update `get_or_create_base` to use `create_session` and remove legacy fallback**:
```rust
pub async fn get_or_create_base(...) -> Result<Arc<RwLock<UnifiedSession>>> {
    // Check cache first
    let key = (agent.to_string(), peer.clone());
    if let Some(session) = self.base_sessions.get(&key) {
        return Ok(session.clone());
    }
    
    // Ensure index is initialized - no legacy fallback
    let sessions_dir = self.sessions_dir.as_ref()
        .ok_or_else(|| anyhow!("SessionManager not initialized - call with_path_resolver() first"))?;
    let peer_key = derive_base_session_key(agent, peer);
    
    if let Some(entry) = self.metadata_controller.get_active_for_peer(&peer_key).await? {
        // Open existing by ID (works correctly)
        let session = UnifiedSession::open_by_id(agent, &entry.session_id, 
            sessions_dir, Some(peer)).await?;
        let arc = Arc::new(RwLock::new(session));
        self.base_sessions.insert(key, arc.clone());
        Ok(arc)
    } else {
        // Create new via unified path
        let handle = self.create_session(agent, peer, SessionCreateOptions::new()).await?;
        Ok(handle.base().clone())
    }
}
```

### Remove Broken Legacy Methods

Since legacy support is not needed, remove these stub/broken methods entirely:

4. **Remove from `unified.rs`**:
```rust
// REMOVE these methods entirely:
// pub async fn open(agent_name: &str, peer: &Peer) -> Result<Option<Self>>  // Lines 258-261
// pub async fn open_by_key(agent_name: &str, session_key: &str) -> Result<Option<Self>>  // Lines 265-270
// pub async fn open_by_key_simple(agent_name: &str, session_key: &str) -> Result<Option<Self>>  // Lines 315-317
// pub async fn open_or_create_by_key(...) -> Result<Self>  // Lines 321-349
// pub async fn get_or_create(...) -> Result<Self>  // Lines 351-357
```

These methods are broken (stubs) or bypass the SessionManager architecture. Use `SessionManager` methods instead.

5. **Remove legacy fallback from `get_or_create_base`** (manager.rs:1167-1189):
```rust
// REMOVE this entire block:
tracing::debug!("No registry available, using legacy session naming");

// Fallback to old behavior (no registry)
let sessions_dir = self.sessions_dir.clone().unwrap_or_else(|| {...});

// Try to open existing  ← This always fails due to open_by_key stub
if let Some(session) = UnifiedSession::open(agent, peer).await? {
    ...
}

// Create new using the correct sessions_dir
...
```

6. **Update test in `unified.rs`**:
The test `test_unified_session_persistence` uses `open_by_key` which will be removed. Update to use `SessionManager`:
```rust
// BEFORE (uses broken open_by_key):
let reopened = UnifiedSession::open_by_key("test_agent", &session_key).await.unwrap();

// AFTER (uses proper SessionManager):
let mut manager = SessionManager::for_cli(resolver, "test_agent", None);
let handle = manager.open_session(&session_id).await?.unwrap();
let reopened = handle.base().read().await;
```

**Files Modified:**
- `src/session/unified.rs` - Remove create methods and broken legacy methods (`open`, `open_by_key`, `open_by_key_simple`, `open_or_create_by_key`, `get_or_create`)
- `src/session/manager.rs` - Centralize creation logic, remove legacy fallback (lines 1167-1189)

**Success Criteria:**
- [ ] `rg "UnifiedSession::create" src/` returns only internal usages within `SessionManager`
- [ ] `rg "open_by_key" src/` returns 0 results
- [ ] `rg "open_or_create_by_key" src/` returns 0 results
- [ ] `rg "UnifiedSession::get_or_create" src/` returns 0 results
- [ ] Test `test_unified_session_persistence` updated to use `SessionManager`
- [ ] All ignored tests pass or are removed if obsolete

---

### Phase 3b: Single Session Deletion Pathway

**Goal**: Only `MetadataController::delete_session()` deletes sessions.

**Current Violations:**
```rust
MetadataController::delete_session()  // Deletes JSONL + metadata + cache
SessionService::delete_session()      // Uses SyncSessionStorage + manual index
```

**Steps:**

1. **Consolidate deletion in MetadataController**:
```rust
impl MetadataController {
    /// Delete session completely (metadata + JSONL + cache + active preference)
    pub async fn delete_session(&mut self, session_id: &str) -> Result<bool> {
        // 1. Check existence
        let exists = self.index.get(session_id).await?.is_some();
        if !exists {
            // Still try to cleanup file if it exists
            self.storage.delete_session(session_id).await.ok();
            return Ok(false);
        }
        
        // 2. Delete JSONL file
        self.storage.delete_session(session_id).await?;
        
        // 3. Delete metadata
        self.index.remove(session_id).await?;
        self.index.save().await?;
        
        // 4. Remove from cache
        self.cache.write().await.remove(session_id);
        
        Ok(true)
    }
}
```

2. **Update SessionService to delegate**:
```rust
impl SessionService {
    pub async fn delete_session(&self, agent_name: &str, team: Option<&str>, 
                                session_id: &str) -> Result<bool> {
        let sessions_dir = self.get_sessions_dir(agent_name, team).await?;
        let mut controller = MetadataController::new(&sessions_dir);
        controller.delete_session(session_id).await
    }
}
```

**Files Modified:**
- `src/session/metadata_controller.rs` - Consolidate deletion
- `src/common/services/session_service.rs` - Delegate to controller

---

### Phase 3c: Stabilization Sprint (Week 12)

**Activities:**
- Test all creation pathways
- Test deletion with various states (active, inactive, with history)
- Verify no `UnifiedSession::create*` calls remain via `rg "UnifiedSession::create" src/`
- **Verify legacy methods removed**: `rg "open_by_key|open_or_create_by_key" src/` returns 0 results
- **Test updated**: Ensure `test_unified_session_persistence` passes with `SessionManager`

---

## Phase 4: SRP Cleanup (Simplified)

**Weeks 13-14**

### Phase 4a: Extract Message Conversion (Functions, Not Struct)

**Goal**: `UnifiedSession` only handles storage operations.

**Current Problem**: `UnifiedSession::load_history()` contains format conversion logic:
```rust
// In unified.rs:load_history_native() - handles multiple event types
match event {
    SessionEvent::LlmMessage(e) => { /* convert */ }
    SessionEvent::UserMessage(e) => { /* convert */ }
    // ... 4 more variants
}
```

**Simplified Solution** (free functions, no new struct):
```rust
// In unified.rs - free functions, no new module or file
fn event_to_chat_message(event: &SessionEvent) -> Option<ChatMessage> {
    match event {
        SessionEvent::LlmMessage(e) => Some(ChatMessage { ... }),
        SessionEvent::UserMessage(e) => Some(ChatMessage { ... }),
        // ... other variants
        _ => None,
    }
}

fn events_to_context_text(entries: &[NormalizedEntry]) -> String {
    // Extract text content
}

impl UnifiedSession {
    pub async fn load_history(&self) -> Result<Vec<ChatMessage>> {
        let events = self.storage.load_events(&self.id).await?;
        Ok(events.iter()
            .filter_map(|e| event_to_chat_message(e))
            .collect())
    }
    
    pub async fn get_context_text(&self, _limit: usize) -> String {
        let entries = self.storage.load_normalized(&self.id).await?;
        events_to_context_text(&entries)
    }
}
```

**Skip:** No new `MessageConverter` struct or file.

**Files Modified:**
- `src/session/unified.rs` - Extract private functions

---

### Phase 4b: Remove Deprecated Methods

**Current**: `UnifiedSession::storage_dir()` deprecated but still present.

**Action**: Remove the deprecated method entirely. All path resolution should use `PathResolver`.

```rust
// In unified.rs
// REMOVE these lines:
// #[deprecated(...)]
// pub fn storage_dir(...) -> PathBuf { ... }
```

**Also Remove:**
- `SessionManager::with_registry()` (deprecated since 0.9.0)
- `SessionManager::with_directory()` (deprecated since 0.9.1)

**Files Modified:**
- `src/session/unified.rs`
- `src/session/manager.rs`

---

### Phase 4c: Stabilization Sprint (Week 14)

**Activities:**
- Final integration tests
- Performance comparison to Phase 1 baseline
- Verify no deprecated method usage remains

---

## Phase 5: Legacy Cleanup

**Weeks 15-16**

### Phase 5a: Remove SessionMetadataBuilder

**Current State (in codebase):**
```rust
// SessionMetadata has BOTH:
impl SessionMetadata {
    pub fn into_builder(self) -> SessionMetadataBuilder { ... }  // Builder pattern
    
    // AND direct mutation methods:
    pub fn record_tokens(&mut self, input: usize, output: usize) { ... }
    pub fn set_message_count(&mut self, count: usize) { ... }
    pub fn set_model(&mut self, provider: impl Into<String>, model: impl Into<String>) { ... }
}
```

**Problem:** Two mutation patterns for the same type. Builder is **only used in tests**.

**Evidence:**
```rust
// Only usage in entire codebase:
fn test_metadata_builder() {
    let meta = SessionMetadata::new(...)
        .into_builder()
        .with_title("Test Title")
        .build();
}
```

**Action:** Remove the builder entirely. Keep simple mutable methods—they're more ergonomic.

```rust
// REMOVE entirely from metadata.rs:
pub struct SessionMetadataBuilder(SessionMetadata);
impl SessionMetadataBuilder { ... }
pub fn into_builder(self) -> SessionMetadataBuilder { ... }
```

**Update test:** Use mutable methods directly.

---

### Phase 5b: Remove Migration Code

**Action:** Move `migrate_to_v2()` from `src/session/index.rs` to standalone binary.

```bash
mkdir -p src/bin
# Move migrate_to_v2 to src/bin/migrate_sessions_v2.rs
```

Make it a standalone utility:
```rust
// src/bin/migrate_sessions_v2.rs
#[tokio::main]
async fn main() -> Result<()> {
    let sessions_dir = std::env::args()
        .nth(1)
        .expect("Usage: migrate_sessions_v2 <sessions_dir>");
    
    let report = migrate_to_v2(Path::new(&sessions_dir)).await?;
    println!("Migration complete: {:?}", report);
    Ok(())
}
```

**Files Modified:**
- `src/session/index.rs` - Remove migrate_to_v2
- `src/bin/migrate_sessions_v2.rs` - New file

---

### Phase 5c: Remove Stub Implementations

**Remove or implement:**
```rust
// In unified.rs - REMOVE (if unused) or implement:
pub async fn open_by_key(agent_name: &str, session_key: &str) -> Result<Option<Self>> {
    let _ = (agent_name, session_key);
    Ok(None)  // Stub!
}

// Check if used:
// grep -r "open_by_key" src/
```

---

### Phase 5d: Final Review & Documentation (Week 16)

**Activities:**
- Update `API_CONTRACT.md` if needed
- Add architecture notes to `SESSION_ARCHITECTURE.md`
- Final performance check
- **No breaking API changes**—verify `SessionMetadata` still exported

---

## What Was Removed (And Why)

| Original Plan | Removed Because |
|---------------|-----------------|
| **Phase 7 (public API break)** | Not worth breaking external code for rename from `SessionMetadata` to `SessionEntry` |
| **6 stabilization sprints** | Reduced to 3—sufficient for code-only changes with no data migration |
| **`MessageConverter` struct/file** | Free functions in `unified.rs` suffice; no new file needed |
| **`SessionMetadataBuilder`** | Only used in tests; mutable methods are clearer |
| **Cache TTL** | Add only if profiling shows cache invalidation issues |
| **Lock timeouts (`try_lock`)** | Document ordering only; add timeouts if deadlocks observed |
| **HybridSession/SessionHandle merger** | High risk, low value—circular ref fix is enough |
| **`CachedEntry` wrapper** | Use `(SessionEntry, Instant)` tuple if TTL ever needed |
| **`MetadataService` trait** | Deferred until multiple implementations actually needed |
| **`open_by_key` and legacy methods** | Stub implementation always returned `Ok(None)` - **broken code removed entirely** |

---

## Lock Hierarchy (Documentation Only)

```rust
//! Lock Ordering Rules (prevent deadlocks)
//!
//! To prevent deadlocks, all code should follow these rules:
//!
//! 1. Acquire SessionManager locks BEFORE sub-component locks
//! 2. Acquire in this order:
//!    a. SessionManager::base_sessions (write if needed)
//!    b. SessionManager::channel_overlays
//!    c. SessionManager::spawn_overlays
//!    d. UnifiedSession (via handle)
//!    e. MetadataController (via shared reference)
//! 3. NEVER hold two base session locks simultaneously
//! 4. ALWAYS release locks before async I/O
//!
//! Note: These are guidelines. Only add try_lock() timeouts if 
//! actual deadlocks are observed in production.
```

---

## Cache Strategy (Simplified)

| Cache | Owner | Purpose | Action |
|-------|-------|---------|--------|
| `MetadataController.cache` | MetadataController | Avoid repeated index reads | **Keep simple HashMap, no TTL** |
| `SessionIndex.sessions_cache` | SessionIndex | File read batching | **Remove**—redundant with controller cache |
| `SessionManager.base_sessions` | SessionManager | Keep loaded sessions in memory | Keep, document |

**Note:** Add TTL to controller cache only if profiling shows stale data issues.

---

## Implementation Timeline

```
Week 1-2:  Phase 1a - Internal SessionEntry adoption
Week 3:    Phase 1b - History event consolidation
Week 4:    Phase 1c - STABILIZATION

Week 5-6:  Phase 2a - Fix SessionHandle circular reference
Week 7:    Phase 2b - SessionIndex privatization
Week 8:    Phase 2c - STABILIZATION
Week 8:    Phase 2d - Cache consistency (shared controller)
Week 8:    Phase 2e - Lock timeout implementation

Week 9:    Phase 3a - Single creation pathway
Week 10:   Phase 3b - Single deletion pathway
Week 11:   Phase 3c - STABILIZATION

Week 12:   Phase 4a - Message conversion functions
Week 13:   Phase 4b - Remove deprecated methods
Week 14:   Phase 4c - STABILIZATION

Week 15:   Phase 5a/b/c - Builder removal, migration cleanup, stubs
Week 16:   Phase 5d - Final review & documentation
```

**Total: 16 weeks (4 months)** — reduced from 22 weeks

**Note on Phase 2 density**: Phases 2c, 2d, and 2e all occur in Week 8. These are complementary:
- Phase 2c: Testing and validation
- Phase 2d: Shared controller implementation
- Phase 2e: Lock timeout utilities

These can be done in parallel or 2d/2e can spill into Week 9 if needed.

---

## Risk Mitigation (Updated)

| Risk | Mitigation | Status |
|------|------------|--------|
| Breaking API changes | **None planned**—keep `SessionMetadata` as public name | Eliminated |
| Data migration issues | No data format changes, only code paths | Low risk |
| Performance regression | Benchmark after Phase 1c, 4c | Monitored |
| Concurrent access bugs | **Lock timeouts with try_lock** (Phase 2e) | Addressed by implementation |
| Memory leaks from Arc cycles | Shared MetadataController instead of SessionManager reference | Fixed |
| Stale cache data | **Single shared MetadataController** (Phase 2d) | Fixed |
| SessionHandle metadata operations | **Shared controller reference** (Phase 2a) | Fixed |
| Deadlock detection | **5-10 second timeouts on all locks** (Phase 2e) | Implemented |
| Type conversion overhead | Profile before optimizing; trait-based approach in Phase 6 | Monitored |

---

## Success Metrics (Revised)

| Metric | Target | Measurement |
|--------|--------|-------------|
| Lines of code | -15% | `cargo loc` before/after |
| Direct SessionIndex usage | 0 | `rg "SessionIndex::" src/ \| wc -l` |
| Deprecated method usage | 0 | `rg "#\[deprecated" src/ \| wc -l` |
| Test coverage | 80%+ | `cargo tarpaulin` |
| Cache hit rate | >80% | Log analysis after Phase 2d |
| Lock timeout incidents | 0 | Error logging in production |
| **Breaking changes** | **0** | API compatibility check |

---

## Appendix B: Future Roadmap (Phase 6+)

**Goal**: Complete type unification and DRY compliance (post-refactor, 6+ months).

### Phase 6: Type Unification (Breaking Change - v2.0)

**Current Technical Debt**: 
- `SessionEntry` (internal) and `SessionMetadata` (public) are identical
- `SessionInfo` and `SessionResponse` duplicate fields
- Conversion methods proliferate (`to_metadata()`, `to_info()`, `from_entry()`)

**Proposed Solution**:
```rust
// Single canonical type (internal)
pub struct SessionEntry { /* all fields */ }

// Public API uses type aliases with deprecated shims
pub type SessionMetadata = SessionEntry;

// Deprecation path for smooth migration
#[deprecated(since = "2.0.0", note = "Use SessionEntry directly")]
pub type SessionMetadata = SessionEntry;
```

**Benefits**:
- Zero conversion overhead
- Single source of truth
- Simpler mental model

### Phase 7: Response Type Consolidation (Breaking Change - v2.0)

**Current Issue**:
```rust
// 3 types for the same data
SessionInfo      // service layer
SessionResponse  // API layer  
SessionEntry     // internal layer
```

**Proposed Solution**:
Use serde attributes for API-specific serialization:
```rust
#[derive(Serialize)]
pub struct SessionEntry {
    #[serde(rename = "id", skip_serializing_if = "Option::is_none")]
    pub session_id: String,
    // ... other fields with serde attributes for API compatibility
}

// In API layer - no separate SessionResponse needed
pub async fn list_sessions(...) -> Json<Vec<SessionEntry>> { ... }
```

Or use a derive macro for API view generation:
```rust
#[derive(ApiResponse)]
#[api(rename_all = "camelCase")]
pub struct SessionEntry { ... }
// Generates SessionEntryResponse with camelCase fields
```

### Phase 8: Trait-Based Views (Non-breaking)

Instead of multiple types, use traits:
```rust
trait SessionView {
    fn session_id(&self) -> &str;
    fn agent_name(&self) -> &str;
    // ... common accessors
}

trait SessionViewMut: SessionView {
    fn set_message_count(&mut self, count: usize);
    fn record_tokens(&mut self, input: usize, output: usize);
}

impl SessionView for SessionEntry { ... }
impl SessionViewMut for SessionEntry { ... }

// Generic APIs work with any SessionView
pub fn format_session_info<T: SessionView>(session: &T) -> String { ... }
```

**Timeline**: These changes are planned for v2.0 with breaking changes. They are deferred now to maintain API compatibility.

---

## Migration Guide (For External Contributors)

### No Action Required
This refactor makes **no breaking changes** to public APIs. External code continues to work.

### Internal Changes (For Contributors)

**If you use `SessionEntry` internally:**
```rust
// Use conversion methods:
let metadata = entry.to_metadata();
let info = entry.to_info();
```

**If you use `SessionIndex` directly:**
```rust
// Before:
let mut index = SessionIndex::open(dir);
let sessions = index.list_all().await?;

// After:
let mut controller = MetadataController::new(dir);
let sessions = controller.list_all().await?;
```

---

## Appendix A: What Was Simplified vs Original Plan

| Aspect | Original | Trimmed |
|--------|----------|---------|
| **Duration** | 22 weeks | 16 weeks |
| **Phases** | 7 | 5 (with 2 sub-phases) |
| **Stabilization sprints** | 6 | 3 |
| **Breaking changes** | Yes (Phase 7) | **No** |
| **New files** | 2 (converter, migrate) | 2 (migrate binary, lock_utils) |
| **New structs** | 3 (Converter, Builder, CachedEntry) | 0 |
| **Cache complexity** | TTL with wrapper | Shared Arc<RwLock<>> |
| **Lock complexity** | try_lock timeouts everywhere | **Timeout implementation** (Phase 2e) |
| **Handle/Session merger** | Full merge | **Shared controller pattern** |

**Core fixes preserved:** Type consolidation, SRP enforcement, DRY compliance, clean boundaries.
**Over-engineering removed:** Builders, premature optimization, unnecessary breaking changes.
**Added protections:** Lock timeouts, cache consistency, SessionHandle operation model.

## Appendix C: Implementation Checklist

### Pre-Implementation
- [ ] Review and approve SessionHandle shared controller design
- [ ] Determine lock timeout durations (suggest 5s write, 10s read)
- [ ] Set up cache hit rate logging for Phase 2d validation
- [ ] **Verify legacy methods to be removed**: `open_by_key`, `open_or_create_by_key`, `get_or_create` have no production callers (only tests)

### Per-Phase Verification
- [ ] Phase 1a: All internal paths use SessionEntry
- [ ] Phase 1b: SessionEvent used throughout internal processing
- [ ] Phase 2a: SessionHandle metadata operations work without SessionManager ref
- [ ] Phase 2b: Zero direct SessionIndex::open calls outside MetadataController
- [ ] Phase 2d: Single MetadataController instance per SessionManager
- [ ] Phase 2e: All locks have timeout protection
- [ ] Phase 3a: Only SessionManager::create_session creates sessions
- [ ] Phase 3b: Only MetadataController::delete_session deletes sessions
- [ ] Phase 4a: UnifiedSession uses free functions for conversion
- [ ] Phase 4b: No deprecated method usage remains
- [ ] Phase 5: Migration code moved to standalone binary

### Post-Refactor Metrics
- [ ] Measure cache hit rate (target >80%)
- [ ] Load test concurrent session operations
- [ ] Verify zero lock timeout errors in stress test
- [ ] Confirm 15% LOC reduction
- [ ] Validate API compatibility (zero breaking changes)
