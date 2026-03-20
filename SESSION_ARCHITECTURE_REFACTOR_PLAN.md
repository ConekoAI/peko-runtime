# Session Architecture Refactor Plan

## Executive Summary

This plan refactors the session management architecture to establish:
- **Single Point of Truth**: `SessionManager` is the sole authority for session metadata
- **Separation of Concerns**: Clear boundaries between storage, metadata, and business logic
- **Centralized Control**: All session operations flow through `SessionManager`
- **Data Consistency**: Guaranteed synchronization between JSONL and index

## Current Architecture Issues

### 1. Mixed Responsibilities
```
UnifiedSession currently does BOTH:
├── JSONL storage operations (good)
├── Index updates via update_index() (BAD - violates SRP)
└── Message counting from JSONL (good)
```

### 2. Multiple Entry Points
- `UnifiedSession::create()` - creates session
- `UnifiedSession::create_with_path()` - creates session WITHOUT index entry
- `UnifiedSession::open_by_id()` - opens but doesn't sync message count to index
- `SessionManager::get_or_create_base()` - manages caching but delegates to UnifiedSession

### 3. Index/JSONL Drift
- Index updated incrementally during message addition
- No reconciliation when loading existing sessions
- No verification that index matches actual JSONL content

## Proposed Architecture

### Layer Separation
```
┌─────────────────────────────────────────────────────────────┐
│  LAYER 3: Business Logic                                     │
│  ┌─────────────────────────────────────────────────────┐    │
│  │  SessionManager                                     │    │
│  │  ├─ SessionLifecycleController (create, open, close)│    │
│  │  ├─ MetadataController (read/write all metadata)    │    │
│  │  └─ ConsistencyGuard (ensure index/jsonl sync)      │    │
│  └─────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│  LAYER 2: Domain Model                                       │
│  ┌─────────────────────────────────────────────────────┐    │
│  │  UnifiedSession (read-only metadata view)           │    │
│  │  ├─ storage: SessionStorage (JSONL ops only)        │    │
│  │  ├─ compute_message_count() -> reads JSONL          │    │
│  │  └─ NO index update methods                         │    │
│  └─────────────────────────────────────────────────────┘    │
│                                                              │
│  ┌─────────────────────────────────────────────────────┐    │
│  │  SessionMetadata (value object)                     │    │
│  │  ├─ message_count, token counts, timestamps         │    │
│  │  └─ Immutable, controlled by SessionManager         │    │
│  └─────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│  LAYER 1: Persistence                                        │
│  ┌─────────────────────────────────────────────────────┐    │
│  │  SessionStorage (JSONL file operations)             │    │
│  └─────────────────────────────────────────────────────┘    │
│                                                              │
│  ┌─────────────────────────────────────────────────────┐    │
│  │  SessionIndex (persistent metadata index)           │    │
│  │  └─ ONLY accessed through SessionManager            │    │
│  └─────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
```

### Data Flow

```
Create Session:
  SessionManager::create_session()
    ├── Create JSONL via SessionStorage
    ├── Create SessionEntry with initial metadata
    └── Write to SessionIndex

Add Message:
  SessionManager::add_message(session_id, message)
    ├── UnifiedSession.append_message() -> writes JSONL
    ├── Compute actual message count from JSONL
    └── Update SessionIndex with new counts

Open Session:
  SessionManager::open_session(session_id)
    ├── Load JSONL into UnifiedSession
    ├── Read metadata from SessionIndex
    ├── VERIFY: JSONL message count == index.message_count
    ├── IF mismatch: reconcile and update index
    └── Return UnifiedSession with verified metadata

List Sessions:
  SessionManager::list_sessions()
    ├── Read all entries from SessionIndex
    ├── FOR each entry: verify JSONL exists
    ├── IF JSONL missing: mark as orphaned
    └── Return list with verified metadata
```

## Implementation Phases

### Phase 1: Extract MetadataController (Foundation)

**Goal**: Centralize all index operations in SessionManager

