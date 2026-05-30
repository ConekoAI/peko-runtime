# Issue 028: Team Pull Doesn't Auto-Pull/Enable Extensions

**Status:** Open
**Priority:** P2
**Area:** Team Pull / Extension System / Registry
**Related:** `src/commands/team.rs`, `src/commands/ext.rs`, `src/portable/unpackager.rs`, `src/portable/types.rs`, `src/common/types/team.rs`

---

## 1. Problem Summary

When `peko team pull` reconstructs a team from the registry, it:
1. Pulls agent configs including `extensions.enabled` whitelists
2. Writes `team/extensions.toml` and `agent/config/agent.toml` with those whitelists
3. But the extension **binaries** are not pulled from the registry — they must be installed separately via `peko ext pull`

**Impact:** A pulled team "looks" imported but tools don't work until the user manually runs `peko ext pull` and `peko ext enable` for each extension.

---

## 2. Root Cause Analysis

### 2.1 The Layer System Has No Extension Variant

[`LayerType`](src/portable/types.rs:101-120) only knows these layer types:
```rust
pub enum LayerType {
    Config, Identity, Skills, Workspace, Sessions, Mcp, TeamConfig,
}
```

There is **no `Extension` variant**. Extension binaries are not stored as content-addressable layers in the registry, and therefore cannot be deduplicated or pulled as part of team push/pull.

### 2.2 Extension Config Is Names, Not Registry Refs

Agent extensions are declared as simple names in `agent.toml`:
```toml
[extensions]
enabled = ["calculator-skill", "shell", "read_file"]
```

These names are not registry references (e.g., `pekohub.com/extensions/calculator-skill:latest`). The extension system has no mapping from extension name → registry ref at pull time.

### 2.3 `handle_team_pull` Has No Extension Processing

[`handle_team_pull`](src/commands/team.rs:722) calls `reconstruct_team()` → `Unpackager::import_from_files()`. The import process:
- Reads `extensions.enabled` from the agent's `config/agent.toml`
- Writes `team/extensions.toml`
- **Does nothing else** — no checking if extensions are installed, no pull requests, no enable calls

### 2.4 Compare: `peko ext pull` Has Dependency Resolution (Issue 027)

[`handle_ext_pull`](src/commands/ext.rs:1249) does implement dependency resolution — it downloads extension packages and recursively resolves declared dependencies. But `handle_team_pull` has no analogous path.

---

## 3. Design Goals

1. **Zero manual steps after `peko team pull`** — extensions should be auto-pulled and auto-enabled as part of team pull
2. **Leverage existing infrastructure** — reuse `handle_ext_pull`, existing registry refs, and dependency resolution from Issue 027
3. **Preserve extension deduplication** — multiple teams sharing the same extension should share the same extension binary layers
4. **Graceful degradation** — if an extension can't be found in the registry, report clearly and continue with others
5. **Backward compatibility** — `peko team export`/`import` unchanged; only `push`/`pull` gain auto-extension behavior

---

## 4. Proposed Solution (Revised)

### 4.1 Extension References in Team Manifest (Recommended)

The team manifest (inside `TeamConfig` layer) should include extension registry references alongside agent configs.

**Add an `extensions` field to `TeamAgentIndex`** in `team_packager.rs`:
```rust
#[derive(Serialize, Deserialize)]
pub struct TeamAgentIndex {
    pub team: TeamInfo,
    pub agents: HashMap<String, AgentLayerRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<ExtensionRef>,  // NEW
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionRef {
    /// Extension ID as declared in manifest.yaml
    pub id: String,
    /// Registry reference for pull (e.g., "pekohub.com/extensions/calculator-skill:latest")
    pub registry_ref: String,
}
```

**When `handle_team_push` builds the team package:**
1. For each agent's `config/agent.toml`, read `extensions.enabled` list
2. For each enabled extension that is an installed extension (not built-in):
   - Look up its registry reference via `ExtensionManager::resolve_tool_to_registry_ref()`
   - Build `ExtensionRef` and add to team's extension list (deduplicated)
3. Write `extensions` list into `TeamAgentIndex`

