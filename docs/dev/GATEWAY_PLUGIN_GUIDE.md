# Gateway Plugin Development Guide

This guide explains how to build custom gateway plugins for Pekobot, enabling integration with messaging platforms beyond the built-in channels.

## Overview

Pekobot uses a **dynamic plugin system** for gateway (channel) integration. Each gateway is compiled as a separate dynamic library (`.gateway` file) that implements the `GatewayPlugin` trait.

### Architecture

```
┌─────────────────────────────────────────┐
│           Pekobot Core                  │
│      ┌─────────────────────┐            │
│      │   GatewayManager    │            │
│      │  ┌───────────────┐  │            │
│      │  │   Registry    │  │            │
│      │  └───────────────┘  │            │
│      └─────────────────────┘            │
└──────────────────┬──────────────────────┘
                   │ dlopen()
┌──────────────────▼──────────────────────┐
│         Gateway Plugin (.gateway)       │
│  ┌──────────────────────────────────┐   │
│  │  impl GatewayPlugin for Discord  │   │
│  │  - message receiving             │   │
│  │  - message sending               │   │
│  │  - typing indicators             │   │
│  │  - attachments                   │   │
│  └──────────────────────────────────┘   │
└─────────────────────────────────────────┘
```

## Quick Start

### 1. Create a New Gateway Project

```bash
cargo new --lib my-gateway
cd my-gateway
```

### 2. Add Dependencies

```toml
[package]
name = "my-gateway"
version = "0.1.0"
edition = "2021"
crate-type = ["cdylib"]  # Required for dynamic library

[dependencies]
# Provided by Pekobot at runtime
gateway-interface = { path = "../pekobot/gateway-interface" }
async-trait = "0.1"
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
tracing = "0.1"
anyhow = "1"

# Add your platform-specific SDK
# Example: Discord
serenity = "0.12"
```

### 3. Implement the GatewayPlugin Trait

```rust
use gateway_interface::{
    async_trait, GatewayCapabilities, GatewayError, GatewayFactory,
    GatewayMetadata, GatewayPlugin, GatewayResult, IncomingMessage,
    MessageContent, MessageStream, OutgoingMessage, Target,
};
use async_trait::async_trait;
use std::collections::HashMap;
use tokio::sync::mpsc;

/// Your gateway implementation
pub struct MyGateway {
    config: HashMap<String, String>,
}

#[async_trait]
impl GatewayPlugin for MyGateway {
    /// Return gateway metadata
    fn metadata(&self) -> GatewayMetadata {
        GatewayMetadata {
            name: "my-gateway".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            description: "My custom messaging platform integration".to_string(),
            author: Some("Your Name".to_string()),
            homepage: Some("https://example.com".to_string()),
        }
    }

    /// Return supported capabilities
    fn capabilities(&self) -> GatewayCapabilities {
        GatewayCapabilities {
            supports_reactions: true,
            supports_threads: true,
            supports_attachments: true,
            supports_editing: true,
            supports_typing: true,
            supports_read_receipts: false,
            max_message_length: Some(2000),
            max_attachment_size: Some(8 * 1024 * 1024), // 8MB
        }
    }

    /// Initialize the gateway with configuration
    async fn init(&mut self, config: HashMap<String, String>) -> GatewayResult<()> {
        self.config = config;
        // Initialize connection to your platform
        Ok(())
    }

    /// Start receiving messages
    async fn start(&self) -> GatewayResult<MessageStream> {
        let (tx, rx) = mpsc::channel(100);
        
        // Spawn your platform listener
        tokio::spawn(async move {
            // Listen for messages from your platform
            // Convert to IncomingMessage and send via tx
        });
        
        Ok(rx)
    }

    /// Send a message
    async fn send(&self, message: OutgoingMessage) -> GatewayResult<()> {
        // Send message to your platform
        Ok(())
    }

    /// Shutdown the gateway
    async fn shutdown(&self) -> GatewayResult<()> {
        // Clean up connections
        Ok(())
    }
}

/// Factory for creating gateway instances
pub struct MyGatewayFactory;

impl GatewayFactory for MyGatewayFactory {
    fn create(&self) -> Box<dyn GatewayPlugin> {
        Box::new(MyGateway {
            config: HashMap::new(),
        })
    }
}

/// Plugin entry point - exported symbol
#[no_mangle]
pub extern "C" fn gateway_factory() -> Box<dyn GatewayFactory> {
    Box::new(MyGatewayFactory)
}
```

### 4. Build the Plugin

```bash
cargo build --release
```

The output will be:
- Linux: `target/release/libmy_gateway.so` → rename to `my-gateway.gateway`
- macOS: `target/release/libmy_gateway.dylib` → rename to `my-gateway.gateway`
- Windows: `target/release/my_gateway.dll` → rename to `my-gateway.gateway`

### 5. Install the Plugin

```bash
# Copy to Pekobot's gateway directory
cp target/release/libmy_gateway.so ~/.config/pekobot/gateways/my-gateway.gateway

# Or use the CLI
pekobot gateway install ./target/release/libmy_gateway.so --name my-gateway
```

### 6. Configure and Enable

Edit `~/.config/pekobot/gateways.toml`:

```toml
[[gateway]]
name = "my-bot"
plugin = "my-gateway"
enabled = true

[gateway.config]
api_key = "your-api-key"
webhook_url = "https://..."
```

