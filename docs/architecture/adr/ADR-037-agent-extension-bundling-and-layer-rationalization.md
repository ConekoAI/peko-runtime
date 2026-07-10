# ADR-037: Agent-Extension Bundling and Package Layer Rationalization

| Field | Value |
|---|---|
| **ADR** | 037 |
| **Title** | Agent-Extension Bundling and Package Layer Rationalization |
| **Status** | Superseded by [ADR-039](ADR-039-principal-model.md) / [ADR-041](ADR-041-principal-as-container.md) — `peko agent push/pull` and the standalone `.agent` bundle were retired in favor of `.principal` packages |
| **Date** | 2026-06-10 |
| **Last Updated** | 2026-06-10 |
| **Implemented** | Yes |
| **Author** | Kimi Code CLI |
| **Reviewers** | TBD |
| **Depends On** | ADR-017 (Unified Extension Architecture), ADR-024 (Unified Extension Manifest), ADR-027 (Unified Packaging), ADR-036 (Extension Developer Experience) |
| **Related** | ADR-031 (Agent-Team Membership) |

---

## Context

`peko agent push` and `peko agent pull` work for standalone agents, but they fail silently when the agent depends on extensions. The recipient imports the agent config, identity, workspace, and sessions correctly, but the enabled extensions are missing. When the agent runs, tool calls and prompt sections provided by those extensions are unavailable, producing a broken agent with no clear error message.

This is the same problem that `peko team push` / `peko team pull` already solved via `TeamAgentIndex.extensions: Vec<ExtensionRef>` and `ensure_extensions_for_team()`. Standalone agent packaging has not yet received the same treatment.

Separately, the current `.agent` package format defines standalone layers for `skills/` and `mcp/`. After ADR-017 and ADR-024, both skills and MCP servers are first-class extension types managed by the extension framework. Treating them as special-case layers is conceptually inconsistent, complicates the packager, and leaves the `mcp/` layer only half-implemented (the `mcp_config_path` export option is currently unused in `Packager::collect_files`).

### Current Layer Semantics (Problematic)

| Layer | Rationale in ADR-027 | Extension-Architecture View |
|---|---|---|
| `config/` | `agent.toml` — single source of truth | ✅ Legitimate — agent-scoped behavior config |
| `identity/` | DID document + keys | ✅ Legitimate — agent-scoped identity |
| `workspace/` | `SYSTEM.md`, `AGENTS.md`, etc. | ✅ Legitimate — agent-scoped user files |
| `sessions/` | Conversation history | ✅ Legitimate — agent-scoped runtime data |
| `skills/` | `SKILL.md` files | ⚠️ Redundant — skills are `skill` extensions under ADR-024 |
| `mcp/` | MCP server configs | ⚠️ Redundant — MCP servers are `mcp` extensions under ADR-024 |

The `skills/` layer predates the extension framework. It copies raw `SKILL.md` files from a global skills directory into the package, bypassing extension installation, versioning, and dependency resolution. The `mcp/` layer was added speculatively but is not actually populated during export (`Packager::collect_files` never reads `self.mcp_config_path`).

Under the unified extension architecture, the correct way to bundle a skill or MCP server with an agent is the same as for a universal tool or gateway: declare it in `extensions.enabled`, resolve it to an installed extension, and either (a) record a registry reference for auto-pull or (b) embed the extension package itself.

---

## Decision

### 1. Track Extension Dependencies in `AgentManifest`

Add an `extensions` field to `AgentManifest` that records which non-built-in extensions the agent requires and where to pull them from:

```toml
[agent]
name = "researcher"
version = "1.0.0"
export_format = "1.2"

[[extensions]]
id = "docker-skill"
registry_ref = "pekohub.com/extensions/docker-skill:latest"

[[extensions]]
id = "filesystem-mcp"
registry_ref = "pekohub.com/extensions/filesystem-mcp:v1.2.0"
```

