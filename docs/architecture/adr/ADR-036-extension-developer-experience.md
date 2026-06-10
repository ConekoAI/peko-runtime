# ADR-036: Extension Developer Experience — `peko ext init` and Semantic Validation

| Field | Value |
|---|---|
| **ADR** | 036 |
| **Title** | Extension Developer Experience — `peko ext init` and Semantic Validation |
| **Status** | Accepted |
| **Date** | 2026-06-10 |
| **Depends On** | ADR-024 (Unified Extension Manifest), ADR-025 (Gateway Extension Architecture), ADR-026 (Extension Lifecycle Separation), ADR-027 (Unified Packaging) |
| **Related** | ADR-017 (Unified Extension Architecture), EXTENSION_SYSTEM.md |

---

## Context

The Pekobot extension ecosystem has a mature **consumption layer** (`peko ext install`, `peko ext push`, `peko ext pull`, `peko ext export`) but a broken **creation layer**. Today, developers who want to build an extension must:

1. Read `EXTENSION_SYSTEM.md` and multiple ADRs to understand manifest formats.
2. Hand-write `manifest.yaml`, `SKILL.md`, or `server.json` from scratch.
3. Guess at the correct field names, hook points, and adapter-specific requirements.
4. Run `peko ext validate` only after writing the manifest — discovering errors late.
5. Manually create any stub handler code (Python, Node.js, etc.).

This is an **expert-only workflow**. It blocks community contributions and makes the "without extensions, agents are just chatbots" vision impossible.

### Current Gap Analysis

| Capability | Status | Severity |
|---|---|---|
| Extension manifest format (ADR-024) | ✅ Works | — |
| `peko ext export` / `peko ext push` | ✅ Works | — |
| `peko ext create/init` command | ❌ Missing | **High** |
| Extension scaffolding/templates | ❌ Missing | **High** |
| Semantic validation (beyond static manifest check) | ❌ Missing | **High** |
| Dependency resolution on push | ⚠️ Partial (local check only) | Medium |

### Why Now

- ADR-024 stabilized the unified manifest format (`manifest.yaml` + `extension_type`).
- ADR-025 and ADR-026 stabilized runtime lifecycle semantics (`start`/`stop` vs `enable`/`disable`).
- ADR-027 stabilized the `.ext` packaging format.
- The extension type system (`skill`, `mcp`, `universal-tool`, `gateway`, `general`) is now stable enough to template.

---

## Decision

### 1. Introduce `peko ext init <name> --type <type>`

A CLI command that scaffolds a working extension directory with:
- Correct manifest file for the type
- Type-specific stub code (when applicable)
- A `README.md` with usage instructions
- A `.gitignore`

**Supported types:**

| Type | Manifest File | Stub Code | Notes |
|---|---|---|---|
| `skill` | `SKILL.md` | No code | Frontmatter + instruction sections |
| `mcp` | `manifest.yaml` (wrapper) or `server.json` (bare) | Optional | Prompts for wrapper vs bare |
| `universal-tool` | `manifest.yaml` | `handler.py` or `handler.js` | JSON-over-stdio stub |
| `gateway` | `manifest.yaml` | `gateway.py` or `gateway.js` | GatewayPacket protocol stub |
| `general` | `manifest.yaml` | No code | Hook declarations + README examples |

**CLI signature:**

```bash
peko ext init <name> --type <TYPE> [OPTIONS]

Options:
  --output <DIR>       Output directory (default: ./<name>)
  --lang <LANG>        Language for stub code: python, javascript (default: python)
  --bare               For MCP: ship server.json instead of manifest.yaml wrapper
  --gateway-type <T>   For gateway: http, websocket, pubsub, out-of-process, external
```

**Example:**

```bash
$ peko ext init github-helper --type skill
Created skill extension 'github-helper' in ./github-helper
  SKILL.md          — edit your skill instructions
  README.md         — documentation for users

$ peko ext init calculator --type universal-tool --lang python
Created universal-tool extension 'calculator' in ./calculator
  manifest.yaml     — extension metadata and parameter schema
  handler.py        — implement your tool logic
  README.md         — documentation for users

$ peko ext init discord-bot --type gateway --lang javascript --gateway-type out-of-process
Created gateway extension 'discord-bot' in ./discord-bot
  manifest.yaml     — extension metadata and routing config
  gateway.js        — GatewayPacket protocol implementation
  README.md         — documentation for users
```

### 2. Template Engine Architecture

Templates are **embedded Rust strings** (via `include_str!`) stored in `src/extension/scaffold/templates/`. This avoids runtime file dependencies and ensures templates are version-locked with the binary.