**When `handle_team_pull` imports a team:**
1. After importing all agents, read `extensions` from team manifest
2. For each `ExtensionRef`, check if already installed via `ExtensionManager`
3. If not installed, call `handle_ext_pull(registry_ref)` (which handles dependencies recursively per Issue 027)
4. For each installed extension, ensure it's enabled in `team/extensions.toml`
5. Report what was pulled/enabled vs. already present

### 4.2 Prerequisite: Track Extension Source at Install Time

**Problem:** `ExtensionManifest` has no `registry_ref` field. We cannot look up where an extension came from.

**Solution:** Add `source: Option<String>` to `ExtensionManifest` and populate it during `peko ext pull`:
```rust
pub struct ExtensionManifest {
    // ... existing fields ...
    /// Registry reference this extension was pulled from (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}
```

Also write a `.source` file alongside installed extensions in `ExtensionStorage` for reliable retrieval even if the manifest doesn't have the field.

### 4.3 Prerequisite: Tool-Name → Extension Resolution

**Problem:** `extensions.enabled` contains tool names (e.g., `"calculator-skill"`), not extension IDs. Built-ins like `"shell"` are mixed with installed extensions.

**Solution:** Add `resolve_tool_name()` to `ExtensionManager`:
```rust
impl ExtensionManager {
    /// Given a tool/capability name from an agent's extensions.enabled whitelist,
    /// resolve it to an installed extension's ID and registry ref (if available).
    /// Returns None for built-in tools or unknown names.
    pub fn resolve_tool_name(&self, name: &str) -> Option<ToolResolution> {
        // 1. Check if it's a built-in tool
        if BuiltinToolAdapter::is_builtin(name) { return None; }
        
        // 2. Check loaded extensions for a manifest.name match
        for ext in self.extensions.values() {
            if ext.manifest.name.eq_ignore_ascii_case(name) {
                return Some(ToolResolution {
                    id: ext.manifest.id.0.clone(),
                    registry_ref: ext.manifest.source.clone(),
                });
            }
        }
        None
    }
}
```

---

## 5. Implementation Plan (Revised)

### Phase 0: Prerequisites (Foundation)

1. **Add `source: Option<String>` to `ExtensionManifest`** (`src/extension/types/manifest.rs`)
2. **Track source in `ExtensionStorage`** — write `.source` file during `copy_to_storage`, read it during `load_all`
3. **Populate `source` during `handle_ext_pull`** — set `manifest.source = Some(registry_ref.to_string())` after pull
4. **Add `resolve_tool_name()` to `ExtensionManager`** (`src/extension/manager/mod.rs`)

### Phase 1: Extension Registry Refs in Team Manifest

5. **Add `ExtensionRef` struct** to `src/portable/team_packager.rs`
6. **Add `extensions: Vec<ExtensionRef>` to `TeamAgentIndex`** with `#[serde(default)]`
7. **Update `build_team_config_layer`** to accept and write the extensions list

### Phase 2: Team Push Records Extensions

8. **Modify `handle_team_push`** (`src/commands/team.rs`):
   - After extracting agent files from the `.team` archive
   - For each agent's `extensions.enabled` list:
     - Create a temporary `ExtensionManager`, load all installed extensions
     - Call `manager.resolve_tool_name(name)` to get extension ID and registry ref
     - Skip built-ins and unresolved names (with a warning)
     - Build deduplicated `ExtensionRef` list
   - Pass extensions list to layer builder

### Phase 3: Team Pull Auto-Pulls and Enables

9. **Add `ensure_extensions_for_team`** method to a new service or inline in `handle_team_pull`:
   - Takes `&[ExtensionRef]` and `team_name`
   - Checks which extensions are already installed
   - For missing extensions, calls `handle_ext_pull` (which handles dependencies per Issue 027)
   - Ensures each extension is enabled in the team's `extensions.toml`
   - Returns summary: pulled, already_present, failed

10. **Modify `handle_team_pull`** to call the ensure method after agent import

### Phase 4: Graceful Degradation

11. **Continue on partial failure** — if extension X fails to pull, log a warning but proceed with the rest. The team is still usable with the extensions that succeeded.

