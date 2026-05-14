# Phase 2 Success Criteria: Public Registry Beta + Shared Services Fabric

> **Version**: 1.0  
> **Phase**: 6–12 Months  
> **Predecessor**: Phase 1 (Runtime Engine v1.0 + CLI v1.0 + Agent Bundle Spec v1.0)  
> **Objective**: Enable agent discovery and sharing at scale through a public registry, while introducing shared infrastructure services that reduce resource duplication across agent teams.  
> **Companion Document**: See `Phase2_Roadmap.md` for the implementation roadmap, execution order, milestones, and architecture diagrams.

---

## 1. Overview

Phase 2 transitions the Agent Runtime from a **local developer tool** into a **distributed platform**. This phase builds upon the stable foundation of Phase 1 to address two critical market gaps identified in the research:

| Deliverable | Market Gap Addressed | Research Source |
|-------------|---------------------|-----------------|
| **Public Registry Beta** | "Agent Registry/Hub ecosystem almost blank" — no Docker Hub for Agents exists | Dim05, Insight 2 |
| **Shared Services Fabric** | "Each agent duplicates infrastructure" — browsers, vector DBs, memory systems are heavy and wasteful when per-agent | Dim06, Dim09, Insight 7 |

**Phase 2 is considered successful only when all P0 criteria are met AND the platform demonstrates multi-agent orchestration with shared services in production-like conditions.**

---

## 2. Assumptions from Phase 1

Phase 2 depends on the following Phase 1 outputs being production-stable:

| Dependency | Minimum Version | Status Gate |
|------------|----------------|-------------|
| Runtime Engine | v1.0.0-rc1+ | All P0 criteria met, 1,024 unit tests passing |
| CLI | v1.0.0-rc1+ | Core commands stable (`agent`, `team`, `ext`, `session`, `send`, `config`, `daemon`) |
| Agent Bundle Spec | v1.0 | OCI-compatible, `.agent`/`.team`/`.ext` formats with SHA-256 checksums |
| Registry Client | v1.0.0-rc1+ | Push/pull with bearer/basic auth, layer deduplication |
| Extension Framework | v1.0.0-rc1+ | 22 hook points, 6 extension types, dynamic registration |

> **Note**: Base image inheritance was removed per ADR-027. The canonical workflow is `peko agent create` → `peko agent export`.

---

## 3. Public Registry Beta v1.0

### 3.1 Purpose

The Public Registry is the discovery and distribution layer for Agent Bundles. It addresses the "almost blank" state of the agent registry ecosystem — currently, only Microsoft Copilot Studio (70+ agents) and Salesforce AgentExchange offer curated collections, but neither is cross-platform or open. The Public Registry must become the "Docker Hub for Agents."

### 3.2 P0 — Must Have

#### 3.2.1 Registry Core Infrastructure
- [ ] **REG-001**: Registry MUST implement the OCI Distribution Spec v1.1 for push/pull/tag operations, ensuring any OCI-compliant client (docker, oras, CLI) can interact with it
- [ ] **REG-002**: Registry MUST support manifest listing for bundle discovery, including support for tag lists, digest resolution, and multi-arch/index manifests
- [ ] **REG-003**: Registry MUST persist bundle metadata (manifest.json content) in a queryable database (PostgreSQL or equivalent) enabling search and filtering
- [ ] **REG-004**: Registry MUST implement content-addressable storage — all blob uploads are de-duplicated via digest, and layer re-use is automatic across bundles
- [ ] **REG-005**: Registry MUST support garbage collection of unreferenced blobs with configurable retention policies
- [ ] **REG-006**: Registry MUST expose a REST API (OpenAPI 3.1 documented) with the following endpoints:
  - `GET /v2/_catalog` — List all bundle namespaces
  - `GET /v2/{namespace}/bundles/tags/list` — List tags for a bundle
  - `GET /v2/{namespace}/bundles/manifests/{reference}` — Pull a manifest
  - `PUT /v2/{namespace}/bundles/manifests/{reference}` — Push a manifest
  - `GET /api/v1/search?q={query}` — Full-text search across bundles
  - `GET /api/v1/bundles/{namespace}/{name}` — Bundle metadata and README
  - `GET /api/v1/bundles/{namespace}/{name}/versions` — Version history
