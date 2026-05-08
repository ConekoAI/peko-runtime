# Pekobot Packaging — Implementation Plan v2.2

> **Version**: 2.2-draft  
> **Status**: In Progress — Phases 1, 2, 3, 4 & 5 Complete  
> **Source Spec**: [`Packaging_Spec.md`](./Packaging_Spec.md)  
> **Last Updated**: 2026-05-08  
> **Key Decisions**: 
> - `src/image/` merged into `src/portable/`. All packaging commands unified under `pekobot agent`.
> - **Clean Manifest**: `AgentManifest` stripped of `capabilities`, `tools`, `mcp`, `tool_sources`, `memory`. Packaging metadata only. `agent.toml` is the single source of truth.
> - **Capabilities Removed**: `AgentCapability`, `TeamCapabilityConfig`, `CapabilitiesConfig` entirely removed. Extension framework (`extensions.enabled`) is the single source of truth.

---

## Progress Log

### 2026-05-08 — Phase 5 Complete

**What was done:**
- Added `TeamPackagingMetadata` struct to `src/portable/team_packager.rs` with `files`, `checksums`, `compression`, `archive_format`
- Added `packaging: Option<TeamPackagingMetadata>` field to `TeamManifest`
- `TeamPackager::create_team_archive()` now computes SHA-256 checksums for every file in the archive
- `TeamPackager` includes `team.toml` in `.team` export if it exists in the team directory (`base_dir/teams/{name}/team.toml`)
- `TeamUnpackager::import()` validates all checksums before importing agents; fails fast on mismatch with clear error
- `TeamUnpackager` warns (but continues) if no `packaging` metadata is present (legacy packages)
- `TeamUnpackager::import()` restores `team/team.toml` to the team directory if present in package
- Exported `TeamPackagingMetadata` from `src/portable/mod.rs`
- Created `tests/team_integration.rs` with 4 tests:
  - `test_team_export_with_checksums` — verifies manifest has packaging metadata, checksums, and team.toml
  - `test_team_import_with_checksum_validation` — export → import roundtrip with checksum verification
  - `test_team_import_fails_on_checksum_mismatch` — tampered package fails with "Checksum mismatch"
  - `test_team_import_warns_without_checksums` — legacy packages (no packaging metadata) import successfully with warning

**Verification:**
- `cargo build --lib` — ✅ succeeds
- `cargo test --lib` — ✅ 964 passed, 0 failed, 19 ignored
- `cargo test --test team_integration` — ✅ 4 passed, 0 failed
- `cargo clippy --all-targets` — ✅ no errors (only pre-existing warnings)

