# Session Module Architectural Review

## Executive Summary

The session module suffers from significant architectural debt accumulated through iterative development without adequate refactoring. While the `SESSION_ARCHITECTURE_REFACTOR_PLAN.md` documents the intended direction, the implementation remains incomplete with multiple overlapping systems coexisting.

---

## 1. DRY Violations (Don't Repeat Yourself)

### 1.1 Duplicate Session Metadata Types

**Problem**: Four different structs represent the same session metadata concept:

| File | Struct | Fields (key) |
|------|--------|--------------|
| `session/index.rs` | `SessionEntry` | session_id, agent_name, created_at, updated_at, message_count, turn_count, tokens, transcript_file, title, parent_session_id, ended, trigger, provider, model, channel, recipient, cwd, peer_type, peer_id |
| `session/metadata.rs` | `SessionMetadata` | session_id, agent_name, created_at, updated_at, message_count, turn_count, tokens, transcript_file, title, parent_session_id, ended, trigger, provider, model, channel, recipient, cwd, peer_type, peer_id |
| `common/services/session_service.rs` | `SessionInfo` | id, agent_name, created_at, updated_at, turn_count, message_count, total_tokens, parent_session_id, title, ended |
| `api/routes/sessions.rs` | `SessionResponse` | id, instance_id, created_at, updated_at, turn_count, message_count, total_tokens, parent_session_id, title |

**Impact**: 
- Changes require updates across multiple files
- Risk of inconsistency when fields diverge
- Confusion about which type to use in new code
- Conversion boilerplate (`From` implementations) scattered throughout

**Evidence**:
```rust
// session/index.rs:30-55 - SessionEntry
pub struct SessionEntry {
    pub session_id: String,
    pub agent_name: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub message_count: usize,
    // ... 20 fields total
}

// session/metadata.rs:17-41 - SessionMetadata  
pub struct SessionMetadata {
    pub session_id: String,
    pub agent_name: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub message_count: usize,
    // ... identical fields
}

// Conversion duplicated everywhere
impl From<SessionEntry> for SessionMetadata { ... }
impl From<SessionMetadata> for SessionEntry { ... }
impl From<SessionEntry> for SessionInfo { ... }
impl From<SessionInfo> for SessionResponse { ... }
```

### 1.2 Duplicate History/Event Types

**Problem**: Multiple representations of session history events:

| File | Type | Variants |
|------|------|----------|
| `session/events.rs` | `SessionEvent` | UserMessage, AssistantMessage, LlmMessage, ToolCall, ToolResult, Thinking, System, etc. |
| `common/services/session_service.rs` | `HistoryEvent` | Session, Message, ToolCall, ToolResult, Thinking, ModelChange, Compaction, Custom |
| `commands/session.rs` | `HistoryDisplayEntry` | Session, Message, ToolResult, ModelChange, Compaction, Custom |
| `tools/session_introspection.rs` | `HistoryMessage` | role, content, tool_calls, tool_results, timestamp |

**Impact**:
- Event conversion logic duplicated across layers
- Loss of information during conversions
- Inconsistent event type naming (e.g., "tool.call" vs "ToolCall")

### 1.3 Multiple Session Storage Interfaces

**Problem**: Multiple storage abstractions for the same JSONL files:

- `session/jsonl.rs` - `SessionStorage` (async)
- `session/sync.rs` - `SyncSessionStorage` (sync wrapper)
- `UnifiedSession` - direct storage via `self.storage`
- `MetadataController` - has its own `storage` field

**Evidence**:
```rust
// session/metadata_controller.rs:23-29
pub struct MetadataController {
    index: SessionIndex,
    storage: SessionStorage,  // Duplicate of what's in UnifiedSession
    sessions_dir: PathBuf,
    cache: Arc<RwLock<HashMap<String, SessionMetadata>>>,
}
```

---

## 2. SRP Violations (Single Responsibility Principle)

### 2.1 UnifiedSession - God Object

**Problem**: `UnifiedSession` handles too many responsibilities:

1. **JSONL Storage Operations** (legitimate)
2. **Message Counting** from JSONL (legitimate)
3. **Path Resolution** - `storage_dir()` method (violation - should use PathResolver)
4. **Session Lifecycle** - `create()`, `open()`, `get_or_create()` (violation - should be SessionManager)
5. **Message Formatting** - converting between content block types
6. **History Loading** - with format conversion logic

