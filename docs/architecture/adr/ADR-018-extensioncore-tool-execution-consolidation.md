# ADR-018: ExtensionCore Tool Execution Consolidation

**Status**: Proposed  
**Date**: 2026-04-11  
**Author**: Kimi Code CLI  
**Related**: ADR-017 (Extensions 2.0), ADR-019 (Dynamic Tool/Prompt Updates - future work)

## Context

The codebase has accumulated significant architectural debt across multiple dimensions of tool handling. This ADR addresses **five critical areas** of fragmentation that create inconsistency, security gaps, and maintenance burden.

---

## Problem Statement: Five Areas of Architectural Debt

### 1. Tool Execution Paths (Dual Path Problem)

**Current State:** Tools execute through two completely different code paths:

| Path | Used By | Features |
|------|---------|----------|
| **Direct** (`ToolExecutor`) | Built-in tools in `AgenticLoopV4` | Panic isolation, timeout, context injection |
| **Hook-based** (`ExtensionCore`) | MCP, Universal, General extensions | Same features + reserved param injection |

**The schism:**
```
AgenticLoopV4::run_loop()
    │
    ├──► Built-in? ──Yes──► ToolExecutor::execute_with_context()
    │                          ├── Panic isolation
    │                          ├── Timeout
    │                          └── Context injection
    │
    └──► Extension tool? ──Yes──► ExtensionCore::invoke_hook()
                                     ├── Hook routing
                                     ├── ToolExecutionService
                                     │       ├── User param validation
                                     │       ├── Reserved param injection
                                     │       └── Schema filtering
                                     └── Panic isolation (duplicated)
```

### 2. Reserved Parameters (Native + Outbound)

**Two categories of reserved parameters** with inconsistent handling:

#### Native Reserved Params (Execution Control)
- `_async`: Route to async execution
- `_timeout`: Execution timeout
- `_callback`: Result delivery mode (queue/stream/blocking)
- `_progress`: Request progress updates
- `_priority`: Task priority
- `_retry`: Retry count

**Current handling:** Extracted in `ToolWrapper::execute()` before routing, but paths handle them differently.

#### Outbound Reserved Params (Context Injection)
- `session_id`: Current session identifier
- `workspace`: Workspace path
- `agent_id`: Agent name
- `run_id`: Execution run identifier
- `peer_id`: Peer agent identifier

**CRITICAL INCONSISTENCY:**

| Tool Type | Reserved Param Validation | Reserved Param Injection | Context Available |
|-----------|---------------------------|--------------------------|-------------------|
| **Built-in** (direct) | ❌ No | ❌ No | Fake values ("unknown") |
| **Built-in** (hook) | ❌ No | ❌ No | Fake values ("unknown") |
| **MCP** | ✅ Yes | ✅ Yes | Real values via `ToolContext` |
| **Universal** | ✅ Yes | ✅ Yes | Real values via `ToolContext` |

**Built-in tools completely bypass `ToolExecutionService`**, missing validation and injection that MCP/Universal tools get.

### 3. System Prompt Build Paths

**Multiple builders with inconsistent behavior:**

| Location | Called When | Tools Included | Skills |
|----------|-------------|----------------|--------|
| `loop_v4::build_system_prompt()` | Constructor | All passed tools | Via ExtensionCore |
| `SystemPromptBuilder::build()` | Called by above | Via `llm_description()` | Via hooks |
| `async_agentic_loop::build_system_prompt_with_async()` | Per-iteration | Same as V4 | Same as V4 |
| `subagent_announce::build_subagent_system_prompt()` | Per spawn | None | No |

**Issues:**
- System prompt built **once at construction** - dynamic tool changes don't appear
- Subagent prompts don't include tools (by design, but inconsistent)
- Async loop "appends" to base prompt instead of rebuilding

### 4. Tool Definition Build Paths

**Tool definitions (schemas sent to LLM) built in multiple places:**

| Location | Method Used | Purpose |
|----------|-------------|---------|
| `loop_v4::build_tool_definitions()` | `llm_description()` | LLM API calls |
| `BuiltinRegisterHandler` | `description()` (NOT `llm_description()`!) | Extension registration |
| `BuiltinPromptHandler` | `llm_description()` | System prompt |
| `UniversalToolRegistrationHandler` | `manifest.description` | Extension registration |
| `UniversalToolPromptHandler` | `manifest.llm_description` or `manifest.description` | System prompt |