- [ ] **REG-007**: Registry MUST authenticate users via OAuth 2.0 (GitHub, Google, or equivalent) and support organization/team namespaces
- [ ] **REG-008**: Registry MUST enforce namespace ownership — only authenticated owners of a namespace can push or delete bundles within it

#### 3.2.2 Bundle Discovery & Search
- [ ] **REG-009**: Registry MUST index the following searchable fields from bundle manifests: name, description, author, tags/keywords, required MCP servers, supported model providers, skill definitions, and base image references
- [ ] **REG-010**: Registry MUST provide full-text search with relevance ranking across all indexed fields, supporting phrase matching and boolean operators (`AND`, `OR`, `NOT`)
- [ ] **REG-011**: Registry MUST support faceted search — users can filter by: model provider (OpenAI/Anthropic/local), MCP server dependencies, category (research/support/development/content), license, and base image
- [ ] **REG-012**: Registry MUST auto-generate a public HTML page for every published bundle containing: metadata, README, version history, dependency tree, installation command, and usage statistics (pull count, star count)
- [ ] **REG-013**: Registry MUST expose a public Web UI (`https://pekohub.org`) with: homepage featuring trending bundles, category browsing, search with autocomplete, bundle detail pages, and user profile pages
- [ ] **REG-014**: Web UI MUST be responsive and functional on mobile devices (viewport ≥ 375px)

#### 3.2.3 Bundle Management
- [ ] **REG-015**: Registry MUST support semantic versioning (MAJOR.MINOR.PATCH) with `latest` tag resolution and version constraint syntax (`>=1.0.0`, `^1.2.0`, `~1.2.3`)
- [ ] **REG-016**: Registry MUST maintain version history with immutable manifests — once published, a manifest digest can never be modified (following OCI immutability guarantees)
- [ ] **REG-017**: Registry MUST support bundle deprecation — owners can mark versions as deprecated with a redirect message to a replacement bundle
- [ ] **REG-018**: Registry MUST provide download/pull statistics (daily, weekly, monthly, all-time) per bundle and per version
- [ ] **REG-019**: Registry MUST support bundle forking — any authenticated user can fork a public bundle to their own namespace, preserving provenance metadata

#### 3.2.4 Security & Trust
- [ ] **REG-020**: Registry MUST verify bundle signatures (`signature.json`) on push and reject bundles with invalid or missing signatures
- [ ] **REG-021**: Registry MUST support Sigstore/cosign signing as an alternative to the built-in signature scheme, enabling keyless signing via OIDC
- [ ] **REG-022**: Registry MUST implement vulnerability scanning for bundle layers — report known CVEs in bundled tool binaries or Python/Node.js dependencies
- [ ] **REG-023**: Registry MUST enforce rate limits: anonymous users 100 pulls/hour, authenticated users 1,000 pulls/hour, with configurable limits per namespace
- [ ] **REG-024**: Registry MUST provide an audit log of all push, pull, delete, and permission-change events per namespace, accessible to namespace owners

#### 3.2.5 CLI Integration
- [ ] **REG-025**: CLI `peko agent push` MUST integrate with the Public Registry as the default endpoint, requiring only `peko auth login` for authentication
- [ ] **REG-026**: CLI `peko agent pull` MUST resolve bundle references from the Public Registry (e.g. `peko agent pull pekohub.org/user/researcher:v1.0`)
- [ ] **REG-027**: CLI MUST implement `peko search <query>` command that queries the Registry search API and displays results with metadata in a terminal-friendly table
- [ ] **REG-028**: CLI MUST implement `peko agent info <registry-ref>` command that displays bundle metadata without downloading

> **CLI Naming Note**: The CLI uses `peko` as the binary name (not `agent`). Commands are `peko agent push`, `peko agent pull`, `peko search`, etc. See `Phase2_Roadmap.md` §3.3 for the full CLI command reference.

### 3.3 P1 — Should Have

