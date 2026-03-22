# Public API Surface Documentation

> Generated during Phase 0 of refactoring - establishes baseline for compatibility

## Module: `agent`

### Public Types (Affected by Refactoring)

#### `agent::manager` (DEPRECATED - Phase 1)
```rust
pub struct AgentManager { ... }
impl AgentManager {
    pub async fn new() -> Result<(Self, mpsc::Receiver<ManagerEvent>)>
    pub async fn with_data_dir(data_dir: PathBuf) -> Result<(Self, mpsc::Receiver<ManagerEvent>)>
    pub async fn spawn(&self, config: AgentConfig) -> Result<AgentHandle>
    pub async fn stop(&self, did: &str) -> Result<()>
    pub async fn get(&self, did: &str) -> Option<AgentHandle>
    pub async fn get_by_name(&self, name: &str) -> Option<AgentHandle>
    pub async fn list_agents(&self) -> Vec<AgentInfo>
    pub async fn shutdown(&self) -> Result<()>
    pub fn create_all_tools(&self, agent_did: &str) -> Vec<Arc<dyn Tool>>
    pub async fn create_all_tools_async(&self, agent_did: &str) -> Result<Vec<Arc<dyn Tool>>>
    // ... 4 more tool creation methods
}
```

**Migration Path**: Use `StatelessAgentManager` instead

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

---

## Module: `providers`

### Public Types (Affected by Refactoring)

#### `providers::kimi` (TO BE CONSOLIDATED - Phase 2)
```rust
pub struct KimiProvider { ... }
impl KimiProvider {
    pub fn from_env() -> Result<Self>
    pub fn new(api_key: String) -> Self
    pub fn with_model(self, model: &str) -> Self
}
impl Provider for KimiProvider { ... }
```

**Migration Path**: Will become thin wrapper around `OpenAICompatibleBase`

#### `providers::kimi_code` (DEPRECATED - Phase 3)
```rust
pub struct KimiCodeProvider { ... }
pub struct KimiCodeConfig { ... }
impl KimiCodeProvider { ... }
```

**Migration Path**: Use `AnthropicProvider` or `MoonshotProvider` instead

#### `providers::openai_compatible` (ENHANCED - Phase 2)
```rust
pub struct OpenAICompatibleProvider { ... }
pub struct OpenAICompatibleConfig { ... }

impl OpenAICompatibleConfig {
    pub fn groq(api_key: &str, model: &str) -> Self
    pub fn together(api_key: &str, model: &str) -> Self
    pub fn fireworks(api_key: &str, model: &str) -> Self
}
```

**Change**: Will become base class for all OpenAI-compatible providers

---

## Module: `common::services`

### Public Types (Affected by Refactoring)

#### `common::services::agent_creation_service` (TO BE REMOVED - Phase 4)
```rust
pub struct AgentCreationService { ... }
pub struct AgentCreationRequest { ... }
pub enum AgentSource { ... }
pub struct AgentCreationResult { ... }

impl AgentCreationService {
    pub fn new(config_service: Arc<AgentConfigService>, path_resolver: PathResolver, team_manager: Arc<TeamManager>) -> Self
    pub async fn create(&self, request: AgentCreationRequest, auth_resolver: &dyn AuthResolver) -> Result<AgentCreationResult>
}
```

**Migration Path**: Use `AgentService::create_agent()` instead

#### `common::services::agent_config_builder` (TO BE REMOVED - Phase 4)
```rust
pub struct AgentConfigBuilder { ... }
pub fn build_default_config(name: &str, provider: &str, model: Option<String>, workspace: Option<PathBuf>) -> AgentConfig
pub async fn build_config_with_auth(...) -> Result<AgentConfig>
```

**Migration Path**: Use `AgentService` methods directly

#### `common::services::agent_service` (ENHANCED - Phase 4)
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

**Change**: Will absorb functionality from `AgentCreationService`

