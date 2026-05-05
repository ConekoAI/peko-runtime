# ExtensionManager Migration Plan: Universal Tool Discovery

**Date:** 2026-04-10  
**Status:** Draft  
**Related:** ADR-017: Unified Extension Architecture, ADR-017-GAPS  

---

## Executive Summary

This plan addresses the migration from deprecated `discover_universal_tools()` and `load_universal_tools()` functions to the `ExtensionManager` system. The migration eliminates deprecation warnings, consolidates tool discovery under the unified Extension Architecture, and maintains backward compatibility.

### Current State

```
┌─────────────────────────────────────────────────────────────────────┐
│ LEGACY PATH (Deprecated - generates warnings)                       │
├─────────────────────────────────────────────────────────────────────┤
│  src/tools/universal/discovery.rs                                   │
│    ├── discover_universal_tools()  ← #[deprecated]                  │
│    └── load_universal_tools()      ← #[deprecated]                  │
│           │                                                         │
│           ▼                                                         │
│  • src/agent/agent.rs:191         load_universal_tools()            │
│  • src/tools/factory.rs:916       load_universal_tools()            │
│  • src/cap/catalog.rs:125         discover_universal_tools()        │
│  • src/commands/tool.rs:252       discover_universal_tools()        │
│  • src/tool_management/catalog.rs:113  discover_universal_tools()   │
│  • src/extensions/adapters/universal_tool_adapter.rs:81             │
│                                   discover_universal_tools()        │
└─────────────────────────────────────────────────────────────────────┘
```

### Target State

```
┌─────────────────────────────────────────────────────────────────────┐
│ UNIFIED PATH (ExtensionManager)                                     │
├─────────────────────────────────────────────────────────────────────┤
│  src/extensions/manager/mod.rs                                      │
│    ├── ExtensionManager::load_all()         (discovers & loads)     │
│    ├── ExtensionManager::install()          (install single)        │
│    └── ExtensionManager::list_extensions()  (list all)              │
│           │                                                         │
│           ▼                                                         │
│  UniversalToolAdapter                                               │
│    ├── extension_type() → "universal-tool"                          │
│    ├── manifest_format() → ManifestFormat::Json                     │
│    ├── resolve_hooks() → Vec<HookBinding>                           │
│    └── discover_tools() [REFACTORED]                                │
│           - No longer calls deprecated function                     │
│           - Uses direct directory scanning                          │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 1. Gap Analysis

### 1.1 Missing ExtensionManager APIs

| Required API | Current Status | Gap Description |
|--------------|----------------|-----------------|
| `discover_from_directory(&self, path: &Path)` | **Missing** | ExtensionManager only scans fixed discovery paths; cannot scan arbitrary directories like `~/.pekobot/tools/` |
| `get_tools_as_trait_objects(&self)` | **Missing** | ExtensionManager returns `LoadedExtension` metadata; agents need `Arc<dyn Tool>` for execution |
| `load_with_report(&self, path: &Path)` | **Partial** | `LoadReport` exists but doesn't expose discovery vs loaded counts needed for custom tool loading |

### 1.2 Deprecated Function Call Sites

| File | Line | Function | Context |
|------|------|----------|---------|
| `src/agent/agent.rs` | 191 | `load_universal_tools()` | Agent loading system-wide tools |
| `src/tools/factory.rs` | 916 | `load_universal_tools()` | Custom tools from workspace `tools/` dir |
| `src/cap/catalog.rs` | 125 | `discover_universal_tools()` | CAP capability discovery |
| `src/commands/tool.rs` | 252 | `discover_universal_tools()` | CLI tool discovery (via `ext` command) |
| `src/tool_management/catalog.rs` | 113 | `discover_universal_tools()` | Tool management catalog |
| `src/extensions/adapters/universal_tool_adapter.rs` | 81 | `discover_universal_tools()` | Adapter discovery method |

### 1.3 Architecture Mismatch

```
PROBLEM: Dual Discovery Paths

