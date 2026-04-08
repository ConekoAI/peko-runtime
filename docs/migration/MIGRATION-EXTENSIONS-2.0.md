# Migration Plan: Extensions 2.0

This document details the phased migration from the current extension systems (skills, MCP, universal tools, channels, hooks, gateways) to the Unified Extension Architecture (ADR-017).

## Current State Overview

```
src/
├── skills/           # SKILL.md loading and prompt injection
├── mcp/              # MCP client, transport, tool proxy
├── tools/
│   └── universal/    # Universal tool protocol
├── channels/         # CLI, Discord I/O
├── hooks/            # Event bus, file watch, webhooks
└── gateway/          # Gateway plugin interface
```

Each module has independent:
- Discovery mechanisms
- Configuration formats
- Registration APIs
- Lifecycle management

## Target State Overview

```
src/
└── extensions/
    ├── core/              # Hook point definitions, registry
    │   ├── hook_points.rs
    │   ├── registry.rs
    │   ├── context.rs
    │   └── mod.rs
    ├── adapters/          # Extension type implementations
    │   ├── skill_adapter.rs
    │   ├── mcp_adapter.rs
    │   ├── universal_tool_adapter.rs
    │   ├── channel_adapter.rs
    │   ├── hook_adapter.rs
    │   └── gateway_adapter.rs
    ├── manager/           # Extension lifecycle management
    │   ├── discovery.rs
    │   ├── lifecycle.rs
    │   ├── bundling.rs
    │   └── mod.rs
    └── types.rs           # Shared types
```

## Migration Phases

### Phase 1: Foundation (Week 1-2)

**Goal:** Create the Extension Core without disrupting existing systems.

#### 1.1 Create Extension Core Module

```bash
# New files to create
src/extensions/mod.rs
src/extensions/core/mod.rs
src/extensions/core/hook_points.rs
src/extensions/core/registry.rs
src/extensions/core/context.rs
src/extensions/types.rs
```

**Implementation:**
- Define `HookPoint` enum with all known hook points
- Implement `ExtensionCore` registry (register/unregister/enable/disable)
- Define `HookHandler` trait and `HookContext`
- Add comprehensive unit tests

**Backward Compatibility:** None affected (new module).

#### 1.2 Add Integration Points to Existing Code

**In `src/prompt/builder.rs`:**
```rust
// Add hook point invocation
pub fn build(&self) -> String {
    // ... existing code ...
    
    // NEW: Allow extensions to inject into prompt sections
    let tools_section = self.invoke_hooks(
        HookPoint::PromptSystemSection { 
            section: "tools".to_string(), 
            priority: 0 
        },
        self.build_tools_section()
    );
    
    // ... rest of build ...
}
```

**In `src/engine/loop_v4.rs`:**
```rust
// Add tool registration hook point
async fn run_loop(...) {
    // NEW: Allow extensions to register tools
    let dynamic_tools = extension_core.invoke_hooks(
        HookPoint::ToolRegister,
        ()
    ).await;
    
    let all_tools = [&self.tools, &dynamic_tools].concat();
    // ... rest of loop ...
}
```

**Backward Compatibility:** Existing code paths remain unchanged; hook points initially return empty results.

### Phase 2: Skill Adapter (Week 2-3)

**Goal:** Migrate skills to the new system as the first adapter.

#### 2.1 Create Skill Adapter

```bash
src/extensions/adapters/skill_adapter.rs   # NEW
```

**Implementation:**
```rust
pub struct SkillAdapter;

impl ExtensionTypeAdapter for SkillAdapter {
    fn extension_type(&self) -> &'static str { "skill" }
    
    fn manifest_format(&self) -> ManifestFormat {
        ManifestFormat::YamlFrontmatterMarkdown {
            required_fields: vec!["name", "description"],
            file_name: "SKILL.md",
        }
    }
    
    fn resolve_hooks(&self, manifest: &ExtensionManifest) -> Vec<HookBinding> {
        vec![HookBinding {
            point: HookPoint::PromptSystemSection {
                section: "skills".to_string(),
                priority: 100,
            },
            handler_factory: Box::new(SkillPromptHandlerFactory {
                manifest: manifest.clone(),
            }),
        }]
    }
}
```

#### 2.2 Dual-Mode Skills

Keep `src/skills/mod.rs` but refactor to use Extension Core:

