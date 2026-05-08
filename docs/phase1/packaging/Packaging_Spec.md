# Pekobot Packaging Specification v2.2 — Capabilities Removed

> **Version**: 2.2  
> **Status**: Complete — All Phases 1–7 Done  
> **Scope**: Unified agent packaging (`.agent`), Team packaging (`.team`), Registry push/pull, Extension packaging  
> **Related**: `DATA_MODEL.md` §6, `Phase1_Success_Criteria_Revised.md` §3  
> **Key Decisions**: 
> - `src/image/` merged into `src/portable/`. Manifest schema stripped of extension concerns — `agent.toml` is the single source of truth.
> - **`capabilities` concept entirely removed** — superseded by the extension framework. `extensions.enabled` is the single source of truth for tool/MCP/skill access.

---

## 1. Executive Summary

This document defines the unified packaging system for Pekobot v0.1.0. **A single `.agent` format serves all use cases**: user export/import, directory builds, and registry push/pull.

| Format | Extension | Purpose | Audience |
|--------|-----------|---------|----------|
| **Agent Package** | `.agent` | Universal agent archive: export, build, registry | All users |
| **Team Snapshot** | `.team` | Multi-agent team data snapshot (backup/share) | End users sharing team state |
| **Team Definition** | `team.toml` | Team runtime definition (agents, bus, shared services) | Team operators deploying teams |
| **Extension Package** | `.ext` | Extension bundle export/import | Extension developers, users |

**Key decision**: The former `src/image/` module (content-addressable layers, `ImageManifest`, `ImageBuilder`) is merged into `src/portable/`. The `.agent` format gains content-addressable layer digests. Registry push/pull operates on `.agent` packages, not a separate image format.

**Key decision — Clean Manifest**: `AgentManifest` contains **only packaging metadata** (what files are in the package, their integrity). It does NOT duplicate agent behavior configuration (tools, MCP, skills). That information lives in `agent.toml`, which is one of the files inside the package (the `config` layer). This eliminates competing sources of truth.

**Key decision — Capabilities Removed**: The pre-extension `capabilities` concept (`AgentCapability`, `TeamCapabilityConfig`, `CapabilitiesConfig`) has been **entirely removed** from the codebase. The extension framework (`extensions.enabled` whitelist, `ExtensionManager`) is the single source of truth for all tool/MCP/skill access control.

**What was removed**:
- `ImageManifest` (JSON) → merged into `AgentManifest` (TOML)
- `ImageBuilder` → merged into `Packager::build_from_directory()`
- `ImageRegistry` (local) → merged into `AgentRegistry` (local store for `.agent` layers)
- `pekobot build` as top-level command → moved under `pekobot agent build`
- `pekobot agent push` / `pekobot agent pull` as agent subcommands
- **Manifest fields removed**: `capabilities`, `tools`, `mcp`, `tool_sources`, `memory` — all belong in `agent.toml`
- **`AgentCapability` struct** → removed from `src/types/agent.rs`
- **`TeamCapabilityConfig`** → removed from `src/team/capability.rs` (file deleted)
- **`CapabilitiesConfig`** → removed from `src/portable/manifest.rs`
- **`CapabilityIndex`** → renamed to `ExtensionIndex` in `src/agent/context.rs`

---

## 2. Terminology

| Term | Definition |
|------|-----------|
| **Agent Package** | A `.agent` file — gzip-compressed tar archive containing agent manifest, identity, config, skills, and content-addressable layers |
| **Layer** | A category of agent files (config, skills, workspace, etc.) with a SHA-256 digest for content-addressable storage |
| **Layer Digest** | SHA-256 hash of a layer's content, used for deduplication and integrity |
| **Manifest** | TOML metadata describing an agent package (name, version, layers, checksums). Contains packaging metadata only — no behavior config. |
| **Registry** | A remote or local store for agent packages with push/pull capability |
| **Local Registry Store** | Content-addressable filesystem storage for layer deduplication (`~/.pekobot/registry/`) |
| **DID** | Decentralized Identifier — self-sovereign identity for agents |
| **Team Snapshot** | A `.team` file — gzip-compressed tar archive containing full agent data for all agents in a team |
| **Team Definition** | A `team.toml` file describing how to deploy a team (agent refs, bus config, shared services) |
| **Extension Bundle** | A packaged extension with all files included for offline distribution |
| **Single Source of Truth** | `agent.toml` inside the package is the canonical definition of agent behavior (tools, MCP, skills). The manifest only references it via the `config` layer digest. |

---

## 3. Baseline: What Already Exists

### 3.1 Agent Packaging (`src/portable/`) — The Survivor

| Component | Status | Notes |
|-----------|--------|-------|
| `AgentManifest` (TOML) | ✅ Implemented | **Will be cleaned** — strip `capabilities`, `tools`, `mcp`, `tool_sources`, `memory`. Keep `agent`, `identity`, `layers`, `packaging`, `signatures`. |
| `Packager` (export) | ✅ Implemented | Creates `.agent` tar.gz with all agent data |
| `Unpackager` (import) | ✅ Implemented | Extracts `.agent`, validates, imports identity/config/skills/workspace/sessions/MCP |
| `crypto.rs` | ✅ Implemented | AES-256-GCM + Argon2id encryption/decryption (not wired) |
| `validation.rs` | ✅ Implemented | Checksum verification, format version checking, required file validation |
| Signing | ⚠️ Partial | `sign_manifest()` creates ed25519 signatures but `signature.json` is not written; verification not implemented |
| `get_package_info()` | ✅ Implemented | Lightweight inspection without full extraction |
| **Content-addressable layers** | ❌ Missing | `.agent` uses flat file list; no layer digests (**merge target**) |
| **Local registry store** | ❌ Missing | No deduplicated layer storage (**merge target**) |