#### 1.1 Create `SessionMetadata` Value Object
```rust
// src/session/metadata.rs
#[derive(Debug, Clone)]
pub struct SessionMetadata {
    pub session_id: String,
    pub agent_name: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub message_count: usize,
    pub turn_count: u32,
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub total_tokens: usize,
    pub transcript_file: String,
    pub title: Option<String>,
    pub parent_session_id: Option<String>,
    pub ended: bool,
    pub trigger: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub channel: Option<String>,
    pub recipient: Option<String>,
    pub cwd: Option<String>,
}

impl SessionMetadata {
    /// Create from SessionEntry (index data)
    pub fn from_entry(entry: SessionEntry) -> Self;
    
    /// Convert to SessionEntry for index storage
    pub fn to_entry(self) -> SessionEntry;
    
    /// Merge with computed values from JSONL
    pub fn reconcile_with_jsonl(&mut self, actual_message_count: usize);
}
```

#### 1.2 Create `MetadataController` inside SessionManager
```rust
// src/session/manager.rs
pub struct SessionManager {
    // ... existing fields ...
    metadata_controller: MetadataController,
}

struct MetadataController {
    index: SessionIndex,
    sessions_dir: PathBuf,
}

impl MetadataController {
    /// Create new metadata entry (atomic operation)
    pub async fn create_metadata(&mut self, metadata: SessionMetadata) -> Result<()>;
    
    /// Read metadata with optional consistency check
    pub async fn get_metadata(
        &mut self, 
        session_id: &str,
        verify_against_jsonl: bool
    ) -> Result<Option<SessionMetadata>>;
    
    /// Update metadata (full replacement)
    pub async fn update_metadata(&mut self, metadata: SessionMetadata) -> Result<()>;
    
    /// Update specific fields (partial update)
    pub async fn update_message_counts(
        &mut self,
        session_id: &str,
        message_count: usize,
        input_tokens: usize,
        output_tokens: usize,
    ) -> Result<()>;
    
    /// List all metadata with consistency verification
    pub async fn list_metadata(&mut self) -> Result<Vec<SessionMetadata>>;
    
    /// Reconcile index with actual JSONL content
    pub async fn reconcile_session(&mut self, session_id: &str) -> Result<ReconciliationReport>;
}

pub struct ReconciliationReport {
    pub session_id: String,
    pub index_message_count: usize,
    pub jsonl_message_count: usize,
    pub was_reconciled: bool,
    pub discrepancies: Vec<Discrepancy>,
}
```

#### 1.3 Remove Index Operations from UnifiedSession
```rust
// src/session/unified.rs
// REMOVE:
// - update_index() method
// - index field (move to SessionManager only)
// - record_usage() method (move to SessionManager)
// - set_model() method (move to SessionManager)

// KEEP:
// - All JSONL storage operations
// - load_history(), get_context_text()
// - add_user(), add_assistant(), add_tool_result() - BUT these become
//   "dumb" storage ops that DON'T update index

// ADD:
/// Get actual message count by reading JSONL
pub async fn compute_message_count(&self) -> Result<usize>;

/// Get token counts from JSONL (for reconciliation)
pub async fn compute_token_usage(&self) -> Result<(usize, usize)>;
```

### Phase 2: Centralize Session Lifecycle (Control)

**Goal**: All session operations go through SessionManager

#### 2.1 Define `SessionHandle` for External Access
```rust
// src/session/manager.rs
/// Opaque handle to a managed session
/// External code cannot modify session directly - must go through SessionManager
pub struct SessionHandle {
    base_session: Arc<RwLock<UnifiedSession>>,
    manager: Weak<RwLock<SessionManager>>, // For callbacks
}

impl SessionHandle {
    /// Add user message (delegates to SessionManager)
    pub async fn add_user(&self, content: impl Into<String>) -> Result<()>;
    
    /// Add assistant message (delegates to SessionManager)
    pub async fn add_assistant(&self, content: impl Into<String>, tool_calls: Option<Vec<ToolCall>>) -> Result<()>;
    
    /// Get read-only view of metadata
    pub async fn get_metadata(&self) -> Result<SessionMetadata>;
    
    /// Load history (direct delegation to UnifiedSession)
    pub async fn load_history(&self) -> Result<Vec<ChatMessage>>;
}
```

