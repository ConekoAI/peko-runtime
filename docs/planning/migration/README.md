# Pekobot Migration Guide

**Version:** 0.1.0  
**Date:** 2026-04-11  
**Status:** Current  

This guide covers migration between major versions of Pekobot.

---

## Migration Overview

| From | To | Status | Effort |
|------|-----|--------|--------|
| Legacy (pre-ADR-017) | Extensions 2.0 | ✅ Automatic | None |
| Extensions 2.0 | Control Plane | 📝 Planned | Manual |

---

## Extensions 2.0 Migration (Complete)

The migration from legacy extension systems to the Unified Extension Architecture (ADR-017) is **automatic and idempotent**. No manual action required.

### What Migrated

| Legacy Component | New System | Migration Status |
|------------------|------------|------------------|
| Skills (`~/.pekobot/skills/`) | SkillAdapter | ✅ Complete |
| Universal Tools (`~/.pekobot/tools/`) | UniversalToolAdapter | ✅ Complete |
| MCP Servers (`mcp.toml`) | McpAdapter | ✅ Complete |
| Built-in Tools (hardcoded) | BuiltinToolAdapter | ✅ Complete |

### Migration Process

On first startup after updating to Pekobot 0.1.0:

1. **Detection**: System scans legacy paths
2. **Conversion**: Legacy items converted to extension format
3. **Registration**: Extensions registered with ExtensionManager
4. **Persistence**: Migration state saved to `migration-state.json`

```
2026-05-05T09:00:00Z [INFO] Starting legacy extension migration
2026-05-05T09:00:01Z [INFO] Migrating skills: 5 found, 5 succeeded
2026-05-05T09:00:02Z [INFO] Migrating tools: 3 found, 3 succeeded  
2026-05-05T09:00:03Z [INFO] Migrating MCP servers: 2 found, 2 succeeded
2026-05-05T09:00:04Z [INFO] Migration complete: 10/10 items migrated
```

### Post-Migration Structure

After migration, your extensions are organized as:

```
~/.pekobot/extensions/
├── github/
│   └── SKILL.md              # Migrated from ~/.pekobot/skills/github/
├── web_search/
│   └── manifest.json         # Migrated from ~/.pekobot/tools/web_search/
├── filesystem/
│   └── config.json           # Migrated from mcp.toml
└── ...
```

### Rollback

To rollback to legacy behavior (not recommended):

```bash
# Remove migration state
rm ~/.pekobot/migration-state.json

# System will re-migrate on next startup
```

---

## Breaking Changes

### CLI Changes

| Old Command | New Command | Status |
|-------------|-------------|--------|
| `pekobot skill list` | `pekobot ext list` | ❌ Removed |
| `pekobot tool list` | `pekobot ext list` | ❌ Removed |
| `pekobot mcp list` | `pekobot ext list` | ❌ Removed |

Old commands no longer exist. Use `pekobot ext` for all extension management.

### API Changes

| Old API | New API | Status |
|---------|---------|--------|
| `AgentManager` | `StatelessAgentManager` | ⚠️ Deprecated |
| `MessageService` | `StatelessAgentService` | ❌ Removed |
| `SessionResolver` | `SessionManager::resolve_session()` | ❌ Removed |

See [API_SURFACE.md](../../../API_SURFACE.md) for full migration details.

---

## Configuration Migration

### Agent Config (config.toml)

No changes required. Agent configuration format is stable.

### Runtime Config (runtime.toml)

New optional fields for extension system:

```toml
[extensions]
auto_install = true              # Auto-install declared capabilities
discovery_paths = [              # Additional extension search paths
    "./custom-extensions/"
]

[extensions.defaults]
enabled_types = ["skill", "tool", "mcp"]  # Types auto-enabled on install
```

### Team Config (team.toml)

No changes required. Team configuration format is stable.

---

## Custom Extension Migration

### Converting Legacy Custom Tools

If you have custom tools in your workspace:

**Before (legacy):**
```
./tools/
└── my_custom_tool/
    ├── manifest.json
    └── my_custom_tool.py
```

**After (Unified Extensions):**
```
./.pekobot/extensions/
└── my_custom_tool/
    ├── manifest.json      # Same content
    └── my_custom_tool.py  # Same content
```

Or keep in `./tools/` and reference in `config.toml`:

```toml
[extensions]
discovery_paths = ["./tools/"]
```

### Converting Legacy Hooks

**Before (legacy):**
```toml
# config.toml
[[hooks]]
type = "webhook"
path = "/custom-hook"
```

**After (Extensions 2.0):**
```toml
# HOOK.toml in extension directory
---
name: custom-hook
hooks:
  - point: channel.input
    path: "/custom-hook"
```

---

## Extension Development Migration

### For Skill Developers

No changes required. `SKILL.md` format is unchanged.

### For Tool Developers

No changes required. `manifest.json` format is unchanged.

### For MCP Server Developers

No changes required. MCP protocol is unchanged.

### For Gateway Developers

**Before (legacy):**
```rust
impl Gateway for MyGateway {
    // Gateway trait implementation
}
```

**After (Extensions 2.0):**
```yaml
# GATEWAY.toml
---
name: my-gateway
extension_type: gateway
hooks:
  - point: channel.input
    handler: handle_input
  - point: channel.output
    handler: handle_output
```

---

## Verification

### Check Migration Status

```bash
# View migration state
pekobot system doctor

# Output:
# Migration Status: Complete
# Skills: 5 migrated
# Tools: 3 migrated
# MCP Servers: 2 migrated
```

### Test Extensions

```bash
# List all extensions
pekobot ext list

# Test specific extension
pekobot ext debug my-extension

# Validate all extensions
pekobot ext validate --all
```

---

## Troubleshooting

### Migration Failed

**Symptom:** Extensions not appearing after update

**Solution:**
```bash
# Force re-migration
rm ~/.pekobot/migration-state.json
pekobot system doctor --fix
```

### Extension Conflicts

**Symptom:** "Extension ID already exists" warning

**Solution:**
```bash
# Check for duplicates
pekobot ext list | grep duplicate-id

# Remove or rename duplicate
pekobot ext uninstall duplicate-id
```

### Hook Priority Issues

**Symptom:** Extensions firing in wrong order

**Solution:**
```bash
# Check hook priority
pekobot ext debug my-extension

# Adjust priority in manifest if supported
```

---

## Migration Checklist

- [ ] Update to latest Pekobot
- [ ] Run `pekobot system doctor` to verify migration
- [ ] Check `pekobot ext list` shows all expected extensions
- [ ] Test agent execution
- [ ] Update any custom scripts using deprecated CLI commands
- [ ] Review and update CI/CD pipelines

---

## Getting Help

- **Documentation**: [docs/architecture/EXTENSION_SYSTEM.md](../../architecture/EXTENSION_SYSTEM.md)
- **CLI Help**: `pekobot ext --help`
- **Debug Info**: `pekobot system info --json`

---

*Version 0.1.0 · Last Updated: 2026-05-05*