- Reuse `ExtensionRef { id, registry_ref }` from the team packaging module (relocate to `src/portable/types.rs` if needed to avoid circular dependencies).
- Populate this list during `peko agent push` / `peko agent export` by reading `config.extensions.enabled`, skipping `builtin:*` entries and wildcards, and resolving each remaining entry via `ExtensionManager::resolve_tool_name()`.
- Only extensions with a known `registry_ref` (recorded when they were originally pulled from a registry) are included. Locally-installed extensions without a registry ref are omitted with a warning.

### 2. Auto-Install Missing Extensions on `peko agent pull`

After importing the agent, read `manifest.extensions` and `config.extensions.enabled`, then ensure each referenced extension is installed:

- If already installed → no-op.
- If missing → call the existing `handle_ext_pull()` path (which already handles recursive dependency resolution, download, and IPC install).
- Failures are reported as warnings; the agent import itself succeeds. This matches the behavior of `ensure_extensions_for_team()`.

CLI output (non-JSON):

```
📥 Imported 'researcher'
   Config: ~/.peko/agents/researcher/config.toml
   Extensions: 2 pulled, 1 already present, 0 failed
```

### 3. Deprecate `skills/` and `mcp/` as Standalone Layers

The `skills/` and `mcp/` layers are deprecated. They remain readable for backward compatibility during import, but new exports must not produce them.

| Layer | Status | Action |
|---|---|---|
| `config/` | Retained | No change |
| `identity/` | Retained | No change |
| `workspace/` | Retained | No change |
| `sessions/` | Retained | No change |
| `skills/` | **Deprecated** | Do not emit on export; still accept on import for legacy packages |
| `mcp/` | **Deprecated** | Do not emit on export; still accept on import for legacy packages |
| `extensions/` | **New** | Optional layer containing embedded extension packages (see §4) |

**Rationale:**
- Skills are `skill` extensions under ADR-024 Tier 1 (`SKILL.md`). They should be discovered, loaded, and versioned through the extension manager like any other extension.
- MCP servers are `mcp` extensions under ADR-024 (either Tier 1 `server.json` or Tier 2 `extension_type: "mcp"`). Their configuration lives in the extension directory or in `agent.toml`'s `extensions` section, not in a separate package layer.
- Removing these special-case layers simplifies `Packager::collect_files`, `AgentLayers`, `LayerType`, and the registry layer conversion logic.

### 4. Introduce Optional `extensions/` Layer for Composite Bundles

For air-gapped or private-extension sharing, support embedding full extension packages inside the `.agent` file:

```
my-agent.agent (tar.gz)
├── manifest.toml
├── config/
├── identity/
├── workspace/
├── sessions/
└── extensions/              # NEW — optional composite bundle layer
    ├── docker-skill/
    │   ├── manifest.toml    # ExtensionPackageManifest
    │   └── extension/
    │       ├── manifest.yaml
    │       └── SKILL.md
    └── filesystem-mcp/
        ├── manifest.toml
        └── extension/
            ├── server.json
            └── ...
```

- Enabled via `peko agent push --with-extensions` (CLI flag to be added).
- Uses `ExtensionPackager::export_with_deps()` to bundle each enabled extension and its transitive dependencies.
- Adds `LayerType::Extensions` with media type `application/vnd.peko.layer.extensions.v1.tar+gzip`.
- On import, `Unpackager` extracts each embedded extension to a temp directory and installs it via the existing `ExtensionInstall` IPC path.

This is **optional**. The default behavior is registry-reference only (smaller packages, registry-resolved dependencies).

### 5. Bump Export Format to `1.2`

`AgentManifest.agent.export_format` becomes `"1.2"` to indicate:
- `extensions` field may be present.
- `skills/` and `mcp/` layers are no longer produced by current tooling.
- `extensions/` layer may be present.

---

## Reasoning

**Fix the silent failure first.** The immediate user pain is that pulling an agent with extensions produces a broken agent. Phase 1 (registry refs in manifest + auto-install on pull) fixes this with minimal code by reusing the existing team-extension machinery.

