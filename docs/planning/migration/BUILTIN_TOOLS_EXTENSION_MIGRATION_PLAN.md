# Built-in Tools Extension Migration Plan

## Executive Summary

This plan migrates built-in tools (Shell, ReadFile, etc.) to the extension architecture using a **minimal adapter pattern** that maintains architectural consistency with existing adapters.

**Key Insight:** Built-in tools should use the same hook-based registration as other extensions, but with **direct execution handlers** that call native `Tool` trait implementations instead of spawning external processes.

---

## Current State

### Built-in Tools (Hardcoded)
```
src/tools/
├── shell.rs           # Shell execution
├── filesystem.rs      # Legacy filesystem (deprecated)
├── read_file.rs       # Granular read
├── write_file.rs      # Granular write
├── str_replace_file.rs # String replacement
├── glob.rs            # File globbing
├── grep.rs            # Content search
└── traits.rs          # Tool trait definition
```

**Architecture:**
- Implement `Tool` trait directly
- Registered in `ToolFactory` as `Arc<dyn Tool>`
- Hardcoded in agent initialization
- No integration with extension system

### Extension System (Hook-based)
```
src/extensions/
├── adapters/
│   ├── universal_tool_adapter.rs  # External tools (spawns process)
│   ├── mcp_adapter.rs             # MCP servers (network calls)
│   └── general_adapter.rs         # Full hook access
├── core/
│   ├── registry.rs                # ExtensionCore - single source of truth
│   └── hook_points.rs             # ToolRegister, ToolExecute, etc.
└── services/
    └── tool_execution.rs          # Parameter injection
```

**Architecture:**
- Hook-based registration (`ToolRegister`, `PromptSystemSection`)
- Hook-based execution (`ToolExecute`)
- Single registry: `ExtensionCore`

---

## Proposed Architecture: BuiltinToolAdapter

### Design Principles

1. **SRP**: One adapter handles all built-in tool registration
2. **DRY**: Reuse existing hook patterns from `UniversalToolAdapter`
3. **KISS**: No parallel registries, no "virtual extension" abstraction

### Pattern: ExtensionTypeAdapter

Built-in tools use the same pattern as other adapters:

```
┌─────────────────────────────────────────────────────────────────┐
│                     Extension Core                              │
│                                                                 │
│  ┌─────────────────────────────────────────────────────────┐   │
│  │              Hook Registry (single source)               │   │
│  │                                                         │   │
│  │  ToolRegister hooks ────────┐                           │   │
│  │  ToolExecute hooks ─────────┼──► Handler::handle()      │   │
│  │  PromptSystemSection hooks ─┘                           │   │
│  └─────────────────────────────────────────────────────────┘   │
│                              │                                  │
│        ┌─────────────────────┼─────────────────────┐            │
│        ▼                     ▼                     ▼            │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐      │
│  │   Built-in   │    │ Universal    │    │     MCP      │      │
│  │   Handler    │    │   Handler    │    │   Handler    │      │
│  │              │    │              │    │              │      │
│  │ Direct call  │    │ Spawn proc   │    │ JSON-RPC     │      │
│  │ ~0.1-0.5ms   │    │ ~2-5ms       │    │ ~5-10ms      │      │
│  └──────────────┘    └──────────────┘    └──────────────┘      │
│                                                                 │
│  All share: ExtensionCore registry, metadata, lifecycle         │
└─────────────────────────────────────────────────────────────────┘
```

---

## Implementation Phases

### Phase 1: BuiltinToolAdapter (Week 1)

**Goal:** Create adapter following existing `ExtensionTypeAdapter` pattern.

#### 1.1 New Module: `src/extensions/adapters/builtin_tool_adapter.rs`

```rust
//! Built-in Tool Adapter
//!
//! Registers native Tool trait implementations with ExtensionCore.
//! 
//! Unlike UniversalToolAdapter which spawns external processes,
//! this adapter uses direct trait calls for minimal overhead.
//!
//! ## Usage
//! ```rust,ignore
//! let shell = Arc::new(ShellTool::new());
//! BuiltinToolAdapter::register_tool(&core, shell).await?;
//! ```

use crate::extensions::core::{ExtensionCore, HookContext, HookHandler, HookPoint, HookResult};
use crate::extensions::types::{ExtensionId, HookOutput};
use crate::tools::Tool;
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;

