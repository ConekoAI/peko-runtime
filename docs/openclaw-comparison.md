# OpenClaw vs Pekobot: Session, Memory & Compaction Analysis

## Executive Summary

| Feature | OpenClaw | Pekobot | Gap |
|---------|----------|---------|-----|
| **Session Model** | Rich, multi-scope, with expiration policies | Basic, agent-centric | **Major** |
| **Context Compaction** | Automatic + manual summarization | ❌ Not implemented | **Critical** |
| **Memory Architecture** | Markdown files + vector search | SQLite + structured entries | Different approaches |
| **Pre-compaction Flush** | Silent memory flush before compaction | ❌ Not implemented | **Major** |
| **Session Pruning** | In-memory tool result trimming | ❌ Not implemented | **Minor** |
| **Semantic Search** | Multiple providers (OpenAI, Gemini, Voyage, Local) | Framework only (not wired) | **Major** |
| **DM Session Isolation** | `dmScope`: main/per-peer/per-channel-peer | ❌ Not implemented | **Moderate** |

---

## 1. Session Management

### OpenClaw Session Model

OpenClaw has a sophisticated session system:

```typescript
// Session key structure
agent:<agentId>:<mainKey>                    // Direct messages (default)
agent:<agentId>:dm:<peerId>                  // Per-peer isolation
agent:<agentId>:<channel>:dm:<peerId>        // Per-channel-peer
agent:<agentId>:<channel>:group:<id>         // Group chats
agent:<agentId>:<channel>:channel:<id>       // Channels
cron:<job.id>                                // Cron jobs
hook:<uuid>                                  // Webhooks
```

**Session Scopes:**
- `dmScope: "main"` - All DMs share context (default, single-user)
- `dmScope: "per-peer"` - Isolate by sender ID
- `dmScope: "per-channel-peer"` - Isolate by channel + sender (multi-user inboxes)
- `dmScope: "per-account-channel-peer"` - Multi-account inboxes

**Session Lifecycle:**
- **Daily reset**: 4:00 AM local time (configurable)
- **Idle reset**: Sliding window (configurable `idleMinutes`)
- **Manual reset**: `/new` or `/reset` commands
- **Per-type overrides**: Different policies for direct/group/thread

**Session Storage:**
```
~/.openclaw/agents/<agentId>/sessions/
├── sessions.json          // Session metadata store
├── <sessionId>.jsonl      // Full conversation transcript
└── <sessionId>-topic-<threadId>.jsonl  // Telegram topics
```

### Pekobot Session Model

Pekobot has a simpler, less structured approach:

```rust
// Session introspection exists but no formal session management
pub struct SessionInfo {
    pub session_key: String,
    pub agent_id: String,
    pub created_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub message_count: usize,
    pub is_active: bool,
}
```

**Key Limitations:**
- No session expiration/reset policies
- No DM isolation scopes
- No structured transcript storage (JSONL)
- Sessions are ephemeral, not persisted with full context

---

## 2. Context Compaction

### OpenClaw Compaction System

**Auto-compaction (default on):**
```json5
{
  agents: {
    defaults: {
      compaction: {
        reserveTokensFloor: 20000,
        memoryFlush: {
          enabled: true,
          softThresholdTokens: 4000,
          systemPrompt: "Session nearing compaction. Store durable memories now.",
          prompt: "Write any lasting notes to memory/YYYY-MM-DD.md; reply with NO_REPLY if nothing to store.",
        },
      },
    },
  },
}
```

**Compaction Process:**
1. When token count exceeds `contextWindow - reserveTokensFloor`
2. Trigger **pre-compaction memory flush** (silent turn)
3. Summarize older conversation into compact summary entry
4. Persist summary in JSONL history
5. Keep recent messages intact
6. Retry request with compacted context

**Manual Compaction:**
- `/compact [instructions]` - Force compaction with optional focus

**Compaction vs Pruning:**
| Feature | Compaction | Pruning |
|---------|------------|---------|
| Scope | Full conversation | Tool results only |
| Persistence | ✅ Persists in JSONL | ❌ In-memory only |
| Trigger | Token threshold | Per-request |
| Summary | Yes | No (just truncation) |

### Pekobot Compaction

❌ **Not Implemented**

Pekobot has no context compaction mechanism. Long conversations will eventually hit model context limits without mitigation.

---

## 3. Memory System

### OpenClaw Memory Architecture

**Dual-layer Memory:**
```
workspace/
├── memory/YYYY-MM-DD.md     // Daily logs (auto-created)
├── MEMORY.md                 // Curated long-term memory
└── (other files)
```