#### 2.2 Refactor SessionManager API
```rust
// src/session/manager.rs - NEW API

impl SessionManager {
    // ========== Lifecycle Operations ==========
    
    /// Create a completely new session
    /// This is the ONLY way to create a session
    pub async fn create_session(
        &mut self,
        agent: &str,
        peer: &Peer,
        options: SessionCreateOptions,
    ) -> Result<SessionHandle>;
    
    /// Open an existing session by ID
    /// Automatically reconciles metadata if needed
    pub async fn open_session(
        &mut self,
        session_id: &str,
    ) -> Result<Option<SessionHandle>>;
    
    /// Get or create base session for a peer
    /// Maintains backward compatibility with existing code
    pub async fn get_or_create_base(
        &mut self,
        agent: &str,
        peer: &Peer,
    ) -> Result<SessionHandle>;
    
    // ========== Metadata Operations ==========
    
    /// Get metadata for any session (doesn't require open session)
    pub async fn get_session_metadata(
        &mut self,
        session_id: &str,
    ) -> Result<Option<SessionMetadata>>;
    
    /// List all sessions with metadata
    pub async fn list_sessions(&mut self) -> Result<Vec<SessionMetadata>>;
    
    /// Update session metadata (title, etc.)
    pub async fn update_session_metadata(
        &mut self,
        session_id: &str,
        update: MetadataUpdate,
    ) -> Result<()>;
    
    /// Record token usage for a session
    pub async fn record_token_usage(
        &mut self,
        session_id: &str,
        input_tokens: usize,
        output_tokens: usize,
    ) -> Result<()>;
    
    // ========== Consistency Operations ==========
    
    /// Reconcile a specific session's metadata with JSONL
    pub async fn reconcile_session(
        &mut self,
        session_id: &str,
    ) -> Result<ReconciliationReport>;
    
    /// Reconcile ALL sessions (for maintenance)
    pub async fn reconcile_all_sessions(&mut self) -> Result<Vec<ReconciliationReport>>;
    
    /// Check if a session needs reconciliation
    pub async fn check_consistency(&mut self, session_id: &str) -> Result<ConsistencyStatus>;
}

pub struct SessionCreateOptions {
    pub parent_session_id: Option<String>,
    pub title: Option<String>,
    pub trigger: String, // "user", "branch", "api", etc.
    pub cwd: Option<String>,
}

pub enum MetadataUpdate {
    Title(Option<String>),
    Model { provider: String, model: String },
    Ended(bool),
}

pub struct ConsistencyStatus {
    pub session_id: String,
    pub is_consistent: bool,
    pub index_message_count: usize,
    pub jsonl_message_count: usize,
}
```

#### 2.3 Internal Message Operations (Private)
```rust
// src/session/manager.rs - internal implementation

impl SessionManager {
    /// Internal: Add message to session and update metadata atomically
    async fn add_message_internal(
        &mut self,
        session_id: &str,
        message_type: MessageType,
        content: MessageContent,
    ) -> Result<MessageId> {
        // 1. Get the base session from cache
        let session = self.get_cached_session(session_id).await?;
        
        // 2. Append to JSONL
        let message_id = match message_type {
            MessageType::User => {
                session.write().await.add_user(content.text).await?
            }
            MessageType::Assistant => {
                session.write().await.add_assistant(content.text, content.tool_calls).await?
            }
            // ... etc
        };
        
        // 3. Compute ACTUAL message count from JSONL
        let actual_count = session.read().await.compute_message_count().await?;
        
        // 4. Update index with verified count
        self.metadata_controller
            .update_message_counts(session_id, actual_count, /* tokens */ 0, 0)
            .await?;
        
        Ok(message_id)
    }
}
```

### Phase 3: Consistency & Verification (Reliability)

**Goal**: Ensure index always reflects actual JSONL state

#### 3.1 Add ConsistencyGuard
```rust
// src/session/consistency.rs

/// Ensures SessionIndex and JSONL are always in sync
pub struct ConsistencyGuard;

impl ConsistencyGuard {
    /// Verify that index metadata matches JSONL content
    pub async fn verify(
        index: &mut SessionIndex,
        storage: &SessionStorage,
        session_id: &str,
    ) -> Result<VerificationResult>;
    
    /// Repair index to match JSONL (JSONL is source of truth)
    pub async fn repair_from_jsonl(
        index: &mut SessionIndex,
        storage: &SessionStorage,
        session_id: &str,
    ) -> Result<RepairReport>;
    
    /// Check for orphaned index entries (no JSONL)
    pub async fn find_orphans(
        index: &SessionIndex,
        storage: &SessionStorage,
    ) -> Result<Vec<String>>;
    
    /// Check for orphaned JSONL files (no index entry)
    pub async fn find_unindexed(
        index: &SessionIndex,
        storage: &SessionStorage,
    ) -> Result<Vec<String>>;
}

pub struct VerificationResult {
    pub session_id: String,
    pub passed: bool,
    pub checks: Vec<ConsistencyCheck>,
}

pub struct ConsistencyCheck {
    pub field: String,
    pub index_value: String,
    pub jsonl_value: String,
    pub passed: bool,
}
```

