# Issue 023: Team Registry Push/Pull Uses Single Opaque Layer

**Status:** Closed  
**Priority:** P1  
**Area:** Packaging / Registry / Teams  
**Related:** ADR-027 (Unified Packaging), `src/commands/team.rs`, `src/portable/team_packager.rs`

---

## 1. Problem Summary (Confirmed)

When a team is pushed to a registry via `peko team push`, the entire `.team` package is exported to a temporary file and stored as a **single opaque blob** with `LayerType::Config`. When pulled, that single blob is downloaded and imported as a `.team` file. This architectural shortcut means:

1. **Zero layer deduplication** — If two teams share the same agent, both `.team` files are stored as separate blobs because each is a distinct tar.gz archive.
2. **Inefficient storage** — Registry storage grows with team count, not unique agent count.
3. **Redundant uploads** — Pushing a team re-uploads all its agents even if those agents already exist in the registry.
4. **Protocol mismatch with intent** — The `.team` format already contains structured agent data, but the registry protocol doesn't leverage it.

---

## 2. Root Cause Analysis

### 2.1 How Agent Push/Pull Works (Correctly)

The `.agent` registry path implements content-addressable layers properly:

```
Source dir → AgentBuilder::build_from_directory()
  ├── config/      → pack_layer() → digest A → store_layer(A)
  ├── identity/    → pack_layer() → digest B → store_layer(B)
  ├── skills/      → pack_layer() → digest C → store_layer(C)  [optional]
  ├── workspace/   → pack_layer() → digest D → store_layer(D)  [optional]
  └── ...

RegistryManifest: layers = [Config(A), Identity(B), Skills(C), Workspace(D), ...]
```

Each layer is a **gzipped tarball** of files for that layer type. The `RegistryClient::push()` checks `check_existing_layers()` via HEAD requests, skipping already-present layers. The `RegistryClient::pull()` downloads each layer independently.

### 2.2 How Team Push/Pull Works (Broken)

```
Team export → TeamPackager::export()
  ├── Agent 1 files (collected in-memory)
  ├── Agent 2 files (collected in-memory)
  └── team.toml + manifest.toml
  → create_team_archive() → single tar.gz → digest X

handle_team_push():
  data = read(entire .team file)
  digest = compute_digest(data)          ← ONE digest for everything
  manifest.add_layer(Layer::new(digest, Config, size))  ← ONE layer
```

The entire team is treated as an opaque Config layer. The registry's deduplication machinery is completely bypassed.

### 2.3 Why the Existing Code Can't Fix This Trivially

| Component | Assumption | Why It Blocks |
|-----------|-----------|---------------|
| `LayerType` | Enum of 6 agent layer types | No `TeamConfig` or `AgentBundle` variant |
| `RegistryManifest` | `kind="agent"` or `"team"` | No schema for multi-agent layer references |
| `AgentRegistry::store_layer()` | Stores gzipped tarballs | Team layers would need different semantics |
| `TeamPackager::export()` | Builds single tar.gz archive | No decomposition into per-agent layers |
| `TeamUnpackager::import()` | Expects single `.team` file | No reconstruction from separate layers |

---

## 3. Refined Solution: Team-as-Layers (Option A+ Revised)

### 3.1 Design Principles

1. **Reuse existing infrastructure** — Don't invent new storage formats; reuse `AgentRegistry` layers and `RegistryClient` push/pull.
2. **Agent layers are first-class** — Each agent in a team becomes a set of standard `LayerType` layers (Config, Identity, Skills, etc.).
3. **Team metadata is a thin config layer** — Team manifest + `team.toml` become a small "team config" layer.
4. **No `.team` file reconstruction on pull** — Import agents directly from layers; skip the temporary `.team` file entirely.
5. **Backward compatibility** — `peko team export` still produces `.team` files; only `push`/`pull` change.

### 3.2 New Layer Type

Add one new `LayerType` variant:

