# Legacy Code Audit: Extension Architecture Migration

**Date:** 2026-04-10  
**Scope:** Identify code made obsolete by ADR-017 Extension Architecture  
**Status:** Ready for cleanup

---

## Executive Summary

The ADR-017 Unified Extension Architecture has replaced three legacy extension systems:
1. **Skills** (`src/skills/`) → `SkillAdapter` + `ExtensionManager`
2. **Universal Tools** (`src/tools/universal/discovery.rs`) → `UniversalToolAdapter` + `ExtensionManager`
3. **MCP Servers** (`src/mcp/discovery.rs`) → `McpAdapter` + `ExtensionManager`

**Estimated Lines of Code for Removal:** ~1,500-2,000 lines  
**Migration Path:** All legacy code has deprecated markers; safe to remove after migration period

---

## 🔴 HIGH PRIORITY: Duplicate/Obsolete Code

### 1. Legacy Skills Module (`src/skills/mod.rs`)

**Status:** Re-export facade over Extension Architecture  
**Obsolescence:** 90%

#### Duplicated Functions:
| Function | Line | Issue | Replacement |
|----------|------|-------|-------------|
| `parse_frontmatter()` | 68-94 | **Exact duplicate** of `adapters::parsing::parse_yaml_frontmatter()` | Use shared utility |
| `Skill` struct | 33-48 | Duplicate of `DiscoveredSkill` | Use `DiscoveredSkill` |
| `SkillFrontmatter` | 51-59 | Duplicate of adapter's `SkillFrontmatter` | Use adapter version |

#### Recommended Action:
```rust
// KEEP: Re-exports for backward compatibility
pub use crate::extensions::adapters::skill_adapter::{
    DiscoveredSkill, SkillAdapter, ...
};

// REMOVE: Duplicate definitions
- Skill struct (lines 33-48)
- SkillFrontmatter struct (lines 51-59)  
- parse_frontmatter() function (lines 68-94)
- format_skills_for_prompt() (lines 97-114) - now in adapter
- build_skills_prompt() (lines 117-132) - now in adapter
```

---

### 2. Universal Tool Discovery (`src/tools/universal/discovery.rs`)

**Status:** Deprecated, marked for removal  
**Obsolescence:** 100%

#### Deprecated Functions (with warnings):
| Function | Line | Deprecated Since | Replacement |
|----------|------|------------------|-------------|
| `discover_universal_tools()` | 40 | 0.2.0 | `ExtensionManager::scan_directory()` |
| `load_universal_tools()` | 141 | 0.2.0 | `ExtensionManager::load_from_directory()` |
| `default_tools_dir()` | 22 | - | Use `PathResolver::tools_dir()` |

#### Recommended Action:
```rust
// REMOVE entire file after migration period
// All functionality now in UniversalToolAdapter
```

---

### 3. MCP Discovery (`src/mcp/discovery.rs`)

**Status:** Deprecated, marked for removal  
**Obsolescence:** 100%

#### Deprecated Functions:
| Function | Line | Deprecated Since | Replacement |
|----------|------|------------------|-------------|
| `should_use_mcp_tools()` | 44 | 0.2.0 | `ExtensionManager::scan_directory()` |
| `list_available_servers()` | 216 | 0.2.0 | `McpAdapter::discover_servers()` |

#### Keep (Still Used):
- `find_server_binary()` - utility still needed
- `is_server_installed()` - utility still needed
- `mcp_config_path()` - path utility
- `mcp_install_dir()` - path utility

#### Recommended Action:
```rust
// REMOVE deprecated functions (lines 44-62, 216-240)
// KEEP path utilities and binary discovery
```

---

### 4. Legacy Command Modules

#### `src/commands/tool.rs`
**Status:** Deprecated facade  
**Line Count:** ~600 lines

```rust
// Module header states:
//! # Deprecation
//!
//! This module is deprecated. Use `pekobot tools universal` instead.
```

**Recommendation:** Remove after `pekobot ext` commands are fully adopted.

---

#### `src/commands/mcp.rs`
**Status:** Deprecated facade  
**Line Count:** ~900 lines

```rust
// Module header states:
//! # Deprecation
//!
//! This module is deprecated. Use `pekobot tools mcp` instead.
```

**Recommendation:** Remove after `pekobot ext` commands are fully adopted.

---

## 🟡 MEDIUM PRIORITY: Redundant Abstractions

### 5. Tool Catalog Redundancy (`src/tool_management/catalog.rs`)

**Issue:** Duplicates ExtensionManager functionality

