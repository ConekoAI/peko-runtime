# ADR-027: Unified Packaging System

**Status**: Superseded by [ADR-039](ADR-039-principal-model.md) and [ADR-041](ADR-041-principal-as-container.md)  
**Date**: 2026-05-08  

> **Supersession note:** the `.agent` / `.team` package formats and the `peko agent` / `peko team` push/pull surface described in this ADR were retired in favor of `.principal` packages. Kept for historical context.
**Last Updated**: 2026-05-08  
**Author**: Core team  
**Reviewers**: Core team  
**Depends On**: ADR-017 (Unified Extension Architecture)  
**Replaces / Supersedes**: `src/image/` module, `ImageManifest` JSON format, top-level `build`/`push`/`pull` CLI commands

---

## Context

Peko v0.1.0 had **two parallel packaging systems** that created confusion:

| System | Format | Purpose | Location |
|--------|--------|---------|----------|
| **Portable** | `.agent` tar.gz with TOML manifest | User export/import | `src/portable/` |
| **Image** | `ImageManifest` JSON + content-addressable layers | Registry push/pull | `src/image/` |

Problems with this split:

1. **No unified mental model**: Users couldn't `build` an image and then `import` it — the formats were incompatible.
2. **Competing sources of truth**: `AgentManifest` duplicated `capabilities`, `tools`, `mcp`, `tool_sources` from `agent.toml`.
3. **Dead abstractions**: `src/image/` had zero production consumers — beautiful code with no users.
4. **Confusing CLI**: `peko build`, `peko push`, `peko pull` were top-level commands, while `peko agent export`/`import` lived under `agent`.
5. **Team packages lacked integrity**: `.team` exports had no checksum validation.
6. **No extension packaging**: Extensions could only be installed from local paths, not distributed as `.ext` bundles.

Additionally, the pre-extension `capabilities` concept (`AgentCapability`, `TeamCapabilityConfig`, `CapabilitiesConfig`) was declarative but never enforced. The extension framework's `extensions.enabled` whitelist (ADR-017) is the actual enforcement mechanism. Having both was confusing and redundant.

---

## Decision

Merge `src/image/` into `src/portable/`, creating a **single `.agent` format** that serves all use cases: export/import, directory builds, and registry push/pull. Strip `AgentManifest` of all behavior configuration — it contains **packaging metadata only**. Remove the `capabilities` concept entirely.

### Key Decisions

1. **Unified `.agent` format**: One format for build, export, push, pull, and import.
2. **Clean Manifest**: `AgentManifest` contains only packaging metadata (name, version, layers, checksums). Agent behavior lives in `agent.toml` inside the `config` layer.
3. **Content-addressable layers**: `.agent` gains SHA-256 layer digests for deduplication and incremental push/pull.
4. **Local registry store**: `AgentRegistry` provides content-addressable layer storage at `~/.peko/registry/`.
5. **Unified CLI**: All packaging commands live under `peko agent` or `peko ext`. No top-level `build`/`push`/`pull`.
6. **Team checksums**: `.team` packages include SHA-256 checksums for all files; import validates them.
7. **Extension bundles**: `.ext` packages enable offline extension distribution.
8. **Capabilities removed**: `AgentCapability`, `TeamCapabilityConfig`, `CapabilitiesConfig` deleted. Extension framework is the single source of truth.

---

## Consequences

### Positive

- **Single mental model**: Users learn one format and one set of commands.
- **No competing sources of truth**: `agent.toml` is the single source of truth for agent behavior.
- **Registry efficiency**: Content-addressable layers enable deduplication and incremental transfer.
- **Data integrity**: SHA-256 checksums on all packages (`.agent`, `.team`, `.ext`).
- **Simpler codebase**: `src/image/` deleted (~5 files); no `capabilities` types to maintain.
- **Offline extension distribution**: `.ext` packages work in air-gapped environments.

### Negative

- **Breaking change**: `ImageManifest` JSON format is gone; registry wire format changed to JSON `RegistryManifest`.
- **Deferred features**: Base image inheritance, signing/encryption, extension source references (GitHub/URL/MCP) moved to Phase 2.

### Neutral

- **Mock registry is Python**: The test fixture ~~was~~ a FastAPI server in `e2e_tests/packaging/mock_registry/main.py`, not a Rust in-memory server. *(The mock_registry folder was deleted in Phase A; the Rust integration tests in `tests/pekohub_integration.rs` + `tests/registry_integration.rs` exercise the real pekohub fixture server in `pekohub/backend/tests/fixtures/server.ts` instead.)*

---

## Architecture

### Module Layout (After Merge)

