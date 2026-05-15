# Phase 2 Roadmap: Public Registry Beta + Extension Ecosystem

> **Version**: 2.1  
> **Phase**: 4–5 Months (following Phase 1 completion 2026-05-14)  
> **Objective**: Enable agent discovery and sharing at scale through a public registry, while maturing the extension ecosystem so that advanced behaviors (including multi-agent orchestration) are implemented as extensions, not core runtime features.

See `Phase2_Success_Criteria.md` for the complete, detailed success criteria. This document is the **implementation roadmap** — it identifies the workstreams, their dependencies, and the suggested execution order.

---

## 1. Overview

Phase 2 has two workstreams that build on the Phase 1 runtime:

| Workstream | What It Does | Depends On |
|------------|-------------|------------|
| **A. Public Registry Beta** | Hosted registry + web UI for publishing and discovering agents, teams, and extensions | Phase 1 packaging (`src/portable/`), registry client (`src/registry/`) |
| **B. Extension Ecosystem** | Extension source references, team orchestration as an extension type, and A2A tooling that integrates with the extension framework's existing hooks | Phase 1 extension framework (22 hooks, 6 types), A2A event bus, IPC layer (ADR-021) |

The success criteria for both workstreams are defined in the companion document. This roadmap focuses on **execution order** and **integration points**.

> **Design Principle**: Phase 2 does NOT add team orchestration, memory management, or coordination patterns to the core runtime. These are **extension concerns**. The core runtime provides the 22 hook points, the A2A event bus, and built-in tools — extensions compose these primitives into higher-level behaviors like team coordination. See §8 for the full rationale.

> **Note on Shared Services Fabric**: The Shared Services Fabric (shared browser pool, vector DB, memory tiers) was originally scoped for Phase 2 but has been **deferred to Phase 3 / Cloud Runtime**. Agents and teams continue to use MCP servers and built-in tools for browser, vector, and memory operations — exactly as they do in Phase 1. See `Phase2_Roadmap.md` §8 (previous version) for the full rationale.

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

**User outcome:** `peko agent push` and `peko agent pull` work against the public registry.

**Success criteria:** REG-001 through REG-006

### Milestone 2: Auth, Search, and Web UI (Weeks 5–8)
**Goal:** Users can discover and browse packages without using the CLI.

- OAuth 2.0 login (GitHub, Google)
- API key generation for CLI auth
- Namespace ownership enforcement
- Full-text search with faceted filtering
- Public web UI at `https://pekohub.org`
- Bundle detail pages (README, version history, install command)

**User outcome:** A developer can find an agent or extension on the web, copy the install command, and run it.

**Success criteria:** REG-007 through REG-014, REG-025 through REG-028

### Milestone 3: Extension Ecosystem (Weeks 9–14)
**Goal:** Extensions can be installed from remote sources, and team orchestration is available as an installable extension — not core runtime.

- Extension source references: `github:`, `https:`, `mcp+https:` resolution (deferred from Phase 1)
- `peko ext install github:owner/repo` — install extension from GitHub
- `peko ext install https://example.com/extension.ext` — install from direct URL
- `peko ext update <id>` — check for and install updates
- `peko ext list --outdated` — show extensions with available updates
- Team orchestration as a `general` extension type (or new `team` extension type):
  - Uses existing `EventSubscribe`/`EventEmit` hooks for A2A messaging
  - Uses existing `AgentInit`/`AgentShutdown` hooks for agent lifecycle
  - Uses existing `ToolRegister` hook to provide coordination tools (`delegate`, `synthesize`, `broadcast`)
  - Uses existing `SessionStateChange` hook for shared context
- A2A built-in tools (`a2a_send`, `a2a_receive`) integrate with the extension framework's event hooks
- Example team orchestrator extensions published to registry:
  - `pekohub.org/extensions/supervisor-team` — supervisor pattern
  - `pekohub.org/extensions/pipeline-team` — pipeline pattern
  - `pekohub.org/extensions/mesh-team` — mesh pattern