ExtensionManager.load_all() scans:
  ~/.config/pekobot/extensions/     (user_config)
  ~/.local/share/pekobot/extensions/ (user_data)
  ./.pekobot/extensions/            (project_local)
  /usr/share/pekobot/extensions/    (system_wide)

Legacy system scans:
  ~/.pekobot/tools/                 (tools_dir via default_tools_dir())
  ./tools/                          (custom_tools_dir)

These paths do NOT overlap - migration must bridge both.
```

---

## 2. Migration Strategy

### 2.1 Guiding Principles

| Principle | Application |
|-----------|-------------|
| **SRP** | Each component has ONE reason to change: ExtensionManager manages lifecycle, Adapters handle type-specific logic, Tools handle execution |
| **DRY** | Discovery logic exists in ONE place (ExtensionManager), not duplicated in agent/factory/catalog/command modules |
| **KISS** | No new abstractions; use existing ExtensionManager patterns; avoid "manager of managers" |
| **ADR-017** | All tool types route through ExtensionCore hooks; no parallel registries |

### 2.2 Approach: Adapter Refactoring + Manager Enhancement

Instead of refactoring all call sites to use ExtensionManager directly, we **refactor the UniversalToolAdapter** to be the single integration point:

```
┌─────────────────────────────────────────────────────────────────────┐
│ MIGRATION ARCHITECTURE                                              │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  Agent/Factory/Catalog/Commands                                     │
│       │                                                             │
│       │ (call via adapter, not deprecated functions)                │
│       ▼                                                             │
│  UniversalToolAdapter                                               │
│       │                                                             │
│       │ (use ExtensionManager for discovery)                        │
│       ▼                                                             │
│  ExtensionManager ─────┐                                            │
│       │                │                                            │
│       │ (scans)        │ (fallback for legacy paths)               │
│       ▼                │                                            │
│  Discovery Paths       │                                            │
│  + ~/.pekobot/tools/ ──┘                                            │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 3. Implementation Plan

### Phase 1: ExtensionManager Enhancement (1 day)

#### 3.1.1 Add `scan_directory()` Method

**File:** `src/extensions/manager/mod.rs`

```rust
impl ExtensionManager {
    /// Scan a specific directory for extensions without installing to storage
    /// 
    /// This is used for:
    /// - Legacy tools directory (~/.pekobot/tools/)
    /// - Workspace custom tools (./tools/)
    /// - CAP catalog discovery
    pub async fn scan_directory(&self, path: &Path) -> Result<Vec<DiscoveredExtension>> {
        let mut discovered = Vec::new();
        
        if !path.exists() {
            return Ok(discovered);
        }

        let entries = tokio::fs::read_dir(path).await?;
        
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            // Try to detect extension type using registered adapters
            if let Some(adapter) = self.detect_extension_type(&path) {
                let format = adapter.manifest_format();
                if let Some(manifest_path) = format.manifest_path(&path) {
                    discovered.push(DiscoveredExtension {
                        path,
                        manifest_path,
                        extension_type: adapter.extension_type().to_string(),
                    });
                }
            }
        }

        Ok(discovered)
    }
}

pub struct DiscoveredExtension {
    pub path: PathBuf,
    pub manifest_path: PathBuf,
    pub extension_type: String,
}
```

**SRP Compliance:** ExtensionManager handles directory scanning (its responsibility). Caller decides what to do with discovered items.

**ADR-017 Alignment:** Uses existing adapter detection mechanism; no new concepts.

#### 3.1.2 Add `get_tool_instances()` Method

**File:** `src/extensions/manager/mod.rs`