```
src/extension/scaffold/
├── mod.rs              # ScaffoldEngine, public API
├── templates/
│   ├── skill/
│   │   └── SKILL.md
│   ├── mcp/
│   │   ├── manifest.yaml
│   │   └── server.json
│   ├── universal-tool/
│   │   ├── manifest.yaml
│   │   ├── handler.py
│   │   └── handler.js
│   ├── gateway/
│   │   ├── manifest.yaml
│   │   ├── gateway.py
│   │   └── gateway.js
│   └── general/
│       └── manifest.yaml
├── engine.rs           # Template rendering, variable substitution
└── validator.rs        # Post-scaffold validation
```

**Variable substitution uses simple `{{name}}` placeholders:**

```yaml
# manifest.yaml template
id: "{{id}}"
name: "{{name}}"
version: "1.0.0"
description: "{{description}}"
extension_type: "{{extension_type}}"
```

**Why not a full templating engine?** The templates are small and structurally simple. A custom `str::replace`-based renderer avoids adding dependencies like `handlebars` or `tera`. If templates become complex in the future, we can upgrade.

### 3. Semantic Validation Depth Levels

Extend `ExtensionValidationService` with three validation depths:

```rust
pub enum ValidationDepth {
    /// Syntax and required fields only (current behavior)
    Static,
    /// + Referenced files exist, commands are in $PATH, schemas are valid JSON
    Semantic,
    /// + Spawn runtime and verify health check (future, gated behind --functional)
    Functional,
}
```

**Semantic checks by type:**

| Type | Semantic Checks |
|---|---|
| `skill` | Frontmatter YAML is valid; at least one `#` heading exists |
| `mcp` | `server.json` schema validates against MCP Registry schema; `command` (if specified) exists in `$PATH` |
| `universal-tool` | `command` exists in `$PATH`; parameter schema is valid JSON Schema |
| `gateway` | `gateway_type` is known; `command` exists in `$PATH` (for out-of-process); `endpoint` is a valid URL (for external) |
| `general` | All hook points in `hooks` are valid; all referenced handler files exist |

**CLI integration:**

```bash
peko ext validate ./my-extension          # default: Static
peko ext validate ./my-extension --semantic
peko ext validate ./my-extension --functional  # future
```

### 4. Dependency Resolution on Push

Currently `peko ext push` exports a single extension with no dependency bundling. We add:

1. **`--with-deps` flag**: Bundle all transitive dependencies into the `.ext` package.
2. **Pre-push dependency check**: Warn if required dependencies are missing from the registry.
3. **`peko.lock` generation**: A lockfile recording exact versions of resolved dependencies, generated during `peko ext init` or `peko ext export`.

**Out of scope for this ADR:** Full package manager semantics (version constraint solving, conflict resolution). We only bundle and verify.

### 5. Gateway Type: Keep, Do Not Remove

A suggestion was made to remove the `gateway` extension type because "remote chat is now coordinated at the Pekohub-daemon level" (ADR-035). After analysis, **we reject this suggestion**:

- **Pekohub tunnel (ADR-035)** provides **remote connectivity** — it gets requests from the internet into the daemon.
- **Gateway extensions (ADR-025)** provide **platform channels** — they determine how users interact (Discord, Slack, HTTP webhook, TUI) and handle platform-specific formatting.

These are orthogonal. A user may access their agent via:
- Discord (gateway) + local network
- Discord (gateway) + Pekohub tunnel (remote)
- Web UI (gateway) + Pekohub tunnel (remote)
- Pekohub web chat (no gateway — direct tunnel)

**Action:** Keep `gateway` as a supported `extension_type`. Update documentation to clarify the boundary. Consider a rename to `channel` in a future ADR if confusion persists.

---

## Reasoning

**Lower the barrier to entry.** Hand-writing manifests requires reading 400+ lines of documentation. Templates reduce this to one command.

**Reduce errors.** Semantic validation catches mistakes at authoring time, not after push/install.

**Version-locked templates.** Embedding templates in the binary ensures `peko ext init` always produces manifests compatible with the current runtime.

**Minimal dependencies.** Simple string substitution avoids pulling in templating crates.

**Ecosystem growth.** Without easy creation, there are no extensions. Without extensions, agents are just chatbots.

---

## Tradeoffs Accepted

| Tradeoff | Mitigation |
|---|---|
| Templates may drift from adapter requirements | Templates are tested in CI; `peko ext validate` catches mismatches |
| Embedded templates bloat binary | Total template size < 50KB; negligible compared to runtime |
| Simple renderer limits template complexity | Templates are intentionally simple; upgrade path exists |
| Semantic validation requires spawning processes / network calls | `--semantic` is opt-in; `Static` remains the default |
| `--with-deps` bundling may create large packages | Only bundle direct dependencies; transitive deps resolved at install time |

