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
| **Team Package** | `.team` | Multi-agent team export/import | End users sharing teams |
| **Image Manifest** | `manifest.json` | Content-addressable build artifact | Build system, registry |
| **Extension Package** | `.ext` or URL ref | Extension distribution (source, binary, or remote) | Extension developers, users |

The goal is to unify the existing dual systems (`src/portable/` for `.agent`/`.team` and `src/image/` for content-addressable images) into a single coherent packaging layer that supports validation and registry interoperability. **Security (signing/encryption) is explicitly deferred** — checksum validation is sufficient for Phase 1.

---

## 2. Terminology

| Term | Definition |
|------|-----------|
| **Package** | A `.agent`, `.team`, or `.ext` file — a gzip-compressed tar archive for portability |
| **Image** | A content-addressable build artifact described by an `ImageManifest` with SHA-256 digested layers |
| **Layer** | A gzip-compressed tar archive containing a specific category of files (config, markdown, tools, etc.) |
| **Manifest** | Metadata describing a package or image (TOML for packages, JSON for images) |
| **Registry** | A remote or local store for images with push/pull capability |
| **Base Image** | An image that another image inherits from via `base = "ref"` in `config.toml` |
| **DID** | Decentralized Identifier — self-sovereign identity for agents |
| **Extension Source** | A reference to an extension that can be resolved at install time (URL, git repo, registry) |
| **Extension Bundle** | A packaged extension with all files included for offline distribution |

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

### 3.2 Team Packaging (`src/portable/`)

| Component | Status | Notes |
|-----------|--------|-------|
| `TeamManifest` (TOML) | ✅ Implemented | Basic team info, format version, export metadata |
| `TeamPackager` | ✅ Implemented | Exports multiple agents into `.team` tar.gz |
| `TeamUnpackager` | ✅ Implemented | Imports `.team`, delegates to `Unpackager` per agent |
| Team-level signing | ❌ Missing | No team-level signature; relies on per-agent signatures |
| Team-level validation | ❌ Missing | No checksum validation at team level |

### 3.3 Image Build System (`src/image/`)

| Component | Status | Notes |
|-----------|--------|-------|
| `ImageManifest` (JSON) | ✅ Implemented | Schema v1 with layers, base info, capabilities |
| `ImageBuilder` | ✅ Implemented | Builds images from directories, creates content-addressable layers |
| `ImageRegistry` (local) | ✅ Implemented | Content-addressable storage, tag resolution, layer dedup, GC |
| `LayerType` enum | ✅ Implemented | Config, Markdown, Tools, Projects, Memories, Skills, McpConfig |
| `ImageRef` parsing | ✅ Implemented | Registry ref, local tag, digest, path |
| Base image inheritance | ⚠️ Declared | `base` field exists but `ImageBuilder::build()` does not resolve/merge parent layers |
| `BuildOptions` | ✅ Implemented | Tag, base image ref, registry path |
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
| Layer existence check | ⚠️ Stub | Returns empty set always — pushes all layers |

### 3.5 Extension System (`src/extension/`, `src/extensions/`)

| Component | Status | Notes |
|-----------|--------|-------|
| `ExtensionManifest` | ✅ Implemented | ID, type, name, version, path, metadata |
| `ExtensionManager` | ✅ Implemented | Discovery, install, uninstall, enable/disable, bundle |
| `ExtensionBundle` | ✅ Implemented | In-memory bundle of multiple extensions |
| `install_bundle()` | ✅ Implemented | Installs bundled extensions with conflict checking |
| Extension discovery | ✅ Implemented | Two-tier hierarchy (SKILL.md/server.json → manifest.yaml) |
| Extension packaging | ❌ Missing | No `.ext` package format; no export of extensions |
| Extension source refs | ❌ Missing | No URL/git/registry references for extensions |
| Extension registry client | ❌ Missing | No push/pull for extensions |

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

### 4.3 Extension Packaging: Two Modes

**Decision**: Support **two extension distribution modes**:

| Mode | Format | Use Case | Example |
|------|--------|----------|---------|
| **Bundle** | `.ext` tar.gz | Offline distribution, air-gapped environments | `docker-skill.ext` |
| **Source** | URL reference | Online installation, always up-to-date | `github:owner/repo`, `https://...`, `mcp:server-url` |

**Rationale**:
- Different users have different needs — some want everything bundled, some want lightweight references
- MCP servers are inherently remote (URL-based); bundling binaries is fragile across platforms
- GitHub is the de facto extension distribution channel today
- A unified "source reference" format lets us add new sources without changing the install flow

**Extension source reference format**:

