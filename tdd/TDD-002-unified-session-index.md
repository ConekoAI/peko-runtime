# Technical Design: Unified Session Index

**Status:** ✅ **Implemented**

**Implementation Date:** 2026-03-19

**Build Status:** ✅ Compiles (cargo build) | ✅ Tests Pass (860/860) | ✅ Formatted (cargo fmt) | ⚠️ Clippy (warnings only)

**Files Changed:** 15 files (+640/-950 lines)
- `src/session/index.rs` - Rewritten with new two-file architecture
- `src/session/sidecar.rs` - **Deleted**
- `src/session/registry.rs` - **Deleted**
- `src/session/mod.rs`, `base.rs`, `manager.rs`, `recovery.rs`, `sync.rs` - Updated
- `src/engine/simple_session.rs` - Updated
- `src/commands/session.rs` - Updated
- `src/api/routes/sessions.rs` - Updated
- `src/channels/cli.rs` - Updated
- `src/agent/agent.rs` - Updated
- `src/daemon/mod.rs` - Updated

**Author:** @kimi-code

**Date:** 2026-03-19

**Related Issues:** Session duplication bug - same session listed twice in `pekobot session list`

---

## 1. Overview

This design consolidates the fragmented session indexing system into a clean two-file architecture. The current system has THREE competing index mechanisms (`sessions.json` centralized index, `{session_id}.index.json` sidecar files, and `registry.json` peer mappings) causing data duplication, synchronization bugs, and maintenance overhead.

The solution uses TWO files with clear separation of concerns:
- `sessions.json` - Complete session metadata keyed by `session_id`
- `peers.json` - Peer routing index (peer_key → session_ids[]) for O(1) lookups

---

## 2. Goals & Non-Goals

### Goals

- ✅ **Single source of truth per data type** - No metadata duplication
- ✅ **Optimal performance** - O(1) HashMap lookup for all operations
- ✅ **O(1) peer-to-session lookup** - Critical for message routing
- ✅ **Unified API** - CLI and API use identical `SessionIndex` interface
- ✅ **Eliminate duplication bug** - Each session appears exactly once
- ✅ **Atomic updates** - Single file write per operation type
- ✅ **Simplified codebase** - Remove ~950 lines, add ~640 lines (net reduction)

### Non-Goals

- No changes to JSONL event format (remains append-only)
- No changes to session creation/identification logic
- No breaking changes to user-facing CLI commands
- No migration of old session file naming (UUID-based naming already in use)
- No sidecar files per session

---

## 3. Problem Statement

### Current Architecture (Broken)

```
sessions/
├── {session_id}.jsonl              # Conversation events
├── {session_id}.index.json         # Sidecar metadata (API uses)
├── sessions.json                   # Centralized index (CLI uses)
└── registry.json                   # Peer routing + SessionInfo duplication
```

**The Bug:** When `BaseSession::create_with_key()` runs:
1. Creates `{session_id}.jsonl` file
2. Calls `migrate_from_directory()` → finds new file, adds entry with key `agent:X:session:Y`
3. Inserts entry with actual `session_key` like `agent:X:peer:user:default`
4. **Result:** TWO entries pointing to SAME file, SAME session_id

**User Impact:**
```bash
$ pekobot session list testagent
📋 Sessions for default/testagent (2 found):

  🟢 6b45a045-1b89-4d4d-883a-c2bd59e41650
     Messages: 2 | Tokens: 0
     
  🟢 6b45a045-1b89-4d4d-883a-c2bd59e41650  # DUPLICATE!
     Messages: 0 | Tokens: 0
```

### Root Cause

Multiple index systems with different key formats:
- `migrate_from_directory` generates: `agent:{agent}:session:{filename}`
- `BaseSession::create` uses: `agent:{agent}:peer:user:{id}`

Same session, different keys, both stored in `sessions.json`.

### Current registry.json Problem

