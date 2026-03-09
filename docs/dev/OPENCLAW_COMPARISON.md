# Pekobot vs OpenClaw - Core Capabilities Comparison

## Executive Summary

| Aspect | Pekobot | OpenClaw |
|--------|---------|----------|
| **Language** | Rust | TypeScript (Node.js) |
| **Binary Size** | ~2MB (minimal core) | ~100MB+ (Node.js bundled) |
| **Startup Time** | <50ms | ~1-2s |
| **Architecture** | Modular core + on-demand tools | Monolithic with bundled features |
| **Network** | Optional Coneko integration | Standalone gateway |
| **License** | TBD | MIT |

---

## Core Runtime Comparison

### 1. Agent Runtime

| Feature | Pekobot | OpenClaw | Notes |
|---------|---------|----------|-------|
| **Agent Lifecycle** | ✅ Full spawn/start/stop/restart | ✅ Full lifecycle | Both support complete lifecycle |
| **Agent Pool** | ✅ With concurrency limits | ✅ Per-session isolation | Pekobot has configurable caps |
| **Multi-Agent** | ✅ Local registry + optional Coneko | ✅ Workspace-based routing | OpenClaw: native multi-agent |
| **Agent State** | ✅ Idle/Busy/Error/Stopped | ✅ Similar states | Both track agent state |
| **Portable Agents** | ✅ `.agent` packages with encryption | ❌ Not supported | Pekobot: unique feature |
| **Agent Export/Import** | ✅ Packager/Unpackager | ❌ Not supported | Pekobot: unique feature |

### 2. Identity & Security

| Feature | Pekobot | OpenClaw | Notes |
|---------|---------|----------|-------|
| **DID Identity** | ✅ ed25519-based DIDs | ❌ Not mentioned | Pekobot: decentralized identifiers |
| **Key Storage** | ✅ Encrypted SQLite | ❌ Not mentioned | Pekobot: secure key management |
| **Secret Manager** | ✅ 6-phase implementation | ❌ Not mentioned | Pekobot: encrypted secrets |
| **Security Sandbox** | ✅ Filesystem restrictions | ✅ Channel restrictions | Different approaches |
| **Allowlists** | ✅ Tool-level + Channel-level | ✅ Channel allowFrom | Both support access control |

### 3. Communication Channels

| Channel | Pekobot | OpenClaw | Notes |
|---------|---------|----------|-------|
| **CLI** | ✅ Interactive | ✅ CLI + TUI | Both fully supported |
| **HTTP/Webhook** | ✅ REST API | ✅ Webhooks | Both supported |
| **Telegram** | ✅ Bot API | ✅ gramY | Both supported |
| **Discord** | ✅ Bot API | ✅ discord.js | Both supported |
| **Slack** | ✅ Web API | ✅ Socket Mode | Both supported |
| **Matrix** | ✅ Client-Server API | ❌ Not mentioned | Pekobot: unique |
| **WhatsApp** | ✅ Business Cloud API | ✅ WhatsApp Web (Baileys) | Different implementations |
| **iMessage** | ❌ Not supported | ✅ macOS only | OpenClaw: macOS exclusive |
| **Mattermost** | ❌ Not supported | ✅ Plugin | OpenClaw: plugin |
| **BlueBubbles** | ❌ Not supported | ✅ Plugin | OpenClaw: iMessage bridge |

**Winner:** OpenClaw has more channels (iMessage, Mattermost, BlueBubbles)
**Pekobot Advantage:** Matrix support, WhatsApp Business API (official)

### 4. LLM Providers

| Provider | Pekobot | OpenClaw | Notes |
|----------|---------|----------|-------|
| **OpenAI** | ✅ Full support | ✅ Supported | Both |
| **Anthropic** | ✅ Full support | ✅ Supported | Both |
| **Kimi** | ✅ Native support | ✅ Supported | Both |
| **Ollama** | ✅ Local models | ✅ Supported | Both |
| **OpenRouter** | ✅ Supported | ✅ Supported | Both |
| **Groq** | ✅ Supported | ✅ Supported | Both |
| **Together** | ✅ Supported | ✅ Supported | Both |
| **Fireworks** | ✅ Supported | ❓ Unknown | Pekobot advantage |
| **Cohere** | ✅ Supported | ❓ Unknown | Pekobot advantage |
| **Perplexity** | ✅ Supported | ❓ Unknown | Pekobot advantage |
| **xAI** | ✅ Supported | ❓ Unknown | Pekobot advantage |
| **Bedrock** | ✅ AWS | ❓ Unknown | Pekobot advantage |