```toml
# In agent config or team config
[extensions]
# Mode 1: Bundled/local path
bundled = [
    "./extensions/docker-skill",      # local directory
    "./extensions/calculator.ext",    # packaged .ext file
]

# Mode 2: Source references (resolved at install time)
sources = [
    { type = "github", url = "github:pekobot-extensions/docker-skill", version = "v1.2.0" },
    { type = "url", url = "https://example.com/extensions/calculator.tar.gz" },
    { type = "mcp", url = "https://api.example.com/mcp/sse", transport = "sse" },
    { type = "registry", name = "docker-skill", version = "1.2.0", source = "default" },
]
```

**`.ext` package format**:

```
docker-skill.ext (gzip-compressed tar)
├── manifest.toml           # Extension metadata
├── extension/
│   ├── manifest.yaml       # Extension manifest (per extension type)
│   ├── SKILL.md            # or server.json, or other type-specific files
│   └── ...                 # Additional files
└── packaging.toml          # Packaging metadata (checksums, format version)
```

---

## 5. Gap Analysis

### 5.1 P0 Gaps (Must Close for v1.0)

| ID | Gap | Impact | Owner Module |
|----|-----|--------|-------------|
| GAP-001 | No `pekobot build` CLI command | Cannot build images from CLI | `src/commands/` |
| GAP-002 | No `pekobot run` CLI command | Cannot run images from CLI | `src/commands/` |
| GAP-003 | No `pekobot pull` CLI command | Registry client not exposed | `src/commands/` |
| GAP-004 | No `pekobot push` CLI command | Registry client not exposed | `src/commands/` |
| GAP-005 | Base image inheritance declared but not resolved at build | `FROM` semantics don't work | `src/image/` |
| GAP-006 | No mock registry server for testing | Cannot test registry client | `src/registry/` |
| GAP-007 | No extension `.ext` package format | Cannot distribute extensions offline | `src/extension/` |
| GAP-008 | No extension source reference format | Cannot install extensions from URLs | `src/extension/` |
| GAP-009 | No `pekobot ext export` command | Cannot package extensions | `src/commands/` |
| GAP-010 | Team packages have no checksums | Cannot verify team package integrity | `src/portable/` |

### 5.2 P1 Gaps (Should Have)

| ID | Gap | Impact |
|----|-----|--------|
| GAP-011 | No `pekobot validate <path>` command | Cannot validate without building/importing |
| GAP-012 | Build does not print layer sizes, total size, compression ratio | Poor UX |
| GAP-013 | Registry client layer existence check is stubbed | Pushes all layers every time |
| GAP-014 | No extension registry client | Cannot push/pull extensions to registry |

### 5.3 P2 Gaps (Nice to Have / Deferred)

| ID | Gap | Impact | Target |
|----|-----|--------|--------|
| GAP-015 | Signing and encryption | Security for shared packages | Phase 2 |
| GAP-016 | Multi-arch manifest support | Platform-specific binaries | Phase 2 |
| GAP-017 | Content deduplication across images | Storage inefficiency | Phase 2 |
| GAP-018 | `pekobot diff <image-a> <image-b>` | Hard to compare images | Phase 2 |

---

## 6. Unified Architecture

### 6.1 Module Layout (Phase 1 — Minimal Churn)

Keep existing module structure. Add new files only.

```
src/
├── portable/               # EXISTING — agent/team packaging
│   ├── mod.rs
│   ├── manifest.rs
│   ├── packager.rs         # MODIFIED: remove signing/encryption wiring
│   ├── unpackager.rs       # MODIFIED: remove signature verification
│   ├── team_packager.rs    # MODIFIED: add checksums
│   ├── team_unpackager.rs  # MODIFIED: add validation
│   ├── validation.rs       # MODIFIED: remove signature validation
│   └── crypto.rs           # KEEP: but don't wire into packager
│
├── image/                  # EXISTING — image build system
│   ├── mod.rs
│   ├── manifest.rs
│   ├── builder.rs          # MODIFIED: implement base image resolution
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
│   │   └── packaging.rs    # NEW: ExtensionPackager, ExtensionSourceResolver
│   └── types/
│       ├── mod.rs
│       └── source.rs       # NEW: ExtensionSourceRef, SourceType
│
└── commands/
    ├── mod.rs              # MODIFIED: add Build, Run, Pull, Push, Validate
    ├── build.rs            # NEW: pekobot build
    ├── run.rs              # NEW: pekobot run
    ├── pull.rs             # NEW: pekobot pull
    ├── push.rs             # NEW: pekobot push
    ├── validate.rs         # NEW: pekobot validate
    └── ext.rs              # MODIFIED: add Export subcommand
```

### 6.2 Package Format Specifications

#### 6.2.1 Agent Package (`.agent`) — Simplified for Phase 1