### 3.2 Image System (`src/image/`) — To Be Merged

| Component | Status | Fate |
|-----------|--------|------|
| `ImageManifest` (JSON) | ✅ Implemented | **Merge into `AgentManifest`** — add `layers` section to TOML |
| `ImageBuilder` | ✅ Implemented | **Merge into `Packager`** — `Packager::build_from_directory()` |
| `ImageRegistry` (local) | ✅ Implemented | **Merge into `AgentRegistry`** — local store for `.agent` layer dedup |
| `LayerType` enum | ✅ Implemented | **Keep** — layer categories map to `.agent` file categories |
| `ImageRef` parsing | ✅ Implemented | **Keep** — registry reference format |
| `RegistryClient` | ✅ Implemented | **Keep** — adapt to push/pull `.agent` layer digests |
| Base image inheritance | ⚠️ Declared | **Deferred** — `base` field stays ignored |

### 3.3 Team Packaging (`src/portable/`)

| Component | Status | Notes |
|-----------|--------|-------|
| `TeamManifest` (TOML) | ✅ Implemented | Basic team info, format version, export metadata |
| `TeamPackager` | ✅ Implemented | Exports multiple agents into `.team` tar.gz |
| `TeamUnpackager` | ✅ Implemented | Imports `.team`, delegates to `Unpackager` per agent |
| Team-level checksums | ❌ Missing | No checksum validation at team level (Phase 1 target) |
| `team.toml` in `.team` | ❌ Missing | Not included in export (Phase 1 target) |

### 3.4 Team Runtime Definition (`src/team/config.rs`)

| Component | Status | Notes |
|-----------|--------|-------|
| `TeamConfig` (TOML) | ✅ Implemented | Team identity, agent definitions with image refs, shared services |
| `AgentDefinition` | ✅ Implemented | Name, image ref, instances, role, env overrides |
| `TeamManager::deploy()` | ✅ Implemented | Deploys team from `TeamConfig` |

### 3.5 Extension System (`src/extension/`, `src/extensions/`)

| Component | Status | Notes |
|-----------|--------|-------|
| `ExtensionManifest` | ✅ Implemented | ID, type, name, version, path, metadata |
| `ExtensionManager` | ✅ Implemented | Discovery, install, uninstall, enable/disable, bundle |
| Extension packaging | ❌ Missing | No `.ext` package format; no export to file (Phase 1 target) |
| Extension source refs | ❌ Missing | No URL/git/registry references (**deferred**) |

### 3.6 Agent Configuration (`src/types/agent.rs`) — The Single Source of Truth

| Component | Status | Notes |
|-----------|--------|-------|
| `AgentConfig` (agent.toml) | ✅ Implemented | Runtime agent configuration. Contains `capabilities`, `extensions.enabled` whitelist, `provider`, `prompt`, etc. |
| `ExtensionConfig` | ✅ Implemented | Unified whitelist for all extension types (tools, skills, MCP, universal tools) |
| `AgentCapability` | ✅ Implemented | Capability definitions with name, version, description |

**Key principle**: `agent.toml` is the single source of truth for agent behavior. The manifest must not duplicate it.

**Key principle — No capabilities**: The `capabilities` concept (pre-extension system) has been entirely removed. All tool/MCP/skill access is controlled by the extension framework:
- `AgentConfig.extensions.enabled` — per-agent whitelist
- `ExtensionManager` — loads and manages extensions
- `ExtensionConfig.is_extension_enabled()` — runtime access check

---

## 4. Design Decisions

### 4.1 Merge Image into Portable

**Decision**: The `src/image/` module is merged into `src/portable/`. A single `.agent` format serves all packaging use cases.

**Rationale**:
- The image system (`src/image/`) has **zero production consumers** — beautiful abstractions with no users
- The portable system (`src/portable/`) is what users actually interact with via `export`/`import` CLI
- Two parallel systems create mental model burden: users can't `build` an image and then `import` it
- Content-addressable layers and registry push/pull are **features**, not a separate format
- The merge is additive: `.agent` gains layer digests and local registry storage

**What the unified `.agent` format includes**:
- All existing `.agent` content (identity, config, skills, workspace, sessions, MCP)
- Content-addressable layer digests (from former `ImageManifest`)
- Registry tag references (from former `ImageRef`)
- Local registry storage path (for deduplicated layer cache)

**What happens to `src/image/`**:
- `ImageManifest` → merged into `AgentManifest` (add `layers` section)
- `ImageBuilder` → merged into `Packager` (add `build_from_directory()`)
- `ImageRegistry` (local) → merged into `AgentRegistry` (new, in `src/portable/`)
- `ImageDigest`, `LayerType` → moved to `src/portable/types.rs`
- `src/image/` directory → deleted after migration

