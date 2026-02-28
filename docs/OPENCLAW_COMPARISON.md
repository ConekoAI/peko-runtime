# Pekobot vs OpenClaw: Module Comparison

## Executive Summary

| Aspect | Pekobot | OpenClaw |
|--------|---------|----------|
| **Language** | Rust | TypeScript |
| **Core Size** | ~2MB (minimal) | ~27,000 lines |
| **Architecture** | Modular runtime + external tools | Monolithic with bundled features |
| **Plugin System** | Dynamic library loading (WIP) | Built-in plugin registry |
| **Multi-Agent** | A2A protocol | Native multi-agent orchestration |
| **Channels** | 7 channels | 10+ channels |
| **LLM Providers** | 15 providers | 20+ providers |

---

## Core Capabilities Comparison

### 1. Agent Runtime

| Feature | Pekobot | OpenClaw | Notes |
|---------|---------|----------|-------|
| **Agent Lifecycle** | ✅ Manager + Pool | ✅ agents/ + lifecycle/ | Pekobot uses Manager pattern; OpenClaw has embedded runner |
| **State Machine** | ✅ Engine + State | ✅ pi-embedded-runner/ | Both have robust state management |
| **Multi-Agent Coordination** | ✅ A2A Protocol | ✅ Native multi-agent | OpenClaw has more mature subagent system |
| **Session Management** | ✅ SessionManager | ✅ sessions/ + auto-reply/ | OpenClaw has more sophisticated session routing |
| **Queue Management** | ✅ Lane-aware FIFO | ✅ process/command-queue.ts | Both implement queue modes (steer/followup/collect) |
| **Subagents** | ✅ sessions_spawn | ✅ subagent-registry/ | OpenClaw has more mature subagent registry |

**Winner: OpenClaw** — More mature subagent and session routing infrastructure.

---

### 2. Identity & Security

| Feature | Pekobot | OpenClaw | Notes |
|---------|---------|----------|-------|
| **DID Identity** | ✅ ed25519-based | ✅ agents/identity/ | Pekobot has cleaner DID implementation |
| **Key Storage** | ✅ SQLite + encrypted | ✅ auth-profiles/ | OpenClaw has more auth provider support |
| **Sandbox** | ⚠️ Planned | ✅ agents/sandbox/ | OpenClaw has Docker sandbox |
| **Audit Logging** | ✅ Basic | ✅ security/audit/ | OpenClaw has comprehensive audit |
| **Secret Manager** | ✅ 6 phases complete | ⚠️ Basic | **Pekobot leads** on secret management |
| **Capability Registry** | ✅ Local + reputation | ✅ agents/model-catalog/ | Both have capability systems |

**Winner: Tie** — Pekobot has better secret management; OpenClaw has better sandbox/audit.

---

### 3. Communication Channels

| Channel | Pekobot | OpenClaw |
|---------|---------|----------|
| CLI | ✅ | ✅ |
| HTTP/Webhook | ✅ | ✅ |
| Telegram | ✅ | ✅ |
| Discord | ✅ | ✅ |
| Slack | ✅ | ✅ |
| Matrix | ✅ | ✅ |
| WhatsApp | ✅ | ✅ |
| Signal | ❌ | ✅ |
| iMessage/BlueBubbles | ❌ | ✅ |
| LINE | ❌ | ✅ |
| WebChat | ❌ | ✅ |

**Winner: OpenClaw** — More channel integrations (Signal, iMessage, LINE, WebChat).

---

### 4. LLM Provider Support

| Provider | Pekobot | OpenClaw |
|----------|---------|----------|
| OpenAI | ✅ | ✅ |
| Anthropic | ✅ | ✅ |
| Kimi | ✅ | ✅ |
| Ollama | ✅ | ✅ |
| OpenRouter | ✅ | ✅ |
| Perplexity | ✅ | ✅ |
| Cohere | ✅ | ❌ |
| Together | ✅ | ❌ |
| Groq | ✅ | ✅ |
| Fireworks | ✅ | ❌ |
| Bedrock | ✅ | ✅ |
| XAI/Grok | ✅ | ✅ |
| Venice | ✅ | ❌ |
| GitHub Copilot | ❌ | ✅ |
| Hugging Face | ❌ | ✅ |
| Minimax | ❌ | ✅ |
| Google Gemini | ❌ | ✅ |
| ZAI | ❌ | ✅ |

