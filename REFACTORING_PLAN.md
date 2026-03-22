# Architecture Refactoring Plan

## Executive Summary

This plan addresses SRP and DRY violations identified in the codebase. The refactoring is organized into 7 phases, prioritized by impact and dependency order. Each phase includes incremental steps to minimize risk.

**Estimated Effort**: 2-3 weeks  
**Risk Level**: Medium (extensive test coverage required)  
**Breaking Changes**: Minimal (mostly internal refactoring)

---

## Phase 0: Preparation & Safety (Day 1)

### Goal
Establish safety nets before making changes.

### Tasks

1. **Create Integration Test Suite**
   ```bash
   # Create tests that verify current behavior
   cargo test --lib -- --test-threads=1 > baseline_tests.log
   ```

2. **Document Public API Surface**
   - Identify all `pub` types in affected modules
   - Mark which are internal vs external API
   - Create API compatibility checklist

3. **Add Deprecation Attributes**
   ```rust
   // In agent/manager.rs
   #[deprecated(since = "0.9.0", note = "Use StatelessAgentManager instead")]
   pub struct AgentManager { ... }
   ```

### Success Criteria
- [ ] All existing tests pass
- [ ] Public API surface documented
- [ ] Deprecation warnings added to legacy types

---

## Phase 1: Consolidate Agent Managers (CRITICAL) - Days 2-4

### Goal
Eliminate the dual manager architecture.

### Current State
- `AgentManager` (447 lines, deprecated)
- `StatelessAgentManager` (377 lines, active)
- ~200 lines of duplicated logic

### Strategy: Adapter Pattern Migration

#### Step 1.1: Extract Common Interface (Day 2)

Create `src/agent/manager_trait.rs`:

```rust
//! Unified agent manager trait
//! 
//! This trait abstracts both legacy and stateless managers
//! during the migration period.

use async_trait::async_trait;

#[async_trait]
pub trait AgentManager: Send + Sync {
    async fn list(&self) -> anyhow::Result<Vec<AgentInfo>>;
    async fn get(&self, name: &str, team: Option<&str>) -> anyhow::Result<Option<AgentInfo>>;
    async fn shutdown(&self) -> anyhow::Result<()>;
    // ... other common methods
}
```

#### Step 1.2: Implement Adapter for Legacy Manager (Day 2-3)

Create `src/agent/legacy_adapter.rs`:

```rust
//! Adapter that wraps StatelessAgentManager to provide legacy AgentManager interface

pub struct LegacyAgentManagerAdapter {
    inner: StatelessAgentManager,
}

impl LegacyAgentManagerAdapter {
    pub async fn new() -> anyhow::Result<(Self, mpsc::Receiver<ManagerEvent>)> {
        let (inner, events) = StatelessAgentManager::new().await?;
        // Transform StatelessManagerEvent -> ManagerEvent
        let events = transform_events(events);
        Ok((Self { inner }, events))
    }
    
    // Delegate all methods to inner stateless manager
    pub async fn spawn(&self, config: AgentConfig) -> Result<AgentHandle> {
        // Transform to stateless execution
        let request = ExecutionRequest::from_config(config);
        let result = self.inner.execute(request).await?;
        // Transform ExecutionResult -> AgentHandle
        Ok(result.into())
    }
}
```

#### Step 1.3: Update Call Sites (Day 3-4)

Find all usages of `AgentManager`:

```bash
grep -r "AgentManager" --include="*.rs" src/ | grep -v "StatelessAgentManager" | grep -v "mod.rs"
```

Update each call site:
1. Replace `AgentManager::new()` with `StatelessAgentManager::new()`
2. Transform event handling if needed
3. Update method calls to new API

#### Step 1.4: Remove Legacy Manager (Day 4)

Once all call sites updated:

```bash
# Remove files
rm src/agent/manager.rs

# Update mod.rs
# Remove: pub mod manager;
# Keep: pub mod stateless_manager;
```

### Success Criteria
- [ ] `AgentManager` removed from codebase
- [ ] All tests pass
- [ ] No regression in agent lifecycle operations
- [ ] API routes work correctly

---

## Phase 2: Unify Provider Implementations (HIGH) - Days 5-7

