# Gateway Extension System

Pekobot's gateway system allows you to connect to messaging platforms through the Unified Extension Architecture. Gateways are implemented as extensions that hook into the I/O lifecycle.

## Overview

```
┌─────────────────────────────────────────┐
│         Pekobot Core                    │
│  ┌─────────────────────────────────┐    │
│  │      ExtensionCore              │    │
│  │  ┌───────────────────────────┐  │    │
│  │  │   Gateway Adapter         │  │    │
│  │  │   - ChannelInput hook     │  │    │
│  │  │   - ChannelOutput hook    │  │    │
│  │  │   - EventEmit hook        │  │    │
│  │  └───────────────────────────┘  │    │
│  └─────────────────────────────────┘    │
└──────────────────┬──────────────────────┘
                   │ extension.yaml
┌──────────────────▼──────────────────────┐
│         Gateway Extension               │
│  ┌──────────┐ ┌──────────┐ ┌─────────┐ │
│  │ discord  │ │ whatsapp │ │  slack  │ │
│  └──────────┘ └──────────┘ └─────────┘ │
└─────────────────────────────────────────┘
```

## Available Gateways

| Gateway | Status | Description |
|---------|--------|-------------|
| Discord | ⏳ Planned | Discord bot support via extension |
| WhatsApp | ⏳ Planned | WhatsApp Business API |
| Telegram | ⏳ Planned | Telegram Bot API |
| Slack | ⏳ Planned | Slack Web API |
| Signal | ⏳ Planned | Signal client |
| IRC | ⏳ Planned | IRC client |

## CLI Usage

Gateways are managed through the unified extension system:

### Install a gateway
```bash
pekobot ext install ./discord-gateway
```

### List extensions (including gateways)
```bash
pekobot ext list --type gateway
```

### Enable a gateway
```bash
pekobot ext enable discord
```

### Show gateway details
```bash
pekobot ext info discord
```

## Configuration

Gateway extensions use `GATEWAY.toml` or `extension.yaml` manifests:

```yaml
---
id: "discord"
name: "Discord Gateway"
version: "1.0.0"
extension_type: "gateway"

hooks:
  - point: "channel.input"
    handler: "handle_discord_message"
  - point: "channel.output"
    handler: "send_discord_message"
  - point: "event.emit"
    handler: "handle_system_event"
```

## Developing Gateways

See [Gateway Plugin Development Guide](../dev/GATEWAY_PLUGIN_GUIDE.md) for details on building custom gateway extensions.

### Key Hook Points

Gateway extensions typically use these hook points:

| Hook Point | Purpose |
|------------|---------|
| `ChannelInput` | Process incoming messages from the platform |
| `ChannelOutput` | Format and send outgoing messages |
| `EventEmit` | React to system events (agent ready, error, etc.) |

### Example Extension Structure

```
discord-gateway/
├── GATEWAY.toml          # Extension manifest
├── src/
│   └── main.rs           # Gateway implementation
└── README.md
```

## Architecture

### Core Components

- **ExtensionCore**: Central hook registry (22 hook points)
- **Gateway Adapter**: Maps gateway extensions to I/O hooks
- **ExtensionManager**: Lifecycle management (install, enable, disable)

### Loading Process

1. `pekobot ext install ./discord-gateway` — copies extension to extensions dir
2. `pekobot ext enable discord` — registers hooks with ExtensionCore
3. Daemon loads the extension and invokes hooks during agent execution

### Security

- Extensions are loaded from user-controlled directories
- No dynamic library loading — extensions run in the daemon process
- API keys stored via environment variables or config files

## See Also

- [Extension System](../architecture/EXTENSION_SYSTEM.md) — Unified Extension Architecture
- [Gateway Plugin Development Guide](../dev/GATEWAY_PLUGIN_GUIDE.md) — Building custom gateways
- [Extension ADRs](../architecture/adr/) — ADR-025 (Gateway Extension), ADR-026 (Extension Lifecycle)
