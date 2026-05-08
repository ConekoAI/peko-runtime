# Pekobot Packaging Specification v1.0

> **Version**: 1.0-draft  
> **Status**: In Progress — Phase 1  
> **Scope**: Agent packaging (`.agent`), Team packaging (`.team`), Image build system, Registry push/pull, Extension packaging  
> **Related**: `DATA_MODEL.md` §6, `Phase1_Success_Criteria_Revised.md` §3

---

## 1. Executive Summary

This document defines the unified packaging system for Pekobot v1.0. It covers four distinct but related packaging formats:

| Format | Extension | Purpose | Audience |
|--------|-----------|---------|----------|
| **Agent Package** | `.agent` | Portable, self-contained agent export/import | End users sharing agents |
| **Team Snapshot** | `.team` | Multi-agent team data snapshot (backup/share) | End users sharing team state |
| **Team Definition** | `team.toml` | Team runtime definition (agents, bus, shared services) | Team operators deploying teams |
| **Image Manifest** | `manifest.json` | Content-addressable build artifact | Build system, registry |
| **Extension Package** | `.ext` | Extension bundle export/import | Extension developers, users |

The goal is to unify the existing dual systems (`src/portable/` for `.agent`/`.team` and `src/image/` for content-addressable images) into a single coherent packaging layer that supports validation and registry interoperability. **Security (signing/encryption) and base image inheritance are explicitly deferred** — checksum validation is sufficient for Phase 1.

---

## 2. Terminology

| Term | Definition |
|------|-----------|
| **Package** | A `.agent`, `.team`, or `.ext` file — a gzip-compressed tar archive for portability |
| **Image** | A content-addressable build artifact described by an `ImageManifest` with SHA-256 digested layers |
| **Layer** | A gzip-compressed tar archive containing a specific category of files (config, markdown, tools, etc.) |
| **Manifest** | Metadata describing a package or image (TOML for packages, JSON for images) |
| **Registry** | A remote or local store for images with push/pull capability |
| **DID** | Decentralized Identifier — self-sovereign identity for agents |
| **Extension Bundle** | A packaged extension with all files included for offline distribution |
| **Team Snapshot** | A `.team` file — a gzip-compressed tar archive containing full agent data for all agents in a team |
| **Team Definition** | A `team.toml` file describing how to deploy a team (agent images, bus config, shared services) |

---

## 3. Baseline: What Already Exists

### 3.1 Agent Packaging (`src/portable/`)

| Component | Status | Notes |
|-----------|--------|-------|
| `AgentManifest` (TOML) | ✅ Implemented | Full schema with agent metadata, identity, capabilities, tools, MCP servers, packaging metadata |
| `Packager` (export) | ✅ Implemented | Creates `.agent` tar.gz with all agent data |
| `Unpackager` (import) | ✅ Implemented | Extracts `.agent`, validates, imports identity/config/skills/workspace/sessions/MCP |
| `crypto.rs` | ✅ Implemented | AES-256-GCM + Argon2id encryption/decryption (not wired) |
| `validation.rs` | ✅ Implemented | Checksum verification, format version checking, required file validation |
| Signing | ⚠️ Partial | `sign_manifest()` creates ed25519 signatures but `signature.json` is not written; verification not implemented |
| `get_package_info()` | ✅ Implemented | Lightweight inspection without full extraction |

### 3.2 Team Snapshot Packaging (`src/portable/`) — Data Portability

| Component | Status | Notes |
|-----------|--------|-------|
| `TeamManifest` (TOML) | ✅ Implemented | Basic team info, format version, export metadata |
| `TeamPackager` | ✅ Implemented | Exports multiple agents into `.team` tar.gz (data snapshot) |
| `TeamUnpackager` | ✅ Implemented | Imports `.team`, delegates to `Unpackager` per agent |
| Team-level checksums | ❌ Missing | No checksum validation at team level (Phase 1 target) |