### Goal
Eliminate duplication between OpenAI-compatible providers.

### Current State
- `openai.rs` (758 lines) - Base implementation
- `kimi.rs` (605 lines) - 90% duplicate of OpenAI
- `openai_compatible.rs` (494 lines) - Generic OpenAI-compatible

### Strategy: Composition over Inheritance

#### Step 2.1: Create OpenAI-Compatible Base (Day 5)

Refactor `src/providers/openai_compatible.rs`:

```rust
//! Generic OpenAI-compatible provider base

pub struct OpenAICompatibleBase {
    config: OpenAICompatibleConfig,
    client: Client,
    name: String,
}

impl OpenAICompatibleBase {
    pub fn new(name: &str, config: OpenAICompatibleConfig) -> anyhow::Result<Self> { ... }
    
    // Move all common logic here
    pub async fn chat_with_tools(&self, ...) -> anyhow::Result<ChatResponse> { ... }
    pub async fn stream_with_tools(&self, ...) -> anyhow::Result<...> { ... }
    
    // Protected methods for customization
    fn convert_messages(&self, messages: &[ChatMessage]) -> Vec<Value> { ... }
    fn convert_tools(&self, tools: &[ToolDefinition]) -> Vec<Value> { ... }
}
```

#### Step 2.2: Refactor OpenAI Provider (Day 5-6)

Update `src/providers/openai.rs`:

```rust
//! OpenAI provider - thin wrapper around OpenAICompatibleBase

pub struct OpenAIProvider {
    base: OpenAICompatibleBase,
}

#[async_trait]
impl Provider for OpenAIProvider {
    fn name(&self) -> &'static str { "openai" }
    
    async fn chat_with_tools(&self, messages: &[ChatMessage], tools: &[ToolDefinition], options: &ChatOptions) -> anyhow::Result<ChatResponse> {
        // Delegate to base
        self.base.chat_with_tools(messages, tools, options).await
    }
    
    // Override only what's different
    async fn complete(&self, prompt: &str) -> anyhow::Result<String> {
        // OpenAI-specific optimizations
    }
}
```

#### Step 2.3: Migrate Kimi to Use Base (Day 6-7)

Replace `src/providers/kimi.rs`:

```rust
//! Kimi (Moonshot) provider
//! 
//! Kimi uses OpenAI-compatible API - this is a thin wrapper.

pub struct KimiProvider {
    base: OpenAICompatibleBase,
}

impl KimiProvider {
    pub fn new(api_key: String) -> anyhow::Result<Self> {
        let config = OpenAICompatibleConfig {
            api_key,
            base_url: "https://api.moonshot.cn/v1".to_string(),
            model: "kimi-k2.5".to_string(),
            ..Default::default()
        };
        Ok(Self {
            base: OpenAICompatibleBase::new("kimi", config)?,
        })
    }
}

#[async_trait]
impl Provider for KimiProvider {
    fn name(&self) -> &'static str { self.base.name() }
    
    // Delegate ALL methods to base
    async fn chat_with_tools(&self, messages: &[ChatMessage], tools: &[ToolDefinition], options: &ChatOptions) -> anyhow::Result<ChatResponse> {
        self.base.chat_with_tools(messages, tools, options).await
    }
    
    async fn stream_with_tools(&self, ...) -> anyhow::Result<...> {
        self.base.stream_with_tools(...).await
    }
    
    // ... all other methods delegate
}
```

#### Step 2.4: Remove Duplicated SSE Parsing (Day 7)

Extract common SSE parsing to `src/providers/sse.rs`:

```rust
//! Shared SSE parsing utilities

pub struct SseParser<T> {
    buffer: String,
    _phantom: PhantomData<T>,
}

impl<T: DeserializeOwned> SseParser<T> {
    pub fn new() -> Self { ... }
    pub fn feed(&mut self, chunk: &str) -> Vec<T> { ... }
    pub fn parse_event(&self, line: &str) -> Option<T> { ... }
}

// Common stream state that works for all OpenAI-compatible providers
pub struct OpenAIStreamState {
    pub model: String,
    pub provider: String,
    pub content: Vec<ContentBlock>,
    pub tool_calls: Vec<PartialToolCall>,
    pub text_buffer: String,
}
```