**Evidence**:
```rust
// unified.rs - 951 lines handling all aspects of sessions
pub struct UnifiedSession {
    pub id: String,
    pub agent_name: String,
    pub session_key: String,
    pub peer: Peer,
    storage: SessionStorage,
    // ... state fields
}

impl UnifiedSession {
    // Storage operations
    pub async fn add_user(&mut self, content: impl Into<String>) -> Result<()> { ... }
    pub async fn add_assistant(...) -> Result<()> { ... }
    pub async fn load_history(&self) -> Result<Vec<ChatMessage>> { ... }
    
    // Factory methods (should be in SessionManager)
    pub(crate) async fn create(...) -> Result<Self> { ... }
    pub async fn open(...) -> Result<Option<Self>> { ... }
    pub async fn get_or_create(...) -> Result<Self> { ... }
    
    // Path resolution (should use PathResolver)
    #[deprecated] 
    pub fn storage_dir(...) -> PathBuf { ... }
}
```

### 2.2 SessionManager - Multiple Responsibilities

**Problem**: `SessionManager` mixes concerns:

1. **Base Session Cache Management** - `base_sessions: HashMap<..., Arc<RwLock<UnifiedSession>>>`
2. **Overlay Management** - `channel_overlays`, `spawn_overlays`
3. **Session Resolution** - `resolve_session()`, `auto_resume_session()`
4. **Session Lifecycle** - `create_session()`, `open_session()`, `branch_session()`
5. **A2A Session Management** - `resolve_agent_session()`, `get_or_create_a2a_session()`
6. **Metadata Operations** (delegated but accessible) - via `metadata_controller`

**Evidence**:
```rust
// manager.rs:367-384
pub struct SessionManager {
    base_sessions: HashMap<(String, Peer), Arc<RwLock<UnifiedSession>>>,
    channel_overlays: HashMap<String, Arc<RwLock<ChannelOverlay>>>,
    spawn_overlays: HashMap<String, Arc<RwLock<SpawnOverlay>>>,
    pub(crate) metadata_controller: MetadataController,
    index: Option<SessionIndex>,
    sessions_dir: Option<PathBuf>,
    agent_name: Option<String>,
    path_resolver: Option<PathResolver>,
}
```

### 2.3 MetadataController vs SessionIndex Confusion

**Problem**: Unclear separation between `MetadataController` and `SessionIndex`:

- `SessionIndex`: Low-level index operations (CRUD on sessions.json/peers.json)
- `MetadataController`: High-level metadata operations with caching and reconciliation

But both do similar things, and code often bypasses `MetadataController` to use `SessionIndex` directly.

**Evidence**:
```rust
// api/routes/sessions.rs - bypasses MetadataController
async fn list_sessions(...) {
    let sessions = state.session_service().list_sessions(...).await;  // Uses SessionIndex directly
}

// commands/session.rs - sometimes uses MetadataController, sometimes SessionIndex
async fn switch_session(...) {
    let mut index = SessionIndex::open(&loc.sessions_dir);  // Direct use
    index.set_active_for_peer(&peer_key, session_id).await?;
}

async fn delete_session(...) {
    let mut controller = MetadataController::new(&loc.sessions_dir);  // Uses controller
    controller.delete_session(session_id).await?;
}
```

---

## 3. Duplicate Responsibilities

### 3.1 Session Creation Pathways

**Problem**: Sessions can be created through multiple paths:

1. `UnifiedSession::create()` → directly creates JSONL
2. `UnifiedSession::create_with_path()` → used by SessionManager
3. `SessionManager::create_session()` → proper path, creates JSONL + metadata
4. `SessionManager::get_or_create_base()` → may create via index or direct
5. `SessionIndex::create_for_peer()` → creates index entry only

**Evidence**:
```rust
// unified.rs:127-131
pub(crate) async fn create(agent_name: &str, peer: &Peer, team: Option<&str>) -> Result<Self> {
    // Creates JSONL directly
}

// unified.rs:213-249  
pub(crate) async fn create_with_path(...) -> Result<Self> {
    // Creates JSONL at specific path
}

// manager.rs:712-779
pub async fn create_session(...) -> Result<SessionHandle> {
    // 1. Create JSONL via UnifiedSession::create_with_path
    // 2. Create metadata via MetadataController
    // 3. Update peer routing in index
}

// manager.rs:1110-1190
pub async fn get_or_create_base(...) -> Result<Arc<RwLock<UnifiedSession>>> {
    // May create via index or direct UnifiedSession::create_with_path
}
```

