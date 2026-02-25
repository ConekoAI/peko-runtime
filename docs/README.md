# Pekobot Documentation

Complete documentation for the Pekobot multi-agent runtime.

## Getting Started

- [README.md](../README.md) - Quick start and overview
- [GETTING_STARTED.md](./GETTING_STARTED.md) - Detailed setup guide (if exists)

## Core Features

### Cron System
- [cron.md](./cron.md) - Scheduled task execution
  - Schedule types (cron, every, at)
  - Execution modes (main, isolated)
  - CLI commands
  - Database schema
  - Best practices

### Daemon Mode
- [daemon.md](./daemon.md) - Long-running execution engine
  - Architecture
  - CLI commands
  - Signal handling
  - Systemd integration
  - Troubleshooting

### Agent Management
- [agents.md](./agents.md) - Creating and managing agents (if exists)
- [portable-agents.md](./portable-agents.md) - Export/import (if exists)

### Tools
- [tools.md](./tools.md) - Tool system overview (if exists)
- [tool-registry.md](./tool-registry.md) - Pekohub integration (if exists)

### Identity & Security
- [identity.md](./identity.md) - DID identity system (if exists)
- [secrets.md](./secrets.md) - Secret management (if exists)

## Protocols

- [A2A Protocol](./A2A_v0.1.md) - Agent-to-Agent messaging

## Development

- [API.md](./API.md) - Internal API documentation
- [ISSUES.md](./ISSUES.md) - Known issues and workarounds
- [ARCHITECTURE.md](./ARCHITECTURE.md) - System architecture (if exists)

## CLI Reference

### Command Structure

Pekobot uses a hierarchical command structure:

```
pekobot <noun> <verb> [options]
```

**Agent commands:**
```bash
pekobot agent start       # Start an agent
pekobot agent list        # List all agents
pekobot agent create      # Create new agent
pekobot agent delete      # Delete an agent
pekobot agent export      # Export to .agent package
pekobot agent import      # Import from .agent package
pekobot agent inspect     # Inspect package without importing
```

**Tool commands:**
```bash
pekobot tool list         # List installed tools
pekobot tool search       # Search for tools
pekobot tool install      # Install a tool
pekobot tool uninstall    # Remove a tool
pekobot tool info         # Show tool details
```

**Cron commands:**
```bash
pekobot cron list         # List cron jobs
pekobot cron add          # Add cron job
pekobot cron at           # One-shot job
pekobot cron every        # Interval job
pekobot cron remove       # Delete job
pekobot cron run          # Manual trigger
pekobot cron history      # View history
```

**Daemon commands:**
```bash
pekobot daemon start      # Start daemon
pekobot daemon stop       # Stop daemon
pekobot daemon status     # Check status
pekobot daemon restart    # Restart daemon
pekobot daemon check      # Trigger cron check
```

**Session commands:**
```bash
pekobot session list      # List sessions
pekobot session show      # Show session details
pekobot session send      # Send message to session
pekobot session kill      # Terminate session
```

**Config commands:**
```bash
pekobot config validate   # Validate config
pekobot config init       # Create default config
pekobot config defaults   # Show defaults
pekobot config path       # Show config path
pekobot config get        # Get value
pekobot config set        # Set value
```

**System commands:**
```bash
pekobot system status     # Show status
pekobot system info       # System information
pekobot system doctor     # Health check
pekobot system clean      # Clean cache
pekobot system update     # Update Pekobot
```

**Gateway commands:**
```bash
pekobot gateway list      # List gateways
pekobot gateway search    # Search registries
pekobot gateway install   # Install gateway
pekobot gateway info      # Gateway details
```

**Shell completions:**
```bash
pekobot completions bash      # Generate bash completions
pekobot completions zsh       # Generate zsh completions
pekobot completions fish      # Generate fish completions
pekobot completions powershell # Generate PowerShell completions
pekobot completions elvish    # Generate elvish completions
```

## Global Flags

Available on all commands:

```bash
--config-dir PATH    # Configuration directory
--data-dir PATH      # Data directory
--cache-dir PATH     # Cache directory
--json               # JSON output
--quiet, -q          # Suppress output
--verbose, -v        # Verbose output
--help, -h           # Show help
--version, -V        # Show version
```

## Environment Variables

```bash
PEKOBOT_CONFIG_DIR   # Config directory
PEKOBOT_DATA_DIR     # Data directory
PEKOBOT_CACHE_DIR    # Cache directory
PEKOBOT_LOG          # Log level (error, warn, info, debug, trace)
RUST_LOG             # Rust tracing log level
```

## Quick Reference

### Start an agent
```bash
pekobot agent start --name my-agent
```

### Create a cron job
```bash
pekobot cron add \
  --name "daily" \
  --schedule "0 9 * * *" \
  --message "Good morning!"
```

### Start the daemon
```bash
pekobot daemon start --foreground
```

### Export an agent
```bash
pekobot agent export --name my-agent --output backup.agent
```

### Install a tool
```bash
pekobot tool install calendar
```

## Contributing

See the main [README.md](../README.md) for development setup.

## License

MIT License - See [LICENSE](../LICENSE) for details.
