# Gateway Plugin System

Pekobot's gateway plugin system allows you to connect to any messaging platform through a unified interface. Gateways are loaded on-demand as dynamic libraries.

## Overview

```
┌─────────────────────────────────────────┐
│         Pekobot Core                    │
│  ┌─────────────────────────────────┐    │
│  │      GatewayManager             │    │
│  │  ┌───────────────────────────┐  │    │
│  │  │   GatewayRegistry         │  │    │
│  │  │   - Load plugins          │  │    │
│  │  │   - Manage instances      │  │    │
│  │  └───────────────────────────┘  │    │
│  └─────────────────────────────────┘    │
└──────────────────┬──────────────────────┘
                   │ loads .gateway files
┌──────────────────▼──────────────────────┐
│         Gateway Plugins                 │
│  ┌──────────┐ ┌──────────┐ ┌─────────┐ │
│  │ discord  │ │ whatsapp │ │  slack  │ │
│  └──────────┘ └──────────┘ └─────────┘ │
└─────────────────────────────────────────┘
```

## Available Gateways

| Gateway | Status | Description |
|---------|--------|-------------|
| Discord | ✅ Available | Full Discord bot support |
| WhatsApp | ⏳ Planned | WhatsApp Business API |
| Telegram | ⏳ Planned | Telegram Bot API |
| Slack | ⏳ Planned | Slack Web API |
| Signal | ⏳ Planned | Signal client |
| IRC | ⏳ Planned | IRC client |
| Google Chat | ⏳ Planned | Google Chat API |

## CLI Usage

### Install a gateway
```bash
pekobot gateway install discord
```

### List installed gateways
```bash
pekobot gateway list
```

### Start a gateway instance
Create a config file `discord.toml`:
```toml
name = "my-bot"
plugin = "discord"
config = { token = "${secret:DISCORD_TOKEN}" }
```

Then start it:
```bash
pekobot gateway start --config discord.toml
```

### Update gateways
```bash
# Update specific gateway
pekobot gateway update discord

# Update all
pekobot gateway update
```

## Configuration

Gateway configuration supports:

- **Rate limiting**: Control message throughput
- **Retry logic**: Automatic reconnection
- **Message filtering**: Drop or forward based on rules
- **Multiple instances**: Run multiple bots on same platform

Example:
```toml
[[gateways]]
name = "main-discord"
plugin = "discord"
enabled = true
config = { token = "${secret:DISCORD_TOKEN}", default_channel = "general" }

[rate_limit]
messages_per_second = 5.0
burst = 10

[retry]
max_retries = 3
base_delay_ms = 1000
```

## Developing Gateways

See [gateways/discord/](gateways/discord/) for a complete example.

### Key Traits

```rust
use gateway_interface::{GatewayPlugin, GatewayFactory};

#[async_trait]
impl GatewayPlugin for MyGateway {
    async fn initialize(&mut self, 
        config: HashMap<String, Value>
    ) -> GatewayResult<()>;
    
    async fn start(&self) -> GatewayResult<MessageStream>;
    
    async fn send(
        &self, 
        target: Target, 
        content: MessageContent
    ) -> GatewayResult<MessageId>;
    
    async fn shutdown(&self) -> GatewayResult<()>;
}
```

### FFI Export

```rust
#[no_mangle]
pub extern "C" fn create_gateway_factory() 
    -> *mut dyn GatewayFactory {
    Box::into_raw(Box::new(MyGatewayFactory))
}
```

## Architecture

### Core Components

- **`gateway-interface`**: Shared types and traits (stable API)
- **`GatewayRegistry`**: Plugin lifecycle management
- **`GatewayManager`**: Active instance coordination
- **`PluginLoader`**: Dynamic library loading

### Loading Process

1. CLI calls `registry.load("discord")`
2. Registry checks local cache
3. If not cached, downloads from Pekohub
4. `PluginLoader` loads the `.gateway` file
5. FFI calls `create_gateway_factory()`
6. Factory creates `GatewayPlugin` instances

### Security

- Plugins are signed and verified by Pekohub
- API version checking prevents incompatible plugins
- File permissions set on download (Unix: 755)
- Sandboxed execution planned (WASM)

## Migration from Built-in Channels

If you were using the old built-in channel system:

1. Install the gateway plugin: `pekobot gateway install discord`
2. Update your config from:
   ```toml
   [channel.discord]
   enabled = true
   token = "..."
   ```
   To:
   ```toml
   [[gateways]]
   name = "discord"
   plugin = "discord"
   config = { token = "..." }
   ```
3. Restart your agent
