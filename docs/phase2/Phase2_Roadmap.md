# Phase 2 Roadmap: Public Registry Beta + Shared Services Fabric

> **Version**: 1.0  
> **Phase**: 6–12 Months (following Phase 1 completion 2026-05-14)  
> **Objective**: Transition Pekobot from a local developer tool into a distributed platform with public agent discovery and shared infrastructure services.

See `Phase2_Success_Criteria_Public_Registry_Shared_Services_Fabric.md` for the complete, detailed success criteria. This document is the **implementation roadmap** — it identifies the workstreams, their dependencies, and the suggested execution order.

---

## 1. Overview

Phase 2 has three workstreams that build on the Phase 1 runtime:

| Workstream | What It Does | Depends On |
|------------|-------------|------------|
| **A. Public Registry Beta** | Hosted registry + web UI for publishing and discovering agents, teams, and extensions | Phase 1 packaging (`src/portable/`), registry client (`src/registry/`) |
| **B. Shared Services Fabric** | Multi-tenant shared infrastructure (browser, vector DB, memory) that agents use via MCP proxy | Phase 1 daemon, MCP runtime, extension framework |
| **C. Team Runtime Preview v0.5** | Multi-agent orchestration with supervisor/pipeline/mesh patterns | Phase 1 team runtime, A2A messaging, shared services fabric |

The success criteria for all three workstreams are defined in the companion document. This roadmap focuses on **execution order** and **integration points**.

---

## 2. Execution Order

We recommend building Phase 2 in four milestones, each delivering user-visible value:

### Milestone 1: Registry Foundation (Weeks 1–4)
**Goal:** A deployable registry server that can accept pushes and serve pulls.

- Registry server implementing OCI Distribution Spec v1.1
- Content-addressable blob storage (filesystem → S3)
- PostgreSQL metadata store for search indexing
- Tag → digest resolution
- SHA-256 verification on upload
- Garbage collection of unreferenced blobs

**User outcome:** `pekobot agent push` and `pekobot agent pull` work against the public registry.

**Success criteria:** REG-001 through REG-006

### Milestone 2: Auth, Search, and Web UI (Weeks 5–8)
**Goal:** Users can discover and browse packages without using the CLI.

- OAuth 2.0 login (GitHub, Google)
- API key generation for CLI auth
- Namespace ownership enforcement
- Full-text search with faceted filtering
- Public web UI at `https://pekohub.org`
- Bundle detail pages (README, version history, install command)

**User outcome:** A developer can find an agent on the web, copy the install command, and run it.

**Success criteria:** REG-007 through REG-014, REG-025 through REG-028

### Milestone 3: Shared Services Fabric Core (Weeks 9–14)
**Goal:** Agents can use shared browser, vector DB, and memory services instead of bundling their own.

- Fabric control plane (gRPC/Unix socket sidecar)
- Service catalog (`services.toml` / `team.toml` `[services]`)
- MCP Proxy as unified endpoint for all shared services
- Browser pool with tenant-isolated contexts
- Vector DB (pgvector) with tenant-scoped collections
- Memory service (3-tier: short-term Redis, long-term Postgres, shared knowledge via vector DB)
- Reference counting and idle shutdown
- Health checks and automatic restart
- Resource quotas and Prometheus metrics

**User outcome:** An agent declares `fabric.services = ["browser", "vector", "memory"]` in `team.toml` and the fabric provisions them on startup.

**Success criteria:** FAB-001 through FAB-029

### Milestone 4: Team Runtime Preview + Integration (Weeks 15–20)
**Goal:** Multi-agent teams execute with shared services, demonstrating the full platform.

- `team.toml` declarative format with bundle references from registry
- Supervisor coordination pattern (coordinator + workers)
- Shared context and unified output stream across team agents
- `pekobot team run team.toml` CLI command
- Integration demo: 3-agent research team using shared browser + vector DB

**User outcome:** A user writes a `team.toml`, runs `pekobot team run`, and watches a coordinator delegate tasks to workers with shared services.

**Success criteria:** TEAM-001 through TEAM-010

---

## 3. Workstream A: Public Registry Beta

### 3.1 Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    Public Registry                            │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐    │
│  │  Search  │  │  Bundle  │  │   Auth   │  │  Audit   │    │
│  │  Index   │  │  Store   │  │  (OAuth) │  │   Log    │    │
│  └──────────┘  └──────────┘  └──────────┘  └──────────┘    │
│  ┌──────────────────────────────────────────────────────┐   │
│  │          Web UI (pekohub.org)                    │   │
│  └──────────────────────────────────────────────────────┘   │
└──────────────────────────┬──────────────────────────────────┘
                           │ HTTPS / OCI