```json
{
  "peers": {
    "agent:testagent:peer:user:default": {
      "active_session_id": "sess_abc123",
      "sessions": {
        "sess_abc123": { 
          "session_id": "sess_abc123", 
          "transcript_file": "...", 
          "created_at": 123,
          "message_count": 5
        }
      }
    }
  }
}
```

Session metadata exists in BOTH `registry.json` AND `sessions.json` - synchronization nightmare.

---

## 4. Proposed Solution

### 4.1 Target Architecture

```
sessions/
├── {session_id}.jsonl              # Conversation events (unchanged)
├── sessions.json                   # Session metadata ONLY (by session_id)
└── peers.json                      # Peer routing ONLY (peer_key → session_ids[])
```

### 4.2 File Schemas

#### sessions.json - Complete Session Metadata

```json
{
  "6b45a045-1b89-4d4d-883a-c2bd59e41650": {
    "session_id": "6b45a045-1b89-4d4d-883a-c2bd59e41650",
    "agent_name": "testagent",
    "created_at": 1742367300000,
    "updated_at": 1742367310000,
    "message_count": 2,
    "turn_count": 1,
    "input_tokens": 15,
    "output_tokens": 42,
    "total_tokens": 57,
    "transcript_file": "6b45a045-1b89-4d4d-883a-c2bd59e41650.jsonl",
    "title": "USA's capital question",
    "parent_session_id": null,
    "ended": false,
    "trigger": "user",
    "provider": "kimi",
    "model": "claude-sonnet-4-6",
    "channel": "cli",
    "recipient": "default",
    "cwd": "D:\\Workplace\\pekobot\\tests"
  }
}
```

**Key:** `session_id` (UUID)  
**Value:** Complete metadata for that session  
**NO peer_key here** - peer mapping is in peers.json

#### peers.json - Peer Routing Index

```json
{
  "agent:testagent:peer:user:default": {
    "active_session_id": "6b45a045-1b89-4d4d-883a-c2bd59e41650",
    "session_ids": [
      "6b45a045-1b89-4d4d-883a-c2bd59e41650",
      "sess_abc123"
    ]
  },
  "agent:testagent:peer:user:alice": {
    "active_session_id": "sess_xyz789",
    "session_ids": ["sess_xyz789"]
  }
}
```

**Key:** `peer_key` (e.g., `agent:X:peer:user:Y`)  
**Value:** Active session + all session IDs for this peer  
**NO metadata here** - just session IDs for routing

### 4.3 Separation of Concerns

| File | Purpose | Key | Value |
|------|---------|-----|-------|
| `sessions.json` | Session metadata | `session_id` | Complete `SessionEntry` |
| `peers.json` | Peer routing | `peer_key` | `PeerInfo { active_session_id, session_ids[] }` |

**Benefits:**
- ✅ No data duplication
- ✅ `peers.json` stays tiny (just IDs, no metadata)
- ✅ Update session metadata → only touch `sessions.json`
- ✅ Update peer routing → only touch `peers.json`

### 4.4 Unified API

```rust
/// Complete session metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
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

/// Peer routing information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub active_session_id: String,
    pub session_ids: Vec<String>,
}

/// Peer index structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerIndex {
    pub peers: HashMap<String, PeerInfo>,
}

/// Unified session index manager
pub struct SessionIndex {
    sessions_path: PathBuf,
    peers_path: PathBuf,
    sessions_cache: Option<HashMap<String, SessionEntry>>,
    peers_cache: Option<PeerIndex>,
    sessions_modified: bool,
    peers_modified: bool,
}

impl SessionIndex {
    // Core CRUD - O(1) operations
    pub async fn get(&self, session_id: &str) -> Result<Option<SessionEntry>>;
    pub async fn insert(&mut self, entry: SessionEntry) -> Result<()>;
    pub async fn remove(&mut self, session_id: &str) -> Result<Option<SessionEntry>>;
    
    // Peer routing - O(1) operations
    pub async fn get_active_for_peer(&self, peer_key: &str) -> Result<Option<SessionEntry>>;
    pub async fn list_for_peer(&self, peer_key: &str) -> Result<Vec<SessionEntry>>;
    pub async fn set_active_for_peer(&mut self, peer_key: &str, session_id: &str) -> Result<()>;
    pub async fn create_for_peer(&mut self, entry: SessionEntry, peer_key: &str) -> Result<()>;
    pub async fn branch_for_peer(&mut self, new_entry: SessionEntry, peer_key: &str) -> Result<()>;
    
    // Listing operations
    pub async fn list_all(&self) -> Result<Vec<SessionEntry>>;
    pub async fn list_for_agent(&self, agent_name: &str) -> Result<Vec<SessionEntry>>;
    
    // Persistence
    pub async fn save_sessions(&mut self) -> Result<()>;
    pub async fn save_peers(&mut self) -> Result<()>;
    pub async fn save(&mut self) -> Result<()>;
}
```

