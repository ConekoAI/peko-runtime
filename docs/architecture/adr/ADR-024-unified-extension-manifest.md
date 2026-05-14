# ADR-024: Unified Extension Manifest

**Status**: Accepted  
**Date**: 2026-05-02  
**Last Updated**: 2026-05-02  
**Author**: Kimi Code CLI  
**Reviewers**: Kimi Code CLI (self-reviewed)  
**Depends On**: ADR-017 (Unified Extension Architecture)  
**Replaces / Supersedes**: File-name-based type detection for non-ecosystem-standard extension types

---

## Context

ADR-017 introduced the Unified Extension Architecture with type-specific adapters, each detecting extensions by checking for a **unique manifest file name**:

| Extension Type | Manifest File | Detection Method |
|---------------|---------------|------------------|
| `skill` | `SKILL.md` | File-name based |
| `universal-tool` | `manifest.json` | File-name based |
| `mcp` | `config.toml` / `config.json` | File-name based (custom detector) |
| `gateway` | `manifest.yaml` | File-name based (also `gateway_type` field) |
| `general` | `manifest.yaml` | File-name based |

This creates several problems:

**1. Ambiguous Detection**

What happens when a directory contains both `manifest.yaml` and `config.toml`? The current "first match wins" approach is non-deterministic and depends on adapter registration order:

```
my-extension/
├── manifest.yaml     ← gateway or general adapter? ambiguous!
├── config.toml       ← mcp adapter sees this first?
└── manifest.json     ← universal-tool adapter sees this first?
```

**2. Inconsistent Developer Experience**

Extension authors must remember which file name maps to which type. There is no single mental model:

```bash
# Why does a skill use SKILL.md but a gateway uses manifest.yaml?
# Why does MCP use config.toml but universal tools use manifest.json?
```

**3. Tooling Friction**

External tools (IDEs, validators, registries, CI pipelines) cannot determine an extension's type without implementing Pekobot's adapter-specific detection logic. A unified entry point would enable generic tooling.

**4. No Escape Hatch for Hybrid Extensions**

An extension that wants to combine capabilities (e.g., provide both tools and a gateway) cannot express this cleanly when each type is tied to a specific file name.

**5. Misalignment with MCP Ecosystem Standards**

The original architecture assumed MCP lacked ecosystem-wide standards and invented `config.toml` / `config.json` as Pekobot-specific MCP manifests. In reality, the MCP ecosystem has well-established conventions:

- **`mcpServers` JSON** — the de facto client configuration standard used by Claude Desktop, Kimi CLI, VS Code, Cursor, and others. Example: `claude_desktop_config.json` and `~/.kimi/mcp.json` both use `{"mcpServers": {"name": {"command": "...", "args": [...]}}}`.
- **`server.json`** — the official MCP Registry metadata format (`$schema: https://static.modelcontextprotocol.io/schemas/.../server.schema.json`) used with the `mcp-publisher` CLI for publishing servers.

Neither standard uses a directory-level `config.toml` or `config.json`. Pekobot's invented convention creates friction for extension authors familiar with the broader MCP ecosystem.

**6. `general` Adapter as the Natural Default**

When a `manifest.yaml` lacks any type-specific discriminator, the most reasonable interpretation is that it is a general-purpose extension with hooks. Falling back to `gateway` (because it happens to also use `manifest.yaml`) is arbitrary. A unified manifest with an explicit `extension_type` field allows `general` to be the natural, predictable default when no specific type is declared.

---

## Decision

We will introduce a **unified manifest convention** that consolidates all Pekobot-specific extension types under a single file name with an explicit `extension_type` field, while respecting true ecosystem standards.

### Detection Hierarchy