### 3.2b Team Runtime Definition (`src/team/config.rs`) — Orchestration

| Component | Status | Notes |
|-----------|--------|-------|
| `TeamConfig` (TOML) | ✅ Implemented | Team identity, agent definitions with image refs, shared services (bus, memory, files, MCPs) |
| `AgentDefinition` | ✅ Implemented | Name, image ref, instances, role, env overrides |
| `BusConfig` | ✅ Implemented | Backend (in-memory/redis/nats), URL |
| `SharedServices` | ✅ Implemented | Memory, files, MCPs |
| `TeamManager::deploy()` | ✅ Implemented | Deploys team from `TeamConfig` with event bus and shared services |
| Team definition packaging | ❌ Missing | No `team.toml` export/import in `.team` package (**Phase 1 target**) |

### 3.3 Image Build System (`src/image/`)

| Component | Status | Notes |
|-----------|--------|-------|
| `ImageManifest` (JSON) | ✅ Implemented | Schema v1 with layers, base info, capabilities |
| `ImageBuilder` | ✅ Implemented | Builds images from directories, creates content-addressable layers |
| `ImageRegistry` (local) | ✅ Implemented | Content-addressable storage, tag resolution, layer dedup, GC |
| `LayerType` enum | ✅ Implemented | Config, Markdown, Tools, Projects, Memories, Skills, McpConfig |
| `ImageRef` parsing | ✅ Implemented | Registry ref, local tag, digest, path |
| Base image inheritance | ⚠️ Declared | `base` field exists but `ImageBuilder::build()` does not resolve/merge parent layers (**deferred**) |
| `BuildOptions` | ✅ Implemented | Tag, base image ref (ignored), registry path |
| Build progress callbacks | ✅ Implemented | `BuildProgress` enum with all stages |

### 3.4 Registry Client (`src/registry/`)

| Component | Status | Notes |
|-----------|--------|-------|
| `RegistryClient` | ✅ Implemented | HTTP client with pull/push, progress events |
| `RegistryConfig` | ✅ Implemented | Sources, auth (bearer/basic/none), priority |
| `RegistryRef` parsing | ✅ Implemented | `host/path:tag` format |
| Pull/Push | ✅ Implemented | Full flow with digest verification |
| Auth resolution | ✅ Implemented | Env-var based bearer/basic auth |
| Media types | ✅ Implemented | Custom Pekobot media types |
| Layer existence check | ⚠️ Stub | Returns empty set always — pushes all layers (Phase 1 target) |

### 3.5 Extension System (`src/extension/`, `src/extensions/`)

| Component | Status | Notes |
|-----------|--------|-------|
| `ExtensionManifest` | ✅ Implemented | ID, type, name, version, path, metadata |
| `ExtensionManager` | ✅ Implemented | Discovery, install, uninstall, enable/disable, bundle |
| `ExtensionBundle` | ✅ Implemented | In-memory bundle of multiple extensions |
| `install_bundle()` | ✅ Implemented | Installs bundled extensions with conflict checking |
| Extension discovery | ✅ Implemented | Two-tier hierarchy (SKILL.md/server.json → manifest.yaml) |
| Extension packaging | ❌ Missing | No `.ext` package format; no export of extensions to file (Phase 1 target) |
| Extension source refs | ❌ Missing | No URL/git/registry references for extensions (**deferred**) |
| Extension registry client | ❌ Missing | No push/pull for extensions (**deferred**) |

---

## 4. Design Decisions

### 4.1 Security: Deferred for Phase 1

**Decision**: Signing and encryption are **explicitly out of scope** for Phase 1 packaging.

**Rationale**:
- Packaging is primarily for sharing; trust is established out-of-band (git, private channels)
- Checksum validation (`SHA-256`) is sufficient to detect accidental corruption
- Adding signing/encryption now would delay more critical features without proportional value
- Can be added later without breaking format changes (signature fields are already in manifest schema)