### 4.5 Performance Characteristics

| Operation | Complexity | Notes |
|-----------|-----------|-------|
| **Get by session_id** | O(1) | Direct HashMap lookup in sessions_cache |
| **Get active for peer** | O(1) | Load peers.json (tiny) + O(1) lookup in sessions_cache |
| **List for peer** | O(N) | N = sessions for that peer (typically 1-10) |
| **List all** | O(N) | N = total sessions for agent |
| **List for agent** | O(N) | Filter all sessions by agent_name |
| **Insert session** | O(1) | Update both caches, mark modified |
| **Update session** | O(1) | Update sessions_cache only |
| **Save** | O(N) | Serialize modified file(s) only |

### 4.6 File Size Analysis

**sessions.json** (scales with session count):
- Per session: ~500 bytes (metadata)
- 1000 sessions: ~500 KB
- 10000 sessions: ~5 MB

**peers.json** (scales with peer count, NOT session count):
- Per peer: ~100 bytes (session IDs only)
- 100 peers: ~10 KB
- 1000 peers: ~100 KB

---

## 5. Implementation Plan

### Phase 1: Schema & Core Implementation

**Files Modified:**
- `src/session/index.rs` - Rewrite with new two-file architecture

**Changes:**
1. Define `SessionEntry` struct (complete metadata)
2. Define `PeerInfo` and `PeerIndex` structs (routing only)
3. Implement `SessionIndex` with dual-cache design
4. Implement all CRUD operations with O(1) complexity
5. Remove `migrate_from_directory` method entirely

### Phase 2: Remove Redundant Systems

**Files Deleted:**
- `src/session/sidecar.rs` - Entire `SidecarManager` and `SessionSidecarIndex`
- `src/session/registry.rs` - Entire peer registry system (replaced by peers.json)

### Phase 3: Update Callers

**Files Modified:**

| File | Changes |
|------|---------|
| `src/session/base.rs` | Use `SessionIndex` instead of sidecar |
| `src/engine/simple_session.rs` | Use `SessionIndex` instead of sidecar |
| `src/session/manager.rs` | Use `get_active_for_peer()` instead of registry |
| `src/commands/session.rs` | Use `SessionIndex` methods |
| `src/api/routes/sessions.rs` | Use `SessionIndex` instead of `SidecarManager` |
| `src/daemon/mod.rs` | Use `SessionIndex` for maintenance |
| `src/session/maintenance.rs` | Update to use new schema |
| `src/session/sync.rs` | Update to use `SessionIndex` |

### Phase 4: Migration

One-time migration from v1.0 to v2.0:
1. Load old sessions.json (may have duplicates)
2. Load old registry.json (has peer mappings + duplicate metadata)
3. Build new sessions.json (deduplicated, complete metadata)
4. Build new peers.json (routing only, no metadata)
5. Write new files
6. Backup old files to `.backup.v1/`

### Phase 5: Testing

- Unit tests for `SessionIndex` CRUD operations
- Unit tests for peer routing operations
- Integration test for migration
- Performance test: O(1) peer lookup
- Manual test: Verify no duplicate sessions

---

## 6. Code Changes Summary

### Files to Delete