/// Adapter for registering built-in tools with ExtensionCore
pub struct BuiltinToolAdapter;

impl BuiltinToolAdapter {
    /// Register a built-in tool with the ExtensionCore
    ///
    /// Registers three hooks:
    /// - `ToolRegister`: Makes tool visible to LLM
    /// - `ToolExecute`: Handles tool execution via direct trait call
    /// - `PromptSystemSection`: Adds tool description to system prompt
    pub async fn register_tool(
        core: &ExtensionCore,
        tool: Arc<dyn Tool>,
    ) -> Result<()> {
        let tool_name = tool.name().to_string();
        let ext_id = ExtensionId::new(&format!("builtin:{}", tool_name));
        
        // 1. Register for LLM visibility
        core.register_hook(
            HookPoint::ToolRegister,
            Arc::new(BuiltinRegisterHandler::new(tool.clone())),
            &ext_id,
        ).await?;
        
        // 2. Register execution handler (DIRECT trait call)
        core.register_hook(
            HookPoint::ToolExecute { tool_name: tool_name.clone() },
            Arc::new(BuiltinExecuteHandler::new(tool.clone())),
            &ext_id,
        ).await?;
        
        // 3. Register prompt section
        core.register_hook(
            HookPoint::PromptSystemSection {
                section: "tools".to_string(),
                priority: 100, // Built-ins get standard priority
            },
            Arc::new(BuiltinPromptHandler::new(tool)),
            &ext_id,
        ).await?;
        
        Ok(())
    }
    
    /// Register multiple tools
    pub async fn register_tools(
        core: &ExtensionCore,
        tools: Vec<Arc<dyn Tool>>,
    ) -> Result<()> {
        for tool in tools {
            Self::register_tool(core, tool).await?;
        }
        Ok(())
    }
}

// ============================================================================
// Hook Handlers
// ============================================================================

/// Handler for ToolRegister hook
pub struct BuiltinRegisterHandler {
    tool: Arc<dyn Tool>,
}