**Critical inconsistency:** `BuiltinRegisterHandler` uses `description()` while everyone else uses `llm_description()`. This causes tools to have different descriptions in the ExtensionCore registry vs. what the LLM sees.

### 5. Tool Lifecycle & Registration

**Tools are registered through multiple parallel paths:**

```
┌─────────────────────────────────────────────────────────────────────────┐
│  REGISTRATION PATHS (ALL PARALLEL)                                      │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                         │
│  Path A: Built-in Tools                                                 │
│  ─────────────────                                                      │
│  BuiltinRegistry::register() ──► BuiltinToolAdapter                     │
│       ├─► HookPoint::ToolRegister (for discovery)                       │
│       ├─► HookPoint::ToolExecute (for execution)                        │
│       └─► HookPoint::PromptSystemSection (for prompts)                  │
│                                                                         │
│  Path B: MCP Tools                                                      │
│  ────────────────                                                       │
│  ToolFactory::load_mcp_tools() ──► McpManager ──► McpToolProxy          │
│       ├─► NO HookPoint::ToolRegister (bypasses!)                        │
│       ├─► HookPoint::ToolExecute (via McpAdapter)                       │
│       └─► Stored separately in McpManager                               │
│                                                                         │
│  Path C: Universal Tools                                                │
│  ───────────────────                                                    │
│  ToolFactory::load_custom_tools() ──► ExtensionManager                  │
│       ├─► HookPoint::ToolRegister (via UniversalToolAdapter)            │
│       ├─► HookPoint::ToolExecute                                        │
│       └─► HookPoint::PromptSystemSection                                │
│                                                                         │
│  Path D: AgenticLoopV4.tools                                            │
│  ─────────────────────────                                              │
│  Agent::create_tools() ──► Vec<Arc<dyn Tool>>                           │
│       ├─► Separate storage from ExtensionCore                           │
│       ├─► Linear lookup (not hashmap)                                   │
│       └─► Duplicates tools from other paths                             │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

**No single source of truth.** Tools exist in:
1. `ExtensionCore.hooks` (for hook-based tools)
2. `AgenticLoopV4.tools` (for execution lookup)
3. `McpManager` (for MCP tools)
4. `ToolFactory` output (temporary during creation)

**Whitelist enforcement is inconsistent:**
- `Agent::create_tools()` uses `ToolConfig.enabled` whitelist
- `ToolFactory` uses `disabled_tools` blacklist (opposite semantics!)
- Extension Framework tools bypass both checks

---

## Current Architecture (Fragmented)

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         TOOL EXECUTION LANDSCAPE                            │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  AGENTIC LOOP V4                                                    │    │
│  │  ────────────────                                                   │    │
│  │                                                                     │    │
│  │  Built-in? ──Yes──► self.tools.iter().find()                        │    │
│  │       │                   │                                         │    │
│  │       │                   ▼                                         │    │
│  │       │           ToolExecutor::execute_with_context()              │    │
│  │       │                   │                                         │    │
│  │       │                   ├──► Panic isolation                      │    │
│  │       │                   ├──► Timeout                              │    │
│  │       │                   └──► Context injection ("unknown")        │    │
│  │       │                                                             │    │
│  │       No                                                            │    │
│  │       │                                                             │    │
│  │       ▼                                                             │    │
│  │   ExtensionCore::invoke_hook()                                      │    │
│  │       │                                                             │    │
│  │       ├──► MCP ──► McpAdapter ──► McpManager ──► JSON-RPC           │    │
│  │       │                                                             │    │
│  │       ├──► Universal ──► UniversalToolAdapter ──► Process spawn     │    │
│  │       │                                                             │    │
│  │       └──► Built-in (hook) ──► BuiltinToolAdapter                   │    │
│  │               │                                                     │    │
│  │               ▼                                                     │    │
│  │       tool.execute() [NO ToolExecutionService!]                     │    │
│  │                                                                     │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  RESERVED PARAM HANDLING                                            │    │
│  │  ───────────────────────                                            │    │
│  │                                                                     │    │
│  │  Built-in:           [NONE - direct trait call]                     │    │
│  │  MCP/Universal:      ToolExecutionService                           │    │
│  │       ├──► validate_user_params() - no reserved from user           │    │
│  │       ├──► inject_reserved_params() - merge from config             │    │
│  │       └──► ContextResolver - resolve session_id, workspace, etc.    │    │
│  │                                                                     │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  SYSTEM PROMPT BUILDING                                             │    │
│  │  ──────────────────────                                             │    │
│  │                                                                     │    │
│  │  loop_v4::build_system_prompt()                                     │    │
│  │       ├──► SystemPromptBuilder::build()                             │    │
│  │       │       ├──► build_tools_section() - llm_description()        │    │
│  │       │       └──► ExtensionCore hooks for skills                   │    │
│  │       │                                                             │    │
│  │       └──► load_and_register_skills_sync()                          │    │
│  │               └──► HookPoint::PromptSystemSection                   │    │
│  │                                                                     │    │
│  │  async_loop::build_system_prompt_with_async()                       │    │
│  │       └──► Appends async section to V4 prompt                       │    │
│  │                                                                     │    │
│  │  subagent_announce::build_subagent_system_prompt()                  │    │
│  │       └──► NO tools, custom format                                  │    │
│  │                                                                     │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  TOOL DEFINITION BUILDING                                           │    │
│  │  ────────────────────────                                           │    │
│  │                                                                     │    │
│  │  For LLM API (loop_v4):                                             │    │
│  │       build_tool_definitions() ──► tool.llm_description()           │    │
│  │                                                                     │    │
│  │  For Extension Registration:                                        │    │
│  │       BuiltinRegisterHandler ──► tool.description()  [WRONG!]       │    │
│  │       UniversalRegHandler ──► manifest.description                  │    │
│  │                                                                     │    │
│  │  For System Prompt:                                                 │    │
│  │       BuiltinPromptHandler ──► tool.llm_description()               │    │
│  │       UniversalPromptHandler ──► manifest.llm_description           │    │
│  │                                                                     │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Decision

**Consolidate ALL tool handling through ExtensionCore**, unifying:
1. ✅ **Execution**: All tools via `HookPoint::ToolExecute`
2. ✅ **Reserved Params**: All tools via `ToolExecutionService`
3. ✅ **System Prompt**: Single builder with ExtensionCore hooks
4. ✅ **Tool Definitions**: Single builder with consistent `llm_description()`
5. ✅ **Registration**: All tools via `HookPoint::ToolRegister`

### Target Architecture (Unified)

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         UNIFIED TOOL ARCHITECTURE                           │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  AGENTIC LOOP V4                                                    │    │
│  │  ────────────────                                                   │    │
│  │                                                                     │    │
│  │  ALL tools ───────────────────────► ExtensionCore::invoke_hook()    │    │
│  │                                             │                       │    │
│  │                                             ▼                       │    │
│  │  ┌─────────────────────────────────────────────────────────────┐   │    │
│  │  │              EXTENSIONCORE HOOK SYSTEM                      │   │    │
│  │  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐  │   │    │
│  │  │  │BuiltinAdapter│  │  MCP        │  │  Universal          │  │   │    │
│  │  │  │  Handler    │  │  Handler    │  │  Handler            │  │   │    │
│  │  │  └──────┬──────┘  └──────┬──────┘  └──────────┬──────────┘  │   │    │
│  │  │         │                │                    │             │   │    │
│  │  │         └────────────────┼────────────────────┘             │   │    │
│  │  │                          │                                  │   │    │
│  │  │                          ▼                                  │   │    │
│  │  │              ┌─────────────────────┐                        │   │    │
│  │  │              │ TOOL EXECUTION      │                        │   │    │
│  │  │              │ SERVICE             │                        │   │    │
│  │  │              │ ─────────────────── │                        │   │    │
│  │  │              │ • Validate params   │                        │   │    │
│  │  │              │ • Inject reserved   │                        │   │    │
│  │  │              │ • Context resolution│                        │   │    │
│  │  │              └─────────────────────┘                        │   │    │
│  │  │                          │                                  │   │    │
│  │  │                          ▼                                  │   │    │
│  │  │              ┌─────────────────────┐                        │   │    │
│  │  │              │ TOOL EXECUTOR       │                        │   │    │
│  │  │              │ (implementation)    │                        │   │    │
│  │  │              │ • Panic isolation   │                        │   │    │
│  │  │              │ • Timeout           │                        │   │    │
│  │  │              │ • Execution         │                        │   │    │
│  │  │              └─────────────────────┘                        │   │    │
│  │  └─────────────────────────────────────────────────────────────┘   │    │
│  │                                                                     │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  SYSTEM PROMPT BUILDER (Unified)                                    │    │
│  │  ───────────────────────────────                                    │    │
│  │                                                                     │    │
│  │  SystemPromptBuilder::build()                                       │    │
│  │       ├──► ToolRegistry::get_all_tools()                            │    │
│  │       │       └──► Consistent llm_description() for all             │    │
│  │       ├──► HookPoint::PromptSystemSection                           │    │
│  │       │       └──► Skills, extensions contribute                    │    │
│  │       └──► Reserved params section (global)                         │    │
│  │                                                                     │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  TOOL REGISTRY (Single Source of Truth)                             │    │
│  │  ──────────────────────────────────────                             │    │
│  │                                                                     │    │
│  │  UnifiedToolRegistry {                                              │    │
│  │      tools: HashMap<name, RegisteredTool>,                          │    │
│  │      whitelist_enforced: true,                                      │    │
│  │      reserved_params_config: ReservedParamsConfig,                  │    │
│  │  }                                                                  │    │
│  │                                                                     │    │
│  │  ALL tool types register here:                                      │    │
│  │       ├──► Built-in ──► HookPoint::ToolRegister                     │    │
│  │       ├──► MCP ──► HookPoint::ToolRegister (NEW)                    │    │
│  │       └──► Universal ──► HookPoint::ToolRegister                    │    │
│  │                                                                     │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Implementation

### Phase 1: Fix Reserved Param Handling for Built-in Tools

**Problem**: Built-in tools bypass `ToolExecutionService`, missing validation and injection.

**Changes:**

1. **Update `BuiltinToolAdapter`** to use `ToolExecutionService`:

```rust
// src/extensions/adapters/builtin_tool_adapter.rs
impl BuiltinExecuteHandler {
    async fn handle(&self, ctx: HookContext, input: ToolExecuteInput) -> HookResult {
        let tool = self.registry.get_tool(&input.tool_name)?;
        
        // NEW: Use ToolExecutionService like MCP/Universal
        let exec_service = ctx.services.tool_execution();
        let exec_config = ToolExecutionConfig::new(
            self.reserved_params.clone(), // NEW: Configurable reserved params
            tool.parameters(),
        );
        
        let result = exec_service
            .execute(
                input.params,
                &exec_config,
                ctx.as_tool_context(), // Real context, not "unknown"
                |merged_params| async move {
                    // Execute with merged params (includes session_id, workspace, etc.)
                    tool.execute(merged_params).await
                },
            )
            .await?;
        
        HookResult::Success(ToolExecuteOutput { result })
    }
}
```

2. **Add reserved params config to built-in tools**:

```rust
// src/tools/builtin_registry.rs
pub struct BuiltinRegistryConfig {
    pub disabled_tools: Vec<String>,
    pub reserved_params: ReservedParamsConfig, // NEW
}

