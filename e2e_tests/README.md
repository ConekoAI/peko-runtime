# Pekobot E2E Tests

This directory holds end-to-end (E2E) tests for Pekobot written in PowerShell. They live outside CI and are being progressively migrated into Rust integration tests under [`tests/`](../tests/) — see [`docs/integration/TESTING.md` §7](../docs/integration/TESTING.md) for the migration roadmap. Once Phase E lands, this folder is deleted.

## Test Structure

```
e2e_tests/
├── README.md              # This file
├── agent/                 # Agent management tests
├── extensions/            # Extension 2.0 Architecture tests
├── providers/             # Provider tests (minimax)
├── send/                  # Message sending tests
├── session/               # Session management tests
├── team/                  # Team management tests
└── _archive/              # Archived legacy tests
    ├── cap/               # Legacy CAP framework tests
    └── providers/         # Legacy provider tests (kimi)
```

> `packaging/` was removed in Phase A — its coverage now lives in the Rust integration tests at [`tests/registry_integration.rs`](../tests/registry_integration.rs), [`tests/pekohub_integration.rs`](../tests/pekohub_integration.rs), [`tests/packaging_integration.rs`](../tests/packaging_integration.rs), [`tests/team_integration.rs`](../tests/team_integration.rs), and [`tests/extension_packaging.rs`](../tests/extension_packaging.rs).

## Prerequisites

- PowerShell 7+
- Rust/Cargo
- `MINIMAX_API_KEY` environment variable (for LLM-involved tests)

## Running Tests

### Set environment variable
```powershell
$env:MINIMAX_API_KEY = "your-api-key-here"
```

### Run an individual test
```powershell
cd e2e_tests/extensions
./skill_extension_test.ps1
```

### Run with a specific provider (default: minimax)
```powershell
./skill_extension_test.ps1 -Provider minimax
```

> There is no top-level runner script. The old `run_all_tests.ps1` was removed; the canonical way to run "all tests" is now `make test-integration` / `make test-integration-llm` from the runtime crate, which exercises the Rust replacements.

## Extension 2.0 Architecture Tests

The `extensions/` directory contains tests for the unified Extension Architecture:

| Test | Description |
|------|-------------|
| `extensions/skill/python/test.ps1` | Skill lifecycle via `pekobot ext` |
| `extensions/mcp/python/*/test.ps1` | MCP server management |
| `extensions/universal/python/*/test.ps1` | Universal tool management |
| `extensions/gateway/http_basics/test.ps1` | Gateway extension HTTP basics |

## Legacy Tests

Tests in `_archive/` use the legacy `pekobot cap` command structure and are kept for reference only.

See `_archive/README.md` for details.

