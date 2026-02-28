# Gateway Plugin Architecture Refactor Plan

## Overview

Transform Pekobot from a monolithic runtime with built-in channels to a minimal core with pluggable gateway extensions. This mirrors the successful tool extraction and achieves the true "minimal core" vision.

## Goals

1. **Core Size Target**: Reduce from ~2MB to ~500KB-1MB
2. **Zero Built-in Channels**: All 7 channels become optional plugins
3. **Unified Extension System**: Tools, channels, and providers use the same plugin interface
4. **On-Demand Loading**: Only load channels you actually use
5. **Third-Party Extensibility**: Community can build custom gateway plugins

## Current State (Pre-Refactor)

```
Pekobot Core (~2MB)
├── Agent Runtime
├── State Machine
├── Tool Registry
├── Secret Manager
├── Discord Channel (compiled in)
├── WhatsApp Channel (compiled in)
├── Telegram Channel (compiled in)
├── Slack Channel (compiled in)
├── Signal Channel (compiled in)
├── IRC Channel (compiled in)
└── Google Chat Channel (compiled in)
```

## Target Architecture (Post-Refactor)

```
Pekobot Core (~500KB-1MB)
├── Agent Runtime
├── State Machine
├── Tool Registry
├── Secret Manager
├── Unified Gateway Interface
└── Plugin Loader

Gateway Plugins (loaded on demand)
├── discord.gateway (via Pekohub)
├── whatsapp.gateway (via Pekohub)
├── telegram.gateway (via Pekohub)
├── slack.gateway (via Pekohub)
├── signal.gateway (via Pekohub)
├── irc.gateway (via Pekohub)
└── googlechat.gateway (via Pekohub)
```

## Core Components

### 1. Unified Gateway Interface (`src/gateway/interface.rs`)

```rust
/// Trait that all gateway plugins must implement
#[async_trait]
pub trait GatewayPlugin: Send + Sync {
    /// Unique identifier for this gateway (e.g., "discord", "whatsapp")
    fn name(&self) -> &str;
    
    /// Initialize the gateway with configuration
    async fn initialize(&mut self, config: GatewayConfig) -> Result<(), GatewayError>;
    
    /// Start listening for incoming messages
    async fn start(&self) -> Result<MessageStream, GatewayError>;
    
    /// Send a message to a specific channel/user
    async fn send(&self, target: Target, content: MessageContent) -> Result<MessageId, GatewayError>;
    
    /// Get user/channel information
    async fn get_info(&self, entity: EntityRef) -> Result<EntityInfo, GatewayError>;
    
    /// React to a message (if supported)
    async fn react(&self, message_id: MessageId, emoji: &str) -> Result<(), GatewayError>;
    
    /// Shutdown the gateway cleanly
    async fn shutdown(&self) -> Result<(), GatewayError>;
}

/// Factory trait for creating gateway instances
pub trait GatewayFactory: Send + Sync {
    fn create(&self) -> Box<dyn GatewayPlugin>;
    fn metadata(&self) -> GatewayMetadata;
}
```

### 2. Plugin Registry (`src/gateway/registry.rs`)

```rust
/// Registry for loaded gateway plugins
pub struct GatewayRegistry {
    plugins: HashMap<String, Box<dyn GatewayPlugin>>,
    factories: HashMap<String, Box<dyn GatewayFactory>>,
}

impl GatewayRegistry {
    /// Load a gateway plugin from the local cache or Pekohub
    pub async fn load(&mut self, name: &str) -> Result<(), RegistryError> {
        // 1. Check local cache
        // 2. Download from Pekohub if not cached
        // 3. Load the dynamic library
        // 4. Register the factory
    }
    
    /// Get a loaded gateway by name
    pub fn get(&self, name: &str) -> Option<&dyn GatewayPlugin>;
    
    /// List all available gateways (from Pekohub)
    pub async fn list_available(&self) -> Vec<GatewayInfo>;
}
```

### 3. Plugin Manifest Format (`gateway.toml`)