- [ ] **REG-029**: Registry SHOULD support "Verified Publisher" badges for organizations that complete identity verification (domain ownership, public profile)
- [ ] **REG-030**: Registry SHOULD implement a review/rating system (1–5 stars + text review) for published bundles
- [ ] **REG-031**: Registry SHOULD support "Collections" — curated groups of bundles with a shared theme or use case (e.g. "Customer Support Team", "Research Toolkit")
- [ ] **REG-032**: Registry SHOULD provide an embeddable badge (`https://pekohub.org/badges/{namespace}/{name}/downloads`) for README files
- [ ] **REG-033**: Registry SHOULD support webhook notifications — notify external systems on push, pull, or security scan events
- [ ] **REG-034**: Registry SHOULD implement a public API rate limit increase program for open-source projects and educational institutions

### 3.4 P2 — Nice to Have

- [ ] **REG-035**: Registry COULD support "Agent Teams" as first-class publishable entities — a `team.toml` + multiple bundle references published as a single artifact
- [ ] **REG-036**: Registry COULD provide analytics dashboards for publishers (geographic distribution of pulls, provider breakdown, dependency graphs)
- [ ] **REG-037**: Registry COULD implement an "Agent of the Week" editorial program featuring community-contributed bundles

---

## 4. Shared Services Fabric v1.0

### 4.1 Purpose

The Shared Services Fabric addresses a critical inefficiency: **every agent instance currently duplicates heavy infrastructure** (browser instances, vector databases, embedding models, long-term memory systems). The Fabric provides a multi-tenant, reference-counted infrastructure layer where agents share expensive resources securely. Research identified this as a key differentiator with significant engineering complexity (Insight 7).

### 4.2 P0 — Must Have

#### 4.2.1 Architecture & Service Model
- [ ] **FAB-001**: Fabric MUST be deployed as a set of sidecar services alongside the Runtime Engine, communicating via localhost gRPC or Unix sockets
- [ ] **FAB-002**: Fabric MUST support multi-tenancy — multiple agent instances from different teams can share the same Fabric deployment with namespace isolation
- [ ] **FAB-003**: Fabric MUST implement reference counting for all shared services — services are started on first use, kept warm while referenced, and shut down after a configurable idle timeout (default: 5 minutes)
- [ ] **FAB-004**: Fabric MUST expose a unified control API (`/fabric/v1/services`) for: listing available services, checking service health, allocating a service instance to an agent, and releasing a service instance
- [ ] **FAB-005**: Fabric MUST be configurable via a `fabric.yaml` file with: service definitions, resource limits per service type, scaling policies, and tenant isolation rules

#### 4.2.2 Shared Browser Service
- [ ] **FAB-006**: Fabric MUST provide a shared browser pool using Playwright or Puppeteer with the following features:
  - Persistent browser contexts per tenant (cookies, localStorage, session state isolated)
  - Concurrent tab management — multiple agents can use different tabs within the same browser context
  - Automatic session cleanup after agent termination
  - Screenshot and PDF capture capability
- [ ] **FAB-007**: Browser contexts MUST be isolated per tenant — no cross-tenant cookie or localStorage leakage
- [ ] **FAB-008**: Browser pool MUST support configurable limits: max concurrent contexts (default: 10), max tabs per context (default: 5), max page load timeout (default: 30s)
- [ ] **FAB-009**: Browser service MUST expose health metrics: active contexts, tab count, average page load time, memory usage
- [ ] **FAB-010**: Runtime Engine MUST integrate with the browser service via an MCP server wrapper (`@agent-runtime/mcp-browser`), making browser tools available to any agent without per-agent browser initialization

#### 4.2.3 Shared Vector Database Service
- [ ] **FAB-011**: Fabric MUST provide a shared vector database using pgvector (PostgreSQL extension) with the following features:
  - Tenant-scoped collections — each tenant has isolated vector collections with namespace prefixing
  - Support for multiple embedding models (OpenAI text-embedding-3, sentence-transformers, Ollama embeddings) with model-versioned collection naming
  - CRUD operations: insert, query (with similarity threshold), delete, list collections
  - Automatic index creation (IVFFlat or HNSW) based on collection size
