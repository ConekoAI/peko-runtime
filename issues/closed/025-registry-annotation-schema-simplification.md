# Issue 025: Registry Annotation Schema Inconsistency — Simplify to OCI Standards + Peko-Specific Keys Only

**Status:** Closed (Implemented)
**Area:** Registry / Push Protocol / CLI-Backend Contract
**Affected:** `peko-runtime/src/registry/manifest.rs`, `pekohub/backend/src/routes/oci/manifests.ts`
**Resolution:** Implemented as proposed. All changes complete and tested.

---

## Resolution Summary

The annotation key mismatch was resolved by standardizing on flat OCI + Peko-specific annotation keys, removing the `dev.pekohub.metadata` JSON blob, and keeping `org.peko.*` keys only for identity fields needed for pull-side round-tripping.

## Changes Made

### Phase 1: Backend — pekohub/backend/src/routes/oci/manifests.ts

- Removed `dev.pekohub.metadata` JSON blob parsing
- Added `parseJsonAnnotation<T>()` helper for JSON-valued flat annotations
- Updated field extraction for upsert and update to read from flat annotation keys:
  - `org.opencontainers.image.description`, `org.opencontainers.image.authors`, `org.opencontainers.image.licenses`
  - `dev.pekohub.bundleType`, `dev.pekohub.extensionType`, `dev.pekohub.tags`, `dev.pekohub.categories`, `dev.pekohub.readme`, `dev.pekohub.hooks`, `dev.pekohub.compatibility`, `dev.pekohub.modelProviders`, `dev.pekohub.requiredMcpServers`
- Fixed Meilisearch indexing to use `bundle.hooks` consistently (was reading annotations directly)

### Phase 2: CLI — peko-runtime/src/registry/manifest.rs

- Added 12 new `#[serde(skip)]` discovery metadata fields: `description`, `author`, `license`, `bundle_type`, `extension_type`, `tags`, `categories`, `readme`, `hooks`, `compatibility`, `model_providers`, `required_mcp_servers`
- Added builder methods for all new fields
- Replaced `build_annotations()` to emit flat OCI + Peko-specific keys
- Replaced `apply_annotations()` to read from new flat keys for pull-side reconstruction
- **Kept** `org.peko.name`, `org.peko.version`, `org.peko.kind` in annotations for pull-side round-tripping (needed by `registry_to_agent_manifest()`)
- Updated tests: `test_annotation_roundtrip` now verifies roundtrip of identity + discovery fields; `test_flat_annotation_read` validates deserialization; fragile JSON string assertion replaced with value-based check

### Phase 3: CLI Push Sites

- **Agent push** (`handlers.rs`): Plumbs `bundle_type="agent"` and `description` from `AgentManifest.agent.description`
- **Team push** (`team.rs`): Plumbs `bundle_type="team"` and `description` from team service metadata
- **Extension push** (`ext.rs`): Plumbs `bundle_type="extension"`, `extension_type`, and `description` from `ExtensionManifest`

### Phase 4: Cleanup & Fixes

- `org.peko.ref`, `org.peko.digest`, `org.peko.createdAt`, `org.peko.source` removed from annotations (internal operational metadata)
- `test_annotation_roundtrip` updated to verify `org.peko.name/version/kind` are present and roundtrip correctly
- Backend test `creates extension bundle with hooks and compatibility metadata from flat annotations` updated to use flat annotations

## Final Annotation Contract

| Field | Annotation Key |
|-------|---------------|
| name | `org.peko.name` |
| version | `org.peko.version` |
| kind | `org.peko.kind` |
| description | `org.opencontainers.image.description` |
| author | `org.opencontainers.image.authors` |
| license | `org.opencontainers.image.licenses` |
| bundleType | `dev.pekohub.bundleType` |
| extensionType | `dev.pekohub.extensionType` |
| tags | `dev.pekohub.tags` |
| categories | `dev.pekohub.categories` |
| readme | `dev.pekohub.readme` |
| hooks | `dev.pekohub.hooks` |
| compatibility | `dev.pekohub.compatibility` |
| modelProviders | `dev.pekohub.modelProviders` |
| requiredMcpServers | `dev.pekohub.requiredMcpServers` |

## Verification

- `cargo test --lib registry::manifest` — 7 passed
- `cargo build --lib` — clean
- `tsc --noEmit` (backend) — clean