### 4.2 Clean Manifest — No Extension Duplication

**Decision**: `AgentManifest` contains **only packaging metadata**. It must not duplicate any field that exists in `agent.toml`.

**Rationale**:
- `agent.toml` already defines `capabilities`, `extensions.enabled` (tools/MCP/skills whitelist), `provider`, `prompt`
- Having the same information in two places creates competing sources of truth
- When importing, which wins — manifest or agent.toml? This is a design flaw.
- Packaging metadata (file list, checksums, layer digests) is orthogonal to behavior configuration
- No backward compatibility needed — this is pure dev stage, we make everything fresh

**What the manifest contains** (packaging only):
- `agent` — name, version, description, created_at, export_format, pekobot_version, did
- `identity` — key algorithm, encrypted flag, KDF info
- `layers` — content-addressable digests for each file category
- `packaging` — file list, per-file checksums, compression, archive format
- `signatures` — digital signatures (deferred to Phase 2)

**What the manifest does NOT contain** (behavior lives in `agent.toml`):
- ❌ `capabilities` — **removed concept**; extension framework supersedes this
- ❌ `tools` — defined in `agent.toml` `extensions.enabled` whitelist
- ❌ `mcp` — defined in `agent.toml` or separate `mcp.json`
- ❌ `tool_sources` — defined in `agent.toml` or extension manager
- ❌ `memory` — deprecated, sessions are in `sessions/` layer

**How to discover agent capabilities from a package**:
```bash
# Inspect the package
pekobot agent inspect my-agent.agent
# → shows manifest metadata + layer digests

# Read the actual config (config layer contains agent.toml)
# The config layer is extracted with the rest of the package on import
# After import, agent.toml is the runtime source of truth
```

### 4.3 Unified CLI Commands

**Decision**: All packaging commands live under `pekobot agent` or `pekobot team`. No top-level `build`/`push`/`pull`.

**Before (confusing)**:
```
pekobot agent build ./my-agent -t my-agent:v1.0   ← build .agent
pekobot agent export my-agent -o backup.agent      ← export running agent
pekobot agent push my-agent:v1.0 registry.com/...  ← push .agent
pekobot agent import backup.agent                  ← import .agent
```

**After (unified)**:
```
pekobot agent build ./my-agent -t my-agent:v1.0   ← build .agent from directory
pekobot agent export my-agent -o backup.agent      ← export running agent to .agent
pekobot agent push my-agent:v1.0 registry.com/...  ← push .agent to registry
pekobot agent pull registry.com/...                ← pull .agent from registry
pekobot agent import backup.agent                  ← import .agent
pekobot agent inspect backup.agent                 ← inspect .agent
```

**Rationale**:
- All operations target `.agent` files — single mental model
- `build` and `export` are just two ways to create a `.agent`
- `push`/`pull` operate on `.agent` tags/digests
- Namespace under `agent` avoids polluting top-level CLI

### 4.4 Security: Deferred for Phase 1

**Decision**: Signing and encryption are **explicitly out of scope** for Phase 1.

**What we DO implement**:
- ✅ SHA-256 checksums for all files and layers
- ✅ Checksum verification on import
- ✅ Layer digest verification on push/pull
- ✅ Format version validation

**What we DEFER**:
- ❌ `signature.json` creation and verification
- ❌ Package encryption (crypto.rs stays but remains unwired)
- ❌ DID-based signature verification

### 4.5 Registry: Mock Server for Development

**Decision**: Build a lightweight **mock registry server** as a test fixture.

**Mock server scope**:
- In-memory or file-backed storage
- Implements OCI-inspired protocol for `.agent` layer push/pull
- Supports manifest push/pull, layer push/pull, tag resolution, HEAD checks
- Lives in `e2e_tests/mock_registry/main.py` (Python FastAPI server)

### 4.6 Team Packaging: Snapshot vs Definition

**Decision**: Distinguish between **Team Snapshot** (`.team`) and **Team Definition** (`team.toml`).

| Concept | Format | Contains | Use Case |
|---------|--------|----------|----------|
| **Team Snapshot** | `.team` tar.gz | Full agent data (configs, identities, workspaces, sessions) | Backup, migration, sharing learned state |
| **Team Definition** | `team.toml` | Agent refs, bus config, shared services, topology | Deploy, reproduce, scale |

**For Phase 1**:
- `.team` snapshot gets checksums (data integrity)
- `team.toml` is included in `.team` snapshot for reference
- Full team definition registry push/pull is deferred

### 4.7 Extension Packaging: Bundle Mode Only for Phase 1

**Decision**: Support **one extension distribution mode** for Phase 1:

| Mode | Format | Use Case |
|------|--------|----------|
| **Bundle** | `.ext` tar.gz | Offline distribution, air-gapped environments |

**Source references** (GitHub, URL, MCP) are deferred to Phase 2.

---

## 5. Gap Analysis

### 5.1 P0 Gaps (Must Close for v1.0)

