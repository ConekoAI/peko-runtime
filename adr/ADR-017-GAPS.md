# ADR-017 Implementation Gaps Analysis

**Date:** 2026-04-10  
**Status:** Analysis  
**Related:** ADR-017: Unified Extension Architecture

---

## Executive Summary

The Unified Extension Architecture (ADR-017) is approximately **65% implemented**. The core infrastructure (ExtensionCore, hook points, type-specific adapters) is complete, but several key components are missing or incomplete. This document identifies all gaps and prioritizes them for completion.

---

## Gap Summary by Priority

### 🔴 Critical (Blocking Production Use)

| Gap | Impact | Effort | Owner |
|-----|--------|--------|-------|
| Adapters not registered in BuiltInAdapters | Extension system non-functional | 1 hr | - |
| CLI commands stubbed (`pekobot ext`) | No user interface | 2 days | - |
| GeneralExtensionAdapter missing | Advanced extensions impossible | 1 day | - |

### 🟡 High (Required for v1.0)

| Gap | Impact | Effort | Owner |
|-----|--------|--------|-------|
| ChannelAdapter incomplete | Channel extensions broken | 1 day | - |
| HookAdapter incomplete | Event hook extensions broken | 1 day | - |
| GatewayAdapter incomplete | Gateway extensions broken | 1 day | - |
| Parallel code paths (legacy + new) | Technical debt, confusion | 3 days | - |
| Missing CLI: `validate`, `debug` | Poor DX for general extensions | 4 hrs | - |

### 🟢 Medium (Should Have)

| Gap | Impact | Effort | Owner |
|-----|--------|--------|-------|
| Documentation incomplete | Adoption barrier | 2 days | - |
| E2E tests for extension system | Regression risk | 2 days | - |
| Bundle format not implemented | Advanced distribution | 1 day | - |

---

## Detailed Gap Analysis

### 1. 🔴 Critical Gaps

#### 1.1 Adapters Not Registered (CRITICAL)

**Location:** `src/extensions/adapters/mod.rs:314-322`

**Current State:**
```rust
pub fn adapters(&self) -> Vec<Box<dyn ExtensionTypeAdapter>> {
    vec![
        // Phase 2: Box::new(SkillAdapter::new()),
        // Phase 3: Box::new(UniversalToolAdapter::new()),
        // Phase 4: Box::new(McpAdapter::new()),
        // Phase 5: Box::new(ChannelAdapter::new()),
        // Phase 6: Box::new(HookAdapter::new()),
        // Phase 6: Box::new(GatewayAdapter::new()),
    ]
}
```

**Problem:** Adapters are implemented but not wired into the system. The ExtensionManager has no adapters to route extensions to.

**Fix:**
```rust
pub fn adapters(&self) -> Vec<Box<dyn ExtensionTypeAdapter>> {
    vec![
        Box::new(SkillAdapter::new()),
        Box::new(UniversalToolAdapter::new()),
        Box::new(McpAdapter::with_default_manager()),
        // Channel, Hook, Gateway, General adapters when complete
    ]
}
```

**Effort:** 1 hour

---

#### 1.2 CLI Commands Stubbed (CRITICAL)

**Location:** `src/commands/ext.rs`

**Current State:** Commands exist as stubs with `todo!()` macros or minimal implementations.

**Missing:**
- `pekobot ext install <path>` - Install extension
- `pekobot ext list` - List installed extensions
- `pekobot ext enable <id>` - Enable extension
- `pekobot ext disable <id>` - Disable extension
- `pekobot ext uninstall <id>` - Remove extension
- `pekobot ext validate <path>` - Validate manifest
- `pekobot ext debug <id>` - Show resolved hooks

**Required Implementation:**
```rust
// Example: ext install
pub async fn install(path: &Path) -> Result<()> {
    let mut manager = ExtensionManager::new();
    // Register all adapters
    manager.register_adapter(Box::new(SkillAdapter::new()));
    manager.register_adapter(Box::new(UniversalToolAdapter::new()));
    // ... etc
    
    let extension_id = manager.install(path).await?;
    println!("Installed extension: {}", extension_id);
    Ok(())
}
```

**Effort:** 2 days

---

#### 1.3 GeneralExtensionAdapter Missing (CRITICAL)

**Location:** Not implemented