**What we DO implement**:
- ✅ SHA-256 checksums for all files in package manifests
- ✅ Checksum verification on import
- ✅ Format version validation
- ✅ Required file validation

**What we DEFER**:
- ❌ `signature.json` creation and verification
- ❌ Package encryption (crypto.rs stays but remains unwired)
- ❌ DID-based signature verification

### 4.2 Registry: Mock Server for Development

**Decision**: Build a lightweight **mock registry server** as a test fixture and local development tool.

**Rationale**:
- Registry client needs something to talk to for integration tests
- A mock server validates our protocol design before committing to a real registry backend
- Can be reused for extension registry prototyping
- Running a real registry (Docker Distribution, GHCR) in tests is heavy and flaky

**Mock server scope**:
- In-memory or file-backed storage
- Implements the Pekobot registry protocol (OCI-inspired)
- Supports manifest push/pull, layer push/pull, tag resolution
- Optional: basic auth for testing auth flows
- Lives in `src/registry/mock_server.rs` or `e2e_tests/registry_mock/`

### 4.3 Team Packaging: Two Concepts

**Decision**: Distinguish between **Team Snapshot** (`.team`) and **Team Definition** (`team.toml`).

| Concept | Format | Contains | Use Case |
|---------|--------|----------|----------|
| **Team Snapshot** | `.team` tar.gz | Full agent data (configs, identities, workspaces, sessions, memories) | Backup, migration, sharing learned state |
| **Team Definition** | `team.toml` | Agent image refs, bus config, shared services, topology | Deploy, reproduce, scale |

**Rationale**:
- A running team has **two separable concerns**: (a) how it's wired together, and (b) what data each agent has accumulated
- `team.toml` is the "docker-compose.yml" — it defines the team structure
- `.team` is the "backup tarball" — it captures accumulated state
- Conflating them creates confusion: importing a snapshot doesn't recreate runtime wiring, and deploying from definition doesn't restore memories

**For Phase 1**:
- `.team` snapshot gets checksums (data integrity)
- `team.toml` is included in `.team` snapshot for reference (not used on import)
- Full team definition packaging (registry push/pull of `team.toml`) is deferred

### 4.4 Extension Packaging: Bundle Mode Only for Phase 1

**Decision**: Support **one extension distribution mode** for Phase 1:

| Mode | Format | Use Case | Example |
|------|--------|----------|---------|
| **Bundle** | `.ext` tar.gz | Offline distribution, air-gapped environments | `docker-skill.ext` |

**Source references** (GitHub, URL, MCP) are deferred to Phase 2.

**Rationale**:
- Local path installation already works (`pekobot ext install ./path`)
- Bundling covers the "share extensions" use case
- Source refs add significant complexity (URL resolution, git cloning, MCP validation)
- Can be added later without breaking the install flow

**`.ext` package format**:

```
docker-skill.ext (gzip-compressed tar)
├── manifest.toml           # Extension package metadata
└── extension/
    ├── manifest.yaml       # Extension type manifest
    ├── SKILL.md            # Type-specific entry point
    └── ...                 # Additional files
```

---

## 5. Gap Analysis

### 5.1 P0 Gaps (Must Close for v1.0)

| ID | Gap | Impact | Owner Module |
|----|-----|--------|-------------|
| GAP-001 | No `pekobot build` CLI command | Cannot build images from CLI | `src/commands/` |
| GAP-002 | No `pekobot pull` CLI command | Registry client not exposed | `src/commands/` |
| GAP-003 | No `pekobot push` CLI command | Registry client not exposed | `src/commands/` |
| GAP-004 | No mock registry server for testing | Cannot test registry client | `src/registry/` |
| GAP-005 | Registry client layer existence check is stubbed | Pushes all layers every time | `src/registry/` |
| GAP-006 | Team packages have no checksums | Cannot verify team package integrity | `src/portable/` |
| GAP-007 | No extension `.ext` package format | Cannot distribute extensions offline | `src/extension/` |
| GAP-008 | No `pekobot ext export` command | Cannot package extensions | `src/commands/` |