```toml
[plugin]
name = "discord"
type = "gateway"
version = "1.0.0"
description = "Discord gateway for Pekobot"

[capabilities]
provides = ["gateway.discord"]
requires = []

[binary]
linux_x64 = "https://pekohub.io/gateways/discord/1.0.0/linux_x64.so"
macos_arm = "https://pekohub.io/gateways/discord/1.0.0/macos_arm.dylib"
windows_x64 = "https://pekohub.io/gateways/discord/1.0.0/windows_x64.dll"

[config]
required = ["bot_token"]
optional = ["default_channel", "rate_limit_per_user"]
```

### 4. Core Gateway Manager (`src/gateway/manager.rs`)

```rust
/// Manages all gateway connections
pub struct GatewayManager {
    registry: GatewayRegistry,
    active_connections: HashMap<String, ConnectionHandle>,
    event_router: EventRouter,
}

impl GatewayManager {
    /// Initialize gateways from configuration
    pub async fn init_from_config(&mut self, config: &Config) -> Result<()> {
        for gateway_config in &config.gateways {
            // Load plugin if not already loaded
            self.registry.load(&gateway_config.name).await?;
            
            // Initialize and start
            let gateway = self.registry.get(&gateway_config.name).unwrap();
            gateway.initialize(gateway_config.settings.clone()).await?;
            
            // Start listening
            let stream = gateway.start().await?;
            self.spawn_message_handler(gateway_config.name.clone(), stream);
        }
        Ok(())
    }
    
    /// Route incoming messages to the agent system
    fn spawn_message_handler(&self, gateway_name: String, stream: MessageStream) {
        // Spawn async task to handle messages
    }
    
    /// Send message through appropriate gateway
    pub async fn send(&self, gateway: &str, target: Target, content: MessageContent) -> Result<()>;
}
```

## Refactor Phases

### Phase 1: Interface Design (Day 1-2)

1. **Define core traits** (`GatewayPlugin`, `GatewayFactory`)
2. **Design message types** (`IncomingMessage`, `OutgoingMessage`, `Target`, etc.)
3. **Create error types** (`GatewayError`)
4. **Design configuration schema**

**Files to create:**
- `src/gateway/interface.rs` - Core traits
- `src/gateway/types.rs` - Message types
- `src/gateway/error.rs` - Error handling
- `src/gateway/config.rs` - Configuration types

### Phase 2: Extract First Gateway (Discord) (Day 3-4)

1. **Create gateway plugin crate** at `gateways/discord/`
2. **Implement `GatewayPlugin` trait** for Discord
3. **Set up plugin build system** (shared library output)
4. **Test loading dynamically** from core

**Key insight:** Discord is the most complex channel. If this works, others will be easy.

### Phase 3: Registry & Loader (Day 5-6)

1. **Implement `GatewayRegistry`**
2. **Create plugin loader** (dynamic library loading)
3. **Integrate with Pekohub client** for downloading
4. **Add local caching**

**Files to modify:**
- `src/gateway/registry.rs` - New
- `src/gateway/loader.rs` - New
- `src/gateway/mod.rs` - Refactor

### Phase 4: Core Integration (Day 7-8)

1. **Replace built-in channels** with gateway manager
2. **Update agent loop** to use gateway interface
3. **Refactor configuration** to support gateway list
4. **Update CLI** for gateway management commands

**Files to modify:**
- `src/main.rs` - Use GatewayManager
- `src/config.rs` - Gateway list config
- `src/cli.rs` - Gateway subcommands

### Phase 5: Extract Remaining Gateways (Day 9-12)

1. **WhatsApp** → `gateways/whatsapp/`
2. **Telegram** → `gateways/telegram/`
3. **Slack** → `gateways/slack/`
4. **Signal** → `gateways/signal/`
5. **IRC** → `gateways/irc/`
6. **Google Chat** → `gateways/googlechat/`

Each follows the Discord pattern.

### Phase 6: Pekohub Integration (Day 13-14)

1. **Update Pekohub** to support gateway plugins
2. **Add gateway publishing** workflow
3. **Test end-to-end** (download + load + use)
4. **Update documentation**

