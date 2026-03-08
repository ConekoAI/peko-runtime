# Session Management: OpenClaw vs Pekobot - Gap Analysis

## Executive Summary

| Aspect | OpenClaw | Pekobot | Gap Level |
|--------|----------|---------|-----------|
| **Session Storage** | `sessions.json` index + UUID `.jsonl` files | No index, `{agent}_{timestamp}.jsonl` | 🔴 High |
| **Session Keys** | Rich semantic keys (`agent:main:main`) | Session IDs only | 🔴 High |
| **Metadata** | 40+ fields (tokens, model, delivery context) | Basic (cwd, version) | 🟡 Medium |
| **Concurrency** | File locking with queue | None | 🔴 High |
| **Maintenance** | Prune, cap, rotate | None | 🟡 Medium |
| **Tree Structure** | Full branching support | Linear only | 🟡 Medium |
| **Caching** | 45s TTL in-memory cache | None | 🟢 Low |
| **Format** | pi-mono JSONL v3 | Compatible JSONL v3 | ✅ None |

---

## 1. Architecture Comparison

### OpenClaw (via pi-mono/pi-coding-agent)

```
~/.openclaw/
├── agents/
│   └── {agentId}/
│       └── sessions/
│           ├── sessions.json           # Index: sessionKey → metadata
│           ├── sessions.json.bak.{ts}  # Rotated backups
│           ├── {uuid}.jsonl            # Transcript files
│           └── {uuid}-topic-{id}.jsonl # Topic threads
```

**Key Components:**
1. **Session Store (`sessions.json`)**: Central index mapping session keys to SessionEntry objects
2. **Transcript Files (`.jsonl`)**: UUID-named files containing conversation entries
3. **SessionManager Class**: Full tree structure with branching, compaction, labels
4. **File Locking**: Per-session-file locks with timeout and stale detection

### Pekobot

```
~/.pekobot/
└── agents/
    └── {agentName}/
        └── sessions/
            └── {agent}_{timestamp}.jsonl   # Single file per session
```

**Key Components:**
1. **No Index**: Sessions discovered by directory enumeration
2. **Named Files**: `{agent}_{timestamp}.jsonl` format (human-readable)
3. **SessionStorage**: Linear append-only storage, no branching
4. **No Locking**: Direct file writes without coordination

---

## 2. Detailed Gap Analysis

### 2.1 Session Index (sessions.json)

**OpenClaw Implementation:**
```typescript
// sessions.json structure
{
  "agent:main:main": {
    "sessionId": "fe398eae-0e0f-4dcf-a786-ef977ce9ef8d",
    "updatedAt": 1741253123456,
    "totalTokens": 15234,
    "model": "claude-3-5-sonnet-20241022",
    "deliveryContext": { "channel": "discord", "to": "user#1234" },
    "skillsSnapshot": { "prompt": "...", "skills": [...] },
    "systemPromptReport": { ... },
    // ... 40+ more fields
  }
}
```

**Pekobot Status:**
- ❌ No central index
- ❌ Session lookup requires directory scan
- ❌ No metadata aggregation across sessions

**Impact:**
- Cannot efficiently query "all sessions for agent X"
- Cannot track token usage per session
- Cannot resume sessions by semantic key (only by filename)

**Recommendation:** 
Implement a `sessions.json` index with core fields:
```rust
struct SessionIndexEntry {
    session_id: String,
    agent_name: String,
    updated_at: u64,
    total_tokens: Option<usize>,
    message_count: usize,
    session_key: Option<String>,  // e.g., "agent:testagent:cli:default"
}
```

---

### 2.2 Session Key System

**OpenClaw Session Keys:**
```typescript
// Semantic key format
"agent:{agentId}:{context}:{subkey}"

// Examples:
"agent:main:main"                    // Main session
"agent:main:cli:default"             // CLI default
"agent:main:sessions:{sessionKey}"   // Sub-session
"agent:main:discord:{userId}"        // Per-user Discord
"global"                             // Global scope
```

**Key Derivation:**
```typescript
function resolveSessionKey(scope: "per-sender" | "global", ctx: MsgContext): string {
  // Per-sender: channel:accountId:chatType:normalizedFrom
  // Global: "global"
}
```

**Pekobot Status:**
- ✅ Has OpenClaw-compatible CLI format: `agent:{agent}:cli:default`
- ❌ No general session key system
- ❌ No per-sender scoping
- ❌ No global sessions