**User outcome:** A user installs a team orchestrator extension, enables it in `agent.toml`, and the agent gains team coordination capabilities through the extension framework's hooks.

**Success criteria:** EXT-001 through EXT-015, TEAM-001 through TEAM-010

### Milestone 4: Registry + Extension Integration (Weeks 15–18)
**Goal:** Extensions are first-class citizens in the registry, and the platform demonstrates multi-agent orchestration via extensions.

- Registry supports `.ext` package publishing and discovery
- Extension detail pages show hook points used, example configurations, and compatibility
- `peko ext publish <id>` — publish extension to registry
- `peko ext install pekohub.org/extensions/supervisor-team` — install from registry
- Integration demo: 3-agent research team using the supervisor extension, shared MCP tools, and A2A messaging
- Performance benchmarks for extension hook invocation overhead

**User outcome:** A user finds a team orchestrator extension on `pekohub.org`, installs it, and configures their agent for multi-agent coordination — all without core runtime changes.

**Success criteria:** EXT-016 through EXT-020, REG-035

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
│  │          Web UI (pekohub.org)                        │   │
│  └──────────────────────────────────────────────────────┘   │
└──────────────────────────┬──────────────────────────────────┘
                           │ HTTPS / OCI
┌──────────────────────────┴──────────────────────────────────┐
│                    CLI (peko)                                 │
│  push │ pull │ search │ auth login │ agent install │ ext install│
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
| Registry server in separate repo (`pekobot/registry` or `pekohub`) | Independent deployable service with its own release cycle and ops concerns |

### 3.3 CLI Commands

```bash
# Authentication
peko auth login                    # OAuth browser flow
peko auth logout                   # Clear credentials
peko auth status                   # Show logged-in user

# Discovery
peko search "github assistant"     # Search registry
peko agent info user/agent:1.0     # Show bundle metadata
peko ext info user/extension:1.0   # Show extension metadata

# Publishing (aliases for push/pull)
peko agent publish my-agent        # Push to public registry
peko agent install user/agent:1.0  # Pull + import in one step
peko ext publish my-extension      # Push extension to registry
peko ext install user/extension:1.0 # Pull + install extension from registry
```

---

## 4. Workstream B: Extension Ecosystem

### 4.1 Design Philosophy

**The core runtime is minimal. Everything else is an extension.**

Phase 1's extension framework provides 22 hook points that cover the entire agent lifecycle. Phase 2 does NOT add new hook points or core modules for team orchestration. Instead, it:

1. **Enables remote extension installation** — so users can discover and install team orchestrators from the registry
2. **Provides A2A tools** — built-in tools that integrate with the existing `EventSubscribe`/`EventEmit` hooks
3. **Publishes example extensions** — reference implementations of team coordination patterns

### 4.2 How Team Orchestration Works as an Extension

```
┌─────────────────────────────────────────────────────────────┐
│                    Agent Runtime (Core)                       │
│  ┌──────────────────────────────────────────────────────┐   │
│  │         Extension Core (22 Hook Points)               │   │
│  │  • ToolRegister                                       │   │
│  │  • ToolExecute / ToolExecuteAsync                     │   │
│  │  • EventSubscribe / EventEmit                         │   │
│  │  • AgentInit / AgentShutdown / AgentIteration         │   │
│  │  • SessionStateChange / SessionContextBuild           │   │
│  └──────────────────────────────────────────────────────┘   │
│  ┌──────────────────────────────────────────────────────┐   │
│  │         A2A Event Bus (In-Memory / Redis / NATS)      │   │
│  └──────────────────────────────────────────────────────┘   │
│  ┌──────────────────────────────────────────────────────┐   │
│  │         Built-in Tools                                │   │
│  │  • a2a_send, a2a_receive, a2a_broadcast               │   │
│  │  • memory.store, memory.retrieve                      │   │
│  │  • session.branch, session.overlay                    │   │
│  └──────────────────────────────────────────────────────┘   │
└──────────────────────────┬──────────────────────────────────┘
                           │ Hooks + Tools + Bus
┌──────────────────────────┴──────────────────────────────────┐
│              Team Orchestrator Extension                      │
│  (installed via `peko ext install supervisor-team`)           │
│                                                               │
│  Hooks registered:                                            │
│  • AgentInit → spawn worker agent instances                   │
│  • EventSubscribe "team.task.*" → route tasks to workers      │
│  • EventSubscribe "team.result.*" → collect results           │
│  • ToolRegister → register `team_delegate`, `team_synthesize` │
│  • SessionStateChange → maintain shared team context          │
│                                                               │
│  The extension IS the team runtime. Core knows nothing.       │
└─────────────────────────────────────────────────────────────┘
```