**Conceptual consistency.** ADR-017 unified all capabilities under the extension framework. Having special-case package layers for `skills` and `mcp` contradicts that unification and forces the packager to know about extension-type-specific file layouts.

**Smaller, simpler packages.** Most agents use a small number of registry-published extensions. Recording refs instead of embedding binaries keeps `.agent` files small and avoids duplicating extension code across every agent package.

**Offline support without bloat.** The optional `--with-extensions` composite bundle provides reproducibility and air-gapped use when needed, without making it the default.

**Backward compatibility.** Legacy `.agent` files with `skills/` and `mcp/` layers continue to import correctly. The deprecation only affects new exports.

---

## Tradeoffs Accepted

| Tradeoff | Mitigation |
|---|---|
| Locally-installed extensions without `registry_ref` are not auto-shared | Emit a clear warning at push time; user can either push the extension to a registry first or use `--with-extensions` |
| Removing `skills/` layer may break external tools that read `.agent` files | Keep import support; `export_format = 1.2` signals the new layout |
| `--with-extensions` bundles may be large | Opt-in flag; transitive deps are included only when `--with-deps` is also passed to the underlying extension packager |
| Auto-install on pull requires registry access | Same constraint already exists for `peko ext pull` and `peko team pull`; failures are warnings, not fatal errors |

---

## Alternatives Considered

**Keep `skills/` and `mcp/` layers and add an `extensions/` layer alongside them.** Rejected. It perpetuates the conceptual inconsistency and leaves the unused `mcp/` layer in the format forever. The extension framework is the single source of truth for capabilities; the package format should reflect that.

**Embed all extensions by default (no registry refs).** Rejected. It would make agent packages large, complicate versioning, and duplicate extension code. Registry refs are the right default for the common case where extensions are published.

**Use `agent.toml` alone to discover extensions on pull (no manifest field).** Rejected. `agent.toml` only contains `extensions.enabled`, which is a list of names/wildcards. It does not tell the puller which registry to pull from or what the canonical extension ID is. `ExtensionRef` in the manifest provides the necessary metadata.

**Make missing extensions a fatal error on pull.** Rejected. This would make agent pull brittle in partial-connectivity scenarios. Warning + best-effort install matches the team pull behavior and gives the user actionable feedback.

---

## Migration Path

### Phase 1: Registry References (Immediate Priority)

1. Add `extensions: Vec<ExtensionRef>` to `AgentManifest`.
2. Update `Packager::collect_files` to resolve extension refs from `config.extensions.enabled`.
3. Update `AgentService::export_agent` to pass the extension storage directory to the packager.
4. Update `handle_agent_pull` to call `ensure_extensions_for_agent()` after import.
5. Bump `export_format` to `"1.2"`.
6. Add integration tests covering:
   - Export produces `extensions` field.
   - Built-ins and wildcards are excluded.
   - Pull installs missing extensions.
   - Pull warns but succeeds when an extension cannot be pulled.

### Phase 2: Layer Deprecation

1. Remove `skills/` and `mcp/` emission from `Packager::collect_files`.
2. Remove `LayerType::Mcp` and `LayerType::Skills` from new exports (keep enum variants for reading legacy packages).
3. Update `AgentLayers` to make `skills` and `mcp` optional and deprecated.
4. Update `Unpackager::import_skills` to remain compatible with legacy packages but route new `extensions/` content through the extension installer.
5. Update ADR-027 to mark `skills/` and `mcp/` layers as deprecated.

### Phase 3: Composite `extensions/` Layer

1. Add `LayerType::Extensions` and `extensions/` prefix handling.
2. Add `--with-extensions` flag to `peko agent push`.
3. Wire `ExtensionPackager::export_with_deps` into agent export when flag is set.
4. Update `Unpackager` to install embedded extensions via IPC `ExtensionInstall`.
5. Add tests for round-trip export/import with embedded extensions.