**Impact:**
- Cannot implement proper multi-user isolation
- Cannot implement per-channel sessions
- CLI persistence is ad-hoc, not general

**Recommendation:**
Implement session key derivation similar to OpenClaw:
```rust
enum SessionScope {
    PerSender,  // Unique per sender
    Global,     // Shared across all
}

fn derive_session_key(scope: SessionScope, ctx: &MessageContext) -> String {
    match scope {
        SessionScope::Global => "global".to_string(),
        SessionScope::PerSender => {
            format!("{}:{}:{}:{}", 
                ctx.channel,
                ctx.account_id.as_deref().unwrap_or("default"),
                ctx.chat_type,
                normalize_sender(&ctx.from)
            )
        }
    }
}
```

---

### 2.3 File Locking & Concurrency

**OpenClaw Implementation:**
```typescript
// Per-session-file locking
async function acquireSessionWriteLock({
  sessionFile,
  timeoutMs = 10000,
  staleMs = 30000
}): Promise<Lock> {
  // 1. Check for existing lock file
  // 2. Verify owner PID is alive
  // 3. Handle stale locks
  // 4. Queue-based FIFO ordering
}

// Session store (sessions.json) has its own lock queue
const LOCK_QUEUES = new Map<storePath, LockQueue>();
```

**Lock File Format:**
```json
{
  "pid": 12345,
  "createdAt": "2025-03-06T12:34:56.789Z"
}
```

**Pekobot Status:**
- ❌ No file locking
- ❌ No concurrent access protection
- ❌ Potential for data corruption

**Impact:**
- Race conditions if multiple processes write same session
- Data loss on concurrent append operations

**Recommendation:**
Implement advisory file locking:
```rust
use tokio::fs;
use std::path::PathBuf;

struct SessionLock {
    lock_path: PathBuf,
}

impl SessionLock {
    async fn acquire(session_file: &Path) -> Result<Self> {
        let lock_path = session_file.with_extension("lock");
        // Write PID + timestamp, check for stale locks
    }
}

// Use in SessionStorage
impl SessionStorage {
    async fn append_with_lock(&self, session_id: &str, entry: Entry) -> Result<()> {
        let _lock = SessionLock::acquire(&self.session_path(session_id)).await?;
        self.append_entry(session_id, entry).await
    }
}
```

---

### 2.4 Session Maintenance

**OpenClaw Configuration:**
```json
{
  "session": {
    "maintenance": {
      "mode": "auto",           // "auto" | "warn" | "off"
      "pruneAfter": "30d",      // Remove entries older than
      "maxEntries": 500,        // Keep only N most recent
      "rotateBytes": "10MB"     // Rotate sessions.json if larger
    }
  }
}
```

**Maintenance Operations:**
1. **Prune**: Remove entries with `updatedAt < now - pruneAfterMs`
2. **Cap**: Sort by `updatedAt`, keep only `maxEntries` most recent
3. **Rotate**: Rename `sessions.json` to `sessions.json.bak.{timestamp}`, keep 3 backups

**Pekobot Status:**
- ❌ No automatic pruning
- ❌ Sessions grow unbounded
- ❌ No size limits

**Impact:**
- Disk space exhaustion over time
- Slow session listing as file count grows
- No recovery from corruption

**Recommendation:**
Add maintenance tasks:
```rust
struct SessionMaintenanceConfig {
    mode: MaintenanceMode,      // Auto, Warn, Off
    prune_after: Duration,      // e.g., 30 days
    max_sessions: usize,        // e.g., 500 per agent
    rotate_bytes: usize,        // e.g., 10MB
}

impl SessionStorage {
    async fn maintenance(&self, config: &SessionMaintenanceConfig) -> MaintenanceReport {
        // Prune old sessions
        // Cap to max_sessions
        // Rotate index if needed
    }
}
```

---

### 2.5 Tree Structure & Branching

**OpenClaw (pi-mono) SessionManager:**
```typescript
class SessionManager {
  private leafId: string | null;     // Current position
  private byId: Map<string, Entry>;  // All entries

  // Tree operations
  appendMessage(msg): string;        // Add as child of leaf
  branch(entryId): void;             // Move leaf to entry
  resetLeaf(): void;                 // Start new branch from root
  
  // Navigation
  getBranch(fromId): Entry[];        // Path from entry to root
  getTree(): SessionTreeNode[];      // Full tree structure
  buildSessionContext(): Context;    // LLM-ready messages
}
```

