# MCP Migration Guide

## Overview

Pekobot supports the Model Context Protocol (MCP) for external tool integration. MCP servers are managed through the Unified Extension Architecture (ADR-017) as extensions.

> **Note:** MCP servers are not built into Pekobot core. They are provided as external extensions that can be installed and enabled through the extension system.

## What Changed

### Before (Legacy)

Some tools were built into Pekobot core:

```
pekobot (single binary)
├── filesystem
├── shell
├── cron
└── session
```

### After (Current)

Core tools remain minimal; extended capabilities come via MCP extensions:

```
pekobot (core binary)
├── filesystem (built-in)
├── shell (built-in)
├── cron (built-in)
└── session (built-in)

MCP Extensions (installed separately)
├── filesystem-mcp
├── browser-mcp
├── sqlite-mcp
└── community MCP servers
```

## Migration Steps

### 1. Install MCP Server Extensions

MCP servers are packaged as extensions:

```bash
# Install an MCP server extension
pekobot ext install ./mcp-filesystem-extension

# Or create your own with an extension.yaml
```

### 2. Verify Installation

```bash
# List installed extensions
pekobot ext list

# Show MCP extension details
pekobot ext info filesystem-mcp
```

### 3. Enable the Extension

```bash
pekobot ext enable filesystem-mcp
```

### 4. Use with Agent

MCP tools are automatically available once the extension is enabled:

```bash
pekobot send myagent "Read the file README.md"
```

## Configuration

### Extension Manifest

MCP extensions use `extension.yaml`:

```yaml
---
id: "filesystem-mcp"
name: "Filesystem MCP Server"
version: "1.0.0"
extension_type: "mcp"

hooks:
  - point: "tool.register"
    handler: "register_mcp_tools"
  - point: "tool.execute"
    handler: "execute_mcp_tool"
  - point: "agent.init"
    handler: "init_mcp_client"
  - point: "agent.shutdown"
    handler: "shutdown_mcp_client"
```

### MCP Server Config

The extension adapter reads MCP server configuration:

```json
{
  "mcpServers": {
    "filesystem": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/home/user/docs"]
    }
  }
}
```

## CLI Commands

MCP servers are managed through the extension system:

```bash
# Install an MCP extension
pekobot ext install ./my-mcp-extension

# Enable it
pekobot ext enable my-mcp-extension

# Check status
pekobot ext info my-mcp-extension

# Debug
pekobot ext debug my-mcp-extension

# Disable
pekobot ext disable my-mcp-extension

# Uninstall
pekobot ext uninstall my-mcp-extension
```

## Troubleshooting

### "Extension not found"

**Problem**: MCP extension is not installed.

**Solution**:
```bash
pekobot ext list
pekobot ext install ./my-mcp-extension
```

### "No MCP tools loaded"

**Problem**: Extension is installed but not enabled.

**Solution**:
```bash
pekobot ext enable my-mcp-extension
pekobot ext info my-mcp-extension
```

### "Failed to initialize MCP client"

**Problem**: MCP server binary is missing or crashing.

**Solution**:
1. Check the server binary works directly:
   ```bash
   npx -y @modelcontextprotocol/server-filesystem --help
   ```
2. Check extension logs via daemon foreground mode
3. Validate extension manifest:
   ```bash
   pekobot ext validate ./my-mcp-extension
   ```

### Backward Compatibility

**Problem**: Existing agents expect certain tools.

**Solution**: Built-in tools (filesystem, shell, cron, session) are always available. MCP tools are additive.

## FAQ

### Q: Do I have to use MCP servers?

**A**: No. Core built-in tools work without MCP. MCP extensions are optional for extended functionality.

### Q: Can I mix MCP and built-in tools?

**A**: Yes. Built-in tools are always available. MCP tools are loaded through extensions and add to the tool set.

### Q: What if an MCP server crashes?

**A**: The extension adapter handles errors gracefully. The agent continues with remaining tools.

### Q: Can I add custom MCP servers?

**A**: Yes! Package them as extensions with `extension.yaml` and install via `pekobot ext install`.

### Q: Where are MCP binaries stored?

**A**: MCP binaries are managed by the extension. They may be installed via npm, cargo, etc. and referenced in the extension manifest.

## Performance Impact

| Metric | Built-in Only | With MCP Extensions |
|--------|---------------|---------------------|
| Core binary size | ~5-8MB | Same |
| Startup time | ~50ms | ~50ms + extension init |
| Tool latency | 1-10ms | 5-50ms (cross-process) |

## Benefits of MCP Architecture

1. **Modularity**: Install only tools you need
2. **Isolation**: MCP crashes don't affect core
3. **Updates**: Update servers independently
4. **Language**: Servers can be written in any language
5. **Ecosystem**: Compatible with community MCP servers

## See Also

- [MCP Specification](https://modelcontextprotocol.io/)
- [MCP Overview](MCP.md)
- [Extension System](../architecture/EXTENSION_SYSTEM.md)
- `pekobot ext --help`