**Requirement:** Adapter that parses explicit hook declarations from manifest.

**Required Implementation:**
```rust
// src/extensions/adapters/general_adapter.rs
pub struct GeneralExtensionAdapter;

#[derive(Debug, Clone, Deserialize)]
struct GeneralExtensionConfig {
    hooks: Vec<HookDeclaration>,
}

#[derive(Debug, Clone, Deserialize)]
struct HookDeclaration {
    point: String,           // "tool.execute", "event.subscribe", etc.
    #[serde(flatten)]
    params: HashMap<String, serde_json::Value>,
    handler: String,
}

impl ExtensionTypeAdapter for GeneralExtensionAdapter {
    fn extension_type(&self) -> &'static str { "general" }
    
    fn manifest_format(&self) -> ManifestFormat {
        ManifestFormat::YamlFrontmatterMarkdown {
            required_fields: vec!["id", "name", "hooks"],
            file_name: "manifest.yaml",
        }
    }
    
    fn resolve_hooks(&self, manifest: &ExtensionManifest) -> Vec<HookBinding> {
        // Parse hook declarations and create bindings
        // for all 22 hook point types
    }
}
```

**Hook Point Mapping:**
| Hook Point String | Parsed HookPoint Variant |
|-------------------|-------------------------|
| `prompt.system_section` | `PromptSystemSection { section, priority }` |
| `prompt.pre_process` | `PromptPreProcess` |
| `prompt.post_process` | `PromptPostProcess` |
| `tool.register` | `ToolRegister` |
| `tool.execute` | `ToolExecute { tool_name }` |
| `tool.execute_async` | `ToolExecuteAsync { tool_name }` |
| `tool.check_status` | `ToolCheckStatus { tool_name }` |
| `tool.cancel` | `ToolCancel { tool_name }` |
| `tool.result_transform` | `ToolResultTransform` |
| `session.state_change` | `SessionStateChange` |
| `session.compaction` | `SessionCompaction` |
| `session.context_build` | `SessionContextBuild` |
| `io.channel_input` | `ChannelInput` |
| `io.channel_output` | `ChannelOutput` |
| `io.message_pre_send` | `MessagePreSend` |
| `io.message_post_receive` | `MessagePostReceive` |
| `event.subscribe` | `EventSubscribe { topic_pattern }` |
| `event.emit` | `EventEmit` |
| `agent.init` | `AgentInit` |
| `agent.shutdown` | `AgentShutdown` |
| `agent.iteration` | `AgentIteration { iteration }` |

**Effort:** 1 day

---

### 2. 🟡 High Priority Gaps

#### 2.1 ChannelAdapter Incomplete

**Location:** `src/extensions/adapters/channel_adapter.rs`

**Issues:**
- `discover_channel_extensions()` parses manifest but doesn't fully validate
- `MessageTransformerHandler` is pass-through (no actual transformation logic)
- No integration with actual channel system (`src/channels/`)

**Required:**
- Implement actual message transformation logic
- Wire into channel message flow
- Add validation for transformer configurations

**Effort:** 1 day

---

#### 2.2 HookAdapter Incomplete

**Location:** `src/extensions/adapters/hook_adapter.rs`

**Issues:**
- `EventSubscriptionHandler` is pass-through
- No webhook/cron implementation
- No integration with event bus (`src/hooks/event_bus.rs`)

**Required:**
- Implement webhook dispatch
- Implement cron scheduling
- Wire into event subscription system

**Effort:** 1 day

---

#### 2.3 GatewayAdapter Incomplete

**Location:** `src/extensions/adapters/gateway_adapter.rs`

**Issues:**
- `GatewayHookHandler` is pass-through
- No actual gateway server implementation
- No integration with gateway system (`src/gateway/`)

**Required:**
- Implement gateway lifecycle management
- Wire into existing gateway infrastructure
- Support HTTP/WebSocket/pubsub gateway types

**Effort:** 1 day

---

#### 2.4 Parallel Code Paths

**Problem:** Both legacy and new systems exist simultaneously, causing confusion and maintenance burden.