```rust
// src/skills/mod.rs - REFACTORED
pub fn build_skills_prompt(skills: &[&Skill]) -> String {
    // DEPRECATED: Direct building
    // NEW: Delegate to extension core
    if let Some(core) = ExtensionCore::global() {
        core.invoke_hook(HookPoint::PromptSystemSection { 
            section: "skills".to_string(),
            priority: 0,
        })
    } else {
        // Fallback to legacy implementation
        legacy_build_skills_prompt(skills)
    }
}
```

#### 2.3 Migration Path for Users

Existing skills continue to work (legacy fallback). New skills can be installed via:

```bash
# NEW way
pekobot ext install ~/.pekobot/skills/docker

# OLD way (still works during transition)
# (skills auto-discovered from skills directory)
```

**Backward Compatibility:** 100% - legacy discovery continues to work.

### Phase 3: Universal Tool Adapter (Week 3-4)

**Goal:** Migrate universal tools to Extension Core.

#### 3.1 Create Universal Tool Adapter

```bash
src/extensions/adapters/universal_tool_adapter.rs   # NEW
```

**Implementation:**
- Detect: `MANIFEST.toml` files
- Hooks: `ToolRegister`, `PromptSystemSection`, `ToolExecute`
- Refactor `src/tools/universal/adapter.rs` to use Extension Core

#### 3.2 Refactor Tool Registration

**In `src/tools/universal/adapter.rs`:**
```rust
// REFACTORED to register via Extension Core
pub async fn register_universal_tools(registry: &ToolRegistry) {
    // OLD: Direct registration
    // for tool in discover_tools() {
    //     registry.register(tool);
    // }
    
    // NEW: Via extension core
    ExtensionCore::global()
        .unwrap()
        .invoke_hook(HookPoint::ToolRegister)
        .await;
}
```

**Backward Compatibility:** Existing tool discovery continues to work; new tools can use either path.

### Phase 4: MCP Adapter (Week 4-5)

**Goal:** Migrate MCP servers to Extension Core.

#### 4.1 Create MCP Adapter

```bash
src/extensions/adapters/mcp_adapter.rs   # NEW
```

**Challenges:**
- MCP servers are stateful (active connections)
- Need lifecycle management (start/stop/reconnect)

**Implementation:**
```rust
pub struct McpAdapter {
    client_manager: Arc<McpClientManager>,
}

impl ExtensionTypeAdapter for McpAdapter {
    fn extension_type(&self) -> &'static str { "mcp" }
    
    fn resolve_hooks(&self, manifest: &ExtensionManifest) -> Vec<HookBinding> {
        vec![
            HookBinding::tool_register(),
            HookBinding::prompt_section("tools"),
            HookBinding::tool_execute_pattern(format!("mcp:{}:*", manifest.name)),
        ]
    }
    
    async fn initialize(&self, manifest: &ExtensionManifest) -> Result<ExtensionState> {
        // Start MCP server process
        let client = self.client_manager.connect(manifest).await?;
        Ok(ExtensionState::McpClient(client))
    }
    
    async fn shutdown(&self, state: ExtensionState) -> Result<()> {
        if let ExtensionState::McpClient(client) = state {
            client.shutdown().await?;
        }
        Ok(())
    }
}
```

#### 4.2 Refactor MCP Manager

**In `src/mcp/manager.rs`:**
```rust
// REFACTORED: Delegate lifecycle to Extension Manager
pub struct McpManager {
    // OLD: Direct server management
    // servers: HashMap<String, ServerHandle>,
    
    // NEW: Use Extension Manager
    extension_id: ExtensionId,
}

impl McpManager {
    pub async fn start_server(&self, config: McpServerConfig) -> Result<()> {
        // Register as extension instead of direct management
        ExtensionManager::global()
            .install_from_config(config)
            .await
    }
}
```

**Backward Compatibility:** Existing `mcp_servers/` directory auto-migrated on first run.

### Phase 5: Channel Adapter (Week 5-6)

**Goal:** Migrate CLI and Discord channels to Extension Core.

#### 5.1 Create Channel Adapter

```bash
src/extensions/adapters/channel_adapter.rs   # NEW
```

