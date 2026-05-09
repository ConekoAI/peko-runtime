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
- MINIMAX_API_KEY environment variable (for LLM-involved tests)
- Python 3 with fastapi + uvicorn (for packaging tests that use mock registry)

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
| `registry_push_pull.ps1` | Push/pull .agent via mock registry (content-addressable layers, dedup) |
| `team_registry_snapshot.ps1` | Team snapshot lifecycle: create → run → export → push → pull → import → verify |
| `team_with_extensions.ps1` | Team with extensions: install → enable → export → import → tool execution |
| `agent_registry_lifecycle.ps1` | Agent v1/v2 build, push, pull, upgrade path, incremental layer upload, LLM verification |
| `team_snapshot_with_sessions.ps1` | Team export with/without sessions, registry push/pull, session memory continuity |
| `extension_bundle_registry.ps1` | Extension .ext export, registry push/pull, fresh-install, tool execution |
| `team_subteam_hierarchy.ps1` | Multi-team hierarchy export/import, workspace isolation, cross-team references |
| `cross_platform_agent_share.ps1` | Cross-platform agent build/share, config/workspace/skill preservation, dedup |
| `packaging_all.ps1` | Orchestrates all packaging tests in sequence |

### Deterministic Verification

All packaging tests that involve LLM calls use **deterministic keyword verification** to eliminate flakiness:
- Prompts instruct the LLM to respond with exact keywords like `SUCCESS`, `FAIL`, `MEMORY_SUCCESS`, etc.
- Tests match against these keywords rather than parsing free-form text.
- When no API key is available, LLM-dependent steps are skipped with a warning.

### Mock Registry

Packaging tests that need registry operations use the Python FastAPI mock server at:
```
e2e_tests/packaging/mock_registry/main.py
```

Each test starts its own mock registry on a unique port to avoid conflicts.

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
