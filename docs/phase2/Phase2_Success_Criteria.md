# Phase 2 Success Criteria: Public Registry Beta + Extension Ecosystem

> **Version**: 2.1  
> **Phase**: 4–5 Months  
> **Predecessor**: Phase 1 (Runtime Engine v1.0 + CLI v1.0 + Agent Bundle Spec v1.0)  
> **Objective**: Enable agent discovery and sharing at scale through a public registry, while maturing the extension ecosystem so that advanced behaviors (including multi-agent orchestration) are implemented as extensions, not core runtime features.  
> **Companion Document**: See `Phase2_Roadmap.md` for the implementation roadmap, execution order, milestones, and architecture diagrams.

---

## 1. Overview

Phase 2 transitions the Agent Runtime from a **local developer tool** into a **distributed platform for agent publishing and extensible multi-agent collaboration**. This phase builds upon the stable foundation of Phase 1 to address two critical market gaps identified in the research:

| Deliverable | Market Gap Addressed | Research Source |
|-------------|---------------------|-----------------|
| **Public Registry Beta** | "Agent Registry/Hub ecosystem almost blank" — no Docker Hub for Agents exists | Dim05, Insight 2 |
| **Extension Ecosystem** | "Single agents cannot solve complex multi-step problems" — but the solution should be composable extensions, not monolithic core features | Dim09, Insight 7 |

**Phase 2 is considered successful only when all P0 criteria are met AND the platform demonstrates multi-agent team orchestration via installable extensions.**

> **Design Principle**: Phase 2 does NOT add team orchestration, memory management, or coordination patterns to the core runtime. These are **extension concerns**. The core runtime provides the 22 hook points, the A2A event bus, and built-in tools — extensions compose these primitives into higher-level behaviors. See §10 for the full rationale.

> **Note on Shared Services Fabric**: The Shared Services Fabric (shared browser pool, vector DB, memory tiers) was originally scoped for Phase 2 but has been **deferred to Phase 3 / Cloud Runtime**. Agents and teams continue to use MCP servers and built-in tools for browser, vector, and memory operations — exactly as they do in Phase 1.

---

## 2. Assumptions from Phase 1

Phase 2 depends on the following Phase 1 outputs being production-stable:

| Dependency | Minimum Version | Status Gate |
|------------|----------------|-------------|
| Runtime Engine | v0.1.0+ | All P0 criteria met, 1,024 unit tests passing |
| CLI | v0.1.0+ | Core commands stable (`agent`, `team`, `ext`, `session`, `send`, `config`, `daemon`) |
| Agent Bundle Spec | v1.0 | OCI-compatible, `.agent`/`.team`/`.ext` formats with SHA-256 checksums |
| Registry Client | v0.1.0+ | Push/pull with bearer/basic auth, layer deduplication |
| Extension Framework | v0.1.0+ | 22 hook points, 6 extension types, dynamic registration |
| IPC Layer (ADR-021) | v0.1.0+ | UDP/Unix socket CLI↔daemon communication |
| A2A Event Bus | v0.1.0+ | In-memory bus with Direct, Task, TaskResult, Broadcast, Subscribe message types |

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
- [ ] **REG-009**: Registry MUST index the following searchable fields from bundle manifests: name, description, author, tags/keywords, required MCP servers, supported model providers, skill definitions, license, and extension type
- [ ] **REG-010**: Registry MUST provide full-text search with relevance ranking across all indexed fields, supporting phrase matching and boolean operators (`AND`, `OR`, `NOT`)
- [ ] **REG-011**: Registry MUST support faceted search — users can filter by: model provider (OpenAI/Anthropic/local), MCP server dependencies, category (research/support/development/content), license, bundle type (agent/team/extension), and extension type (skill/mcp/gateway/universal/general/team)
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

## 4. Extension Ecosystem

### 4.1 Purpose

Phase 2 matures the extension ecosystem by enabling remote installation and publishing of extensions. This includes:

1. **Extension source references** — installing extensions from GitHub, URLs, and MCP endpoints (deferred from Phase 1)
2. **Team orchestration as extensions** — multi-agent coordination patterns implemented via the existing 22 hook points, not core runtime features
3. **A2A tooling** — built-in tools that integrate with the extension framework's event hooks

> **Design Principle**: The core runtime does not implement team orchestration. It provides hook points, an event bus, and built-in tools. Extensions compose these into coordination patterns.