| Method | Lines | Duplicates |
|--------|-------|------------|
| `load_mcp_tools()` | 101-109 | `McpAdapter` |
| `load_universal_tools()` | 112-133 | `UniversalToolAdapter` + `ExtensionManager` |

**Current Implementation:**
```rust
// Lines 112-133: Already uses ExtensionManager internally!
async fn load_universal_tools(&self) -> anyhow::Result<Vec<InstalledToolInfo>> {
    use crate::extensions::adapters::BuiltInAdapters;
    use crate::extensions::manager::ExtensionManager;
    let mut manager = ExtensionManager::new();
    for adapter in BuiltInAdapters::new().adapters() {
        manager.register_adapter(adapter);
    }
    let discovered = manager.scan_directory(&self.tools_dir).await?;
    // ...
}
```

**Recommendation:** Refactor to use ExtensionManager directly instead of maintaining parallel catalog.

---

### 6. ReservedParams Duplication

**Legacy:** `src/mcp/config.rs` - `ReservedParamConfig` struct  
**New:** `src/extensions/services/reserved_params.rs` - `ReservedParamsConfig`

**Status:** Legacy marked deprecated, but still widely used.

**Files Still Using Legacy:**
- `src/mcp/config.rs` (definitions)
- `src/mcp/injectable_proxy.rs`
- `src/mcp/tool_proxy.rs`
- `src/commands/mcp.rs`
- `src/extensions/services/reserved_params.rs` (conversion functions)

**Recommendation:** Migrate all usages to new `ReservedParamsConfig`.

---

## 🟢 LOW PRIORITY: Agent/Config Legacy

### 7. Agent Config Registry (`src/agent/config_registry.rs`)

**Status:** Deprecated since 0.11.0  
**Replacement:** `AgentConfigService` or `ConfigAuthority` from `crate::common::services`

```rust
#![deprecated(since = "0.11.0", note = "...")]
```

**Recommendation:** Remove after confirming all usages migrated to ConfigAuthority.

---

### 8. Agent Lifecycle Legacy Methods (`src/agent/lifecycle.rs`)

**Status:** Deprecated since 0.2.0 (stateless migration)

| Method | Line | Status |
|--------|------|--------|
| `register()` | 228 | No-op in stateless |
| `start()` | 238 | No-op in stateless |
| `stop()` | 260 | No-op in stateless |
| `mark_busy()` | 248 | Use `start_execution()` |
| `mark_idle()` | 254 | Use `complete_execution()` |

---

## 📊 Code Removal Estimate

| Module | Lines | Priority | Action |
|--------|-------|----------|--------|
| `src/skills/mod.rs` duplicates | ~80 | 🔴 High | Remove duplicates, keep re-exports |
| `src/tools/universal/discovery.rs` | ~350 | 🔴 High | Remove entire file |
| `src/mcp/discovery.rs` deprecated fns | ~100 | 🔴 High | Remove deprecated functions |
| `src/commands/tool.rs` | ~600 | 🔴 High | Remove deprecated command module |
| `src/commands/mcp.rs` | ~900 | 🔴 High | Remove deprecated command module |
| `src/tool_management/catalog.rs` redundancy | ~100 | 🟡 Med | Refactor to use ExtensionManager |
| `src/mcp/config.rs` ReservedParamConfig | ~200 | 🟡 Med | Migrate to new struct |
| `src/agent/config_registry.rs` | ~400 | 🟢 Low | Remove after migration |
| **Total** | **~2,730** | | |

---

## ✅ Safe to Remove Now (Post-Migration)

These have been marked deprecated for >1 version and have working replacements:

1. **`src/tools/universal/discovery.rs`** - All functions emit deprecation warnings
2. **`src/mcp/discovery.rs:44-62`** - `should_use_mcp_tools()`
3. **`src/mcp/discovery.rs:216-240`** - `list_available_servers()`
4. **`src/skills/mod.rs:33-94`** - Duplicate struct/function definitions
5. **`src/commands/tool.rs`** - Entire module deprecated
6. **`src/commands/mcp.rs`** - Entire module deprecated

---

## 🔧 Migration Checklist

Before removing legacy code, ensure:

- [ ] All users migrated from `pekobot tool` to `pekobot ext`
- [ ] All users migrated from `pekobot mcp` to `pekobot ext`  
- [ ] Migration system has processed all legacy extensions
- [ ] `migration-state.json` marks completion
- [ ] E2E tests pass without legacy code paths
- [ ] Documentation updated to remove legacy references

---

## Related Files

- `src/extensions/migration.rs` - Handles legacy → new migration
- `src/main.rs:55-98` - Migration runner
- `adr/ADR-017-GAPS.md` - Implementation status