### Phase 7: Polish & Migration (Day 15-16)

1. **Remove all built-in channel code**
2. **Clean up dependencies** in core Cargo.toml
3. **Update tests** for new architecture
4. **Write migration guide** for users

## Configuration Changes

### Before (Current)

```toml
[channel.discord]
enabled = true
token = "${secret:DISCORD_TOKEN}"
guild_id = "123456789"

[channel.whatsapp]
enabled = true
# ...
```

### After (New)

```toml
[[gateways]]
name = "discord"
plugin = "discord"
config = { token = "${secret:DISCORD_TOKEN}", guild_id = "123456789" }

[[gateways]]
name = "whatsapp"
plugin = "whatsapp"
config = { # ... }
```

## CLI Changes

### New Commands

```bash
# List available gateway plugins
pekobot gateway list

# Install a gateway from Pekohub
pekobot gateway install discord

# Install specific version
pekobot gateway install discord@1.2.0

# Update all gateways
pekobot gateway update

# Remove a gateway
pekobot gateway remove discord

# Test gateway connection
pekobot gateway test discord
```

## Project Structure Changes

### Before

```
projects/pekobot/
├── src/
│   ├── channels/
│   │   ├── discord.rs
│   │   ├── whatsapp.rs
│   │   ├── telegram.rs
│   │   ├── slack.rs
│   │   ├── signal.rs
│   │   ├── irc.rs
│   │   ├── googlechat.rs
│   │   └── mod.rs
│   └── ...
└── Cargo.toml (includes all channel deps)
```

### After

```
projects/pekobot/
├── src/
│   ├── gateway/
│   │   ├── interface.rs      # Core trait
│   │   ├── types.rs          # Message types
│   │   ├── registry.rs       # Plugin registry
│   │   ├── loader.rs         # Dynamic loading
│   │   ├── manager.rs        # Gateway manager
│   │   ├── config.rs         # Configuration
│   │   ├── error.rs          # Error types
│   │   └── mod.rs
│   └── ...
└── Cargo.toml (minimal deps, no channels)

projects/gateway_bundle/      # New repo
├── discord/
│   ├── Cargo.toml
│   └── src/lib.rs
├── whatsapp/
│   ├── Cargo.toml
│   └── src/lib.rs
├── telegram/
├── slack/
├── signal/
├── irc/
├── googlechat/
└── Cargo.toml (workspace)
```

## Dependency Reduction

### Current Core Dependencies (channel-related)

```toml
[dependencies]
# Discord
serenity = { version = "0.12", default-features = false, features = [...] }

# WhatsApp
whatsrs = { git = "..." }

# Telegram
teloxide = { version = "0.13", features = [...] }

# Slack
slack-morphism = "2"

# Signal
libsignal-client = { git = "..." }

# IRC
irc = "1"

# Google Chat
google-chat = { git = "..." }
```

### New Core Dependencies

```toml
[dependencies]
# Only core runtime dependencies
# No channel-specific deps!
libloading = "0.8"  # For dynamic plugin loading
```

**Estimated size reduction:** ~1MB → ~200-300KB for core

## Technical Considerations

### 1. Dynamic Loading Safety

Use `libloading` crate with proper safety wrappers:

```rust
pub struct PluginHandle {
    library: Library,
    factory: Box<dyn GatewayFactory>,
}

impl Drop for PluginHandle {
    fn drop(&mut self) {
        // Ensure clean shutdown before unloading
    }
}
```

### 2. Version Compatibility

Plugins must declare compatible core versions:

```toml
[plugin]
name = "discord"
api_version = "1.0"  # Gateway API version
```

Core checks compatibility before loading.

### 3. Sandboxed Execution (Future)

Consider WASM for plugins instead of native code:

```rust
// Future: WASM-based plugins for true sandboxing
pub enum PluginType {
    Native(Box<dyn GatewayPlugin>),
    Wasm(WasmGateway),  // Future
}
```

### 4. Async Runtime

All gateway methods are async to support different async runtimes:

