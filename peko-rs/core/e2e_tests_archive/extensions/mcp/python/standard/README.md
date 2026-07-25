# Standard MCP Server E2E Test

This directory contains a **pure standard MCP server** that validates Pekobot's
Tier 1 ecosystem standard detection (`server.json` → MCP adapter).

## Architecture

```
standard/
├── server.json       ← MCP Registry standard (Tier 1 detection)
├── mcp_server.py     ← Pure MCP server implementation
└── test.ps1          ← E2E test script
```

**No `manifest.yaml`. No Pekobot-specific fields.** This is intentional — it
proves that standard MCP servers from the broader ecosystem work in Pekobot
without any custom metadata.

## server.json

Follows the official MCP Registry schema:
- `name`, `version`, `description` — metadata
- `transport` — stdio-based execution
- `packages` — server packages for distribution

## Tools

| Tool | Purpose |
|------|---------|
| `echo` | Echoes a message (basic execution test) |
| `add` | Adds two numbers (schema/parameter test) |
| `get_server_info` | Returns server metadata (structured output test) |

## Running the Test

```powershell
# From project root
.\e2e_tests\extensions\mcp\python\standard\test.ps1 -Provider minimax
```

## What This Tests

1. **Tier 1 Detection**: `server.json` is detected as MCP without `--type` flag
2. **Auto-install**: `pekobot ext install` works with pure `server.json`
3. **Tool Registration**: Tools are discovered and registered automatically
4. **Tool Execution**: Agent can invoke standard MCP tools via `pekobot send`
5. **No Peko-specific metadata needed**: Validates ADR-024's ecosystem compatibility goal