### 5.2 P1 Gaps (Deferred to Phase 2)

| ID | Gap | Impact | Target |
|----|-----|--------|--------|
| GAP-009 | `pekobot run <image-ref>` | No image runtime exists | Phase 2 (if needed) |
| GAP-010 | Base image inheritance | No consumer of merged images | Phase 2 |
| GAP-011 | `pekobot validate <path>` command | Partially covered by inspect | Phase 2 |
| GAP-012 | Extension source references (GitHub, URL, MCP) | Complex, not critical | Phase 2 |
| GAP-013 | Extension registry client | No registry protocol for extensions | Phase 2 |
| GAP-014 | Signing and encryption | Security for shared packages | Phase 2 |
| GAP-015 | Multi-arch manifest support | Platform-specific binaries | Phase 2 |
| GAP-016 | Content deduplication across images | Storage inefficiency | Phase 2 |
| GAP-017 | `pekobot diff <image-a> <image-b>` | Hard to compare images | Phase 2 |

---

## 6. Unified Architecture

### 6.1 Module Layout (Phase 1 — Minimal Churn)

Keep existing module structure. Add new files only.

```
src/
├── portable/               # EXISTING — agent/team packaging
│   ├── mod.rs
│   ├── manifest.rs
│   ├── packager.rs         # KEEP: unchanged
│   ├── unpackager.rs       # KEEP: unchanged
│   ├── team_packager.rs    # MODIFIED: add checksums
│   ├── team_unpackager.rs  # MODIFIED: add validation
│   ├── validation.rs       # KEEP: unchanged
│   └── crypto.rs           # KEEP: unwired
│
├── image/                  # EXISTING — image build system
│   ├── mod.rs
│   ├── manifest.rs
│   ├── builder.rs          # KEEP: base field remains ignored
│   ├── config.rs
│   └── registry.rs         # EXISTING: local registry
│
├── registry/               # EXISTING — remote registry client
│   ├── mod.rs
│   ├── client.rs           # MODIFIED: implement layer existence check
│   ├── config.rs
│   └── mock_server.rs      # NEW: mock registry for testing
│
├── extension/
│   ├── manager/
│   │   ├── mod.rs          # EXISTING
│   │   └── packaging.rs    # NEW: ExtensionPackager
│   └── types/
│       ├── mod.rs
│       └── source.rs       # NEW: ExtensionSourceRef (minimal, for future use)
│
└── commands/
    ├── mod.rs              # MODIFIED: add Build, Pull, Push
    ├── build.rs            # NEW: pekobot build
    ├── pull.rs             # NEW: pekobot pull
    ├── push.rs             # NEW: pekobot push
    └── ext.rs              # MODIFIED: add Export subcommand
```

### 6.2 Package Format Specifications

#### 6.2.1 Agent Package (`.agent`) — Unchanged

The `.agent` format is unchanged for Phase 1. See `src/portable/` for existing implementation.

#### 6.2.2 Team Snapshot (`.team`) — Data Snapshot with Checksums

> **Semantics**: A `.team` file is a **data snapshot**, not a runtime definition. It captures the on-disk state of a team and its agents at export time. It does NOT include runtime configuration (event bus, shared services, agent wiring).
>
> **To recreate a running team** from a snapshot: import the `.team` file, then deploy using the team's `team.toml` definition (if available) or configure manually.

```
my-team.team (gzip-compressed tar)
├── team/
│   ├── manifest.toml       # Team metadata + file checksums
│   └── team.toml           # Team runtime definition (for reference, optional)
├── agents/
│   ├── {agent-name}/
│   │   ├── manifest.toml
│   │   ├── identity/
│   │   ├── config/
│   │   ├── skills/
│   │   ├── workspace/
│   │   └── sessions/
│   └── {agent-name}/
│       └── ...
└── shared/
    └── skills/             # Team-shared skills (optional)
```