┌──────────────────────────┴──────────────────────────────────┐
│                    CLI v2.0                                   │
│  push │ pull │ search │ auth login │ agent install           │
└─────────────────────────────────────────────────────────────┘
```

### 3.2 Key Design Decisions

| Decision | Rationale |
|----------|-----------|
| OCI Distribution Spec v1.1 | Existing client (`src/registry/`) already speaks this; ecosystem compatibility |
| PostgreSQL for metadata | Queryable, transactional, supports full-text search (pg_trgm) |
| Filesystem blob store initially | Simplest to deploy; migrate to S3 when scale demands |
| OAuth 2.0 only (no email/password initially) | Reduces security surface; GitHub/Google cover 99% of developers |
| Namespace = GitHub username initially | Simple, no custom namespace disputes |

### 3.3 CLI Commands

```bash
# Authentication
pekobot auth login                    # OAuth browser flow
pekobot auth logout                   # Clear credentials
pekobot auth status                   # Show logged-in user

# Discovery
pekobot search "github assistant"     # Search registry
pekobot agent info user/agent:1.0     # Show bundle metadata

# Publishing (aliases for push/pull)
pekobot agent publish my-agent        # Push to public registry
pekobot agent install user/agent:1.0  # Pull + import in one step
```

---

## 4. Workstream B: Shared Services Fabric

### 4.1 Architecture

```
┌─────────────┐     gRPC/Unix Socket     ┌─────────────────────┐
│   Agent A   │◄────────────────────────►│                     │
│   (Router)  │                          │   MCP Proxy         │
└─────────────┘                          │   (Unified API)     │
                                         │                     │
┌─────────────┐     ┌──────────────┐    │  ┌──┐ ┌──┐ ┌──┐   │
│   Agent B   │────►│   Fabric     │◄───┘  │Br│ │VD│ │Me│   │
│(Researcher) │     │  Control     │       │ow│ │ct│ │mo│   │
└─────────────┘     │   Plane      │       │se│ │or│ │ry│   │
                    └──────────────┘       │r │ │DB│ │  │   │
                           │               └──┘ └──┘ └──┘   │
                    ┌──────┴──────┐                         │
                    │  Prometheus │    Resource Metrics       │
                    │   + Logs    │    & Audit Trail          │
                    └─────────────┘                         │
```

### 4.2 Service Types

| Service | Backend | Tenant Isolation | MCP Tools Exposed |
|---------|---------|-----------------|-------------------|
| Browser | Playwright | Context per tenant | `browser.navigate`, `browser.screenshot`, `browser.pdf` |
| Vector DB | pgvector | Collection prefixing | `vector.insert`, `vector.search`, `vector.delete` |
| Memory (short) | Redis | Key prefixing | `memory.store`, `memory.retrieve` |
| Memory (long) | PostgreSQL | Schema per tenant | `memory.search`, `memory.forget`, `memory.list_topics` |

### 4.3 Configuration

Agents declare fabric services in `team.toml`:

```toml
[fabric.services]
browser = { max_contexts = 5 }
vector = { collections = ["documents", "embeddings"], model = "text-embedding-3" }
memory = { scope = "team" }
```

Or globally in `~/.pekobot/fabric.yaml`:

```yaml
services:
  browser:
    type: browser
    max_contexts: 10
    max_tabs_per_context: 5
  vector:
    type: vector_db
    endpoint: postgres://localhost:5432/pekobot_vectors
    default_model: text-embedding-3
```

### 4.4 Key Design Decisions

| Decision | Rationale |
|----------|-----------|
| Sidecar deployment (localhost gRPC) | Minimal latency; same security boundary as agent |
| MCP Proxy as unified endpoint | Agents already speak MCP; no new protocol to learn |
| pgvector for vector DB | Already need Postgres for metadata; one less dependency |
| Redis for short-term memory | Fast TTL-based eviction; perfect for session cache |
| Reference counting + idle timeout | Prevents resource leaks; keeps services warm |

---

## 5. Workstream C: Team Runtime Preview v0.5

### 5.1 Purpose

The Team Runtime Preview demonstrates multi-agent orchestration using shared services. It is intentionally limited to the **supervisor pattern** for v0.5 — pipeline and mesh come in Phase 3.

### 5.2 `team.toml` Format

```toml
name = "research-team"
description = "A multi-agent research team"

[[agents]]
bundle = "pekohub.org/agents/router:v1.0"
role = "coordinator"
replicas = 1

[[agents]]
bundle = "pekohub.org/agents/researcher:v1.0"
role = "worker"
replicas = 2

[coordination]
pattern = "supervisor"

[fabric.services]
browser = {}
vector = { collections = ["research-docs"] }
memory = { scope = "team" }
```

### 5.3 CLI Commands

```bash
pekobot team validate team.toml       # Validate team spec
pekobot team run team.toml            # Execute team locally
pekobot team logs                     # Show structured logs from last run
```

### 5.4 Supervisor Pattern Flow

```
User Task ──► Coordinator ──► Delegate to Worker 1
                    │         (with shared context)
                    │◄────────── Result
                    │
                    ├──► Delegate to Worker 2
                    │◄────────── Result
                    │
                    └──► Synthesize & Return Final Answer
