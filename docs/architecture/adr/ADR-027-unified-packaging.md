# ADR-027: Unified Packaging System

**Status**: Accepted  
**Date**: 2026-05-08  
**Last Updated**: 2026-05-08  
**Author**: Core team  
**Reviewers**: Core team  
**Depends On**: ADR-017 (Unified Extension Architecture)  
**Replaces / Supersedes**: `src/image/` module, `ImageManifest` JSON format, top-level `build`/`push`/`pull` CLI commands

---

## Context

Pekobot v0.1.0 had **two parallel packaging systems** that created confusion:

| System | Format | Purpose | Location |
|--------|--------|---------|----------|
| **Portable** | `.agent` tar.gz with TOML manifest | User export/import | `src/portable/` |
| **Image** | `ImageManifest` JSON + content-addressable layers | Registry push/pull | `src/image/` |

Problems with this split:

1. **No unified mental model**: Users couldn't `build` an image and then `import` it — the formats were incompatible.
2. **Competing sources of truth**: `AgentManifest` duplicated `capabilities`, `tools`, `mcp`, `tool_sources` from `agent.toml`.
3. **Dead abstractions**: `src/image/` had zero production consumers — beautiful code with no users.
4. **Confusing CLI**: `pekobot build`, `pekobot push`, `pekobot pull` were top-level commands, while `pekobot agent export`/`import` lived under `agent`.
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
4. **Local registry store**: `AgentRegistry` provides content-addressable layer storage at `~/.pekobot/registry/`.
5. **Unified CLI**: All packaging commands live under `pekobot agent` or `pekobot ext`. No top-level `build`/`push`/`pull`.
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

- **Mock registry is Python**: The test fixture is a FastAPI server in `e2e_tests/packaging/mock_registry/main.py`, not a Rust in-memory server. This is acceptable because it's test-only infrastructure.

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
├── skills/
│   └── {name}/
│       └── SKILL.md
├── workspace/
├── sessions/
└── mcp/
```

**`manifest.toml` schema (clean — packaging only)**:

```toml
[agent]
name = "researcher"
version = "1.0.0"
description = "A research assistant agent"
created_at = "2026-05-08T10:00:00Z"
export_format = "2.0"
pekobot_version = "0.1.0"
did = "did:pekobot:local:abc123..."

[identity]
key_algorithm = "ed25519"
encrypted = false

[layers]
config = "sha256:abc123..."
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

### Layer Semantics

| Layer | Source Files | Optional | Contains Behavior Config? |
|-------|-------------|----------|---------------------------|
| `config` | `config/agent.toml` | No | ✅ Yes — agent.toml is the SSOT |
| `identity` | `identity/did.json`, `identity/keys.json` | No | ❌ No |
| `skills` | `skills/**` | Yes | ❌ No |
| `workspace` | `workspace/**` | Yes | ❌ No |
| `sessions` | `sessions/**` | Yes | ❌ No |
| `mcp` | `mcp/**` | Yes | ❌ No |

### Local Registry Store

```
~/.pekobot/registry/
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
pekobot agent build <path> -t <name:tag> [--json]
pekobot agent export <name> -o <file.agent> [--no-sessions] [--no-workspace]
pekobot agent import <file.agent> [--name <new-name>] [--force]
pekobot agent inspect <file.agent>
pekobot agent push <local-tag> <registry-ref>
pekobot agent pull <registry-ref>

pekobot team export <name> -o <file.team> [--no-sessions]
pekobot team import <file.team> [--name <new-name>] [--force]
pekobot team deploy <team.toml>

pekobot ext install <path>
pekobot ext export <id> -o <file.ext>
pekobot ext list
```

**Removed commands**:
- `pekobot build <path>` → `pekobot agent build`
- `pekobot push <local> <remote>` → `pekobot agent push`
- `pekobot pull <registry-ref>` → `pekobot agent pull`

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

A Python-based FastAPI mock registry server is provided for integration testing:

```bash
python e2e_tests/packaging/mock_registry/main.py --port 18765
```

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
| `pekobot validate <path>` | Partially covered by `inspect` |
| Extension source references (GitHub, URL, MCP) | Complex, not critical for v1.0 |
| Extension registry push/pull | No protocol defined |
| Team definition registry push/pull | No protocol defined |
| Signing and encryption | Security for shared packages |
| Multi-arch manifest support | Platform-specific binaries |
| Content deduplication across agents | Storage optimization |
| `pekobot diff <agent-a> <agent-b>` | Debugging tool |

---

## References

- `DATA_MODEL.md` §6–§9, §14 — Package format schemas
- `AGENTS.md` — Architecture overview with merged `src/portable/`
- `CHANGELOG.md` — Packaging release notes under v0.1.0
- `docs/phase1/packaging/Implementation_Plan.md` — Detailed task breakdown (superseded by this ADR)
- `docs/phase1/packaging/Packaging_Spec.md` — Full specification (superseded by this ADR)