| ID | Gap | Impact | Owner Module |
|----|-----|--------|-------------|
| GAP-001 | No `pekobot agent build` CLI command | Cannot build .agent from directory | `src/commands/agent.rs` |
| GAP-002 | No `pekobot agent push` CLI command | Cannot push .agent to registry | `src/commands/agent.rs` |
| GAP-003 | No `pekobot agent pull` CLI command | Cannot pull .agent from registry | `src/commands/agent.rs` |
| GAP-004 | No content-addressable layers in `.agent` | No deduplication, no registry efficiency | `src/portable/` |
| GAP-005 | No local registry store for layers | Layers stored redundantly | `src/portable/` |
| GAP-006 | No mock registry server for testing | Cannot test registry client | `src/registry/` |
| GAP-007 | Registry client layer existence check is stubbed | Pushes all layers every time | `src/registry/client.rs` |
| GAP-008 | Team packages have no checksums | Cannot verify team package integrity | `src/portable/` |
| GAP-009 | No extension `.ext` package format | Cannot distribute extensions offline | `src/extension/` |
| GAP-010 | No `pekobot ext export` command | Cannot package extensions | `src/commands/ext.rs` |
| GAP-011 | Manifest duplicates extension concerns | Competing source of truth with agent.toml | `src/portable/manifest.rs` |

### 5.2 P1 Gaps (Deferred to Phase 2)

| ID | Gap | Target |
|----|-----|--------|
| GAP-012 | Base image inheritance | Phase 2 |
| GAP-013 | `pekobot validate <path>` command | Phase 2 |
| GAP-014 | Extension source references (GitHub, URL, MCP) | Phase 2 |
| GAP-015 | Extension registry push/pull | Phase 2 |
| GAP-016 | Team definition registry push/pull | Phase 2 |
| GAP-017 | Signing and encryption | Phase 2 |
| GAP-018 | Multi-arch manifest support | Phase 2 |
| GAP-019 | Content deduplication across agents | Phase 2 |
| GAP-020 | `pekobot diff <agent-a> <agent-b>` | Phase 2 |

---

## 6. Unified Architecture

### 6.1 Module Layout (After Merge)

```
src/
├── portable/               # UNIFIED — agent/team packaging + former image/
│   ├── mod.rs              # Re-exports
│   ├── manifest.rs         # MODIFIED: clean manifest — packaging metadata only
│   ├── packager.rs         # MODIFIED: add build_from_directory(), layer digests
│   ├── unpackager.rs       # KEEP: unchanged
│   ├── registry.rs         # NEW: AgentRegistry (local store, from image/registry.rs)
│   ├── types.rs            # NEW: ImageDigest, LayerType (from image/manifest.rs)
│   ├── team_packager.rs    # MODIFIED: add checksums, include team.toml
│   ├── team_unpackager.rs  # MODIFIED: add validation, restore team.toml
│   ├── validation.rs       # KEEP: unchanged
│   └── crypto.rs           # KEEP: unwired
│
├── registry/               # EXISTING — remote registry client
│   ├── mod.rs
│   ├── client.rs           # MODIFIED: operate on .agent layer digests
│   ├── config.rs
│   └── mock_server.rs      # NEW: mock registry for testing
│
├── extension/
│   ├── manager/
│   │   ├── mod.rs          # EXISTING
│   │   └── packaging.rs    # NEW: ExtensionPackager
│   └── types/
│       ├── mod.rs
│       └── source.rs       # NEW: ExtensionSourceRef (minimal, for future)
│
└── commands/
    ├── mod.rs              # MODIFIED: remove Build/Pull/Push top-level
    ├── agent.rs            # MODIFIED: add Build, Push, Pull subcommands
    └── ext.rs              # MODIFIED: add Export subcommand
```

**Deleted**: `src/image/` (entire directory — contents merged into `src/portable/`)

### 6.2 Unified `.agent` Package Format

```
my-agent.agent (gzip-compressed tar)
├── manifest.toml           # Agent metadata + file checksums + layer digests
├── identity/
│   ├── did.json            # DID document
│   └── keys.json           # Private keys (unencrypted for Phase 1)
├── config/
│   ├── agent.toml          # Agent configuration (portable, API keys stripped)
│   └── prompts.toml        # System prompts and personality
├── skills/
│   └── {name}/
│       └── SKILL.md
├── workspace/              # Working files
├── sessions/               # Session history (optional)
└── mcp/
    └── {server}/
        └── bin             # Bundled MCP binaries (optional)
```

**`manifest.toml` schema (Clean — packaging metadata only)**:

```toml
[agent]
name = "researcher"
version = "1.0.0"
description = "A research assistant agent"
created_at = "2026-05-08T10:00:00Z"
export_format = "2.0"       # Bumped for unified format
pekobot_version = "0.1.0"
did = "did:pekobot:local:abc123..."

[identity]
key_algorithm = "ed25519"
encrypted = false

# Content-addressable layers (merged from ImageManifest)
# Each layer is a category of files with a SHA-256 digest.
# The config layer contains agent.toml — the single source of truth
# for agent behavior (tools, MCP, skills, capabilities).
[layers]
config = "sha256:abc123..."      # config/ directory (contains agent.toml)
identity = "sha256:def456..."    # identity/ directory
skills = "sha256:ghi789..."      # skills/ directory (optional)
workspace = "sha256:jkl012..."   # workspace/ directory (optional)
sessions = "sha256:mno345..."    # sessions/ directory (optional)
mcp = "sha256:pqr678..."         # mcp/ directory (optional)

[packaging]
files = ["manifest.toml", "identity/did.json", "config/agent.toml", ...]
checksums = { "manifest.toml" = "sha256:...", ... }
compression = "gzip"
archive_format = "tar"
```