```
┌─────────────────────────────────────────────────────────────────────┐
│                    EXTENSION DETECTION HIERARCHY                    │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  TIER 1: Ecosystem Standards (retained, file-name based)           │
│  ├── SKILL.md          → skill adapter                             │
│  │   └── Rationale: Established convention in LLM tooling          │
│  │       ecosystem (OpenClaw, Claude Code, etc.)                   │
│  │                                                                 │
│  ├── server.json       → mcp adapter (read as registry metadata)   │
│  │   └── Rationale: Official MCP Registry standard. Contains       │
│  │       name, version, packages[], transport, etc.                │
│  │                                                                 │
│  TIER 2: Unified Manifest (new primary path for Pekobot-specific)  │
│  ├── manifest.yaml     → read "extension_type" field               │
│  │   ├── "universal-tool" → universal-tool adapter                 │
│  │   ├── "mcp"            → mcp adapter (Pekobot wrapper/bundle)   │
│  │   ├── "gateway"        → gateway adapter                        │
│  │   ├── "general"        → general adapter                        │
│  │   └── "custom:*"       → custom adapters                        │
│  │                                                                 │
│  TIER 3: Legacy Fallback (deprecated, with warnings)               │
│  ├── manifest.json     → universal-tool adapter (warn)             │
│  ├── config.toml/json  → mcp adapter (warn)                        │
│  └── manifest.yaml     → general adapter (warn, if no ext_type)    │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

### The Unified Manifest Format

All non-skill, non-`server.json` extensions use `manifest.yaml` with a required `extension_type` field.

### File Format

`manifest.yaml` is **pure YAML** — a single document, no frontmatter delimiters required. The `---` shown in examples is optional YAML document-start marker, not frontmatter:

```yaml
id: "my-calculator"
name: "My Calculator"
version: "1.0.0"
description: "A simple calculator tool"
extension_type: "universal-tool"
# ... type-specific fields below
```

> **Note:** Unlike `SKILL.md`, `manifest.yaml` does **not** use frontmatter-with-body format. It is parsed entirely as YAML. If authors want to include documentation, they should use a separate `README.md` or `docs/` directory.

### Manifest Examples by Type

#### Universal Tool

```yaml
id: "calculator"
name: "Calculator"
version: "1.0.0"
description: "Perform arithmetic calculations"
extension_type: "universal-tool"
parameters:
  type: object
  properties:
    expression:
      type: string
      description: "Mathematical expression to evaluate"
  required: [expression]
```

#### MCP Server (Pekobot Wrapper / Bundle)

For Pekobot-specific MCP bundles that wrap or compose standard MCP servers with additional hooks:

```yaml
id: "filesystem"
name: "Filesystem MCP Server"
version: "1.0.0"
description: "Access and manipulate the local filesystem"
extension_type: "mcp"
# Embed standard MCP configuration using the ecosystem mcpServers format
mcp_servers:
  filesystem:
    command: "npx"
    args: ["-y", "@modelcontextprotocol/server-filesystem", "/path"]
    auto_start: true
```

> **Note:** If an extension ships a bare `server.json` (official MCP Registry format) in its root, the MCP adapter should read it directly as **Tier 1** ecosystem standard metadata, bypassing the unified manifest entirely.

**When to use which MCP format:**

| Format | Use When |
|--------|----------|
| `server.json` | Your extension is a **pure MCP server** following the official MCP Registry standard. No Pekobot-specific hooks or configuration needed. |
| `manifest.yaml` + `extension_type: "mcp"` | Your extension is a **Pekobot-specific bundle** that wraps one or more MCP servers with additional hooks, custom lifecycle management, or Pekobot-specific metadata. |

#### Gateway

```yaml
id: "redis-gateway"
name: "Redis Pub/Sub Gateway"
version: "1.0.0"
description: "Redis-based pub/sub messaging"
extension_type: "gateway"
gateway_type: "pubsub"
config:
  redis_url: "redis://localhost:6379"
  channels:
    - "peko:events"
    - "peko:commands"
hooks:
  - point: "agent.init"
    handler: "init_redis"
  - point: "agent.shutdown"
    handler: "cleanup_redis"