```rust
#[async_trait]
pub trait GatewayPlugin: Send + Sync {
    async fn start(&self) -> Result<MessageStream, GatewayError>;
}
```

### 5. Error Handling

Unified error type with context:

```rust
#[derive(Error, Debug)]
pub enum GatewayError {
    #[error("Plugin not found: {0}")]
    PluginNotFound(String),
    
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),
    
    #[error("Send failed: {0}")]
    SendFailed(String),
    
    #[error("Plugin error: {0}")]
    Plugin(#[from] PluginError),
}
```

## Migration Path for Users

### Automatic Migration Tool

```bash
# One-time migration
pekobot migrate --from 0.1.0 --to 0.2.0

# This will:
# 1. Save current config
# 2. Extract channel configs
# 3. Install required gateway plugins
# 4. Update config format
# 5. Verify all gateways load
```

### Manual Migration

1. **Backup config** before upgrade
2. **Install required gateways:**
   ```bash
   pekobot gateway install discord
   pekobot gateway install telegram
   # ... etc
   ```
3. **Update config** to new format
4. **Test connections:**
   ```bash
   pekobot gateway test discord
   ```

## Success Metrics

| Metric | Before | After | Target |
|--------|--------|-------|--------|
| Core binary size | ~2MB | ~500KB | <1MB |
| Channel deps in core | 7 | 0 | 0 |
| Startup time | ~50ms | ~20ms | <30ms |
| Memory (no channels) | ~5MB | ~2MB | <3MB |
| Channels loaded | All compiled | On-demand | Configurable |

## Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| Dynamic loading overhead | Medium | Lazy loading + caching |
| Plugin API instability | High | Version gating + compatibility layer |
| Security (downloaded plugins) | High | Signatures + checksums (already in Pekohub) |
| Debugging complexity | Medium | Better logging + plugin introspection |
| Breaking changes for users | Medium | Migration tool + clear documentation |

## Next Steps

1. ✅ **Archive current state** → `feature/builtin-channels-archive`
2. ✅ **Create refactor branch** → `feature/gateway-plugin-architecture`
3. **Review this plan** and approve/refine
4. **Phase 1**: Interface design
5. **Phase 2**: Discord extraction (proof of concept)
6. **Iterate** based on learnings

---

## Phase Completion Notes

### Phase 1: Interface Design ✅ COMPLETE

**Delivered:**
- `src/gateway/types.rs` - Complete message/entity type system
- `src/gateway/error.rs` - Comprehensive error types with context
- `src/gateway/interface.rs` - `GatewayPlugin` trait + `GatewayFactory`
- `src/gateway/config.rs` - Configuration with rate limiting, retry, filters
- `src/gateway/mod.rs` - Module documentation and exports

**Key Design Decisions:**
- Async trait methods for all gateway operations
- Default trait implementations for optional features (returns `NotSupported`)
- Plugin manifest format (`gateway.toml`) matching tool bundle pattern
- API versioning (v1.0.0) for compatibility checking
- Message filtering built into config layer

### Phase 2: Discord Extraction ✅ COMPLETE

**Delivered:**
- `gateway-interface/` - Shared crate separating core from plugins
- `gateways/discord/` - First plugin implementation
  - Implements `GatewayPlugin` trait for Discord
  - FFI exports: `create_gateway_factory()`, `destroy_gateway_factory()`
  - `gateway.toml` manifest for Pekohub integration
  - README with configuration guide

**Architecture Insight:**
The shared `gateway-interface` crate solves the circular dependency problem:
- Core depends on `gateway-interface` (for trait definitions)
- Plugins depend on `gateway-interface` (for trait implementations)
- No direct core↔plugin dependency at compile time
- Dynamic loading bridges them at runtime

**Implementation Pattern:**
```rust
// Plugin implements trait
gateway-interface = "0.1"

pub struct DiscordGateway;
#[async_trait]
impl GatewayPlugin for DiscordGateway { ... }

// FFI export for dynamic loading
#[no_mangle]
pub extern "C" fn create_gateway_factory() 
    -> *mut dyn GatewayFactory { ... }
```