**What is NOT in the manifest** (lives in `agent.toml` in the `config` layer):
- ❌ Capabilities list
- ❌ Tool whitelist
- ❌ MCP server definitions
- ❌ Extension settings
- ❌ Provider configuration
- ❌ Prompt configuration

**Why this is correct**: When you import a `.agent` package, the `config` layer is extracted and `agent.toml` becomes the runtime configuration. The manifest's job is to ensure the package is complete and unmodified — not to describe what the agent does.

**Why capabilities were removed**: The `capabilities` concept predated the extension framework and created parallel access control. `AgentCapability` was declarative ("this agent CAN do X") but never actually enforced. The extension framework's `extensions.enabled` whitelist is the actual enforcement mechanism. Having both was confusing and redundant.

### 6.3 Layer Semantics

Each layer is a **category of files** with a SHA-256 digest. Layers enable:
- **Deduplication**: Same layer content stored once in local registry
- **Incremental push/pull**: Only changed layers transferred
- **Integrity**: Digest verified on every operation

| Layer Name | Source Files | Optional | Contains Behavior Config? |
|------------|-------------|----------|---------------------------|
| `config` | `config/agent.toml`, `config/prompts.toml` | No | ✅ Yes — agent.toml is the SSOT |
| `identity` | `identity/did.json`, `identity/keys.json` | No | ❌ No |
| `skills` | `skills/**` | Yes | ❌ No (skill content, not config) |
| `workspace` | `workspace/**` | Yes | ❌ No |
| `sessions` | `sessions/**` | Yes | ❌ No |
| `mcp` | `mcp/**` | Yes | ❌ No (binaries, not config) |

**Layer digest computation**:
1. Collect all files in the category
2. Sort by path (deterministic order)
3. Concatenate: `path1\ncontent1\npath2\ncontent2\n...`
4. SHA-256 of concatenated bytes

### 6.4 Local Registry Store

The local registry store provides content-addressable storage for layers:

```
~/.pekobot/registry/
├── layers/
│   └── sha256-abc123.../
│       └── layer.tar.gz      # Deduplicated layer content
├── manifests/
│   └── sha256-xyz789.../
│       └── manifest.toml     # Agent manifest by digest
└── tags/
    └── my-agent_v1.0         # Tag file contains manifest digest
```

**Operations**:
- `store_layer(digest, bytes)` → write if not exists
- `get_layer(digest)` → read layer bytes
- `store_manifest(manifest)` → write manifest, update tag
- `get_manifest_by_tag(tag)` → resolve tag to manifest
- `get_manifest_by_digest(digest)` → read manifest directly

### 6.5 Team Snapshot Format

```
my-team.team (gzip-compressed tar)
├── team/
│   ├── manifest.toml       # Team metadata + file checksums
│   └── team.toml           # Team runtime definition (for reference, optional)
├── agents/
│   ├── {agent-name}/
│   │   ├── manifest.toml   # Agent manifest (unified format, clean)
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

### 6.6 Extension Package Format

```
docker-skill.ext (gzip-compressed tar)
├── manifest.toml           # Extension package metadata
└── extension/
    ├── manifest.yaml       # Extension type manifest
    ├── SKILL.md            # Type-specific entry point
    └── ...                 # Additional files