// Default config injects standard context
impl Default for BuiltinRegistryConfig {
    fn default() -> Self {
        Self {
            disabled_tools: vec![],
            reserved_params: ReservedParamsConfig::builder()
                .runtime("session_id", "session_id")
                .runtime("workspace", "workspace")
                .runtime("agent_id", "agent_id")
                .build(),
        }
    }
}
```

### Phase 2: Route ALL Tools Through ExtensionCore

**Changes to `AgenticLoopV4`**:

```rust
// src/engine/loop_v4.rs

// BEFORE: Dual path
async fn execute_tool(&self, name: &str, params: Value) -> Result<Value> {
    if let Some(tool) = self.tools.iter().find(|t| t.name() == name) {
        // Direct execution for built-in
        self.tool_executor.execute_with_context(tool, params, &ctx).await
    } else {
        // Extension execution
        self.extension_core.invoke_hook(...).await
    }
}

// AFTER: Unified path
async fn execute_tool(&self, name: &str, params: Value) -> Result<Value> {
    // ALL tools go through ExtensionCore
    let result = self.extension_core
        .invoke_hook(
            HookPoint::ToolExecute {
                tool_name: name.to_string(),
                params,
                context: self.build_execution_context(),
            }
        )
        .await?;
    
    match result {
        HookOutput::Json(value) => Ok(value),
        HookOutput::Error(e) => Err(anyhow!(e)),
        _ => Err(anyhow!("Unexpected hook output")),
    }
}
```

**Register built-in tools with ExtensionCore**:

```rust
// In AgenticLoopV4::new()
// AFTER: Register built-ins as hooks
for tool in &self.tools {
    BuiltinToolAdapter::register_tool(
        &self.extension_core,
        tool,
        &builtin_config,
    ).await?;
}
```

### Phase 3: Unify Tool Definition Building

**Problem**: `description()` vs `llm_description()` inconsistency.

**Solution**: Standardize on single method.

```rust
// src/providers/types.rs
pub struct ToolDefinition {
    pub name: String,
    pub description: String,        // Always LLM-optimized
    pub parameters: ParameterSchema,
}

