# Issue 023: Team Registry Push/Pull Uses Single Opaque Layer — Loses Content-Addressable Deduplication

**Status:** Open  
**Priority:** P1  
**Area:** Packaging / Registry / Teams  
**Related:** ADR-027 (Unified Packaging), `src/commands/team.rs`, `src/portable/team_packager.rs`

---

## Problem Summary

When a team is pushed to a registry via `peko team push`, the entire `.team` package is exported to a temporary file and stored as a **single opaque blob** with `LayerType::Config`. When pulled, that single blob is downloaded and imported as a `.team` file. This architectural shortcut means:

1. **Zero layer deduplication** — If two teams share the same agent, both `.team` files are stored as separate blobs because each is a distinct tar.gz archive.
2. **Inefficient storage** — Registry storage grows with team count, not unique agent count.
3. **Redundant uploads** — Pushing a team re-uploads all its agents even if those agents already exist in the registry.
4. **Protocol mismatch with intent** — The `.team` format already contains structured agent data (see below), but the registry protocol doesn't leverage it.

---

## Evidence

### `handle_team_push` (`src/commands/team.rs:496-597`)

```rust
async fn handle_team_push(...) {
    // Export team to a temp .team file
    let export_result = service.export_team(name, ...).await?;
    let data = tokio::fs::read(&export_result.output_path).await?;
    let layer_digest = compute_digest(&data);

    // Build RegistryManifest with kind="team", SINGLE layer
    let mut manifest = RegistryManifest::new(name.to_string(), "1.0.0".to_string())
        .with_kind("team");
    manifest.add_layer(Layer::new(
        layer_digest.clone(),
        LayerType::Config,  // <-- Entire team stored as one Config layer
        data.len() as u64,
    ));
}
```

### `handle_team_pull` (`src/commands/team.rs:599-702`)

```rust
async fn handle_team_pull(...) {
    // ... pull manifest ...
    let layer = manifest.layers.first().ok_or_else(...)?;
    let data = agent_registry.get_layer(&layer.digest).await?;

    // Write to a temp .team file
    let temp_path = temp_dir.join(format!("{}.team", manifest.name));
    tokio::fs::write(&temp_path, &data).await?;

    // Import the team from the single blob
    let result = service.import_team(temp_path.to_string_lossy().as_ref(), ...).await?;
}
```

### What `.team` contains internally (`src/portable/team_packager.rs`)

A `.team` file is a `tar.gz` archive containing:

```
team/manifest.toml     # Team metadata with per-file checksums
team/team.toml         # Team configuration
agents/{agent1}/
  config.toml
  identity/
    did.json
    keys/
      ...
  workspace/
    ...
  sessions/
    ...
agents/{agent2}/
  ...
```

Each agent's files are already individually accessible inside the tar. The registry protocol **could** store each agent as its own layer(s), but currently treats the entire archive as one blob.

---

## Impact

| Scenario | Current Behavior | Expected Behavior |
|----------|-----------------|-------------------|
| Team A has Agent X, Team B has Agent X | Two separate `.team` blobs stored | Agent X stored once, both team manifests reference it |
| Push Team B after Team A | Re-uploads Agent X | Skips Agent X (already exists) |
| Registry storage | O(team count × avg team size) | O(unique agent count × avg agent size + team metadata) |

---

## Why Tests Don't Catch This

The existing E2E tests (`team_registry_snapshot.ps1`, `team_full_lifecycle.ps1`, `team_subteam_hierarchy.ps1`) verify:
- ✓ Push succeeds
- ✓ Pull succeeds
- ✓ Structural integrity after import

They do **NOT** verify:
- ✗ Registry blob count when teams share agents
- ✗ Layer deduplication across teams
- ✗ That individual agents inside a team are addressable layers

The `registry_layer_dedup.ps1` test only tests **agent** layer deduplication, not team layer deduplication. It would pass even if team push/pull never deduplicated anything.

---

## Proposed Fix

### Option A: Decompose `.team` into agent layers + team manifest layer (Recommended)

1. **On push:**
   - Parse the `.team` tar.gz
   - For each agent, compute its digest and push it as a separate layer
   - Push team metadata (team.toml, manifest.toml) as a small "team config" layer
   - Build a `RegistryManifest` with `kind="team"` that references all agent layers + the team config layer

2. **On pull:**
   - Download all layers referenced by the team manifest
   - Reconstruct the `.team` tar.gz from the layers
   - Import as usual

### Option B: Store `.team` as a single layer but with agent-level annotations

- Keep the single blob approach
- Add annotations to the manifest listing embedded agent digests
- Use these for cross-team deduplication at the registry level
- More complex registry logic, less standard

---

## Test That Should Be Added

A new E2E test should:

1. Create Team A with Agent X
2. Push Team A to registry → note blob count
3. Create Team B with Agent X (same agent config)
4. Push Team B to registry → verify blob count increased by only the team metadata size, not the full agent size
5. Pull Team B → verify it imports correctly
6. Verify Agent X functions identically in both teams

---

## Related Code

- `src/commands/team.rs` — `handle_team_push`, `handle_team_pull`
- `src/portable/team_packager.rs` — `TeamPackager::export`, `create_team_archive`
- `src/registry/client.rs` — `RegistryClient::push`, `RegistryClient::pull`
- `e2e_tests/packaging/team_registry_snapshot.ps1`
- `e2e_tests/packaging/registry_layer_dedup.ps1`