| File | Lines | Reason |
|------|-------|--------|
| `src/session/sidecar.rs` | ~450 | Functionality merged into `index.rs` |
| `src/session/registry.rs` | ~400 | Replaced by `peers.json` |

### Files to Modify

| File | Est. Changes | Description |
|------|--------------|-------------|
| `src/session/index.rs` | ~250 | New two-file schema, unified API |
| `src/session/base.rs` | ~50 | Use new index, remove sidecar |
| `src/engine/simple_session.rs` | ~30 | Use new index, remove sidecar |
| `src/session/manager.rs` | ~100 | Use index for peer routing |
| `src/commands/session.rs` | ~100 | Use new index methods |
| `src/api/routes/sessions.rs` | ~50 | Use `SessionIndex` |
| `src/daemon/mod.rs` | ~20 | Simplified maintenance |
| `src/session/maintenance.rs` | ~50 | Update for new schema |
| `src/session/sync.rs` | ~30 | Use new index |
| `src/session/mod.rs` | ~10 | Remove sidecar/registry exports |
| `tests/session_management.rs` | ~100 | Update tests |

**Net change:** ~950 lines removed, ~690 lines added = **~260 lines reduced**

---

## 7. Backward Compatibility

### Data Migration

- ✅ Automatic migration on first run (background task)
- ✅ Old files backed up to `.backup.v1/` directory
- ✅ No user action required
- ✅ Migration is idempotent (safe to re-run)

### API Compatibility

- ✅ API responses remain identical (same fields)
- ✅ CLI commands unchanged
- ✅ JSONL format unchanged

### Rollback

If issues occur, user can restore:
```bash
cd ~/.pekobot/teams/default/agents/<agent>/sessions/
mv sessions.json sessions.json.v2
mv peers.json peers.json.v2
mv .backup.v1/sessions.json sessions.json
mv .backup.v1/registry.json registry.json
```

---

## 8. Design Decisions

| Question | Decision | Rationale |
|----------|----------|-----------|
| Two files vs one? | Two files | Clean separation, O(1) peer lookup without scan |
| peers.json content? | Just session IDs | Keeps file tiny, no metadata duplication |
| In-memory cache? | Yes, both files | O(1) reads, lazy load peers.json |
| Migration automatic? | Yes | Seamless user experience |
| Delete old files? | No, backup to `.backup.v1/` | Safety, allows rollback |
| Session key in entry? | No | peer_key is in peers.json only |

---

## 9. Success Criteria

- [ ] `pekobot session list` shows no duplicates
- [ ] Peer lookup (`get_active_for_peer`) is O(1)
- [ ] `pekobot send` completes in < 100ms (includes O(1) peer lookup)
- [ ] API session endpoints return correct data
- [ ] Session branching works correctly
- [ ] Session switching works correctly
- [ ] Migration completes without data loss
- [ ] All existing tests pass
- [ ] File size reduced (no duplication)

---

## 10. Future Enhancements

- Add `SessionIndex::list_recent(n)` for pagination
- Add background compaction for sessions.json
- Add `SessionIndex::search(query)` for full-text search on titles
- Consider SQLite for agents with 10,000+ sessions (optional)

---

## 11. Appendix: Complete Schema Reference

### SessionEntry

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
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
```

### PeerInfo

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub active_session_id: String,
    pub session_ids: Vec<String>,
}
```

### PeerIndex

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerIndex {
    pub peers: HashMap<String, PeerInfo>,
}
```

---

## 12. Comparison: Before vs After

### Before (Broken)
```
sessions/
├── sess_abc.jsonl
├── sess_abc.index.json
├── sess_def.jsonl
├── sess_def.index.json
├── sessions.json
└── registry.json
```

**Problems:**
- 4 files minimum
- Metadata duplicated in 3 places
- Duplication bug
- O(N) scans for some operations

### After (Unified)
```
sessions/
├── sess_abc.jsonl
├── sess_def.jsonl
├── sessions.json
└── peers.json
```

**Benefits:**
- 2 files + 1 per session
- No metadata duplication
- O(1) for all critical operations
- Clean separation of concerns