```

All agents share:
- Session state (via shared memory service)
- Browser contexts (via fabric browser pool)
- Vector collections (via fabric vector DB)
- Unified SSE output stream (tagged by agent)

---

## 6. Cross-Cutting Concerns

### 6.1 Security

| Concern | Approach | Success Criteria |
|---------|----------|-----------------|
| Registry HTTPS | TLS 1.3 only | SEC-001 |
| Bundle signing | ed25519 DID keys (Phase 1 deferred) | REG-020, REG-021 |
| Tenant isolation | Database-level separation (schemas/collections) | SEC-002 |
| Browser isolation | Separate OS processes per tenant | SEC-003 |
| Token expiry | 24-hour expiry + revocation | SEC-004 |
| Audit logs | Append-only with tamper-evident hashing | SEC-005 |
| Vulnerability scans | Scan on push, block critical | SEC-006 |

### 6.2 Performance Targets

| Metric | Target | Verification |
|--------|--------|-------------|
| Registry concurrent ops | 100 push/pull, p95 < 2s | Load test |
| Fabric concurrent agents | 20 agents, no degradation | Stress test |
| Browser cold allocation | < 3 seconds | Benchmark |
| Browser warm allocation | < 500ms | Benchmark |
| Vector search (100K) | < 200ms | Benchmark |
| Memory read/write | < 50ms (short), < 200ms (long) | Benchmark |
| 3-agent team E2E | < 30s (excl. LLM latency) | Integration test |
| Registry uptime | ≥ 99.5% monthly | Monitoring |
| Service crash recovery | < 30s | Chaos test |

### 6.3 Developer Experience

| Deliverable | Target | Owner |
|-------------|--------|-------|
| Public registry live | `pekohub.org` | Registry team |
| Community bundles | ≥ 50 published | Community |
| Fabric documentation | Architecture + config + MCP integration | Docs |
| Tutorial series | 3+ tutorials (multi-agent team, publishing, shared services) | Docs |
| Team runtime docs | `team.toml` reference + coordination patterns | Docs |
| CLI getting-started wizard | Interactive `pekobot init` for registry + team | CLI |

---

## 7. Success Metrics & KPIs

Phase 2 is successful when all P0 criteria are met AND:

| Metric | Target | Measurement |
|--------|--------|-------------|
| Published bundles | ≥ 50 community + 10 official | Registry API |
| Monthly active users | ≥ 500 | OAuth events |
| Total pulls | ≥ 10,000 cumulative | Analytics |
| Active fabric instances | ≥ 20 concurrent agents | Prometheus |
| Fabric uptime | ≥ 99.5% | Monitoring |
| Team executions | ≥ 100 unique | Execution logs |
| Test coverage | ≥ 75% | Coverage report |
| Documentation pages | ≥ 30 | docs site |
| GitHub stars | ≥ 5,000 | GitHub API |
| Active contributors | ≥ 30 | Commit log |

---

## 8. Out of Scope (Deferred to Phase 3)

| Feature | Rationale |
|---------|-----------|
| Enterprise Governance (RBAC, SSO, audit dashboards) | Needs enterprise customers |
| Cloud Runtime SaaS | Needs operational infrastructure + cost modeling |
| A2A Protocol v1.0 | Spec still stabilizing |
| microVM isolation (gVisor/Firecracker) | Benchmark in Phase 2.5 first |
| Registry monetization | Needs market validation |
| Federated registries | Single registry must mature first |
| Agent commerce / payments | Market not mature |

---

## 9. Definition of Done

Phase 2 is complete when:

1. All P0 success criteria (REG-001 through TEAM-010) are implemented, tested, and documented
2. All quantitative KPIs meet or exceed targets
3. Public Registry is live at `pekohub.org` with ≥ 50 community bundles
4. Shared Services Fabric is deployable via single-command installation
5. A public demo showcases a 3-agent team using shared browser and vector DB
6. At least 3 external teams have deployed multi-agent systems on the platform
7. Phase 2 retrospective is published

---

## 10. Document Reference

| Document | Purpose |
|----------|---------|
| `Phase2_Success_Criteria_Public_Registry_Shared_Services_Fabric.md` | Complete, detailed success criteria with numbered requirements (REG-001 through FAB-037, TEAM-001 through TEAM-015, PERF-001 through PERF-010, SEC-001 through SEC-008, DX-001 through DX-007) |
| `Phase2_Roadmap.md` (this document) | Implementation roadmap: execution order, milestones, architecture diagrams, design decisions |

---

*End of Phase 2 Roadmap v1.0*