**Winner:** Pekobot has more provider integrations (15+)

### 5. Memory Systems

| Feature | Pekobot | OpenClaw | Notes |
|---------|---------|----------|-------|
| **SQLite Memory** | ✅ Full implementation | ✅ Supported | Both |
| **Vector Search** | ✅ Embedding-based | ✅ Supported | Both |
| **Hybrid Memory** | ✅ SQLite + Vector | ❓ Unknown | Pekobot: combined approach |
| **Memory Scopes** | ✅ Agent/Tenant/Local/Network/System | ❌ Not mentioned | Pekobot: granular scoping |
| **Memory Hygiene** | ✅ TTL, archiving, cleanup | ❌ Not mentioned | Pekobot: auto-cleanup |
| **Markdown Export** | ✅ Full support | ❌ Not mentioned | Pekobot: unique |
| **Transcript Storage** | ✅ JSONL format | ❌ Not mentioned | Pekobot: audit trail |

**Winner:** Pekobot has more sophisticated memory management

### 6. Tool System

| Feature | Pekobot | OpenClaw | Notes |
|---------|---------|----------|-------|
| **Tool Registry** | ✅ Unified multi-backend | ✅ Tool support | Both |
| **Remote Registry** | ✅ Pekohub (cloud) | ❌ Not mentioned | Pekobot: unique |
| **On-Demand Tools** | ✅ Download from registry | ❌ Bundled only | Pekobot: unique |
| **Tool Bundling** | ✅ Optional features | ❌ Always bundled | Pekobot: minimal core |
| **18 Core Tools** | ✅ Full suite | ~9 tools | Pekobot: more tools |
| **Filesystem** | ✅ Full access | ✅ Supported | Both |
| **HTTP/Fetch** | ✅ Built-in | ✅ Supported | Both |
| **Browser Control** | ✅ Playwright | ✅ Playwright | Both |
| **Web Search** | ✅ Brave/DuckDuckGo | ❓ Unknown | Pekobot: built-in |
| **Calendar** | ✅ Google/Outlook | ❌ Not mentioned | Pekobot: unique |
| **Email** | ✅ Gmail/Outlook | ❌ Not mentioned | Pekobot: unique |
| **Social Media** | ✅ Twitter/X, LinkedIn | ❌ Not mentioned | Pekobot: unique |
| **Document Processing** | ✅ PDF/OCR | ❌ Not mentioned | Pekobot: unique |
| **Inventory** | ✅ Shopify/WooCommerce | ❌ Not mentioned | Pekobot: unique |
| **Expense Tracking** | ✅ Receipt OCR | ❌ Not mentioned | Pekobot: unique |
| **Research** | ✅ Web search + citations | ❌ Not mentioned | Pekobot: unique |

**Winner:** Pekobot has significantly more tools (18 vs ~9)

### 7. Session Management

| Feature | Pekobot | OpenClaw | Notes |
|---------|---------|----------|-------|
| **Session Keys** | ✅ Agent-scoped | ✅ Per-sender/workspace | Different approaches |
| **DM Scope** | ✅ Main/PerPeer/PerChannelPeer | ✅ Similar isolation | Both support |
| **Session Reset** | ✅ Daily/Idle/First modes | ✅ Reset triggers | Both |
| **Reset Triggers** | ✅ /reset, /new commands | ✅ Similar | Both |
| **Session Pruning** | ✅ Automatic cleanup | ✅ Compaction | Both |
| **Transcript Storage** | ✅ JSONL with metadata | ❌ Not mentioned | Pekobot: audit trail |

**Winner:** Roughly equivalent

### 8. Message Queue & Routing

