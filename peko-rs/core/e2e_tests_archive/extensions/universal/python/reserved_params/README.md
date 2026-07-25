# Universal Tool _async Reserved Parameter E2E Test

This E2E test verifies that a Python-based Universal Tool can be executed
asynchronously via the `_async` reserved parameter.

## Files

| File | Purpose |
|------|---------|
| `slow_calculator.py` | Python tool that sleeps before returning (simulates long-running work) |
| `manifest.yaml` | Unified YAML manifest with `extension_type: universal-tool` |
| `test.ps1` | E2E test script |

## What It Tests

1. **Sync execution** — Tool blocks and returns result directly (baseline)
2. **Async receipt** — `_async: true` returns an immediate receipt with `task_id`, `task_file`, `_async_status`
3. **Task file polling** — Agent can read `task_file` to retrieve completed results
4. **Custom timeout** — `_timeout` reserved parameter is reflected in the receipt

## Quick Start

### Prerequisites

```powershell
$env:MINIMAX_API_KEY = "your-api-key"
```

### Run the Test

```powershell
.\test.ps1
```

## Tool Design

The `slow_calculator` tool is identical to `calculator_simple` except it accepts
a `delay_seconds` parameter and calls `time.sleep()` before computing the result.
This makes it easy to verify async behavior:

- With `_async: true` → immediate receipt (non-blocking)
- Without `_async` → blocks for `delay_seconds` then returns result