Update all providers to use shared SSE parsing.

### Success Criteria
- [ ] Kimi provider < 100 lines (was 605)
- [ ] OpenAI provider < 200 lines (was 758)
- [ ] SSE parsing deduplicated
- [ ] All provider tests pass
- [ ] No behavioral changes

---

## Phase 3: Merge Kimi Providers (HIGH) - Days 8-9

### Goal
Resolve confusion between kimi.rs and kimi_code.rs.

### Strategy: Clear Naming + Consolidation

#### Step 3.1: Rename for Clarity (Day 8)

```bash
# Rename files for clarity
mv src/providers/kimi.rs src/providers/moonshot.rs
mv src/providers/kimi_code.rs src/providers/kimi_code.rs  # Keep but deprecate

# Update mod.rs
pub mod moonshot;  // OpenAI-compatible Moonshot API
pub mod kimi_code; // Anthropic-based Kimi Code - DEPRECATED
```

Update all internal references:
- `KimiProvider` → `MoonshotProvider`
- `KimiConfig` → `MoonshotConfig`

#### Step 3.2: Extract Shared Anthropic Types (Day 8-9)

Create `src/providers/anthropic_types.rs`:

```rust
//! Shared Anthropic API types
//! 
//! Used by AnthropicProvider and KimiCodeProvider

#[derive(Debug, Serialize)]
pub struct MessagesRequest { ... }

#[derive(Debug, Serialize, Deserialize)]
pub struct Message { ... }

#[derive(Debug, Deserialize)]
pub struct MessagesResponse { ... }

#[derive(Debug, Deserialize)]
pub struct ContentBlock { ... }

#[derive(Debug, Deserialize)]
pub struct Usage { ... }
```

Update `anthropic.rs` and `kimi_code.rs` to use shared types.

#### Step 3.3: Deprecate Kimi Code (Day 9)

Add deprecation to `kimi_code.rs`:

```rust
//! Kimi Code provider (DEPRECATED)
//! 
//! This provider is being phased out. Use AnthropicProvider directly
//! or migrate to MoonshotProvider (kimi.rs).

#[deprecated(since = "0.9.0", note = "Use AnthropicProvider or MoonshotProvider instead")]
pub struct KimiCodeProvider { ... }
```

### Success Criteria
- [ ] Clear naming (Moonshot vs Kimi Code)
- [ ] Anthropic types shared between providers
- [ ] Deprecation warnings in place
- [ ] Documentation updated

---

## Phase 4: Refactor Service Layer (MEDIUM) - Days 10-13

### Goal
Eliminate duplication between AgentService, AgentCreationService, and AgentConfigService.

### Current State
- `agent_service.rs` (589 lines) - CRUD operations
- `agent_creation_service.rs` (260 lines) - Creation only
- `agent_config_service.rs` (350 lines) - Config management

### Strategy: Layered Architecture

```
┌─────────────────────────────────────┐
│  AgentService (High-level API)      │  ← Public interface
├─────────────────────────────────────┤
│  AgentCreationService (Orchestration)│  ← Complex operations
├─────────────────────────────────────┤
│  AgentConfigService (Storage)       │  ← Filesystem only
└─────────────────────────────────────┘
```

#### Step 4.1: Reduce AgentConfigService Scope (Day 10)

Refactor to be pure filesystem storage:

```rust
//! Agent configuration storage service
//! 
//! RESPONSIBILITY: Filesystem CRUD only. No business logic.

pub struct AgentConfigService {
    path_resolver: PathResolver,
    cache: RwLock<HashMap<(String, String), AgentConfigEntry>>,
}

impl AgentConfigService {
    // Keep only these methods:
    pub async fn get(&self, name: &str, team: &str) -> Result<Option<AgentConfigEntry>>;
    pub async fn save(&self, name: &str, team: &str, config: &AgentConfig) -> Result<()>;
    pub async fn delete(&self, name: &str, team: &str) -> Result<bool>;
    pub async fn list_in_team(&self, team: &str) -> Result<Vec<AgentConfigEntry>>;
    pub async fn list_all(&self) -> Result<Vec<AgentConfigEntry>>;
    pub async fn exists(&self, name: &str, team: &str) -> Result<bool>;
    
    // Remove: caching logic, team validation, etc.
}
```