**`team/manifest.toml` schema (Phase 1):**

```toml
[team]
name = "dev-team"
description = "Development team with specialized agents"
version = "1.0.0"
agent_count = 3

[format]
version = "1.0"
pekobot_version = "0.1.0"

[export]
created_at = "2026-05-07T10:00:00Z"
include_sessions = true
include_workspace = true
include_mcp = true

[packaging]
files = ["team/manifest.toml", "agents/alice/manifest.toml", ...]
checksums = { "team/manifest.toml" = "sha256:...", ... }
```

#### 6.2.3 Extension Package (`.ext`)

```
docker-skill.ext (gzip-compressed tar)
├── manifest.toml           # Extension package metadata
└── extension/
    ├── manifest.yaml       # Extension type manifest
    ├── SKILL.md            # Type-specific entry point
    └── ...                 # Additional files
```

**`manifest.toml` schema:**

```toml
[extension]
id = "docker-skill"
name = "Docker Skill"
description = "Manage Docker containers"
version = "1.0.0"
extension_type = "skill"
created_at = "2026-05-07T10:00:00Z"
pekobot_version = "0.1.0"

[packaging]
files = ["manifest.toml", "extension/manifest.yaml", "extension/SKILL.md"]
checksums = { ... }
compression = "gzip"
archive_format = "tar"
```

---

## 6.3 Team Package Semantics

### What a `.team` Snapshot IS

A `.team` file is a **portable data snapshot** — like a tarball or zip file, but structured. It captures:

- **Agent configurations** — each agent's `config.toml`, prompts, capabilities
- **Agent identities** — DID documents, key pairs (unencrypted in Phase 1)
- **Agent workspaces** — files the agent has been working with
- **Agent sessions** — conversation history (optional, can be large)
- **Agent skills** — SKILL.md files and related resources
- **Team-shared resources** — skills, files shared across agents

### What a `.team` Snapshot IS NOT

A `.team` file does **not** capture:

- **Runtime state** — running agent instances, open connections, in-memory data
- **Event bus configuration** — Redis/NATS URLs, backend type, connection params
- **Shared service connections** — vector DB URLs, MCP server processes
- **Agent-to-agent wiring** — which agent sends tasks to which, routing rules
- **Team deployment topology** — instance counts, roles, environment overrides

### Comparison with Other Tools

| Tool | Package Contains | Runtime State |
|------|-----------------|---------------|
| `.team` snapshot | Agent data, configs, files | ❌ Not included |
| `.agent` snapshot | Single agent data, configs | ❌ Not included |
| Docker Compose YAML | Service definitions, image refs | ❌ Not included (`up` creates it) |
| Kubernetes YAML | Pod/Deployment definitions | ❌ Not included (`apply` creates it) |
| VM snapshot | Full disk + memory | ✅ Included (heavy) |

The `.team` format is intentionally closer to **Git repository snapshot** than to docker-compose. It captures files, not running processes.

### Team Definition (`team.toml`) vs Team Snapshot (`.team`)

| Aspect | `team.toml` | `.team` snapshot |
|--------|-------------|------------------|
| **Purpose** | Define how to build/deploy a team | Save/share accumulated team state |
| **Contains** | Image refs, bus config, shared services | Full agent files, identities, memories |
| **Size** | Small (~KB) | Large (~MB-GB depending on sessions) |
| **Versioned** | Yes (in git, registry) | Yes (as artifact) |
| **Editable** | Yes (text file) | No (opaque archive) |
| **Use case** | `pekobot team deploy` | `pekobot team export/import` |

### Recommended Workflow