**What's Next:**
- Phase 3: Registry & Loader (dynamic loading from `.gateway` files)
- Phase 4: Core Integration (replace built-in channels)
- Phase 5: Extract remaining 6 gateways (WhatsApp, Telegram, etc.)

### Phase 3: Registry & Loader ✅ COMPLETE

**Delivered:**
- `src/gateway/loader.rs` - Dynamic library loading via `libloading`
  - `PluginHandle` - Manages loaded library lifetime
  - `PluginLoader` - Loads `.so`/`.dylib`/`.dll` files
  - Platform detection (`linux_x64`, `macos_arm`, etc.)
  - FFI symbol resolution (`create_gateway_factory`)
  - API version validation on load

- `src/gateway/registry.rs` - Plugin lifecycle management
  - `GatewayRegistry` - Central registry for plugins
  - Download from Pekohub with platform selection
  - Local caching in `~/.pekobot/gateways/`
  - Version management and updates
  - Instance creation from loaded factories

**Key Capabilities:**
```rust
// Install from Pekohub
registry.load("discord").await?;

// Create instance with config
let instance = registry
    .create_instance("discord", config)
    .await?;

// Start receiving messages
let stream = instance.start().await?;

// Update to latest version
registry.update("discord").await?;
```

**Security Features:**
- API version compatibility checking before load
- Platform-specific binary selection
- File permissions set on download (Unix: 755)
- Pekohub signature verification (placeholder for integration)

**Cache Structure:**
```
~/.pekobot/gateways/
├── discord_0.1.0_linux_x64.gateway.so
├── whatsapp_0.1.0_linux_x64.gateway.so
└── manifests/
    ├── discord.toml
    └── whatsapp.toml
```

**What's Next:**
- Phase 4: Core Integration (replace built-in `channels/` module)
- Phase 5: Extract remaining 6 gateways
- Phase 6: Pekohub gateway publishing endpoint
- Phase 7: Migration tool and polish

---

*Drafted: 2026-02-23*
*Updated: 2026-02-23*
*Branch: feature/gateway-plugin-architecture*


### Phase 4: Core Integration ✅ COMPLETE

**Delivered:**
- `src/gateway/manager.rs` - GatewayManager for coordinating gateway instances
  - Manages multiple concurrent gateway connections
  - Routes messages between agent system and gateways
  - Event-driven architecture (connect/disconnect/message/error)
  - `adapter` module for backward compatibility with old `Channel` trait

- `src/main.rs` - CLI commands for gateway management
  - `gateway list` - Show installed plugins
  - `gateway available` - List from Pekohub
  - `gateway install <name>` - Download and install
  - `gateway uninstall <name>` - Remove plugin
  - `gateway update [name]` - Update to latest
  - `gateway info <name>` - Show plugin details
  - `gateway start --config <file>` - Start instance
  - `gateway stop <instance>` - Stop instance
  - `gateway instances` - List active instances
  - `gateway test <instance>` - Test connection

**Integration Points:**
```rust
// Initialize manager
let manager = GatewayManager::new(config).await?;

// Load from config
manager.init_from_config(&config).await?;

// Send message
manager.send_text(
    "discord-1",
    Target::Channel(ChannelId("general".to_string())),
    "Hello!"
).await?;

// Receive events
while let Some(event) = manager.next_event().await {
    match event {
        GatewayEvent::Message { message, .. } => {
            // Process incoming message
        }
        // ...
    }
}
```

**Architecture:**
- CLI channel remains built-in (local terminal interface, not a messaging gateway)
- Discord, Slack, WhatsApp, etc. moved to plugin system
- GatewayManager handles multiple concurrent connections
- Each gateway instance runs in its own async task

**What's Next:**
- Phase 5: Extract remaining 6 gateways (WhatsApp, Telegram, Slack, Signal, IRC, Google Chat)
- Phase 6: Pekohub publishing endpoint for gateway plugins
- Phase 7: Migration tool and documentation

---

*Drafted: 2026-02-23*
*Updated: 2026-02-23*
*Branch: feature/gateway-plugin-architecture*