### 3.2 Session Deletion Pathways

**Problem**: Three different deletion implementations:

1. `MetadataController::delete_session()` - deletes JSONL + metadata + cache
2. `SessionService::delete_session()` - deletes JSONL via SyncSessionStorage + index
3. Direct deletion in `commands/session.rs` - could use controller but doesn't always

**Evidence**:
```rust
// metadata_controller.rs:263-282
pub async fn delete_session(&mut self, session_id: &str) -> Result<bool> {
    // Deletes JSONL, then metadata, then cache
}

// session_service.rs:305-354  
pub async fn delete_session(...) -> Result<bool> {
    // Uses SyncSessionStorage to delete file
    // Then manually removes from SessionIndex
    // Then handles active preference
}
```

### 3.3 Session Listing Pathways

**Problem**: Multiple ways to list sessions:

1. `MetadataController::list_metadata()` - with optional JSONL sync
2. `SessionIndex::list_all()` - raw index entries
3. `SessionIndex::list_for_peer()` - per-peer sessions
4. `SessionService::list_sessions()` - converts to SessionInfo
5. `UnifiedSession::list_sessions()` - static method reading filesystem directly

---

## 4. Redundancy

### 4.1 Caching Redundancy

**Problem**: Multiple cache layers:

1. `MetadataController.cache` - `Arc<RwLock<HashMap<String, SessionMetadata>>>`
2. `SessionIndex.sessions_cache` - `Option<HashMap<String, SessionEntry>>`
3. `SessionManager.base_sessions` - in-memory session cache

These caches don't coordinate invalidation, leading to potential stale data.

### 4.2 Initialization Method Proliferation

**Problem**: SessionManager has multiple initialization paths:

```rust
// manager.rs:388-404 - creates temp controller
pub fn new() -> Self { ... }

// manager.rs:410-430 - preferred path
pub async fn with_path_resolver(...) -> Result<Self> { ... }

// manager.rs:433-438 - deprecated
#[deprecated] 
pub async fn with_registry(...) -> Result<Self> { ... }

// manager.rs:443-450 - deprecated
#[deprecated]
pub fn with_directory(...) -> Self { ... }

// manager.rs:470-476 - CLI factory
pub fn for_cli(...) -> Self { ... }
```

### 4.3 Builder/Constructor Duplication

**Problem**: `SessionMetadata` has both builder pattern AND mutation methods:

```rust
// metadata.rs:148-150 - builder pattern
pub fn into_builder(self) -> SessionMetadataBuilder { ... }

// metadata.rs:160-166 - direct mutation
pub fn record_tokens(&mut self, input: usize, output: usize) { ... }

// metadata.rs:168-180 - direct mutation  
pub fn set_message_count(&mut self, count: usize) { ... }

// metadata.rs:188-193 - direct mutation
pub fn set_model(&mut self, provider: impl Into<String>, model: impl Into<String>) { ... }
```

The struct is documented as "immutable value object" but has mutable methods.

---

## 5. Uncleaned Legacy Code

### 5.1 Deprecated Methods Still in Use

**Evidence**:
```rust
// unified.rs:101-113
#[deprecated(since = "0.9.0", note = "Use PathResolver::agent_sessions_dir() instead")]
pub fn storage_dir(...) -> PathBuf {
    // Still implemented, may still be called
}

// manager.rs:433-438
#[deprecated(since = "0.9.0", note = "Use with_path_resolver instead")]
pub async fn with_registry(mut self, agent_name: &str) -> Result<Self> {
    // Still implemented
}

// manager.rs:1079-1085
#[deprecated(since = "0.9.0", note = "Use list_sessions_for_peer() or list_sessions() instead")]
pub async fn list_sessions(&mut self, peer: &Peer) -> Result<Vec<SessionEntry>> {
    // Still implemented
}
```

### 5.2 "Temporary" Code Still Present

**Evidence**:
```rust
// manager.rs:388-404
pub fn new() -> Self {
    // Create a temporary metadata controller (will be replaced in with_registry)
    let temp_dir = std::env::temp_dir();
    let metadata_controller = MetadataController::new(temp_dir);
    // ... creates invalid state that MUST be replaced
}
```

The `new()` constructor creates an invalid state that must be "fixed" by calling another method.