```rust
pub enum LayerType {
    Config,
    Identity,
    Skills,
    Workspace,
    Sessions,
    Mcp,
    TeamConfig,  // ← NEW: team metadata (team.toml, manifest.toml, agent index)
}
```

`TeamConfig` is a gzipped tarball containing:
```
team.toml          # Team runtime definition (optional)
manifest.toml      # Team packaging metadata + agent index
```

The `manifest.toml` inside the `TeamConfig` layer contains an **agent index**:

```toml
[team]
name = "prod-team"
version = "1.0.0"
agent_count = 3

[agents]
# agent_name → list of layer digests (same format as AgentManifest.layers)
researcher = { config = "sha256:abc...", identity = "sha256:def...", skills = "sha256:ghi..." }
coder = { config = "sha256:jkl...", identity = "sha256:mno..." }
reviewer = { config = "sha256:pqr...", identity = "sha256:stu...", workspace = "sha256:vwx..." }
```

### 3.3 RegistryManifest for Teams

```rust
// handle_team_push builds this:
RegistryManifest {
    kind: "team",
    name: "prod-team",
    version: "1.0.0",
    layers: [
        Layer::new(team_config_digest, TeamConfig, team_config_size),
        Layer::new(agent1_config_digest, Config, agent1_config_size),
        Layer::new(agent1_identity_digest, Identity, agent1_identity_size),
        Layer::new(agent1_skills_digest, Skills, agent1_skills_size),
        Layer::new(agent2_config_digest, Config, agent2_config_size),
        Layer::new(agent2_identity_digest, Identity, agent2_identity_size),
        // ... etc
    ],
}
```

**Key insight:** All agent layers use the **same** `LayerType` variants as standalone agents. The `TeamConfig` layer is the only new type, and it acts as the "manifest of manifests" — telling the pull side which agents exist and which layer digests compose each agent.

### 3.4 Push Flow (Revised)

```
handle_team_push(team_name, registry_ref):
  1. service.export_team(team_name, temp_path, ...) → .team file
  2. Extract .team file in-memory (tar.gz → file map)
  3. Group files by agent: Map<agent_name, Map<file_path, bytes>>
  4. For each agent:
       a. Categorize files into layer types (config/, identity/, skills/, ...)
       b. Build gzipped tarball per layer type (reuse AgentBuilder::build_tarball pattern)
       c. Compute digest, store_layer() in AgentRegistry
       d. Record digests in agent_layers map
  5. Build TeamConfig layer:
       a. Serialize team manifest.toml with agent index
       b. Include team.toml if present
       c. Gzip tarball → compute digest → store_layer()
  6. Build RegistryManifest with kind="team":
       a. Add TeamConfig layer
       b. Add all agent layers (deduplicated — same digest = same layer)
  7. Store manifest, push via RegistryClient (skips existing layers automatically)
  8. Clean up temp file
```

### 3.5 Pull Flow (Revised)

```
handle_team_pull(registry_ref, new_name):
  1. client.pull(registry_ref) → RegistryManifest
  2. Verify manifest.kind == "team"
  3. Find TeamConfig layer, extract manifest.toml
  4. Parse agent index from manifest.toml
  5. For each agent in index:
       a. Collect its layers from AgentRegistry (already downloaded by client.pull)
       b. Reconstruct agent files in-memory from layer tarballs
       c. Import agent directly via Unpackager::import_from_files()
  6. Write team.toml if present in TeamConfig layer
  7. Report results
```

**Critical improvement:** No temporary `.team` file is created. Agents are imported directly from registry layers.

---

## 4. Cross-Team Deduplication Mechanics

### Scenario: Team A and Team B share Agent X

**Team A push:**
```
RegistryManifest layers:
  TeamConfig(A)        → NEW
  Config(X)            → NEW
  Identity(X)          → NEW
  Skills(X)            → NEW
```