// src/tools/traits.rs
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    
    // Remove: description() - was for human-readable
    // Keep only: llm_description() - for LLM consumption
    fn llm_description(&self) -> String;
    
    fn parameters(&self) -> ParameterSchema;
}
```

**Update all registration handlers**:

```rust
// src/extensions/adapters/builtin_tool_adapter.rs
impl BuiltinRegisterHandler {
    async fn handle(&self, _ctx: HookContext) -> HookResult {
        let tool_def = ToolDefinition {
            name: self.tool.name().to_string(),
            description: self.tool.llm_description(), // FIXED: was description()
            parameters: self.tool.parameters(),
        };
        HookResult::Continue(HookOutput::Tool(tool_def))
    }
}
```

### Phase 4: Unify System Prompt Building

**Single builder with ExtensionCore integration**:

```rust
// src/prompt/builder.rs
impl SystemPromptBuilder {
    pub async fn build(&self) -> Result<String> {
        // Get tools from ExtensionCore (unified source)
        let tools = self.extension_core
            .invoke_hook::<Vec<ToolDefinition>>(HookPoint::ToolList)
            .await?;
        
        // Build sections
        let tools_section = self.build_tools_section(&tools).await?;
        let skills_section = self.build_skills_section().await?;
        let runtime_section = self.build_runtime_section();
        
        // Combine
        Ok(self.template
            .replace("{{tools}}", &tools_section)
            .replace("{{skills}}", &skills_section)
            .replace("{{runtime}}", &runtime_section)
            // ... other sections
        )
    }
    