### 4.3 Extension Types for Phase 2

| Extension Type | Use Case | How It Works |
|----------------|----------|--------------|
| **`general`** | Team orchestrator, custom coordination logic | Declares hooks in `manifest.yaml`, implements handlers |
| **`skill`** | Team behavior instructions | Injects coordination patterns into system prompt via `PromptSystemSection` |
| **`mcp`** | Shared tools for teams (browser, vector DB) | MCP server shared across all agents in a team |
| **`team`** *(new)* | Declarative team composition | New adapter that reads `team.toml`-like manifest, registers appropriate hooks |

### 4.4 Example: Supervisor Team as a `general` Extension

```yaml
# manifest.yaml
id: "supervisor-team"
name: "Supervisor Team Orchestrator"
version: "1.0.0"
extension_type: "general"
description: "Enables supervisor-pattern multi-agent teams"

hooks:
  - point: "agent.init"
    handler: "init_team"
  - point: "tool.register"
    handler: "register_team_tools"
  - point: "event.subscribe"
    topic_pattern: "team.task.*"
    handler: "on_task_received"
  - point: "event.subscribe"
    topic_pattern: "team.result.*"
    handler: "on_result_received"
  - point: "session.state_change"
    handler: "sync_team_context"
```

```toml
# agent.toml — user enables the extension
[extensions]
enabled = ["supervisor-team"]

[extensions.config.supervisor-team]
workers = ["researcher", "coder"]
max_iterations = 10
```

### 4.5 Example: Team as a Declarative Extension Type

```yaml
# manifest.yaml
id: "research-team"
name: "Research Team"
version: "1.0.0"
extension_type: "team"

team:
  pattern: "supervisor"
  agents:
    - bundle: "pekohub.org/agents/router:v1.0"
      role: "coordinator"
    - bundle: "pekohub.org/agents/researcher:v1.0"
      role: "worker"
      replicas: 2
```

A new `TeamAdapter` would:
1. Parse the `team` section from the manifest
2. Register `AgentInit` hook to spawn team agents
3. Register `EventSubscribe` hooks for A2A coordination
4. Register `ToolRegister` to provide `delegate`, `synthesize` tools

### 4.6 A2A Built-in Tools (Core)

These tools ship with the core runtime and integrate with the extension framework's event hooks:

| Tool | What It Does | Hook Used |
|------|-------------|-----------|
| `a2a_send` | Send direct message to another agent | `EventEmit` |
| `a2a_broadcast` | Broadcast message to all agents on a topic | `EventEmit` |
| `a2a_receive` | Receive messages from inbox | `EventSubscribe` |
| `a2a_subscribe` | Subscribe to a topic | `EventSubscribe` |

Extensions build coordination patterns on top of these primitives.

### 4.7 Key Design Decisions