---

## Migration Path

### Phase 1: Scaffold Engine + `peko ext init` (Immediate)

1. Create `src/extension/scaffold/` module with embedded templates.
2. Add `ExtCommands::Init` to CLI.
3. Implement `ScaffoldEngine::scaffold(type, name, options) -> Result<PathBuf>`.
4. Add unit tests for each template type.

### Phase 2: Semantic Validation (Short Term)

1. Extend `ExtensionValidationService::validate` with `ValidationDepth` parameter.
2. Implement semantic checks per extension type.
3. Update CLI `--validate` flags.
4. Add tests for each semantic check.

### Phase 3: Dependency Bundling on Push (Short Term)

1. Add `--with-deps` to `peko ext push`.
2. Implement transitive dependency collection in `ExtensionPackager`.
3. Generate `peko.lock` during init/export.
4. Update `ExtensionUnpackager` to install bundled dependencies.

### Phase 4: Documentation + E2E Tests (Short Term)

1. Update `EXTENSION_SYSTEM.md` with `peko ext init` examples.
2. Update `README.md` quickstart.
3. Add E2E test: `init -> validate -> install -> export -> push -> pull -> install` roundtrip.

---

## File Changes

### New Files

| File | Purpose |
|---|---|
| `src/extension/scaffold/mod.rs` | `ScaffoldEngine` public API |
| `src/extension/scaffold/engine.rs` | Template rendering and variable substitution |
| `src/extension/scaffold/templates/skill/SKILL.md` | Skill template |
| `src/extension/scaffold/templates/mcp/manifest.yaml` | MCP wrapper template |
| `src/extension/scaffold/templates/mcp/server.json` | Bare MCP server template |
| `src/extension/scaffold/templates/universal-tool/manifest.yaml` | Universal tool manifest template |
| `src/extension/scaffold/templates/universal-tool/handler.py` | Python stub handler |
| `src/extension/scaffold/templates/universal-tool/handler.js` | JavaScript stub handler |
| `src/extension/scaffold/templates/gateway/manifest.yaml` | Gateway manifest template |
| `src/extension/scaffold/templates/gateway/gateway.py` | Python GatewayPacket stub |
| `src/extension/scaffold/templates/gateway/gateway.js` | JavaScript GatewayPacket stub |
| `src/extension/scaffold/templates/general/manifest.yaml` | General extension manifest template |
| `src/extension/scaffold/templates/shared/README.md` | Shared README template |
| `src/extension/scaffold/templates/shared/.gitignore` | Shared .gitignore template |

### Modified Files

| File | Changes |
|---|---|
| `src/commands/ext.rs` | Add `ExtCommands::Init`; wire to `ScaffoldEngine` |
| `src/extension/adapters/validation.rs` | Add `ValidationDepth` enum; implement semantic checks |
| `src/extension/manager/packaging.rs` | Add `--with-deps` support; `peko.lock` generation |
| `src/extension/mod.rs` | Add `scaffold` module |
| `docs/architecture/EXTENSION_SYSTEM.md` | Update with `peko ext init` examples |
| `README.md` | Update quickstart |

---

## Success Criteria

- [ ] `peko ext init my-skill --type skill` creates a valid skill extension.
- [ ] `peko ext init my-tool --type universal-tool --lang python` creates a valid tool with working handler stub.
- [ ] `peko ext init my-gateway --type gateway --lang javascript` creates a valid gateway with GatewayPacket stub.
- [ ] `peko ext validate ./my-extension --semantic` catches missing commands, invalid URLs, and bad schemas.
- [ ] `peko ext push my-extension --with-deps` bundles dependencies into the `.ext` package.
- [ ] All templates produce extensions that pass `peko ext validate` and `peko ext install`.
- [ ] E2E test validates full `init -> validate -> install -> export -> push -> pull -> install` roundtrip.

---

## Related Documents

- ADR-017: Unified Extension Architecture
- ADR-024: Unified Extension Manifest
- ADR-025: Gateway Extension Architecture
- ADR-026: Extension Lifecycle Separation
- ADR-027: Unified Packaging
- ADR-035: Runtime-Pekohub Tunnel Protocol
- `docs/architecture/EXTENSION_SYSTEM.md`
- `src/commands/ext.rs`
- `src/extension/adapters/validation.rs`
- `src/extension/manager/packaging.rs`
