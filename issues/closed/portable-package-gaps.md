# Portable Package Gaps: Static Image Completeness

## Current State

Portable packages (`.agent` files) currently bundle:

| Component | Status | Notes |
|-----------|--------|-------|
| Agent Identity | ✅ Bundled | `identity/` - encrypted keys |
| Agent Config | ✅ Bundled | `config.toml` - agent configuration |
| Skills | ✅ Bundled | `skills/{name}/` - full skill directories |
| Workspace | ✅ Bundled | `workspace/` - agent workspace files |
| Sessions | ✅ Bundled | `sessions/` - conversation history |
| **Universal Tools** | ❌ **NOT bundled** | Only names in manifest |
| **MCP Servers** | ❌ **NOT bundled** | Only names/configs in manifest |

> **Note on Memory**: Core memory was removed. If memory functionality is needed, users should use external MCP memory servers, which are treated as regular MCPs for bundling purposes.

## The Gap

### Problem
Portable packages are **not truly self-contained**. When you export an agent with Universal Tools or MCP servers configured, the package only contains:

```toml
# manifest.toml
tools.required = ["filesystem", "calculator"]
mcp.servers = [{ name = "memory", transport = "stdio", command = "mcp-memory-server" }]
```

But the actual binaries for these tools/MCPs are NOT included. When importing on another machine:

1. Universal Tools must be manually installed/available
2. MCP servers must be manually configured/installed
3. Version mismatches may occur

### Why This Matters

A portable agent package should be **hermetic** - it should work identically on any machine without external dependencies (except the Pekobot runtime itself).

## Options for Resolution

### Universal Tools

| Option | Approach | Pros | Cons |
|--------|----------|------|------|
| A | **Bundle Binaries** | True hermeticity | Large packages, platform-specific, security |
| B | **Registry References** | Smaller packages, version-locked | Requires network, registry availability |
| C | **Keep Current** | Simple, small packages | Manual installation, version mismatches |

### MCP Servers

| Option | Approach | Pros | Cons |
|--------|----------|------|------|
| A | **Bundle Binaries** | Self-contained | Platform-specific, large size |
| B | **Bundle Container Specs** | Platform-agnostic, version-locked | Requires container runtime |
| C | **Config-Only (Current)** | Small packages, flexible | Manual MCP setup on target machine |

### Combined Analysis

```
┌─────────────────────────────────────────────────────────────────┐
│                    Bundling Strategy Matrix                      │
├─────────────────┬─────────────────┬─────────────────────────────┤
│ Hermeticity     │ Package Size    │ Implementation Complexity   │
├─────────────────┼─────────────────┼─────────────────────────────┤
│ Full (A+A)      │ Large           │ High                        │
│ Partial (B+B)   │ Medium          │ Medium                      │
│ Config-Only     │ Small           │ Low (Current)               │
└─────────────────┴─────────────────┴─────────────────────────────┘
```

## Recommendation

### Immediate (Current)
**Config-Only approach** - Document that Universal Tools and MCP servers are external dependencies. Portable packages bundle agent-specific state (identity, config, workspace, skills, sessions).

### Short-term
1. **MCP Server Bundling (Option A)** - Bundle actual MCP binaries when specified:
   - Add `bundle_mcp = true` option to MCP server config
   - Include binary paths in portable package
   - On import: extract to `.pekobot/tools/mcp/{name}/`
   - Update manifest to reference bundled paths

2. **Tool Registry for Universal Tools (Option B)**:
   - Extend `tools.required` with `version` and `source` fields
   - Import-time resolution with fallback

### Medium-term
**Hybrid approach**:
- MCP servers: Bundle binaries (they're typically small, single-purpose)
- Universal Tools: Use registry references (larger, shared across agents)

### Rejected Options
- **Container-based (Option B for MCPs)**: Adds heavy dependency (container runtime)
- **Full binary bundling**: Too large for universal tools which are shared

## Related Work

- Memory core audit (`issues/memory-core-audit.md`) - COMPLETED: Memory removed from core
- CAP system (`src/cap/`) - Unified capability management may help with tool references
