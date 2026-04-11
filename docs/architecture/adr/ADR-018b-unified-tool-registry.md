# ADR-018b: Unified Tool Registry

**Status**: Proposed  
**Date**: 2026-04-11  
**Author**: Kimi Code CLI  
**Related**: ADR-017 (Extensions 2.0), ADR-018a (Tool Execution Unification), ADR-018c (Tool Naming Cleanup)

## Context

Tool metadata is currently stored in **multiple places** with no single source of truth:
- `AgenticLoopV4.tools: Vec<Arc<dyn Tool>>` - execution lookup
- `ExtensionCore.hooks` - hook-based registration
- `McpManager` - MCP-specific storage
- `ToolFactory` output - temporary during creation

This creates:
- Inconsistent whitelist enforcement
- Duplicate tool storage
- Registration path fragmentation
- Stale tool metadata

## Problem Statement

### Current State: Multiple Registries

```
┌─────────────────────────────────────────────────────────────────────────┐
│                    CURRENT TOOL STORAGE (Fragmented)                    │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                         │
│  AgenticLoopV4                                                          │
│  ───────────────                                                        │
│  tools: Vec<Arc<dyn Tool>>                                              │
│       ├── Built-in tools                                                │
│       ├── MCP tools (proxies)                                           │
│       └── Universal tools                                               │
│  Lookup: O(n) linear search                                             │
│  Whitelist: Checked at Agent::create_tools()                            │
│                                                                         │
│  ExtensionCore                                                          │
│  ───────────────                                                        │
│  hooks: HashMap<HookId, RegisteredHook>                                 │
│       ├── HookPoint::ToolRegister                                       │
│       ├── HookPoint::ToolExecute { name }                               │
│       └── HookPoint::PromptSystemSection                                │
│  Lookup: Via HookPoint name matching                                    │
│  Whitelist: NOT enforced                                                │
│                                                                         │
│  McpManager                                                             │
│  ──────────                                                             │
│  tools: HashMap<(server, name), McpToolProxy>                           │
│  Lookup: Direct HashMap access                                          │
│  Whitelist: NOT enforced                                                │
│                                                                         │
│  ToolFactory                                                            │
│  ──────────                                                             │
│  Creates tools but doesn't own them                                     │
│  Uses DisabledToolFilter (blacklist, not whitelist)                     │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

### Inconsistency Issues

| Aspect | Built-in | MCP | Universal |
|--------|----------|-----|-----------|
| **Storage** | AgenticLoopV4.tools | McpManager | ExtensionCore.hooks |
| **Whitelist** | ToolConfig.enabled | disabled_tools | None |
| **Reserved params** | None | ToolExecutionService | ToolExecutionService |
| **Lookup** | O(n) linear | O(1) hashmap | Hook matching |

## Decision

**Extend ExtensionCore's hook registry to be the single source of truth for ALL tool metadata**, rather than creating a parallel structure.

### Design Rationale

Instead of creating a new `UnifiedToolRegistry` (parallel structure), we:
1. **Extend** `ExtensionCore` to track tool metadata alongside hooks
2. **Use** existing `HookPoint::ToolRegister` for ALL tool registration
3. **Enforce** whitelist at registration time
4. **Provide** unified lookup via ExtensionCore API

## Implementation

### Phase 1: Extend ExtensionCore with Tool Metadata

```rust
// src/extensions/core/registry.rs

/// Extended RegisteredHook with tool metadata
pub struct RegisteredHook {
    pub id: HookId,
    pub extension_id: ExtensionId,
    pub point: HookPoint,
    pub handler: Arc<dyn HookHandler>,
    pub priority: HookPriority,
    pub enabled: bool,
    
    // NEW: Tool metadata (only populated for ToolRegister hooks)
    pub tool_metadata: Option<ToolMetadata>,
}

