# Extension Architecture E2E Tests

This directory contains E2E tests for the Extension 2.0 Architecture (ADR-017).

## Overview

These tests verify the new unified extension management system via the `pekobot ext` CLI commands:

- `pekobot ext install <path>` - Install an extension
- `pekobot ext list [--type <type>] [--enabled-only]` - List extensions
- `pekobot ext info <id>` - Show extension details
- `pekobot ext enable <id>` - Enable an extension
- `pekobot ext disable <id>` - Disable an extension
- `pekobot ext uninstall <id>` - Uninstall an extension
- `pekobot ext bundle --name <name> <ids...>` - Create extension bundle

## Test Files

| Test File | Description |
|-----------|-------------|
| `skill_extension_test.ps1` | Tests skill extension lifecycle using Extension 2.0 |
| `mcp_extension_test.ps1` | Tests MCP server extension management |
| `universal_tool_extension_test.ps1` | Tests universal tool extension management |
| `image_extension_test.ps1` | Tests extension handling in agent image export/import |

## Prerequisites

- PowerShell 7+
- Rust/Cargo
- Python 3.x
- MINIMAX_API_KEY environment variable (or specify --Provider)

## Running Tests

### Run all extension tests:
```powershell
cd e2e_tests/extensions
./skill_extension_test.ps1
./mcp_extension_test.ps1
./universal_tool_extension_test.ps1
```

### Run with specific provider:
```powershell
./skill_extension_test.ps1 -Provider kimi
```

## What Each Test Verifies

### Skill Extension Test
1. Extension installation from local directory
2. Extension listing with type filtering
3. Extension info display
4. Extension enable/disable lifecycle
5. Agent using skill via pekobot send
6. Extension bundle creation
7. Extension uninstallation

### MCP Extension Test
1. MCP server packaged as extension
2. Extension manifest with MCP configuration
3. Extension installation and detection
4. MCP type filtering
5. Extension lifecycle for stateful MCP servers

### Universal Tool Extension Test
1. Universal tool packaged as extension
2. Extension manifest with tool configuration
3. Extension installation and detection
4. Universal tool type filtering
5. Tool execution via agent

### Image Extension Test
1. Extension installation and enablement
2. Agent export to .agent package
3. Verification of package contents
4. Agent import to new team
5. Extension availability after import
6. Extension functionality in imported agent

## Extension Behavior in Agent Images

Extensions are **system-wide resources** (not bundled per-agent):

- Extensions are stored in `~/.pekobot/extensions/` (or `%APPDATA%/pekobot/extensions/` on Windows)
- Extensions persist across agent export/import operations
- Extensions are available to all agents after installation
- Agent images contain only agent config, identity, and workspace

This design allows:
- Shared extensions across multiple agents
- Smaller agent image sizes
- Independent extension lifecycle management

## Migration from Legacy Tests

These new tests replace the legacy tests that used:
- `pekobot cap skill install` → `pekobot ext install`
- `pekobot cap mcp add` → `pekobot ext install`
- `pekobot cap universal install` → `pekobot ext install`

The Extension 2.0 architecture provides:
- Unified management for all extension types
- Consistent CLI commands regardless of extension type
- Better extension lifecycle management
- Bundle support for packaging multiple extensions
