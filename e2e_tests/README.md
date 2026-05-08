# Pekobot E2E Tests

This directory contains end-to-end (E2E) tests for Pekobot using PowerShell.

## Test Structure

```
e2e_tests/
├── README.md              # This file
├── agent/                 # Agent management tests
├── extensions/            # Extension 2.0 Architecture tests
├── packaging/             # Agent/team/extension packaging tests (ADR-027)
├── providers/             # Provider tests (minimax)
├── send/                  # Message sending tests
├── session/               # Session management tests
├── team/                  # Team management tests
└── _archive/              # Archived legacy tests
    ├── cap/               # Legacy CAP framework tests
    └── providers/         # Legacy provider tests (kimi)
```

## Prerequisites

- PowerShell 7+
- Rust/Cargo
- MINIMAX_API_KEY environment variable

## Running Tests

### Set environment variable:
```powershell
$env:MINIMAX_API_KEY = "your-api-key-here"
```

### Run all tests:
```powershell
cd e2e_tests
./run_all_tests.ps1
```

### Run individual test:
```powershell
cd e2e_tests/extensions
./skill_extension_test.ps1
```

### Run with specific provider (default: minimax):
```powershell
./skill_extension_test.ps1 -Provider minimax
```

## Packaging Tests (ADR-027)

The `packaging/` directory contains tests for the unified packaging system:

| Test | Description |
|------|-------------|
| `agent_build_export_import.ps1` | Build, export, inspect, import .agent packages |
| `team_export_import.ps1` | Export, import .team packages with checksum validation |

## Extension 2.0 Architecture Tests

The `extensions/` directory contains tests for the new unified Extension Architecture:

| Test | Description |
|------|-------------|
| `skill_extension_test.ps1` | Skill lifecycle via `pekobot ext` |
| `mcp_extension_test.ps1` | MCP server management |
| `universal_tool_extension_test.ps1` | Universal tool management |
| `image_extension_test.ps1` | Extension packaging in images |

## Legacy Tests

Tests in `_archive/` use the legacy `pekobot cap` command structure and are kept for reference only.

See `_archive/README.md` for details.