```
# 1. Define a team (team.toml)
cat > research-team.toml << 'EOF'
[team]
name = "research-team"
description = "Multi-agent research team"

[[agents]]
name = "coordinator"
image = "pekohub.com/agents/coordinator:v1.0"
instances = 1
role = "coordinator"

[[agents]]
name = "researcher"
image = "pekohub.com/agents/researcher:v2.5"
instances = 3
role = "worker"

[shared.bus]
backend = "in_memory"
EOF

# 2. Deploy the team
pekobot team deploy research-team.toml

# 3. Run for a while — agents learn, gain memories, create files
pekobot send research-team/coordinator "Research quantum computing"

# 4. Save a snapshot (includes all learned state)
pekobot team export research-team -o research-team-snapshot.team

# 5. Share the snapshot
cp research-team-snapshot.team /shared/backups/

# 6. On another machine: restore the team data
pekobot team import research-team-snapshot.team

# 7. Re-deploy with the same definition (or a new one)
pekobot team deploy research-team.toml
```

---

## 7. Implementation Plan

### 7.1 Phase 1A: Team Packaging Hardening (P0)

| Task | Description | File(s) |
|------|-------------|---------|
| PKG-A1 | Add file checksums to `TeamManifest` and `TeamPackager` | `src/portable/team_packager.rs` |
| PKG-A2 | Implement team-level checksum validation in `TeamUnpackager` | `src/portable/team_unpackager.rs` |

### 7.2 Phase 1B: Image Build CLI (P0)

| Task | Description | File(s) |
|------|-------------|---------|
| PKG-B1 | Add `pekobot build <path> -t <name:tag>` CLI command | `src/commands/mod.rs`, `src/commands/build.rs` |

**Note**: Base image inheritance (`--base`) is accepted by `BuildOptions` but not resolved. This is documented behavior for Phase 1.

### 7.3 Phase 1C: Registry Integration (P0)

| Task | Description | File(s) |
|------|-------------|---------|
| PKG-C1 | Add `pekobot pull <registry-ref>` CLI command | `src/commands/pull.rs` |
| PKG-C2 | Add `pekobot push <local-tag> <registry-ref>` CLI command | `src/commands/push.rs` |
| PKG-C3 | Implement proper layer existence checking (HEAD request) | `src/registry/client.rs` |
| PKG-C4 | Build mock registry server for testing | `src/registry/mock_server.rs` |
| PKG-C5 | Integration tests using mock server | `tests/registry_integration.rs` |

### 7.4 Phase 1D: Extension Packaging (P0)

| Task | Description | File(s) |
|------|-------------|---------|
| PKG-D1 | Implement `.ext` packager (tar.gz from installed extension) | `src/extension/manager/packaging.rs` |
| PKG-D2 | Add `pekobot ext export <id> -o <file.ext>` command | `src/commands/ext.rs` |

### 7.5 Phase 1E: Testing & Documentation (P0)

| Task | Description |
|------|-------------|
| PKG-E1 | Integration test: build → push → pull with mock registry |
| PKG-E2 | Integration test: team export → import with checksum validation |
| PKG-E5 | Verify `team.toml` is included in `.team` snapshot (for reference) |
| PKG-E3 | Integration test: extension export → install roundtrip |
| PKG-E4 | Update `DATA_MODEL.md` if any format changes |

---

## 8. CLI Command Reference

### 8.1 New Commands

```
pekobot build <path> -t <name:tag> [--json]
    Build an agent image from a directory.

pekobot pull <registry-ref>
    Pull an image from a registry.
    Example: pekobot pull pekohub.com/agents/researcher:v2.5

pekobot push <local-tag> <registry-ref>
    Push a local image to a registry.
    Example: pekobot push researcher:v2.5 pekohub.com/agents/researcher:v2.5

pekobot ext export <id> -o <file.ext>
    Export an installed extension to a .ext package.
```

### 8.2 Existing Commands (Already Implemented)

```
pekobot agent export <name> -o <file.agent>
pekobot agent import <file.agent>
pekobot agent inspect <file.agent>
pekobot team export <name> -o <file.team>
pekobot team import <file.team>
pekobot ext install <path>        # Local path only
```

