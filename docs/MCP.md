# MCP (Model Context Protocol) Support

Pekobot supports the [Model Context Protocol (MCP)](https://modelcontextprotocol.io/), allowing you to extend agent capabilities with external tools from MCP servers.

## Overview

MCP is an open protocol that standardizes how applications provide context to LLMs. With MCP support, Pekobot can:

- Connect to MCP servers via stdio (local) or SSE (remote)
- Discover and invoke tools from MCP servers
- Use MCP resources and prompts
- Manage multiple MCP servers with lifecycle and health monitoring

## Quick Start

### 1. Install an MCP Server

For example, install the filesystem MCP server:

```bash
npm install -g @anthropic/mcp-filesystem-server
```

### 2. Configure the Server

Add the server to Pekobot's MCP configuration:

```bash
pekobot mcp add filesystem stdio --command mcp-filesystem-server --args /home/user/docs
```

### 3. List Available Tools

```bash
pekobot mcp tools
```

### 4. Test the Connection

```bash
pekobot mcp test filesystem
```

## CLI Commands

### `pekobot mcp list`

List configured MCP servers.

```bash
pekobot mcp list              # Simple list
pekobot mcp list --long       # Detailed information
```

### `pekobot mcp show <name>`

Show detailed information about a specific server.

```bash
pekobot mcp show filesystem
```

### `pekobot mcp add`

Add a new MCP server.

**Stdio transport:**
```bash
pekobot mcp add browser stdio \
  --command mcp-browser-server \
  --args --headless \
  --env LOG_LEVEL=debug
```

**SSE transport:**
```bash
pekobot mcp add remote-tools sse \
  --endpoint https://tools.example.com/mcp
```

Options:
- `--command`: Command to execute (stdio only)
- `--args`: Arguments for the command (can be repeated)
- `--endpoint`: Endpoint URL (SSE only)
- `--env`: Environment variables (KEY=VALUE, can be repeated)
- `--cwd`: Working directory
- `--no-auto-start`: Don't start automatically

### `pekobot mcp remove <name>`

Remove an MCP server configuration.

```bash
pekobot mcp remove filesystem
pekobot mcp remove filesystem --force
```

### `pekobot mcp start|stop|restart <name>`

Control server lifecycle (runtime management).

```bash
pekobot mcp start filesystem
pekobot mcp stop filesystem
pekobot mcp restart filesystem
```

### `pekobot mcp test <name>`

Test connection to an MCP server.

```bash
pekobot mcp test filesystem
```

### `pekobot mcp tools`

List available tools from MCP servers.

```bash
pekobot mcp tools              # All servers
pekobot mcp tools --server filesystem  # Specific server
```

### `pekobot mcp config`

View or edit MCP configuration.

```bash
pekobot mcp config             # View configuration
pekobot mcp config --edit      # Open in editor
```

## Configuration File

MCP servers are configured in `~/.pekobot/mcp.toml`:

```toml
[[server]]
name = "filesystem"
transport = "stdio"
command = "mcp-filesystem-server"
args = ["/home/user/documents"]
auto_start = true

[[server]]
name = "browser"
transport = "stdio"
command = "mcp-browser-server"
args = ["--headless"]
env = { "BROWSER_TIMEOUT" = "30" }

[[server]]
name = "remote-tools"
transport = "sse"
endpoint = "https://tools.example.com/mcp"
auto_start = false
```

### Configuration Options

| Option | Type | Description |
|--------|------|-------------|
| `name` | string | Unique server identifier |
| `transport` | "stdio" or "sse" | Connection method |
| `command` | string | Command to execute (stdio) |
| `args` | array | Arguments for command |
| `env` | table | Environment variables |
| `cwd` | string | Working directory |
| `endpoint` | string | URL endpoint (SSE) |
| `auto_start` | boolean | Start automatically |
| `health_check_interval_secs` | integer | Health check frequency |
| `max_restarts` | integer | Max restart attempts |
| `init_timeout_secs` | integer | Initialization timeout |
| `tool_timeout_secs` | integer | Tool execution timeout |

## Using MCP Tools with Agents

MCP tools are automatically available to agents. When you add MCP servers, their tools are discovered and merged with built-in tools.

### Example Agent Configuration

```toml
# ~/.pekobot/agents/coding-agent.toml
[agent]
name = "coding-agent"
capabilities = ["filesystem", "mcp"]

# MCP servers are loaded from ~/.pekobot/mcp.toml
```

### Programmatic Usage

```rust
use pekobot::mcp::{McpConfig, McpManager, McpServerConfig};
use std::sync::Arc;
use tokio::sync::RwLock;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load MCP configuration
    let mut config = McpConfig::new();
    config.add_server(McpServerConfig::stdio(
        "filesystem",
        "mcp-filesystem-server",
        vec!["/home/user/docs".to_string()],
    ));

    // Create and initialize manager
    let manager = Arc::new(RwLock::new(McpManager::new(config)));
    manager.read().await.init().await?;

    // Get built-in tools
    let config = pekobot::tools::ToolFactoryConfig::full(".".into());
    let mut tools = pekobot::tools::ToolFactory::create_tools(&config);

    // Add MCP tools
    let mcp_tools = manager.read().await.get_tools().await;
    tools.extend(mcp_tools);

    // Use tools with agent...

    // Shutdown
    manager.read().await.shutdown().await?;
    Ok(())
}
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

1. Check that the command exists:
   ```bash
   which mcp-filesystem-server
   ```

2. Test manually:
   ```bash
   mcp-filesystem-server /path/to/docs
   ```

3. Check logs with verbose mode:
   ```bash
   pekobot -vv mcp test filesystem
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

1. Verify server is running:
   ```bash
   pekobot mcp test <server-name>
   ```

2. Check server capabilities:
   ```bash
   pekobot mcp show <server-name>
   ```

3. Review server logs for errors

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
│                      MCP Manager                            │
│         (lifecycle, health monitoring, routing)             │
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