    async fn build_tools_section(&self, tools: &[ToolDefinition]) -> Result<String> {
        // Consistent formatting for all tool types
        let descriptions: Vec<String> = tools
            .iter()
            .map(|t| format!("### {}\n\n{}", t.name, t.description))
            .collect();
        
        Ok(format!(
            "## Available Tools\n\n{}\n\n### Tool Use Guidelines\n...",
            descriptions.join("\n\n")
        ))
    }
}
```

**New HookPoint for tool listing**:

```rust
// src/extensions/core/hook_points.rs
pub enum HookPoint {
    // ... existing
    
    /// Request list of all registered tools
    ToolList {
        filter: Option<ToolFilter>, // enabled only, by type, etc.
    },
}
```

### Phase 5: MCP Tools via Hook Registration

**Currently MCP bypasses `ToolRegister` hook. Fix this:**

```rust
// src/extensions/adapters/mcp_adapter.rs
impl McpAdapter {
    async fn register_tools(&self, core: &ExtensionCore) -> Result<()> {
        let tools = self.manager.list_all_tools().await?;
        
        for tool in tools {
            // NEW: Register via ToolRegister hook
            core.register_hook(
                HookPoint::ToolRegister,
                Arc::new(McpToolRegisterHandler {
                    tool_name: tool.name.clone(),
                    server_name: tool.server_name.clone(),
                    description: tool.description.clone(),
                    parameters: tool.input_schema.clone(),
                }),
                &self.extension_id,
            ).await;
            
            // Register execution handler
            core.register_hook(
                HookPoint::ToolExecute {
                    tool_name: format!("mcp:{}:{}", tool.server_name, tool.name),
                },
                Arc::new(McpToolExecuteHandler {
                    manager: Arc::clone(&self.manager),
                    tool_name: tool.name.clone(),
                    server_name: tool.server_name.clone(),
                }),
                &self.extension_id,
            ).await;
        }
        
        Ok(())
    }
}
```

### Phase 6: Consolidate Tool Storage (Single Source of Truth)

**New unified registry**:

```rust
// src/extensions/core/tool_registry.rs
pub struct UnifiedToolRegistry {
    /// All registered tools by name
    tools: RwLock<HashMap<String, RegisteredTool>>,
    