| Legacy System | New System | Status |
|---------------|------------|--------|
| `src/skills/mod.rs` | `extensions/adapters/skill_adapter.rs` | Duplicated parsing |
| `src/tools/universal/discovery.rs` | `extensions/adapters/universal_tool_adapter.rs` | Adapter calls legacy |
| `src/mcp/` | `extensions/adapters/mcp_adapter.rs` | Adapter wraps MCP |
| `src/channels/` | `extensions/adapters/channel_adapter.rs` | Parallel |
| `src/hooks/` | `extensions/adapters/hook_adapter.rs` | Parallel |
| `src/gateway/` | `extensions/adapters/gateway_adapter.rs` | Parallel |

**Resolution Strategy:**
1. Keep legacy modules as thin wrappers around ExtensionCore
2. Migrate internal logic to adapters
3. Deprecate legacy APIs
4. Eventually remove legacy modules

**Effort:** 3 days

---

#### 2.5 Missing CLI Commands

**`pekobot ext validate <path>`**
- Parse manifest
- Validate hook declarations (if general extension)
- Check required fields
- Verify handler references

**`pekobot ext debug <id>`**
- Show all registered hooks for extension
- Show hook point bindings
- Show priority ordering
- Show enable/disable status

**Effort:** 4 hours

---

### 3. 🟢 Medium Priority Gaps

#### 3.1 Documentation

**Missing:**
- Extension manifest schema documentation
- Hook point semantics guide
- Migration guide from legacy systems
- Best practices for general extensions
- API reference for custom adapters

**Effort:** 2 days

---

#### 3.2 E2E Tests

**Missing:**
- End-to-end test for extension install/enable/disable/uninstall cycle
- Test for each adapter type
- Test for general extension with multiple hook types
- Test for bundle creation and installation

**Effort:** 2 days

---

#### 3.3 Bundle Format

**Current State:** `ExtensionBundle` struct exists but no serialization/packaging logic.

**Required:**
- Tar.gz packaging format
- Manifest inclusion
- Dependency resolution during install
- Conflict detection

**Effort:** 1 day

---

## Implementation Roadmap

### Phase 1: Critical (Week 1)
- [ ] Register adapters in BuiltInAdapters
- [ ] Implement GeneralExtensionAdapter
- [ ] Implement core CLI commands (install, list, enable, disable, uninstall)

### Phase 2: High Priority (Week 2)
- [ ] Complete ChannelAdapter
- [ ] Complete HookAdapter
- [ ] Complete GatewayAdapter
- [ ] Implement `validate` and `debug` CLI commands
- [ ] Consolidate parallel code paths

### Phase 3: Medium Priority (Week 3)
- [ ] Implement bundle format
- [ ] Write comprehensive documentation
- [ ] Add E2E tests
- [ ] Performance benchmarking

### Phase 4: Polish (Week 4)
- [ ] Bug fixes
- [ ] Performance optimization
- [ ] Migration tooling improvements
- [ ] Release preparation

---

## Code Quality Issues

### Current Compilation Warnings

```
warning: unused import: `std::sync::Arc` in config_authority/implementation.rs
warning: unused import: `crate::common::paths::PathResolver` in config_authority/io.rs
warning: unused import: `std::collections::HashMap` in common/types/agent.rs
... (18 more warnings)
```

**Recommendation:** Clean up warnings before v1.0 release.

### Test Coverage

| Module | Coverage | Notes |
|--------|----------|-------|
| `extensions/core/hook_points.rs` | Good | Unit tests for matching, categories |
| `extensions/core/registry.rs` | Good | Tests for register/unregister/invoke |
| `extensions/adapters/skill_adapter.rs` | Good | Full test suite |
| `extensions/adapters/mcp_adapter.rs` | Partial | Basic tests only |
| `extensions/adapters/universal_tool_adapter.rs` | Good | Full test suite |
| `extensions/adapters/channel_adapter.rs` | Minimal | Only config tests |
| `extensions/adapters/hook_adapter.rs` | Minimal | Only config tests |
| `extensions/adapters/gateway_adapter.rs` | Minimal | Only config tests |
| `extensions/manager/` | Minimal | Only basic tests |

---

## Conclusion

The Extension Architecture foundation is solid, but several critical pieces are missing:

1. **Immediate action:** Register adapters and implement CLI
2. **Short-term:** Complete partial adapters and add GeneralExtensionAdapter
3. **Medium-term:** Documentation and comprehensive testing

With focused effort (~2 weeks), the system can reach production readiness.