### 5.3 Placeholder/Stubs

**Evidence**:
```rust
// unified.rs:265-270
pub async fn open_by_key(agent_name: &str, session_key: &str) -> Result<Option<Self>> {
    // This method requires SessionManager to look up the session ID from the index
    // UnifiedSession no longer accesses the index directly
    let _ = (agent_name, session_key);
    Ok(None)  // Stub implementation!
}
```

### 5.4 Migration Code in Production

**Evidence**:
```rust
// index.rs:709-892
pub async fn migrate_to_v2(sessions_dir: &Path) -> Result<MigrationReport> {
    // 183 lines of one-time migration code still in production
    // Check if already migrated
    if sessions_dir.join("sessions.json").exists()
        && sessions_dir.join("peers.json").exists()
        && !sessions_dir.join("registry.json").exists()
    {
        return Ok(report);  // Skip if already migrated
    }
    // ... migration logic
}
```

---

## 5.5 Broken Stub Implementation (CRITICAL)

**Problem**: `UnifiedSession::open_by_key()` is a stub that always returns `Ok(None)`, silently breaking session resumption in legacy code paths.

**Evidence**:
```rust
// unified.rs:265-270
pub async fn open_by_key(agent_name: &str, session_key: &str) -> Result<Option<Self>> {
    // This method requires SessionManager to look up the session ID from the index
    // UnifiedSession no longer accesses the index directly
    let _ = (agent_name, session_key);
    Ok(None)  // Stub implementation! Always returns None
}
```

**Impact**:
- `UnifiedSession::open()` calls `open_by_key()` → **always returns None**
- `manager.rs:1177`: Legacy fallback path always creates new sessions instead of resuming
- Users without initialized SessionManager experience "lost" session history
- Test `test_unified_session_persistence` is `#[ignore]`d, hiding the bug

**Call Chain**:
```
SessionManager::get_or_create_base() 
  → UnifiedSession::open(agent, peer)  [when index is None]
    → UnifiedSession::open_by_key(...)  [always returns Ok(None)]
      → **Session never resumed, always creates new**
```

**Resolution** (in SESSION_MODULE_REFACTOR_PLAN.md Phase 3a):
- Remove `open_by_key()`, `open()`, `open_by_key_simple()`, `open_or_create_by_key()` entirely
- Remove legacy fallback path in `get_or_create_base()` (manager.rs:1167-1189)
- Enforce SessionManager initialization requirement
- Update test to use `SessionManager` instead of direct UnifiedSession calls

---

## 6. Vague Boundaries

### 6.1 Unclear Division: SessionManager vs MetadataController

| Responsibility | SessionManager | MetadataController | SessionIndex |
|----------------|---------------|-------------------|--------------|
| Create session | ✅ | ❌ | ❌ |
| Create metadata | ❌ | ✅ | ✅ |
| Update metadata | ❌ | ✅ | ✅ |
| Delete session | ❌ | ✅ | ✅ |
| List sessions | ✅ (wraps controller) | ✅ | ✅ |
| Get metadata | ✅ (creates new controller!) | ✅ | ✅ |
| Cache metadata | ❌ | ✅ | ✅ |

**Problem**: `SessionManager::get_session_metadata()` creates a NEW `MetadataController` instead of using its own:

```rust
// manager.rs:841-853
pub async fn get_session_metadata(&self, session_id: &str) -> Result<SessionMetadata> {
    // Use a new controller instance to avoid borrowing issues
    let sessions_dir = self.sessions_dir.clone().unwrap_or_else(|| std::env::temp_dir());
    let mut controller = MetadataController::new(sessions_dir);  // NEW instance!
    
    match controller.get_metadata(session_id, false).await? { ... }
}
```

This bypasses the `MetadataController` cache entirely!

### 6.2 Unclear Division: SessionHandle vs HybridSession

| Feature | SessionHandle | HybridSession |
|---------|--------------|---------------|
| Wraps base session | ✅ | ✅ |
| Has overlay | ❌ (via base.hybrid) | ✅ |
| Can add messages | ✅ | ❌ (must access base) |
| Can get metadata | ✅ (via manager) | ❌ |
| Cloneable | ✅ | ✅ |

Both are wrapper types around the same underlying data. The distinction is unclear.