#### Step 4.2: Merge Creation into AgentService (Day 11-12)

Move `AgentCreationService::create()` into `AgentService`:

```rust
impl AgentService {
    /// Create a new agent (unified method)
    /// 
    /// Combines functionality from:
    /// - AgentService::create_agent
    /// - AgentCreationService::create
    pub async fn create(&self, request: AgentCreateRequest) -> Result<AgentCreationResult> {
        // Single implementation that handles:
        // 1. Validation (name, team)
        // 2. Team auto-creation
        // 3. Config building (from image or provider)
        // 4. Filesystem operations
        // 5. Workspace bootstrapping
    }
}
```

Remove `AgentCreationService` entirely.

#### Step 4.3: Update Call Sites (Day 12-13)

Find all usages:

```bash
grep -r "AgentCreationService" --include="*.rs" src/
```

Update:
- `api/routes/agents.rs` - Use `AgentService::create()`
- `commands/agent.rs` - Use `AgentService::create()`
- `common/services/mod.rs` - Remove export

#### Step 4.4: Consolidate Config Building (Day 13)

Merge `AgentConfigBuilder` into `AgentService`:

```rust
impl AgentService {
    /// Build default configuration for a new agent
    fn build_default_config(&self, name: &str, provider: &str, model: Option<String>) -> AgentConfig {
        // Move logic from agent_config_builder.rs
    }
}
```

Remove `agent_config_builder.rs`.

### Success Criteria
- [ ] `AgentCreationService` removed
- [ ] `agent_config_builder.rs` removed
- [ ] `AgentService` < 600 lines (was 589 + merged code)
- [ ] `AgentConfigService` only handles filesystem
- [ ] All tests pass

---

## Phase 5: Simplify Tool Factory (MEDIUM) - Days 14-15

### Goal
Reduce 6 creation methods to 2.

### Current State
- `create_tools()` - sync, no MCP
- `create_tools_async()` - async, with MCP
- `create_minimal_tools()` - filesystem + process only
- `create_coding_tools()` - filesystem + apply_patch + process
- `create_full_tools()` - sync, all tools
- `create_full_tools_async()` - async, all tools + MCP

### Strategy: Configuration-Driven Creation

#### Step 5.1: Create Tool Presets (Day 14)

Add preset configurations to `ToolFactoryConfig`:

```rust
impl ToolFactoryConfig {
    /// Minimal tools: filesystem + process only
    pub fn minimal(workspace_dir: PathBuf) -> Self {
        Self {
            workspace_dir,
            enable_filesystem: true,
            enable_apply_patch: false,
            enable_process: true,
            enable_session_tools: false,
            enable_cron: false,
            mcp: McpFactoryConfig::disabled(),
            ..Default::default()
        }
    }
    
    /// Coding tools: filesystem + apply_patch + process
    pub fn coding(workspace_dir: PathBuf) -> Self {
        Self {
            workspace_dir,
            enable_filesystem: true,
            enable_apply_patch: true,
            enable_process: true,
            enable_session_tools: false,
            enable_cron: false,
            mcp: McpFactoryConfig::disabled(),
            ..Default::default()
        }
    }
    
    /// Full tools: everything except MCP (use async for MCP)
    pub fn full(workspace_dir: PathBuf) -> Self {
        Self {
            workspace_dir,
            enable_filesystem: true,
            enable_apply_patch: true,
            enable_process: true,
            enable_session_tools: true,
            enable_cron: true,
            mcp: McpFactoryConfig::disabled(),
            ..Default::default()
        }
    }
}

impl McpFactoryConfig {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Default::default()
        }
    }
}
```

#### Step 5.2: Deprecate Convenience Methods (Day 14-15)

Mark convenience methods as deprecated:

