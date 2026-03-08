# Pekobot Session Management Upgrade Plan
## Objective: Achieve OpenClaw Parity

**Target:** Full compatibility with OpenClaw's session system while maintaining Pekobot's lightweight philosophy.

---

## Phase 1: Data Integrity Foundation (Week 1)

**Goal:** Prevent data corruption and establish safe concurrent access

### 1.1 File Locking System
**Files:** `src/session/lock.rs` (new), `src/session/jsonl.rs` (modify)

```rust
// src/session/lock.rs
pub struct FileLock {
    lock_path: PathBuf,
}

impl FileLock {
    pub async fn acquire(session_path: &Path, timeout_ms: u64) -> Result<Self>;
    pub async fn release(self) -> Result<()>;
}
```

**Implementation:**
- Create `.session.lock` files with PID + timestamp
- Stale lock detection (30s timeout)
- Timeout-based waiting (default 10s)
- Cleanup on process termination (SIGTERM handler)

**Testing:**
- Concurrent write test (100 parallel appends)
- Stale lock recovery test
- Timeout verification

**Estimate:** 2-3 days

---

### 1.2 Session Store Index (sessions.json)
**Files:** `src/session/index.rs` (new), `src/session/mod.rs` (modify)

```rust
// src/session/index.rs
pub struct SessionIndex {
    path: PathBuf,
    entries: HashMap<String, IndexEntry>,
    cache: Option<CachedIndex>,
}

pub struct IndexEntry {
    pub session_id: String,
    pub agent_name: String,
    pub session_key: Option<String>,  // "agent:test:cli:default"
    pub updated_at: u64,
    pub created_at: u64,
    pub message_count: usize,
    pub total_tokens: Option<usize>,
    pub transcript_file: PathBuf,
}
```

**Storage Format:**
```json
{
  "agent:testagent:cli:default": {
    "session_id": "testagent_1741253123456",
    "agent_name": "testagent",
    "updated_at": 1741253123456,
    "created_at": 1741253000000,
    "message_count": 42,
    "total_tokens": 15234,
    "transcript_file": "testagent_1741253123456.jsonl"
  }
}
```

**Operations:**
- Load/save with file locking
- Atomic updates (write to temp, rename)
- 45s TTL caching (same as OpenClaw)

**Migration:**
- On first run, scan existing `.jsonl` files and populate index
- Store migration timestamp

**Estimate:** 3-4 days

---

### 1.3 Session Key Derivation
**Files:** `src/session/key.rs` (new)

```rust
// src/session/key.rs
pub enum SessionScope {
    PerSender,   // Unique per sender (e.g., discord:user#1234)
    PerChannel,  // Shared within channel
    Global,      // One session for all
    CliDefault,  // CLI persistent session
}

pub fn derive_session_key(
    agent: &str,
    scope: SessionScope,
    ctx: &MessageContext
) -> String;

// Examples:
// "agent:testagent:discord:1247184530419220533"
// "agent:testagent:cli:default"
// "agent:testagent:global"
```

**Integration Points:**
- CLI: Use `agent:{agent}:cli:default`
- Discord: Use `agent:{agent}:discord:{user_id}` for DMs
- Web/API: Use `agent:{agent}:web:{session_token}`

**Estimate:** 2 days

---

**Phase 1 Deliverables:**
- [ ] File locking prevents concurrent write corruption
- [ ] `sessions.json` index exists and is maintained
- [ ] Session keys work for CLI and channels
- [ ] All existing sessions migrated to index

---

## Phase 2: Operational Infrastructure (Week 2)

**Goal:** Production-ready session lifecycle management

### 2.1 Session Maintenance System
**Files:** `src/session/maintenance.rs` (new)

```rust
// src/session/maintenance.rs
pub struct MaintenanceConfig {
    pub mode: MaintenanceMode,        // Auto, Warn, Off
    pub prune_after: Duration,        // e.g., 30 days
    pub max_sessions: usize,          // e.g., 500 per agent
    pub rotate_bytes: usize,          // e.g., 10MB
}

pub struct MaintenanceReport {
    pub pruned: usize,
    pub capped: usize,
    pub rotated: bool,
    pub bytes_reclaimed: u64,
}

impl SessionIndex {
    pub async fn maintenance(&mut self, config: &MaintenanceConfig) -> MaintenanceReport;
}
```

**Operations:**
- **Prune:** Remove entries with `updated_at < now - prune_after`
- **Cap:** Keep only `max_sessions` most recently updated
- **Rotate:** Rename `sessions.json` to `sessions.json.bak.{timestamp}` if > rotate_bytes
- Cleanup old backups (keep 3)