**Memory Access:**
- `memory_search` - Semantic search across memory files
- `memory_get` - Read specific memory file
- `memory_search` includes `Source: <path#line>` citations

**Vector Search Backends:**
1. **Built-in SQLite** (default) - sqlite-vec extension
2. **QMD** (experimental) - Local-first BM25 + vector + reranking

**Embedding Providers:**
- `openai` - text-embedding-3-small (batch API support)
- `gemini` - gemini-embedding-001
- `voyage` - Voyage embeddings
- `local` - node-llama-cpp with GGUF models

**Hybrid Search (BM25 + Vector):**
```json5
{
  memorySearch: {
    query: {
      hybrid: {
        enabled: true,
        vectorWeight: 0.7,
        textWeight: 0.3,
        candidateMultiplier: 4
      }
    }
  }
}
```

**Pre-compaction Memory Flush:**
- Silent agentic turn before compaction
- Reminds model to write durable notes
- Only runs when workspace is writable
- Usually replies with `NO_REPLY`

### Pekobot Memory Architecture

**SQLite-backed Structured Storage:**
```rust
pub struct MemoryEntry {
    pub id: String,
    pub agent_did: String,
    pub scope: MemoryScope,       // Agent/Tenant/Local/Network/System
    pub memory_type: String,
    pub content: serde_json::Value,
    pub embedding: Option<Vec<f32>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub importance: f32,          // 0.0 - 1.0
    pub thread_id: Option<String>,
    pub tags: Vec<String>,
    pub source: String,
}
```

**Memory Scopes:**
- `Agent` - Single agent only
- `Tenant` - All agents in tenant
- `Local` - All agents in Pekobot instance
- `Network` - Across Coneko network
- `System` - System-level configuration

**Memory Tools:**
- `memory` tool with actions: `store`, `search`, `get`, `recent`, `delete`, `clear`
- Simple substring search (no semantic search wired up)
- Vector memory module exists but not integrated

**Key Limitations:**
- No semantic search implementation (framework only)
- No hybrid search (BM25)
- No embedding providers configured
- No pre-compaction flush
- No Markdown file backing

---

## 4. Session Pruning

### OpenClaw Session Pruning

**In-memory tool result trimming** before LLM calls:
- Trims old tool results from context
- Does NOT rewrite JSONL history
- Complements compaction (different layers)

Configuration:
```json5
{
  // See /concepts/session-pruning for details
}
```

### Pekobot Session Pruning

❌ **Not Implemented**

---

## 5. Key Gaps & Recommendations

### Critical Gaps (Blockers for Long-running Sessions)

| # | Gap | Impact | Recommendation |
|---|-----|--------|----------------|
| 1 | **No Context Compaction** | Sessions hit token limits | Implement auto-compaction with summarization |
| 2 | **No Pre-compaction Flush** | Loss of important context before compaction | Add silent memory flush turn |
| 3 | **No Semantic Search** | Memory retrieval is keyword-only | Wire up vector memory with providers |
| 4 | **No Session Expiration** | Sessions accumulate indefinitely | Add reset policies (daily/idle) |

### Moderate Gaps

| # | Gap | Impact | Recommendation |
|---|-----|--------|----------------|
| 5 | **No DM Session Isolation** | Multi-user setups leak context | Implement `dmScope` equivalent |
| 6 | **No Session Pruning** | Tool results bloat context | Add in-memory tool result trimming |
| 7 | **No Hybrid Search** | Weaker retrieval than OpenClaw | Add BM25 + vector fusion |
| 8 | **No Markdown Memory** | Structured only, less flexible | Consider Markdown file backing |

### Minor Gaps

| # | Gap | Impact | Recommendation |
|---|-----|--------|----------------|
| 9 | **No QMD Backend** | Less sophisticated local search | Evaluate QMD integration |
| 10 | **No Session Transcripts** | No full conversation history | Add JSONL transcript storage |

---

## 6. Implementation Priority

### Phase 1: Critical (Needed for Production)

1. **Context Compaction**
   - Auto-compaction when nearing token limits
   - Summarization of older conversation
   - Persist summary in history

2. **Pre-compaction Memory Flush**
   - Silent turn before compaction
   - Prompt model to store durable notes

3. **Semantic Search Wiring**
   - Connect vector memory to embedding providers
   - Support OpenAI/Gemini/local embeddings

### Phase 2: Important (Better Multi-user Support)

4. **Session Expiration Policies**
   - Daily reset time
   - Idle timeout
   - Per-type/channel overrides

5. **DM Session Isolation**
   - `per-peer`, `per-channel-peer` scopes
   - Identity links across channels