### 4.2 P0 — Must Have

#### 4.2.1 Extension Source References
- [ ] **EXT-001**: Extension installation MUST support source references in addition to local paths and `.ext` files
- [ ] **EXT-002**: Source reference types MUST include at minimum: `github:owner/repo[@ref]`, `https://...` (direct URL), `mcp+https://...` (MCP endpoint), and registry references `pekohub.org/namespace/name:version`
- [ ] **EXT-003**: `peko ext install <source>` MUST resolve source references, download if needed, and install the extension
- [ ] **EXT-004**: Source resolution MUST support GitHub repositories with tag/branch/commit refs
- [ ] **EXT-005**: MCP source references MUST create appropriate server config entries without downloading files
- [ ] **EXT-006**: `peko ext install` MUST cache downloaded files to avoid redundant downloads
- [ ] **EXT-007**: `peko ext update <id>` MUST check for updates and install the latest version
- [ ] **EXT-008**: `peko ext list --outdated` MUST show extensions with available updates

#### 4.2.2 Extension Registry Integration
- [ ] **EXT-009**: `peko ext publish <id>` MUST push an extension `.ext` package to the Public Registry
- [ ] **EXT-010**: `peko ext install <registry-ref>` MUST pull and install an extension from the Public Registry
- [ ] **EXT-011**: Registry MUST index extension metadata including: extension type, hook points used, example configuration, and compatibility information
- [ ] **EXT-012**: Registry Web UI MUST display extension detail pages with hook usage, configuration examples, and installation commands

#### 4.2.3 A2A Built-in Tools (Core)
- [ ] **EXT-013**: The runtime MUST provide `a2a_send` built-in tool that sends a direct message to another agent via the event bus, integrating with the `EventEmit` hook
- [ ] **EXT-014**: The runtime MUST provide `a2a_broadcast` built-in tool that broadcasts a message to all agents on a topic, integrating with the `EventEmit` hook
- [ ] **EXT-015**: The runtime MUST provide `a2a_receive` built-in tool that receives messages from an agent's inbox, integrating with the `EventSubscribe` hook

### 4.3 P1 — Should Have

- [ ] **EXT-016**: `a2a_subscribe` and `a2a_unsubscribe` built-in tools SHOULD allow agents to dynamically manage topic subscriptions
- [ ] **EXT-017**: Extension source references SHOULD support version constraints (semver ranges) for GitHub and registry sources
- [ ] **EXT-018**: `peko ext init --type <type>` SHOULD scaffold a new extension with the appropriate manifest and boilerplate
- [ ] **EXT-019**: Extensions SHOULD be able to declare dependencies on other extensions, with automatic resolution at install time
- [ ] **EXT-020**: A `team` extension type adapter SHOULD be added to `src/extensions/` for declarative team composition via `manifest.yaml`

### 4.4 P2 — Nice to Have

- [ ] **EXT-021**: Extensions COULD support hot-reloading — update extension code without restarting the daemon
- [ ] **EXT-022**: An extension marketplace API COULD enable querying extensions by hook point ("show me all extensions that use `EventSubscribe`")

---

## 5. Team Orchestration via Extensions

### 5.1 Purpose

Team orchestration enables multi-agent coordination for complex tasks. In Phase 2, this is implemented **entirely through extensions** using the existing 22 hook points — no new core runtime features are added.

### 5.2 P0 — Must Have

#### 5.2.1 Team Orchestrator Extensions
- [ ] **TEAM-001**: At least one team orchestrator extension MUST be published to the registry (e.g., `pekohub.org/extensions/supervisor-team`) that implements multi-agent coordination using the extension framework's hooks
- [ ] **TEAM-002**: The supervisor extension MUST implement the following behavior:
  - Hook `AgentInit` to initialize team state and spawn worker agents
  - Hook `EventSubscribe` with topic pattern `team.task.*` to receive task assignments
  - Hook `EventSubscribe` with topic pattern `team.result.*` to collect worker results
  - Hook `ToolRegister` to provide `team_delegate` and `team_synthesize` tools
  - Hook `SessionStateChange` to maintain shared team context