```
my-agent.agent (gzip-compressed tar)
├── manifest.toml           # Package metadata and file checksums
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

**`manifest.toml` schema (Phase 1 — no signatures):**

```toml
[agent]
name = "researcher"
version = "1.0.0"
description = "A research assistant agent"
created_at = "2026-05-07T10:00:00Z"
export_format = "1.1"
did = "did:pekobot:local:abc123..."
pekobot_version = "0.1.0"

[identity]
key_algorithm = "ed25519"
encrypted = false

[capabilities]
names = ["web_search", "read_file"]
versions = { web_search = "1.0.0", read_file = "2.1.0" }

[tools]
required = ["web_search", "read_file"]
optional = ["browser"]

[mcp]
[[mcp.server]]
name = "filesystem"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem"]
bundled = false

[tool_sources]
required = [
    { name = "universal-github", version = "1.2.0", source = "default" }
]

[packaging]
files = ["manifest.toml", "identity/did.json", ...]
checksums = { "manifest.toml" = "sha256:...", ... }
compression = "gzip"
archive_format = "tar"
```

#### 6.2.2 Team Package (`.team`) — With Checksums

```
my-team.team (gzip-compressed tar)
├── team/
│   └── manifest.toml       # Team metadata + file checksums
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

#### 6.2.4 Extension Source Reference

```rust
/// A reference to an extension that can be resolved at install time
pub struct ExtensionSourceRef {
    pub name: String,
    pub version: String,
    pub source_type: SourceType,
}

pub enum SourceType {
    /// GitHub repository: github:owner/repo[@ref]
    GitHub { owner: String, repo: String, ref_: Option<String> },
    /// Direct URL to a tarball/zip
    Url { url: String },
    /// MCP server endpoint
    Mcp { url: String, transport: McpTransport },
    /// Extension registry (future)
    Registry { registry: String, name: String },
    /// Local path (already resolved)
    Local { path: PathBuf },
}
```

---

## 7. Implementation Plan

### 7.1 Phase 1A: Agent/Team Packaging Hardening (P0)

| Task | Description | File(s) |
|------|-------------|---------|
| PKG-A1 | Remove signing/encryption from critical path; keep checksum validation only | `src/portable/packager.rs`, `src/portable/unpackager.rs` |
| PKG-A2 | Add file checksums to `TeamManifest` and `TeamPackager` | `src/portable/team_packager.rs` |
| PKG-A3 | Implement team-level checksum validation in `TeamUnpackager` | `src/portable/team_unpackager.rs` |
| PKG-A4 | Add `pekobot validate <path>` command | `src/commands/validate.rs` (NEW) |

### 7.2 Phase 1B: Image Build System (P0)

| Task | Description | File(s) |
|------|-------------|---------|
| PKG-B1 | Add `pekobot build <path> -t <name:tag>` CLI command | `src/commands/mod.rs`, `src/commands/build.rs` |
| PKG-B2 | Implement base image resolution in `ImageBuilder` | `src/image/builder.rs` |
| PKG-B3 | Implement base image layer merging | `src/image/builder.rs` |
| PKG-B4 | Add circular dependency detection for base images | `src/image/builder.rs` |
| PKG-B5 | Add `pekobot run <image-ref>` CLI command | `src/commands/run.rs` |

### 7.3 Phase 1C: Registry Integration (P0)

| Task | Description | File(s) |
|------|-------------|---------|
| PKG-C1 | Add `pekobot pull <registry-ref>` CLI command | `src/commands/pull.rs` |
| PKG-C2 | Add `pekobot push <registry-ref>` CLI command | `src/commands/push.rs` |
| PKG-C3 | Implement proper layer existence checking | `src/registry/client.rs` |
| PKG-C4 | Build mock registry server for testing | `src/registry/mock_server.rs` |
| PKG-C5 | Integration tests using mock server | `tests/registry_integration.rs` |

### 7.4 Phase 1D: Extension Packaging (P0)

| Task | Description | File(s) |
|------|-------------|---------|
| PKG-D1 | Define `ExtensionSourceRef` and `SourceType` types | `src/extension/types/source.rs` |
| PKG-D2 | Implement extension source resolver (GitHub, URL, MCP) | `src/extension/manager/packaging.rs` |
| PKG-D3 | Implement `.ext` packager | `src/extension/manager/packaging.rs` |
| PKG-D4 | Implement `.ext` unpackager/installer | `src/extension/manager/packaging.rs` |
| PKG-D5 | Add `pekobot ext export <id>` command | `src/commands/ext.rs` |
| PKG-D6 | Add source reference support to `pekobot ext install` | `src/commands/ext.rs` |