### 8.3 Explicitly Deferred Commands

The following commands are **not** part of Phase 1:

```
pekobot run <image-ref>           # No image runtime; use agent create + send
pekobot validate <path>           # Partially covered by inspect
pekobot ext install <source-ref>  # Source refs deferred to Phase 2
```

---

## 9. Mock Registry Server

### 9.1 Purpose

- Test the registry client without external dependencies
- Validate the Pekobot registry protocol design
- Enable CI/CD integration tests
- Serve as a reference implementation for future registry backends

### 9.2 Implementation

```rust
// src/registry/mock_server.rs

pub struct MockRegistryServer {
    listener: TcpListener,
    storage: Arc<MockStorage>,
    auth: Option<MockAuth>,
}

impl MockRegistryServer {
    pub async fn new(port: u16) -> Self;
    pub async fn start(self) -> ServerHandle;
    pub fn base_url(&self) -> String;
}

// Usage in tests:
#[tokio::test]
async fn test_push_pull_roundtrip() {
    let server = MockRegistryServer::new(0).await;
    let handle = server.start().await;
    
    let client = RegistryClient::new(
        RegistryConfig::with_source(&server.base_url()),
        temp_dir.path()
    );
    
    // Build an image
    let manifest = builder.build(...).await.unwrap();
    
    // Push
    client.push(&manifest.digest, &format!("{}/test:v1", server.base_url()), |_| {}).await.unwrap();
    
    // Pull
    let pulled = client.pull(&format!("{}/test:v1", server.base_url()), |_| {}).await.unwrap();
    
    assert_eq!(manifest.digest, pulled.digest);
    handle.shutdown().await;
}
```

### 9.3 Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/v2/` | GET | Registry capability check |
| `/v2/{name}/manifests/{reference}` | GET/PUT | Manifest pull/push |
| `/v2/{name}/blobs/{digest}` | GET | Layer pull |
| `/v2/{name}/blobs/uploads/` | POST | Initiate layer upload |
| `/v2/{name}/blobs/uploads/{uuid}` | PUT | Complete layer upload |
| `/v2/{name}/blobs/{digest}` | HEAD | Layer existence check |

---

## 10. UX Use Cases

### UC-1: Quickly Spin Up a Fresh Team from Registry

**User goal**: Start a new team with pre-configured agents from a registry.

**Current gap**: No team definition packaging in registry.

**Phase 1 workaround**:
```bash
# User has a team.toml file (from git, docs, or created manually)
pekobot team deploy ./research-team.toml
# This pulls agent images from registry and starts the team
```

**Future (Phase 2)**:
```bash
pekobot team pull pekohub.com/teams/research-team:v1.0
pekobot team deploy research-team
```

### UC-2: Save and Share a Team Snapshot with Learned State

**User goal**: A team has been running, agents have gained skills/memories. Save and share this state.

**Phase 1 solution**:
```bash
# Export full team state (configs, identities, workspaces, sessions, skills)
pekobot team export research-team -o research-team-v1.team

# Share the file
cp research-team-v1.team ~/shared/

# On recipient's machine: import restores all agent data
pekobot team import ~/shared/research-team-v1.team

# Recipient deploys using their own team.toml (or the included one)
pekobot team deploy research-team/team.toml
```

**Key insight**: The snapshot preserves **what agents know**, not **how they're wired**. The recipient decides the runtime topology.

### UC-3: Pull a Team Snapshot and Get It to Work

**User goal**: Download a `.team` file and have a functioning team.

**Phase 1 solution**:
```bash
# Import the snapshot (restores agent data to filesystem)
pekobot team import research-team.team

# Check if team.toml exists in imported team
ls ~/.pekobot/teams/research-team/team.toml

# If yes: deploy directly
pekobot team deploy ~/.pekobot/teams/research-team/team.toml

# If no: create a minimal team.toml or run agents individually
pekobot agent start research-team/coordinator
pekobot agent start research-team/researcher
```