**Entry Structure:**
```typescript
interface SessionEntry {
  type: "message" | "compaction" | "model_change" | "custom";
  id: string;
  parentId: string | null;  // Tree linkage
  timestamp: string;
  // ... type-specific fields
}
```

**Pekobot Status:**
- ❌ Linear structure only
- ❌ No `parentId` tracking (in entries)
- ❌ No branching capability
- ❌ No tree navigation

**Impact:**
- Cannot implement "/branch" command for conversation forking
- Cannot implement conversation editing (reverting to earlier point)
- Cannot support complex conversation flows

**Recommendation:**
Extend session format with tree support:
```rust
struct SessionEntry {
    id: String,
    parent_id: Option<String>,  // NEW
    timestamp: DateTime<Utc>,
    kind: EntryKind,
}

struct Session {
    storage: SessionStorage,
    leaf_id: Option<String>,  // Current position
    entries: HashMap<String, SessionEntry>,
}

impl Session {
    fn branch(&mut self, entry_id: &str) {
        self.leaf_id = Some(entry_id.to_string());
    }
    
    fn get_branch(&self, from_id: &str) -> Vec<&SessionEntry> {
        // Walk from entry to root following parent_ids
    }
}
```

---

### 2.6 Metadata & Context Tracking

**OpenClaw SessionEntry Fields:**
```typescript
interface SessionEntry {
  // Identity
  sessionId: string;
  label?: string;
  displayName?: string;
  
  // Usage
  inputTokens?: number;
  outputTokens?: number;
  totalTokens?: number;
  totalTokensFresh?: boolean;
  contextTokens?: number;
  compactionCount?: number;
  
  // Model
  modelProvider?: string;
  model?: string;
  modelOverride?: string;
  thinkingLevel?: string;
  verboseLevel?: string;
  reasoningLevel?: string;
  elevatedLevel?: string;
  
  // Delivery Context
  deliveryContext?: DeliveryContext;
  lastChannel?: string;
  lastTo?: string;
  lastAccountId?: string;
  lastThreadId?: string | number;
  
  // Skills
  skillsSnapshot?: SessionSkillSnapshot;
  systemPromptReport?: SessionSystemPromptReport;
  
  // State
  systemSent?: boolean;
  abortedLastRun?: boolean;
  spawnedBy?: string;  // Parent session key
  
  // Heartbeat
  lastHeartbeatText?: string;
  lastHeartbeatSentAt?: number;
  
  // Group
  chatType?: "direct" | "group" | "channel";
  groupId?: string;
  groupChannel?: string;
  subject?: string;
  groupActivation?: "mention" | "always";
}
```

**Pekobot Status:**
- ❌ Only basic session metadata (version, id, cwd, timestamp)
- ❌ No token tracking
- ❌ No model/provider tracking
- ❌ No delivery context
- ❌ No skills snapshot

**Impact:**
- Cannot implement usage tracking
- Cannot resume with correct context
- Cannot implement proper multi-user support

**Recommendation:**
Add comprehensive metadata:
```rust
struct SessionMetadata {
    session_id: String,
    session_key: Option<String>,
    created_at: u64,
    updated_at: u64,
    
    // Usage tracking
    input_tokens: usize,
    output_tokens: usize,
    total_tokens: usize,
    message_count: usize,
    
    // Model context
    provider: Option<String>,
    model: Option<String>,
    
    // Delivery context (for resuming conversations)
    channel: Option<String>,
    recipient: Option<String>,
    account_id: Option<String>,
    
    // State
    system_prompt_sent: bool,
    last_error: Option<String>,
}
```

---

### 2.7 Caching

**OpenClaw Implementation:**
```typescript
const SESSION_STORE_CACHE = new Map<string, CachedStore>();
const DEFAULT_TTL_MS = 45000;  // 45 seconds

function loadSessionStore(storePath: string, opts?: { skipCache?: boolean }) {
  // 1. Check cache
  const cached = SESSION_STORE_CACHE.get(storePath);
  if (cached && isValid(cached)) {
    if (getMtime(storePath) === cached.mtimeMs) {
      return structuredClone(cached.store);
    }
  }
  
  // 2. Load from disk
  const store = JSON.parse(fs.readFileSync(storePath));
  
  // 3. Update cache
  SESSION_STORE_CACHE.set(storePath, {
    store: structuredClone(store),
    loadedAt: Date.now(),
    mtimeMs: getMtime(storePath)
  });
  
  return store;
}
```