**Integration:**
- Run on daemon startup
- Run periodically (every hour in daemon mode)
- CLI command: `pekobot session maintenance --prune --max-sessions 100`

**Estimate:** 2-3 days

---

### 2.2 Enhanced Metadata Tracking
**Files:** `src/session/index.rs` (extend), `src/engine/loop_v4.rs` (modify)

**Track per session:**
```rust
pub struct SessionMetadata {
    // Existing
    pub session_id: String,
    pub agent_name: String,
    pub session_key: Option<String>,
    
    // New: Usage
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub total_tokens: usize,
    pub message_count: usize,
    pub tool_call_count: usize,
    
    // New: Model context
    pub provider: Option<String>,
    pub model: Option<String>,
    pub context_tokens: Option<usize>,
    
    // New: State
    pub system_prompt_hash: Option<String>,
    pub last_error: Option<String>,
    pub compaction_count: usize,
    
    // New: Delivery context (for resuming)
    pub channel: Option<String>,
    pub recipient: Option<String>,
    pub account_id: Option<String>,
    pub thread_id: Option<String>,
}
```

**Update Points:**
- After each LLM completion (token counts)
- On model change
- On compaction
- On error

**CLI Display:**
```
$ pekobot session show testagent_1741253123456
Session: testagent_1741253123456
Agent: testagent
Key: agent:testagent:cli:default
Messages: 42
Tokens: 15,234 / 128,000 (11.9%)
Model: claude-3-5-sonnet-20241022
Last active: 2 minutes ago
```

**Estimate:** 2-3 days

---

### 2.3 Session Index Caching
**Files:** `src/session/index.rs` (extend)

```rust
struct CachedIndex {
    data: HashMap<String, IndexEntry>,
    loaded_at: Instant,
    mtime_ms: u64,
}

impl SessionIndex {
    fn with_cache(path: PathBuf, ttl: Duration) -> Self;
    
    async fn get(&mut self, key: &str) -> Option<&IndexEntry> {
        // Check cache first, fallback to disk
    }
}
```

**Estimate:** 1 day

---

**Phase 2 Deliverables:**
- [ ] Automatic session pruning and rotation
- [ ] Rich metadata tracking token usage
- [ ] Cached index reads (45s TTL)
- [ ] `pekobot session maintenance` command

---

## Phase 3: Tree Structure & Branching (Week 3)

**Goal:** Full conversation tree support (OpenClaw parity)

### 3.1 Tree-Aware Session Format
**Files:** `src/session/tree.rs` (new), `src/session/jsonl.rs` (extend)

```rust
// src/session/tree.rs
pub struct TreeEntry {
    pub id: String,
    pub parent_id: Option<String>,  // NEW: Tree linkage
    pub timestamp: DateTime<Utc>,
    pub kind: EntryKind,
}

pub struct ConversationTree {
    entries: HashMap<String, TreeEntry>,
    leaf_id: Option<String>,  // Current position
}

impl ConversationTree {
    pub fn append(&mut self, parent_id: Option<String>, entry: Entry) -> String;
    pub fn branch(&mut self, entry_id: &str);  // Move leaf to entry
    pub fn reset(&mut self);  // Start new branch from root
    pub fn get_branch(&self, from_id: &str) -> Vec<&TreeEntry>;  // Path to root
    pub fn build_context(&self) -> Vec<Message>;  // LLM-ready messages
}
```

**JSONL Format (adds parent_id):**
```json
{"type":"session","version":3,"id":"test_123","timestamp":"...","cwd":"..."}
{"type":"message","id":"msg_001","parent_id":null,"timestamp":"...","message":{...}}
{"type":"message","id":"msg_002","parent_id":"msg_001","timestamp":"...","message":{...}}
{"type":"message","id":"msg_003","parent_id":"msg_001","timestamp":"...","message":{...}}  // Branch!
```

**Estimate:** 3-4 days

---

### 3.2 Branch Commands
**Files:** `src/commands/session.rs` (extend), `src/engine/interactive.rs` (modify)

**Interactive Commands:**
```
/new          - Start fresh session (existing)
/sessions     - List sessions (existing)
/branch <id>  - Start new branch from message <id>
/rewind <id>  - Go back to message <id> and continue from there
/history      - Show tree structure with IDs
```

**CLI Commands:**
```bash
pekobot session branch <session_id> --from <entry_id> --new-name <name>
pekobot session tree <session_id>   # ASCII tree view
```

**Estimate:** 2-3 days

---

### 3.3 Compaction Awareness
**Files:** `src/compaction/mod.rs` (modify)

When compacting, add parent_id to compaction entry:
```json
{
  "type": "compaction",
  "id": "compact_001",
  "parent_id": "msg_042",
  "timestamp": "...",
  "summary": "...",
  "messages_compacted": 40,
  "tokens_before": 50000,
  "tokens_after": 500
}
```

