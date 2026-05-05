# MCP Quick Start Guide

Get started with MCP servers in 5 minutes.

## Prerequisites

- Pekobot built from source
- MCP server binaries or extension packages

## 1. Install MCP Servers

MCP servers are installed as extensions via the unified extension system:

```bash
# Install an MCP server extension
pekobot ext install ./mcp-filesystem-server

# Or from a remote source
pekobot ext install ./mcp-web-server
```

## 2. Verify Installation

```bash
# List installed extensions
pekobot ext list

# Show MCP extension details
pekobot ext info filesystem-mcp
```

You should see the extension listed with type `mcp`.

## 3. Enable the Extension

```bash
# Enable the MCP extension
pekobot ext enable filesystem-mcp
```

## 4. Use with Agent

MCP tools are automatically available to agents once the extension is enabled:

```bash
pekobot send myagent "Read the file README.md"
```

## Common Tasks

### List Available Extensions

```bash
pekobot ext list --type mcp
```

### Test Extension Health

```bash
# Check extension status
pekobot ext info filesystem-mcp

# Debug extension
pekobot ext debug filesystem-mcp
```

### View Extension Configuration

```bash
# Show config
pekobot ext config filesystem-mcp --show

# Set config value
pekobot ext config filesystem-mcp --set timeout=30
```

### Reinstall an Extension

```bash
# Uninstall and reinstall
pekobot ext uninstall filesystem-mcp
pekobot ext install ./mcp-filesystem-server
```

## Troubleshooting

### Extension Not Found

```bash
# Check if extension is installed
pekobot ext list

# Reinstall if missing
pekobot ext install ./mcp-filesystem-server
```

### Connection Failed

```bash
# Check extension status
pekobot ext info filesystem-mcp

# Check daemon logs
pekobot daemon start --foreground
```

### Tools Not Appearing

```bash
# Ensure extension is enabled
pekobot ext enable filesystem-mcp

# Validate extension manifest
pekobot ext validate ./mcp-filesystem-server
```

## Next Steps

- Read the [MCP Overview](MCP.md)
- Read the [Migration Guide](MIGRATION_GUIDE.md)
- Learn about the [Extension System](../architecture/EXTENSION_SYSTEM.md)