- [ ] **FAB-012**: Vector queries MUST be scoped to the requesting tenant — no cross-tenant data access is possible
- [ ] **FAB-013**: Vector DB service MUST support configurable limits: max collections per tenant (default: 20), max vectors per collection (default: 100,000), max query results (default: 10)
- [ ] **FAB-014**: Runtime Engine MUST expose vector operations to agents via an MCP server wrapper (`@agent-runtime/mcp-vector`) with standardized insert/search/delete tools
- [ ] **FAB-015**: Vector DB service MUST persist data to durable storage (PostgreSQL) with configurable backup intervals

#### 4.2.4 Shared Memory Service
- [ ] **FAB-016**: Fabric MUST provide a shared memory service with three tiers:
  - **Short-term memory**: Redis-backed session cache with TTL (default: 1 hour), scoped to a single agent execution
  - **Long-term memory**: PostgreSQL-backed persistent store for agent-specific facts, preferences, and learned behaviors
  - **Shared knowledge**: Read-only access to vector DB collections for cross-agent knowledge retrieval
- [ ] **FAB-017**: Memory service MUST support namespace isolation — agent A cannot read or write agent B's memory without explicit capability grants
- [ ] **FAB-018**: Memory service MUST implement an MCP server (`@agent-runtime/mcp-memory`) exposing: `memory.store`, `memory.retrieve`, `memory.search`, `memory.forget`, `memory.list_topics`
- [ ] **FAB-019**: Memory service MUST support context window optimization — automatically summarize and compress old memory entries when approaching context limits
- [ ] **FAB-020**: Memory service MUST provide memory export/import functionality for agent migration and backup

#### 4.2.5 MCP Proxy & Service Discovery
- [ ] **FAB-021**: Fabric MUST include an MCP Proxy that acts as a single MCP server endpoint for agents, routing tool calls to the appropriate shared service
- [ ] **FAB-022**: MCP Proxy MUST auto-register all running shared services and expose their tools under a unified namespace (e.g. `browser.navigate`, `vector.insert`, `memory.store`)
- [ ] **FAB-023**: MCP Proxy MUST handle service lifecycle events — if a shared service restarts, the proxy must transparently reconnect without agent intervention
- [ ] **FAB-024**: Bundle `runtime.yaml` MUST support `fabric.services` declaration — agents specify which shared services they need, and the Fabric allocates them on startup:
  ```yaml
  fabric:
    services:
      - browser
      - vector:
          collections: [documents, embeddings]
      - memory:
          scope: team
  ```
- [ ] **FAB-025**: Fabric MUST support health checks and automatic restart for all shared services, with a maximum restart backoff of 5 minutes

#### 4.2.6 Resource Governance
- [ ] **FAB-026**: Fabric MUST enforce per-tenant resource quotas: max concurrent browser contexts, max vector collections, max memory entries, max total memory per tenant
- [ ] **FAB-027**: Fabric MUST expose Prometheus-compatible metrics for all services: active instances, request latency (p50/p95/p99), error rate, memory usage, CPU usage
- [ ] **FAB-028**: Fabric MUST write structured audit logs for all service allocations, releases, and quota violations with tenant identification
- [ ] **FAB-029**: Fabric MUST implement circuit breaker patterns for shared services — if a service fails repeatedly, new requests are fast-failed until recovery

### 4.3 P1 — Should Have

- [ ] **FAB-030**: Fabric SHOULD support a shared code execution sandbox (E2B-compatible API) for running untrusted code in Firecracker microVMs
- [ ] **FAB-031**: Fabric SHOULD support horizontal scaling of individual services via a pluggable backend (Kubernetes StatefulSets or Docker Compose replicas)
- [ ] **FAB-032**: Fabric SHOULD support warm pools for browser contexts — pre-initialized contexts ready for immediate allocation, reducing agent start time to <1 second
- [ ] **FAB-033**: Fabric SHOULD provide a web dashboard (`/fabric/dashboard`) showing real-time service status, tenant quotas, and resource utilization
- [ ] **FAB-034**: Fabric SHOULD support custom service plugins — third-party shared services can register with the Fabric via a standard plugin interface

