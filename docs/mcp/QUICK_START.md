# MCP Quick Start Guide

Get started with MCP servers in 5 minutes.

## Prerequisites

- Peko built from source
- MCP server binaries or extension packages

## 1. Install MCP Servers

MCP servers are installed as extensions via the unified extension system:

```bash
# Install an MCP server extension
peko ext install ./mcp-filesystem-server

# Or from a remote source
peko ext install ./mcp-web-server
```

## 2. Verify Installation

```bash
# List installed extensions
peko ext list

# Show MCP extension details
peko ext info filesystem-mcp
```

You should see the extension listed with type `mcp`.

## 3. Grant the Capability

```bash
# Grant the MCP capability to a Principal
peko capability grant --principal <principal-name> filesystem-mcp
```

## 4. Use with Agent

MCP tools are automatically available to a Principal once the capability is granted:

```bash
peko send <principal-name> "Read the file README.md"
```

## Common Tasks

### List Available Extensions

```bash
peko ext list --type mcp
```

### Test Extension Health

```bash
# Check extension status
peko ext info filesystem-mcp

# Debug extension
peko ext debug filesystem-mcp
```

### View Extension Configuration

```bash
# Show config
peko ext config filesystem-mcp --show

# Set config value
peko ext config filesystem-mcp --set timeout=30
```

### Reinstall an Extension

```bash
# Uninstall and reinstall
peko ext uninstall filesystem-mcp
peko ext install ./mcp-filesystem-server
```

## Troubleshooting

### Extension Not Found

```bash
# Check if extension is installed
peko ext list

# Reinstall if missing
peko ext install ./mcp-filesystem-server
```

### Connection Failed

```bash
# Check extension status
peko ext info filesystem-mcp

# Check daemon logs
peko daemon start --foreground
```

### Tools Not Appearing

```bash
# Ensure the capability is granted to the Principal
peko capability grant --principal <principal-name> filesystem-mcp

# Validate extension manifest
peko ext validate ./mcp-filesystem-server
```

## Next Steps

- Read the [MCP Overview](MCP.md)
- Read the [Migration Guide](MIGRATION_GUIDE.md)
- Learn about the [Extension System](../architecture/EXTENSION_SYSTEM.md)
