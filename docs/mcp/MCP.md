# MCP (Model Context Protocol) Support

Pekobot supports the [Model Context Protocol (MCP)](https://modelcontextprotocol.io/), allowing you to extend agent capabilities with external tools from MCP servers.

## Overview

MCP is an open protocol that standardizes how applications provide context to LLMs. With MCP support, Pekobot can:

- Connect to MCP servers via stdio (local) or SSE (remote)
- Discover and invoke tools from MCP servers
- Use MCP resources and prompts
- Manage multiple MCP servers through the Unified Extension Architecture

## Quick Start

### 1. Install an MCP Server

MCP servers are managed as extensions. For example, to use an MCP filesystem server:

```bash
# Install the MCP server as an extension
pekobot ext install <mcp-extension-path-or-url>
```

### 2. Start the MCP Server

```bash
# Start the MCP server runtime
pekobot ext start <mcp-extension-name>
```

### 3. Verify It's Working

```bash
# Check extension status
pekobot ext status <mcp-extension-name>

# List installed extensions
pekobot ext list
```

## Managing MCP via Extensions

Pekobot manages MCP servers through the Unified Extension Architecture (`pekobot ext`):

### Install an MCP Extension

```bash
pekobot ext install <path-or-url>
```

### List Extensions

```bash
pekobot ext list
```

### Enable/Disable Capabilities

```bash
pekobot ext enable <capability>
pekobot ext disable <capability>
```

### Start/Stop MCP Runtimes

```bash
# Start a background runtime for the extension
pekobot ext start <mcp-extension-name>

# Stop the runtime
pekobot ext stop <mcp-extension-name>

# Restart the runtime
pekobot ext restart <mcp-extension-name>

# Check runtime status
pekobot ext status <mcp-extension-name>
```

### Show Extension Info

```bash
pekobot ext info <mcp-extension-name>
```

### Debug an Extension

```bash
pekobot ext debug <mcp-extension-name>
```

## Configuration

Extension settings can be configured at global, team, or agent level:

```bash
# Configure extension settings
pekobot ext config <mcp-extension-name>
```

## Using MCP Tools with Agents

MCP tools are automatically available to agents when the MCP extension is enabled. When MCP servers are running, their tools are discovered and merged with built-in tools.

### Example Agent Configuration

```toml
# ~/.pekobot/agents/coding-agent/config.toml
[agent]
name = "coding-agent"
```

Enable the MCP extension for the agent:

```bash
pekobot ext enable <mcp-capability>
```

## Available MCP Servers

### Official Servers

- **@anthropic/mcp-filesystem-server** - File system operations
- **@anthropic/mcp-browser-server** - Browser automation
- **@anthropic/mcp-sqlite-server** - SQLite database access

### Community Servers

See the [MCP Servers Repository](https://github.com/modelcontextprotocol/servers) for a list of community-built servers.

## Troubleshooting

### Server Won't Start

1. Check that the extension is installed:
   ```bash
   pekobot ext list
   ```

2. Check extension status:
   ```bash
   pekobot ext status <mcp-extension-name>
   ```

3. Debug the extension:
   ```bash
   pekobot ext debug <mcp-extension-name>
   ```

### Connection Issues

For SSE servers:
- Verify the endpoint URL is accessible
- Check firewall settings
- Ensure TLS certificates are valid

For stdio servers:
- Verify the command is in PATH
- Check file permissions
- Ensure all dependencies are installed

### Tool Discovery Fails

1. Verify the extension runtime is running:
   ```bash
   pekobot ext status <mcp-extension-name>
   ```

2. Check extension info:
   ```bash
   pekobot ext info <mcp-extension-name>
   ```

3. Review logs with verbose mode:
   ```bash
   pekobot -vv ext debug <mcp-extension-name>
   ```

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                     Pekobot Agent                           │
│                   (uses Vec<Arc<dyn Tool>>)                 │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                      Tool Collection                        │
│  ┌─────────────────┐  ┌─────────────────────────────────┐   │
│  │ Built-in Tools  │  │ MCP Tool Proxies                │   │
│  │ - FileSystem    │  │ (dynamically discovered)        │   │
│  │ - Http          │  │                                 │   │
│  │ - Process       │  │                                 │   │
│  └─────────────────┘  └─────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                  Extension Architecture                       │
│         (MCP adapter, lifecycle, health monitoring)           │
└─────────────────────────────────────────────────────────────┘
                              │
        ┌─────────────────────┼─────────────────────┐
        ▼                     ▼                     ▼
┌──────────────┐      ┌──────────────┐      ┌──────────────┐
│ MCP Client   │      │ MCP Client   │      │ MCP Client   │
│ (stdio)      │      │ (stdio)      │      │ (SSE)        │
└──────────────┘      └──────────────┘      └──────────────┘
        │                     │                     │
        ▼                     ▼                     ▼
┌──────────────┐      ┌──────────────┐      ┌──────────────┐
│ filesystem   │      │ browser      │      │ remote-tools │
│ server       │      │ server       │      │ server       │
└──────────────┘      └──────────────┘      └──────────────┘
```

## Specification

Pekobot implements MCP protocol version `2024-11-05`.

Supported features:
- ✅ stdio transport
- ✅ SSE transport
- ✅ Tool discovery and invocation
- ✅ Resource listing and reading
- ✅ Prompt listing and retrieval
- ✅ Server initialization
- ✅ Health monitoring
- 🚧 Sampling (coming soon)

## Contributing

To add support for a new MCP feature:

1. Update `src/mcp/types.rs` with new protocol types
2. Implement in `src/mcp/client.rs` or `src/mcp/transport.rs`
3. Add tests in the respective test modules
4. Update this documentation

## Resources

- [MCP Specification](https://modelcontextprotocol.io/specification/)
- [MCP GitHub](https://github.com/modelcontextprotocol)
- [MCP Servers](https://github.com/modelcontextprotocol/servers)