### 4.4 P2 — Nice to Have

- [ ] **FAB-035**: Fabric COULD support GPU sharing for local embedding model inference, allocating GPU time slices across tenants
- [ ] **FAB-036**: Fabric COULD implement cross-tenant knowledge sharing with explicit capability grants ("Team B can read Team A's vector collection 'public-docs'")
- [ ] **FAB-037**: Fabric COULD support geographic service placement — route browser sessions through region-specific proxies for localized content access

---

## 5. Team Runtime Preview v0.5

### 5.1 Purpose

Phase 2 introduces the foundation for multi-agent orchestration — the **Team Spec** format and basic team execution. This is a preview release (v0.5) because full Team Runtime is the centerpiece of Phase 3, but Shared Services Fabric requires multi-agent scenarios to demonstrate its value.

### 5.2 P0 — Must Have

#### 5.2.1 Team Spec Format
- [ ] **TEAM-001**: Team Spec MUST be a declarative `team.toml` file with the following sections:
  - `name` and `description` — Team identity
  - `agents` — List of agent bundle references with role assignments:
    ```toml
    [[agents]]
    bundle = "pekohub.org/agents/router:v1.0"
    role = "coordinator"
    replicas = 1
    
    [[agents]]
    bundle = "pekohub.org/agents/researcher:v1.0"
    role = "worker"
    replicas = 2
    ```
  - `coordination` — Pattern selection: `supervisor`, `pipeline`, or `mesh`
  - `shared_services` — Fabric services required by the team
  - `communication` — Message bus configuration (in-memory for v0.5)
- [ ] **TEAM-002**: Team Spec MUST support three coordination patterns:
  - **Supervisor**: One coordinator agent delegates tasks to worker agents and aggregates results
  - **Pipeline**: Agents connected in a DAG, output of agent N is input to agent N+1
  - **Mesh**: All agents can broadcast messages to each other with topic-based routing
- [ ] **TEAM-003**: Team Spec MUST be validated by the CLI (`peko team validate`) before execution
- [ ] **TEAM-004**: Team Spec MUST reference agent bundles from the Public Registry or local filesystem, resolving dependencies automatically

#### 5.2.2 Team Execution
- [ ] **TEAM-005**: Runtime Engine MUST support loading a `team.toml` and instantiating all declared agents with their configured roles
- [ ] **TEAM-006**: Runtime Engine MUST implement the **supervisor pattern** as the primary coordination mode for v0.5:
  - Coordinator receives a task
  - Coordinator decides which worker(s) to invoke
  - Workers execute with access to shared services
  - Coordinator synthesizes results and produces final output
- [ ] **TEAM-007**: Runtime Engine MUST provide shared context across team agents — all agents in a team share the same session state and can access shared memory topics
- [ ] **TEAM-008**: Runtime Engine MUST expose a unified output stream for the team — progress from all agents is merged into a single SSE/WebSocket stream with agent tagging
- [ ] **TEAM-009**: CLI MUST implement `peko team run team.toml` command for local team execution
- [ ] **TEAM-010**: CLI MUST implement `peko team logs` command to display structured logs from the most recent team execution, with per-agent filtering

### 5.3 P1 — Should Have

- [ ] **TEAM-011**: Pipeline pattern SHOULD be supported for linear multi-step workflows (e.g. Research → Analysis → Writing)
- [ ] **TEAM-012**: Mesh pattern SHOULD be supported with basic topic-based message routing
- [ ] **TEAM-013**: Team execution SHOULD emit OpenTelemetry spans for each agent invocation, enabling distributed tracing of multi-agent workflows

### 5.4 P2 — Nice to Have

- [ ] **TEAM-014**: Team execution COULD support dynamic agent scaling — adding or removing worker replicas during execution based on workload
- [ ] **TEAM-015**: Team execution COULD support human-in-the-loop checkpoints — pause execution at specified points for human approval

---

## 6. Performance & Reliability

