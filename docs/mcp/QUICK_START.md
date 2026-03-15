# MCP Quick Start Guide

Get started with MCP servers in 5 minutes.

## Prerequisites

- Pekobot v0.7.0 or later
- Rust toolchain (for building from source)

## 1. Install MCP Servers

```bash
# Install all recommended servers
pekobot mcp install web
pekobot mcp install browser
pekobot mcp install memory
```

## 2. Verify Installation

```bash
pekobot mcp status
```

You should see:
```
NAME            STATUS       TOOLS      BINARY
------------------------------------------------------------
web             ✅           0          installed
browser         ✅           0          installed
memory          ✅           0          installed
```

## 3. Test Tools

```bash
# Test web search
pekobot mcp call web web_search --kv query="rust programming"

# Test fetch
pekobot mcp call web fetch --kv url=https://example.com

# Test HTTP
pekobot mcp call web http --kv method=GET --kv url=https://api.github.com
```

## 4. Use with Agent

Start an agent as usual. MCP tools are automatically available:

```bash
pekobot agent start my-agent
```

## Common Tasks

### List Available Tools

```bash
pekobot mcp tools
```

### Test Server Health

```bash
pekobot mcp test web
```

### View Configuration

```bash
pekobot mcp config
```

### Reinstall a Server

```bash
pekobot mcp install web --force
```

## Troubleshooting

### Server Not Found

```bash
# Check if binary exists
ls ~/.pekobot/mcp-servers/

# Reinstall if missing
pekobot mcp install web
```

### Connection Failed

```bash
# Test the server
pekobot mcp test web

# Check for errors
pekobot mcp test web -vv
```

## Next Steps

- Read the [Migration Guide](MIGRATION_GUIDE.md)
- Learn about [MCP Configuration](CONFIGURATION.md)
- Explore [Available Tools](TOOLS.md)
