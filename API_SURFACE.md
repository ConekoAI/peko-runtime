# Public API Surface Documentation

> **Version:** 0.1.0 (Post-ADR-017, Post-Issue-021)  
> **Last Updated:** 2026-07-10  

This document defines the public API surface for Peko, including the new Unified Extension Architecture (ADR-017) APIs.

---

## Table of Contents

1. [Module: `extensions::framework`](#module-extensionsframework) - Generic Extension Framework (ADR-017)
2. [Module: `extensions`](#module-extensions) - Extension Type Implementations
3. [Module: `principal` / `subject`](#module-principal--subject) - Principal container & actor (ADR-039/041)
4. [Module: `agent`](#module-agent)
5. [Module: `providers`](#module-providers)
6. [Module: `common::services`](#module-commonservices)
7. [Module: `tools::factory`](#module-toolsfactory)
8. [Module: `session::context`](#module-sessioncontext)
9. [Compatibility Notes](#compatibility-notes)

---

## Module: `extensions::framework`

**Status:** ACTIVE  
**ADR:** ADR-017: Unified Extension Architecture

The `extensions::framework` module contains the **generic extension framework** — hook points, registries, types, managers, and shared services. It has **zero dependencies** on extension type implementations (which live under the sibling `extensions::<type>/` modules).

### `extensions::framework::core`

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

### `extensions::framework::manager`

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

### `extensions::framework::adapters`

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

### `extensions::framework::types`

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

### `extensions::framework::services`

#### `ToolExecutionService`

```rust
pub struct ToolExecutionService { ... }
pub struct ToolExecutionConfig { ... }
pub struct ReservedParamsConfig { ... }
pub enum ParamSource { ... }
```

---

### `extensions::framework::protocols::shared`

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

## Module: `principal` / `subject`

**Status:** ACTIVE  
**ADRs:** ADR-039 (Principal model), ADR-041 (Principal-as-container & session blackboxing), ADR-042 (no external `session` concept in CLI/IPC)

A `Principal` is the top-level container: it owns identity (DID + keys), configuration, capability grants, memory, and the agent prompts that run inside it. A `Subject` is an addressable actor (a Principal or a human user) used for routing and authorization. Sessions are an internal concern of the Principal and are not exposed on the CLI/IPC surface (the only read path is `peko log`).

### Public Types

#### `principal::Principal` (ACTIVE)

```rust
pub struct Principal { ... }
```

The loaded runtime instance of a Principal container, produced by the `PrincipalManager`.

#### `principal::PrincipalManager` (ACTIVE)

```rust
pub struct PrincipalManager { ... }
```

Owns the lifecycle of Principal containers on disk and in memory — create/load/list and resolution of a session to its owning Principal. Capability checks resolve against the Principal that owns the session (ADR-042); grants are never accepted from the IPC packet.

#### `principal::PrincipalConfig` / `principal::Capabilities` (ACTIVE)

```rust
pub struct PrincipalConfig { ... }
pub struct Capabilities { ... }
```

On-disk configuration and the capability grants derived from it. An empty/absent grant set is fail-closed (deny-all) — see the `*_fail_closed_without_principal_id` tests.

#### `subject::Subject` (ACTIVE)

```rust
pub struct Subject { ... }
```

The actor identity attached to a session (peer, caller). Replaces the pre-ADR-039 agent/team-scoped addressing.

---

## Module: `chat_log` (Runtime-Owned Consumer-Visible History)

**Status:** RETIRED (sprint 3 Phase 13, 2026-08-19)  
**ADRs:** ADR-042 (no external `session` concept in CLI/IPC surface)

The `peko-chat-log` crate (`ChatLogStore` / `ChatThreadKey` /
`ChatLogMessage` / `ChatLogPage` / `ChatLogError`) was deleted from
the workspace. Sprint 3 re-founded the consumer-visible conversation
record on the per-peer **DM channels** (see the `channel` module):
`peko log` and the `principal_log` IPC read the DM channel's
`Posted` events. The cron `Send` fired-prompt projection
(`PrincipalManager::record_cron_input`) — the store's last writer —
was dropped; fired prompts live in the trunk session JSONL.

The `peko log` row DTO survives, moved and renamed:

#### `ipc::packet::PrincipalLogMessage` (ACTIVE)

```rust
pub struct PrincipalLogMessage {
    pub schema_version: u8,
    pub id: String,
    pub sender: Subject,
    pub timestamp: DateTime<Utc>,
    pub text: String,
    pub correlation_id: Option<String>,
}
```

One immutable consumer-visible message — the row type of
`ResponsePacket::PrincipalLog`. Wire shape is byte-identical to the
retired `ChatLogMessage` (camelCase fields,
`PRINCIPAL_LOG_SCHEMA_VERSION`); the daemon mints `chan_<line>` ids
from the DM channel's line numbers.

### IPC: `RequestPacket::PrincipalLog` / `ResponsePacket::PrincipalLog`

`RequestPacket::PrincipalLog` carries `name`, `peer`,
`limit`, `since_secs`, and `cursor` (the new field). The
response is `name`, `peer`, `messages`, `next_cursor`, and
`has_more`. The legacy `events: Vec<HistoryEvent>` and
`truncated: bool` fields were removed: a chat thread is not a
session, and session-internal `kind` rows no longer leak into
the user-visible surface. `Pe ko log` walks pages via
`--cursor` until exhaustion (or a 25-page hard cap) so a
runaway caller can't pin the daemon forever.

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

**Status:** ❌ REMOVED  
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
| `ExtensionCore` | `extensions::framework::core` | ✅ New | Central hook registry |
| `ExtensionManager` | `extensions::framework::manager` | ✅ New | Extension lifecycle |
| `HookPoint` (22 variants) | `extensions::framework::core` | ✅ New | Extension hook points |
| `HookHandler` trait | `extensions::framework::core` | ✅ New | Hook implementation |
| `ExtensionTypeAdapter` trait | `extensions::framework::adapters` | ✅ New | Extension type adapter trait |
| `BuiltInAdapters` | `extensions::framework::adapters` | ✅ New | Built-in adapter provider |
| `SkillAdapter` | `extensions::skill::adapter` | ✅ New | SKILL.md-based capabilities |
| `McpAdapter` | `extensions::mcp::adapter` | ✅ New | MCP server integration |
| `UniversalToolAdapter` | `extensions::universal::adapter` | ✅ New | Executable tools |
| `BuiltinToolAdapter` | `extensions::builtin::adapter` | ✅ New | Core built-in tools |
| `GatewayAdapter` | `extensions::gateway::adapter` | ✅ New | Platform gateways |
| `GeneralExtensionAdapter` | `extensions::general::adapter` | ✅ New | Multi-hook extensions |

### Agent-Owned Session Management (2026-08-09; revised 2026-08-13)

The unified session/run framework ("coin model"): `Agent` runs work in
sessions (actions `new` / `resume` / `compact`), the `session` tool
manages them (9 storage actions). Session ids are stable for life;
oversized transcripts page in place. New/changed public items:

| Component | Module | Status | Purpose |
|-----------|--------|--------|---------|
| `SessionRuntime` v2 (adds `search_sessions`, `branch_session`, `rename_session`, `set_archived`, `delete_session`, `request_compaction`; `list_sessions` gains `include_archived`) | `tools::builtin::session` | ✅ Extended | Session tool port trait |
| `SessionInfo` (+`archived`, +`run_active`), `SessionSearchHit`, `BranchOutcome`, `DeleteOutcome`, `CompactRequestOutcome` | `tools::builtin::session` | ✅ New | Session tool DTOs |
| `ownership::{CallerContext, caller_context, in_subtree, descendants_of, err_*}` | `session::ownership` | ✅ New | Ownership tree + guard refusals (shared by both tools) |
| `SessionManager::{set_archived, set_compact_requested, set_session_title, delete_session_by_id}` | `peko_session::manager` | ✅ New | Controller passthroughs |
| `SessionStorage::search_transcripts` / `TranscriptSearchHit` | `peko_session::jsonl` | ✅ New | Case-insensitive transcript substring search |
| `RotationOutcome::Paged` / `page_numbers` / `page_path` | `peko_session::jsonl` | ✅ New | Stable-id transcript paging (`<id>.N.jsonl` pages, transparent read-path stitching) |
| `SessionCore`/`SessionView::{peek_compact_request, clear_compact_request}` (defaulted) | `peko_session::session_core` | ✅ Extended | Forced-compaction flag port (plan D2) |
| `SessionIndex::get_uncached` | `peko_session::index` | ✅ New | Cache-bypassing entry read (mid-run flag peek) |
| `MetadataController::{set_archived, set_compact_requested, peek_compact_requested}` | `peko_session::metadata_controller` | ✅ New | Flag writers + read-through peek |
| `SubagentExecutor::{resume_and_execute, resume_and_wait, request_compaction, validate_context_parent}` | `agents::subagent_executor` | ✅ New | Persistent subagents (`Agent` `action:"resume"`) + deferred compaction flagging (`action:"compact"`) |
| `SpawnRequest` (+`resume_session`, +`caller_session_key`) | `tools::builtin::messaging` | ✅ Extended | Agent tool port input |
| `SubagentRuntime::request_compaction` | `tools::builtin::messaging` | ✅ New | Agent tool compact-action port method |
| `SubagentMetadata.child_session_id` / `AsyncTaskRegistry::has_active_subagent_run_for_child` | `extensions::framework::async_exec::executor` | ✅ New | Subagent active-run detection |

### Agent–Session Paradigm Sprint (2026-08-15)

Branch `feat/agent-session-paradigm`; see
`docs/architecture/AGENT_SESSION_PARADIGM.md`. New/changed public items:

| Component | Module | Status | Purpose |
|-----------|--------|--------|---------|
| `SessionMetadata`/`SessionEntry` (+`standing`, +`slug`) | `peko_session::{metadata,index}` | ✅ Extended | Durability flag (prune exemption) + per-parent-unique path segment |
| `MetadataController::{set_parent, set_slug, set_standing}` / `SessionManager::{move_session, set_session_slug, set_standing}` | `peko_session::{metadata_controller,manager}` | ✅ New | Reparent / slug / standing writers |
| `peko_session::path` (`resolve_path`, `compute_path`, `validate_slug`, `derive_branch_slug`) | `peko_session::path` | ✅ New | `/a/b/c` path addressing over the session tree |
| `SessionRuntime::move_session`; `rename_session` (+`slug`); `SessionInfo` (+`slug`, +`path`) | `tools::builtin::session` | ✅ Extended | Session tool 10th action + path-aware DTOs |
| `ownership::{err_move_ancestor, err_move_cycle}` | `session::ownership` | ✅ New | Move guard refusals (incl. cycle prevention) |
| `SpawnRequest.name` / `ExecutionConfig.{slug, subagent_type}` | `tools::builtin::messaging`, `agents` | ✅ Extended | Named spawns + standing-child attach |
| `PrincipalConfig.children` / `ChildDeclaration` | `principal::config` | ✅ New | `[children]` standing-children declaration |
| `principal::children::ensure_declared_children` / `session::standing` helpers | `principal::children`, `session::standing` | ✅ New | Ensure-declared + declaration recovery |
| `trunk_session_id()` / `ChannelKind::Trunk` / `PrincipalManager::receive_trunk` | `principal::routers::root`, `principal::router`, `principal::manager` | ✅ New | Principal trunk session `root:self` (peer-less, cron-kept) |
| `CronJobAction::Send` (+`target`); `SEND_TARGET_TRUNK` / `validate_send_target` | `peko_cron::tools` | ✅ Extended | `target = "trunk"` routes the turn into `root:self` |

### PEKO Sprint 2 (2026-08-17) — external ingress off the root

Breaking (prelaunch): per-peer `root:{peer}` / `root:cron:{owner}`
sessions retired; all external ingress lands in per-peer standing
children of the trunk. New/changed public items:

| Component | Module | Status | Purpose |
|-----------|--------|--------|---------|
| `SessionMetadata`/`SessionEntry` (+`privileged`) | `peko_session::{metadata,index}` | ✅ Extended | Owner's child gets whole-store guard reach |
| `CallerContext.privileged` | `session::ownership` | ✅ Extended | Privilege = guard reach, not tree membership |
| `peer_child_slug` / `ensure_peer_child` | `principal::peer_children` | ✅ New | Spawn-on-contact per-peer children (`/local-user`, `/user-x`, `/principal-{did}`), trunk-anchored |
| `PeerChildTurns` (`ensure_child`, `drive_turn{,_streaming}`) | `principal::child_turns` | ✅ New | Persona-inheriting child turn driver (shared with channel binding) |
| `SubagentExecutor::resume_streaming` / `StreamingResumeOutcome` | `agents::subagent_executor` | ✅ New | Streaming child turns (same event shape as the root path) |
| `PrincipalManager::record_peer_recall` | `principal::manager` | ✅ New | Per-peer recall artifact → peer-child session id |
| ~~`PrincipalManager::receive`~~ / `receive_streaming` | `principal::manager` | ❌ Deleted / ⚠️ Changed | Peer channels route to peer children; Trunk/Cron → `receive_trunk`. The one-shot `receive` was deleted in sprint 3 Phase 12b (its only production callers were the retired A2A RPC paths) |

### Sprint 4 (2026-08-19) — `send_peer` consolidated into `ChannelSend`

Breaking (prelaunch): the parallel `send_peer` tool and the bare-post
`ChannelSend` are folded into one tool — `ChannelSend` (registered
per-agent with caller DID bound at construction). The LLM picks the
dispatch branch by choosing the wire form of its `channel` parameter:

| `channel` value        | Dispatch                                  |
|------------------------|-------------------------------------------|
| `chan_<8 base36>`      | Bare post (today's `ChannelSend`)         |
| `principal:<did>`      | RPC: ensure_peer_child + await reply      |
| `user:<id>`            | Peer messenger note (originating-user gate) |
| `group:<slug>`         | Bare post to a group channel              |

The runtime owns await / timeout / mirror / exposure-gate /
originating-user-gate policies — no parallel state machine, no second
tool name. Deleted/changed public items:

| Component | Module | Status | Purpose |
|-----------|--------|--------|---------|
| `tunnel::principal_send_tool` (`SendPeerTool`, `SendPeerArgs`, `PrincipalSendResult`, `build_tool`) | `tunnel::principal_send_tool` | ❌ Deleted | Whole module retired (1775 lines); consolidated into `tools::builtin::channel::ChannelSendTool` with typed-prefix dispatch |
| `ChannelSendTool::new` (single-arg constructor) | `tools::builtin::channel::channel_send` | ❌ Deleted | Replaced by `new_with_peer(port, did, ctx)` (per-agent, full principal / cross-runtime support) and `new_local_only(port, did)` (bare / group / user only). The global `Arc::new(ChannelSendTool::new(port))` registration in `engine/tool_runtime.rs` is removed — the tool is now per-agent only. |
| `SendPeerArgs` / `PrincipalSendResult` | (renamed) | ⚠️ Renamed | `ChannelSendArgs` / `ChannelSendResult` in `tools::builtin::channel`. JSON wire shape `{ channel, text, parent?, label? }` — no `target` / `message` fields; the wire form of `channel` carries the routing identity. |
| `tool:send_peer` capability grant | `peko-extension-api::capabilities` | ❌ Removed | Retired outright (no compat alias). `Capabilities::starter_bundle()` no longer includes the grant; `starter_bundle_does_not_grant_send_peer` pins the absence. Use `tool:ChannelSend` with a typed channel id. |
| `ChannelSend` registration | `peko::runtime::builtin_tools` | ⚠️ Moved | Was in `GLOBAL_TOOL_NAMES` (singleton, no caller DID); now in `AGENT_SPECIFIC_TOOL_NAMES` (per-agent, caller DID bound at construction). `send_peer` removed. |
| `ExtensionServices::channel_port()` / `set_channel_port` | `extensions::framework::core::config` | ✅ New | Lets the per-agent `ChannelSendTool` constructor find the file-backed `ChannelPort`. `daemon/state.rs` installs it on the same path as `set_cross_runtime_a2a_ctx`. |
| `ChannelId` constructors | `peko_protocol::channel` | ✅ New | `ChannelId::for_principal(did)`, `for_user(id)`, `for_group(slug)`. `parse()` accepts any of the four wire forms; `kind()` returns the dispatch kind. |
| `CreateOpts::id` | `peko_channel::port` | ✅ New | Pin the channel id on creation. Used by `peer_dm` for principal peers so the routing `ChannelId` is `principal:<did>` and `peko log` consumers and `ChannelSend`'s principal-branch agree without translation. |
| `peko_channel::fs` (`channel_dir_name`, `channel_dir_name_inverse`, `id_needs_colon_normalization`) | `peko_channel::fs` | ✅ New | On-disk path normalizer: typed-prefix ids gain colons that are invalid in directory names on Windows / classic Unix filesystems; `.3A.` replaces `:` for the storage path only. Wire form unchanged. |
| `ensure_peer_dm_channel` routing id | `principal::peer_dm` | ⚠️ Changed | Principal peers now route on `ChannelId::for_principal(did)` (`CreateOpts::id`). Display name stays `dm-<slug>` for `peko log` continuity (tests pin). User / public peers keep the slug-based routing id since the user-branch dispatch is messenger-port, not channel-port. |

### Sprint 3 Phase 12b (2026-08-19) — principal DM over channels; A2A RPC stack retired

Breaking (prelaunch): principal-to-principal messaging runs over the
peer DM channels now. Deleted/changed public items:

| Component | Module | Status | Purpose |
|-----------|--------|--------|---------|
| `TunnelMessage::{PrincipalToPrincipalRequest, PrincipalToPrincipalResponse}` | `tunnel::protocol` | ❌ Deleted | Signed RPC envelopes superseded by `TunnelChannelEvent`/`TunnelChannelInvite` fan-out |
| `PendingA2aResponses` / `A2aResponsePayload` / `A2aWaitError`; `tunnel::a2a_audit` | `tunnel::{a2a_pending,a2a_audit}` | ❌ Deleted | Response correlation registry + audit helpers for the retired RPC stack |
| `SignedFields` / `sign_request` / `verify_request` | `tunnel::a2a_signature` | ❌ Deleted | `build_pre_image` / `sign_pre_image` / `verify_pre_image` / `A2A_SIGNATURE_DOMAIN` remain (shared by `tunnel_channel_signature` + `invite_token`) |
| `tunnel::direct` (server/client/manager/routing/handshake) | `tunnel::direct` | ❌ Deleted | Direct transport retired; `tls.rs` relocated to `tunnel::tls` (tunnel client still consumes `build_client_config`) |
| `TunnelHost::pending_a2a_responses`; `AppState::{pending_a2a_responses, direct_manager, direct_*}`; `DirectHealth` | `tunnel::host`, `daemon::state` | ❌ Deleted | Host/state surface of the retired stack |
| `CrossRuntimeA2aCtx` | `tunnel::cross_runtime` | ⚠️ Changed | Now `{ directory, caller_runtime_id, principal_manager, channel_port: Arc<TunnelChannelPort>, response_timeout }` |
| `PeerMessenger::deliver_note` | `principal::messenger` | ⚠️ Changed | Note posts to the peer's DM channel (principal-authored root) instead of the chat log; child-JSONL append + trunk `[notify]` unchanged |
| ~~`root_session_id`~~; `root_session_id_for_channel` (trunk-only) | `principal::routers::root` | ❌ Deleted / narrowed | Per-peer root routing retired |
| Cron `Send` default target | `daemon::cron_engine`, `peko_cron` | ⚠️ Changed | Default = trunk; `TRUNK_MIN_INTERVAL_MS` covers `target: None` |
| `TRUNK_MIN_INTERVAL_MS` / `validate_trunk_send_interval` | `peko_cron::tools` | ✅ New | Phase 3b: 60s floor for trunk-targeted `Every` keepalive (token-burn guard); SpawnTool wake posts to `root:self` |

### Type Aliases for Backward Compatibility

```rust
// SessionContext → ExecutionContext
pub type SessionContext = ExecutionContext;

// SessionContext (key module) → SessionKeyContext  
pub type SessionContext = SessionKeyContext;
```

### PEKO Sprint 5 (2026-08-20) — slug-path addressing on the LLM-facing surface

Breaking (prelaunch): the LLM-facing addressing surface collapses to
slug paths. Three commits land in order (`96d3e55a`, `89ca46d9`,
then the doc sweep this section belongs to). Engine-internal call
sites keep using raw session ids via `resolve_id_or_path`; the
`SessionStoreBindingResolver` (channel binding) keeps its raw-id
passthrough by design — bindings are config-authored, not
LLM-authored. **Sprint 6 commit 2 collapsed the three-form grammar
to two and retired the engine-internal `resolve_id_or_path`** — see
the Sprint 6 section below.

| Component | Module | Status | Purpose |
|-----------|--------|--------|---------|
| `peko_session::path::resolve_reference` | `peko_session::path` | ✅ New | Single LLM-facing entry point: `/a/b/c` → `resolve_path`, `agent-c` → `resolve_relative` (BFS), raw id → REFUSED. **Sprint 6 commit 2** collapsed to two arms (`/`-path or caller self-reference); see below |
| `peko_session::path::resolve_id_or_path` | `peko_session::path` | ⚠️ Retired | Engine-internal entry point: same dispatch, raw ids pass through verbatim. Used by `resume_preflight`, `request_compaction`, `validate_context_parent` in `agents::subagent_executor.rs`. **Retired in sprint 6 commit 2** — engine sites now canonicalize via `SessionId::from` |
| `peko_session::path::resolve_relative` | `peko_session::path` | ⚠️ Retired | BFS descent resolver: direct children first (unique-per-parent), then breadth-first. **Retired in sprint 6 commit 2** alongside the caller-relative slug arm |
| `peko_session::path::looks_like_session_id` | `peko_session::path` | ⚠️ Retired | Shape heuristic for raw-id refusal: `:`-bearing values + 32+ char all-hex/dash blobs. **Retired in sprint 6 commit 2** — UUIDs lose discriminative power over legacy `:`-bearing shapes once engine ids are opaque |
| `validate_slug` (+`:` arm) | `peko_session::path` | ✅ Extended | Rejects `:` so slugs cannot collide with raw ids by construction |
| `Agent` `name` (REQUIRED for `action = "new"`) | `tools::builtin::messaging::agent` | ✅ Breaking | `validate_slug` runs upfront; old in-repo callers updated to pass `"writer"` |
| `Agent` `session_key` (raw-id refusal) | `tools::builtin::messaging::agent` | ✅ Breaking | `Resume` / `Compact` errors cite the slug-path grammar instead of "pass a session id" |
| `channel_binding::SessionStoreBindingResolver` (raw-id passthrough retained, doc-only) | `daemon::channel_binding` | ✅ Documented | Deliberate divergence — bindings come from `principals.toml` and never go through the tool surface |

**Deferred to follow-ups (NOT in sprint 5; some closed in sprint 6):**

- ✅ `peko_session::path::resolve_id_or_path` engine-internal helper — closed in sprint 6 commit 2 (engine sites canonicalize via `SessionId::from`).
- ✅ `peko_session::path::resolve_relative` BFS helper — closed in sprint 6 commit 2 alongside the caller-relative slug arm.
- ✅ `peko_session::path::looks_like_session_id` heuristic — closed in sprint 6 commit 2.
- `SessionInfo` dropping `session_key` / `session_id` (keep only `path` / `slug`)
- `SessionStatusResult` / `BranchOutcome` / `DeleteOutcome` / `CompactRequestOutcome` — replace raw `session_id` with computed `path`
- `ListScope { root: Option<String>, principal: bool }` + `SessionRuntime::list_sessions(scope: ListScope)` signature change
- `session list` default scope = caller's subtree (with `scope: "principal"` widening for privileged trunk callers and `path: "/other"` subtree scoping)
- `SessionCache` (test impl) — path-resolution seam
- Spawn response — replace `child_session_key` with `path` + `slug` (additive `child_path` on `SubagentMetadata` → `SubagentRunView`)

### PEKO Sprint 6 (2026-08-20) — opaque UUID session ids, peer is a channel concern

Breaking (prelaunch): the session layer takes back one job — storage
+ parent chain — and gives up peer routing, which has lived at the
channel layer since sprint 3 phase 12b. Three commits land in order
on `feat/agent-session-paradigm`:

1. `feat(session)!: opaque UUID session ids + peer via parent walk` (`6bcfa9fa`)
2. `refactor(session): collapse path resolver heuristics` (`2294f13e`)
3. The doc sweep (this section).

**Three-layer identity model:**

| Layer | Id shape | Job |
|---|---|---|
| LLM surface (Agent + session tools) | slug path (`/a/b/c`) | Address by human-meaningful name |
| Engine-internal session | opaque UUID | Storage key + parent-chain walk |
| Peer / routing | channel id (`principal:<did>`, `user:<id>`, `chan_<8>`, `group:<slug>`) | Conversation surface + cross-runtime |

| Component | Module | Status | Purpose |
|-----------|--------|--------|---------|
| `peko_session::SessionId` | `peko_session::id` | ✅ New | Newtype around `Uuid`, `#[serde(transparent)]`, `Copy`. Engine-internal `SessionMetadata.session_id` and `parent_session_id` are `SessionId`. `SessionId::from(s)` falls back to v5 UUID for non-UUID inputs so fixture-style literals round-trip without breakage |
| `peko_session::ownership::find_trunk_session` | `peko_session::ownership` | ✅ New | Replaces `trunk_session_id()` magic string. Returns the session with `parent_session_id = None`, or `None` if no trunk exists. The anchor for engine-managed guards (`err_live_base_managed`), `ensure_declared_children`, `ensure_peer_child`, `principal::routers::root::root_session_id_for_channel` |
| `peko_session::ownership::resolve_peer_via_parent_walk` | `peko_session::ownership` | ✅ New | Replaces `peer_from_session_key`. Walks `parent_session_id` from any leaf up to `parent_session_id = None`, reading stamped `peer_type` / `peer_id` on each ancestor; the trunk has no peer stamped so the walk terminates with `None`. Filter skips the `standing_*` / `spawn_*` placeholders |
| `peko_session::path::resolve_reference` (collapsed) | `peko_session::path` | ✅ Breaking | Three-form grammar (sprint 5) → two-form. `/`-prefixed path → `resolve_path`; non-`/` non-self → REFUSED. Caller self-reference shortcut retained for engine `current_session` shape |
| ❌ `trunk_session_id()` | `principal::routers::root` | ❌ Retired | Replaced by `find_trunk_session(metas)`. The legacy `root:self` magic-string id is gone |
| ❌ `peer_from_session_key` | `principal::messenger` | ❌ Retired | Replaced by `resolve_peer_via_parent_walk` |
| ❌ `parse_session_key_v2`, `parse_session_key` (v1), `base_key_from_overlay` | `peko_session::key` | ❌ Retired | The `agent:{a}:peer:{type}:{id}[:subagent:{uuid}][:overlay:{type}:{id}]` shape is gone. Peer identity comes from stamped metadata; agent identity from `meta.agent_name`; subagent / overlay identity from `parent_session_id` linking. JSONL filenames use `SessionId::as_str()` (UUIDs are filesystem-safe) |
| ❌ `peko_session::path::looks_like_session_id` | `peko_session::path` | ❌ Retired | Engine-internal ids are opaque UUIDs, so the heuristic loses discriminative power; the LLM-facing refusal now keys on the strict two-form grammar instead |
| ❌ `peko_session::path::resolve_id_or_path` | `peko_session::path` | ❌ Retired | Engine-internal callers in `agents::subagent_executor.rs` canonicalize via `SessionId::from` directly — no shape heuristic, no separate resolver |

**Risk callouts (none materialized):**

- **Subagent key shape change**: 15+ test files using `:peer:` / `:subagent:` / `:overlay:` literals migrated to UUID fixtures with parent-chain metadata. Mechanical sweep.
- **`find_trunk_session` `Option` return**: sites that load the principal use `.expect("trunk exists")`; sites that handle the no-trunk case use `.unwrap_or_default()`. Both patterns verified.
- **`child_session_key` → `child_session_id` DTO ripple**: DTOs are `serde(transparent)` over `SessionId`, so the wire shape stays a string. No IPC packet actually carries this field today.
- **`live_root_id` integration test helper**: rewritten to walk `sessions.json` for the entry with `parent_session_id = None`.

**Out of scope (deferred):**

- Migrating pre-existing on-disk sessions — prelaunch posture, no on-disk sessions exist.
- Multi-principal trunk lookup — today there's exactly one principal per runtime instance.
- Hash-keyed map optimization (`HashMap<String, ...>` → `HashMap<Uuid, ...>`) — opportunistic, not audited.

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