```
src/
├── portable/               # UNIFIED — agent/team packaging + former image/
│   ├── mod.rs              # Re-exports
│   ├── manifest.rs         # Clean AgentManifest — packaging metadata only
│   ├── packager.rs         # Export agent to .agent
│   ├── unpackager.rs       # Import .agent
│   ├── ~~builder.rs~~      # ~~AgentBuilder~~ — removed; use Packager + export_agent
│   ├── registry.rs         # AgentRegistry — local content-addressable store
│   ├── types.rs            # ImageDigest, LayerType, LayerDigest
│   ├── team_packager.rs    # Export team to .team with checksums
│   ├── team_unpackager.rs  # Import .team with checksum validation
│   ├── validation.rs       # Checksum/format validation
│   └── crypto.rs           # AES-256-GCM + Argon2id (unwired)
│
├── registry/               # Remote registry client
│   ├── mod.rs
│   ├── client.rs           # RegistryClient — push/pull .agent layers
│   ├── config.rs           # Registry configuration and auth
│   └── manifest.rs         # RegistryManifest — JSON wire format
│
├── extension/
│   ├── manager/
│   │   ├── mod.rs
│   │   └── packaging.rs    # ExtensionPackager / ExtensionUnpackager
│   └── types/
│       ├── mod.rs
│       └── source.rs       # ExtensionSourceRef (deferred to Phase 2)
│
└── commands/
    ├── mod.rs              # Top-level Commands (Build/Push/Pull removed)
    ├── agent.rs            # AgentCommands with Export, Push, Pull subcommands
    └── ext.rs              # ExtCommands with Export subcommand
```

**Deleted**: `src/image/` (entire directory)

### `.agent` Package Format

```
my-agent.agent (gzip-compressed tar)
├── manifest.toml           # Agent metadata + layer digests + file checksums
├── identity/
│   ├── did.json
│   └── keys.json
├── config/
│   └── agent.toml          # Single source of truth for behavior
├── workspace/
├── sessions/
└── extensions/             # Optional — embedded .ext packages (ADR-037)
    └── {id}.ext
```

**Note:** Standalone `skills/` and `mcp/` layers were deprecated by
[ADR-037](ADR-037-agent-extension-bundling-and-layer-rationalization.md).
Skills and MCP servers are now managed as extensions and are declared in
`agent.toml`'s `extensions.enabled` list. Their dependency metadata is
recorded in `manifest.extensions` (a list of `ExtensionRef` structs) so
that `peko agent pull` can auto-install missing extensions. Legacy
packages that still contain `skills/` or `mcp/` layers can still be
imported, but new exports do not emit them.

**`manifest.toml` schema (clean — packaging only)**:

```toml
[agent]
name = "researcher"
version = "1.0.0"
description = "A research assistant agent"
created_at = "2026-05-08T10:00:00Z"
export_format = "1.2"     # ADR-037: extension dependency tracking
peko_version = "0.1.0"
did = "did:peko:local:abc123..."

[identity]
key_algorithm = "ed25519"
encrypted = false

[layers]
config = "sha256:abc123..."
identity = "sha256:def456..."
workspace = "sha256:jkl012..."
sessions = "sha256:mno345..."
extensions = "sha256:stu901..."   # Optional — embedded extensions layer

[[extensions]]              # ADR-037: dependency metadata
id = "docker-skill"
registry_ref = "pekohub.com/extensions/docker-skill:latest"

[[extensions]]
id = "filesystem-mcp"
registry_ref = "pekohub.com/extensions/filesystem-mcp:v1.2.0"

[packaging]
files = ["manifest.toml", "identity/did.json", "config/agent.toml", ...]
checksums = { "manifest.toml" = "sha256:...", ... }
compression = "gzip"
archive_format = "tar"
```

### Layer Semantics

| Layer | Source Files | Optional | Status | Contains Behavior Config? |
|-------|-------------|----------|--------|---------------------------|
| `config` | `config/agent.toml` | No | ✅ Active | ✅ Yes — agent.toml is the SSOT |
| `identity` | `identity/did.json`, `identity/keys.json` | No | ✅ Active | ❌ No |
| `workspace` | `workspace/**` | Yes | ✅ Active | ❌ No |
| `sessions` | `sessions/**` | Yes | ✅ Active | ❌ No |
| `extensions` | `extensions/*.ext` | Yes | ✅ Active (ADR-037) | ❌ No |
| `skills` | `skills/**` | Yes | ⚠️ Deprecated (ADR-037) | ❌ No |
| `mcp` | `mcp/**` | Yes | ⚠️ Deprecated (ADR-037) | ❌ No |

**Deprecation rationale (ADR-037):** Under the unified extension
architecture (ADR-017), skills are `skill` extensions and MCP servers are
`mcp` extensions. Treating them as special-case package layers was
inconsistent and left the `mcp/` layer only half-implemented. The
replacement is:

1. `agent.toml`'s `extensions.enabled` list declares which tools/skills/MCP
   servers the agent uses.
2. `AgentManifest.extensions` records `ExtensionRef { id, registry_ref }`
   for each non-built-in extension so that `peko agent pull` can
   auto-install missing extensions.
3. For air-gapped use, `peko agent push --with-extensions` embeds the
   actual `.ext` packages in the `extensions/` layer.

### Local Registry Store