```rust
impl ExtensionManager {
    /// Get loaded tools as Arc<dyn Tool> trait objects for agent consumption
    /// 
    /// This bridges the gap between ExtensionManager (tracks metadata)
    /// and Agent (needs executable Tool instances)
    pub async fn get_tool_instances(&self) -> Vec<Arc<dyn Tool>> {
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
        
        for (ext_id, loaded_ext) in &self.extensions {
            if !loaded_ext.enabled {
                continue;
            }
            
            // Only universal tools can be converted to Tool trait objects
            if loaded_ext.extension_type == "universal-tool" {
                if let Some(tool) = self.create_tool_instance(ext_id).await {
                    tools.push(tool);
                }
            }
        }
        
        tools
    }
    
    async fn create_tool_instance(&self, ext_id: &ExtensionId) -> Option<Arc<dyn Tool>> {
        let loaded = self.extensions.get(ext_id)?;
        
        // Create UniversalToolAdapter from manifest
        let adapter = crate::tools::universal::UniversalToolAdapter::from_manifest(
            &loaded.manifest.path.join("manifest.json"),
            &loaded.path.join(&loaded.manifest.name), // executable
        ).await.ok()?;
        
        Some(Arc::new(adapter))
    }
}
```

**SRP Compliance:** ExtensionManager doesn't implement Tool logic; delegates to UniversalToolAdapter.

**DRY Compliance:** Single place where ExtensionManager → Tool conversion happens.

### Phase 2: UniversalToolAdapter Refactoring (1 day)

#### 3.2.1 Refactor `discover_tools()` to Not Use Deprecated Function

**File:** `src/extensions/adapters/universal_tool_adapter.rs`

```rust
impl UniversalToolAdapter {
    /// Discover universal tools from a directory
    /// 
    /// REFACTORED: Previously called deprecated discover_universal_tools()
    /// Now implements its own scanning using the same logic
    pub async fn discover_tools(&self, path: &Path) -> Vec<DiscoveredUniversalTool> {
        let mut tools = Vec::new();

        if !path.exists() {
            debug!("Tools directory does not exist: {:?}", path);
            return tools;
        }

        let mut entries = match tokio::fs::read_dir(path).await {
            Ok(entries) => entries,
            Err(e) => {
                warn!("Failed to read tools directory {:?}: {}", path, e);
                return tools;
            }
        };

        while let Some(entry) = entries.next_entry().await.ok().flatten() {
            let tool_path = entry.path();
            
            // Skip non-directories and hidden
            if !tool_path.is_dir() {
                continue;
            }
            
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with('.') {
                continue;
            }

            // Look for manifest.json
            let manifest_path = tool_path.join("manifest.json");
            if !manifest_path.exists() {
                trace!("No manifest.json found in {}", tool_path.display());
                continue;
            }

            // Parse manifest to get tool name
            match self.parse_tool_manifest(&manifest_path, &tool_path).await {
                Ok(manifest) => {
                    // Find executable
                    if let Some(executable) = self.find_executable(&tool_path, &manifest.name).await {
                        tools.push(DiscoveredUniversalTool {
                            manifest,
                            executable,
                            manifest_path,
                        });
                    }
                }
                Err(e) => {
                    warn!("Failed to parse manifest {:?}: {}", manifest_path, e);
                }
            }
        }

        tools
    }
    
    async fn find_executable(&self, tool_path: &Path, tool_name: &str) -> Option<PathBuf> {
        // Try common patterns
        let candidates = vec![
            tool_path.join(format!("{}.py", tool_name)),
            tool_path.join(format!("{}.js", tool_name)),
            tool_path.join(format!("{}.sh", tool_name)),
            tool_path.join(tool_name),
        ];
        
        for candidate in candidates {
            if candidate.exists() {
                return Some(candidate);
            }
        }
        
        // Fallback: find any executable
        let mut entries = tokio::fs::read_dir(tool_path).await.ok()?;
        while let Some(entry) = entries.next_entry().await.ok().flatten() {
            let path = entry.path();
            if path.is_file() && path.file_name() != Some("manifest.json".as_ref()) {
                return Some(path);
            }
        }
        
        None
    }
}
```