### 6.1 P0 — Must Have

- [ ] **PERF-001**: Registry MUST handle 100 concurrent push/pull operations with p95 latency < 2 seconds for manifests and < 10 seconds for 100MB bundles
- [ ] **PERF-002**: Shared Services Fabric MUST support 20 concurrent agent instances sharing browser, vector DB, and memory services without degradation
- [ ] **PERF-003**: Browser context allocation latency MUST be < 3 seconds (cold) and < 500ms (warm pool)
- [ ] **PERF-004**: Vector DB similarity search MUST complete in < 200ms for collections up to 100,000 vectors
- [ ] **PERF-005**: Memory read/write latency MUST be < 50ms for short-term memory and < 200ms for long-term memory
- [ ] **PERF-006**: Team Runtime supervisor pattern MUST execute a 3-agent team end-to-end in < 30 seconds (excluding LLM API latency)
- [ ] **PERF-007**: Registry uptime MUST be ≥ 99.5% (measured monthly)
- [ ] **PERF-008**: All Fabric services MUST recover automatically from process crashes within 30 seconds

### 6.2 P1 — Should Have

- [ ] **PERF-009**: Registry SHOULD scale horizontally to 1,000 concurrent operations via load balancing
- [ ] **PERF-010**: Fabric SHOULD support 100 concurrent agents with < 20% performance degradation

---

## 7. Security

### 7.1 P0 — Must Have

- [ ] **SEC-001**: Registry MUST enforce HTTPS-only communication with TLS 1.3
- [ ] **SEC-002**: All tenant data in shared services MUST be isolated at the database level (separate schemas/collections per tenant)
- [ ] **SEC-003**: Fabric browser contexts MUST run in separate OS processes with no shared memory between tenants
- [ ] **SEC-004**: Registry access tokens MUST expire within 24 hours and support revocation
- [ ] **SEC-005**: All audit logs MUST be append-only with tamper-evident hashing (Merkle tree or equivalent)
- [ ] **SEC-006**: Bundle vulnerability scans MUST complete within 5 minutes of push and block pull of critical-severity bundles until acknowledged

### 7.2 P1 — Should Have

- [ ] **SEC-007**: Registry SHOULD support private namespaces with granular collaborator permissions (read, write, admin)
- [ ] **SEC-008**: Fabric SHOULD support network policies restricting inter-service communication to declared dependencies

---

## 8. Developer Experience

### 8.1 P0 — Must Have

- [ ] **DX-001**: Public Registry Web UI MUST be publicly accessible at `https://pekohub.org` with no authentication required for browsing and search
- [ ] **DX-002**: At least 50 community-contributed bundles MUST be published on the Public Registry by the end of Phase 2
- [ ] **DX-003**: Complete documentation for Shared Services Fabric architecture, configuration, and MCP server integration
- [ ] **DX-004**: A tutorial series (3+ tutorials) covering: "Building a multi-agent research team", "Publishing and sharing agent bundles", "Configuring shared services for your team"
- [ ] **DX-005**: Team Runtime documentation with `team.toml` reference and coordination pattern guides

### 8.2 P1 — Should Have

- [ ] **DX-006**: A "Getting Started" wizard in the CLI that guides new users through registry login, bundle publishing, and team creation
- [ ] **DX-007**: Video walkthroughs (3+ videos) of Registry usage and Shared Services setup

---

## 9. Success Metrics & KPIs

Phase 2 is considered **successful** when all P0 criteria are met AND:

| Metric | Target | Measurement Method |
|--------|--------|-------------------|
| **Registry: Published Bundles** | ≥ 50 (community) + ≥ 10 (official) | Registry API count |
| **Registry: Monthly Active Users** | ≥ 500 unique users | OAuth login events |
| **Registry: Total Pulls** | ≥ 10,000 cumulative | Registry analytics |
| **Fabric: Active Service Instances** | ≥ 20 concurrent agents in production | Prometheus metrics |
| **Fabric: Service Uptime** | ≥ 99.5% | Uptime monitoring |
| **Team Runtime: Teams Executed** | ≥ 100 unique team executions | Execution logs |
| **Test Coverage** | ≥ 75% | Code coverage report |
| **Documentation Pages** | ≥ 30 pages | docs site count |
| **GitHub Stars** | ≥ 5,000 | GitHub API |
| **Active Contributors** | ≥ 30 unique | GitHub commit log |
| **Response Time (p95)** | Registry <2s, Fabric <500ms | Load testing |