**Team B push:**
```
RegistryManifest layers:
  TeamConfig(B)        → NEW (different team metadata)
  Config(X)            → EXISTS (HEAD /blobs/Config(X) → 200, skipped)
  Identity(X)          → EXISTS (skipped)
  Skills(X)            → EXISTS (skipped)
  Config(Y)            → NEW (Agent Y is unique to Team B)
```

**Registry blob count after both pushes:** 6 blobs (not 8). Deduplication works automatically via the existing `RegistryClient::check_existing_layers()`.

---

## 5. Edge Cases and Handling

| Edge Case | Handling |
|-----------|----------|
| **Same agent name, different content** | Different digests → different layers → stored separately. Correct behavior. |
| **Same agent content, different names** | Same digests → deduplicated. Agent name is metadata in TeamConfig, not part of layer data. |
| **Shared skills across agents in same team** | Each agent's `skills/` directory is packed independently. If identical, digests match → deduplicated. |
| **Empty team (no agents)** | TeamConfig layer only. Valid but unusual. |
| **Team with 50+ agents** | RegistryManifest has many layers. Within HTTP header limits (OK: ~100 layers × 80 chars = 8KB). |
| **Subteams / hierarchical teams** | Agent index uses flat names (`subteam/agent`). Subteam structure encoded in TeamConfig metadata, not layer semantics. |
| **Pull with missing layers** | `RegistryClient::pull_layer()` already errors on HTTP failure. Propagate up. |
| **Backward compat: old `.team` files** | `peko team export` / `import` unchanged. Only `push`/`pull` use new protocol. |
| **Agent pull after team pull** | Agent layers are now in local `AgentRegistry`. Could add `peko agent pull --from-team` but out of scope. |

---

## 6. Implementation Plan

### Phase 1: Foundation (1-2 days)

1. **Add `TeamConfig` to `LayerType`** (`src/portable/types.rs`)
   - Add variant, update `dir_name()` → `"team"`
   - Update serialization tests

2. **Add `TeamAgentIndex` types** (`src/portable/team_packager.rs` or new `src/portable/team_manifest.rs`)
   ```rust
   #[derive(Serialize, Deserialize)]
   pub struct TeamAgentIndex {
       pub agents: HashMap<String, AgentLayerRef>,
   }

   #[derive(Serialize, Deserialize)]
   pub struct AgentLayerRef {
       pub config: String,
       pub identity: String,
       pub skills: Option<String>,
       pub workspace: Option<String>,
       pub sessions: Option<String>,
       pub mcp: Option<String>,
   }
   ```

3. **Add `AgentRegistry::store_team_config_layer()` helper** (optional convenience)

### Phase 2: Push Refactor (2-3 days)

1. **Refactor `handle_team_push`** (`src/commands/team.rs:496-597`)
   - Replace single-blob logic with layer decomposition
   - Extract agent files from `.team` tar.gz
   - Build per-agent layers using `AgentBuilder::build_tarball` pattern
   - Build `TeamConfig` layer with agent index
   - Construct multi-layer `RegistryManifest`

2. **Add `TeamLayerBuilder` utility** (`src/portable/`)
   - `decompose_team_archive(bytes) -> (TeamConfigBytes, Vec<AgentLayers>)`
   - `build_agent_layer(files, layer_type) -> (digest, bytes)`

### Phase 3: Pull Refactor (2-3 days)

1. **Refactor `handle_team_pull`** (`src/commands/team.rs:599-702`)
   - Replace single-blob download with multi-layer processing
   - Extract TeamConfig layer, parse agent index
   - Reconstruct each agent from its layers
   - Import directly via `Unpackager::import_from_files()`

2. **Add `TeamLayerReconstructor` utility** (`src/portable/`)
   - `extract_team_config(manifest) -> TeamAgentIndex`
   - `reconstruct_agent_files(registry, agent_ref) -> HashMap<String, Vec<u8>>`

### Phase 4: Tests (2-3 days)