### Phase 4: Documentation

1. Update `ADR-027` layer semantics table.
2. Update `EXTENSION_SYSTEM.md` with agent-extension bundling behavior.
3. Add user-facing docs for `peko agent push --with-extensions`.

---

## File Changes

### Modified Files

| File | Changes |
|---|---|
| `src/portable/manifest.rs` | Add `extensions: Vec<ExtensionRef>` to `AgentManifest`; add `extensions` field to `AgentLayers`; bump `export_format` to `1.2`; add tests |
| `src/portable/types.rs` | Add `LayerType::Extensions` with media type and dir name; host `ExtensionRef`; add deprecation comments to `Skills` and `Mcp` |
| `src/portable/packager.rs` | Add `with_extensions` to `ExportOptions`; add `embedded_extensions` to `Packager`; deprecate `skills_dir`/`mcp_config_path`/`tools_dir`; emit `extensions/` layer when flag set; add tests |
| `src/portable/unpackager.rs` | Add `import_embedded_extensions()` to install `.ext` packages from `extensions/` layer; add deprecation comment to `import_skills()` |
| `src/portable/registry.rs` | Handle `layers.extensions` in `export_package` |
| `src/portable/validation.rs` | Accept `1.2` as valid export format |
| `src/portable/mod.rs` | Update module doc to reflect deprecated layers |
| `src/portable/team_packager.rs` | Set `with_extensions: false` in agent export options used by team packaging |
| `src/portable/team_layer_builder.rs` | Minor import updates for `ExtensionRef` relocation |
| `src/portable/team_layer_reconstructor.rs` | Minor import updates for `ExtensionRef` relocation |
| `src/common/services/agent_service.rs` | Add `resolve_extension_refs()` and `build_embedded_extensions()` helpers; wire into `export_agent()` |
| `src/common/types/agent.rs` | Add `with_extensions: bool` to `AgentExportOptions` |
| `src/commands/agent.rs` | Add `--with-extensions` flag to `Export` and `Push` commands; wire through dispatch |
| `src/commands/agent/handlers.rs` | Add `ensure_extensions_for_agent()` helper; call after `import_agent()` in pull flow; update JSON/human output with extension status; handle `extensions` layer in registry manifest conversion |
| `src/commands/team.rs` | Import `ExtensionRef` from new location |
| `src/ipc/packet.rs` | Add `with_extensions` field to `AgentExport` request packet; update tests |
| `src/ipc/server.rs` | Wire `with_extensions` through IPC to `AgentService::export_agent()` |
| `docs/architecture/adr/ADR-027-unified-packaging.md` | Update layer semantics table, package format diagram, CLI examples |

### New Files

None — all logic was inlined into existing modules to keep the change set minimal. The `ensure_extensions_for_agent()` helper lives directly in `src/commands/agent/handlers.rs` rather than a separate `extension_resolver.rs` submodule. This can be extracted later if the logic grows.

---

## Success Criteria

- [x] `peko agent push` produces an `AgentManifest` with `extensions` populated for all enabled non-built-in extensions that have a `registry_ref`.
- [x] `peko agent pull` installs missing extensions before reporting success.
- [x] Built-in extensions (`builtin:*`) and wildcards are never listed in `manifest.extensions`.
- [x] Legacy `.agent` files with `skills/` and `mcp/` layers still import correctly.
- [x] New `.agent` files do not contain `skills/` or `mcp/` layers unless the export explicitly targets an older format.
- [x] `peko agent push --with-extensions` embeds extension packages in an `extensions/` layer.
- [x] Pulling an agent with an embedded `extensions/` layer installs those extensions locally.
- [x] All existing packaging integration tests continue to pass.
- [x] ADR-027 is updated to reflect deprecated layers.

---

## Implementation Progress