```

> **Note:** `gateway_type` remains a **required type-specific field** within the gateway adapter (e.g., `"pubsub"`, `"websocket"`, `"grpc"`). It is not the primary detection mechanism — `extension_type: "gateway"` is — but it is still required for the gateway adapter to know which transport to initialize.

#### General Extension

```yaml
id: "advanced-deploy-helper"
name: "Advanced Deployment Helper"
version: "1.0.0"
description: "Multi-hook deployment automation"
extension_type: "general"
hooks:
  - point: "prompt.system_section"
    section: "deployment"
    priority: 100
    handler: "generate_deployment_guide"
  - point: "tool.execute"
    tool_name: "deploy:*"
    handler: "handle_deploy_tool"
  - point: "event.subscribe"
    topic_pattern: "instance.created"
    handler: "on_instance_created"
```

---

## Reasoning

**Explicit Over Implicit.** Requiring `extension_type` in `manifest.yaml` eliminates ambiguity. There is no "first match wins" — the manifest declares what it is.

**Single Mental Model for Pekobot-Specific Extensions.** After this ADR, extension authors only need to remember:
- Skills → `SKILL.md` (ecosystem convention)
- Bare MCP servers → `server.json` (MCP Registry standard)
- Everything else → `manifest.yaml` with `extension_type`

The `general` adapter is the natural default: if you write a `manifest.yaml` with hooks and no specific type, `extension_type: "general"` is what you want.

**Respect Ecosystem Standards.** `server.json` is the official MCP Registry metadata format. By elevating it to Tier 1, Pekobot extensions that are pure MCP servers can be consumed by other MCP-aware tools without translation layers.

**Dockerfile parallel.** This mirrors how Docker works: `Dockerfile` is the standard, but `docker-compose.yml` is a separate, explicitly-typed file. There is no confusion about which is which.

**Tooling Friendly.** A registry, IDE plugin, or CI validator can inspect any extension by reading one file:

```python
def inspect_extension(path: Path) -> ExtensionInfo:
    skill_md = path / "SKILL.md"
    server_json = path / "server.json"
    manifest_yaml = path / "manifest.yaml"
    
    if skill_md.exists():
        return parse_skill(skill_md)
    
    if server_json.exists():
        return parse_mcp_server_json(server_json)
    
    if manifest_yaml.exists():
        manifest = yaml.safe_load(manifest_yaml.read_text())
        ext_type = manifest.get("extension_type")
        if ext_type is None:
            # Legacy fallback: warn and treat as general
            emit_deprecation_warning(
                "manifest.yaml without extension_type is deprecated; "
                "use extension_type: 'general' or another explicit type"
            )
            ext_type = "general"
        return ExtensionInfo(type=ext_type, manifest=manifest)
    
    # Legacy fallback (deprecated)
    return try_legacy_detection(path)