1. **Unit tests**
   - `TeamLayerBuilder::decompose_team_archive` with shared agents
   - `TeamLayerReconstructor::reconstruct_agent_files`
   - Digest determinism (same agent → same digests)

2. **E2E test: `team_registry_dedup.ps1`** (new)
   - Create Team A with Agent X
   - Push Team A → note blob count
   - Create Team B with Agent X (same config)
   - Push Team B → verify blob count increased by only TeamConfig size
   - Pull Team B → verify imports correctly
   - Verify Agent X functions identically in both teams

3. **Update existing E2E tests**
   - `team_registry_snapshot.ps1` — should still pass (push/pull still work)
   - `registry_layer_dedup.ps1` — unchanged (tests agent dedup)

### Phase 5: Documentation (1 day)

1. Update `DATA_MODEL.md` §Team Registry Format
2. Update `API_SURFACE.md` if new public types added
3. Update `CHANGELOG.md`

---

## 7. Files to Modify

| File | Change |
|------|--------|
| `src/portable/types.rs` | Add `LayerType::TeamConfig` |
| `src/portable/team_packager.rs` | Add `TeamAgentIndex`, `AgentLayerRef` types |
| `src/portable/mod.rs` | Re-export new types |
| `src/commands/team.rs` | Rewrite `handle_team_push`, `handle_team_pull` |
| `src/portable/team_layer_builder.rs` | **NEW**: Decompose `.team` into layers |
| `src/portable/team_layer_reconstructor.rs` | **NEW**: Reconstruct agents from layers |
| `e2e_tests/packaging/team_registry_dedup.ps1` | **NEW**: Deduplication E2E test |
| `DATA_MODEL.md` | Document team registry layer schema |
| `CHANGELOG.md` | Entry for v0.1.0 |

---

## 8. Why Not Option B (Annotations)?

The issue proposed Option B: keep single blob + add annotations. **Rejected because:**

1. **Doesn't solve the actual problem** — Storage is still O(team count × avg team size). Annotations only help a hypothetical registry-side optimizer, not the wire protocol.
2. **Complex registry logic** — The mock registry and any real registry would need to parse `.team` tar.gz files to extract agent digests for deduplication. This violates layer boundaries.
3. **No incremental pull** — Pulling a team still downloads the entire blob even if you already have some agents locally.
4. **Inconsistent with ADR-027** — The whole point of ADR-027 was content-addressable layers. Option B is a step backward.

---

## 9. Why Not Store Each Agent as a Single "AgentBundle" Layer?

Alternative: pack each agent's *entire* files into one layer per agent, rather than decomposing into Config/Identity/Skills/etc.

**Rejected because:**

1. **Less deduplication** — If two agents share the same skills, the skills are duplicated inside each agent's bundle layer.
2. **Inconsistent with agent registry** — Agent push/pull uses Config/Identity/Skills layers. Team agents should use the same layer granularity so cross-team *and* cross-agent deduplication both work.
3. **No extra complexity** — The code to decompose into standard layer types already exists in `AgentBuilder`. Reusing it is simpler than inventing a new bundle format.

---

## 10. Summary

| Aspect | Current | Proposed |
|--------|---------|----------|
| Team push | 1 opaque Config layer | TeamConfig + per-agent standard layers |
| Team pull | Download `.team` blob, import file | Download layers, import agents directly |
| Cross-team dedup | None | Full — shared agents share layers |
| Registry storage | O(team × size) | O(unique agents × size + team metadata) |
| Wire format change | — | Add `LayerType::TeamConfig`, agent index in TeamConfig |
| Backward compat | — | `export`/`import` unchanged; only `push`/`pull` affected |
| Estimated effort | — | 6-10 days (Phases 1-5) |

The proposed solution aligns team registry behavior with the content-addressable layer model established for agents in ADR-027, fixes the deduplication gap, and requires no changes to the registry server protocol or storage format.