```rust
// manager.rs:176-181
pub struct SessionHandle {
    session_id: String,
    base: Arc<RwLock<UnifiedSession>>,
    manager: Arc<RwLock<SessionManager>>,
}

// manager.rs:88-94
pub struct HybridSession {
    pub base: Arc<RwLock<UnifiedSession>>,
    pub overlay: OverlayRef,
}
```

### 6.3 Unclear Division: SessionService vs SessionManager vs Commands

The CLI commands, API routes, and internal services all do similar things differently:

```rust
// commands/session.rs - direct index manipulation
async fn switch_session(...) {
    let mut index = SessionIndex::open(&loc.sessions_dir);
    index.set_active_for_peer(&peer_key, session_id).await?;
    index.save().await?;
}

// session_service.rs - uses SessionManager
pub async fn set_active_session(...) {
    let mut index = SessionIndex::open(sessions_dir.clone());  // Direct again!
    // ...
}

// api/routes/sessions.rs - uses SessionService
async fn branch_session(...) {
    let branch_result = state.session_service().branch_session(...).await?;
}
```

### 6.4 Layer Violations

**API routes should use SessionService**, but `SessionService` sometimes bypasses `SessionManager`:

```rust
// session_service.rs:262-300
pub async fn branch_session(...) -> Result<BranchResult> {
    // Use SessionManager for branching
    let mut manager = SessionManager::for_cli(self.path_resolver.clone(), agent_name, team);
    
    // But then creates its OWN SessionIndex for other operations:
    let mut index = SessionIndex::open(&sessions_dir);  // Direct access!
    index.remove(session_id).await?;
}
```

---

## 7. Additional Architectural Issues

### 7.1 Inconsistent Error Handling

Some methods return `Result<T>`, others return `Result<Option<T>>`, and some return `anyhow::Result` while others use custom error types:

```rust
// metadata_controller.rs:96
pub async fn get_metadata(...) -> Result<Option<SessionMetadata>> { ... }

// metadata_controller.rs:236  
pub async fn delete_metadata(...) -> Result<bool> { ... }

// manager.rs:841
pub async fn get_session_metadata(...) -> Result<SessionMetadata> {  // No Option!
```

### 7.2 Inconsistent Async Patterns

Mix of async and sync methods in the same types:

```rust
// SessionIndex has both
pub async fn get(&mut self, ...) -> Result<Option<SessionEntry>> { ... }  // async
pub fn get_active_session_id_cached(&self, ...) -> Option<String> { ... }  // sync!
```

### 7.3 Resource Management Issues

Multiple `Arc<RwLock<...>>` wrappers creating potential deadlock scenarios:

```rust
// manager.rs:219-223
pub async fn create_channel_overlay(...) -> Result<HybridSession> {
    let base = self.get_or_create_base(agent, peer).await?;  // Arc<RwLock<...>>
    let base_key = {
        let base_read = base.read().await;  // Lock held
        base_read.session_key.clone()
    };  // Lock released
    // ...
}
```

---

## 8. Summary Matrix

| Issue | Severity | Files Affected | Effort to Fix |
|-------|----------|----------------|---------------|
| DRY: Duplicate metadata types | High | 4 files | Medium |
| DRY: Duplicate history types | Medium | 4 files | Medium |
| SRP: UnifiedSession god object | High | unified.rs | High |
| SRP: SessionManager complexity | High | manager.rs | High |
| Duplicate: Session creation | High | manager.rs, unified.rs | High |
| Duplicate: Session deletion | Medium | 3 files | Medium |
| Redundancy: Multiple caches | Medium | 3 files | Medium |
| Legacy: Deprecated methods | Medium | 3 files | Low |
| Boundaries: Manager vs Controller | High | manager.rs | High |
| Boundaries: Handle vs Hybrid | Medium | manager.rs | Medium |

---

## 9. Recommended Priority

### P0 (Critical - Fix First)
1. Consolidate session metadata types (`SessionEntry` as source of truth)
2. Clarify `SessionManager` vs `MetadataController` boundaries
3. Remove or complete stub implementations

### P1 (High Priority)
4. Consolidate history event types
5. Eliminate redundant session creation pathways
6. Remove deprecated methods that are no longer called

### P2 (Medium Priority)
7. Consolidate caching into single layer
8. Clarify `SessionHandle` vs `HybridSession` distinction
9. Standardize error handling patterns

### P3 (Cleanup)
10. Remove one-time migration code
11. Consolidate initialization methods
12. Add comprehensive architecture documentation