```rust
impl ToolFactory {
    #[deprecated(since = "0.9.0", note = "Use create_tools with ToolFactoryConfig::minimal() instead")]
    pub fn create_minimal_tools(workspace_dir: PathBuf, disabled_tools: Vec<String>) -> ToolCreationResult {
        let config = ToolFactoryConfig::minimal(workspace_dir);
        Self::create_tools(&config)
    }
    
    #[deprecated(since = "0.9.0", note = "Use create_tools with ToolFactoryConfig::coding() instead")]
    pub fn create_coding_tools(workspace_dir: PathBuf, disabled_tools: Vec<String>) -> ToolCreationResult {
        let config = ToolFactoryConfig::coding(workspace_dir);
        Self::create_tools(&config)
    }
    
    #[deprecated(since = "0.9.0", note = "Use create_tools with ToolFactoryConfig::full() instead")]
    pub fn create_full_tools(workspace_dir: PathBuf, disabled_tools: Vec<String>) -> ToolCreationResult {
        let config = ToolFactoryConfig::full(workspace_dir);
        Self::create_tools(&config)
    }
    
    #[deprecated(since = "0.9.0", note = "Use create_tools_async with ToolFactoryConfig::full() instead")]
    pub async fn create_full_tools_async(workspace_dir: PathBuf, disabled_tools: Vec<String>) -> anyhow::Result<ToolCreationResult> {
        let config = ToolFactoryConfig::full(workspace_dir);
        Self::create_tools_async(&config).await
    }
}
```

#### Step 5.3: Update Call Sites (Day 15)

```bash
grep -r "create_minimal_tools\|create_coding_tools\|create_full_tools" --include="*.rs" src/
```

Update each call site to use configuration:

```rust
// Before:
let tools = ToolFactory::create_coding_tools(workspace, vec![]);

// After:
let config = ToolFactoryConfig::coding(workspace);
let tools = ToolFactory::create_tools(&config);
```

### Success Criteria
- [ ] Only 2 creation methods remain: `create_tools()` and `create_tools_async()`
- [ ] All convenience methods deprecated
- [ ] Call sites updated
- [ ] Factory < 500 lines (was 906)

---

## Phase 6: Clarify Context Types (MEDIUM) - Days 16-17

### Goal
Eliminate confusion between multiple Context types.

### Current State
- `session::context::SessionContext` - agent execution context
- `session::key::SessionContext` - session key parsing context
- `agent::context::AgentContext` - agent execution context

### Strategy: Clear Naming + Consolidation

#### Step 6.1: Rename Session Key Context (Day 16)

Rename to clarify its purpose:

```rust
// In session/key.rs

/// Context extracted from a session key string
/// 
/// This is used for parsing and validating session key formats,
/// NOT for runtime execution context.
#[derive(Debug)]
pub struct SessionKeyContext {  // Was: SessionContext
    pub agent: String,
    pub team: String,
    pub session_type: SessionType,
    pub identifier: String,
}
```

#### Step 6.2: Consolidate Execution Contexts (Day 16-17)

Merge `agent::context::AgentContext` into `session::context::SessionContext`:

```rust
// In session/context.rs

/// Unified context for agent execution
/// 
/// Combines:
/// - Session information (hybrid session, overlays)
/// - Agent information (config, capabilities)
/// - Execution metadata (channel, subagent status)
#[derive(Debug, Clone)]
pub struct ExecutionContext {  // Was: SessionContext
    // Session info
    pub hybrid: HybridSession,
    pub channel_type: Option<ChannelType>,
    pub is_subagent: bool,
    
    // Agent info (moved from AgentContext)
    pub agent_config: Arc<AgentConfig>,
    pub agent_capabilities: Vec<Capability>,
    
    // Tool context
    pub tool_context: ToolContext,
}
```

Deprecate `agent::context::AgentContext`:

```rust
// In agent/context.rs

#[deprecated(since = "0.9.0", note = "Use session::context::ExecutionContext instead")]
pub type AgentContext = crate::session::context::ExecutionContext;
```

#### Step 6.3: Update Imports (Day 17)

```bash
grep -r "use.*AgentContext" --include="*.rs" src/
grep -r "use.*SessionContext" --include="*.rs" src/
```

Update all imports to use `ExecutionContext`.

### Success Criteria
- [ ] `SessionKeyContext` renamed from `session/key.rs`
- [ ] `ExecutionContext` unified context created
- [ ] `AgentContext` deprecated/removed
- [ ] All call sites updated
- [ ] No naming confusion

---

