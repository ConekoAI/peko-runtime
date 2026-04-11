# Public API Surface Documentation

> **Version:** 2.0 (Post-ADR-017)  
> **Last Updated:** 2026-04-11  

This document defines the public API surface for Pekobot, including the new Unified Extension Architecture (ADR-017) APIs.

---

## Table of Contents

1. [Module: `extensions`](#module-extensions) - NEW: Unified Extension System
2. [Module: `agent`](#module-agent)
3. [Module: `providers`](#module-providers)
4. [Module: `common::services`](#module-commonservices)
5. [Module: `tools::factory`](#module-toolsfactory)
6. [Module: `session::context`](#module-sessioncontext)
7. [Compatibility Notes](#compatibility-notes)

---

## Module: `extensions`

**Status:** ACTIVE (New in 2.0)  
**ADR:** ADR-017: Unified Extension Architecture

The extensions module provides a unified system for all agent capabilities (tools, skills, MCP servers, channels, gateways) through 22 hook points.

### `extensions::core`

#### `ExtensionCore`

Central registry for all extension hooks.

```rust
pub struct ExtensionCore { ... }

impl ExtensionCore {
    /// Register a hook handler
    pub async fn register_hook(
        &self,
        point: HookPoint,
        handler: Arc<dyn HookHandler>,
        extension_id: &ExtensionId,
    ) -> Result<HookId>
    
    /// Unregister a hook
    pub async fn unregister_hook(&self, hook_id: &HookId) -> Result<()>
    
    /// Enable/disable hooks
    pub async fn enable_hook(&self, hook_id: &HookId) -> Result<()>
    pub async fn disable_hook(&self, hook_id: &HookId) -> Result<()>
    
    /// Invoke hooks at a specific point
    pub async fn invoke_hooks(
        &self,
        point: HookPoint,
        context: HookContext,
    ) -> Result<Vec<HookResult>>
    
    /// List all registered hooks
    pub async fn list_hooks(&self) -> Vec<HookInfo>
    
    /// Get hooks for a specific extension
    pub async fn get_hooks_for_extension(&self, ext_id: &ExtensionId) -> Vec<HookInfo>
}
```

#### `HookPoint`

All 22 extension hook points:

```rust
pub enum HookPoint {
    // Prompt lifecycle
    PromptSystemSection { section: String, priority: i32 },
    PromptPreProcess,
    PromptPostProcess,
    
    // Tool lifecycle
    ToolRegister,
    ToolExecute { tool_name: String },
    ToolExecuteAsync { tool_name: String },
    ToolCheckStatus { tool_name: String },
    ToolCancel { tool_name: String },
    ToolResultTransform,
    
    // Session lifecycle
    SessionStateChange,
    SessionCompaction,
    SessionContextBuild,
    
    // I/O lifecycle
    ChannelInput,
    ChannelOutput,
    MessagePreSend,
    MessagePostReceive,
    
    // Event lifecycle
    EventSubscribe { topic_pattern: String },
    EventEmit,
    
    // Agent lifecycle
    AgentInit,
    AgentShutdown,
    AgentIteration { iteration: usize },
}
```

#### `HookHandler` Trait

```rust
#[async_trait]
pub trait HookHandler: Send + Sync + std::fmt::Debug {
    async fn handle(&self, ctx: HookContext) -> HookResult;
    fn hook_point(&self) -> HookPoint;
    fn priority(&self) -> i32 { 100 }
    fn name(&self) -> String;
}
```

---

### `extensions::manager`

#### `ExtensionManager`

Unified lifecycle management for all extension types.

```rust
pub struct ExtensionManager { ... }

impl ExtensionManager {
    /// Create new manager with discovery
    pub async fn new() -> Result<Self>
    
    /// Install extension from path
    pub async fn install(&mut self, path: &Path) -> Result<ExtensionId>
    
    /// List all extensions
    pub async fn list_extensions(&self, filter: ExtensionFilter) -> Vec<ExtensionInfo>
    
    /// Enable/disable extensions
    pub async fn enable(&mut self, id: &ExtensionId) -> Result<()>
    pub async fn disable(&mut self, id: &ExtensionId) -> Result<()>
    
    /// Uninstall extension
    pub async fn uninstall(&mut self, id: &ExtensionId) -> Result<()>
    
    /// Get extension info
    pub async fn get_info(&self, id: &ExtensionId) -> Result<ExtensionInfo>
    
    /// Validate extension at path
    pub async fn validate(&self, path: &Path) -> Result<ValidationReport>
    
    /// Debug: show resolved hooks
    pub async fn debug_extension(&self, id: &ExtensionId) -> Result<DebugInfo>
    
    /// Create extension bundle
    pub async fn bundle_create(
        &self,
        name: &str,
        extension_ids: Vec<ExtensionId>,
    ) -> Result<PathBuf>
    
    /// Install bundle
    pub async fn bundle_install(&mut self, path: &Path) -> Result<Vec<ExtensionId>>
}
```

#### ExtensionFilter

```rust
pub struct ExtensionFilter {
    pub enabled_only: bool,
    pub extension_type: Option<ExtensionType>,
    pub search: Option<String>,
}

pub enum ExtensionType {
    Skill,
    Mcp,
    UniversalTool,
    Channel,
    Hook,
    Gateway,
    General,
    Builtin,
}
```

---

### `extensions::adapters`

#### `ExtensionTypeAdapter` Trait

```rust
#[async_trait]
pub trait ExtensionTypeAdapter: Send + Sync + std::fmt::Debug {
    fn extension_type(&self) -> &'static str;
    fn manifest_format(&self) -> ManifestFormat;
    fn resolve_hooks(&self, manifest: &ExtensionManifest) -> Vec<HookBinding>;
    async fn initialize(&self, manifest: &ExtensionManifest) -> Result<ExtensionState>;
    async fn shutdown(&self, state: ExtensionState) -> Result<()>;
    async fn is_healthy(&self, state: &ExtensionState) -> bool;
}
```

#### Built-in Adapters

| Adapter | Type | Purpose |
|---------|------|---------|
| `SkillAdapter` | `skill` | SKILL.md-based capabilities |
| `McpAdapter` | `mcp` | MCP server integration |
| `UniversalToolAdapter` | `universal-tool` | Executable tools |
| `BuiltinToolAdapter` | `builtin` | Core built-in tools |
| `ChannelAdapter` | `channel` | I/O channels |
| `GatewayAdapter` | `gateway` | Platform gateways |
| `GeneralAdapter` | `general` | Multi-hook extensions |

---

## Module: `agent`

### Public Types

#### `agent::stateless_manager` (ACTIVE)

```rust
pub struct StatelessAgentManager { ... }
pub enum StatelessManagerEvent { ... }

impl StatelessAgentManager {
    pub async fn new() -> Result<(Self, mpsc::Receiver<StatelessManagerEvent>)>
    pub async fn with_data_dir(data_dir: PathBuf) -> Result<(Self, mpsc::Receiver<StatelessManagerEvent>)>
    pub async fn register(&self, name: &str, image: &str, team_id: Option<String>) -> Result<AgentConfigEntry>
    pub async fn unregister(&self, name: &str, team: &str) -> Result<bool>
    pub async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult>
    pub async fn list(&self) -> Result<Vec<AgentConfigEntry>>
    pub async fn shutdown(&self) -> Result<()>
}
```

#### `agent::manager` (REMOVED in 2.0)

**Status:** ❌ REMOVED  
**Replaced by:** `StatelessAgentManager`

---

## Module: `providers`

### Public Types

#### `providers::openai_compatible` (ACTIVE)

```rust
pub struct OpenAICompatibleProvider { ... }
pub struct OpenAICompatibleConfig { ... }

impl OpenAICompatibleConfig {
    pub fn groq(api_key: &str, model: &str) -> Self
    pub fn together(api_key: &str, model: &str) -> Self
    pub fn fireworks(api_key: &str, model: &str) -> Self
}
```

#### `providers::kimi` (ACTIVE)

```rust
pub struct KimiProvider { ... }

impl KimiProvider {
    pub fn from_env() -> Result<Self>
    pub fn new(api_key: String) -> Self
    pub fn with_model(self, model: &str) -> Self
}
```

#### `providers::kimi_code` (REMOVED in 2.0)

**Status:** ❌ REMOVED  
**Replaced by:** `AnthropicProvider` or `KimiProvider`

---

## Module: `common::services`

### Public Types

#### `common::services::agent_service` (ACTIVE)

```rust
pub struct AgentService { ... }

impl AgentService {
    pub fn new(resolver: PathResolver) -> Self
    pub async fn list_agents(&self, team_filter: Option<&str>) -> Result<Vec<AgentSummary>>
    pub async fn get_agent(&self, name: &str, team: Option<&str>) -> Result<Option<AgentInfo>>
    pub async fn create_agent(&self, request: AgentCreateRequest) -> Result<AgentCreationResult>
    pub async fn delete_agent(&self, name: &str, team: Option<&str>, opts: AgentDeleteOptions) -> Result<AgentDeleteResult>
    pub async fn rename_agent(&self, old_name: &str, new_name: &str, team: Option<&str>, to_team: Option<&str>) -> Result<AgentRenameResult>
    pub async fn init_agent(&self, request: AgentInitRequest) -> Result<AgentInitResult>
    pub async fn update_agent(&self, name: &str, team: Option<&str>, update: AgentUpdateRequest) -> Result<AgentInfo>
    pub async fn export_agent(&self, name: &str, team: Option<&str>, opts: AgentExportOptions) -> Result<AgentExportResult>
    pub async fn import_agent(&self, file_path: &Path, opts: AgentImportOptions) -> Result<AgentImportResult>
    pub fn agent_exists(&self, name: &str, team: Option<&str>) -> bool
}
```

#### `common::services::agent_config_service` (ACTIVE)

```rust
pub struct AgentConfigService { ... }
pub struct AgentConfigEntry { ... }

impl AgentConfigService {
    pub fn new(path_resolver: PathResolver) -> Self
    pub async fn get(&self, agent_name: &str, team: Option<&str>) -> Result<Option<AgentConfigEntry>>
    pub async fn save(&self, agent_name: &str, team: &str, config: &AgentConfig) -> Result<()>
    pub async fn delete(&self, agent_name: &str, team: &str) -> Result<bool>
    pub async fn exists(&self, agent_name: &str, team: Option<&str>) -> Result<bool>
    pub async fn list_in_team(&self, team: &str) -> Result<Vec<AgentConfigEntry>>
    pub async fn list_all(&self) -> Result<Vec<AgentConfigEntry>>
}
```

#### `common::services::agent_creation_service` (REMOVED in 2.0)

**Status:** ❌ REMOVED  
**Replaced by:** `AgentService::create_agent()`

#### `common::services::agent_config_builder` (REMOVED in 2.0)

**Status:** ❌ REMOVED  
**Replaced by:** `AgentService` methods directly

#### `common::services::message_service` (REMOVED in 2.0)

**Status:** ❌ REMOVED in ADR-016  
**Replaced by:** `StatelessAgentService`

---

## Module: `tools::factory`

### Public Types

#### `tools::factory::ToolFactory`

**Status:** Simplified in 2.0

```rust
impl ToolFactory {
    pub fn create_tools(config: &ToolFactoryConfig) -> ToolCreationResult
    pub async fn create_tools_async(config: &ToolFactoryConfig) -> Result<ToolCreationResult>
}

impl ToolFactoryConfig {
    pub fn minimal(workspace_dir: PathBuf) -> Self
    pub fn coding(workspace_dir: PathBuf) -> Self
    pub fn full(workspace_dir: PathBuf) -> Self
}
```

**Note:** Convenience methods `create_minimal_tools`, `create_coding_tools`, `create_full_tools` are deprecated. Use `ToolFactoryConfig` constructors instead.

---

## Module: `session::context`

### Public Types

#### `session::context::ExecutionContext`

**Status:** ACTIVE (Renamed from SessionContext in 2.0)

```rust
pub struct ExecutionContext {
    pub hybrid: HybridSession,
    pub channel_type: Option<ChannelType>,
    pub is_subagent: bool,
}
```

#### `session::key::SessionKeyContext`

**Status:** ACTIVE (Renamed from SessionContext in 2.0)

```rust
pub struct SessionKeyContext {
    pub agent: String,
    pub team: String,
    pub session_type: SessionType,
    pub identifier: String,
}
```

#### `agent::context::AgentContext` (REMOVED in 2.0)

**Status:** ❌ REMOVED  
**Replaced by:** `ExecutionContext`

---

## Compatibility Notes

### Breaking Changes (2.0)

| Component | Status | Replacement |
|-----------|--------|-------------|
| `AgentManager` | ❌ Removed | `StatelessAgentManager` |
| `MessageService` | ❌ Removed | `StatelessAgentService` |
| `AgentCreationService` | ❌ Removed | `AgentService::create_agent()` |
| `AgentConfigBuilder` | ❌ Removed | `AgentService` methods |
| `SessionResolver` | ❌ Removed | `SessionManager::resolve_session()` |
| `AgentContext` | ❌ Removed | `ExecutionContext` |
| `KimiCodeProvider` | ❌ Removed | `AnthropicProvider` or `KimiProvider` |

### New APIs (2.0)

| Component | Status | Purpose |
|-----------|--------|---------|
| `ExtensionCore` | ✅ New | Central hook registry |
| `ExtensionManager` | ✅ New | Extension lifecycle |
| `HookPoint` (22 variants) | ✅ New | Extension hook points |
| `HookHandler` trait | ✅ New | Hook implementation |
| `ExtensionTypeAdapter` trait | ✅ New | Extension type implementations |

### Type Aliases for Backward Compatibility

```rust
// SessionContext → ExecutionContext
pub type SessionContext = ExecutionContext;

// SessionContext (key module) → SessionKeyContext  
pub type SessionContext = SessionKeyContext;
```

---

## Test Coverage Requirements

### Critical Paths

The following operations must be tested:

1. **Extension Lifecycle**
   - Register extension
   - Enable/disable hooks
   - Invoke hooks
   - Unregister extension

2. **Agent Lifecycle**
   - Create agent
   - Get agent info
   - List agents
   - Delete agent

3. **Provider Operations**
   - Chat with tools
   - Stream responses
   - Token usage tracking

4. **Session Operations**
   - Create session
   - Add messages
   - Branch session
   - Delete session

5. **Tool Operations**
   - Register tools via ExtensionCore
   - Execute built-in tools
   - Execute universal tools
   - Execute MCP tools

---

## Related Documentation

- [Architecture Overview](docs/architecture/OVERVIEW.md)
- [Extension System](docs/architecture/EXTENSION_SYSTEM.md)
- [ADR-017: Unified Extension Architecture](docs/architecture/adr/ADR-017.md)
- [Data Model](DATA_MODEL.md)

---

*Version 2.0 · Post-ADR-017 · 2026-04-11*
