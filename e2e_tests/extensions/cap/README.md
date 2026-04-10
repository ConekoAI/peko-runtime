# CAP Extension Tests (ADR-17)

This directory contains the restored and migrated CAP (Capability) E2E tests, updated to use the new **Unified Extension Architecture** (ADR-017).

## Architecture Overview

The new extension system unifies all extension types under a single CLI interface:

```
pekobot ext install <path>     # Install any extension type
pekobot ext list               # List all extensions
pekobot ext enable <id>        # Enable an extension
pekobot ext disable <id>       # Disable an extension
pekobot ext uninstall <id>     # Remove an extension
pekobot ext info <id>          # Show extension details
```

## Test Structure

| Directory | Contents |
|-----------|----------|
| `tool/` | Built-in tool tests (glob, grep, read_file, write_file, str_replace_file, shell, session_status) |
| `mcp/` | MCP server tests with reserved parameter injection |
| `skill/` | Skill extension tests (calculator-skill) |
| `custom/` | Custom universal tool tests (Node.js and Python) |

## Migration Changes

### Old Commands → New Commands

| Old (`pekobot cap`) | New (`pekobot ext`) |
|---------------------|---------------------|
| `pekobot cap list` | `pekobot ext list` |
| `pekobot cap enable <agent> <tool>` | `pekobot ext enable <id>` |
| `pekobot cap disable <agent> <tool>` | `pekobot ext disable <id>` |
| `pekobot cap skill install <path>` | `pekobot ext install <path> --type skill` |
| `pekobot cap skill list` | `pekobot ext list --type skill` |
| `pekobot cap skill uninstall <id>` | `pekobot ext uninstall <id>` |
| `pekobot cap mcp add <name>` | `pekobot ext install <config> --type mcp` |
| `pekobot cap mcp list` | `pekobot ext list --type mcp` |
| `pekobot cap mcp remove <id>` | `pekobot ext uninstall <id>` |
| `pekobot cap universal install <path>` | `pekobot ext install <path> --type universal-tool` |
| `pekobot cap universal list` | `pekobot ext list --type universal-tool` |
| `pekobot cap universal uninstall <id>` | `pekobot ext uninstall <id>` |

## Running Tests

### Individual Tests

```powershell
# Built-in tools
cd tool/glob
.\glob.ps1 -Provider kimi

# MCP tests
cd mcp/python
.\test.ps1 -Provider minimax

# Skill tests
cd skill/python
.\test.ps1 -Provider minimax

# Custom tools
cd custom/python/simple
.\test.ps1 -Provider minimax
```

### All Tests

```powershell
.\run_all_tests.ps1 -Provider kimi
```

## Requirements

- PowerShell 7+ (for cross-platform compatibility)
- Rust toolchain (for building pekobot)
- API keys:
  - `KIMI_API_KEY` for kimi provider
  - `MINIMAX_API_KEY` for minimax provider
- Python 3.11+ (for MCP and custom Python tool tests)
- Node.js (for custom Node.js tool tests)

## Extension Types Tested

1. **Built-in Tools**: These are core tools that ship with pekobot and are exposed as extensions
   - `read_file`, `write_file`, `glob`, `grep`, `str_replace_file`
   - `shell`, `session_status`

2. **Skills**: Documentation-based capabilities via SKILL.md files
   - `calculator-skill`: Demonstrates skill installation and usage

3. **MCP Servers**: Model Context Protocol servers with reserved parameter injection
   - `identity`: Tests context injection (agent_id, session_id)

4. **Universal Tools**: Custom tools using the universal tool protocol
   - Node.js: `string_tool`, `identity_tool`
   - Python: `calculator_simple`, `multi_file_calc`

## Notes

- All tests reset the pekobot data directory before running
- Tests create temporary agents and clean them up after
- The extension framework supports bundling multiple extensions together
- Extensions can be configured at global, team, or agent scope using `pekobot ext config`
