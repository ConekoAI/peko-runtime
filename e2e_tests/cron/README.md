# Cron E2E Tests

End-to-end tests for the Pekobot cron scheduling system.

## Prerequisites

- PowerShell 7+
- Rust/Cargo
- MINIMAX_API_KEY environment variable
- **Daemon must be running** (`peko daemon start`)

## Test Structure

```
e2e_tests/cron/
├── README.md              # This file
├── cron_basics.ps1        # CLI commands: list, add, remove, history
├── cron_execution.ps1     # Daemon execution: at, every, run, side effects
├── cron_agent_tool.ps1    # Agent uses cron tool to self-schedule
└── cron_idle_event.ps1    # Idle & event triggers, announcements
```

## Running Tests

### Start the daemon first:
```powershell
peko daemon start
```

### Set environment variable:
```powershell
$env:MINIMAX_API_KEY = "your-api-key-here"
```

### Run individual tests:
```powershell
# Basic CLI operations (fast, ~30s)
cd e2e_tests/cron
./cron_basics.ps1

# Execution verification (slow, ~3-4 min due to scheduling delays)
./cron_execution.ps1

# Agent self-scheduling (medium, ~5 min)
./cron_agent_tool.ps1

# Idle & event triggers (medium, ~3 min)
./cron_idle_event.ps1
```

### Run with specific provider:
```powershell
./cron_basics.ps1 -Provider minimax
```

## Test Coverage

| Test File | What It Tests | Duration | Deterministic Verification |
|-----------|--------------|----------|---------------------------|
| `cron_basics.ps1` | CLI CRUD: list, add (at/every/cron), remove, history, JSON output | ~30s | Job count in JSON |
| `cron_execution.ps1` | Daemon actually runs jobs: file writes from `at`, `every`, manual `run` | ~3-4 min | File existence & content |
| `cron_agent_tool.ps1` | Agent uses `cron` tool to schedule/list/cancel jobs for itself | ~5 min | Daemon job list + file content |
| `cron_idle_event.ps1` | Idle detection, event triggers, announcement delivery | ~3 min | File content + announcement JSON |

## Expected Behavior vs. Current State

These tests define the **ideal workflow and UX**. Some functionality may still be stubbed:

- **✅ Working**: CLI commands talk to daemon via IPC, jobs persist in SQLite, list/remove/history work.
- **⚠️ May be stubbed**: Actual agent execution from cron jobs (daemon-side `StatelessAgentService` integration).
- **⚠️ May be stubbed**: Idle detection (requires activity tracking), event publishing (requires external event source).
- **⚠️ May be stubbed**: Announcement delivery to external channels (currently file-based).

Tests use `Write-Warning` (not `Write-Error`) for stubbed features so the test suite completes gracefully while documenting gaps.
