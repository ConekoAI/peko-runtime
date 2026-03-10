# GAP-010: Channel Plugin Architecture

**Priority:** 🟡 Medium  
**Status:** Open  
**Target:** v0.8.0  
**Est. Effort:** 1-2 weeks  

---

## Problem Statement

The Grand Architecture specifies channels as registry-downloadable plugins. Currently:
- All channels are built into core (`cli`, `discord`, `telegram`, `slack`, `matrix`, `whatsapp`)
- No dynamic channel loading
- Core bloat with channel-specific dependencies

---

## Current State

```rust
// src/channels/mod.rs
pub mod cli;
pub mod discord;
pub mod http;
pub mod matrix;
pub mod slack;
pub mod telegram;
pub mod whatsapp;

// All channels compiled into core binary
pub use cli::CliChannel;
pub use discord::{DiscordChannel, DiscordConfig};
// ... etc
```

Cargo.toml includes all channel dependencies:
```toml
[dependencies]
serenity = { version = "0.12", optional = true }  # Discord
teloxide = { version = "0.13", optional = true }  # Telegram
# ... etc
```

---

## Target State

Per [GRAND_ARCHITECTURE.md section 4.2](../GRAND_ARCHITECTURE.md#42-communication-layer):

| Type | Examples | Source |
|------|----------|--------|
| Built-in | CLI, HTTP | Core |
| Registry | Discord, WhatsApp | Pekohub |
| Custom | TUI, Game mods | User-built |

```rust
// Load channel from registry
let channel = registry.load_channel("discord").await?;
channel.initialize(config).await?;
```

---

## Scope

### In Scope
- Channel plugin interface/contract
- Dynamic channel loading
- Registry-based channel distribution
- Move existing channels to plugins (Discord, Telegram, etc.)

### Out of Scope (Future)
- Channel marketplace UI
- Channel certification/verification
- Hot-reloading of channels

---

## Goals

1. **Plugin Interface**: Define channel plugin contract
2. **Dynamic Loading**: Load channels at runtime
3. **Registry Distribution**: Download channels from Pekohub
4. **Core Slimming**: Move channels out of core binary
5. **User Channels**: Support custom user-built channels

---

## Proposed Implementation

### Channel Plugin Interface
```rust
// src/channels/plugin.rs
#[async_trait]
pub trait ChannelPlugin: Send + Sync {
    /// Plugin metadata
    fn metadata(&self) -> ChannelMetadata;

    /// Initialize with configuration
    async fn initialize(&mut self, config: ChannelConfig) -> Result<()>;

    /// Start the channel
    async fn start(&self) -> Result<ChannelStream>;

    /// Send a message
    async fn send(&self, target: &str, message: &str) -> Result<()>;

    /// Check health
    fn health(&self) -> ChannelHealth;

    /// Shutdown
    async fn shutdown(&self) -> Result<()>;
}

pub struct ChannelMetadata {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub dependencies: Vec<String>,
}

pub type ChannelStream = mpsc::Receiver<ChannelMessage>;

pub struct ChannelMessage {
    pub sender: Peer,
    pub content: String,
    pub channel_id: String,
    pub timestamp: DateTime<Utc>,
}
```

### Plugin Loading
```rust
// src/channels/loader.rs
pub struct ChannelLoader {
    plugins_dir: PathBuf,
    loaded: HashMap<String, Box<dyn ChannelPlugin>>,
}

impl ChannelLoader {
    /// Load plugin from WASM or native library
    pub async fn load(&mut self, name: &str) -> Result<&dyn ChannelPlugin> {
        let plugin_path = self.plugins_dir.join(format!("{}", name));

        // Try WASM first
        if let Ok(wasm) = self.load_wasm(&plugin_path.with_extension("wasm")).await {
            self.loaded.insert(name.to_string(), wasm);
            return Ok(self.loaded.get(name).unwrap().as_ref());
        }

        // Fall back to native dynamic library
        let native = self.load_native(&plugin_path.with_extension("so")).await?;
        self.loaded.insert(name.to_string(), native);
        Ok(self.loaded.get(name).unwrap().as_ref())
    }

    async fn load_wasm(&self, path: &Path) -> Result<Box<dyn ChannelPlugin>> {
        // Use wasmtime or similar
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::from_file(&engine, path)?;
        // ... instantiate and return
    }
}
```

### Registry Integration
```rust
// src/channels/registry.rs
pub struct ChannelRegistry {
    client: PekohubClient,
    cache_dir: PathBuf,
}

impl ChannelRegistry {
    /// Download and install channel from registry
    pub async fn install(&self, name: &str) -> Result<PathBuf> {
        let manifest = self.client.get_channel_manifest(name).await?;

        // Download binary/WASM
        let download_url = manifest.download_url_for_current_platform();
        let bytes = reqwest::get(download_url).await?.bytes().await?;

        // Verify signature
        self.verify(&bytes, &manifest.signature)?;

        // Install to cache
        let install_path = self.cache_dir.join(name);
        tokio::fs::write(&install_path, bytes).await?;

        Ok(install_path)
    }

    /// List available channels
    pub async fn list_available(&self) -> Result<Vec<ChannelManifest>> {
        self.client.list_channels().await
    }
}
```

### Channel Manifest
```toml
# channels/discord/CHANNEL.toml
[channel]
name = "discord"
version = "1.0.0"
description = "Discord bot integration"
author = "Pekobot Team"

[binaries]
linux_x64 = "https://pekohub.io/channels/discord/1.0.0/linux_x64.wasm"
macos_arm64 = "https://pekohub.io/channels/discord/1.0.0/macos_arm64.wasm"

[config]
required = ["bot_token"]
optional = ["gateway_intents", "allowed_guilds"]
```

### Migration Strategy

#### Phase 1: Interface Definition
- Define `ChannelPlugin` trait
- Refactor existing channels to implement trait
- Keep built-in for now

#### Phase 2: Plugin Compilation
- Compile channels as WASM or dynamic libraries
- Test loading mechanism
- Keep built-in as fallback

#### Phase 3: Externalization
- Move channels to separate repos
- Publish to registry
- Remove from core (feature flag)

#### Phase 4: Cleanup
- Remove built-in implementations
- Core only has CLI and HTTP
- All other channels via registry

---

## Dependencies

- **Related to:** GAP-001 (MCP follows similar pattern)
- **Related to:** GAP-008 (Tool migration similar process)
- **Uses:** GAP-009 (Channel overlays for plugin state)

---

## Success Criteria

- [ ] Can load channel plugin dynamically
- [ ] Can install channel from registry
- [ ] Discord/Telegram work as plugins
- [ ] Core binary size reduced
- [ ] Can develop custom channel without core changes
- [ ] Channel isolation (crash doesn't bring down core)

---

## References

- [GRAND_ARCHITECTURE.md - Communication Layer](../GRAND_ARCHITECTURE.md#42-communication-layer)
- [GRAND_ARCHITECTURE.md - Channel Trait](../GRAND_ARCHITECTURE.md#42-communication-layer)
- Current channels: `src/channels/`
- Gateway interface: `gateway-interface/src/interface.rs`