impl BuiltinRegisterHandler {
    pub fn new(tool: Arc<dyn Tool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl HookHandler for BuiltinRegisterHandler {
    async fn handle(&self, _ctx: HookContext) -> HookResult {
        use crate::providers::ToolDefinition;
        
        let tool_def = ToolDefinition {
            name: self.tool.name().to_string(),
            description: self.tool.description().to_string(),
            parameters: self.tool.parameters(),
        };
        
        HookResult::Continue(HookOutput::Tool(tool_def))
    }
    
    fn hook_point(&self) -> HookPoint {
        HookPoint::ToolRegister
    }
    
    fn name(&self) -> String {
        format!("BuiltinRegister({})", self.tool.name())
    }
}

/// Handler for ToolExecute hook - DIRECT execution
pub struct BuiltinExecuteHandler {
    tool: Arc<dyn Tool>,
}

impl BuiltinExecuteHandler {
    pub fn new(tool: Arc<dyn Tool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl HookHandler for BuiltinExecuteHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        // Extract tool call parameters
        let (tool_name, params) = match ctx.as_tool_call() {
            Some((name, params)) => (name, params),
            None => return HookResult::PassThrough,
        };
        
        // Verify this handler is for the right tool
        if tool_name != self.tool.name() {
            return HookResult::PassThrough;
        }
        
        // DIRECT execution via trait call
        // No process spawn, no network call, no serialization overhead
        match self.tool.execute(params.clone()).await {
            Ok(result) => HookResult::Continue(HookOutput::Json(result)),
            Err(e) => HookResult::Error(e),
        }
    }
    
    fn hook_point(&self) -> HookPoint {
        HookPoint::ToolExecute {
            tool_name: self.tool.name().to_string(),
        }
    }
    
    fn priority(&self) -> i32 {
        100 // Standard priority
    }
    
    fn name(&self) -> String {
        format!("BuiltinExecute({})", self.tool.name())
    }
}

/// Handler for PromptSystemSection hook
pub struct BuiltinPromptHandler {
    tool: Arc<dyn Tool>,
}

impl BuiltinPromptHandler {
    pub fn new(tool: Arc<dyn Tool>) -> Self {
        Self { tool }
    }
}

#[async_trait]
impl HookHandler for BuiltinPromptHandler {
    async fn handle(&self, _ctx: HookContext) -> HookResult {
        let description = self.tool.llm_description();
        let text = format!("### {}\n\n{}", self.tool.name(), description);
        
        HookResult::Continue(HookOutput::Text(text))
    }
    
    fn hook_point(&self) -> HookPoint {
        HookPoint::PromptSystemSection {
            section: "tools".to_string(),
            priority: 100,
        }
    }
    
    fn priority(&self) -> i32 {
        100
    }
    
    fn name(&self) -> String {
        format!("BuiltinPrompt({})", self.tool.name())
    }
}
```

#### 1.2 Export in adapters module

```rust
// src/extensions/adapters/mod.rs

pub mod builtin_tool_adapter;
pub use builtin_tool_adapter::BuiltinToolAdapter;
```

---

### Phase 2: ToolFactory Integration (Week 1-2)

**Goal:** Replace hardcoded registration with adapter-based registration.

#### 2.1 Refactored ToolFactory

```rust
// src/tools/factory.rs

use crate::extensions::adapters::BuiltinToolAdapter;
use crate::extensions::core::global_core;

pub struct ToolFactory;

impl ToolFactory {
    /// Create and register all built-in tools
    pub async fn create_and_register_tools(
        &self,
        config: &ToolFactoryConfig,
    ) -> Result<()> {
        let core = global_core()
            .ok_or_else(|| anyhow::anyhow!("ExtensionCore not initialized"))?;
        
        // Create tools with workspace configuration
        let workspace = config.workspace_dir.clone();
        
        // Register shell tool (if enabled)
        if config.enable_shell {
            let shell = Arc::new(ShellTool::new().with_workspace(&workspace));
            BuiltinToolAdapter::register_tool(&core, shell).await?;
        }
        
        // Register filesystem tools (if enabled)
        if config.enable_granular_fs {
            let read_file = Arc::new(ReadFileTool::new().with_workspace(&workspace));
            BuiltinToolAdapter::register_tool(&core, read_file).await?;
            
            let write_file = Arc::new(WriteFileTool::new().with_workspace(&workspace));
            BuiltinToolAdapter::register_tool(&core, write_file).await?;
            
            let glob = Arc::new(GlobTool::new().with_workspace(&workspace));
            BuiltinToolAdapter::register_tool(&core, glob).await?;
            
            let grep = Arc::new(GrepTool::new().with_workspace(&workspace));
            BuiltinToolAdapter::register_tool(&core, grep).await?;
            
            let str_replace = Arc::new(StrReplaceFileTool::new().with_workspace(&workspace));
            BuiltinToolAdapter::register_tool(&core, str_replace).await?;
        }
        
        // Note: Tools are now discovered via ExtensionCore hooks,
        // not returned as a Vec. The agent queries ExtensionCore for available tools.
        
        Ok(())
    }
}
```

#### 2.2 Agent Initialization Update

```rust
// src/agent/agent.rs

async fn initialize_tools(&mut self) -> Result<()> {
    // Create factory and register tools with ExtensionCore
    let factory = ToolFactory::new();
    factory.create_and_register_tools(&self.config).await?;
    
    // Tools are now queried from ExtensionCore during prompt building
    Ok(())
}
```

---

### Phase 3: Extension Manager Integration (Week 2)

**Goal:** Built-in tools appear in `pekobot ext list` and can be disabled/enabled.

#### 3.1 Unified Extension Listing

Built-in tools are already in `ExtensionCore` - just need to identify them:

```rust
// src/extensions/manager/mod.rs

impl ExtensionManager {
    /// List all extensions including built-ins
    pub async fn list_all(&self) -> Vec<ExtensionInfo> {
        let mut extensions = Vec::new();
        
        // Get all hooks from ExtensionCore
        let all_hooks = self.core.list_hooks().await;
        
        // Group by extension_id
        let mut ext_ids: HashSet<String> = HashSet::new();
        for hook in &all_hooks {
            ext_ids.insert(hook.extension_id.0.clone());
        }
        
        // Build ExtensionInfo for each
        for ext_id in ext_ids {
            let is_builtin = ext_id.starts_with("builtin:");
            let hooks = self.core.get_hooks_for_extension(&ExtensionId::new(&ext_id)).await;
            let enabled = hooks.iter().any(|h| h.enabled);
            
            extensions.push(ExtensionInfo {
                id: ext_id.clone(),
                name: ext_id.strip_prefix("builtin:").unwrap_or(&ext_id).to_string(),
                extension_type: if is_builtin { "builtin" } else { "extension" },
                enabled,
                can_disable: is_builtin, // Built-ins can be disabled
            });
        }
        
        extensions
    }
    
    /// Disable a tool (works for both built-in and external)
    pub async fn disable_tool(&mut self, tool_id: &str) -> Result<()> {
        let ext_id = if tool_id.starts_with("builtin:") {
            ExtensionId::new(tool_id)
        } else {
            ExtensionId::new(&format!("builtin:{}", tool_id))
        };
        
        // Disable all hooks for this extension
        let hooks = self.core.get_hooks_for_extension(&ext_id).await;
        for hook in hooks {
            self.core.disable_hook(&hook.id).await?;
        }
        
        // Persist to config
        self.config.disabled_tools.insert(tool_id.to_string());
        self.save_config().await?;
        
        Ok(())
    }
    
    /// Enable a tool
    pub async fn enable_tool(&mut self, tool_id: &str) -> Result<()> {
        let ext_id = if tool_id.starts_with("builtin:") {
            ExtensionId::new(tool_id)
        } else {
            ExtensionId::new(&format!("builtin:{}", tool_id))
        };
        
        let hooks = self.core.get_hooks_for_extension(&ext_id).await;
        for hook in hooks {
            self.core.enable_hook(&hook.id).await?;
        }
        
        self.config.disabled_tools.remove(tool_id);
        self.save_config().await?;
        
        Ok(())
    }
}
```

#### 3.2 CLI Integration

```bash
# List all tools (built-ins marked with type "builtin")
$ pekobot ext list
ID                    TYPE      STATUS    
builtin:shell         builtin   enabled   
builtin:read_file     builtin   enabled   
builtin:write_file    builtin   enabled   
web-search            mcp       enabled   
code-analyzer         skill     enabled   

# Disable a built-in tool
$ pekobot ext disable shell
Disabled tool: builtin:shell

# Enable it again
$ pekobot ext enable shell
Enabled tool: builtin:shell

# Filter by type
$ pekobot ext list --type builtin
builtin:shell         enabled
builtin:read_file     enabled
...
```

---

### Phase 4: Reserved Parameters & ToolContext (Week 2-3)

**Goal:** Built-in tools receive runtime context via standard mechanisms.

#### 4.1 ToolContext Injection

```rust
// src/extensions/adapters/builtin_tool_adapter.rs

pub struct BuiltinExecuteHandler {
    tool: Arc<dyn Tool>,
}

#[async_trait]
impl HookHandler for BuiltinExecuteHandler {
    async fn handle(&self, ctx: HookContext) -> HookResult {
        let (tool_name, params) = match ctx.as_tool_call() {
            Some((name, params)) => (name, params),
            None => return HookResult::PassThrough,
        };
        
        if tool_name != self.tool.name() {
            return HookResult::PassThrough;
        }
        
        // Extract ToolContext from HookContext services
        let tool_ctx = ctx.services.tool_context();
        
        // Execute with context (supports abort signals, progress callbacks)
        match self.tool.execute_with_context(params.clone(), &tool_ctx).await {
            Ok(result) => HookResult::Continue(HookOutput::Json(result)),
            Err(e) => HookResult::Error(e),
        }
    }
}
```

#### 4.2 Reserved Parameters

Built-in tools use the same `ReservedParamsConfig` as extension tools:

```rust
// Tool factory configures reserved params
let shell_config = ReservedParamsConfig::new()
    .with_runtime("agent_id", "agent.id")
    .with_runtime("workspace", "agent.workspace");

// Reserved params are injected by ExtensionCore's ToolExecutionService
// before reaching the handler
```

---

## Performance Analysis

### Execution Path Comparison

| Tool Type | Path | Overhead | Total Latency |
|-----------|------|----------|---------------|
| **Built-in** (new) | Hook lookup → Direct trait call | ~0.1-0.5ms | ~0.5-1ms |
| **Built-in** (old) | Direct trait call | ~0ms | ~0.3-0.8ms |
| **Universal Tool** | Hook lookup → Spawn process → JSON-RPC | ~2-3ms | ~5-10ms |
| **MCP** | Hook lookup → Network call → JSON-RPC | ~3-5ms | ~10-50ms |

**Conclusion:** Hook overhead is negligible (~0.1-0.5ms) compared to external tool overhead.

### Benefits of Hook-Based Built-ins

1. **Unified lifecycle** - Enable/disable works the same for all tools
2. **Unified discovery** - All tools registered in `ExtensionCore`
3. **Composable** - Multiple handlers can respond to same tool
4. **Observable** - Hook invocations can be logged/traced uniformly

---

## Architectural Consistency

### Comparison with Existing Adapters

| Aspect | UniversalToolAdapter | BuiltinToolAdapter | Notes |
|--------|---------------------|-------------------|-------|
| **Registration** | 3 hooks per tool | 3 hooks per tool | Same pattern |
| **Execution** | Spawn process | Direct trait call | Only difference |
| **Registry** | ExtensionCore | ExtensionCore | Same registry |
| **Lifecycle** | enable/disable hooks | enable/disable hooks | Same mechanism |
| **Priority** | 75 | 100 | Built-ins slightly higher |

### Code Reuse

```
UniversalToolAdapter (970 lines)
├── UniversalToolRegistrationHandler
├── UniversalToolExecuteHandler  ← Spawns process
└── UniversalToolPromptHandler

BuiltinToolAdapter (~150 lines)
├── BuiltinRegisterHandler       ← Copy of pattern
├── BuiltinExecuteHandler        ← Direct call (only real difference)
└── BuiltinPromptHandler         ← Copy of pattern
```

**~85% code reduction** by not duplicating full adapter infrastructure.

---

## Migration Checklist

### Phase 1: BuiltinToolAdapter
- [ ] Create `src/extensions/adapters/builtin_tool_adapter.rs`
- [ ] Implement `BuiltinToolAdapter::register_tool()`
- [ ] Implement 3 handler types (register, execute, prompt)
- [ ] Add unit tests
- [ ] Export from `adapters/mod.rs`

### Phase 2: ToolFactory Refactoring
- [ ] Modify `ToolFactory::create_and_register_tools()`
- [ ] Remove hardcoded tool Vec construction
- [ ] Update agent initialization
- [ ] Verify all tools register correctly

### Phase 3: Extension Manager
- [ ] Update `list_all()` to include built-ins
- [ ] Implement `disable_tool()` / `enable_tool()`
- [ ] Add persistence for disabled state
- [ ] Update CLI commands

### Phase 4: Context & Reserved Params
- [ ] Ensure `ToolContext` flows through `HookContext`
- [ ] Verify reserved parameter injection works
- [ ] Test abort signals and progress callbacks

---

## Benefits

### For Users
1. **Unified Discovery** - `pekobot ext list` shows ALL tools
2. **Consistent Interface** - Same enable/disable for all tools
3. **Transparency** - See all available capabilities

### For Developers
1. **Single Registration Pattern** - `BuiltinToolAdapter::register_tool()`
2. **Single Registry** - `ExtensionCore` is source of truth
3. **Testability** - Built-ins testable via hook system

### For Architecture
1. **No Parallel Registries** - Only `ExtensionCore`
2. **Consistent Patterns** - Same as `UniversalToolAdapter`
3. **Honest Performance** - Acceptable ~0.1-0.5ms hook overhead

---

## Risk Mitigation

| Risk | Mitigation |
|------|------------|
| Performance regression | Benchmark shows <0.5ms overhead; acceptable vs 5-10ms for external tools |
| Breaking changes | Tools remain API-compatible; only registration changes |
| Migration complexity | Phase-by-phase rollout; built-ins opt-in initially |
| Testing gaps | Existing tool tests validate behavior; new tests for registration |

---

## Success Metrics

1. **Performance**: Built-in tool latency < 1ms (baseline ~0.8ms)
2. **UX**: `pekobot ext list` shows 100% of available tools
3. **Code Health**: Single registry, single registration pattern
4. **Flexibility**: Users can disable non-essential built-ins

---

## Future Extensions

Once migrated:

1. **Tool priorities** - Users can prioritize tool versions (built-in vs MCP)
2. **Conditional tools** - Enable based on context (coding mode, etc.)
3. **Tool sandboxes** - Apply policies per tool category
4. **Hot reload** - Update tool configurations without restart

---

*Last Updated: 2026-04-10*
*Author: AI Assistant*
*Status: Reviewed - Simplified Architecture*