```

---

## 7. Implementation Plan

### 7.1 Phase 1: Foundation — Mock Registry + CLI Scaffolding

| Task | Description | File(s) |
|------|-------------|---------|
| PKG-F1 | Mock registry server | `src/registry/mock_server.rs` (new) |
| PKG-F2 | Add `Build` subcommand to `AgentCommands` | `src/commands/agent.rs` |
| PKG-F3 | Add `Push` subcommand to `AgentCommands` | `src/commands/agent.rs` |
| PKG-F4 | Add `Pull` subcommand to `AgentCommands` | `src/commands/agent.rs` |
| PKG-F5 | Add `Export` subcommand to `ExtCommands` | `src/commands/ext.rs` |

**Exit criteria**:
- [ ] `cargo build` succeeds
- [ ] `pekobot agent build --help` prints usage
- [ ] `pekobot agent push --help` prints usage
- [ ] `pekobot agent pull --help` prints usage
- [ ] `pekobot ext export --help` prints usage
- [ ] Mock server starts and responds to `/v2/`

### 7.2 Phase 2: Clean Manifest + Merge Image into Portable

**⚠️ Critical**: This phase restructures `AgentManifest`. Do not start Phase 3 until fully verified.

| Task | Description | File(s) |
|------|-------------|---------|
| PKG-M0 | **Clean `AgentManifest`** — remove `capabilities`, `tools`, `mcp`, `tool_sources`, `memory` | `src/portable/manifest.rs` |
| PKG-M1 | Create `src/portable/types.rs` with `ImageDigest`, `LayerType` | `src/portable/types.rs` (new) |
| PKG-M2 | Add `layers` section to `AgentManifest` | `src/portable/manifest.rs` |
| PKG-M3 | Add `build_from_directory()` to `Packager` | `src/portable/packager.rs` |
| PKG-M4 | Create `AgentRegistry` (local store) | `src/portable/registry.rs` (new) |
| PKG-M5 | Delete `src/image/` after migration | `src/image/` (delete) |
| PKG-M6 | Update all imports from `crate::image` to `crate::portable` | Various |

**Exit criteria**:
- [ ] `cargo test portable` passes
- [ ] `cargo test` has no `image::` module references
- [ ] `Packager::build_from_directory()` produces `.agent` with layer digests
- [ ] `AgentRegistry` stores/retrieves layers and manifests
- [ ] Manifest has no `capabilities`, `tools`, `mcp`, `tool_sources` fields
- [ ] `cargo clippy` clean in `src/portable/`

### 7.3 Phase 3: Registry Integration

| Task | Description | File(s) |
|------|-------------|---------|
| PKG-R1 | Implement layer existence check (HEAD) | `src/registry/client.rs` |
| PKG-R2 | Implement `pekobot agent push` | `src/commands/agent.rs` |
| PKG-R3 | Implement `pekobot agent pull` | `src/commands/agent.rs` |
| PKG-R4 | Registry integration tests | `tests/registry_integration.rs` |

**Exit criteria**:
- [ ] `cargo test registry_integration` passes
- [ ] Push skips existing layers
- [ ] Pull roundtrip verified with mock server

### 7.4 Phase 4: Image Build CLI

| Task | Description | File(s) |
|------|-------------|---------|
| PKG-B1 | Implement `pekobot agent build` | `src/commands/agent.rs` |
| PKG-B2 | Build command tests | `src/commands/agent.rs` or `tests/build_integration.rs` |

**Exit criteria**:
- [ ] `pekobot agent build ./test-agent -t test:v1.0` produces valid `.agent`
- [ ] Build progress output readable

### 7.5 Phase 5: Team Packaging Hardening

| Task | Description | File(s) |
|------|-------------|---------|
| PKG-T1 | Add checksums to `TeamManifest` | `src/portable/team_packager.rs` |
| PKG-T2 | Include `team.toml` in `.team` export | `src/portable/team_packager.rs` |
| PKG-T3 | Validate checksums on import | `src/portable/team_unpackager.rs` |
| PKG-T4 | Restore `team.toml` on import | `src/portable/team_unpackager.rs` |
| PKG-T5 | Team packaging integration tests | `tests/team_integration.rs` |

**Exit criteria**:
- [ ] Team export includes checksums
- [ ] Team import validates checksums
- [ ] `team.toml` roundtrips through export/import

### 7.6 Phase 6: Extension Packaging

| Task | Description | File(s) |
|------|-------------|---------|
| PKG-E1 | Implement `ExtensionPackager` | `src/extension/manager/packaging.rs` |
| PKG-E2 | Wire `pekobot ext export` | `src/commands/ext.rs` |
| PKG-E3 | Extension export integration tests | `tests/extension_packaging.rs` |

**Exit criteria**:
- [ ] `pekobot ext export` creates valid `.ext`
- [ ] `.ext` can be installed via `pekobot ext install`

### 7.7 Phase 7: Final Integration

| Task | Description |
|------|-------------|
| PKG-I1 | Full integration test: build → push → pull → import |
| PKG-I2 | Update `DATA_MODEL.md` |
| PKG-I3 | Update `AGENTS.md` |
| PKG-I4 | Clippy cleanup |
| PKG-I5 | Coverage check (≥ 70% for packaging modules) |
| PKG-M7 | Delete `src/image/` directory | Deleted after migration verified |
| PKG-M8 | Delete obsolete command files (`build.rs`, `push.rs`, `pull.rs`) | Deleted after commands moved under `agent` |
| PKG-F6 | Remove top-level `Build`/`Push`/`Pull` from `Commands` enum | Moved under `AgentCommands` |
| PKG-R5 | Registry integration tests | `tests/registry_integration.rs` |

---

## 8. CLI Command Reference

### 8.1 Agent Commands (Unified)

```
pekobot agent build <path> -t <name:tag> [--json]
    Build a .agent package from a directory.
    Example: pekobot agent build ./my-agent -t my-agent:v1.0

pekobot agent export <name> -o <file.agent> [--no-sessions] [--no-workspace]
    Export a running agent to a .agent package.
    Example: pekobot agent export researcher -o researcher-backup.agent

pekobot agent import <file.agent> [--name <new-name>] [--force]
    Import a .agent package.
    Example: pekobot agent import ./researcher-backup.agent

pekobot agent inspect <file.agent>
    Show information about a .agent package without importing.
    Displays manifest metadata and layer digests.
    To see agent behavior config, inspect the config layer (agent.toml).

pekobot agent push <local-tag> <registry-ref>
    Push a local .agent to a registry.
    Example: pekobot agent push researcher:v1.0 pekohub.com/agents/researcher:v1.0

pekobot agent pull <registry-ref>
    Pull a .agent from a registry.
    Example: pekobot agent pull pekohub.com/agents/researcher:v2.5