**KISS Compliance:** No new dependencies; same logic as deprecated function but self-contained.

**DRY Compliance:** Logic lives in one place (the adapter), not spread across discovery.rs and here.

### Phase 3: Call Site Migration (2 days)

#### 3.3.1 Migrate `src/agent/agent.rs`

**Current:**
```rust
// Line 190-191
use crate::tools::universal::load_universal_tools;
match load_universal_tools(&tools_dir).await {
```

**Target:**
```rust
// Use UniversalToolAdapter directly (not deprecated)
use crate::extensions::adapters::UniversalToolAdapter;

let adapter = UniversalToolAdapter::new();
let discovered = adapter.discover_tools(&tools_dir).await;

for tool in discovered {
    match UniversalToolAdapter::from_manifest(&tool.manifest_path, &tool.executable).await {
        Ok(tool_adapter) => {
            tools.push(Arc::new(tool_adapter));
        }
        Err(e) => {
            tracing::warn!("Failed to load tool: {}", e);
        }
    }
}
```

#### 3.3.2 Migrate `src/tools/factory.rs`

**Current:**
```rust
// Line 901, 916
use crate::tools::universal::load_universal_tools;
let universal_tools = load_universal_tools(tools_dir).await?;
```

**Target:**
```rust
// Use the refactored adapter
use crate::extensions::adapters::UniversalToolAdapter;

let adapter = UniversalToolAdapter::new();
let discovered = adapter.discover_tools(tools_dir).await;

let mut universal_tools = Vec::new();
for tool in discovered {
    if let Ok(adapter) = UniversalToolAdapter::from_manifest(&tool.manifest_path, &tool.executable).await {
        universal_tools.push(adapter);
    }
}
```

#### 3.3.3 Migrate `src/cap/catalog.rs`

**Current:**
```rust
// Line 125
let discovered = discovery::discover_universal_tools(&self.tools_dir).await?;
```

**Target:**
```rust
use crate::extensions::adapters::UniversalToolAdapter;

let adapter = UniversalToolAdapter::new();
let discovered = adapter.discover_tools(&self.tools_dir).await;
```

#### 3.3.4 Migrate `src/commands/tool.rs`

**Current:**
```rust
// Line 239, 252
use crate::tools::universal::discover_universal_tools;
let discovered = discover_universal_tools(&tools_dir).await?;
```

**Target:**
```rust
use crate::extensions::adapters::UniversalToolAdapter;

let adapter = UniversalToolAdapter::new();
let discovered = adapter.discover_tools(&tools_dir).await;
```

#### 3.3.5 Migrate `src/tool_management/catalog.rs`

**Current:**
```rust
// Line 113
let discovered = discovery::discover_universal_tools(&self.tools_dir).await?;
```

**Target:**
```rust
use crate::extensions::adapters::UniversalToolAdapter;

let adapter = UniversalToolAdapter::new();
let discovered = adapter.discover_tools(&self.tools_dir).await;
```

### Phase 4: Deprecation Cleanup (0.5 days)

#### 3.4.1 Update Deprecation Attributes

**File:** `src/tools/universal/discovery.rs`

```rust
// After all migrations complete, strengthen deprecation:

/// Discover universal tools in a directory
/// 
/// DEPRECATED: Use `UniversalToolAdapter::discover_tools()` or `ExtensionManager::scan_directory()` instead.
#[deprecated(
    since = "0.2.0", 
    note = "Use UniversalToolAdapter::discover_tools() or ExtensionManager::scan_directory() instead"
)]
pub async fn discover_universal_tools(...) { ... }

/// Load discovered tools as adapters
///
/// DEPRECATED: Use `UniversalToolAdapter::discover_tools()` + `UniversalToolAdapter::from_manifest()` instead.
#[deprecated(
    since = "0.2.0",
    note = "Use UniversalToolAdapter::discover_tools() + from_manifest() instead"
)]
pub async fn load_universal_tools(...) { ... }
```