| Decision | Rationale |
|----------|-----------|
| Team orchestration as extension, not core | Maximally flexible — users can install supervisor, pipeline, mesh, debate, market, or custom patterns without core changes |
| No new hook points for Phase 2 | Existing 22 hooks cover all team coordination needs (events, agent lifecycle, session, tools) |
| A2A tools are built-in | Low-level primitives that extensions compose; similar to how `shell` and `fs` are built-in |
| `team` as potential new extension type | If `general` extension proves too verbose for declarative teams, add a `TeamAdapter` with simplified manifest |
| Remote extension sources | Deferred from Phase 1; essential for discovering team orchestrators from registry |

---

## 5. Cross-Cutting Concerns

### 5.1 Security

| Concern | Approach | Success Criteria |
|---------|----------|-----------------|
| Registry HTTPS | TLS 1.3 only | SEC-001 |
| Bundle signing | ed25519 DID keys (Phase 1 deferred) | REG-020, REG-021 |
| Token expiry | 24-hour expiry + revocation | SEC-002 |
| Audit logs | Append-only with tamper-evident hashing | SEC-003 |
| Vulnerability scans | Scan on push, block critical | SEC-004 |
| Extension sandboxing | Extensions run in-process; MCP servers run as separate processes with env stripping | SEC-005 |

### 5.2 Performance Targets

| Metric | Target | Verification |
|--------|--------|-------------|
| Registry concurrent ops | 100 push/pull, p95 < 2s | Load test |
| Extension hook invocation | < 1ms per hook | Benchmark |
| A2A message delivery | < 10ms in-memory | Benchmark |
| Registry uptime | ≥ 99.5% monthly | Monitoring |
| 3-agent team E2E (via extension) | < 30s (excl. LLM latency) | Integration test |

### 5.3 Developer Experience

| Deliverable | Target | Owner |
|-------------|--------|-------|
| Public registry live | `pekohub.org` | Registry team |
| Community bundles | ≥ 50 published | Community |
| Team orchestrator extensions | ≥ 3 patterns (supervisor, pipeline, mesh) | Core team |
| Tutorial series | 3+ tutorials: "Installing extensions", "Building a team orchestrator", "Publishing to registry" | Docs |
| Extension development guide | How to use the 22 hook points, with examples | Docs |
| CLI getting-started wizard | Interactive `peko init` for registry login + extension discovery | CLI |

---

## 6. Success Metrics & KPIs

Phase 2 is successful when all P0 criteria are met AND:

| Metric | Target | Measurement |
|--------|--------|-------------|
| Published bundles | ≥ 50 community + 10 official | Registry API |
| Monthly active users | ≥ 500 | OAuth events |
| Total pulls | ≥ 10,000 cumulative | Analytics |
| Extension installations | ≥ 500 total | Installation logs |
| Team executions (via extensions) | ≥ 100 unique | Execution logs |
| Test coverage | ≥ 75% | Coverage report |
| Documentation pages | ≥ 30 | docs site |
| GitHub stars | ≥ 5,000 | GitHub API |
| Active contributors | ≥ 30 | Commit log |

---

## 7. Out of Scope (Deferred to Phase 3)

| Feature | Rationale |
|---------|-----------|
| **Shared Services Fabric** | Premature for local single-user tool. Agents already share MCP tools via single process. Relevant for cloud/multi-tenant runtime. |
| **Enterprise Governance (RBAC, SSO, audit dashboards)** | Needs enterprise customers |
| **Cloud Runtime SaaS** | Needs operational infrastructure + cost modeling |
| **A2A Protocol v1.0** | Spec still stabilizing; our event bus + A2A tools are sufficient |
| **microVM isolation (gVisor/Firecracker)** | Needs Fabric/cloud runtime first |
| **Registry monetization** | Needs market validation |
| **Federated registries** | Single registry must mature first |
| **Agent commerce / payments** | Market not mature |
| **New hook points beyond the existing 22** | Existing hooks cover all known use cases; add only if proven necessary |

---

## 8. Why Team Orchestration Is an Extension, Not Core