#### `common::services::agent_config_service` (REFINED - Phase 4)
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

**Change**: Will be reduced to filesystem-only operations

---

## Module: `tools::factory`

### Public Types (Affected by Refactoring)

#### `tools::factory::ToolFactory` (SIMPLIFIED - Phase 5)

**Current (6 methods)**:
```rust
impl ToolFactory {
    pub fn create_tools(config: &ToolFactoryConfig) -> ToolCreationResult
    pub async fn create_tools_async(config: &ToolFactoryConfig) -> Result<ToolCreationResult>
    pub fn create_minimal_tools(workspace_dir: PathBuf, disabled_tools: Vec<String>) -> ToolCreationResult
    pub fn create_coding_tools(workspace_dir: PathBuf, disabled_tools: Vec<String>) -> ToolCreationResult
    pub fn create_full_tools(workspace_dir: PathBuf, disabled_tools: Vec<String>) -> ToolCreationResult
    pub async fn create_full_tools_async(workspace_dir: PathBuf, disabled_tools: Vec<String>) -> Result<ToolCreationResult>
}
```

**Target (2 methods + configuration)**:
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

---

## Module: `session::context` & `agent::context`

### Public Types (Affected by Refactoring)

#### `session::context::SessionContext` (TO BE RENAMED - Phase 6)
```rust
pub struct SessionContext {
    pub hybrid: HybridSession,
    pub channel_type: Option<ChannelType>,
    pub is_subagent: bool,
}
```

**Migration Path**: Will be renamed to `ExecutionContext`

#### `session::key::SessionContext` (TO BE RENAMED - Phase 6)
```rust
pub struct SessionContext {  // Will become SessionKeyContext
    pub agent: String,
    pub team: String,
    pub session_type: SessionType,
    pub identifier: String,
}
```

**Migration Path**: Rename to `SessionKeyContext`

#### `agent::context::AgentContext` (DEPRECATED - Phase 6)
```rust
pub struct AgentContext { ... }
```

**Migration Path**: Use `session::context::ExecutionContext` instead

---

## Call Site Inventory

### Files using `AgentManager` (Phase 1)
```bash
# To find:
grep -r "AgentManager" --include="*.rs" src/ | grep -v "StatelessAgentManager" | grep -v "mod.rs"
```

### Files using `KimiProvider` (Phase 2)
```bash
# To find:
grep -r "KimiProvider" --include="*.rs" src/
```

### Files using `AgentCreationService` (Phase 4)
```bash
# To find:
grep -r "AgentCreationService" --include="*.rs" src/
```

### Files using convenience tool methods (Phase 5)
```bash
# To find:
grep -r "create_minimal_tools\|create_coding_tools\|create_full_tools" --include="*.rs" src/
```

---

## Compatibility Notes

### Breaking Changes
1. **Phase 1**: `AgentManager` removal (already deprecated in comments)
2. **Phase 3**: `KimiCodeProvider` removal
3. **Phase 4**: `AgentCreationService` removal
4. **Phase 4**: `AgentConfigBuilder` removal
5. **Phase 6**: `AgentContext` removal

### Non-Breaking Changes
1. **Phase 2**: `KimiProvider` becomes thin wrapper (same API)
2. **Phase 4**: `AgentService` gains new methods (backward compatible)
3. **Phase 5**: Convenience methods deprecated but still available
4. **Phase 6**: Context types get type aliases for backward compatibility

---

## Test Coverage

### Critical Paths
The following operations must be tested after each phase:

1. **Agent Lifecycle**
   - Create agent
   - Get agent info
   - List agents
   - Delete agent

2. **Provider Operations**
   - Chat with tools
   - Stream responses
   - Token usage tracking

3. **Session Operations**
   - Create session
   - Add messages
   - Branch session
   - Delete session

4. **Tool Operations**
   - Create tools
   - Execute filesystem tool
   - Execute process tool