```
~/.peko/registry/
├── layers/
│   └── sha256-abc123.../
│       └── layer.tar.gz
├── manifests/
│   └── sha256-xyz789.../
│       └── manifest.toml
└── tags/
    └── my-agent_v1.0       # file contains manifest digest
```

### Team Snapshot Format

```
my-team.team (gzip-compressed tar)
├── team/
│   ├── manifest.toml       # Team metadata + file checksums
│   └── team.toml           # Team runtime definition (optional)
├── agents/
│   └── {agent-name}/
│       ├── manifest.toml
│       ├── identity/
│       ├── config/
│       └── ...
└── shared/
    └── skills/
```

### Extension Package Format

```
docker-skill.ext (gzip-compressed tar)
├── manifest.toml           # Extension package metadata + checksums
└── extension/
    ├── manifest.yaml
    ├── SKILL.md
    └── ...
```

---

## CLI Commands (Unified)

```
peko agent build <path> -t <name:tag> [--json]
peko agent export <name> -o <file.agent> [--no-sessions] [--no-workspace]
peko agent import <file.agent> [--name <new-name>] [--force]
peko agent inspect <file.agent>
peko agent push <local-tag> <registry-ref>
peko agent pull <registry-ref>

peko team export <name> -o <file.team> [--no-sessions]
peko team import <file.team> [--name <new-name>] [--force]
peko team deploy <team.toml>

peko ext install <path>
peko ext export <id> -o <file.ext>
peko ext list
```

**Removed commands**:
- `peko build <path>` → `peko agent build`
- `peko push <local> <remote>` → `peko agent push`
- `peko pull <registry-ref>` → `peko agent pull`

---

## Registry Protocol

The registry protocol uses **JSON** as the manifest wire format (`RegistryManifest`). The local `AgentManifest` (inside `.agent` packages) remains TOML.

### Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/v2/` | GET | Registry capability check |
| `/v2/{name}/manifests/{reference}` | GET/PUT | Manifest pull/push (JSON) |
| `/v2/{name}/blobs/{digest}` | GET | Layer pull |
| `/v2/{name}/blobs/uploads/` | POST | Initiate layer upload |
| `/v2/{name}/blobs/uploads/{uuid}` | PUT | Complete layer upload |
| `/v2/{name}/blobs/{digest}` | HEAD | Layer existence check |

### Mock Registry Server

A Python-based FastAPI mock registry server ~~was~~ provided for integration testing:

```bash
# No longer present — the mock_registry folder was deleted in Phase A.
# python e2e_tests/packaging/mock_registry/main.py --port 18765
```

The Rust integration tests now exercise the real pekohub fixture server at `pekohub/backend/tests/fixtures/server.ts` instead, via the dual-mode `PekohubBackend::start()` harness in `tests/common/harness.rs`.

---

## Implementation Phases

| Phase | Focus | Status |
|-------|-------|--------|
| Phase 1 | Mock registry + CLI scaffolding | ✅ Complete |
| Phase 2 | Clean manifest + merge `src/image/` into `src/portable/` | ✅ Complete |
| Phase 3 | Registry push/pull with mock server | ✅ Complete |
| Phase 4 | ~~`agent build` command~~ → removed in favor of unified `export` | ✅ Removed |
| Phase 5 | Team checksums + `team.toml` | ✅ Complete |
| Phase 6 | `.ext` export | ✅ Complete |
| Phase 7 | Integration tests + docs | ✅ Complete |

---

## Test Coverage

| Test File | Tests | Status |
|-----------|-------|--------|
| `tests/packaging_integration.rs` | 3 | ✅ All pass |
| `tests/registry_integration.rs` | 4 | ✅ All pass |
| `tests/team_integration.rs` | 4 | ✅ All pass |
| `tests/extension_packaging.rs` | 5 | ✅ All pass |
| ~~`tests/build_integration.rs`~~ | — | ✅ Removed — merged into `packaging_integration.rs` |
| `cargo test --lib` | 970 | ✅ All pass |

---

## Deferred to Phase 2

| Feature | Rationale |
|---------|-----------|
| Base image inheritance | No clear consumer yet |
| `peko validate <path>` | Partially covered by `inspect` |
| Extension source references (GitHub, URL, MCP) | Complex, not critical for v1.0 |
| Extension registry push/pull | No protocol defined |
| Team definition registry push/pull | No protocol defined |
| Signing and encryption | Security for shared packages |
| Multi-arch manifest support | Platform-specific binaries |
| Content deduplication across agents | Storage optimization |
| `peko diff <agent-a> <agent-b>` | Debugging tool |

---

## References

- `DATA_MODEL.md` §6–§9, §14 — Package format schemas
- `AGENTS.md` — Architecture overview with merged `src/portable/`
- `CHANGELOG.md` — Packaging release notes under v0.1.0
- `docs/phase1/packaging/Implementation_Plan.md` — Detailed task breakdown (superseded by this ADR)
- `docs/phase1/packaging/Packaging_Spec.md` — Full specification (superseded by this ADR)