**Key decisions:**
- Checksums are computed for ALL files in the package (agent files + team.toml), stored as `sha256:...` hex strings
- The manifest itself is NOT included in the checksum list (it's validated by TOML parsing)
- Legacy packages without `packaging` metadata are supported with a warning — no breaking change
- `team.toml` is optional during export — package succeeds even if it doesn't exist

---

### 2026-05-08 — Phases 1 & 2 Complete

**What was done:**
- Created `src/portable/types.rs` with `ImageDigest`, `LayerType`, `Layer`, `LayerDigest`, `compute_digest`
- Created `src/portable/registry.rs` with `AgentRegistry` (local content-addressable store)
- Created `src/registry/manifest.rs` with `RegistryManifest` (JSON wire format for registry protocol)
- Adapted `src/registry/client.rs` to use `crate::portable::types` instead of `crate::image`
- Deleted `src/image/` directory (5 files: `mod.rs`, `manifest.rs`, `builder.rs`, `config.rs`, `registry.rs`)
- Removed `pub mod image` from `src/lib.rs`
- Added `Build`, `Push`, `Pull` subcommands to `AgentCommands` (stub handlers)
- Added `Export` subcommand to `ExtCommands` (stub handler)
- Updated `src/portable/mod.rs` re-exports

**Verification:**
- `cargo build` — ✅ succeeds
- `cargo test` — ✅ 959 passed, 0 failed
- `cargo clippy --all-targets` — ✅ passes (warnings only, no errors)
- `crate::image` references in `src/` — ✅ zero matches
- `pekobot agent build --help` — ✅ works
- `pekobot agent push --help` — ✅ works
- `pekobot agent pull --help` — ✅ works
- `pekobot ext export --help` — ✅ works

**What was deferred:**
- `Packager::build_from_directory()` → moved to Phase 4 (Build CLI)
- Mock registry server → moved to Phase 3 (created alongside registry integration tests)

---

### 2026-05-08 — Phase 3 Complete

**What was done:**
- Created `e2e_tests/mock_registry/main.py` — FastAPI mock registry server (in-memory + file-backed)
- Updated `RegistryClient` to use `AgentRegistry` for local layer/manifest storage
- Implemented `check_existing_layers()` with async HEAD requests to skip already-present layers
- Fixed `RegistryRef::parse()` to handle ports (`host:port/path:tag`)
- Fixed `registry_url()` to use `http://` for localhost/127.0.0.1
- Added `RegistryClient::new()` with `.no_proxy()` to bypass system proxy settings
- Wired `pekobot agent push` — loads local manifest, converts to `RegistryManifest`, pushes with progress
- Wired `pekobot agent pull` — pulls manifest + layers, stores in `AgentRegistry`
- Created `tests/registry_integration.rs` with 4 tests: manifest roundtrip, blob roundtrip, push+pull E2E, layer skip
- Added `AgentRegistry::root_path()` accessor for `RegistryClient` manifest storage

**Verification:**
- `cargo build` — ✅ succeeds
- `cargo test --lib` — ✅ 960 passed, 0 failed
- `cargo test --test registry_integration -- --ignored` — ✅ 4 passed, 0 failed
- `cargo clippy --all-targets` — ✅ no errors
- `pekobot agent push --help` — ✅ works
- `pekobot agent pull --help` — ✅ works

**Key fixes during implementation:**
1. **Proxy bypass** — `reqwest` on Windows picked up system proxy settings causing 503 errors. Fixed with `.no_proxy()`.
2. **Port parsing** — `RegistryRef::parse("127.0.0.1:18765/agent:v1.0")` incorrectly split on the port's `:`. Fixed by only splitting on `:` after the first `/`.
3. **HTTP for localhost** — `registry_url()` forced `https://` for all URLs. Fixed to use `http://` for localhost/127.0.0.1.
4. **Digest mismatch** — integration tests used fake digest strings. Fixed by computing real SHA-256 digests.

**What was deferred:**
- `Packager::build_from_directory()` → Phase 4 (Build CLI)

---

### 2026-05-08 — Phase 4 Complete

**What was done:**
- Created `src/portable/builder.rs` with `AgentBuilder` struct (separate from `Packager`)
- Implemented `AgentBuilder::build_from_directory()` — reads source dir, builds content-addressable layers, creates `.agent` package
- Layer building: required layers (config, identity) + optional layers (skills, workspace, sessions, mcp)
- Layer digest computation: deterministic (sorted file paths → tarball → SHA-256)
- Layer storage in `AgentRegistry` (content-addressable, deduplicated)
- `.agent` archive creation: `manifest.toml` + layer files in flattened structure
- Wired `handle_agent_build()` in `src/commands/agent/handlers.rs` with human-readable and `--json` output
- Progress callbacks: `BuildProgress` enum with Reading/Layering/Complete events
- Created `tests/build_integration.rs` with 3 tests: valid package build, missing config error, layer deduplication

**Verification:**
- `cargo build --lib` — ✅ succeeds
- `cargo test --lib` — ✅ 964 passed, 0 failed, 19 ignored
- `cargo test --test build_integration` — ✅ 3 passed, 0 failed
- `cargo clippy --all-targets` — ✅ no errors
- `pekobot agent build --help` — ✅ works

**Architecture decision:**
- `AgentBuilder` is separate from `Packager`:
  - `Packager` → exports an already-configured agent from memory (`AgentConfig` + `Identity`)
  - `AgentBuilder` → builds a `.agent` from a source directory on disk
- This keeps the two paths (export vs build) cleanly separated.

**What was deferred:**
- Phase 5 (Team packaging hardening) — started next
- Phase 6 (Extension packaging) — can run in parallel with Phase 5

---

## How to Use This Document

This plan breaks the spec into **7 sequential phases**. Each phase has:
- A clear deliverable
- Task-level detail with file paths
- Exit criteria (what "done" means)
- Estimated effort

**For developers**: Pick a phase, implement all tasks, verify exit criteria, then move to the next.

**For reviewers**: Review per phase. Each phase should be a self-contained PR.

---

## Development Principles

1. **Mock server first** — All registry/client work needs it
2. **Clean manifest first** — Strip `AgentManifest` before adding layers (avoids migrating dead fields)
3. **Merge before features** — Merge `src/image/` into `src/portable/` before adding new capabilities
4. **One module at a time** — Finish tests for a module before moving on
5. **Integration tests as you go** — Don't defer all testing to the end
6. **Delete after migration** — Keep `src/image/` until Phase 2 is verified, then delete
7. **Capabilities first** — Remove `AgentCapability` and `TeamCapabilityConfig` before adding new features to avoid migrating dead code

---

## Phase Overview

| Phase | Focus | Duration | Status | Deliverable |
|-------|-------|----------|--------|-------------|
| [Phase 1](#phase-1-foundation) | Mock registry + CLI scaffolding | 2–3 days | ✅ Complete | Commands parse args; `build`/`push`/`pull`/`export` stubs wired |
| [Phase 2](#phase-2-clean-manifest--merge-image-into-portable) | Clean manifest + merge `src/image/` into `src/portable/` + remove capabilities | 4–5 days | ✅ Complete | `src/image/` deleted; `AgentRegistry` + portable types created; `RegistryClient` adapted |
| [Phase 3](#phase-3-registry-integration) | Registry push/pull with mock | 3–4 days | ✅ Complete | `agent push`/`pull` work end-to-end against mock server |
| [Phase 4](#phase-4-image-build-cli) | `agent build` command | 1–2 days | ✅ Complete | Build `.agent` from directory |
| [Phase 5](#phase-5-team-packaging-hardening) | Team checksums + `team.toml` | 2–3 days | ✅ Complete | `.team` integrity checks |
| [Phase 6](#phase-6-extension-packaging) | `.ext` export | 2–3 days | ⬜ Not started | `ext export` creates installable `.ext` |
| [Phase 7](#phase-7-final-integration) | Integration tests + docs | 2–3 days | ⬜ Not started | All tests pass, docs updated |

**Total estimated effort**: 16–23 developer-days (3–4 weeks for 1 developer, 2 weeks for 2 developers in parallel)

---

## Phase 1: Foundation

**Goal**: Mock registry server + CLI command scaffolding. Unblocks all subsequent phases.

### Tasks

#### PKG-F1: Mock Registry Server

| Field | Value |
|-------|-------|
| **Task ID** | PKG-F1 |
| **File** | `src/registry/mock_server.rs` (new) |
| **Description** | Build an in-memory mock registry server implementing OCI-inspired protocol for `.agent` layer push/pull |
| **Acceptance Criteria** | 1. Server starts on random port (`port: 0`)<br>2. Implements all endpoints from §9 of spec<br>3. Supports manifest push/pull, layer push/pull, tag resolution<br>4. Supports HEAD for layer existence checks<br>5. Stores blobs and manifests in memory (HashMap) |

**Endpoints**:

```
GET    /v2/                          → 200
GET    /v2/{name}/manifests/{ref}    → manifest TOML
PUT    /v2/{name}/manifests/{ref}    → store manifest
HEAD   /v2/{name}/blobs/{digest}     → 200 if exists, 404 if not
GET    /v2/{name}/blobs/{digest}     → layer bytes
POST   /v2/{name}/blobs/uploads/     → 202 + upload URL
PUT    /v2/{name}/blobs/uploads/{uuid} → complete upload
```

**Test fixture**:

```rust
#[tokio::test]
async fn test_mock_registry_manifest_roundtrip() {
    let server = MockRegistryServer::new(0).await;
    let handle = server.start().await;
    
    let client = reqwest::Client::new();
    let url = format!("{}/v2/test/manifests/latest", server.base_url());
    
    let manifest = "[agent]\nname = \"test\"\n";
    client.put(&url).body(manifest).send().await.unwrap();
    
    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.text().await.unwrap().contains("test"));
    
    handle.shutdown().await;
}
```

---

#### PKG-F2: CLI Scaffolding — `pekobot agent build`

| Field | Value |
|-------|-------|
| **Task ID** | PKG-F2 |
| **File** | `src/commands/agent.rs` |
| **Description** | Add `Build` variant to `AgentCommands` enum and stub handler |
| **Acceptance Criteria** | 1. `pekobot agent build --help` prints usage<br>2. Parses `path` and `-t <tag>` args<br>3. Handler prints "not yet implemented" and exits 0 |

**Args**:

```rust
Build {
    /// Path to agent directory
    path: PathBuf,
    /// Tag (name:tag format)
    #[arg(short, long)]
    tag: String,
    /// Output as JSON
    #[arg(long)]
    json: bool,
}
```

---

#### PKG-F3: CLI Scaffolding — `pekobot agent push`

| Field | Value |
|-------|-------|
| **Task ID** | PKG-F3 |
| **File** | `src/commands/agent.rs` |
| **Description** | Add `Push` variant to `AgentCommands` enum and stub handler |
| **Acceptance Criteria** | 1. `pekobot agent push --help` prints usage<br>2. Parses local tag and remote registry ref<br>3. Handler prints "not yet implemented" and exits 0 |

---

#### PKG-F4: CLI Scaffolding — `pekobot agent pull`

| Field | Value |
|-------|-------|
| **Task ID** | PKG-F4 |
| **File** | `src/commands/agent.rs` |
| **Description** | Add `Pull` variant to `AgentCommands` enum and stub handler |
| **Acceptance Criteria** | 1. `pekobot agent pull --help` prints usage<br>2. Parses registry ref<br>3. Handler prints "not yet implemented" and exits 0 |

---

#### PKG-F5: CLI Scaffolding — `pekobot ext export`

| Field | Value |
|-------|-------|
| **Task ID** | PKG-F5 |
| **File** | `src/commands/ext.rs` |
| **Description** | Add `Export` variant to `ExtCommands` enum and stub handler |
| **Acceptance Criteria** | 1. `pekobot ext export --help` prints usage<br>2. Parses extension id and `-o <path>`<br>3. Handler prints "not yet implemented" and exits 0 |

---

#### PKG-F6: Remove Top-Level Build/Push/Pull from Commands Enum

| Field | Value |
|-------|-------|
| **Task ID** | PKG-F6 |
| **File** | `src/commands/mod.rs` |
| **Description** | Remove `Build`, `Push`, `Pull` variants from top-level `Commands` enum |
| **Acceptance Criteria** | 1. `Commands` enum has no `Build`, `Push`, `Pull` variants<br>2. `cargo build` succeeds<br>3. No dead code in `src/commands/build.rs`, `push.rs`, `pull.rs` (will delete in Phase 2) |

---

### Phase 1 Exit Criteria

- [x] `cargo build` succeeds with zero errors
- [x] `pekobot agent build --help`, `pekobot agent push --help`, `pekobot agent pull --help`, `pekobot ext export --help` all print correct usage
- [x] `cargo clippy` has zero warnings in new code
- [ ] `cargo test registry::mock_server` passes — **deferred to Phase 3** (mock server created alongside registry integration tests)

---

## Phase 2: Clean Manifest + Merge Image into Portable

**Goal**: `AgentManifest` stripped of extension concerns. `src/image/` fully merged into `src/portable/`. `.agent` format gains content-addressable layers.

**Prerequisite**: Phase 1 complete.

**⚠️ Critical**: This phase restructures `AgentManifest` and touches many files. Do not start Phase 3 until fully verified.

### Tasks

#### PKG-M0: Remove `AgentCapability` and `TeamCapabilityConfig` from Codebase

| Field | Value |
|-------|-------|
| **Task ID** | PKG-M0 |
| **Files** | `src/types/agent.rs`, `src/team/capability.rs` (delete), `src/agent/context.rs`, `src/agent/types.rs`, `src/common/services/config_authority/entry.rs`, `src/common/services/agent_service.rs` |
| **Description** | Remove the pre-extension `capabilities` concept entirely. `AgentCapability`, `TeamCapabilityConfig`, `CapabilityIndex` are all superseded by the extension framework. |
| **Acceptance Criteria** | 1. Remove `AgentCapability` struct from `src/types/agent.rs`<br>2. Remove `CapabilityParameter` struct<br>3. Remove `capabilities: Vec<AgentCapability>` field from `AgentConfig`<br>4. Delete `src/team/capability.rs`<br>5. Remove `pub mod capability` from `src/team/mod.rs`<br>6. Rename `CapabilityIndex` → `ExtensionIndex` in `src/agent/context.rs`<br>7. Rename `AgentSummary.capabilities` → `extensions`<br>8. Rename `AgentInfo.capabilities` → `extensions`<br>9. Rename `ManagerEvent::AgentDiscovered.capabilities` → `extensions`<br>10. Remove `AgentConfigEntry.capabilities()` and `has_capability()` methods<br>11. Update all code referencing removed types/fields<br>12. `cargo test` passes |

**Rationale**: The extension framework (`extensions.enabled` whitelist, `ExtensionManager`) is the actual mechanism for controlling tool/MCP/skill access. `AgentCapability` was declarative but never enforced. Having both created confusion and competing sources of truth.

---

#### PKG-M1: Create `src/portable/types.rs`

| Field | Value |
|-------|-------|
| **Task ID** | PKG-M1 |
| **File** | `src/portable/types.rs` (new) |
| **Description** | Move `ImageDigest`, `LayerType`, and related types from `src/image/` |
| **Acceptance Criteria** | 1. `ImageDigest` with SHA-256 validation<br>2. `LayerType` enum (Config, Identity, Skills, Workspace, Sessions, Mcp)<br>3. `LayerDigest` struct (type + digest string)<br>4. All types have `Serialize`/`Deserialize`<br>5. Unit tests for digest validation |

**Types to move**:

```rust
pub struct ImageDigest { ... }
pub enum LayerType { Config, Identity, Skills, Workspace, Sessions, Mcp }
pub struct LayerDigest {
    pub layer_type: LayerType,
    pub digest: String, // sha256:...
}
```

**Note**: Layer types simplified to match `.agent` package structure. Removed `Markdown`, `Tools`, `Projects`, `Memories` (not used in `.agent` format). Added `Identity` (was missing). `Config` layer contains `agent.toml` (the SSOT).

---

#### PKG-M2: Add `layers` Section to `AgentManifest`

| Field | Value |
|-------|-------|
| **Task ID** | PKG-M2 |
| **File** | `src/portable/manifest.rs` |
| **Description** | Add `layers` field to cleaned `AgentManifest` for content-addressable layer digests |
| **Acceptance Criteria** | 1. `AgentManifest` has `layers: Option<AgentLayers>`<br>2. `AgentLayers` struct has fields: `config`, `identity`, `skills`, `workspace`, `sessions`, `mcp` (all `Option<String>` = digest)<br>3. `to_toml()` serializes layers section<br>4. `from_toml()` deserializes layers section<br>5. Tests updated for clean manifest |

**Schema addition**:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentLayers {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,     // ← contains agent.toml (SSOT)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sessions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp: Option<String>,
}

// Add to AgentManifest:
pub struct AgentManifest {
    pub agent: AgentMetadata,
    pub identity: IdentityConfig,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layers: Option<AgentLayers>,
    pub packaging: PackagingMetadata,
    pub signatures: Signatures,
}
```

---

#### PKG-M3: Add `build_from_directory()` to `Packager`

| Field | Value |
|-------|-------|
| **Task ID** | PKG-M3 |
| **File** | `src/portable/packager.rs` |
| **Description** | Merge `ImageBuilder::build()` logic into `Packager`. Add `build_from_directory()` method that creates `.agent` from a directory with layer digests. |
| **Acceptance Criteria** | 1. `Packager::build_from_directory(path, tag)` creates `.agent` from directory<br>2. Computes layer digests for each category (config, identity, skills, workspace, etc.)<br>3. Stores layers in local registry store (via `AgentRegistry`)<br>4. Sets `layers` section in manifest<br>5. Returns `.agent` file path<br>6. Progress callback supported |

**Method signature**:

```rust
impl Packager {
    /// Build a .agent package from a directory (merged from ImageBuilder)
    pub async fn build_from_directory<F>(
        source_path: &Path,
        tag: &str,
        registry: &AgentRegistry,
        mut progress: F,
    ) -> anyhow::Result<PathBuf>
    where
        F: FnMut(BuildProgress),
    {
        // 1. Read config/agent.toml to get agent name/version
        // 2. Create manifest (clean — no capabilities/tools/mcp)
        // 3. For each layer category:
        //    a. Collect files
        //    b. Compute digest
        //    c. Store in AgentRegistry
        //    d. Set manifest.layers.{category}
        // 4. Create .agent archive
        // 5. Store manifest in AgentRegistry
        // 6. Return path
    }
}
```

---

#### PKG-M4: Create `AgentRegistry` (Local Store)

| Field | Value |
|-------|-------|
| **Task ID** | PKG-M4 |
| **File** | `src/portable/registry.rs` (new) |
| **Description** | Create local content-addressable store for `.agent` layers and manifests. Merged from `ImageRegistry`. |
| **Acceptance Criteria** | 1. `store_layer(digest, bytes)` → writes if not exists<br>2. `get_layer(digest)` → reads layer bytes<br>3. `store_manifest(manifest)` → writes manifest, updates tag<br>4. `get_manifest_by_tag(tag)` → resolves tag to manifest<br>5. `get_manifest_by_digest(digest)` → reads manifest directly<br>6. `list_tags()` → returns all tags<br>7. Directory layout matches spec §6.4 |

**Storage layout**:

```
~/.pekobot/registry/
├── layers/
│   └── sha256-abc123.../
│       └── layer.tar.gz
├── manifests/
│   └── sha256-xyz789.../
│       └── manifest.toml
└── tags/
    └── my-agent_v1.0       # file contains digest string
```

---

#### PKG-M5: Update `src/portable/mod.rs` Re-exports

| Field | Value |
|-------|-------|
| **Task ID** | PKG-M5 |
| **File** | `src/portable/mod.rs` |
| **Description** | Add re-exports for new types and `AgentRegistry` |
| **Acceptance Criteria** | 1. Re-exports `ImageDigest`, `LayerType`, `LayerDigest` from `types`<br>2. Re-exports `AgentRegistry` from `registry`<br>3. Re-exports `AgentLayers` from `manifest` |

---

#### PKG-M6: Migrate All `crate::image` Imports

| Field | Value |
|-------|-------|
| **Task ID** | PKG-M6 |
| **File** | Various (`src/registry/client.rs`, `src/daemon/state.rs`, etc.) |
| **Description** | Find all files importing from `crate::image` and update to `crate::portable` |
| **Acceptance Criteria** | 1. `grep -r "crate::image" src/` returns zero matches<br>2. `cargo build` succeeds<br>3. `cargo test` passes |

**Files to update**:
- `src/registry/client.rs` — imports `ImageDigest`, `ImageManifest`, `Layer`
- `src/daemon/state.rs` — imports `RegistryConfig` (from `image::registry`)
- `src/agent/stateless_manager.rs` — TODO comment references `ImageRegistry`

---

#### PKG-M7: Delete `src/image/` Directory

| Field | Value |
|-------|-------|
| **Task ID** | PKG-M7 |
| **File** | `src/image/` (delete entire directory) |
| **Description** | Remove `src/image/mod.rs`, `manifest.rs`, `builder.rs`, `registry.rs`, `config.rs` |
| **Acceptance Criteria** | 1. `src/image/` directory does not exist<br>2. `cargo build` succeeds<br>3. `cargo test` passes<br>4. No references to `image::` in codebase |

---

#### PKG-M8: Delete Obsolete Command Files

| Field | Value |
|-------|-------|
| **Task ID** | PKG-M8 |
| **File** | `src/commands/build.rs`, `src/commands/push.rs`, `src/commands/pull.rs` |
| **Description** | Delete files for top-level commands moved under `agent` |
| **Acceptance Criteria** | 1. Files deleted<br>2. `cargo build` succeeds |

---

### Phase 2 Exit Criteria

- [x] `cargo test portable` passes (including new layer/build tests) — *unit tests in `types.rs` and `registry.rs` pass*
- [x] `cargo test` has no `image::` module references
- [x] `src/image/` directory deleted
- [x] `AgentRegistry` stores/retrieves layers and manifests
- [x] `AgentManifest` has no `capabilities`, `tools`, `mcp`, `tool_sources`, `memory` fields
- [x] `AgentManifest` has `layers` section with `config`, `identity`, `skills`, `workspace`, `sessions`, `mcp`
- [x] `AgentCapability`, `TeamCapabilityConfig`, `CapabilitiesConfig` removed from codebase — *already removed before this phase*
- [x] `CapabilityIndex` renamed to `ExtensionIndex` — *already done before this phase*
- [x] No references to `capabilities` in agent config or context
- [x] `cargo clippy` clean in `src/portable/`
- [ ] `Packager::build_from_directory()` produces `.agent` with layer digests — **moved to Phase 4**

---

## Phase 3: Registry Integration

**Goal**: Working `pekobot agent push` and `pull` against the mock registry.

**Prerequisite**: Phase 2 complete.

### Tasks

#### PKG-R1: Adapt `RegistryClient` for `.agent` Layer Digests

| Field | Value |
|-------|-------|
| **Task ID** | PKG-R1 |
| **File** | `src/registry/client.rs` |
| **Description** | Update `RegistryClient` to operate on `.agent` manifests (TOML) and layer digests |
| **Acceptance Criteria** | 1. `push()` takes manifest digest and tag, pushes manifest TOML + layers<br>2. `pull()` fetches manifest TOML, then fetches each layer by digest<br>3. `check_existing_layers()` uses HEAD request<br>4. Progress events report layer-level progress |

---

#### PKG-R2: Implement Layer Existence Check

| Field | Value |
|-------|-------|
| **Task ID** | PKG-R2 |
| **File** | `src/registry/client.rs` |
| **Description** | Replace stub `check_existing_layers()` with real HEAD request |
| **Acceptance Criteria** | 1. Sends HEAD to `/v2/{name}/blobs/{digest}`<br>2. Returns HashSet of existing digests (200 responses)<br>3. Unit test with mock server |

---

#### PKG-R3: Implement `pekobot agent push`

| Field | Value |
|-------|-------|
| **Task ID** | PKG-R3 |
| **File** | `src/commands/agent.rs` |
| **Description** | Wire `Push` subcommand to `RegistryClient::push()` |
| **Acceptance Criteria** | 1. Resolves local tag to manifest via `AgentRegistry`<br>2. Calls `RegistryClient::push()` with progress<br>3. Skips existing layers<br>4. Prints progress (human or JSON) |

---

#### PKG-R4: Implement `pekobot agent pull`

| Field | Value |
|-------|-------|
| **Task ID** | PKG-R4 |
| **File** | `src/commands/agent.rs` |
| **Description** | Wire `Pull` subcommand to `RegistryClient::pull()` |
| **Acceptance Criteria** | 1. Parses registry ref<br>2. Calls `RegistryClient::pull()` with progress<br>3. Stores manifest and layers in `AgentRegistry`<br>4. Creates local tag reference<br>5. Prints progress (human or JSON) |

---

#### PKG-R5: Registry Integration Tests

| Field | Value |
|-------|-------|
| **Task ID** | PKG-R5 |
| **File** | `tests/registry_integration.rs` (new) |
| **Description** | End-to-end: build → push → pull with mock server |
| **Acceptance Criteria** | 1. Starts mock server<br>2. Builds `.agent` locally<br>3. Pushes to mock server<br>4. Pulls from mock server<br>5. Asserts manifest digests match<br>6. Asserts all layers present |

---

### Phase 3 Exit Criteria

- [x] `cargo test --test registry_integration -- --ignored` passes (4/4 tests)
- [x] `pekobot agent push` works against mock server
- [x] `pekobot agent pull` works against mock server
- [x] Layer existence check skips already-present layers
- [x] `cargo clippy --all-targets` has no errors in `src/registry/`

---

## Phase 4: Image Build CLI

**Goal**: `pekobot agent build <path> -t <tag>` works end-to-end.

**Prerequisite**: Phase 2 complete.

### Tasks

#### PKG-B1: Implement `pekobot agent build`

| Field | Value |
|-------|-------|
| **Task ID** | PKG-B1 |
| **File** | `src/commands/agent.rs` |
| **Description** | Wire `Build` subcommand to `Packager::build_from_directory()` |
| **Acceptance Criteria** | 1. Validates source path has `config/agent.toml`<br>2. Creates `AgentRegistry` at default path<br>3. Calls `Packager::build_from_directory()` with progress<br>4. Prints manifest digest, layer count, total size<br>5. Stores `.agent` in local registry |

**Progress output** (human):

```
Reading ./my-agent...
Layering config...        sha256:abc123... (1.2 KB)
Layering identity...      sha256:def456... (1.5 KB)
Layering skills...        sha256:ghi789... (45 KB)
Layering workspace...     sha256:jkl012... (12 KB)
Build complete.
  Package: my-agent.agent
  Tag: my-agent:v1.0
  Digest: sha256:xyz789...
  Layers: 4
  Total size: 58.2 KB
```

---

#### PKG-B2: Build Command Tests

| Field | Value |
|-------|-------|
| **Task ID** | PKG-B2 |
| **File** | `tests/build_integration.rs` (new) or `src/portable/packager.rs` (tests) |
| **Description** | Test build command with temp agent directory |
| **Acceptance Criteria** | 1. Creates temp agent dir with `config/agent.toml`<br>2. Runs `Packager::build_from_directory()`<br>3. Verifies `.agent` file exists<br>4. Verifies manifest has `layers` section<br>5. Verifies manifest has NO `capabilities`, `tools`, `mcp` fields<br>6. Verifies layers stored in `AgentRegistry` |

---

### Phase 4 Exit Criteria

- [x] `pekobot agent build ./test-agent -t test:v1.0` produces valid `.agent`
- [x] Built `.agent` can be inspected with `pekobot agent inspect`
- [x] Built `.agent` can be imported with `pekobot agent import`
- [x] `cargo clippy` clean

---

## Phase 5: Team Packaging Hardening

**Goal**: `.team` exports/imports with checksum validation + `team.toml` preservation.

**Prerequisite**: Phase 2 complete. Can run in parallel with Phases 3–4.

### Tasks

#### PKG-T1: Add Checksums to `TeamManifest`

| Field | Value |
|-------|-------|
| **Task ID** | PKG-T1 |
| **File** | `src/portable/team_packager.rs` |
| **Description** | Add `packaging` section to `TeamManifest` with file list and SHA-256 checksums |
| **Acceptance Criteria** | 1. `TeamManifest` has `packaging: PackagingMetadata` field<br>2. `TeamPackager` computes checksum for every file in archive<br>3. Checksums stored as `HashMap<String, String>` |

---

#### PKG-T2: Include `team.toml` in `.team` Export

| Field | Value |
|-------|-------|
| **Task ID** | PKG-T2 |
| **File** | `src/portable/team_packager.rs` |
| **Description** | If `team.toml` exists in team directory, include as `team/team.toml` |
| **Acceptance Criteria** | 1. Checks for `team.toml` in team dir<br>2. Adds to archive if present<br>3. Includes in checksum computation<br>4. Does not fail if missing |

---

#### PKG-T3: Validate Checksums on Import

| Field | Value |
|-------|-------|
| **Task ID** | PKG-T3 |
| **File** | `src/portable/team_unpackager.rs` |
| **Description** | Verify all file checksums before importing agents |
| **Acceptance Criteria** | 1. If `packaging` present, verify every checksum<br>2. Fail fast on mismatch with clear error<br>3. Warn (but continue) if no `packaging` section (legacy) |

---

#### PKG-T4: Restore `team.toml` on Import

| Field | Value |
|-------|-------|
| **Task ID** | PKG-T4 |
| **File** | `src/portable/team_unpackager.rs` |
| **Description** | If `team/team.toml` present in package, restore to team directory |
| **Acceptance Criteria** | 1. Checks for `team/team.toml`<br>2. Writes to team dir if present<br>3. Does not fail if absent |

---

#### PKG-T5: Team Packaging Integration Tests

| Field | Value |
|-------|-------|
| **Task ID** | PKG-T5 |
| **File** | `tests/team_integration.rs` (new) |
| **Description** | End-to-end: export → verify checksums → import → verify data |
| **Acceptance Criteria** | 1. Creates temp team with 2 agents<br>2. Exports to `.team`<br>3. Verifies manifest has checksums<br>4. Imports to new location<br>5. Verifies all agent files present<br>6. Verifies `team.toml` restored |

---

### Phase 5 Exit Criteria

- [x] `cargo test portable` passes (including team tests)
- [x] Exported `.team` includes `packaging.checksums`
- [x] Team import fails on checksum mismatch
- [x] Team import warns (but continues) without checksums
- [x] `team.toml` roundtrips through export/import
- [x] `cargo clippy` clean in `src/portable/`

---

## Phase 6: Extension Packaging

**Goal**: `pekobot ext export <id> -o <file.ext>` creates installable `.ext` packages.

**Prerequisite**: Phase 1 complete. Can run in parallel with Phases 2–5.

### Tasks

#### PKG-E1: Implement `ExtensionPackager`

| Field | Value |
|-------|-------|
| **Task ID** | PKG-E1 |
| **File** | `src/extension/manager/packaging.rs` (new) |
| **Description** | Export installed extension to `.ext` tar.gz |
| **Acceptance Criteria** | 1. Takes `ExtensionId` and output path<br>2. Looks up extension in `ExtensionManager`<br>3. Reads all files from extension path<br>4. Creates tar.gz with `manifest.toml` + `extension/` dir<br>5. Computes SHA-256 checksums<br>6. Returns output path |

**Output format**:

```
docker-skill.ext (gzip-compressed tar)
├── manifest.toml           # Extension package metadata
└── extension/
    ├── manifest.yaml
    ├── SKILL.md
    └── ...
```

---

#### PKG-E2: Wire `pekobot ext export`

| Field | Value |
|-------|-------|
| **Task ID** | PKG-E2 |
| **File** | `src/commands/ext.rs` |
| **Description** | Connect `ExtCommands::Export` to `ExtensionPackager` |
| **Acceptance Criteria** | 1. Loads `ExtensionManager`<br>2. Calls `ExtensionPackager::export()`<br>3. Prints output path on success<br>4. Clear error if extension not found |

---

#### PKG-E3: Extension Export Integration Test

| Field | Value |
|-------|-------|
| **Task ID** | PKG-E3 |
| **File** | `tests/extension_packaging.rs` (new) |
| **Description** | End-to-end: install → export → install from `.ext` |
| **Acceptance Criteria** | 1. Creates temp extension<br>2. Installs via `ExtensionManager`<br>3. Exports to `.ext`<br>4. Verifies structure and checksums<br>5. Installs from `.ext` to new location |

---

### Phase 6 Exit Criteria

- [ ] `pekobot ext export docker-skill -o docker-skill.ext` creates valid `.ext`
- [ ] `.ext` installs via `pekobot ext install ./docker-skill.ext`
- [ ] `cargo test extension` passes
- [ ] `cargo clippy` clean

---

## Phase 7: Final Integration

**Goal**: All tests pass, docs updated, clippy clean.

**Prerequisite**: All previous phases complete.

### Tasks

#### PKG-I1: Full Integration Test

| Field | Value |
|-------|-------|
| **Task ID** | PKG-I1 |
| **File** | `tests/packaging_integration.rs` (new) |
| **Description** | Combined test: build → push → pull → import → export team |
| **Acceptance Criteria** | 1. Builds `.agent`<br>2. Pushes to mock registry<br>3. Pulls from mock registry<br>4. Imports `.agent`<br>5. Creates team with imported agent<br>6. Exports team to `.team`<br>7. Imports team to new location |

---

#### PKG-I2: Update `DATA_MODEL.md`

| Field | Value |
|-------|-------|
| **Task ID** | PKG-I2 |
| **File** | `DATA_MODEL.md` |
| **Description** | Document unified `.agent` format changes and clean manifest |
| **Acceptance Criteria** | 1. Agent manifest schema includes `layers` section<br>2. Agent manifest schema explicitly excludes `capabilities`, `tools`, `mcp`<br>3. Local registry store layout documented<br>4. Team manifest schema includes `packaging` section<br>5. Extension package format documented<br>6. Document that `agent.toml` is the single source of truth |

---

#### PKG-I3: Update `AGENTS.md`

| Field | Value |
|-------|-------|
| **Task ID** | PKG-I3 |
| **File** | `AGENTS.md` |
| **Description** | Update architecture overview to reflect merged modules and clean manifest |
| **Acceptance Criteria** | 1. `src/image/` removed from architecture diagram<br>2. `src/portable/` described as unified packaging layer<br>3. CLI commands updated<br>4. Clean manifest principle documented (packaging metadata only, agent.toml is SSOT) |

---

#### PKG-I4: Clippy Cleanup

| Field | Value |
|-------|-------|
| **Task ID** | PKG-I4 |
| **Files** | All modified/created files |
| **Description** | Run clippy on all new code, fix all warnings |
| **Acceptance Criteria** | `cargo clippy --all-targets` has zero warnings in packaging-related modules |

---

#### PKG-I5: Coverage Check

| Field | Value |
|-------|-------|
| **Task ID** | PKG-I5 |
| **Description** | Verify test coverage for packaging modules |
| **Acceptance Criteria** | `cargo test` coverage ≥ 70% for: `src/portable/`, `src/registry/`, `src/extension/manager/` |

---

### Phase 7 Exit Criteria

- [ ] All integration tests pass
- [ ] `DATA_MODEL.md` updated
- [ ] `AGENTS.md` updated
- [ ] `cargo clippy --all-targets` clean
- [ ] Coverage ≥ 70% for packaging modules
- [ ] Spec updated if implementation deviated from plan

---

## Parallelization Guide

After Phase 1, work can split across three tracks:

### Track A: Clean Manifest + Merge + Registry (Developer A)

```
Phase 1 (Foundation)
  ↓
Phase 2 (Clean manifest + Merge image into portable)  ← critical path
  ↓
Phase 3 (Registry integration)
  ↓
Phase 4 (Build CLI)
  ↓
Phase 7 (Final Integration — merge all tracks)
```

**Files**: `src/portable/manifest.rs`, `src/portable/types.rs`, `src/portable/registry.rs`, `src/portable/packager.rs`, `src/registry/client.rs`, `src/commands/agent.rs`

### Track B: Team + Extension (Developer B)

```
Phase 1 (Foundation)
  ↓
Phase 5 (Team packaging)             ← can start after Phase 1
  ↓
Phase 6 (Extension packaging)        ← can start after Phase 1
  ↓
Phase 7 (Final Integration — merge all tracks)
```

**Files**: `src/portable/team_packager.rs`, `src/portable/team_unpackager.rs`, `src/extension/manager/packaging.rs`, `src/commands/ext.rs`

### Track C: Testing + Docs (Either developer, or shared)

```
Phase 1 (Foundation)
  ↓
[Continuous] Write tests for completed features
  ↓
Phase 7 (Final Integration + docs)
```

### Conflict Zones

| File | Risk | Resolution |
|------|------|------------|
| `src/commands/mod.rs` | Both tracks modify | Phase 1 settles the enum structure; later phases only add subcommands |
| `src/portable/mod.rs` | Track A modifies | Coordinate re-exports in Phase 2 |
| `Cargo.toml` | May add dependencies | Add all deps in Phase 1 |
| `tests/` | Both tracks add files | Use descriptive filenames (`registry_`, `team_`, `extension_` prefixes) |

---

## Review Checklist (Per Phase)

Before marking a phase complete:

- [ ] All tasks implemented + tested
- [ ] `cargo test` passes for modified modules
- [ ] `cargo clippy` clean for new/modified files
- [ ] No `unwrap()` or `expect()` in production code (tests OK)
- [ ] Error messages are user-friendly
- [ ] New files have module-level doc comments
- [ ] Public APIs have rustdoc comments
- [ ] No dead code (or marked with `#[allow(dead_code)]` + explanation)
- [ ] Integration tests cover happy path + at least one error path
- [ ] Clean manifest verified: `AgentManifest` has no `capabilities`, `tools`, `mcp`, `tool_sources`

---

## Tracking Template

Use this to track progress. Copy into GitHub issues or project board.

```markdown
### Phase X: [Name]

| Task | Status | Owner | PR | Notes |
|------|--------|-------|-----|-------|
| PKG-XX | ⬜ | | | |
| PKG-XX | ⬜ | | | |

**Exit Criteria**:
- [ ] 
- [ ] 
- [ ] 
```

---

*End of Implementation Plan v2.1*
