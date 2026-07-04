# MCP (Model Context Protocol) Support

Peko supports the [Model Context Protocol (MCP)](https://modelcontextprotocol.io/), allowing you to extend agent capabilities with external tools from MCP servers.

## Overview

MCP is an open protocol that standardizes how applications provide context to LLMs. With MCP support, Peko can:

- Connect to MCP servers via stdio (local) or SSE (remote)
- Discover and invoke tools from MCP servers
- Use MCP resources and prompts
- Manage multiple MCP servers through the Unified Extension Architecture

## Quick Start

### 1. Install an MCP Server

MCP servers are managed as extensions. For example, to use an MCP filesystem server:

```bash
# Install the MCP server as an extension
peko ext install <mcp-extension-path-or-url>
```

### 2. Start the MCP Server

```bash
# Start the MCP server runtime
peko ext start <mcp-extension-name>
```

### 3. Verify It's Working

```bash
# Check extension status
peko ext status <mcp-extension-name>

# List installed extensions
peko ext list
```

## Managing MCP via Extensions

Peko manages MCP servers through the Unified Extension Architecture (`peko ext`):

### Install an MCP Extension

```bash
peko ext install <path-or-url>
```

### List Extensions

```bash
peko ext list
```

### Enable/Disable Extensions

```bash
peko ext enable <mcp-extension>
peko ext disable <mcp-extension>
```

### Start/Stop MCP Runtimes

```bash
# Start a background runtime for the extension
peko ext start <mcp-extension-name>

# Stop the runtime
peko ext stop <mcp-extension-name>

# Restart the runtime
peko ext restart <mcp-extension-name>

# Check runtime status
peko ext status <mcp-extension-name>
```

### Show Extension Info

```bash
peko ext info <mcp-extension-name>
```

### Debug an Extension

```bash
peko ext debug <mcp-extension-name>
```

## Configuration

Extension settings can be configured at global, team, or agent level:

```bash
# Configure extension settings
peko ext config <mcp-extension-name>
```

## Using MCP Tools with Agents

MCP tools are automatically available to agents when the MCP extension is enabled. When MCP servers are running, their tools are discovered and merged with built-in tools.

### Example Agent Configuration

```toml
# ~/.peko/agents/coding-agent/config.toml
[agent]
name = "coding-agent"
```

Enable the MCP extension for the agent:

```bash
peko ext enable <mcp-extension>
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
   peko ext list
   ```

2. Check extension status:
   ```bash
   peko ext status <mcp-extension-name>
   ```

3. Debug the extension:
   ```bash
   peko ext debug <mcp-extension-name>
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
   peko ext status <mcp-extension-name>
   ```

2. Check extension info:
   ```bash
   peko ext info <mcp-extension-name>
   ```

3. Review logs with verbose mode:
   ```bash
   peko -vv ext debug <mcp-extension-name>
   ```

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                     Peko Agent                           │
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

Peko implements MCP protocol version `2024-11-05`.

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