**Pekobot Status:**
- ❌ No caching
- ❌ Disk read on every session operation

**Impact:**
- Performance degradation with frequent session access
- Unnecessary disk I/O

**Recommendation:**
Add simple TTL caching:
```rust
use std::collections::HashMap;
use std::time::{Duration, Instant};

struct SessionCache {
    entries: HashMap<String, CachedSession>,
    ttl: Duration,
}

struct CachedSession {
    data: SessionData,
    loaded_at: Instant,
    mtime_ms: u64,
}

impl SessionCache {
    fn get(&self, key: &str) -> Option<&SessionData> {
        self.entries.get(key).and_then(|cached| {
            if cached.loaded_at.elapsed() < self.ttl {
                Some(&cached.data)
            } else {
                None
            }
        })
    }
}
```

---

## 3. Format Compatibility

### JSONL Entry Types

Both systems use compatible formats:

| Entry Type | OpenClaw | Pekobot | Compatible |
|------------|----------|---------|------------|
| `session` | ✅ | ✅ | ✅ Yes |
| `message` | ✅ | ✅ | ✅ Yes |
| `model_change` | ✅ | ✅ | ✅ Yes |
| `compaction` | ✅ | ✅ | ✅ Yes |
| `toolResult` | ✅ | ✅ | ✅ Yes |
| `custom` | ✅ | ✅ | ✅ Yes |
| `label` | ✅ | ❌ | ⚠️ Pekobot ignores |
| `branch_summary` | ✅ | ❌ | ⚠️ Pekobot ignores |

### Content Blocks

Both use the same `ContentBlock` structure:
```json
{"type": "text", "text": "..."}
{"type": "toolCall", "id": "...", "name": "...", "arguments": "..."}
{"type": "toolResult", "toolCallId": "...", "content": [...]}
{"type": "image", "source": {"type": "base64", "mediaType": "...", "data": "..."}}
```

**Verdict:** ✅ Format is compatible. Pekobot can read OpenClaw sessions and vice versa.

---

## 4. Recommendations by Priority

### 🔴 High Priority (Data Integrity)

1. **Implement File Locking**
   - Prevent data corruption on concurrent access
   - Add timeout and stale lock detection
   - Estimated effort: 2-3 days

2. **Add Session Index (sessions.json)**
   - Enable efficient session lookup
   - Support metadata queries
   - Required for proper CLI session persistence
   - Estimated effort: 3-4 days

3. **Implement Session Key System**
   - Enable per-sender isolation
   - Support multiple concurrent conversations
   - Required for multi-user/channel support
   - Estimated effort: 3-4 days

### 🟡 Medium Priority (Operational)

4. **Add Session Maintenance**
   - Prune old sessions
   - Cap session count
   - Rotate large files
   - Estimated effort: 2-3 days

5. **Extend Metadata Tracking**
   - Token usage
   - Model/provider info
   - Delivery context
   - Estimated effort: 2-3 days

### 🟢 Low Priority (Features)

6. **Add Caching Layer**
   - Reduce disk I/O
   - Estimated effort: 1-2 days

7. **Implement Tree Structure**
   - Enable branching
   - Support conversation editing
   - Estimated effort: 4-5 days

---

## 5. Implementation Roadmap

### Phase 1: Core Infrastructure (1-2 weeks)
- [ ] File locking with `SessionLock`
- [ ] `sessions.json` index with `SessionIndex` struct
- [ ] Session key derivation (`derive_session_key`)
- [ ] Update CLI to use index for `session list`

### Phase 2: Metadata & Maintenance (1 week)
- [ ] Extend `SessionMetadata` with token/model tracking
- [ ] Add `SessionMaintenance` with prune/cap/rotate
- [ ] Scheduled maintenance task (on startup/periodic)

### Phase 3: Advanced Features (1-2 weeks)
- [ ] Tree structure with `parent_id`
- [ ] Branch support (`/branch` command)
- [ ] Caching layer for session index

---

## 6. Open Questions

1. **Multi-Agent Sessions**: Should Pekobot support sessions spanning multiple agents?
2. **Session Migration**: How to migrate existing Pekobot sessions to new index format?
3. **Remote Sessions**: Should Pekobot support loading sessions from Pekohub/cloud?
4. **Encryption**: Should session files be encrypted at rest?

---

*Document generated: 2025-03-06*
*Based on: OpenClaw v0.42.0, Pekobot main branch*
