# Public API Surface Documentation

> **Version:** 0.1.0 (Post-ADR-017, Post-Issue-021)  
> **Last Updated:** 2026-07-10  

This document defines the public API surface for Peko, including the new Unified Extension Architecture (ADR-017) APIs.

---

## Table of Contents

1. [Module: `extension`](#module-extension) - Generic Extension Framework (ADR-017)
2. [Module: `extensions`](#module-extensions) - Extension Type Implementations
3. [Module: `agent`](#module-agent)
4. [Module: `providers`](#module-providers)
5. [Module: `common::services`](#module-commonservices)
6. [Module: `tools::factory`](#module-toolsfactory)
7. [Module: `session::context`](#module-sessioncontext)
8. [Compatibility Notes](#compatibility-notes)

---

## Module: `extension`

**Status:** ACTIVE (New in 0.1.0)  
**ADR:** ADR-017: Unified Extension Architecture

The `extension` module (singular) contains the **generic extension framework** — hook points, registries, types, managers, and shared services. It has **zero dependencies** on extension type implementations.

### `extension::core`

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
    
    /// List all registered tools
    pub async fn list_tools(&self) -> Vec<ToolMetadata>
    
    /// List tool definitions for LLM API
    pub async fn list_tool_definitions(&self) -> Vec<ToolDefinition>
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

### `extension::manager`

#### `ExtensionManager`

Unified lifecycle management for all extension types.

```rust
pub struct ExtensionManager { ... }

impl ExtensionManager {
    /// Create new manager
    pub fn new() -> Self
    
    /// Install extension from path
    pub async fn install(&mut self, path: &Path) -> Result<ExtensionId>
    
    /// List all loaded extensions
    pub fn list_extensions(&self) -> Vec<&LoadedExtension>
    
    /// Enable/disable extensions
    pub async fn enable(&mut self, id: &ExtensionId) -> Result<()>
    pub async fn disable(&mut self, id: &ExtensionId) -> Result<()>
    
    /// Uninstall extension
    pub async fn uninstall(&mut self, id: &ExtensionId) -> Result<()>
    
    /// Create extension bundle
    pub fn create_bundle(&self, ids: Vec<ExtensionId>, name: &str) -> Result<ExtensionBundle>
    
    /// Install bundle
    pub async fn install_bundle(&mut self, bundle: ExtensionBundle) -> Result<Vec<ExtensionId>>
    
    /// Scan directory for extensions
    pub async fn scan_directory(&self, path: &Path) -> Result<Vec<DiscoveredExtension>>
    
    /// Load extensions from directory
    pub async fn load_from_directory(&mut self, path: &Path) -> Result<Vec<ExtensionId>>
}
```

---

### `extension::adapters`

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
    async fn register_tools(&self, core: &ExtensionCore, manifest: &ExtensionManifest) -> Result<usize>;
}
```

#### `BuiltInAdapters`

```rust
pub struct BuiltInAdapters;

impl BuiltInAdapters {
    pub fn new() -> Self;
    pub fn adapters(&self) -> Vec<Box<dyn ExtensionTypeAdapter>>;
}
```

#### `ManifestFormat`

```rust
pub enum ManifestFormat {
    YamlFrontmatterMarkdown { required_fields: Vec<&'static str>, file_name: &'static str },
    Yaml { schema: String, file_name: &'static str },
    Json { schema: String, file_name: &'static str },
    Toml { schema: String, file_name: &'static str },
    Custom { detector: fn(&Path) -> bool },
}
```

---

### `extension::types`

#### Core Types

```rust
pub struct ExtensionManifest { ... }
pub struct ExtensionId(pub String);
pub struct HookId(pub String);
pub enum HookResult { ... }
pub enum HookOutput { ... }
pub struct HookInput { ... }
pub struct ToolMetadata { ... }
pub enum ToolSource { ... }
```

---

### `extension::services`

#### `ToolExecutionService`

```rust
pub struct ToolExecutionService { ... }
pub struct ToolExecutionConfig { ... }
pub struct ReservedParamsConfig { ... }
pub enum ParamSource { ... }
```

---

### `extension::protocols::shared`

#### Process Transport

```rust
pub struct ProcessTransport { ... }
pub struct ProcessTransportBuilder { ... }
pub struct ProcessConfig { ... }
```

#### Validation

```rust
pub fn filter_reserved_params(schema: &Value, reserved: &[String]) -> Result<Value>
pub fn validate_no_reserved_params_leak(params: &Value, reserved: &[String]) -> Result<()>
```

---

## Module: `extensions`

**Status:** ACTIVE (New in 0.1.0)  
**ADR:** ADR-017: Unified Extension Architecture

The `extensions` module (plural) contains **extension type implementations**. Each extension type lives in its own directory with its adapter, runtime, and protocol code.

### Extension Type Directory Layout

```
src/extensions/
├── mcp/           # MCP server integration
│   ├── adapter.rs
│   ├── runtime/
│   │   ├── adapter.rs
│   │   ├── starter.rs
│   │   ├── tool_proxy.rs
│   │   └── injectable_proxy.rs
│   └── protocol/
│       ├── client.rs
│       ├── transport.rs
│       ├── types.rs
│       ├── config.rs
│       ├── discovery.rs
│       └── manager.rs
├── gateway/       # Platform gateways
│   ├── adapter.rs
│   ├── protocol.rs
│   └── runtime/
│       ├── adapter.rs
│       ├── starter.rs
│       └── router.rs
├── universal/     # Executable tools
│   ├── adapter.rs
│   └── protocol/
│       ├── manifest.rs
│       ├── protocol.rs
│       ├── transport.rs
│       └── adapter.rs
├── skill/         # SKILL.md capabilities
│   └── adapter.rs
├── builtin/       # Core built-in tools
│   └── adapter.rs
└── general/       # Multi-hook extensions
    └── adapter.rs
```

### `extensions::<type>::adapter`

Each extension type provides an adapter implementing `ExtensionTypeAdapter`:

| Adapter | Module Path | Type |
|---------|-------------|------|
| `SkillAdapter` | `extensions::skill::adapter` | `skill` |
| `McpAdapter` | `extensions::mcp::adapter` | `mcp` |
| `UniversalToolAdapter` | `extensions::universal::adapter` | `universal-tool` |
| `BuiltinToolAdapter` | `extensions::builtin::adapter` | `builtin` |
| `GatewayAdapter` | `extensions::gateway::adapter` | `gateway` |
| `GeneralExtensionAdapter` | `extensions::general::adapter` | `general` |

### `extensions::extension_types`

```rust
pub const SKILL: &str = "skill";
pub const MCP: &str = "mcp";
pub const UNIVERSAL_TOOL: &str = "universal-tool";
pub const GATEWAY: &str = "gateway";
pub const CUSTOM_PREFIX: &str = "custom:";

pub fn is_valid_type(ext_type: &str) -> bool;
pub fn standard_types() -> Vec<&'static str>;
```

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

## Module: `agents`

### Public Types

#### `agents::stateless_service` (ACTIVE)

```rust
pub struct StatelessAgentService { ... }

impl StatelessAgentService {
    pub async fn new(
        config_service: Arc<ConfigAuthorityImpl>,
        path_resolver: PathResolver,
    ) -> Result<Self>

    pub async fn new_with_resolver(
        config_service: Arc<ConfigAuthorityImpl>,
        path_resolver: PathResolver,
        resolver: Option<Arc<LlmResolver>>,
    ) -> Result<Self>

    pub async fn execute_message(&self, request: MessageRequest) -> Result<MessageResult>
    pub async fn execute(&self, request: ExecutionRequest) -> Result<ExecutionResult>
    pub fn resolver(&self) -> Option<&Arc<LlmResolver>>
}
```

`StatelessAgentService` is the cold-start execution entry point for all agent turns. It resolves the agent configuration through `ConfigAuthority`, builds an `Agent` instance, and runs the turn-based loop. It replaces the legacy `AgentManager`, `MessageService`, and `AgentCreationService`.

#### `agents::manager` (REMOVED in 0.1.0)

**Status:** ❌ REMOVED  
**Replaced by:** `StatelessAgentService`

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

#### `providers::kimi_code` (REMOVED in 0.1.0)

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
    pub fn for_principal(workspace: impl Into<PathBuf>) -> Self
    pub async fn resolve_subagent_type(&self, name: &str) -> Result<AgentConfig>
    pub fn agent_exists(&self, name: &str) -> bool
    pub fn resolver(&self) -> &PathResolver
}
```

`AgentService` is now a **subagent resolution helper** for the built-in `Agent` tool. It no longer implements standalone agent CRUD, export, or import. Given a `subagent_type` name, it resolves the prompt/config from the Principal workspace (`agents/<name>/AGENT.md` or `agents/<name>.md`) and falls back to the global `~/.peko/agents/<name>/config.toml` layout.

#### `common::services::config_authority` (ACTIVE)

```rust
#[async_trait]
pub trait ConfigAuthority: Send + Sync {
    async fn get(&self, agent_name: &str) -> ConfigResult<Option<AgentConfigEntry>>;
    async fn save(&self, agent_name: &str, config: &AgentConfig) -> ConfigResult<PathBuf>;
    async fn exists(&self, agent_name: &str) -> ConfigResult<bool>;
    async fn list_all(&self) -> ConfigResult<Vec<AgentConfigEntry>>;
    async fn delete(&self, agent_name: &str) -> ConfigResult<bool>;
    async fn clear_cache(&self);
    async fn invalidate_cache(&self, agent_name: &str);
    fn path_resolver(&self) -> &PathResolver;
}

pub struct ConfigAuthorityImpl { ... }
pub struct AgentConfigEntry { ... }
```

`ConfigAuthority` is the single interface for loading and persisting standalone agent configurations. It has no team-scoped operations.

#### `common::services::agent_config_service` (REMOVED in 0.1.0)

**Status:** ❌ REMOVED  
**Replaced by:** `ConfigAuthority` / `StatelessAgentService`

#### `common::services::agent_creation_service` (REMOVED in 0.1.0)

**Status:** ❌ REMOVED  
**Replaced by:** `StatelessAgentService`

#### `common::services::agent_config_builder` (REMOVED in 0.1.0)

**Status:** ❌ REMOVED  
**Replaced by:** `ConfigAuthority`

#### `common::services::message_service` (REMOVED in 0.1.0)

**Status:** ❌ REMOVED in ADR-016  
**Replaced by:** `StatelessAgentService`

---

## Module: `tools::factory`

### Public Types

#### `tools::factory::ToolFactory`

**Status:** Simplified in 0.1.0

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

#### `session::context::SessionContext`

**Status:** ACTIVE

```rust
pub struct SessionContext {
    pub session_id: String,
    pub agent_name: String,
    pub session_key: String,
    pub full_session_key: String,
    pub peer: Subject,
    pub channel_type: Option<ChannelType>,
    pub is_subagent: bool,
    pub is_isolated: bool,
}
```

Lightweight routing metadata for a resolved session. For actual session operations, use the `SessionHandle` obtained from `SessionManager::resolve_session`.

#### `session::key::SessionKeyContext`

**Status:** ACTIVE

```rust
pub struct SessionKeyContext {
    pub channel: Option<String>,
    pub sender_id: Option<String>,
    pub channel_id: Option<String>,
    pub account_id: Option<String>,
    pub thread_id: Option<String>,
    pub web_token: Option<String>,
    pub chat_type: ChatType,
}
```

Context for deriving semantic session keys. Not to be confused with the runtime `SessionContext`.

#### `agent::context::AgentContext` (REMOVED in 0.1.0)

**Status:** ❌ REMOVED  
**Replaced by:** `SessionContext`

---

## Compatibility Notes

### Breaking Changes (0.1.0)

| Component | Status | Replacement |
|-----------|--------|-------------|
| `AgentManager` | ❌ Removed | `StatelessAgentService` |
| `MessageService` | ❌ Removed | `StatelessAgentService` |
| `AgentCreationService` | ❌ Removed | `StatelessAgentService` |
| `AgentConfigService` | ❌ Removed | `ConfigAuthority` |
| `AgentConfigBuilder` | ❌ Removed | `ConfigAuthority` |
| `SessionResolver` | ❌ Removed | `SessionManager::resolve_session()` |
| `AgentContext` | ❌ Removed | `SessionContext` |
| `KimiCodeProvider` | ❌ Removed | `AnthropicProvider` or `KimiProvider` |
| Standalone `AgentService` CRUD / export / import | ❌ Removed | `ConfigAuthority` (config persistence) and `AgentService::resolve_subagent_type` (subagent lookup) |

### New APIs (0.1.0)

| Component | Module | Status | Purpose |
|-----------|--------|--------|---------|
| `ExtensionCore` | `extension::core` | ✅ New | Central hook registry |
| `ExtensionManager` | `extension::manager` | ✅ New | Extension lifecycle |
| `HookPoint` (22 variants) | `extension::core` | ✅ New | Extension hook points |
| `HookHandler` trait | `extension::core` | ✅ New | Hook implementation |
| `ExtensionTypeAdapter` trait | `extension::adapters` | ✅ New | Extension type adapter trait |
| `BuiltInAdapters` | `extension::adapters` | ✅ New | Built-in adapter provider |
| `SkillAdapter` | `extensions::skill::adapter` | ✅ New | SKILL.md-based capabilities |
| `McpAdapter` | `extensions::mcp::adapter` | ✅ New | MCP server integration |
| `UniversalToolAdapter` | `extensions::universal::adapter` | ✅ New | Executable tools |
| `BuiltinToolAdapter` | `extensions::builtin::adapter` | ✅ New | Core built-in tools |
| `GatewayAdapter` | `extensions::gateway::adapter` | ✅ New | Platform gateways |
| `GeneralExtensionAdapter` | `extensions::general::adapter` | ✅ New | Multi-hook extensions |

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

- [Extension System](docs/architecture/EXTENSION_SYSTEM.md)
- [ADR-017: Unified Extension Architecture](docs/architecture/adr/ADR-017.md)
- [Data Model](DATA_MODEL.md)

---

*Version 0.1.0 · Post-ADR-017 · Post-Issue-015/020/021 · 2026-07-10*