### 8.1 The Extension Framework Already Covers It

Phase 1's ADR-017 provides 22 hook points that span the entire agent lifecycle. Team orchestration needs:

| Team Concern | Phase 1 Hook Point | Status |
|-------------|-------------------|--------|
| Spawn team agents on startup | `AgentInit` | ✅ Exists |
| Coordinate between agents | `EventSubscribe` / `EventEmit` | ✅ Exists |
| Provide team tools (delegate, synthesize) | `ToolRegister` | ✅ Exists |
| Share context across agents | `SessionStateChange` / `SessionContextBuild` | ✅ Exists |
| Monitor agent iterations | `AgentIteration` | ✅ Exists |
| Clean up on shutdown | `AgentShutdown` | ✅ Exists |

**There is no gap.** The current `src/team/mod.rs` hardcodes behavior that the extension framework can already express.

### 8.2 Core Should Be Minimal

The design principle from ADR-017:

> *"The Extension Core is the single registry of all hook points... Type-specific adapters provide semantic validation and simpler manifests for common cases. The GeneralExtensionAdapter provides unconstrained access to all 22 hook points."*

Team orchestration is a **common case** that deserves a type-specific adapter (e.g., `TeamAdapter`). It is NOT a core runtime concern.

### 8.3 Flexibility Over Prescription

If team orchestration is core:
- Users get supervisor, pipeline, mesh — and nothing else
- Adding a "debate" or "market" pattern requires a core PR

If team orchestration is an extension:
- Anyone can publish a new coordination pattern
- Users install what they need
- The core team focuses on hook points and primitives

### 8.4 The Current `src/team/` Is Technical Debt

The existing `src/team/mod.rs`:
- Hardcodes `EventBus` outside the extension framework
- Hardcodes `SharedServicesFabric` with memory namespaces
- Has TODOs for instance creation, MCP lifecycle, scaling
- Duplicates what extensions already do

**The path forward:** Migrate `src/team/` into an extension type. The event bus stays as a core service (like session storage), but orchestration logic moves to extensions.

---

## 9. Definition of Done

Phase 2 is complete when:

1. All P0 success criteria (REG-001 through REG-028, EXT-001 through EXT-015, TEAM-001 through TEAM-010, PERF-001 through PERF-005, SEC-001 through SEC-005, DX-001 through DX-005) are implemented, tested, and documented
2. All quantitative KPIs meet or exceed targets
3. Public Registry is live at `https://pekohub.org` with ≥ 50 community bundles
4. Extension source references work (GitHub, URL, MCP endpoint)
5. At least 3 team orchestrator extensions are published to the registry (supervisor, pipeline, mesh)
6. A public demo showcases a 3-agent team using an installed orchestrator extension, shared MCP tools, and A2A messaging
7. At least 3 external teams (outside the core dev team) have successfully published extensions or multi-agent systems on the platform
8. Phase 2 retrospective is published

---

## 10. Document Reference

| Document | Purpose |
|----------|---------|
| `Phase2_Success_Criteria.md` | Complete, detailed success criteria with numbered requirements |
| `Phase2_Roadmap.md` (this document) | Implementation roadmap: execution order, milestones, architecture diagrams, design decisions |
| ADR-017 (`docs/architecture/adr/ADR-017.md`) | Unified Extension Architecture — hook points, adapters, design philosophy |
| ADR-021 (`docs/architecture/adr/ADR-021-daemon-as-central-runtime.md`) | IPC layer for CLI↔daemon communication |
| ADR-024 (`docs/architecture/adr/ADR-024-unified-extension-manifest.md`) | Extension manifest format (`manifest.yaml` + `extension_type`) |
| ADR-027 (`docs/architecture/adr/ADR-027-unified-packaging.md`) | Packaging format for `.agent`/`.team`/`.ext` |

---

*End of Phase 2 Roadmap v2.1*