**Implementation:**
```rust
pub struct ChannelAdapter;

impl ExtensionTypeAdapter for ChannelAdapter {
    fn extension_type(&self) -> &'static str { "channel" }
    
    fn resolve_hooks(&self, manifest: &ExtensionManifest) -> Vec<HookBinding> {
        vec![
            HookBinding::channel_input(),
            HookBinding::channel_output(),
            HookBinding::message_transform(),
        ]
    }
}
```

#### 5.2 Refactor Channel System

**In `src/channels/mod.rs`:**
```rust
// REFACTORED: Channel trait now implements HookHandler
#[async_trait]
impl HookHandler for dyn Channel {
    fn hook_point(&self) -> HookPoint {
        HookPoint::ChannelInput
    }
    
    async fn handle(&self, ctx: HookContext) -> HookResult {
        // Channel-specific handling
    }
}
```

**Backward Compatibility:** Existing channel configs continue to work.

### Phase 6: Hook & Gateway Adapters (Week 6-7)

**Goal:** Migrate hooks and gateways.

#### 6.1 Hook Adapter

The existing `src/hooks/` module becomes an extension type:

```rust
// src/extensions/adapters/hook_adapter.rs
pub struct HookExtensionAdapter;

impl ExtensionTypeAdapter for HookExtensionAdapter {
    fn extension_type(&self) -> &'static str { "hook" }
    
    fn resolve_hooks(&self, manifest: &ExtensionManifest) -> Vec<HookBinding> {
        vec![
            HookBinding::event_subscribe(manifest.get("topic").unwrap()),
            HookBinding::webhook_endpoint(manifest.get("path").unwrap()),
        ]
    }
}
```

#### 6.2 Gateway Adapter

```rust
// src/extensions/adapters/gateway_adapter.rs
pub struct GatewayAdapter;

impl ExtensionTypeAdapter for GatewayAdapter {
    fn extension_type(&self) -> &'static str { "gateway" }
    
    fn resolve_hooks(&self, manifest: &ExtensionManifest) -> Vec<HookBinding> {
        vec![
            HookBinding::channel_input(),
            HookBinding::channel_output(),
            HookBinding::tool_register(),  // Gateway-specific tools
            HookBinding::event_emit(),     // Platform events
        ]
    }
}
```

**Backward Compatibility:** Existing gateway plugins continue to work via adapter.

### Phase 7: Extension Manager & CLI (Week 7-8)

**Goal:** Implement the user-facing `pekobot ext` commands.

#### 7.1 Extension Manager Implementation

```bash
src/extensions/manager/mod.rs
src/extensions/manager/discovery.rs
src/extensions/manager/lifecycle.rs
src/extensions/manager/bundling.rs
```

**Implementation:**
```rust
pub struct ExtensionManager {
    core: Arc<ExtensionCore>,
    adapters: HashMap<String, Box<dyn ExtensionTypeAdapter>>,
    extensions: HashMap<ExtensionId, LoadedExtension>,
    storage: ExtensionStorage,
}

impl ExtensionManager {
    pub async fn install(&mut self, path: &Path) -> Result<ExtensionId>;
    pub async fn uninstall(&mut self, id: &ExtensionId) -> Result<()>;
    pub async fn enable(&mut self, id: &ExtensionId) -> Result<()>;
    pub async fn disable(&mut self, id: &ExtensionId) -> Result<()>;
    pub async fn bundle(&self, ids: Vec<ExtensionId>) -> Result<Bundle>;
}
```

#### 7.2 CLI Commands

**In `src/commands/ext.rs` (new file):**
```rust
#[derive(Subcommand)]
pub enum ExtCommands {
    /// Install an extension
    Install {
        path: PathBuf,
        #[arg(long)]
        r#type: Option<String>,  // Auto-detect if not specified
    },
    
    /// List installed extensions
    List {
        #[arg(long)]
        enabled_only: bool,
        #[arg(long)]
        r#type: Option<String>,
    },
    
    /// Enable an extension
    Enable { id: String },
    
    /// Disable an extension
    Disable { id: String },
    
    /// Uninstall an extension
    Uninstall { id: String },
    
    /// Create a bundle
    Bundle {
        #[command(subcommand)]
        command: BundleCommands,
    },
}
```

### Phase 8: Migration & Deprecation (Week 8-10)

**Goal:** Migrate existing extensions and deprecate old APIs.

#### 8.1 Auto-Migration on First Run