**Winner: OpenClaw** — More providers (Copilot, HuggingFace, Gemini, etc.).

---

### 5. Memory & Persistence

| Feature | Pekobot | OpenClaw | Notes |
|---------|---------|----------|-------|
| **SQLite Memory** | ✅ | ✅ | Both use SQLite |
| **Vector Search** | ✅ embeddings | ✅ embeddings/ | Both have vector support |
| **Hybrid Search** | ✅ | ✅ memory/hybrid | Similar implementations |
| **Memory Hygiene** | ✅ TTL + archival | ⚠️ Basic compaction | **Pekobot leads** |
| **Semantic Search** | ✅ | ✅ memory/search-manager | OpenClaw has more sophisticated search |
| **Markdown Export** | ✅ | ❌ | Pekobot unique feature |

**Winner: Tie** — Pekobot has better hygiene; OpenClaw has better semantic search.

---

### 6. Tool System

| Feature | Pekobot | OpenClaw | Notes |
|---------|---------|----------|-------|
| **Built-in Tools** | ✅ 18 core | ✅ 50+ tools | OpenClaw has more built-ins |
| **External Tools** | ✅ Tool Bundle | ❌ Limited | **Pekobot leads** with on-demand tools |
| **Tool Registry** | ✅ Pekohub | ⚠️ Plugin-based | Pekobot has dedicated registry |
| **Browser Automation** | ✅ browser tool | ✅ browser/ extensive | OpenClaw has more browser features |
| **File System** | ✅ | ✅ | Both have FS tools |
| **HTTP/Web** | ✅ fetch/web_search | ✅ web-fetch/ | OpenClaw has more web tools |
| **Cron/Daemon** | ✅ Daemon mode | ✅ daemon/ + cron/ | Both have cron support |
| **Canvas/UI** | ❌ | ✅ canvas-host/ | OpenClaw unique |

**Winner: OpenClaw** — More built-in tools, Canvas support, extensive browser automation.

**BUT: Pekobot's tool extraction architecture is cleaner** — separates concerns better.

---

### 7. Configuration & CLI

| Feature | Pekobot | OpenClaw | Notes |
|---------|---------|----------|-------|
| **Config Format** | TOML | JSON5 | TOML is more readable |
| **CLI Commands** | ✅ 10 commands | ✅ 50+ commands | OpenClaw has more CLI features |
| **Wizard/Onboarding** | ⚠️ Basic | ✅ wizard/ extensive | OpenClaw has better onboarding |
| **Environment Mgmt** | ✅ | ✅ | Both support env vars |
| **Config Validation** | ⚠️ Basic | ✅ config/validation.ts | OpenClaw has Zod schemas |

**Winner: OpenClaw** — More mature CLI and configuration system.

---

### 8. Gateway & Network

| Feature | Pekobot | OpenClaw | Notes |
|---------|---------|----------|-------|
| **HTTP Gateway** | ✅ | ✅ gateway/server-*.ts | Both have HTTP APIs |
| **WebSocket** | ⚠️ Planned | ✅ gateway/ws-connection/ | OpenClaw has mature WS |
| **A2A Protocol** | ✅ | ✅ acp/ | Both support A2A |
| **Discovery** | ⚠️ Basic | ✅ infra/bonjour/ | OpenClaw has mDNS discovery |
| **Tailscale** | ❌ | ✅ infra/tailscale.ts | OpenClaw unique |
| **Node Pairing** | ❌ | ✅ nodes/ | OpenClaw has node system |

**Winner: OpenClaw** — More mature gateway with WS, discovery, Tailscale.

---

### 9. Observability