| Feature | Pekobot | OpenClaw | Notes |
|---------|---------|----------|-------|
| **Lane-Aware Queue** | ✅ Per-session serialization | ✅ Per-session lanes | Both |
| **Queue Modes** | ✅ Steer/Followup/Collect/Interrupt | ✅ Steer/Followup/Collect | Pekobot: +Interrupt |
| **Concurrency Control** | ✅ Configurable caps | ✅ maxConcurrent | Both |
| **Debounce** | ✅ Configurable ms | ✅ debounceMs | Both |
| **Backpressure** | ✅ Drop policies | ✅ Cap/overflow | Both |
| **Global Semaphore** | ✅ Cross-session limit | ✅ Similar | Both |

**Winner:** Roughly equivalent (Pekobot has Interrupt mode)

### 9. Cron & Scheduling

| Feature | Pekobot | OpenClaw | Notes |
|---------|---------|----------|-------|
| **Cron Jobs** | ✅ Full implementation | ✅ Supported | Both |
| **Daemon Mode** | ✅ Long-running process | ❌ Service only | Pekobot: built-in daemon |
| **Delivery Modes** | ✅ None/Announce | ✅ Similar | Both |
| **Execution Modes** | ✅ Main/Isolated | ✅ Similar | Both |
| **Cron CLI** | ✅ Full CLI | ✅ CLI | Both |
| **Job History** | ✅ SQLite storage | ❌ Not mentioned | Pekobot: persistence |

**Winner:** Pekobot has built-in daemon mode

### 10. Gateway & Plugins

| Feature | Pekobot | OpenClaw | Notes |
|---------|---------|----------|-------|
| **Plugin System** | ✅ Dynamic loading (.so/.dll) | ✅ Node.js plugins | Different tech |
| **Plugin Registry** | ✅ Local + Remote | ❌ Local only | Pekobot: +remote |
| **Hot Reload** | ✅ Runtime loading | ✅ Similar | Both |
| **Gateway Manager** | ✅ Full lifecycle | ✅ Gateway process | Both |
| **Control UI** | ❌ Not implemented | ✅ Web dashboard | OpenClaw: unique |
| **macOS App** | ❌ Not supported | ✅ Companion app | OpenClaw: unique |

**Winner:** OpenClaw has better UX (dashboard, macOS app)

### 11. Mobile & Nodes

| Feature | Pekobot | OpenClaw | Notes |
|---------|---------|----------|-------|
| **iOS Node** | ❌ Not supported | ✅ Full support | OpenClaw: unique |
| **Android Node** | ❌ Not supported | ✅ Full support | OpenClaw: unique |
| **Canvas Surface** | ❌ Not supported | ✅ iOS/Android | OpenClaw: unique |
| **Camera Access** | ❌ Not supported | ✅ Mobile nodes | OpenClaw: unique |
| **Location** | ❌ Not supported | ✅ Mobile nodes | OpenClaw: unique |

**Winner:** OpenClaw has full mobile node support

### 12. Prompt System

| Feature | Pekobot | OpenClaw | Notes |
|---------|---------|----------|-------|
| **Multi-Section** | ✅ 27 sections | ✅ Multi-section | Both |
| **Bootstrap Files** | ✅ AGENTS/SOUL/TOOLS/IDENTITY/USER | ✅ Same set | Both |
| **Prompt Modes** | ✅ Full/Minimal/None | ✅ Full/Minimal | Pekobot: +None |
| **Auto-Bootstrap** | ✅ Q&A ritual | ✅ Q&A ritual | Both |
| **HEARTBEAT.md** | ✅ Read proactively | ✅ Read proactively | Both |
| **MEMORY.md** | ✅ Tool-based access | ✅ Tool-based access | Both |

**Winner:** Roughly equivalent

### 13. Observability

| Feature | Pekobot | OpenClaw | Notes |
|---------|---------|----------|-------|
| **Metrics** | ✅ Basic metrics | ✅ Metrics | Both |
| **Tracing** | ✅ tracing crate | ✅ Debug logs | Both |
| **Audit Logs** | ✅ SQLite storage | ❌ Not mentioned | Pekobot: unique |
| **Session Introspection** | ✅ Tools for querying | ❌ Not mentioned | Pekobot: unique |