## Phase 7: Clean Up Legacy Code (LOW) - Days 18-20

### Goal
Remove deprecated code and update documentation.

### Tasks

#### Step 7.1: Remove Deprecated Files (Day 18)

After all migrations complete:

```bash
# Remove files marked deprecated in earlier phases
rm src/agent/manager.rs  # Already removed in Phase 1
rm src/providers/kimi_code.rs  # If fully deprecated
rm src/common/services/agent_creation_service.rs  # Already removed in Phase 4
rm src/common/services/agent_config_builder.rs  # Already removed in Phase 4
```

#### Step 7.2: Update Module Exports (Day 18)

Update `mod.rs` files:

```rust
// src/providers/mod.rs
pub mod anthropic;
pub mod moonshot;  // Renamed from kimi
pub mod openai;
pub mod openai_compatible;
pub mod registry;
pub mod sse;
pub mod traits;

// Remove exports:
// pub mod kimi;  // Now moonshot
// pub mod kimi_code;  // Deprecated and removed
```

#### Step 7.3: Update Documentation (Day 19-20)

Update all relevant documentation:

1. **API_CONTRACT.md** - Update provider examples
2. **CAPABILITY_INTERFACE.md** - Update service layer descriptions
3. **README.md** - Update architecture diagrams
4. **CHANGELOG.md** - Document breaking changes

#### Step 7.4: Final Test Pass (Day 20)

```bash
# Full test suite
cargo test --all-features

# Clippy checks
cargo clippy --all-targets --all-features -- -D warnings

# Format check
cargo fmt --check
```

### Success Criteria
- [ ] All deprecated code removed
- [ ] Documentation updated
- [ ] All tests pass
- [ ] No compiler warnings

---

## Testing Strategy

### Unit Tests
Each phase requires:
1. Tests for new code
2. Tests for deprecated code behavior (until removal)
3. Migration tests (old API → new API)

### Integration Tests
Critical paths to test after each phase:
1. Agent creation flow
2. Message sending flow
3. Provider API calls (mocked)
4. Session management
5. Tool execution

### Regression Tests
```bash
# Before each phase
cargo test --lib > before_phase_N.log

# After each phase
cargo test --lib > after_phase_N.log

# Compare diff before_phase_N.log after_phase_N.log
```

---

## Risk Mitigation

### Rollback Plan

Each phase has a rollback branch:

```bash
# Before starting phase N
git checkout -b refactor/phase-N-attempt
git push -u origin refactor/phase-N-attempt

# If issues arise
git checkout main
git branch -D refactor/phase-N-attempt
```

### Feature Flags (Optional)

For high-risk changes, use feature flags:

```rust
#[cfg(feature = "legacy-manager")]
pub use manager::AgentManager;

#[cfg(not(feature = "legacy-manager"))]
pub use stateless_manager::StatelessAgentManager as AgentManager;
```

### Staged Rollout

1. **Week 1**: Phases 0-2 (Critical/High priority)
2. **Week 2**: Phases 3-4 (High/Medium priority)
3. **Week 3**: Phases 5-7 (Medium/Low priority)

---

## Metrics

### Before
| Metric | Value |
|--------|-------|
| Total lines in providers/ | ~6,000 |
| Total lines in agent/ | ~2,000 |
| Total lines in common/services/ | ~2,500 |
| Duplicate code blocks | 15+ |
| Deprecated types | 5 |

### Target After
| Metric | Value |
|--------|-------|
| Total lines in providers/ | ~3,500 (-40%) |
| Total lines in agent/ | ~1,400 (-30%) |
| Total lines in common/services/ | ~1,800 (-28%) |
| Duplicate code blocks | 3 |
| Deprecated types | 0 |

---

## Summary

This refactoring plan addresses all identified SRP and DRY violations through:

1. **Incremental changes** - Each phase is self-contained
2. **Backward compatibility** - Deprecation periods allow gradual migration
3. **Clear ownership** - Each module has a single responsibility
4. **Code reuse** - Common logic extracted into shared components

**Total Estimated Time**: 20 working days (4 weeks)  
**Risk**: Low to Medium (with proper testing)  
**Benefit**: 30-40% code reduction, improved maintainability