### Phase 5: Future Extension (Optional)

Once all call sites use the adapter, consider full ExtensionManager integration:

```rust
// Future: Agent uses ExtensionManager directly
async fn create_tools_async(&self) -> Result<Vec<Arc<dyn Tool>>> {
    let manager = self.extension_manager();
    
    // Register universal tool adapter if not already registered
    manager.register_adapter(Box::new(UniversalToolAdapter::new()));
    
    // Add legacy tools directory to scan paths
    manager.add_discovery_path(crate::tools::universal::discovery::default_tools_dir());
    
    // Load all extensions
    manager.load_all().await?;
    
    // Get tools as trait objects
    let tools = manager.get_tool_instances().await;
    
    Ok(tools)
}
```

---

## 4. Design Review

### 4.1 SRP (Single Responsibility Principle)

| Component | Responsibility | Assessment |
|-----------|---------------|------------|
| `ExtensionManager` | Extension lifecycle (install, enable, disable, uninstall) | ✅ Clean - doesn't know about Tool trait |
| `UniversalToolAdapter` | Universal tool specific logic (manifest parsing, hook resolution) | ✅ Clean - after refactor, handles its own discovery |
| `Agent` | Coordinate tool creation and usage | ✅ Clean - delegates to adapter, not deprecated functions |
| Factory/Catalog/Commands | Context-specific tool consumption | ✅ Clean - use adapter, not raw discovery |

**Verdict:** Each component has ONE reason to change. No component mixes lifecycle management with type-specific logic.

### 4.2 DRY (Don't Repeat Yourself)

| Code | Before | After |
|------|--------|-------|
| Discovery logic | In `discovery.rs` AND `universal_tool_adapter.rs` | ✅ ONLY in `UniversalToolAdapter::discover_tools()` |
| Manifest parsing | In `discovery.rs`, `catalog.rs`, `tool_management/catalog.rs` | ✅ ONLY in `UniversalToolAdapter::parse_tool_manifest()` |
| Tool instantiation | In `discovery.rs`, `agent.rs`, `factory.rs` | ✅ Via `UniversalToolAdapter::from_manifest()` |

**Verdict:** No code duplication. All universal tool logic centralized in the adapter.

### 4.3 KISS (Keep It Simple, Stupid)

| Aspect | Assessment |
|--------|------------|
| New abstractions | None - uses existing `UniversalToolAdapter` |
| New traits | None - implements methods on existing struct |
| New dependencies | None - uses existing tokio/fs |
| API surface | Minimal - one new public method on adapter |
| Migration path | Incremental - each call site can migrate independently |

**Verdict:** No unnecessary complexity. The "adapter refactoring" approach is simpler than "full ExtensionManager integration" because it doesn't require changing how agents manage extensions.

### 4.4 ADR-017 Alignment

| ADR-017 Requirement | Implementation |
|---------------------|----------------|
| "All extension types route through ExtensionCore" | ✅ Universal tools use `UniversalToolAdapter` which registers hooks with ExtensionCore |
| "No parallel registries" | ✅ After migration, only ExtensionCore (via adapters) manages tools |
| "Unified lifecycle management" | ✅ ExtensionManager manages; adapters implement type-specific logic |
| "Type-specific adapters for guided approach" | ✅ `UniversalToolAdapter` is the type-specific adapter |
| "Hook point registration" | ✅ Adapter resolves `ToolRegister`, `ToolExecute`, `PromptSystemSection` hooks |

**Verdict:** Fully aligned. The migration completes the ADR-017 vision by removing the last "legacy" code path.

---

## 5. Feasibility Assessment

### 5.1 Effort Estimate

