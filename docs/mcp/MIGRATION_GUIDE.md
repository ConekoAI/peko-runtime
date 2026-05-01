# GAP-008: MCP Migration Guide

## Overview

Starting with v0.7.0, Pekobot has migrated heavy tools (web_search, fetch, http, browser, memory) from embedded core to standalone MCP servers. This guide helps you understand and navigate this change.

> **Note:** The built-in MCP servers (`mcp-web`, `mcp-browser`, `mcp-memory`) have been removed from the main repository. They should be maintained as separate external projects or obtained from the community MCP servers ecosystem.

## What Changed

### Before (v0.6.x)

All tools were built into Pekobot core:

```
pekobot (single binary, ~20MB)
├── filesystem
├── process
├── apply_patch
├── web_search  ← embedded
├── fetch       ← embedded
├── http        ← embedded
├── browser     ← embedded
└── memory      ← embedded
```

### After (v0.7.0+)

Core tools are minimal; heavy tools are provided by external MCP servers:

```
pekobot (core binary, ~20MB)
├── filesystem
├── process
├── apply_patch
└── cron

MCP Servers (external binaries — install separately)
├── @anthropic/mcp-filesystem-server
├── @anthropic/mcp-browser-server
├── @anthropic/mcp-sqlite-server
└── community MCP servers
```

## Migration Steps

### 1. Install MCP Servers

Install the servers you need from the community ecosystem:

```bash
# Example: install filesystem server via npm
npm install -g @anthropic/mcp-filesystem-server

# Example: install browser server via npm
npm install -g @anthropic/mcp-browser-server

# Example: install SQLite server via npm
npm install -g @anthropic/mcp-sqlite-server
```

### 2. Verify Installation

Check that servers are installed and recognized:

```bash
pekobot mcp status
```

Expected output:
```
MCP Server Status

Config: /home/user/.pekobot/mcp.toml

NAME            STATUS       TOOLS      BINARY
------------------------------------------------------------
filesystem      ✅           3          installed
browser         ✅           4          installed
```

### 3. Test Servers

Test each server to ensure it works:

```bash
pekobot mcp test filesystem
pekobot mcp test browser
```

### 4. Verify Tool Loading

Start an agent and verify MCP tools are loaded:

```bash
pekobot agent start my-agent
```

Look for log messages:
```
✅ Loaded 8 tools from 3 MCP servers
```

## Configuration

### Default Config Location

`~/.pekobot/mcp.toml`

### Manual Config Editing

```bash
# View config
pekobot mcp config

# Edit in $EDITOR
pekobot mcp config --edit
```

### Config Format

```toml
[[server]]
name = "filesystem"
transport = "stdio"
command = "mcp-filesystem-server"
args = ["/home/user/docs"]
auto_start = true

[[server]]
name = "browser"
transport = "stdio"
command = "mcp-browser-server"
args = ["--headless"]
env = { "BROWSER_TIMEOUT" = "30" }
```

## CLI Commands

### Installation

```bash
# Add a server manually
pekobot mcp add filesystem stdio --command mcp-filesystem-server --args /home/user/docs

# Remove a server
pekobot mcp remove filesystem
```

### Status & Discovery

```bash
# Check all servers
pekobot mcp status

# Check specific server
pekobot mcp status --server web

# List configured servers
pekobot mcp list
pekobot mcp list --long

# Show server details
pekobot mcp show web
```

### Testing

```bash
# Test server connection
pekobot mcp test web

# List available tools
pekobot mcp tools
pekobot mcp tools --server web
```

### Direct Tool Calling

```bash
# Call tool with JSON args
pekobot mcp call web web_search --args '{"query":"rust programming"}'

# Call tool with key=value args
pekobot mcp call web fetch --kv url=https://example.com

# Combine args
pekobot mcp call web http --kv method=GET --kv url=https://api.example.com
```

## Troubleshooting

### "Server not found" Error

**Problem**: MCP server binary is missing.

**Solution**: Install the server from the community ecosystem (e.g., via npm, cargo, etc.) and ensure it is in your PATH.

### "No MCP tools loaded"

**Problem**: Config exists but servers aren't starting.

**Solution**: 
1. Check status: `pekobot mcp status`
2. Test server: `pekobot mcp test filesystem`
3. Check binary exists: `which mcp-filesystem-server`

### "Failed to initialize MCP manager"

**Problem**: Servers are crashing on startup.

**Solution**:
1. Check server binary works directly:
   ```bash
   mcp-filesystem-server --help
   ```
2. Reinstall from the community ecosystem if needed.

### Backward Compatibility

**Problem**: Existing agents expect embedded tools.

**Solution**: No changes needed. Pekobot automatically:
1. Loads MCP tools when available
2. Falls back to core tools if MCP unavailable
3. Logs warnings to guide installation

## FAQ

### Q: Do I have to use MCP servers?

**A**: No. Core tools (filesystem, process, apply_patch, cron) work without MCP. MCP servers are optional for extended functionality.

### Q: Can I mix MCP and embedded tools?

**A**: Yes. Pekobot uses MCP-first loading: it tries MCP first, then falls back. You can have both.

### Q: What if an MCP server crashes?

**A**: Pekobot will log an error and continue with remaining tools. The agent won't fail; it just won't have that server's tools.

### Q: Can I add custom MCP servers?

**A**: Yes! Use `pekobot mcp add`:
```bash
pekobot mcp add my-server stdio --command /path/to/server
```

### Q: Where are MCP binaries stored?

**A**: MCP binaries are installed via your system package manager (npm, cargo, etc.) and should be in your PATH. Pekobot does not manage or store MCP server binaries internally.

### Q: How do I uninstall an MCP server?

**A**: Remove the server from Pekobot's config and uninstall via your package manager:
```bash
pekobot mcp remove filesystem
npm uninstall -g @anthropic/mcp-filesystem-server
```

## Performance Impact

| Metric | Before | After |
|--------|--------|-------|
| Core binary size | ~20MB | ~20MB |
| With MCP servers | N/A | Varies by server |
| Startup time | ~50ms | ~50ms + MCP connection |
| Tool latency | 1-10ms | 5-50ms (cross-process) |

## Benefits of MCP Architecture

1. **Modularity**: Install only tools you need
2. **Isolation**: MCP crashes don't affect core
3. **Updates**: Update servers independently
4. **Language**: Servers can be written in any language
5. **Ecosystem**: Compatible with community MCP servers

## See Also

- [MCP Specification](https://modelcontextprotocol.io/)
- `pekobot mcp --help`
- `pekobot/docs/MCP.md`