#### 3.2 Automatic Reconciliation on Open
```rust
// In SessionManager::open_session()

pub async fn open_session(&mut self, session_id: &str) -> Result<Option<SessionHandle>> {
    // 1. Get metadata from index
    let mut metadata = match self.metadata_controller.get_metadata(session_id).await? {
        Some(m) => m,
        None => return Ok(None),
    };
    
    // 2. Load UnifiedSession from JSONL
    let session = UnifiedSession::open_by_id(
        &metadata.agent_name,
        session_id,
        &self.sessions_dir,
    ).await?;
    
    // 3. VERIFY: Compare index vs actual JSONL
    let actual_message_count = session.compute_message_count().await?;
    if actual_message_count != metadata.message_count {
        tracing::warn!(
            "Session {} message count mismatch: index={}, jsonl={}",
            session_id,
            metadata.message_count,
            actual_message_count
        );
        
        // 4. REPAIR: Update index to match JSONL
        metadata.message_count = actual_message_count;
        self.metadata_controller.update_metadata(metadata.clone()).await?;
    }
    
    // 5. Cache and return handle
    let handle = SessionHandle::new(
        Arc::new(RwLock::new(session)),
        self.self_ref(),
    );
    
    self.cache.insert(session_id.to_string(), handle.clone());
    
    Ok(Some(handle))
}
```

### Phase 4: Migration & Backward Compatibility

#### 4.1 Deprecation Path for UnifiedSession Direct Creation
```rust
// src/session/unified.rs

impl UnifiedSession {
    /// DEPRECATED: Use SessionManager::create_session() instead
    #[deprecated(
        since = "0.9.0",
        note = "Direct session creation is deprecated. Use SessionManager::create_session()"
    )]
    pub async fn create(agent_name: &str, peer: &Peer) -> Result<Self> {
        // Still works for backward compatibility, but logs warning
        tracing::warn!(
            "Direct UnifiedSession::create() is deprecated. "
            "Use SessionManager::create_session() for proper metadata management"
        );
        // ... existing implementation
    }
    
    /// DEPRECATED: Use SessionManager::open_session() instead
    #[deprecated(
        since = "0.9.0",
        note = "Direct session opening is deprecated. Use SessionManager::open_session()"
    )]
    pub async fn open_by_id(...) -> Result<Self> {
        // ... existing implementation
    }
}
```

#### 4.2 Migration Helper for Existing Sessions
```rust
// src/session/migration.rs

/// One-time migration to ensure all existing sessions have consistent metadata
pub async fn migrate_session_metadata(sessions_dir: &Path) -> Result<MigrationReport> {
    let mut report = MigrationReport::default();
    
    // 1. Find all JSONL files
    let jsonl_files = find_all_jsonl(sessions_dir).await?;
    
    // 2. Find all index entries
    let mut index = SessionIndex::open(sessions_dir);
    let index_entries = index.list_all().await?;
    
    // 3. Check for JSONL files without index entries
    for jsonl_id in &jsonl_files {
        if !index_entries.iter().any(|e| e.session_id == *jsonl_id) {
            // Create index entry from JSONL
            let metadata = extract_metadata_from_jsonl(sessions_dir, jsonl_id).await?;
            index.insert(metadata.to_entry()).await?;
            report.created_entries += 1;
        }
    }
    
    // 4. Reconcile all entries
    for entry in &index_entries {
        let jsonl_path = sessions_dir.join(format!("{}.jsonl", entry.session_id));
        if !jsonl_path.exists() {
            // Orphaned index entry
            report.orphaned_entries.push(entry.session_id.clone());
            continue;
        }
        
        // Count actual messages
        let storage = SessionStorage::new(sessions_dir.to_path_buf());
        let entries = storage.load_session(&entry.session_id).await?;
        let actual_count = count_messages(&entries);
        
        if actual_count != entry.message_count {
            // Update index
            let mut updated = entry.clone();
            updated.message_count = actual_count;
            index.insert(updated).await?;
            report.reconciled_entries += 1;
        }
    }
    
    index.save().await?;
    Ok(report)
}
```