| Phase | Description | Status |
|---|---|---|
| Phase 1 | `AgentManifest.extensions` + auto-install on pull | ✅ Complete |
| Phase 2 | Deprecate `skills/` and `mcp/` layers | ✅ Complete |
| Phase 3 | Composite `extensions/` layer with `--with-extensions` | ✅ Complete |
| Phase 4 | Documentation (ADR-027 update, tests) | ✅ Complete |

### Key Implementation Details

**Extension reference resolution** (`src/common/services/agent_service.rs`):
- `resolve_extension_refs()` builds an `ExtensionManager` with all adapters, loads installed extensions, and resolves each enabled tool name to its canonical ID and `registry_ref`.
- Built-ins (`builtin:*`) and wildcards (`*`) are skipped.
- Extensions without a `registry_ref` (locally installed, never pulled from registry) are omitted with a warning.

**Auto-install on pull** (`src/commands/agent/handlers.rs`):
- `ensure_extensions_for_agent()` mirrors the team-side `ensure_extensions_for_team()` pattern.
- After `AgentService::import_agent()` completes, the temp `.agent` file is inspected via `portable::inspect_agent()` to read `manifest.extensions`.
- For each missing extension, `handle_ext_pull()` is called with dependency resolution enabled.
- Failures are collected and reported as warnings; the agent import itself always succeeds.

**Embedded extensions** (`src/common/services/agent_service.rs` + `src/portable/packager.rs`):
- When `with_extensions: true`, `build_embedded_extensions()` exports each referenced extension via `ExtensionPackager::export()` to a temp `.ext` file.
- The raw `.ext` bytes are stored in the packager as `extensions/{id}.ext`.
- `compute_layers()` groups these into the `extensions/` layer and records its digest in `AgentLayers.extensions`.

**Import of embedded extensions** (`src/portable/unpackager.rs`):
- `import_embedded_extensions()` detects `extensions/{id}.ext` files in the package.
- Each is written to a temp file, extracted via `ExtensionUnpackager::install()`, and then installed via `ExtensionManager::install()` with all adapters registered.
- Failures are logged but do not break the agent import.

---

## Known Limitations

| Limitation | Impact | Planned Resolution |
|---|---|---|
| `--with-extensions` embeds only direct dependencies, not transitive deps | Large agents with deep dependency trees may still need registry access | Use `ExtensionPackager::export_with_deps()` when dependency graph traversal is available at export time |
| Embedded extensions are stored as flat `.ext` files (`extensions/{id}.ext`), not extracted directories | Slightly less human-readable package contents, but simpler implementation | Acceptable tradeoff — `.ext` is the canonical extension package format |
| `import_embedded_extensions` builds a fresh `ExtensionManager` without reusing the daemon's global instance | Minor overhead; adapters are re-registered for each embedded extension | Could be optimized by passing the daemon's extension manager into the unpackager |
| No deduplication of embedded extensions across multiple agents in a team package | Each agent in a team may embed the same extension, inflating `.team` size | Team packaging does not yet use `--with-extensions`; when it does, shared extensions should be hoisted to a team-level `extensions/` directory |
| Registry manifest does not carry `extensions` metadata directly | The puller must read the temp `.agent` file's TOML manifest to get `ExtensionRef`s | This is by design — OCI registry manifests are layer-oriented; dependency metadata belongs in the agent manifest |
| `ExtensionCore` must be initialized for extension ref resolution and embedded export to work | CLI commands that run without the daemon (direct mode) may skip extension ref population | Warn and skip gracefully; user can still use `--with-extensions` if extensions are installed |

---

## Related Documents

- ADR-017: Unified Extension Architecture
- ADR-024: Unified Extension Manifest
- ADR-027: Unified Packaging
- ADR-031: Agent-Team Membership
- ADR-036: Extension Developer Experience
- `src/portable/manifest.rs`
- `src/portable/packager.rs`
- `src/commands/team.rs` (`ensure_extensions_for_team` — implementation template)
- `src/commands/ext.rs` (`handle_ext_pull` — reuse target)
