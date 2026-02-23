# Discord Gateway Plugin for Pekobot

This plugin enables Pekobot to connect to Discord servers.

## Installation

```bash
pekobot gateway install discord
```

## Configuration

Add to your `pekobot.toml`:

```toml
[[gateways]]
name = "my-discord-bot"
plugin = "discord"
config = { token = "${secret:DISCORD_TOKEN}" }
```

### Getting a Discord Token

1. Go to [Discord Developer Portal](https://discord.com/developers/applications)
2. Create a new application
3. Go to "Bot" section and add a bot
4. Copy the bot token
5. Add token to Pekobot: `pekobot secret set DISCORD_TOKEN your_token_here`

### Required Permissions

Your bot needs these OAuth2 scopes:
- `bot`
- `applications.commands` (optional, for slash commands)

And these bot permissions:
- Send Messages
- Read Message History
- Add Reactions
- Use Slash Commands (optional)

## Capabilities

| Feature | Supported |
|---------|-----------|
| Send messages | ✅ |
| Receive messages | ✅ |
| Direct messages | ✅ |
| Threads | ✅ |
| Reactions | ✅ |
| Message editing | ✅ |
| Message deletion | ✅ |
| Typing indicators | ✅ |
| Embeds | ✅ |
| Attachments | ✅ |
| Voice | ❌ (not yet) |

## Usage

Once configured, your agents can send messages to Discord:

```rust
// Agent sends a message
agent.use_tool("send_message", json!({
    "gateway": "my-discord-bot",
    "channel": "general",
    "content": "Hello from Pekobot!"
}));
```

## Troubleshooting

### "Invalid token format"
Your token should look like: `MTAxMD...` (a long base64 string)

### "Connection failed"
- Check your internet connection
- Verify the token is correct
- Check Discord status at https://status.discord.com

## Development

Build the plugin:

```bash
cd gateways/discord
cargo build --release
```

The compiled plugin will be at:
- Linux: `target/release/libdiscord_gateway.so`
- macOS: `target/release/libdiscord_gateway.dylib`
- Windows: `target/release/discord_gateway.dll`