### 7.5 Phase 1E: Testing & Documentation (P0)

| Task | Description |
|------|-------------|
| PKG-E1 | Unit tests for base image inheritance |
| PKG-E2 | Integration test: build → export → import → run |
| PKG-E3 | Integration test: push → pull with mock registry |
| PKG-E4 | Integration test: extension source resolution |
| PKG-E5 | Update `DATA_MODEL.md` if any format changes |
| PKG-E6 | Update `API_SURFACE.md` with new public APIs |

---

## 8. CLI Command Reference

### 8.1 New Commands

```
pekobot build <path> -t <name:tag> [--base <ref>] [--json]
    Build an agent image from a directory.

pekobot run <image-ref> [--team <team>] [--name <instance-name>]
    Run an agent image (local path, registry ref, digest, or local tag).

pekobot pull <registry-ref>
    Pull an image from a registry.
    Example: pekobot pull pekohub.com/agents/researcher:v2.5

pekobot push <local-tag> <registry-ref>
    Push a local image to a registry.
    Example: pekobot push researcher:v2.5 pekohub.com/agents/researcher:v2.5

pekobot validate <path>
    Validate a directory or .agent/.team/.ext file against the spec.

pekobot ext export <id> -o <file.ext>
    Export an extension to a .ext package.

pekobot ext install <source>
    Install an extension from various sources:
      pekobot ext install ./local/path
      pekobot ext install ./package.ext
      pekobot ext install github:owner/repo
      pekobot ext install https://example.com/ext.tar.gz
      pekobot ext install mcp+https://api.example.com/mcp
```

### 8.2 Existing Commands (Already Implemented)

```
pekobot agent export <name> -o <file.agent>
pekobot agent import <file.agent>
pekobot agent inspect <file.agent>
pekobot team export <name> -o <file.team>
pekobot team import <file.team>
pekobot ext install <path>        # Currently only local path
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

## 10. Extension Source Resolution Flow

```
User runs: pekobot ext install github:pekobot-extensions/docker-skill@v1.2.0

1. Parse source reference → SourceType::GitHub { owner, repo, ref }
2. Resolve to download URL → https://github.com/pekobot-extensions/docker-skill/archive/refs/tags/v1.2.0.tar.gz
3. Download tarball to temp directory
4. Extract and detect extension type (SKILL.md → skill)
5. Delegate to ExtensionManager::install()
6. Clean up temp files

User runs: pekobot ext install mcp+https://api.example.com/mcp

1. Parse source reference → SourceType::Mcp { url, transport: Sse }
2. Validate URL accessibility (HEAD request)
3. Create MCP server config entry
4. No local files needed — runtime connects on demand
```

---

## 11. Success Criteria

Phase 1 packaging is complete when:

1. ✅ All P0 gaps (GAP-001 through GAP-010) are closed
2. ✅ `cargo test` passes with ≥ 70% coverage for packaging modules
3. ✅ `cargo clippy` passes with zero warnings in packaging code
4. ✅ All new CLI commands (`build`, `run`, `pull`, `push`, `validate`, `ext export`) are functional
5. ✅ Agent packages pass checksum validation on import
6. ✅ Team packages pass checksum validation on import
7. ✅ Base image inheritance works end-to-end
8. ✅ Registry push/pull works against the mock server
9. ✅ Extensions can be installed from GitHub, URL, MCP, and local `.ext` packages
10. ✅ `.ext` packages can be created and installed

---

## Appendix A: File Checklist

### Files to Modify

| File | Changes |
|------|---------|
| `src/commands/mod.rs` | Add `Build`, `Run`, `Pull`, `Push`, `Validate` to `Commands` enum |
| `src/commands/ext.rs` | Add `Export` subcommand, enhance `Install` with source refs |
| `src/portable/team_packager.rs` | Add file checksums to team manifest |
| `src/portable/team_unpackager.rs` | Add checksum validation |
| `src/image/builder.rs` | Implement base image resolution and merging |
| `src/registry/client.rs` | Implement `check_existing_layers()` |

### Files to Create

| File | Purpose |
|------|---------|
| `src/commands/build.rs` | `pekobot build` command handler |
| `src/commands/run.rs` | `pekobot run` command handler |
| `src/commands/pull.rs` | `pekobot pull` command handler |
| `src/commands/push.rs` | `pekobot push` command handler |
| `src/commands/validate.rs` | `pekobot validate` command handler |
| `src/registry/mock_server.rs` | Mock registry server for testing |
| `src/extension/types/source.rs` | `ExtensionSourceRef`, `SourceType` |
| `src/extension/manager/packaging.rs` | Extension packager, source resolver |

---

*End of Packaging Specification v1.0*
