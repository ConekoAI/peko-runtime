# Pekobot Documentation

Complete documentation for the Pekobot multi-agent runtime.

## Documentation Structure

### [Getting Started](./getting-started/)
- [Getting Started](./getting-started/GETTING_STARTED.md) - Installation and initial setup
- [Building Your First Agent](./getting-started/TUTORIAL_BUILDING_FIRST_AGENT.md) - Step-by-step tutorial

### [User Guide](./user-guide/)
- [User's Guide](./user-guide/USERS_GUIDE.md) - Comprehensive usage guide
- [CLI Reference](./user-guide/CLI_REFERENCE.md) - Command-line interface documentation

### [Deployment](./deployment/)
- [VPS Deployment](./deployment/VPS_DEPLOYMENT.md) - Deploy to cloud servers
- [Gateways](./deployment/GATEWAYS.md) - Messaging platform integration

### [Reference](./reference/)
- [Cron System](./reference/cron.md) - Scheduled task execution
- [Daemon Mode](./reference/daemon.md) - Long-running execution engine
- [Security Model](./reference/SECURITY_MODEL.md) - Security architecture

### [Development](./dev/)
- [Architecture](./dev/ARCHITECTURE.md) - System architecture overview
- [Streaming](./dev/STREAMING.md) - Real-time response streaming
- [Registry Config](./dev/REGISTRY_CONFIG.md) - Tool registry configuration
- [Tool Monitoring](./dev/TOOL_MONITORING.md) - Tool execution monitoring
- [Gateway Plugin Guide](./dev/GATEWAY_PLUGIN_GUIDE.md) - Building gateway plugins
- [OpenClaw Comparison](./dev/OPENCLAW_COMPARISON.md) - Comparison with OpenClaw

## Quick Start

```bash
# Install Pekobot
curl -sSL https://raw.githubusercontent.com/yourusername/pekobot/main/install.sh | bash

# Create your first agent
pekobot agent create my-agent --provider kimi

# Start the agent
pekobot agent start my-agent
```

## Main Documentation

For the main project README with quick start, features, and overview, see [../README.md](../README.md).

## Archive

Historical documents and deprecated plans are stored in [./archive/](./archive/).