- [ ] **TEAM-003**: The supervisor extension MUST enable a coordinator agent to: receive a task, delegate sub-tasks to workers via `a2a_send` or `team_delegate`, collect results, and synthesize a final answer
- [ ] **TEAM-004**: Worker agents in a team MUST share MCP tools via the existing MCP server sharing mechanism (one process per server, shared across agents)
- [ ] **TEAM-005**: Worker agents in a team MUST share session context via the existing session overlay system

#### 5.2.2 Team Configuration
- [ ] **TEAM-006**: Team orchestrator extensions MUST be configurable via `agent.toml` `[extensions.config.<id>]` section:
  ```toml
  [extensions]
  enabled = ["supervisor-team"]

  [extensions.config.supervisor-team]
  workers = ["researcher", "coder"]
  max_iterations = 10
  ```
- [ ] **TEAM-007**: Team composition MUST be specifiable in a declarative format (either `agent.toml` extension config or a separate `team.toml` parsed by a `team` extension type adapter)
- [ ] **TEAM-008**: `peko ext install` MUST support installing team orchestrator extensions from the registry
- [ ] **TEAM-009**: A public demo MUST showcase a 3-agent team (coordinator + 2 workers) using the supervisor extension, shared MCP tools, and A2A messaging
- [ ] **TEAM-010**: Documentation MUST explain how to install, configure, and use team orchestrator extensions

### 5.3 P1 — Should Have

- [ ] **TEAM-011**: A pipeline extension SHOULD be published implementing linear multi-step workflows (e.g. Research → Analysis → Writing)
- [ ] **TEAM-012**: A mesh extension SHOULD be published implementing topic-based broadcast coordination
- [ ] **TEAM-013**: Team execution SHOULD emit OpenTelemetry spans for each agent invocation, enabling distributed tracing of multi-agent workflows

### 5.4 P2 — Nice to Have

- [ ] **TEAM-014**: Team execution COULD support dynamic agent scaling — adding or removing worker replicas during execution based on workload
- [ ] **TEAM-015**: Team execution COULD support human-in-the-loop checkpoints — pause execution at specified points for human approval

---

## 6. Performance & Reliability

### 6.1 P0 — Must Have

- [ ] **PERF-001**: Registry MUST handle 100 concurrent push/pull operations with p95 latency < 2 seconds for manifests and < 10 seconds for 100MB bundles
- [ ] **PERF-002**: Extension hook invocation overhead MUST be < 1ms per hook for stateless extensions
- [ ] **PERF-003**: A2A message delivery (in-memory bus) MUST complete in < 10ms end-to-end
- [ ] **PERF-004**: A 3-agent team using an orchestrator extension MUST execute end-to-end in < 30 seconds (excluding LLM API latency)
- [ ] **PERF-005**: Registry uptime MUST be ≥ 99.5% (measured monthly)
- [ ] **PERF-006**: Extension installation from GitHub/URL MUST complete in < 30 seconds for packages < 10MB
- [ ] **PERF-007**: `peko ext list` MUST return within 2 seconds for < 50 installed extensions
- [ ] **PERF-008**: The runtime MUST gracefully handle extension crashes — disable the extension, log the error, and continue execution without the extension's functionality

### 6.2 P1 — Should Have

- [ ] **PERF-009**: Registry SHOULD scale horizontally to 1,000 concurrent operations via load balancing
- [ ] **PERF-010**: Team Runtime via extensions SHOULD support 20 concurrent agents with < 20% performance degradation

---

## 7. Security

### 7.1 P0 — Must Have

- [ ] **SEC-001**: Registry MUST enforce HTTPS-only communication with TLS 1.3
- [ ] **SEC-002**: Registry access tokens MUST expire within 24 hours and support revocation
- [ ] **SEC-003**: All audit logs MUST be append-only with tamper-evident hashing (Merkle tree or equivalent)
- [ ] **SEC-004**: Bundle vulnerability scans MUST complete within 5 minutes of push and block pull of critical-severity bundles until acknowledged
- [ ] **SEC-005**: Extensions installed from remote sources MUST be verified against SHA-256 checksums before activation
- [ ] **SEC-006**: MCP servers spawned by extensions MUST have API keys and secrets stripped from environment variables, consistent with Phase 1's credential isolation

### 7.2 P1 — Should Have

- [ ] **SEC-007**: Registry SHOULD support private namespaces with granular collaborator permissions (read, write, admin)
- [ ] **SEC-008**: Extensions SHOULD run with filesystem restrictions — read-only access outside their own directory and the shared workspace