## Message Types

### IncomingMessage

Received from the platform:

```rust
pub struct IncomingMessage {
    /// Unique message ID from the platform
    pub id: MessageId,
    /// Channel/room where message was sent
    pub channel: ChannelId,
    /// User who sent the message
    pub author: User,
    /// Message content
    pub content: MessageContent,
    /// When the message was sent
    pub timestamp: DateTime<Utc>,
    /// Reply to another message
    pub reply_to: Option<MessageId>,
    /// Attached files
    pub attachments: Vec<Attachment>,
}
```

### OutgoingMessage

Sent to the platform:

```rust
pub struct OutgoingMessage {
    /// Target channel/user
    pub target: Target,
    /// Message content
    pub content: MessageContent,
    /// Reply to a specific message
    pub reply_to: Option<MessageId>,
    /// Attachments to include
    pub attachments: Vec<Attachment>,
}
```

## Best Practices

### 1. Error Handling

Use `GatewayError` for all errors:

```rust
use gateway_interface::GatewayError;

async fn connect(&self) -> GatewayResult<()> {
    match self.client.connect().await {
        Ok(_) => Ok(()),
        Err(e) => Err(GatewayError::Connection {
            message: format!("Failed to connect: {}", e),
            retryable: true,
        }),
    }
}
```

### 2. Reconnection Logic

Implement automatic reconnection:

```rust
async fn run_with_retry(&self, tx: mpsc::Sender<IncomingMessage>) {
    let mut backoff = ExponentialBackoff::default();
    
    loop {
        match self.run(tx.clone()).await {
            Ok(_) => break,
            Err(e) => {
                tracing::error!("Gateway error: {}", e);
                tokio::time::sleep(backoff.next()).await;
            }
        }
    }
}
```

### 3. Configuration Validation

Validate config in `init()`:

```rust
async fn init(&mut self, config: HashMap<String, String>) -> GatewayResult<()> {
    let api_key = config.get("api_key")
        .ok_or_else(|| GatewayError::Configuration {
            field: "api_key".to_string(),
            message: "API key is required".to_string(),
        })?;
    
    if api_key.is_empty() {
        return Err(GatewayError::Configuration {
            field: "api_key".to_string(),
            message: "API key cannot be empty".to_string(),
        });
    }
    
    self.config = config;
    Ok(())
}
```

### 4. Security

- Never log API keys or tokens
- Use `secrecy` crate for sensitive data
- Validate all user input before processing

```rust
use secrecy::{ExposeSecret, Secret};

pub struct SecureConfig {
    api_key: Secret<String>,
}
```

## Testing

### Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_gateway_init() {
        let mut gateway = MyGateway::new();
        let mut config = HashMap::new();
        config.insert("api_key".to_string(), "test-key".to_string());
        
        assert!(gateway.init(config).await.is_ok());
    }
}
```

### Integration Tests

Test with a mock Pekobot instance:

```rust
#[tokio::test]
async fn test_message_roundtrip() {
    let (tx, mut rx) = mpsc::channel(10);
    
    // Start your gateway
    let gateway = MyGateway::new();
    let stream = gateway.start().await.unwrap();
    
    // Simulate incoming message
    gateway.inject_test_message("Hello").await;
    
    // Verify message received
    let msg = rx.recv().await.unwrap();
    assert_eq!(msg.content.text, "Hello");
}
```

## Publishing to Pekohub

### 1. Package Your Gateway

```bash
# Create manifest
cat > manifest.toml << 'EOF'
[gateway]
name = "my-gateway"
version = "0.1.0"
description = "My custom gateway"
author = "Your Name"

[binaries]
linux_x64 = "https://pekohub.io/gateways/my-gateway/0.1.0/linux_x64.gateway"
macos_arm64 = "https://pekohub.io/gateways/my-gateway/0.1.0/macos_arm64.gateway"
EOF

# Build for all platforms
./scripts/build-all-platforms.sh

# Sign with your key
pekobot sign --key ~/.pekobot/signing.key my-gateway.gateway
```

### 2. Submit to Registry

```bash
pekobot gateway publish \
  --manifest manifest.toml \
  --binary ./my-gateway.gateway
```

## Troubleshooting

### Plugin Won't Load

```bash
# Check dependencies
ldd my-gateway.gateway

# Verify symbol export
nm -D my-gateway.gateway | grep gateway_factory

# Enable debug logging
RUST_LOG=debug pekobot gateway list
```

### Connection Issues

- Verify config values: `pekobot config get gateway.my-bot`
- Check network connectivity
- Review platform API documentation
- Enable tracing: `RUST_LOG=gateway_interface=trace`

### Memory Leaks

- Ensure all spawned tasks are joined on shutdown
- Use `tokio::select!` with cancellation tokens
- Profile with `valgrind` or `heaptrack`

## Example Gateways

- **Discord**: `gateways/discord/` in Pekobot repo
- **Telegram**: Reference implementation using `teloxide`
- **Slack**: Uses Slack's WebSocket API

## Further Reading

- `gateway-interface/src/lib.rs` - Trait definitions
- `gateways/discord/src/lib.rs` - Complete example
- Pekohub documentation: https://tools.coneko.ai/docs