---

## 10. Out of Scope (Explicitly Deferred)

| Feature | Rationale | Target Phase |
|---------|-----------|-------------|
| **Enterprise Governance Layer** | RBAC, SSO, audit dashboards — requires enterprise customers and Phase 3 maturity | Phase 3 |
| **Cloud Runtime Service (SaaS)** | Managed multi-tenant cloud offering — requires operational infrastructure and cost modeling | Phase 3 |
| **A2A Protocol Support** | Agent-to-Agent protocol is still stabilizing (v1.0 expected early 2026) — wait for final spec | Phase 3 |
| **Advanced Security (microVM isolation)** | gVisor/Firecracker for Fabric services — benchmark first in Phase 2.5 | Phase 3 |
| **Registry Monetization** | Verified Publisher fees, Private Registry SaaS — needs market validation first | Phase 3 |
| **Federated Registries** | Cross-registry search and mirroring — single registry must mature first | Phase 3+ |
| **Agent Commerce / Payments** | Visa/Mastercard integration — market not mature, protocol not finalized | Phase 3+ |

---

## 11. Architecture Overview

### 11.1 Phase 2 System Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    Public Registry                            │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐    │
│  │  Search  │  │  Bundle  │  │   Auth   │  │  Audit   │    │
│  │  Index   │  │  Store   │  │  (OAuth) │  │   Log    │    │
│  └──────────┘  └──────────┘  └──────────┘  └──────────┘    │
│  ┌──────────────────────────────────────────────────────┐   │
│  │          Web UI (pekohub.org)                     │   │
│  └──────────────────────────────────────────────────────┘   │
└──────────────────────────┬──────────────────────────────────┘
                           │ HTTPS / OCI
┌──────────────────────────┴──────────────────────────────────┐
│                    CLI v2.0                                   │
│  push │ pull │ search │ team run │ fabric status             │
├──────────────────────────┬──────────────────────────────────┤
│            Runtime Engine v2.0                                │
│  ┌──────────────────────────────────────────────────────┐   │
│  │         Shared Services Fabric                        │   │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────────────┐   │   │
│  │  │ Browser  │  │  Vector  │  │     Memory       │   │   │
│  │  │  Pool    │  │    DB    │  │   (3-tier)       │   │   │
│  │  └──────────┘  └──────────┘  └──────────────────┘   │   │
│  │  ┌──────────────────────────────────────────────────┐ │   │
│  │  │        MCP Proxy (Unified Endpoint)              │ │   │
│  │  └──────────────────────────────────────────────────┘ │   │
│  └──────────────────────────────────────────────────────┘   │
│  ┌──────────────────────────────────────────────────────┐   │
│  │         Team Runtime (v0.5 Preview)                   │   │
│  │  team.toml → Agent Loader → Supervisor → Shared Bus  │   │
│  └──────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
```

### 11.2 Shared Services Data Flow

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

---

## 12. Definition of Done

Phase 2 is **officially complete** when:

1. ✅ All P0 success criteria (REG-001 through TEAM-010) are implemented, tested, and documented
2. ✅ All 11 quantitative KPIs meet or exceed their targets
3. ✅ Public Registry is live at `https://pekohub.org` with ≥ 50 community bundles
4. ✅ Shared Services Fabric is deployable via single-command installation (`curl -sSL ... | bash`)
5. ✅ A public demo showcases a 3-agent team (coordinator + 2 workers) using shared browser and vector DB services
6. ✅ At least 3 external teams (outside the core dev team) have successfully deployed multi-agent systems using the platform
7. ✅ Phase 2 retrospective document is published, capturing lessons learned and Phase 3 priorities

---

*End of Phase 2 Success Criteria Document*
