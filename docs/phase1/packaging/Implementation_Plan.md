# Pekobot Packaging — Implementation Plan

> **Version**: 1.0-draft  
> **Status**: Ready for Development  
> **Source Spec**: [`Packaging_Spec.md`](./Packaging_Spec.md)  
> **Last Updated**: 2026-05-08

---

## How to Use This Document

This plan breaks the spec into **6 sequential phases**. Each phase has:
- A clear deliverable
- Task-level detail with file paths
- Exit criteria (what "done" means)
- Estimated effort

**For developers**: Pick a phase, implement all tasks, verify exit criteria, then move to the next.

**For reviewers**: Review per phase. Each phase should be a self-contained PR.

---

## Development Principles

1. **Mock server first** — All registry/client work needs the mock server
2. **CLI scaffolding before logic** — Parse args, print help, then fill handlers
3. **One module at a time** — Finish tests for a module before moving on
4. **Integration tests as you go** — Don't defer all testing to the end
5. **Minimal churn** — Prefer new files over modifying existing ones

---

## Phase Overview

| Phase | Focus | Duration | Deliverable |
|-------|-------|----------|-------------|
| [Phase 1](#phase-1-foundation) | Mock registry + CLI scaffolding | 2–3 days | `pekobot build/pull/push/ext export` parse args; mock server runs |
| [Phase 2](#phase-2-registry-integration) | Registry push/pull with mock | 3–4 days | `pekobot pull` and `push` work end-to-end |
| [Phase 3](#phase-3-image-build-cli) | `pekobot build` command | 1–2 days | Build images from CLI with progress output |
| [Phase 4](#phase-4-team-packaging-hardening) | Team checksums + `team.toml` | 2–3 days | `.team` exports/imports with integrity checks |
| [Phase 5](#phase-5-extension-packaging) | `.ext` export | 2–3 days | `pekobot ext export` creates installable `.ext` files |
| [Phase 6](#phase-6-final-integration) | Integration tests + docs | 2–3 days | All tests pass, docs updated, clippy clean |

**Total estimated effort**: 12–18 developer-days (2–3 weeks for 1 developer, 1 week for 2 developers in parallel)

---

## Phase 1: Foundation

**Goal**: Mock registry server + CLI command scaffolding. Unblocks all subsequent phases.

### Tasks

#### PKG-F1: Mock Registry Server

| Field | Value |
|-------|-------|
| **Task ID** | PKG-F1 |
| **File** | `src/registry/mock_server.rs` (new) |
| **Description** | Build an in-memory mock registry server implementing the Pekobot registry protocol |
| **Acceptance Criteria** | 1. Server starts on random port (`port: 0`)<br>2. Implements all endpoints from §9.3 of spec<br>3. Supports manifest push/pull, layer push/pull, tag resolution<br>4. Supports HEAD for layer existence checks<br>5. Optional basic auth for testing auth flows |

**Endpoints to implement**:

```
GET    /v2/                          → 200 (capability check)
GET    /v2/{name}/manifests/{ref}    → manifest JSON
PUT    /v2/{name}/manifests/{ref}    → store manifest
HEAD   /v2/{name}/blobs/{digest}     → 200 if exists, 404 if not
GET    /v2/{name}/blobs/{digest}     → layer bytes
POST   /v2/{name}/blobs/uploads/     → 202 + upload URL
PUT    /v2/{name}/blobs/uploads/{uuid} → complete upload
```

**Storage**: Use `Arc<RwLock<HashMap<String, Vec<u8>>>>` for in-memory blobs and manifests.

**Test fixture**:

```rust
#[tokio::test]
async fn test_mock_registry_manifest_roundtrip() {
    let server = MockRegistryServer::new(0).await;
    let handle = server.start().await;
    
    let client = reqwest::Client::new();
    let url = format!("{}/v2/test/manifests/latest", server.base_url());
    
    // Push
    let manifest = r#"{"schema_version":1,"name":"test","layers":[]}"#;
    client.put(&url).body(manifest).send().await.unwrap();
    
    // Pull
    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.text().await.unwrap().contains("test"));
    
    handle.shutdown().await;
}
```

---

#### PKG-F2: CLI Scaffolding — `pekobot build`

| Field | Value |
|-------|-------|
| **Task ID** | PKG-F2 |
| **Files** | `src/commands/mod.rs`, `src/commands/build.rs` (new) |
| **Description** | Add `Build` variant to `Commands` enum and stub handler |
| **Acceptance Criteria** | 1. `pekobot build --help` prints usage<br>2. `pekobot build ./my-agent -t my-agent:v1.0` parses args<br>3. Handler prints "not yet implemented" and exits 0 |

**Args to support**:

```rust
#[derive(Args)]
pub struct BuildArgs {
    /// Path to agent directory
    path: PathBuf,
    /// Tag (name:tag format)
    #[arg(short, long)]
    tag: String,
    /// Base image reference (accepted but ignored in Phase 1)
    #[arg(long)]
    base: Option<String>,
    /// Output as JSON
    #[arg(long)]
    json: bool,
}
```

---

#### PKG-F3: CLI Scaffolding — `pekobot pull`

| Field | Value |
|-------|-------|
| **Task ID** | PKG-F3 |
| **Files** | `src/commands/mod.rs`, `src/commands/pull.rs` (new) |
| **Description** | Add `Pull` variant to `Commands` enum and stub handler |
| **Acceptance Criteria** | 1. `pekobot pull --help` prints usage<br>2. `pekobot pull pekohub.com/agents/researcher:v2.5` parses registry ref<br>3. Handler prints "not yet implemented" and exits 0 |

---

#### PKG-F4: CLI Scaffolding — `pekobot push`

| Field | Value |
|-------|-------|
| **Task ID** | PKG-F4 |
| **Files** | `src/commands/mod.rs`, `src/commands/push.rs` (new) |
| **Description** | Add `Push` variant to `Commands` enum and stub handler |
| **Acceptance Criteria** | 1. `pekobot push --help` prints usage<br>2. `pekobot push researcher:v2.5 pekohub.com/agents/researcher:v2.5` parses both refs<br>3. Handler prints "not yet implemented" and exits 0 |

---

#### PKG-F5: CLI Scaffolding — `pekobot ext export`

| Field | Value |
|-------|-------|
| **Task ID** | PKG-F5 |
| **Files** | `src/commands/ext.rs` |
| **Description** | Add `Export` variant to `ExtCommands` enum and stub handler |
| **Acceptance Criteria** | 1. `pekobot ext export --help` prints usage<br>2. `pekobot ext export docker-skill -o docker-skill.ext` parses args<br>3. Handler prints "not yet implemented" and exits 0 |

---

### Phase 1 Exit Criteria

- [ ] `cargo build` succeeds with zero errors
- [ ] `cargo test registry::mock_server` passes (mock server tests)
- [ ] `pekobot build --help`, `pekobot pull --help`, `pekobot push --help`, `pekobot ext export --help` all print correct usage
- [ ] `cargo clippy` has zero warnings in new code

---

## Phase 2: Registry Integration

**Goal**: Working `pekobot pull` and `push` against the mock registry.

**Prerequisite**: Phase 1 complete.

### Tasks

#### PKG-R1: Implement Layer Existence Check

| Field | Value |
|-------|-------|
| **Task ID** | PKG-R1 |
| **File** | `src/registry/client.rs` |
| **Description** | Replace stub `check_existing_layers()` with real HEAD request |
| **Acceptance Criteria** | 1. Sends HEAD to `/v2/{name}/blobs/{digest}`<br>2. Returns HashSet of digests that return 200<br>3. Returns empty set on 404<br>4. Unit test with mock server verifies behavior |

**Implementation sketch**:

```rust
fn check_existing_layers(
    &self,
    reg_ref: &RegistryRef,
    source: &RegistrySource,
    auth: &ResolvedAuth,
) -> anyhow::Result<HashSet<String>> {
    let mut existing = HashSet::new();
    
    for layer in &manifest.layers {
        let url = format!(
            "https://{}/v2/{}/blobs/{}",
            source.url, reg_ref.path, layer.digest
        );
        let req = self.http.head(&url);
        let req = auth.apply(req);
        let response = req.send()?;
        
        if response.status() == reqwest::StatusCode::OK {
            existing.insert(layer.digest.clone());
        }
    }
    
    Ok(existing)
}
```

---

#### PKG-R2: Implement `pekobot pull`

| Field | Value |
|-------|-------|
| **Task ID** | PKG-R2 |
| **Files** | `src/commands/pull.rs` |
| **Description** | Wire CLI to `RegistryClient::pull()` with progress output |
| **Acceptance Criteria** | 1. Resolves registry ref from CLI arg<br>2. Loads registry config from `~/.pekobot/registry.toml` (or default)<br>3. Calls `RegistryClient::pull()` with progress callback<br>4. Prints progress to stderr (or JSON if `--json`)<br>5. Stores manifest and layers to local registry path |

**Progress output format** (human):

```
Resolving pekohub.com/agents/researcher:v2.5...
Pulling layer sha256:abc123... (1.2 MB)
Pulling layer sha256:def456... (512 KB)
Verifying layer sha256:abc123...
Extracting layer sha256:abc123...
Done. Image: researcher:v2.5 (digest: sha256:xyz789...)
```

**Progress output format** (JSON):

```json
{"stage":"resolving","ref":"pekohub.com/agents/researcher:v2.5"}
{"stage":"pulling","layer":"sha256:abc123","bytes_received":1024,"bytes_total":1258291}
{"stage":"done","manifest":{"name":"researcher","version":"2.5.0","digest":"sha256:xyz789"}}
```

---

#### PKG-R3: Implement `pekobot push`

| Field | Value |
|-------|-------|
| **Task ID** | PKG-R3 |
| **Files** | `src/commands/push.rs` |
| **Description** | Wire CLI to `RegistryClient::push()` with progress output |
| **Acceptance Criteria** | 1. Parses local tag and remote registry ref<br>2. Loads local manifest by tag<br>3. Calls `RegistryClient::push()` with progress callback<br>4. Skips existing layers (uses PKG-R1)<br>5. Prints progress to stderr (or JSON if `--json`) |

---

#### PKG-R4: Registry Integration Tests

| Field | Value |
|-------|-------|
| **Task ID** | PKG-R4 |
| **File** | `tests/registry_integration.rs` (new) |
| **Description** | End-to-end test: build → push → pull with mock server |
| **Acceptance Criteria** | 1. Starts mock server<br>2. Builds an image locally<br>3. Pushes to mock server<br>4. Pulls from mock server<br>5. Asserts manifest digests match<br>6. Asserts all layers present |

**Test outline**:

```rust
#[tokio::test]
async fn test_push_pull_roundtrip() {
    // 1. Start mock server
    let server = MockRegistryServer::new(0).await;
    let handle = server.start().await;
    
    // 2. Build image
    let temp_dir = TempDir::new().unwrap();
    let agent_dir = create_test_agent_dir();
    let options = BuildOptions::new(temp_dir.path()).with_tag("test:v1.0");
    let builder = ImageBuilder::new(options);
    let manifest = builder.build(agent_dir.path(), |_| {}).await.unwrap();
    
    // 3. Push
    let client = RegistryClient::new(
        RegistryConfig::with_source(&server.base_url()),
        temp_dir.path()
    );
    client.push(&manifest.digest, &format!("{}/test:v1.0", server.base_url()), |_| {}).await.unwrap();
    
    // 4. Pull
    let pulled = client.pull(&format!("{}/test:v1.0", server.base_url()), |_| {}).await.unwrap();
    
    // 5. Assert
    assert_eq!(manifest.digest, pulled.digest);
    assert_eq!(manifest.layers.len(), pulled.layers.len());
    
    handle.shutdown().await;
}
```

---

### Phase 2 Exit Criteria

- [ ] `cargo test tests::registry_integration` passes
- [ ] `pekobot pull <mock-url>/test:v1.0` works against mock server
- [ ] `pekobot push <local-tag> <mock-url>/test:v1.0` works against mock server
- [ ] Layer existence check skips already-present layers (verify with logs)
- [ ] `cargo clippy` clean in `src/registry/` and `src/commands/pull.rs`, `push.rs`

---

## Phase 3: Image Build CLI

**Goal**: `pekobot build <path> -t <tag>` works end-to-end.

**Prerequisite**: Phase 1 complete.

### Tasks

#### PKG-B1: Implement `pekobot build`

| Field | Value |
|-------|-------|
| **Task ID** | PKG-B1 |
| **Files** | `src/commands/build.rs` |
| **Description** | Wire CLI to `ImageBuilder::build()` with progress output |
| **Acceptance Criteria** | 1. Validates source path exists and contains `config.toml`<br>2. Creates `BuildOptions` with tag and registry path<br>3. Calls `ImageBuilder::build()` with progress callback<br>4. Prints progress to stderr (or JSON if `--json`)<br>5. Prints manifest digest and total size on completion<br>6. Stores manifest and layers to local registry |

**Progress output format** (human):

```
Reading ./my-agent...
Layering config...
Hashing config.toml...
Layering markdown...
Layering tools...
Compressing tools...
Build complete.
  Image: my-agent:v1.0
  Digest: sha256:abc123...
  Layers: 4
  Total size: 2.3 MB
```

---

#### PKG-B2: Build Command Tests

| Field | Value |
|-------|-------|
| **Task ID** | PKG-B2 |
| **File** | `src/commands/build.rs` (unit tests) or `tests/build_integration.rs` |
| **Description** | Test build command with temp agent directory |
| **Acceptance Criteria** | 1. Creates temp agent dir with `config.toml`<br>2. Runs build command<br>3. Verifies manifest exists in registry<br>4. Verifies layers exist in registry |

---

### Phase 3 Exit Criteria

- [ ] `pekobot build ./test-agent -t test:v1.0` produces valid manifest
- [ ] `cargo test image::builder` still passes (no regressions)
- [ ] Build progress output is readable and informative
- [ ] `cargo clippy` clean in `src/commands/build.rs`

---

## Phase 4: Team Packaging Hardening

**Goal**: `.team` exports/imports with checksum validation + `team.toml` preservation.

**Prerequisite**: Phase 1 complete. Can run in parallel with Phases 2–3.

### Tasks

#### PKG-T1: Add Checksums to `TeamManifest`

| Field | Value |
|-------|-------|
| **Task ID** | PKG-T1 |
| **File** | `src/portable/team_packager.rs` |
| **Description** | Add `packaging` section to `TeamManifest` with file list and checksums |
| **Acceptance Criteria** | 1. `TeamManifest` has `packaging: PackagingMetadata` field<br>2. `TeamPackager::create_team_manifest()` computes SHA-256 for every file<br>3. Checksums stored as `HashMap<String, String>` (path → sha256:...)<br>4. `TeamManifest::to_toml()` serializes packaging section |

**Schema addition**:

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TeamManifest {
    pub team: TeamInfo,
    pub format: TeamFormat,
    pub export: ExportMetadata,
    pub packaging: Option<PackagingMetadata>, // NEW
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PackagingMetadata {
    pub files: Vec<String>,
    pub checksums: HashMap<String, String>, // path → "sha256:..."
}
```

---

#### PKG-T2: Include `team.toml` in `.team` Export

| Field | Value |
|-------|-------|
| **Task ID** | PKG-T2 |
| **File** | `src/portable/team_packager.rs` |
| **Description** | If `team.toml` exists in team directory, include it in export |
| **Acceptance Criteria** | 1. Checks for `team.toml` in team directory<br>2. If present, adds to archive as `team/team.toml`<br>3. Includes in checksum computation<br>4. Does not fail if `team.toml` is missing |

---

#### PKG-T3: Validate Checksums on Import

| Field | Value |
|-------|-------|
| **Task ID** | PKG-T3 |
| **File** | `src/portable/team_unpackager.rs` |
| **Description** | After extracting `.team`, verify all file checksums before importing agents |
| **Acceptance Criteria** | 1. If `packaging` section present, verify every file checksum<br>2. If checksum mismatch, fail fast with clear error<br>3. If `packaging` section absent (legacy), warn and continue<br>4. Unit test: valid checksums pass, invalid checksums fail |

---

#### PKG-T4: Restore `team.toml` on Import

| Field | Value |
|-------|-------|
| **Task ID** | PKG-T4 |
| **File** | `src/portable/team_unpackager.rs` |
| **Description** | If `team/team.toml` present in package, restore to team directory |
| **Acceptance Criteria** | 1. Checks for `team/team.toml` in extracted files<br>2. If present, writes to team directory<br>3. Does not fail if absent |

---

#### PKG-T5: Team Packaging Integration Tests

| Field | Value |
|-------|-------|
| **Task ID** | PKG-T5 |
| **File** | `src/portable/team_packager.rs` (tests) or `tests/team_integration.rs` |
| **Description** | End-to-end: export team → verify checksums → import → verify data |
| **Acceptance Criteria** | 1. Creates temp team with 2 agents<br>2. Exports to `.team`<br>3. Verifies manifest has checksums<br>4. Imports to new location<br>5. Verifies all agent files present<br>6. Verifies `team.toml` restored if present |

---

### Phase 4 Exit Criteria

- [ ] `cargo test portable` passes (including new team tests)
- [ ] Exported `.team` includes `packaging.checksums` section
- [ ] Import fails with clear error if checksum mismatch
- [ ] Import warns (but continues) if no checksums present
- [ ] `team.toml` roundtrips through export/import
- [ ] `cargo clippy` clean in `src/portable/`

---

## Phase 5: Extension Packaging

**Goal**: `pekobot ext export <id> -o <file.ext>` creates installable `.ext` packages.

**Prerequisite**: Phase 1 complete. Can run in parallel with Phases 2–4.

### Tasks

#### PKG-E1: Implement `ExtensionPackager`

| Field | Value |
|-------|-------|
| **Task ID** | PKG-E1 |
| **File** | `src/extension/manager/packaging.rs` (new) |
| **Description** | Create packager that exports installed extension to `.ext` tar.gz |
| **Acceptance Criteria** | 1. Takes `ExtensionId` and output path<br>2. Looks up extension in `ExtensionManager`<br>3. Reads all files from extension path<br>4. Creates tar.gz with `manifest.toml` + `extension/` directory<br>5. Computes SHA-256 checksums for all files<br>6. Returns output path on success |

**Output format**:

```
docker-skill.ext (gzip-compressed tar)
├── manifest.toml           # Extension package metadata
└── extension/
    ├── manifest.yaml       # Original extension manifest
    ├── SKILL.md            # Type-specific entry point
    └── ...                 # Additional files
```

**`manifest.toml` schema**:

```toml
[extension]
id = "docker-skill"
name = "Docker Skill"
version = "1.0.0"
extension_type = "skill"
created_at = "2026-05-08T10:00:00Z"
pekobot_version = "0.1.0"

[packaging]
files = ["manifest.toml", "extension/manifest.yaml", "extension/SKILL.md"]
checksums = { "manifest.toml" = "sha256:...", ... }
```

---

#### PKG-E2: Wire `pekobot ext export`

| Field | Value |
|-------|-------|
| **Task ID** | PKG-E2 |
| **File** | `src/commands/ext.rs` |
| **Description** | Connect `ExtCommands::Export` to `ExtensionPackager` |
| **Acceptance Criteria** | 1. Loads `ExtensionManager` with all adapters<br>2. Calls `ExtensionPackager::export()`<br>3. Prints output path on success<br>4. Prints clear error if extension not found |

---

#### PKG-E3: Extension Export Integration Test

| Field | Value |
|-------|-------|
| **Task ID** | PKG-E3 |
| **File** | `tests/extension_packaging.rs` (new) or `src/extension/manager/packaging.rs` (tests) |
| **Description** | End-to-end: install extension → export → verify `.ext` → install from `.ext` |
| **Acceptance Criteria** | 1. Creates temp extension directory<br>2. Installs via `ExtensionManager::install()`<br>3. Exports to `.ext`<br>4. Verifies `.ext` structure and checksums<br>5. Installs from `.ext` to new location<br>6. Verifies extension works after reinstall |

---

### Phase 5 Exit Criteria

- [ ] `pekobot ext export docker-skill -o docker-skill.ext` creates valid `.ext`
- [ ] Exported `.ext` can be installed via `pekobot ext install ./docker-skill.ext`
- [ ] `cargo test extension` passes (including new packaging tests)
- [ ] `cargo clippy` clean in `src/extension/manager/packaging.rs`

---

## Phase 6: Final Integration

**Goal**: All tests pass, docs updated, clippy clean.

**Prerequisite**: All previous phases complete.

### Tasks

#### PKG-I1: Full Integration Test

| Field | Value |
|-------|-------|
| **Task ID** | PKG-I1 |
| **File** | `tests/packaging_integration.rs` (new) |
| **Description** | Combined test: build → push → pull → export team → import team |
| **Acceptance Criteria** | 1. Builds image<br>2. Pushes to mock registry<br>3. Pulls from mock registry<br>4. Creates team with agent using pulled image<br>5. Exports team to `.team`<br>6. Imports team to new location<br>7. Verifies all data intact |

---

#### PKG-I2: Update `DATA_MODEL.md`

| Field | Value |
|-------|-------|
| **Task ID** | PKG-I2 |
| **File** | `DATA_MODEL.md` |
| **Description** | Document any format changes from implementation |
| **Acceptance Criteria** | 1. Team manifest schema includes `packaging` section<br>2. Extension package format documented<br>3. Image manifest schema unchanged (no changes) |

---

#### PKG-I3: Clippy Cleanup

| Field | Value |
|-------|-------|
| **Task ID** | PKG-I3 |
| **Files** | All modified/created files |
| **Description** | Run clippy on all new code, fix all warnings |
| **Acceptance Criteria** | `cargo clippy --all-targets` has zero warnings in packaging-related modules |

---

#### PKG-I4: Coverage Check

| Field | Value |
|-------|-------|
| **Task ID** | PKG-I4 |
| **Description** | Verify test coverage for packaging modules |
| **Acceptance Criteria** | `cargo test` coverage ≥ 70% for: `src/portable/`, `src/image/`, `src/registry/`, `src/extension/manager/` |

---

### Phase 6 Exit Criteria

- [ ] All integration tests pass
- [ ] `DATA_MODEL.md` updated
- [ ] `cargo clippy --all-targets` clean
- [ ] Coverage ≥ 70% for packaging modules
- [ ] Spec updated if implementation deviated from plan

---

## Parallelization Guide

After Phase 1, two developers can work in parallel:

### Track A: Registry + Image (Developer A)

```
Phase 1 (Foundation)
  ↓
Phase 2 (Registry Integration)
  ↓
Phase 3 (Image Build CLI)
  ↓
Phase 6 (Final Integration — shared with Track B)
```

**Files owned**: `src/registry/mock_server.rs`, `src/registry/client.rs`, `src/commands/pull.rs`, `src/commands/push.rs`, `src/commands/build.rs`

### Track B: Team + Extension (Developer B)

```
Phase 1 (Foundation)
  ↓
Phase 4 (Team Packaging Hardening)
  ↓
Phase 5 (Extension Packaging)
  ↓
Phase 6 (Final Integration — shared with Track A)
```

**Files owned**: `src/portable/team_packager.rs`, `src/portable/team_unpackager.rs`, `src/extension/manager/packaging.rs`, `src/commands/ext.rs`

### Conflict Zones (coordinate)

| File | Both tracks touch | Resolution |
|------|-------------------|------------|
| `src/commands/mod.rs` | Both add enum variants | Phase 1 adds all variants; later phases only modify handler files |
| `Cargo.toml` | Both may add dependencies | Coordinate in Phase 1; add all needed deps upfront |
| `tests/` | Both add integration tests | Use descriptive filenames; no conflicts expected |

---

## Review Checklist (Per Phase)

Before marking a phase complete, verify:

- [ ] All tasks in phase have implementation + tests
- [ ] `cargo test` passes for modified modules
- [ ] `cargo clippy` clean for new/modified files
- [ ] No `unwrap()` or `expect()` in production code (tests OK)
- [ ] Error messages are user-friendly (not raw `anyhow` traces)
- [ ] New files have module-level doc comments
- [ ] Public APIs have rustdoc comments
- [ ] No dead code (or marked with `#[allow(dead_code)]` + explanation)
- [ ] Integration tests cover happy path + at least one error path

---

## Tracking Template

Use this to track progress. Copy into a GitHub issue or project board.

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

*End of Implementation Plan*