/// Tool metadata stored with registration hook
pub struct ToolMetadata {
    pub name: String,
    pub description: String,
    pub parameters: ParameterSchema,
    pub source: ToolSource, // BuiltIn, Mcp, Universal, General
    pub reserved_params: ReservedParamsConfig,
}

pub enum ToolSource {
    BuiltIn,
    Mcp { server: String },
    Universal { extension_id: String },
    General { extension_id: String },
}
```

### Phase 2: Add Tool Registry Methods to ExtensionCore

```rust
// src/extensions/core/mod.rs

impl ExtensionCore {
    // ... existing hook methods ...
    
    /// Register a tool (called by adapters)
    pub async fn register_tool(
        &self,
        metadata: ToolMetadata,
        execute_handler: Arc<dyn HookHandler>,
        extension_id: &ExtensionId,
    ) -> Result<()> {
        // Check whitelist
        if !self.is_tool_enabled(&metadata.name).await {
            return Err(anyhow!("Tool '{}' is not enabled", metadata.name));
        }
        
        // Check for duplicates
        if self.get_tool_metadata(&metadata.name).await.is_some() {
            return Err(anyhow!("Tool '{}' already registered", metadata.name));
        }
        
        // Register the tool metadata
        let mut hooks = self.hooks.write().await;
        let hook_id = HookId::new();
        
        hooks.insert(hook_id, RegisteredHook {
            id: hook_id,
            extension_id: extension_id.clone(),
            point: HookPoint::ToolRegister,
            handler: execute_handler.clone(),
            priority: DEFAULT_PRIORITY,
            enabled: true,
            tool_metadata: Some(metadata.clone()),
        });
        
        // Register execution hook
        let exec_hook_id = HookId::new();
        hooks.insert(exec_hook_id, RegisteredHook {
            id: exec_hook_id,
            extension_id: extension_id.clone(),
            point: HookPoint::ToolExecute { 
                tool_name: metadata.name.clone() 
            },
            handler: execute_handler,
            priority: DEFAULT_PRIORITY,
            enabled: true,
            tool_metadata: None,
        });
        
        // Index by tool name for fast lookup
        let mut tool_index = self.tool_index.write().await;
        tool_index.insert(metadata.name.clone(), hook_id);
        
        Ok(())
    }
    
    /// Get tool metadata by name (O(1))
    pub async fn get_tool_metadata(&self, name: &str) -> Option<ToolMetadata> {
        let tool_index = self.tool_index.read().await;
        let hooks = self.hooks.read().await;
        
        tool_index.get(name)
            .and_then(|hook_id| hooks.get(hook_id))
            .and_then(|hook| hook.tool_metadata.clone())
    }
    
    /// List all registered tools (for prompts)
    pub async fn list_tools(&self) -> Vec<ToolMetadata> {
        let hooks = self.hooks.read().await;
        
        hooks.values()
            .filter(|h| h.point == HookPoint::ToolRegister)
            .filter(|h| h.enabled)
            .filter_map(|h| h.tool_metadata.clone())
            .filter(|m| self.is_tool_enabled_sync(&m.name))
            .collect()
    }
    
    /// Check if tool is enabled (whitelist)
    async fn is_tool_enabled(&self, name: &str) -> bool {
        let config = self.tool_config.read().await;
        config.is_tool_enabled(name)
    }
}
```

### Phase 3: Update Adapters to Use Unified Registration

**Built-in Adapter**:
```rust
// src/extensions/adapters/builtin_tool_adapter.rs

impl BuiltinToolAdapter {
    async fn register_tool(
        &self,
        core: &ExtensionCore,
        tool: Arc<dyn Tool>,
    ) -> Result<()> {
        let metadata = ToolMetadata {
            name: tool.name().to_string(),
            description: tool.llm_description(), // Use LLM version
            parameters: tool.parameters(),
            source: ToolSource::BuiltIn,
            reserved_params: self.reserved_params.clone(),
        };
        
        let handler = Arc::new(BuiltinExecuteHandler {
            tool: Arc::clone(&tool),
        });
        
        // Single registration call
        core.register_tool(metadata, handler, &self.extension_id).await?;
        
        Ok(())
    }
}
```

**MCP Adapter**:
```rust
// src/extensions/adapters/mcp_adapter.rs