    /// Whitelist enforcement
    config: RwLock<ToolConfig>,
    
    /// Reserved params config per tool type
    reserved_params: RwLock<HashMap<String, ReservedParamsConfig>>,
}

impl UnifiedToolRegistry {
    /// Register a tool from any source
    pub async fn register(&self, tool: ToolRegistration) -> Result<()> {
        // Enforce whitelist
        if !self.config.read().await.is_tool_enabled(&tool.name) {
            return Err(anyhow!("Tool '{}' is not enabled", tool.name));
        }
        
        let mut tools = self.tools.write().await;
        
        // Check for duplicates
        if tools.contains_key(&tool.name) {
            return Err(anyhow!("Tool '{}' already registered", tool.name));
        }
        
        tools.insert(tool.name.clone(), RegisteredTool {
            name: tool.name,
            description: tool.description,
            parameters: tool.parameters,
            source: tool.source, // BuiltIn, Mcp, Universal
            handler: tool.handler,
            reserved_params: tool.reserved_params,
        });
        
        Ok(())
    }
    
    /// Get tool for execution
    pub async fn get(&self, name: &str) -> Option<RegisteredTool> {
        let tools = self.tools.read().await;
        tools.get(name).cloned()
    }
    
    /// Get all tools for prompt building
    pub async fn list_all(&self) -> Vec<ToolDefinition> {
        let tools = self.tools.read().await;
        tools.values()
            .filter(|t| self.config.read().await.is_tool_enabled(&t.name))
            .map(|t| ToolDefinition {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.parameters.clone(),
            })
            .collect()
    }
}
```

**Integrate with ExtensionCore**:

```rust
// src/extensions/core/mod.rs
pub struct ExtensionCore {
    hooks: RwLock<HashMap<HookId, RegisteredHook>>,
    hooks_by_point: RwLock<HashMap<String, Vec<HookId>>>,
    services: Arc<ExtensionServices>,
    
    // NEW: Unified tool registry
    tool_registry: Arc<UnifiedToolRegistry>,
    
    globally_enabled: RwLock<bool>,
}

impl ExtensionCore {
    /// Register a tool (called by adapters)
    pub async fn register_tool(&self, registration: ToolRegistration) -> Result<()> {
        self.tool_registry.register(registration).await
    }
    
    /// Get tool for execution
    pub async fn get_tool(&self, name: &str) -> Option<RegisteredTool> {
        self.tool_registry.get(name).await
    }
    
