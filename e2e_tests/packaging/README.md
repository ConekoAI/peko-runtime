# Pekobot Packaging E2E Tests (ADR-027)

This directory contains end-to-end tests for the unified packaging system defined in [ADR-027](../../docs/architecture/adr/ADR-027-unified-packaging.md).

## Prerequisites

- PowerShell 7+
- Rust/Cargo (pekobot binary)
- Python 3 with `fastapi` + `uvicorn` (for mock registry)
- `MINIMAX_API_KEY` environment variable (for LLM-involved tests)
- **Daemon must be running** before executing tests that involve tool execution or session memory

## Test Inventory

| Test | Description | Registry | LLM |
|------|-------------|----------|-----|
| `agent_build_export_import.ps1` | Build, inspect, import .agent packages | No | No |
| `registry_push_pull.ps1` | Push/pull .agent via mock registry, dedup, caching | Yes | No |
| `team_export_import.ps1` | Export, import .team with checksums, flags | No | No |
| `team_registry_snapshot.ps1` | Team snapshot: create → run → export → push → pull → import → verify | Yes | Optional |
| `team_with_extensions.ps1` | Team with extensions: install → enable → export → import → tool execution | Yes | Optional |
| `agent_registry_lifecycle.ps1` | Agent v1/v2 build, push, pull, upgrade, incremental layers | Yes | Optional |
| `team_snapshot_with_sessions.ps1` | Team export with/without sessions, memory continuity | Yes | Optional |
| `extension_bundle_registry.ps1` | Extension .ext export, registry push/pull, fresh-install, tool execution | Yes | Optional |
| `team_subteam_hierarchy.ps1` | Multi-team hierarchy export/import, workspace isolation | Yes | No |
| `cross_platform_agent_share.ps1` | Cross-platform agent build/share, config/workspace/skill preservation | Yes | Optional |
| `agent_snapshot_memory.ps1` | **NEW** Agent snapshot with sessions: build → run → export → push → pull → import → memory LLM verify | Yes | Optional |
| `registry_layer_dedup.ps1` | **NEW** Cross-agent layer deduplication in registry (content-addressable storage) | Yes | No |
| `team_full_lifecycle.ps1` | **NEW** Comprehensive team lifecycle: extensions + sessions + registry + fresh machine + tool execution + memory | Yes | Optional |

## Real-World Use Cases Covered

### 1. Spin up a fresh team with selected agents/subteams in a registry
- **`team_registry_snapshot.ps1`** — Creates a team with multiple agents, exports it, pushes to registry, pulls on fresh machine, imports, and verifies structural integrity.
- **`team_subteam_hierarchy.ps1`** — Creates parent/subteams with agents, exports each, pushes to registry, pulls, imports with new names, verifies workspace isolation.
- **`team_full_lifecycle.ps1`** — Installs extensions, creates team, enables extensions per agent, exports team snapshot, pushes to registry.

### 2. Save and share a snapshot of a running team (with memory/skills)
- **`team_snapshot_with_sessions.ps1`** — Generates session history, exports with `--include-sessions`, pushes to registry, pulls, imports, verifies session count and content continuity.
- **`agent_snapshot_memory.ps1`** — Builds an agent, runs it to generate memory, exports with sessions, pushes to registry, pulls on fresh machine, imports, and verifies memory via LLM deterministic keywords.

### 3. Pull a team snapshot from registry with extensions and get it to work
- **`team_with_extensions.ps1`** — Installs extensions, creates team, exports, imports on fresh environment, verifies extension enablement and tool execution.
- **`extension_bundle_registry.ps1`** — Exports extensions to `.ext`, pushes to registry, simulates fresh machine by uninstalling, pulls, installs from pulled `.ext`, creates team, verifies tool execution.
- **`team_full_lifecycle.ps1`** — **Most comprehensive**: pushes both team snapshot and extension `.ext` packages to registry, simulates fresh machine (removes team + extensions), pulls everything, reinstalls extensions, imports team, verifies tools work and memory is preserved.

### 4. Registry efficiency and integrity
- **`registry_push_pull.ps1`** — Verifies layer caching, re-pull uses cached layers, HEAD checks skip redundant uploads.
- **`agent_registry_lifecycle.ps1`** — Builds v1 and v2, verifies incremental layer upload (only changed layers), pulls both versions.
- **`registry_layer_dedup.ps1`** — Builds two agents with shared layers, pushes both, verifies registry stores unique layers only once.
- **`cross_platform_agent_share.ps1`** — Builds rich agent with all layers, pushes, pulls, verifies config/skills/workspace preservation and LLM functionality.

## Deterministic Verification

To eliminate flakiness, all tests that involve LLM calls use **deterministic keyword verification**:

- Prompts instruct the LLM to respond with exact keywords like `SUCCESS`, `FAIL`, `MEMORY_SUCCESS`, `CALC_SUCCESS`, `MCP_SUCCESS`, etc.
- Tests match against these keywords rather than parsing free-form text.
- When no API key is available, LLM-dependent steps are skipped with a warning.

## Mock Registry

Tests that need registry operations use the Python FastAPI mock server at:
```
e2e_tests/packaging/mock_registry/main.py
```

Each test starts its own mock registry on a **unique port** to avoid conflicts.

### Registry helpers (used across tests)

```powershell
function Start-MockRegistry { param([int]$Port) ... }
function Stop-MockRegistry { param($Proc) ... }
function Reset-RegistryStorage { param([int]$Port) ... }
function Get-RegistryBlobs { param([int]$Port) ... }
function Push-BlobToRegistry { param([int]$Port, [string]$Repo, [string]$FilePath) ... }
function Pull-BlobFromRegistry { param([int]$Port, [string]$Repo, [string]$Digest, [string]$OutPath) ... }
```

## Running Tests

### Individual test
```powershell
cd e2e_tests/packaging
./agent_build_export_import.ps1
./team_full_lifecycle.ps1 -Provider minimax
```

### All packaging tests
```powershell
cd e2e_tests/packaging
./packaging_all.ps1 -Provider minimax
```

### With specific provider
```powershell
./team_full_lifecycle.ps1 -Provider minimax
```

## Assumptions

1. **Daemon is running** — Tests that involve `pekobot send`, session creation, or MCP runtime assume the daemon is already started. Start it with:
   ```powershell
   pekobot daemon start
   ```
2. **Fresh state** — Each test resets `~/.pekobot` and `%APPDATA%/pekobot` at the start.
3. **Best UX flow** — Some CLI commands (e.g., `pekobot team push`, `pekobot team pull`) may not be fully implemented yet. Tests use direct blob upload/download via the mock registry API as a fallback, with comments indicating where CLI commands should be substituted when available.
