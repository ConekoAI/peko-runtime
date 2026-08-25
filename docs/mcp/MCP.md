# MCP (Model Context Protocol) Support

> **Deprecated as of ADR-047 (2026-08-25).** The `peko ext *` flow this
> document describes has been retired. MCP servers now live directly in
> the principal's workspace under `~/.peko/principal/<name>/mcp/<id>/server.json`
> and are installed via:
>
> ```bash
> peko principal mcp install <server-path>     # copy server.json into the workspace
> peko principal mcp list                     # list workspace MCP servers
> ```
>
> No `start` / `stop` / `restart` / `status` step is required — the
> runtime discovers the server at principal boot. See
> [PRINCIPAL_WORKSPACE.md](../architecture/PRINCIPAL_WORKSPACE.md) for
> the full workspace layout. The body of this document is retained for
> historical reference and will be rewritten in a follow-up PR.

Peko supports the [Model Context Protocol (MCP)](https://modelcontextprotocol.io/), allowing you to extend agent capabilities with external tools from MCP servers.

## Overview

MCP is an open protocol that standardizes how applications provide context to LLMs. With MCP support, Peko can:

- Connect to MCP servers via stdio (local) or SSE (remote)
- Discover and invoke tools from MCP servers
- Use MCP resources and prompts
- Manage multiple MCP servers through the principal workspace (ADR-047) — formerly via the Unified Extension Architecture

## Quick Start

### 1. Install an MCP Server

MCP servers are managed as workspace plugins. For example, to use an MCP filesystem server:

```bash
# Install the MCP server into the principal's workspace (ADR-047)
peko principal mcp install <server-path>
```

### 2. Verify It's Working

```bash
# List MCP servers installed in the workspace
peko principal mcp list
```

## Managing MCP via the Principal Workspace

Peko manages MCP servers through the principal workspace (ADR-047); the
`peko ext *` surface was retired.

### Install an MCP Server

```bash
peko principal mcp install <server-path>
```

### List MCP Servers

```bash
peko principal mcp list
```

### Grant/Revoke MCP Capabilities

```bash
peko capability grant --principal <principal-name> mcp:<mcp-server>
peko capability revoke --principal <principal-name> mcp:<mcp-server>
```

> **Note (ADR-047):** the capability grant/revoke CLI is itself
> scheduled for retirement in favor of the per-tier authority model.
> The above form remains the documented path during the migration
> window.

## Configuration

MCP server settings live in `server.json` inside the workspace
directory; edit the file directly or re-run
`peko principal mcp install` with a new manifest.

## Using MCP Tools with Agents

MCP tools are automatically available to a Principal when the corresponding
`mcp:` capability is granted. When MCP servers are present in the
workspace, their tools are discovered and merged with built-in tools.

### Example Agent Configuration

```toml
# ~/.peko/principals/coding-agent/principal.toml
[capabilities]
grants = ["mcp:filesystem-mcp"]
```

Grant the MCP capability to the Principal:

```bash
peko capability grant --principal coding-agent mcp:filesystem-mcp
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