impl McpAdapter {
    async fn register_tools(&self, core: &ExtensionCore) -> Result<()> {
        let tools = self.manager.list_all_tools().await?;
        
        for tool in tools {
            let metadata = ToolMetadata {
                name: format!("mcp:{}:{}", tool.server_name, tool.name),
                description: tool.description,
                parameters: tool.input_schema,
                source: ToolSource::Mcp { 
                    server: tool.server_name.clone() 
                },
                reserved_params: tool.reserved_params,
            };
            
            let handler = Arc::new(McpToolExecuteHandler {
                manager: Arc::clone(&self.manager),
                server: tool.server_name,
                tool: tool.name,
            });
            
            // Same registration API
            core.register_tool(metadata, handler, &self.extension_id).await?;
        }
        
        Ok(())
    }
}
```

### Phase 4: Migrate ToolFactory to Use ExtensionCore

```rust
// src/tools/factory.rs

impl ToolFactory {
    pub async fn create_tools_async(
        &self,
        extension_core: &Arc<ExtensionCore>, // Now required
    ) -> Result<Vec<Arc<dyn Tool>>> {
        // Built-in tools: register via ExtensionCore
        self.register_builtin_tools(extension_core).await?;
        
        // MCP tools: register via ExtensionCore
        self.register_mcp_tools(extension_core).await?;
        
        // Universal tools: register via ExtensionCore
        self.register_universal_tools(extension_core).await?;
        
        // Return tool references (optional - can be removed later)
        Ok(self.get_tool_refs(extension_core).await)
    }
}
```

### Phase 5: Remove Legacy Storage

After migration, remove or deprecate:

| Legacy Component | Action |
|-----------------|--------|
| `AgenticLoopV4.tools: Vec<Arc<dyn Tool>>` | Remove, use `ExtensionCore::get_tool()` |
| `McpManager.tools` | Remove, use ExtensionCore |
| `ToolFactory` tool storage | Remove, delegate to ExtensionCore |
| `DisabledToolFilter` | Remove, whitelist enforced in ExtensionCore |

## Benefits

| Benefit | Description |
|---------|-------------|
| **Single Source of Truth** | One registry for all tool metadata |
| **Consistent Whitelist** | Enforced at registration time for ALL tools |
| **O(1) Lookup** | HashMap index by tool name |
| **No Duplication** | Hooks and metadata stored together |
| **Extensible** | New tool types use same registration API |

## Drawbacks

| Drawback | Mitigation |
|----------|------------|
| **Tighter coupling** | ExtensionCore becomes larger |
| **Migration complexity** | Multiple adapters to update |
| **Async complexity** | Registration is now async throughout |

## Acceptance Criteria

- [ ] ExtensionCore tracks tool metadata alongside hooks
- [ ] Built-in tools register via `ExtensionCore::register_tool()`
- [ ] MCP tools register via `ExtensionCore::register_tool()`
- [ ] Universal tools register via `ExtensionCore::register_tool()`
- [ ] Whitelist enforced consistently at registration
- [ ] `ExtensionCore::get_tool_metadata()` provides O(1) lookup
- [ ] `ExtensionCore::list_tools()` returns all enabled tools
- [ ] Legacy storage locations deprecated

## Blocked By

None - this is foundational and can be implemented first.

## Blocks

- **ADR-018a**: Tool Execution Unification (needs unified lookup)
- **ADR-019**: Dynamic Tool Updates (needs single registry to update)

## References

- ExtensionCore: `src/extensions/core/mod.rs`
- Hook registry: `src/extensions/core/registry.rs`
- ToolFactory: `src/tools/factory.rs`
- McpManager: `src/mcp/manager.rs`