12. **Add `--no-extensions` flag to `peko team pull`** for users who want to skip auto-extension pull (e.g., air-gapped environments).

---

## 6. Key Design Decisions

**Why store registry refs in team manifest instead of resolving at pull time?**
- Registry refs are stable — once pushed, the team should pull the same version
- Deduplication is explicit — we know which extensions the team needs before pulling
- No dependency on registry index at pull time (resilience, offline scenarios)
- Consistent with how agent registry works: layers are content-addressed, manifest tells you what layers exist

**Why not use `LayerType::Extension` and store extension binaries as layers?**
- Extensions are versioned independently of teams. Storing them as team layers would pin extension versions to team versions.
- The existing `ExtensionStorage` at `$data_dir/extensions/` is the right place for extension binaries.
- Team manifests should reference extensions, not embed them.

**Why not just copy extension binaries during team pull (like copying agent files)?**
- Extension binaries are large and versioned separately
- The same extension should be shared across teams, not copied per-team
- Content-addressable storage is already implemented for extensions via `ExtensionStorage`

**Why add `source` to `ExtensionManifest` AND a `.source` file?**
- The manifest field is convenient for in-memory lookups
- The `.source` file is durable — it survives even if the manifest is regenerated
- Both are written at install time and read at load time

**Relationship to Issue 027 (Extension Dependency Resolution):**
- Issue 027 makes `peko ext pull` handle extension → extension dependencies
- This issue extends that: `peko team pull` should use `peko ext pull` for its extensions
- The dependency chain works: `handle_team_pull` → `handle_ext_pull` (per extension) → `resolve_dependencies` (per extension)

---

## 7. Files to Modify

| File | Change |
|------|--------|
| `src/extension/types/manifest.rs` | Add `source: Option<String>` to `ExtensionManifest` |
| `src/extension/manager/storage.rs` | Write/read `.source` file for installed extensions |
| `src/extension/manager/mod.rs` | Add `resolve_tool_name()`; add `source` population during load |
| `src/commands/ext.rs` | Populate `manifest.source` during `handle_ext_pull` |
| `src/portable/team_packager.rs` | Add `ExtensionRef`, add `extensions` field to `TeamAgentIndex` |
| `src/portable/team_layer_builder.rs` | Accept extensions list in `build_team_config_layer` |
| `src/commands/team.rs` | Modify `handle_team_push` to collect extensions; `handle_team_pull` to auto-pull |
| `src/common/types/team.rs` | Update `TeamExtConfig` if needed for the auto-enable step |
| `e2e_tests/packaging/team_extensions_auto_pull.ps1` | **NEW** E2E test |

---

## 8. Tasks

- [ ] Add `source: Option<String>` to `ExtensionManifest`
- [ ] Write `.source` file in `ExtensionStorage::copy_to_storage`; read in `ExtensionManager::load_all`
- [ ] Populate `manifest.source` during `handle_ext_pull`
- [ ] Add `resolve_tool_name()` to `ExtensionManager`
- [ ] Add `ExtensionRef` struct to `src/portable/team_packager.rs`
- [ ] Add `extensions: Vec<ExtensionRef>` to `TeamAgentIndex`
- [ ] Update `build_team_config_layer` to accept extensions list
- [ ] Modify `handle_team_push` to build `extensions` list from agent configs
- [ ] Add `ensure_extensions_for_team` logic (inline or as service method)
- [ ] Add `ExtensionPullResult` struct with `pulled`, `already_present`, `failed` lists
- [ ] Modify `handle_team_pull` to call ensure method after agent import
- [ ] Add `--no-extensions` flag to `peko team pull`
- [ ] Ensure `handle_ext_pull` dependency resolution (Issue 027) is wired up correctly
- [ ] Test: team pull with all extensions already installed (no-op, reports "already present")
- [ ] Test: team pull with missing extensions (pulls each, respects dependency chain)
- [ ] Test: team pull with one extension pull failure (warns, continues with others)
- [ ] Test: team pull with `--no-extensions` skips extension processing
- [ ] Test: two teams sharing same extension (second pull reports "already present")