---

## 8. Developer Experience

### 8.1 P0 — Must Have

- [ ] **DX-001**: Public Registry Web UI MUST be publicly accessible at `https://pekohub.org` with no authentication required for browsing and search
- [ ] **DX-002**: At least 50 community-contributed bundles MUST be published on the Public Registry by the end of Phase 2
- [ ] **DX-003**: Complete documentation for extension development — how to use the 22 hook points, with examples for each
- [ ] **DX-004**: A tutorial series (3+ tutorials) covering: "Installing and configuring extensions", "Building a team orchestrator extension", "Publishing extensions to the registry"
- [ ] **DX-005**: Team orchestration documentation explaining how to install, configure, and run multi-agent teams using extensions

### 8.2 P1 — Should Have

- [ ] **DX-006**: A "Getting Started" wizard in the CLI that guides new users through registry login, extension discovery, and team creation
- [ ] **DX-007**: Video walkthroughs (3+ videos) of Registry usage and Extension development

---

## 9. Success Metrics & KPIs

Phase 2 is considered **successful** when all P0 criteria are met AND:

| Metric | Target | Measurement Method |
|--------|--------|-------------------|
| **Registry: Published Bundles** | ≥ 50 (community) + ≥ 10 (official) | Registry API count |
| **Registry: Monthly Active Users** | ≥ 500 unique users | OAuth login events |
| **Registry: Total Pulls** | ≥ 10,000 cumulative | Registry analytics |
| **Extension: Installations** | ≥ 500 total | Installation logs |
| **Extension: Published Extensions** | ≥ 20 (including 3+ team orchestrators) | Registry API count |
| **Team Runtime: Teams Executed** | ≥ 100 unique team executions | Execution logs |
| **Test Coverage** | ≥ 75% | Code coverage report |
| **Documentation Pages** | ≥ 30 pages | docs site count |
| **GitHub Stars** | ≥ 5,000 | GitHub API |
| **Active Contributors** | ≥ 30 unique | GitHub commit log |
| **Response Time (p95)** | Registry <2s, Hook invocation <1ms | Load testing |

---

## 10. Out of Scope (Explicitly Deferred)

| Feature | Rationale | Target Phase |
|---------|-----------|-------------|
| **Shared Services Fabric** | Premature for local single-user tool. Agents already share MCP tools via single process. Relevant for cloud/multi-tenant runtime. | Phase 3 |
| **Enterprise Governance Layer** | RBAC, SSO, audit dashboards — requires enterprise customers and Phase 3 maturity | Phase 3 |
| **Cloud Runtime Service (SaaS)** | Managed multi-tenant cloud offering — requires operational infrastructure and cost modeling | Phase 3 |
| **A2A Protocol v1.0** | Agent-to-Agent protocol is still stabilizing (v1.0 expected early 2026) — our event bus + A2A tools are sufficient | Phase 3 |
| **Advanced Security (microVM isolation)** | gVisor/Firecracker — needs Fabric/cloud runtime first | Phase 3 |
| **Registry Monetization** | Verified Publisher fees, Private Registry SaaS — needs market validation first | Phase 3 |
| **Federated Registries** | Cross-registry search and mirroring — single registry must mature first | Phase 3+ |
| **Agent Commerce / Payments** | Visa/Mastercard integration — market not mature, protocol not finalized | Phase 3+ |
| **New hook points beyond the existing 22** | Existing hooks cover all known use cases for Phase 2; add only if proven necessary | Future |
| **Team orchestration in core runtime** | Violates extension philosophy; all coordination patterns are extension concerns | N/A |

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
│  │          Web UI (pekohub.org)                        │   │
│  └──────────────────────────────────────────────────────┘   │
└──────────────────────────┬──────────────────────────────────┘
                           │ HTTPS / OCI