| Feature | Pekobot | OpenClaw | Notes |
|---------|---------|----------|-------|
| **Logging** | ✅ tracing | ✅ logging/ | Both have structured logging |
| **Metrics** | ⚠️ Basic | ✅ | OpenClaw has more metrics |
| **Audit** | ✅ | ✅ security/audit/ | Both have audit |
| **Health Checks** | ⚠️ Basic | ✅ commands/health.ts | OpenClaw has health system |
| **Dashboard** | ❌ | ✅ gateway/control-ui.ts | OpenClaw has web dashboard |

**Winner: OpenClaw** — Dashboard, health checks, better metrics.

---

### 10. Unique Strengths

#### Pekobot Strengths
1. **Clean Architecture** — Better separation of concerns (tools extracted)
2. **Rust Performance** — Native performance, memory safety
3. **Minimal Core** — ~2MB runtime vs OpenClaw's larger bundle
4. **Secret Management** — More sophisticated secret store
5. **Memory Hygiene** — Better TTL/archival system
6. **Tool Extraction** — On-demand tool installation from Pekohub

#### OpenClaw Strengths
1. **Mature Ecosystem** — 27,000+ lines, battle-tested
2. **More Channels** — Signal, iMessage, LINE, WebChat
3. **Canvas/UI** — Visual output support
4. **Browser Automation** — Extensive Playwright integration
5. **Subagent System** — Sophisticated multi-agent orchestration
6. **Dashboard** — Web-based management UI
7. **Tailscale** — Built-in mesh networking
8. **More Providers** — Copilot, HuggingFace, Gemini

---

## Architecture Comparison

### Pekobot Architecture
```
┌─────────────────────────────────────────┐
│           CLI / Commands                │
├─────────────────────────────────────────┤
│  Agent Manager → Pool → Agent → Engine  │
├─────────────────────────────────────────┤
│  Queue → Session → Memory → Identity    │
├─────────────────────────────────────────┤
│  Channels → Gateway → Providers         │
├─────────────────────────────────────────┤
│  Tools (18 core + Pekohub on-demand)    │
└─────────────────────────────────────────┘
```

**Design Philosophy:** Minimal core, external tools, Rust performance

### OpenClaw Architecture
```
┌─────────────────────────────────────────┐
│  CLI → TUI → Wizard → Dashboard         │
├─────────────────────────────────────────┤
│  Auto-Reply → Queue → Agent Runner      │
├─────────────────────────────────────────┤
│  Sessions → Memory → Skills → Sandbox   │
├─────────────────────────────────────────┤
│  Channels → Gateway → Browser → Canvas  │
├─────────────────────────────────────────┤
│  Tools (50+) + Plugins + Hooks          │
└─────────────────────────────────────────┘
```

**Design Philosophy:** Batteries included, extensive integrations

---

## Gap Analysis

### What Pekobot Needs (to match OpenClaw)

| Priority | Feature | Effort |
|----------|---------|--------|
| High | WebSocket Gateway | Medium |
| High | More Channels (Signal, iMessage) | High |
| Medium | Canvas/UI Support | High |
| Medium | Web Dashboard | High |
| Medium | More LLM Providers | Low |
| Low | Tailscale Integration | Medium |
| Low | Node Pairing System | High |

### What OpenClaw Could Learn from Pekobot

| Feature | Benefit |
|---------|---------|
| Tool Extraction | Smaller core, faster startup |
| Secret Manager | Better security |
| Memory Hygiene | Better resource management |
| Rust Performance | Lower resource usage |

---

## Conclusion

**OpenClaw** is the more mature, feature-complete platform with extensive integrations, UI support, and a large ecosystem.

**Pekobot** is the leaner, more modular architecture with better performance characteristics and a cleaner separation of concerns.

### Recommendation

- **Use OpenClaw** if you need: Canvas UI, extensive channel support, web dashboard, Tailscale, mature subagent system
- **Use Pekobot** if you need: Minimal footprint, Rust performance, clean tool separation, on-demand tool loading, better secret management

**Pekobot's architecture is cleaner** but **OpenClaw has more features**.