**In startup sequence:**
```rust
async fn migrate_legacy_extensions() -> Result<()> {
    // Check if migration already done
    if has_migration_flag("extensions-2.0") {
        return Ok(());
    }
    
    // Migrate skills
    for skill in legacy_skills_discovery() {
        ExtensionManager::install(&skill.path).await?;
    }
    
    // Migrate MCP servers
    for mcp in legacy_mcp_discovery() {
        ExtensionManager::install(&mcp.config_path).await?;
    }
    
    // Migrate universal tools
    for tool in legacy_universal_tool_discovery() {
        ExtensionManager::install(&tool.manifest_path).await?;
    }
    
    set_migration_flag("extensions-2.0");
    Ok(())
}
```

#### 8.2 Deprecation Warnings

Add warnings when old APIs are used:

```rust
// src/skills/mod.rs
pub fn build_skills_prompt(skills: &[&Skill]) -> String {
    eprintln!("WARNING: Legacy skills API deprecated. Use Extension Manager instead.");
    // ... implementation
}
```

#### 8.3 Documentation Update

Update all docs to use new `pekobot ext` commands:
- `docs/getting-started/GETTING_STARTED.md`
- `docs/skills/SKILL_DEVELOPMENT.md` (new)
- `docs/extensions/CREATING_ADAPTERS.md` (new)

### Phase 9: Cleanup (Week 10-12)

**Goal:** Remove legacy code paths.

#### 9.1 Remove Legacy Implementations

After deprecation period (e.g., 2 releases):

```bash
# Remove legacy files
rm src/skills/mod.rs          # Now fully in adapters/skill_adapter.rs
rm src/tools/universal/       # Now in adapters/universal_tool_adapter.rs
rm src/mcp/manager.rs         # Now in adapters/mcp_adapter.rs
rm src/channels/cli.rs        # Now in adapters/channel_adapter.rs
rm src/channels/discord.rs    # Now in adapters/channel_adapter.rs
```

#### 9.2 Final Cleanup

- Remove deprecation warnings
- Update all internal code to use Extension Core directly
- Archive migration documentation

## Rollback Plan

If issues arise during migration:

1. **Feature Flag:** All new code behind `extensions-v2` feature flag
2. **Legacy Fallback:** Each adapter has legacy fallback path
3. **Database Migration:** Extension registry stored separately; can be deleted to reset

```rust
// Emergency rollback command
pekobot system reset-extensions --to=legacy
```

## Testing Strategy

### Unit Tests
- Each adapter has comprehensive test suite
- Mock hook handlers for testing

### Integration Tests
```bash
# Test each extension type
cargo test --test extension_skill
cargo test --test extension_mcp
cargo test --test extension_channel
cargo test --test extension_gateway

# Test extension manager
cargo test --test extension_manager

# Test bundling
cargo test --test extension_bundling
```

### E2E Tests
```bash
# Install and use each extension type
./e2e_tests/extensions/test_skill.sh
./e2e_tests/extensions/test_mcp.sh
./e2e_tests/extensions/test_universal_tool.sh
./e2e_tests/extensions/test_bundle.sh
```

## Success Criteria

- [ ] All existing extensions work without modification
- [ ] `pekobot ext list` shows all extension types
- [ ] `pekobot ext install` works for all extension types
- [ ] Bundling works across extension types
- [ ] Custom adapters can be registered
- [ ] Performance: <5ms overhead per hook invocation
- [ ] No regression in existing tests

## Timeline Summary

| Phase | Duration | Deliverable |
|-------|----------|-------------|
| 1 | Week 1-2 | Extension Core foundation |
| 2 | Week 2-3 | Skill adapter |
| 3 | Week 3-4 | Universal tool adapter |
| 4 | Week 4-5 | MCP adapter |
| 5 | Week 5-6 | Channel adapter |
| 6 | Week 6-7 | Hook & gateway adapters |
| 7 | Week 7-8 | Extension manager & CLI |
| 8 | Week 8-10 | Migration & deprecation |
| 9 | Week 10-12 | Legacy cleanup |

**Total: 12 weeks** (3 months) for complete migration.

## Related Documents

- ADR-017: Unified Extension Architecture
- `docs/extensions/ARCHITECTURE.md` (to be created)
- `docs/extensions/CREATING_ADAPTERS.md` (to be created)