┌──────────────────────────┴──────────────────────────────────┐
│                    CLI (peko)                                 │
│  push │ pull │ search │ auth login │ ext install │ ext publish│
├──────────────────────────┬──────────────────────────────────┤
│            Runtime Engine (v0.2.0)                            │
│  ┌──────────────────────────────────────────────────────┐   │
│  │         Extension Core (22 Hook Points)               │   │
│  │  • ToolRegister, ToolExecute, ToolResultTransform    │   │
│  │  • EventSubscribe, EventEmit                         │   │
│  │  • AgentInit, AgentShutdown, AgentIteration          │   │
│  │  • SessionStateChange, SessionContextBuild           │   │
│  └──────────────────────────────────────────────────────┘   │
│  ┌──────────────────────────────────────────────────────┐   │
│  │         A2A Event Bus + Built-in Tools                │   │
│  │  a2a_send, a2a_broadcast, a2a_receive                │   │
│  └──────────────────────────────────────────────────────┘   │
│  ┌──────────────────────────────────────────────────────┐   │
│  │         IPC Layer (ADR-021)                           │   │
│  │  UDP / Unix Socket — CLI↔Daemon streaming            │   │
│  └──────────────────────────────────────────────────────┘   │
└──────────────────────────┬──────────────────────────────────┘
                           │ Hooks + Tools + Bus
┌──────────────────────────┴──────────────────────────────────┐
│              Extensions (Installed from Registry)             │
│                                                               │
│  ┌─────────────────┐ ┌─────────────────┐ ┌───────────────┐  │
│  │ Supervisor Team │ │  Pipeline Team  │ │   Mesh Team   │  │
│  │   Extension     │ │   Extension     │ │  Extension    │  │
│  │                 │ │                 │ │               │  │
│  │ Uses: AgentInit │ │ Uses: AgentInit │ │ Uses: AgentInit│  │
│  │   EventSubscribe│ │   ToolRegister  │ │   EventSubscribe│  │
│  │   ToolRegister  │ │   SessionContext│ │   ToolRegister │  │
│  └─────────────────┘ └─────────────────┘ └───────────────┘  │
│                                                               │
│  Team orchestration is an extension concern. Core knows       │
│  nothing about supervisors, pipelines, or meshes.             │
└─────────────────────────────────────────────────────────────┘
```

### 11.2 Extension Hook Usage for Team Orchestration

```
┌─────────────────────────────────────────────────────────────┐
│              Supervisor Team Extension                        │
│                                                               │
│  Hook Registration:                                           │
│  ┌──────────────────────────────────────────────────────┐   │
│  │ AgentInit                                            │   │
│  │ └── Spawn worker agents, init team state             │   │
│  ├──────────────────────────────────────────────────────┤   │
│  │ EventSubscribe "team.task.*"                         │   │
│  │ └── Route incoming tasks to appropriate workers      │   │
│  ├──────────────────────────────────────────────────────┤   │
│  │ EventSubscribe "team.result.*"                       │   │
│  │ └── Collect worker results, check completion         │   │
│  ├──────────────────────────────────────────────────────┤   │
│  │ ToolRegister                                         │   │
│  │ └── Register: team_delegate, team_synthesize         │   │
│  ├──────────────────────────────────────────────────────┤   │
│  │ SessionStateChange                                   │   │
│  │ └── Sync shared context across team agents           │   │
│  └──────────────────────────────────────────────────────┘   │
│                                                               │
│  Tools provided by extension:                                 │
│  • team_delegate(agent, task) → uses a2a_send internally      │
│  • team_synthesize(results) → produces final answer           │
│  • team_broadcast(topic, message) → uses a2a_broadcast        │
└─────────────────────────────────────────────────────────────┘
```

---

## 12. Definition of Done

Phase 2 is **officially complete** when:

1. ✅ All P0 success criteria (REG-001 through REG-028, EXT-001 through EXT-015, TEAM-001 through TEAM-010, PERF-001 through PERF-008, SEC-001 through SEC-006, DX-001 through DX-005) are implemented, tested, and documented
2. ✅ All 11 quantitative KPIs meet or exceed their targets
3. ✅ Public Registry is live at `https://pekohub.org` with ≥ 50 community bundles
4. ✅ Extension source references work (GitHub, URL, MCP endpoint, registry)
5. ✅ At least 3 team orchestrator extensions are published to the registry (supervisor, pipeline, mesh)
6. ✅ A public demo showcases a 3-agent team using an installed orchestrator extension, shared MCP tools, and A2A messaging
7. ✅ At least 3 external teams (outside the core dev team) have successfully published extensions or multi-agent systems on the platform
8. ✅ Phase 2 retrospective document is published, capturing lessons learned and Phase 3 priorities

---

*End of Phase 2 Success Criteria Document v2.1*