```

### 8.2 Team Commands

```
pekobot team export <name> -o <file.team> [--no-sessions]
    Export a team snapshot.

pekobot team import <file.team> [--name <new-name>] [--force]
    Import a team snapshot.

pekobot team deploy <team.toml>
    Deploy a team from a definition file.
```

### 8.3 Extension Commands

```
pekobot ext install <path>
    Install an extension from a local path or .ext file.

pekobot ext export <id> -o <file.ext>
    Export an installed extension to a .ext package.

pekobot ext list
    List installed extensions.
```

### 8.4 Removed Commands

The following commands from earlier drafts are **removed**:

```
pekobot build <path>              → Use pekobot agent build
pekobot push <local> <remote>     → Use pekobot agent push
pekobot pull <registry-ref>       → Use pekobot agent pull
pekobot agent run <ref>           → Deferred; no agent runtime
pekobot validate <path>           → Deferred
```

---

## 9. Registry Protocol

### 9.1 Manifest Wire Format

The registry protocol uses **JSON** as the manifest wire format (`RegistryManifest`), not TOML. The `RegistryClient` serializes manifests to JSON for push and deserializes from JSON on pull. The local `AgentManifest` (inside `.agent` packages) remains TOML.

### 9.2 Mock Registry Server

A Python-based FastAPI mock registry server is provided for integration testing:

```bash
python e2e_tests/mock_registry/main.py --port 18765
```

**Implementation**: `e2e_tests/mock_registry/main.py`

**Features**:
- In-memory or file-backed storage (`--storage-dir`)
- All OCI-inspired endpoints for `.agent` push/pull
- Supports manifest push/pull, layer push/pull, tag resolution, HEAD checks

### 9.3 Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/v2/` | GET | Registry capability check |
| `/v2/{name}/manifests/{reference}` | GET/PUT | Manifest pull/push (JSON) |
| `/v2/{name}/blobs/{digest}` | GET | Layer pull |
| `/v2/{name}/blobs/uploads/` | POST | Initiate layer upload |
| `/v2/{name}/blobs/uploads/{uuid}` | PUT | Complete layer upload |
| `/v2/{name}/blobs/{digest}` | HEAD | Layer existence check |

---

## 10. UX Use Cases

### UC-1: Build and Push an Agent to Registry

```bash
# Build .agent from directory
pekobot agent build ./my-agent -t my-agent:v1.0

# Push to registry
pekobot agent push my-agent:v1.0 pekohub.com/users/me/my-agent:v1.0
```

### UC-2: Pull and Import an Agent from Registry

```bash
# Pull from registry
pekobot agent pull pekohub.com/agents/researcher:v2.5

# Agent is now available locally as researcher:v2.5
pekobot agent inspect researcher:v2.5

# Import creates the agent in your local config
pekobot agent import researcher:v2.5 --name my-researcher
```

### UC-3: Export and Share a Running Agent

```bash
# Export with all state
pekobot agent export my-researcher -o researcher-with-memories.agent

# Share the file
cp researcher-with-memories.agent /shared/

# Recipient imports
pekobot agent import /shared/researcher-with-memories.agent
```

### UC-4: Inspect Package Without Importing

```bash
# See packaging metadata and layer digests
pekobot agent inspect researcher-with-memories.agent

# Output:
# Name: researcher
# Version: 1.0.0
# DID: did:pekobot:local:abc123...
# Layers:
#   config     sha256:abc123...  (2.1 KB)
#   identity   sha256:def456...  (1.5 KB)
#   skills     sha256:ghi789...  (45 KB)
#   workspace  sha256:jkl012...  (12 KB)
# Total size: 60.6 KB

# To see agent behavior (tools, capabilities), read the config layer:
# tar -xzf researcher-with-memories.agent -O config/agent.toml
```

### UC-5: Save and Share a Team Snapshot

```bash
# Export full team state
pekobot team export research-team -o research-team-v1.team

# Share and import elsewhere
pekobot team import research-team-v1.team

# Deploy with definition
pekobot team deploy research-team/team.toml
```

### UC-6: Team Backup and Disaster Recovery

```bash
# Cron job backup
pekobot team export research-team -o backups/research-team-$(date +%Y%m%d).team

# Restore
pekobot team import backups/research-team-20260508.team --force
```

---

## 11. Success Criteria

Phase 1 packaging is complete when:

1. ✅ All P0 gaps (GAP-001 through GAP-011) are closed
2. ✅ `src/image/` is fully merged into `src/portable/`
3. ✅ `cargo test` passes with ≥ 70% coverage for packaging modules
4. ✅ `cargo clippy` passes with zero warnings in packaging code
5. ✅ Unified CLI commands (`agent build`, `agent push`, `agent pull`, `ext export`) are functional
6. ✅ `.agent` packages include content-addressable layer digests
7. ✅ Local registry store deduplicates layers
8. ✅ Team snapshot packages pass checksum validation on import
9. ✅ `team.toml` is included in `.team` exports for reference
10. ✅ Registry push/pull works against the mock server
11. ✅ `.ext` packages can be created and installed
12. ✅ Registry client skips existing layers on push (HEAD check works)
13. ✅ `AgentManifest` has no `capabilities`, `tools`, `mcp`, `tool_sources` fields
14. ✅ `AgentCapability`, `TeamCapabilityConfig`, `CapabilitiesConfig` removed from codebase
15. ✅ `AGENTS.md` updated to reflect merged architecture, clean manifest, and removed capabilities