**Limitation**: The `.team` snapshot may not include a `team.toml`. If the original team was created ad-hoc (not from a definition), the recipient must manually configure the runtime.

**Mitigation**: Phase 1 includes `team.toml` in `.team` exports for reference. If present on import, it's restored to the team directory.

### UC-4: Team Backup and Disaster Recovery

**User goal**: Regular backups of team state.

**Phase 1 solution**:
```bash
# Cron job or manual backup
pekobot team export research-team -o backups/research-team-$(date +%Y%m%d).team

# Restore from backup
pekobot team import backups/research-team-20260508.team --force
```

### UC-5: Fork a Team (Duplicate with Modifications)

**User goal**: Create a copy of a team with some changes.

**Phase 1 solution**:
```bash
# Export original
pekobot team export research-team -o original.team

# Import as new name
pekobot team import original.team --name research-team-v2

# Modify agents in the new team
pekobot agent config research-team-v2/coordinator --set model=gpt-5

# Export the fork
pekobot team export research-team-v2 -o research-team-v2.team
```

---

## 11. Success Criteria

Phase 1 packaging is complete when:

1. ✅ All P0 gaps (GAP-001 through GAP-008) are closed
2. ✅ `cargo test` passes with ≥ 70% coverage for packaging modules
3. ✅ `cargo clippy` passes with zero warnings in packaging code
4. ✅ All new CLI commands (`build`, `pull`, `push`, `ext export`) are functional
5. ✅ Agent packages pass checksum validation on import (existing behavior)
6. ✅ Team snapshot packages pass checksum validation on import
7. ✅ `team.toml` is included in `.team` exports for reference
8. ✅ Registry push/pull works against the mock server
9. ✅ `.ext` packages can be created and installed
10. ✅ Registry client skips existing layers on push (HEAD check works)

---

## Appendix A: Deferred to Phase 2

The following features are explicitly deferred and listed here to show the roadmap:

| Feature | Rationale | Approximate Effort |
|---------|-----------|-------------------|
| `pekobot run <image-ref>` | No image runtime exists; agents run via `send`/`daemon` | Large (new subsystem) |
| Base image inheritance (`--base`) | No clear consumer; template use case unclear | Medium |
| `pekobot validate <path>` | Partially covered by `inspect`; standalone validation is nice-to-have | Small |
| Extension source references (GitHub, URL, MCP) | Complex URL resolution, git cloning, MCP validation | Medium |
| Extension registry push/pull | No extension registry protocol defined | Medium |
| Team definition registry push/pull | No registry protocol for `team.toml` | Medium |
| Signing and encryption | Security for shared packages | Medium |
| Multi-arch manifest support | Platform-specific binaries | Small |
| Content deduplication across images | Storage optimization | Small |
| `pekobot diff <image-a> <image-b>` | Debugging tool | Small |

---

## Appendix B: File Checklist

### Files to Modify

| File | Changes |
|------|---------|
| `src/commands/mod.rs` | Add `Build`, `Pull`, `Push` to `Commands` enum |
| `src/commands/ext.rs` | Add `Export` subcommand |
| `src/portable/team_packager.rs` | Add file checksums to team manifest; include `team.toml` if exists |
| `src/portable/team_unpackager.rs` | Add checksum validation; restore `team.toml` if present in package |
| `src/registry/client.rs` | Implement `check_existing_layers()` with HEAD request |

### Files to Create

| File | Purpose |
|------|---------|
| `src/commands/build.rs` | `pekobot build` command handler |
| `src/commands/pull.rs` | `pekobot pull` command handler |
| `src/commands/push.rs` | `pekobot push` command handler |
| `src/registry/mock_server.rs` | Mock registry server for testing |
| `src/extension/manager/packaging.rs` | Extension packager (`.ext` export) |
| `src/extension/types/source.rs` | `ExtensionSourceRef` types (minimal, for future) |

---

*End of Packaging Specification v1.0*