    /// List all tools (for prompts)
    pub async fn list_tools(&self) -> Vec<ToolDefinition> {
        self.tool_registry.list_all().await
    }
}
```

### Phase 7: Deprecate Legacy Paths

**Remove or deprecate:**

| Legacy Component | Replacement | Action |
|-----------------|-------------|--------|
| `AgenticLoopV4.tools: Vec<Arc<dyn Tool>>` | `ExtensionCore.tool_registry` | Remove field, use registry |
| `ToolFactory::create_tools()` direct path | Register via `HookPoint::ToolRegister` | Migrate to hooks |
| `McpManager` direct tool storage | `ExtensionCore.tool_registry` | Delegate to registry |
| `TaskManager` tool execution | `ExtensionCore::invoke_hook()` | Migrate callers |
| `BuiltinRegistry::register()` direct | `ExtensionCore::register_tool()` | Update to use registry |

---

## Benefits

| Benefit | Description |
|---------|-------------|
| **Single Execution Path** | All tools execute through same code path |
| **Consistent Reserved Params** | All tools get session context, validation, injection |
| **Unified Security** | Single whitelist enforcement point |
| **DRY Compliance** | No duplicate panic isolation, timeout, context injection |
| **Consistent Descriptions** | Single `llm_description()` used everywhere |
| **Single Source of Truth** | One registry for all tool metadata |
| **Easier Testing** | Mock ExtensionCore for all tool scenarios |
| **Future Extensibility** | Middleware applies to all tools uniformly |

---

## Drawbacks

| Drawback | Mitigation |
|----------|------------|
| **Hook Overhead** | ~1-2μs per call (negligible vs tool execution) |
| **Breaking Change** | Internal refactor, no public API changes |
| **Migration Complexity** | 7 phases, ~1 week effort |
| **Temporary Duplication** | Old and new paths coexist during migration |

---

## Acceptance Criteria

### Phase 1 (Reserved Params)
- [ ] Built-in tools use `ToolExecutionService`
- [ ] Built-in tools receive real `ToolContext` (not "unknown")
- [ ] Reserved param validation works for all tool types
- [ ] Reserved param injection works for all tool types

### Phase 2 (Execution Path)
- [ ] `AgenticLoopV4` routes ALL tools through `ExtensionCore`
- [ ] No direct `ToolExecutor` calls from `AgenticLoopV4`
- [ ] Built-in tools registered as ExtensionCore hooks

### Phase 3 (Tool Definitions)
- [ ] Single `llm_description()` method used everywhere
- [ ] `description()` method removed from `Tool` trait
- [ ] All registration handlers use consistent method

### Phase 4 (System Prompt)
- [ ] Single `SystemPromptBuilder` for all paths
- [ ] `HookPoint::ToolList` provides unified tool enumeration
- [ ] Subagent prompts can optionally include tools

### Phase 5 (MCP Hook Registration)
- [ ] MCP tools register via `HookPoint::ToolRegister`
- [ ] MCP tools no longer bypass ExtensionCore registry

### Phase 6 (Unified Registry)
- [ ] `UnifiedToolRegistry` is single source of truth
- [ ] Whitelist enforced consistently at registration time
- [ ] No duplicate tool storage

### Phase 7 (Legacy Cleanup)
- [ ] `AgenticLoopV4.tools` field removed
- [ ] `TaskManager` tool execution removed
- [ ] `DisabledToolFilter` deprecated
- [ ] All 980+ tests pass

---

## Related Work

This ADR **blocks** ADR-019 (Dynamic Tool and Prompt Updates). Once unified:
- Single permission check in ExtensionCore covers ALL tools
- Single tool registration path for dynamic additions
- Single system prompt rebuild point

---

## References

### Execution Paths
- Direct execution: `src/engine/loop_v4.rs:722-757`
- Hook execution: `src/extensions/core.rs`
- ToolExecutor: `src/engine/tool_executor.rs`
- BuiltinToolAdapter (incomplete): `src/extensions/adapters/builtin_tool_adapter.rs:145-179`

### Reserved Parameters
- ToolExecutionService: `src/extensions/services/tool_execution.rs`
- Reserved params config: `src/extensions/services/reserved_params.rs`
- Context resolver: `src/tools/shared/context_resolver.rs`
- ToolWrapper routing: `src/tools/wrapper.rs:45-157`

### System Prompt Building
- loop_v4 builder: `src/engine/loop_v4.rs:1416-1477`
- SystemPromptBuilder: `src/prompt/builder.rs`
- Async enhancement: `src/engine/async_agentic_loop.rs:499-504`
- Subagent prompt: `src/agent/subagent_announce.rs:100-134`

### Tool Definition Building
- loop_v4 definitions: `src/engine/loop_v4.rs:858-870`
- Builtin registration handler: `src/extensions/adapters/builtin_tool_adapter.rs:103-114`
- Builtin prompt handler: `src/extensions/adapters/builtin_tool_adapter.rs:203-214`
- Universal registration: `src/extensions/adapters/universal_tool_adapter.rs:326-337`

### Tool Lifecycle
- ToolFactory: `src/tools/factory.rs`
- BuiltinRegistry: `src/tools/builtin_registry.rs`
- McpManager: `src/mcp/manager.rs`
- ExtensionManager: `src/extensions/manager/mod.rs`

### Legacy Components
- TaskManager: `src/engine/task_manager.rs`
- ADR-017 (Extensions 2.0): `docs/architecture/adr/ADR-017-extensions-2-0.md`