---

## Appendix A: Deferred to Phase 2

| Feature | Rationale | Effort |
|---------|-----------|--------|
| Base image inheritance | No clear consumer | Medium |
| `pekobot validate <path>` | Partially covered by inspect | Small |
| Extension source references | Complex, not critical | Medium |
| Extension registry push/pull | No protocol defined | Medium |
| Team definition registry push/pull | No protocol defined | Medium |
| Signing and encryption | Security for shared packages | Medium |
| Multi-arch manifest support | Platform-specific binaries | Small |
| Content deduplication across agents | Storage optimization | Small |
| `pekobot diff <agent-a> <agent-b>` | Debugging tool | Small |

---

## Appendix B: Manifest Schema Comparison

### Before (v1.x — overreaching)

```toml
[agent]
name = "researcher"
version = "1.0.0"

[identity]
key_algorithm = "ed25519"
encrypted = false

[capabilities]          # ← DUPLICATE of agent.toml
names = ["web_search", "read_file"]

[tools]                 # ← DUPLICATE of agent.toml extensions.enabled
required = ["web_search", "read_file"]
optional = ["browser"]

[mcp]                   # ← DUPLICATE of agent.toml or mcp.json
[[mcp.server]]
name = "filesystem"
transport = "stdio"

[tool_sources]          # ← DUPLICATE of extension manager
required = [{ name = "docker", version = "^1.0" }]

[packaging]
files = ["..."]
checksums = { "..." = "sha256:..." }
```

### After (v2.0 — clean, packaging only)

```toml
[agent]
name = "researcher"
version = "1.0.0"
description = "A research assistant"
created_at = "2026-05-08T10:00:00Z"
export_format = "2.0"
pekobot_version = "0.1.0"
did = "did:pekobot:local:abc123..."

[identity]
key_algorithm = "ed25519"
encrypted = false

[layers]                # ← NEW: content-addressable digests
config = "sha256:abc123..."      # ← contains agent.toml (SSOT)
identity = "sha256:def456..."
skills = "sha256:ghi789..."
workspace = "sha256:jkl012..."
sessions = "sha256:mno345..."
mcp = "sha256:pqr678..."

[packaging]
files = ["manifest.toml", "identity/did.json", "config/agent.toml", ...]
checksums = { "manifest.toml" = "sha256:...", ... }
compression = "gzip"
archive_format = "tar"
```

**Behavior config lives in `config/agent.toml` (the `config` layer)**:

```toml
# This file is inside the .agent package, in the config layer
name = "researcher"
capabilities = [
    { name = "web_search", version = "1.0" },
    { name = "read_file", version = "2.1" }
]

[extensions]
enabled = ["web_search", "read_file", "browser", "mcp:filesystem"]

[provider]
model = "gpt-4"
```

---

## Appendix C: File Checklist

### Files to Create

| File | Purpose |
|------|---------|
| `src/portable/types.rs` | `ImageDigest`, `LayerType`, `LayerDigest` (from image/) |
| `src/portable/registry.rs` | `AgentRegistry` — local content-addressable store |
| `e2e_tests/mock_registry/main.py` | Mock registry server for testing (Python FastAPI) |
| `src/extension/manager/packaging.rs` | `ExtensionPackager` |
| `src/extension/types/source.rs` | `ExtensionSourceRef` types (minimal) |
| `tests/registry_integration.rs` | Registry push/pull integration tests |
| `tests/team_integration.rs` | Team export/import integration tests |
| `tests/extension_packaging.rs` | Extension export/install roundtrip tests |

### Files to Modify

| File | Changes |
|------|---------|
| `src/portable/manifest.rs` | **Clean manifest**: remove `capabilities`, `tools`, `mcp`, `tool_sources`, `memory`; add `layers` section |
| `src/portable/packager.rs` | Add `build_from_directory()`, compute layer digests |
| `src/portable/team_packager.rs` | Add checksums; include `team.toml` |
| `src/portable/team_unpackager.rs` | Validate checksums; restore `team.toml` |
| `src/portable/mod.rs` | Re-export new types |
| `src/registry/client.rs` | Operate on `.agent` layer digests; implement HEAD check |
| `src/commands/agent.rs` | Add `Build`, `Push`, `Pull` subcommands |
| `src/commands/ext.rs` | Add `Export` subcommand |
| `src/commands/mod.rs` | Remove top-level `Build`, `Push`, `Pull` variants |

### Files to Delete

| File | Reason |
|------|--------|
| `src/image/mod.rs` | Merged into portable |
| `src/image/manifest.rs` | Merged into portable/manifest.rs |
| `src/image/builder.rs` | Merged into portable/packager.rs |
| `src/image/registry.rs` | Merged into portable/registry.rs |
| `src/image/config.rs` | Merged into portable/types.rs |
| `src/commands/build.rs` | Command moved under `agent` |
| `src/commands/push.rs` | Command moved under `agent` |
| `src/commands/pull.rs` | Command moved under `agent` |

---

*End of Packaging Specification v2.2*