**Estimate:** 1 day

---

**Phase 3 Deliverables:**
- [ ] Tree structure with parent_id in all entries
- [ ] `/branch` and `/rewind` commands in interactive mode
- [ ] `pekobot session tree` for visualization
- [ ] Compaction preserves tree integrity

---

## Phase 4: Channel Integration (Week 4)

**Goal:** Per-channel session isolation

### 4.1 Channel-Aware Session Resolution
**Files:** `src/channels/discord.rs`, `src/channels/cli.rs`, etc.

```rust
// In each channel handler
async fn resolve_session(
    &self,
    agent: &str,
    ctx: &MessageContext
) -> Result<Session> {
    let key = derive_session_key(agent, SessionScope::PerSender, ctx);
    
    // Look up in index
    if let Some(entry) = self.index.get(&key).await {
        Session::open(&entry.transcript_file).await
    } else {
        // Create new
        let session = Session::create(agent).await?;
        self.index.insert(key, session.to_entry()).await?;
        Ok(session)
    }
}
```

**Discord Specific:**
- DMs: `agent:{agent}:discord:{user_id}`
- Guild channels: `agent:{agent}:discord:guild:{guild_id}:channel:{channel_id}`
- Threads: `agent:{agent}:discord:guild:{guild_id}:thread:{thread_id}`

**Estimate:** 2-3 days

---

### 4.2 Session Scoping Configuration
**Files:** `src/config/session.rs` (new)

```toml
# In agent.toml or openclaw.json equivalent
[session]
scope = "per-sender"  # "per-sender" | "per-channel" | "global"
max_history = 100     # Messages to load on resume
persist = true        # Save to disk vs in-memory only

[session.maintenance]
mode = "auto"         # "auto" | "warn" | "off"
prune_after = "30d"
max_sessions = 500
```

**Estimate:** 2 days

---

### 4.3 Cross-Session Messaging
**Files:** `src/tools/session_messaging.rs` (extend)

Enable sessions to message each other using session keys:
```rust
// Tool: session_send
async fn session_send(
    target_key: String,  // e.g., "agent:other:cli:default"
    message: String
) -> Result<()>;
```

**Estimate:** 1-2 days

---

**Phase 4 Deliverables:**
- [ ] Discord DMs use per-user sessions
- [ ] Discord guild channels use per-channel sessions
- [ ] Configurable session scoping
- [ ] Cross-session messaging works

---

## Phase 5: Polish & Optimization (Week 5)

**Goal:** Production hardening

### 5.1 Migration Tooling
**Files:** `src/commands/session.rs` (extend)

```bash
# Migrate old format to new
pekobot session migrate --from 0.1.0 --to 0.2.0

# Export/Import for backup
pekobot session export <session_id> > backup.jsonl
pekobot session import < backup.jsonl
```

**Estimate:** 2 days

---

### 5.2 Performance Optimization
- Batch index updates
- Lazy loading of transcript entries
- Memory-mapped file access for large sessions

**Estimate:** 2 days

---

### 5.3 Comprehensive Testing
**Files:** `tests/session_*.rs` (new)

- Unit tests for all tree operations
- Concurrent write stress tests
- Migration tests
- Fuzz testing for session corruption

**Estimate:** 2-3 days

---

**Phase 5 Deliverables:**
- [ ] Migration command works
- [ ] Export/import for backup
- [ ] 95%+ test coverage on session module
- [ ] Stress tests pass (1000 concurrent operations)

---

## Summary Timeline

| Phase | Duration | Key Deliverable |
|-------|----------|-----------------|
| 1: Foundation | Week 1 | File locking + Index |
| 2: Operations | Week 2 | Maintenance + Metadata |
| 3: Tree | Week 3 | Branching support |
| 4: Channels | Week 4 | Per-channel sessions |
| 5: Polish | Week 5 | Migration + Testing |

**Total: 5 weeks** (with 1 developer)

---

## Risk Mitigation

| Risk | Mitigation |
|------|------------|
| Data migration fails | Keep backups, make reversible |
| Performance regression | Benchmark before/after, keep caching |
| Complexity explosion | Maintain simple API, hide complexity |
| OpenClaw changes | Monitor changelog, maintain compatibility layer |

---

## Success Metrics

- [ ] Zero data corruption in concurrent scenarios
- [ ] Session lookup < 10ms (with cache)
- [ ] Can load OpenClaw sessions without modification
- [ ] All OpenClaw session commands work in Pekobot
- [ ] Migration from old format is lossless

---

*Plan created: 2025-03-07*
*Target version: Pekobot 0.2.0*