### Phase 5: API & Integration Updates

#### 5.1 Update `loop_v4.rs` to Use SessionManager
```rust
// src/engine/loop_v4.rs

pub async fn run(
    &self,
    prompt: &str,
    on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
) -> Result<AgenticResult> {
    // OLD:
    // let mut session_manager = SessionManager::new().with_registry(...)?;
    // let session = session_manager.get_or_create_base(...)?;
    
    // NEW:
    let manager = SessionManager::new().with_registry(self.agent.name()).await?;
    let handle = manager
        .get_or_create_base(self.agent.name(), &Peer::User("default".to_string()))
        .await?;
    
    // Use handle for all session operations
    self.run_with_resume(prompt, on_event, handle, None).await
}

// Update run_with_resume to take SessionHandle instead of Arc<RwLock<UnifiedSession>>
pub async fn run_with_resume(
    &self,
    prompt: &str,
    on_event: impl Fn(AgenticEvent) + Send + Sync + 'static,
    session: SessionHandle,  // Changed from Arc<RwLock<UnifiedSession>>
    history: Option<Vec<ChatMessage>>,
) -> Result<AgenticResult> {
    // ... implementation uses session.add_user(), session.add_assistant() etc.
}
```

#### 5.2 Update CLI Commands
```rust
// src/commands/session.rs

async fn list_sessions(...) -> anyhow::Result<()> {
    // OLD:
    // let sessions = list_sessions_from_disk(&loc.sessions_dir).await?;
    
    // NEW:
    let manager = SessionManager::new().with_registry(&agent).await?;
    let sessions = manager.list_sessions().await?;
    // Metadata is now guaranteed to be consistent
}
```

## Testing Strategy

### Unit Tests
```rust
#[tokio::test]
async fn test_metadata_reconciliation() {
    let (manager, temp_dir) = setup_test_manager().await;
    
    // Create session
    let handle = manager.create_session("test_agent", &Peer::User("alice".to_string()), Default::default()).await?;
    
    // Add messages
    handle.add_user("Hello").await?;
    handle.add_assistant("Hi!", None).await?;
    
    // Verify metadata consistency
    let metadata = manager.get_session_metadata(&handle.id()).await?;
    assert_eq!(metadata.message_count, 3); // system + user + assistant
    
    // Simulate corruption: manually modify index
    corrupt_index(&temp_dir, &handle.id(), 0).await?;
    
    // Reopen session - should reconcile
    let handle2 = manager.open_session(&handle.id()).await?.unwrap();
    let metadata2 = manager.get_session_metadata(&handle.id()).await?;
    assert_eq!(metadata2.message_count, 3); // Reconciled to actual count
}
```

### Integration Tests
```rust
#[tokio::test]
async fn test_concurrent_message_addition() {
    // Ensure concurrent operations maintain consistency
}

#[tokio::test]
async fn test_crash_recovery() {
    // Simulate crash mid-write, verify reconciliation on reopen
}
```

## Rollout Plan

1. **Phase 1** (This PR): Fix immediate bug with minimal changes
   - Add `reconcile_on_open` to `open_by_id()`
   - Ensure `create_with_path()` creates index entry
   - Add warning logs for inconsistencies

2. **Phase 2** (Next PR): Introduce MetadataController
   - Create new types alongside existing ones
   - Mark old methods as deprecated
   - Dual-write to old and new paths

3. **Phase 3** (Future PR): Complete migration
   - Remove deprecated methods
   - Remove backward compatibility code
   - Update all callers to use new API

## Benefits of New Architecture

1. **Single Point of Truth**: All metadata operations go through `MetadataController`
2. **Separation of Concerns**: Storage, metadata, and business logic are cleanly separated
3. **Automatic Consistency**: Sessions are automatically reconciled when opened
4. **Testability**: Each layer can be tested independently with mocks
5. **Future Scalability**: Easy to add new metadata fields or storage backends
6. **Debugging**: Clear ownership makes it easy to trace metadata flow