### Phase 3: Nice-to-have (Performance & UX)

6. **Session Pruning**
   - In-memory tool result trimming

7. **Hybrid Search**
   - BM25 + vector fusion

8. **Markdown Memory Files**
   - Human-readable memory format

---

## 7. Architecture Comparison Diagram

### OpenClaw

```
┌─────────────────────────────────────────────────────────────┐
│                    Session Management                        │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐         │
│  │   Daily     │  │   Idle      │  │   Manual    │         │
│  │   Reset     │  │   Timeout   │  │  (/reset)   │         │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘         │
│         └─────────────────┴─────────────────┘               │
│                           │                                 │
│                           ▼                                 │
│  ┌─────────────────────────────────────────────────────┐   │
│  │              Context Window Manager                  │   │
│  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐ │   │
│  │  │   Pruning   │  │  Compaction │  │    Flush    │ │   │
│  │  │  (in-mem)   │  │  (persist)  │  │   (silent)  │ │   │
│  │  └─────────────┘  └──────┬──────┘  └─────────────┘ │   │
│  │                          │                        │   │
│  │              ┌───────────▼───────────┐            │   │
│  │              │  Summarize & Store    │            │   │
│  │              └───────────┬───────────┘            │   │
│  └──────────────────────────┼────────────────────────┘   │
│                             │                             │
│                             ▼                             │
│  ┌─────────────────────────────────────────────────────┐   │
│  │                  Memory System                       │   │
│  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐ │   │
│  │  │   Daily     │  │   Curated   │  │   Vector    │ │   │
│  │  │    Logs     │  │   MEMORY.md │  │    Index    │ │   │
│  │  │  (append)   │  │  (read/write)│  │  (search)   │ │   │
│  │  └─────────────┘  └─────────────┘  └─────────────┘ │   │
│  └─────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
```

### Pekobot

```
┌─────────────────────────────────────────────────────────────┐
│                    Session Management                        │
│  ┌─────────────┐                                            │
│  │   Basic     │  ❌ No expiration policies                  │
│  │   Session   │  ❌ No DM isolation                         │
│  │    Info     │  ❌ No reset triggers                       │
│  └──────┬──────┘                                            │
│         │                                                   │
│         ▼                                                   │
│  ┌─────────────────────────────────────────────────────┐   │
│  │              Context Window Manager                  │   │
│  │                                                     │   │
│  │  ❌ No Pruning                                      │   │
│  │  ❌ No Compaction                                   │   │
│  │  ❌ No Flush                                        │   │
│  │                                                     │   │
│  │  (Direct pass-through to LLM)                       │   │
│  │                                                     │   │
│  └─────────────────────────────────────────────────────┘   │
│                                                             │
│                             ▼                               │
│  ┌─────────────────────────────────────────────────────┐   │
│  │                  Memory System                       │   │
│  │  ┌─────────────┐                                    │   │
│  │  │   SQLite    │  ✅ Structured storage              │   │
│  │  │   Entries   │  ✅ Scoped (Agent/Tenant/Local)     │   │
│  │  │  (CRUD)     │  ✅ Importance scoring              │   │
│  │  └─────────────┘                                    │   │
│  │                                                     │   │
│  │  ❌ No semantic search (framework only)             │   │
│  │  ❌ No Markdown backing                             │   │
│  │  ❌ No hybrid search                                │   │
│  └─────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
```

---

## 8. Migration Path

### For Pekobot to Match OpenClaw's Capabilities

**Short-term (1-2 weeks):**
1. Implement basic compaction with summarization
2. Add session expiration (daily + idle)
3. Wire up semantic search with OpenAI embeddings

**Medium-term (1-2 months):**
4. Add pre-compaction memory flush
5. Implement DM session isolation
6. Add hybrid search (BM25)

**Long-term (3+ months):**
7. Evaluate Markdown memory backing
8. Consider QMD backend for local-first search
9. Add session transcript storage (JSONL)

---

## 9. Conclusion

Pekobot has a solid foundation with SQLite-backed structured memory, but lacks the sophisticated session lifecycle management and context compaction that OpenClaw provides. The **critical gaps** are:

1. **Context compaction** - Essential for long-running sessions
2. **Pre-compaction flush** - Prevents loss of important context
3. **Semantic search wiring** - Memory is only as good as retrieval

Without these, Pekobot will struggle with:
- Long-running agent sessions (context overflow)
- Multi-user deployments (context leakage)
- Effective memory utilization (keyword-only search)

The good news is that Pekobot's structured memory architecture provides a solid foundation to build these features on.