**Winner:** Pekobot has better observability

### 14. Compaction & Memory

| Feature | Pekobot | OpenClaw | Notes |
|---------|---------|----------|-------|
| **Compaction** | ✅ Automatic | ✅ Automatic | Both |
| **Transcript Flush** | ✅ JSONL export | ❌ Not mentioned | Pekobot: unique |
| **Context Overflow** | ✅ Handling | ✅ Handling | Both |

---

## Unique Strengths

### Pekobot Unique Features
1. **Portable Agents** - Export/import `.agent` packages
2. **Pekohub** - Cloud tool registry
3. **15+ LLM Providers** - More than OpenClaw
4. **18 Core Tools** - Calendar, email, social media, etc.
5. **Memory Hygiene** - Automatic cleanup
6. **DID Identity** - Decentralized identifiers
7. **Secret Manager** - Encrypted secrets
8. **Rust Performance** - 2MB binary, <50ms startup
9. **Audit Logging** - Full observability

### OpenClaw Unique Features
1. **Web Control UI** - Browser dashboard
2. **macOS Companion App** - Native integration
3. **Mobile Nodes** - iOS/Android support
4. **Canvas Surface** - Rich mobile UI
5. **iMessage Support** - macOS only
6. **Mattermost Plugin** - Enterprise chat
7. **Voice/Audio** - Voice note transcription
8. **Camera Access** - Mobile camera
9. **Location** - Mobile location

---

## Architecture Differences

### Pekobot
```
Minimal Core (~2MB)
├── Agent Runtime
├── Session Management
├── Tool Registry (Unified)
└── Identity System

On-Demand:
├── Tools (downloaded from Pekohub)
├── Gateway Plugins
└── Skills
```

### OpenClaw
```
Full Runtime (~100MB)
├── Gateway (Node.js)
├── Pi Agent (bundled)
├── All Channels (bundled)
├── All Tools (bundled)
└── Web UI (bundled)

Extensions:
├── Mobile Nodes (iOS/Android)
└── macOS App
```

---

## Recommendations

### Use Pekobot When:
- You want a **minimal footprint** (2MB vs 100MB+)
- You need **fast startup** (<50ms)
- You want **on-demand tools** (not bundled)
- You need **portable agents** (export/import)
- You want **18 built-in tools** (calendar, email, etc.)
- You need **enterprise observability** (audit logs)
- You want **15+ LLM provider options**
- You prefer **Rust performance**

### Use OpenClaw When:
- You want a **Web Dashboard** (Control UI)
- You need **mobile nodes** (iOS/Android)
- You want **macOS integration** (companion app)
- You need **iMessage support**
- You want **voice/audio features**
- You prefer **TypeScript/Node.js ecosystem**
- You want **immediate full feature set** (no downloads)

---

## Gap Analysis

### What Pekobot Should Add (from OpenClaw):
1. **Web Control UI** - Dashboard for management
2. **Mobile Node Support** - iOS/Android apps
3. **iMessage Integration** - macOS bridge
4. **Voice/Audio** - Voice note transcription
5. **Camera Access** - Mobile camera integration

### What OpenClaw Should Add (from Pekobot):
1. **Portable Agents** - Export/import functionality
2. **Pekohub-style Registry** - Cloud tool distribution
3. **More Tools** - Calendar, email, social media
4. **DID Identity** - Decentralized identifiers
5. **Secret Manager** - Encrypted credential storage
6. **Memory Hygiene** - Automatic cleanup
7. **More Providers** - 15+ LLM options

---

## Conclusion

**Pekobot** excels at:
- Performance and minimal footprint
- Tool ecosystem and extensibility
- Enterprise features (audit, secrets, DIDs)
- Provider flexibility

**OpenClaw** excels at:
- User experience (Web UI, mobile apps)
- Channel coverage (iMessage, mobile)
- Media handling (voice, camera)
- Ease of use (bundled everything)

Both are capable multi-agent runtimes with different philosophies: Pekobot prioritizes modularity and performance, while OpenClaw prioritizes UX and convenience.