```

**Future-Proof.** New extension types only need a new `extension_type` value. No new file name conventions to learn.

**Custom Adapters.** Adapters can register custom type strings using the `custom:` prefix (e.g., `extension_type: "custom:my-org/proprietary-type"`). The prefix ensures custom types never collide with built-in types. The adapter registry is responsible for routing `custom:*` types to the appropriate adapter. This is an extension point for third-party or internal adapters without modifying core Pekobot code.

**Hybrid Extensions.** A single `manifest.yaml` can theoretically declare multiple `extension_type` values (future enhancement), or a meta-adapter can compose multiple adapters from one manifest. For now, `extension_type` is a single string; arrays or composite types are reserved for future ADRs.

---

## Tradeoffs Accepted

**Migration Cost.** Existing extensions using `manifest.json`, `config.toml`, or untyped `manifest.yaml` must add `extension_type: "..."` and rename/restructure. Mitigated by:
- Legacy fallback with deprecation warnings (Tier 3)
- Untyped `manifest.yaml` falls back to `general` adapter (the natural default)
- Migration command: `peko ext migrate <path>`

**Breaking Change for Universal Tools.** `manifest.json` is currently the standard for universal tools. Changing to `manifest.yaml` with YAML frontmatter is a breaking change. Mitigated by:
- `manifest.json` remains as a deprecated fallback for one major version
- Clear migration guide and automated converter

**More Verbose for Simple Tools.** A universal tool that previously needed only a `manifest.json` now needs YAML frontmatter with `extension_type`. Mitigated by:
- The additional fields (`id`, `name`, `version`, `description`) were already best practice
- Template generators (`peko ext init --type universal-tool`) can scaffold the file

---

## Alternatives Considered

**Keep Current File-Name-Based Detection.** Rejected. The "first match wins" ambiguity, inconsistent UX, and tooling friction compound as more extension types are added.

**Unify Everything Including SKILL.md.** Rejected. `SKILL.md` is an established ecosystem convention. Forcing skills into `manifest.yaml` would alienate users familiar with OpenClaw, Claude Code, and similar tools. The two-tier approach (ecosystem standard + unified manifest) is the right balance.

**Treat `server.json` as Tier 2 (unified manifest) instead of Tier 1.** Rejected. `server.json` is an official, schema-backed standard maintained by the MCP project. Moving it under Pekobot's `manifest.yaml` would force MCP-native extensions to maintain two metadata files and would break compatibility with the MCP Registry and `mcp-publisher` tooling.

**Use `peko.yaml` as Unified File Name.** Rejected. `manifest.yaml` is more generic and recognizable. `peko.yaml` is Pekobot-specific and would not help external tooling. However, `peko.yaml` may be considered as a future alias.

**Use `manifest.yaml` with Frontmatter.** Rejected. Frontmatter (YAML between `---` delimiters followed by a body) is appropriate for `SKILL.md` because skills are primarily documentation. Extension manifests are primarily structured configuration. Requiring frontmatter parsing adds complexity for tools that just want to `yaml.safe_load()`. The `---` in examples is an optional YAML document-start marker, not frontmatter.

**Support Both `manifest.yaml` and `manifest.json` for Unified Format.** Considered. JSON does not natively support frontmatter (the `---` delimiter), which is useful for separating metadata from documentation. YAML is the unified format; JSON remains a legacy fallback for universal tools.

---

## Migration Path

### Phase 1: Unified Manifest Support (Immediate)

1. Update `ExtensionManager::detect_extension_type()` to implement the three-tier hierarchy.
2. Update all non-skill adapters to accept `manifest.yaml` with `extension_type`:
   - `UniversalToolAdapter`: Support `manifest.yaml` with `extension_type: "universal-tool"`
   - `McpAdapter`: Support `manifest.yaml` with `extension_type: "mcp"`; also support `server.json` as Tier 1 ecosystem standard
   - `GatewayAdapter`: Require `extension_type: "gateway"` in `manifest.yaml`
   - `GeneralExtensionAdapter`: Require `extension_type: "general"` in `manifest.yaml`
3. Add deprecation warnings when legacy formats are detected.

### Phase 2: Legacy Deprecation (Next Minor Version)

1. Emit warnings: `"manifest.json is deprecated; use manifest.yaml with extension_type: 'universal-tool'"`
2. Emit warnings: `"config.toml is deprecated; use manifest.yaml with extension_type: 'mcp' or ship a server.json for bare MCP servers"`
3. Emit warnings: `"manifest.yaml without extension_type is deprecated; add extension_type: 'general' (or the appropriate type)"`
4. Update documentation and examples to use unified manifest.

### Phase 3: Legacy Removal (Next Major Version)

1. Remove `manifest.json`, `config.toml`, and untyped `manifest.yaml` detection.
2. Only `SKILL.md`, `server.json`, and typed `manifest.yaml` are supported.

---

## File Changes

### Modified Files

| File | Changes |
|------|---------|
| `src/extensions/manager/mod.rs` | Update `detect_extension_type()` and `detect_extension_type_string()` with three-tier hierarchy |
| `src/extensions/adapters/mod.rs` | Add `extract_extension_type_from_yaml()` helper; update `ManifestFormat` docs |
| `src/extensions/adapters/universal_tool_adapter.rs` | Support `manifest.yaml` with `extension_type: "universal-tool"`; keep `manifest.json` as deprecated fallback |
| `src/extensions/adapters/mcp_adapter.rs` | Support `server.json` as Tier 1; support `manifest.yaml` with `extension_type: "mcp"`; keep `config.toml`/`config.json` as deprecated fallback |
| `src/extensions/adapters/gateway_adapter.rs` | Require `extension_type: "gateway"` in `manifest.yaml`; `gateway_type` remains required as transport discriminator |
| `src/extensions/adapters/general_adapter.rs` | Require `extension_type: "general"` in `manifest.yaml` |
| `src/extensions/adapters/skill_adapter.rs` | No changes (ecosystem standard, exempt) |
| `docs/architecture/adr/ADR-017.md` | Add note referencing this ADR for manifest conventions |

### New Files

| File | Purpose |
|------|---------|
| `docs/migration/UNIFIED_MANIFEST_MIGRATION.md` | Step-by-step migration guide for extension authors |

---

## Consequences

### Positive

- **Unambiguous detection.** `extension_type` explicitly declares the adapter; no "first match wins" ambiguity.
- **Single mental model.** Extension authors learn one pattern: `manifest.yaml` + `extension_type` for everything except skills and bare MCP servers.
- **Ecosystem compatible.** Pure MCP servers can ship `server.json` and be understood by Pekobot and the broader MCP ecosystem (Registry, Claude Desktop, Kimi CLI, etc.).
- **Tooling friendly.** External tools can inspect any extension by reading `manifest.yaml` or `server.json` and checking one field.
- **Future-proof.** New extension types need only a new `extension_type` string.
- **Self-documenting directories.** A `manifest.yaml` or `server.json` immediately signals "this is a Pekobot extension."

### Negative / Risks

| Risk | Mitigation |
|------|------------|
| Breaking change for existing extensions | Tier 3 legacy fallback with deprecation warnings; migration guide |
| `manifest.json` universal tools need migration | Automated converter; `manifest.json` supported as deprecated fallback |
| More verbose for simple tools | Template generators (`peko ext init`) scaffold the file |
| Adapter registration order still matters for legacy fallback | Legacy fallback is temporary; unified manifest is order-independent; `general` is the natural default for untyped manifests |

---

## Success Criteria

- [x] `manifest.yaml` with `extension_type` works for all non-skill, non-MCP-registry extension types.
- [x] `SKILL.md` continues to work unchanged (ecosystem standard preserved).
- [x] `server.json` is detected as MCP Tier 1 without requiring `extension_type`.
- [x] Legacy formats (`manifest.json`, `config.toml`) work with deprecation warnings.
- [x] Untyped `manifest.yaml` falls back to `general` adapter with deprecation warning.
- [x] `peko ext install` correctly routes all extension types via the three-tier hierarchy.
- [x] `peko ext list` shows correct types for unified-manifest and `server.json` extensions.
- [ ] Documentation and examples updated to use unified manifest. *(Pending: examples/ directory update)*
- [ ] Migration guide published. *(Pending: `docs/migration/UNIFIED_MANIFEST_MIGRATION.md`)*
- [x] `id` field uniqueness is validated per-extension (no global uniqueness requirement enforced at this layer).

---

## Related Documents

- ADR-017: Unified Extension Architecture
- `src/extensions/manager/mod.rs` — ExtensionManager detection logic
- `src/extensions/adapters/mod.rs` — Adapter trait and ManifestFormat
- `docs/migration/MIGRATION-EXTENSIONS-2.0.md` — Extension system migration plan
- [MCP Registry `server.json` Schema](https://github.com/modelcontextprotocol/registry/blob/main/docs/reference/server-json/server.schema.json)
- [MCP Documentation](https://modelcontextprotocol.io/)