| Phase | Effort | Risk |
|-------|--------|------|
| Phase 1: Manager Enhancement | 1 day | Low - additive changes only |
| Phase 2: Adapter Refactoring | 1 day | Low - internal implementation change |
| Phase 3: Call Site Migration | 2 days | Medium - 6 locations, needs testing |
| Phase 4: Deprecation Cleanup | 0.5 day | Low - documentation change |
| **Total** | **4.5 days** | **Low-Medium** |

### 5.2 Risk Mitigation

| Risk | Mitigation |
|------|------------|
| Regression in tool loading | Comprehensive E2E tests for tool discovery |
| Manifest parsing differences | Copy exact logic from deprecated function |
| Performance regression | No new async boundaries; same I/O pattern |
| Breaking changes | Keep all public APIs; only internal implementation changes |

### 5.3 Testing Strategy

```rust
// Test 1: Adapter discovers same tools as legacy function
#[tokio::test]
async fn test_adapter_parity_with_legacy() {
    let temp = TempDir::new().unwrap();
    create_test_tool(temp.path(), "test_tool").await;
    
    let legacy = discover_universal_tools(temp.path()).await.unwrap();
    let adapter = UniversalToolAdapter::new();
    let modern = adapter.discover_tools(temp.path()).await;
    
    assert_eq!(legacy.len(), modern.len());
    assert_eq!(legacy[0].name, modern[0].manifest.name);
}

// Test 2: Agent loads tools without deprecation warnings
#[tokio::test]
async fn test_agent_no_deprecated_calls() {
    // Run agent initialization and capture logs
    // Assert no "deprecated" warnings in logs
}

// Test 3: Factory loads custom tools correctly
#[tokio::test]
async fn test_factory_custom_tools() {
    // Create workspace with custom tools/
    // Call ToolFactory::create_tools_async()
    // Verify all expected tools loaded
}
```

---

## 6. Migration Checklist

### Phase 1: Manager Enhancement
- [ ] Add `ExtensionManager::scan_directory()` method
- [ ] Add `ExtensionManager::get_tool_instances()` method (optional/future)
- [ ] Add `DiscoveredExtension` struct
- [ ] Unit tests for new methods

### Phase 2: Adapter Refactoring
- [ ] Refactor `UniversalToolAdapter::discover_tools()` to not call deprecated function
- [ ] Add `UniversalToolAdapter::find_executable()` helper
- [ ] Ensure parity with legacy discovery logic
- [ ] Unit tests for refactored methods

### Phase 3: Call Site Migration
- [ ] Migrate `src/agent/agent.rs:191`
- [ ] Migrate `src/tools/factory.rs:916`
- [ ] Migrate `src/cap/catalog.rs:125`
- [ ] Migrate `src/commands/tool.rs:252`
- [ ] Migrate `src/tool_management/catalog.rs:113`
- [ ] Verify `src/extensions/adapters/universal_tool_adapter.rs:81` now uses internal logic

### Phase 4: Verification
- [ ] E2E test: Tool discovery works
- [ ] E2E test: No deprecation warnings in logs
- [ ] E2E test: Custom tools from workspace load correctly
- [ ] E2E test: System-wide tools load correctly

### Phase 5: Cleanup
- [ ] Update deprecation attributes with clearer messages
- [ ] Add doc comments pointing to new methods
- [ ] Update migration documentation

---

## 7. Conclusion

This migration plan:

1. **Eliminates deprecation warnings** by removing all calls to deprecated functions
2. **Maintains SRP** by keeping responsibilities clear and separated
3. **Ensures DRY** by centralizing discovery logic in the adapter
4. **Follows KISS** by using existing patterns without new abstractions
5. **Aligns with ADR-017** by routing all tool management through the extension architecture

The **adapter refactoring approach** is preferred over full ExtensionManager integration because:
- Lower risk (incremental changes)
- Faster implementation (4.5 days vs 2+ weeks)
- No breaking changes to public APIs
- Can evolve to full ExtensionManager integration later

**Recommendation:** Proceed with Phase 1 and 2 immediately (low risk), then Phase 3 with E2E test coverage.
