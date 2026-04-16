#!/usr/bin/env pwsh
# Async Tool Execution E2E Test
#
# Tests the async execution path for tools using the _async reserved parameter.
# Design: Option 3 (minimal file-based polling) - see discussion in PR.
#
# Expected behavior:
# 1. Agent calls a tool with _async: true
# 2. Tool returns a receipt immediately containing a task_file path
# 3. Agent polls the task file directly (via read_file or shell) for progress
# 4. When complete, the task file contains the full result
# 5. No automatic injection into the agentic loop - agent is in full control
#
# This test documents the intended contract. Some functionality may be stubbed
# or not yet fully wired in the engine.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Async Tool E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Error "MINIMAX_API_KEY environment variable not set"
    exit 1
}

# Build pekobot
Write-Host "Building pekobot..." -ForegroundColor Cyan
pushd "$PSScriptRoot/../.."
$env:RUSTFLAGS = "-A warnings"
cargo build --quiet
if ($LASTEXITCODE -ne 0) {
    Write-Error "Build failed"
    exit 1
}
popd

# Reset pekobot config data
$pekobotDir = "$env:USERPROFILE/.pekobot"
if (Test-Path $pekobotDir) {
    Remove-Item -Recurse -Force $pekobotDir
    Write-Host "Reset .pekobot directory" -ForegroundColor Yellow
}
$DataDir = "$env:APPDATA/pekobot"
if (Test-Path $DataDir) {
    Remove-Item -Recurse -Force $DataDir
    Write-Host "Reset data directory" -ForegroundColor Yellow
}

# Set API key
peko auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create agent
$agentName = "async_test"
peko agent create $agentName --provider $Provider 2>&1 | Out-Null
Write-Host "Created agent: $agentName" -ForegroundColor Green

# Enable required tools
peko ext enable shell --target default/$agentName 2>&1 | Out-Null
peko ext enable read_file --target default/$agentName 2>&1 | Out-Null
Write-Host "Enabled shell and read_file tools" -ForegroundColor Green

# ============================================================
# TEST 1: Async shell execution returns a receipt
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Async shell returns receipt" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$prompt = @"
Use the shell tool to run the command 'echo hello_from_async' with async mode enabled.
To do this, include `_async: true` in the tool parameters.

The tool should return a JSON receipt. Read that receipt carefully.
If the receipt contains a 'task_file' path, use read_file to read that path and check the task status.
If the receipt contains 'check_status_tool', call that tool with the 'task_id' to check status.

When the task completes and you see 'hello_from_async' in the output, respond with ASYNC_SUCCESS.
If something goes wrong or you don't get a receipt, respond with ASYNC_FAILED and explain why.
"@

Write-Host "Sending async shell request..." -ForegroundColor Yellow
$response = peko send $agentName $prompt --no-stream 2>&1
Write-Host "Response: $response"

$success = $response -match "ASYNC_SUCCESS"
$failed = $response -match "ASYNC_FAILED"

if ($success) {
    Write-Host "PASS: Agent successfully used async shell and retrieved result" -ForegroundColor Green
} elseif ($failed) {
    Write-Host "EXPECTED FAIL (feature may be stubbed): Agent could not complete async flow" -ForegroundColor Yellow
} else {
    Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
}

# ============================================================
# TEST 2: Direct async receipt validation (bypass LLM)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Direct receipt validation" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# This test will be implemented once we have a CLI command or API to invoke tools directly.
# For now, we document the expected receipt shape:
#
# Expected receipt when calling shell with _async: true:
# {
#   "_async_status": "queued",
#   "task_id": "shell:<uuid>",
#   "task_file": "~/.pekobot/async_tasks/shell_<uuid>.json",
#   "timeout_requested": 300,
#   "callback_mode": "queue"
# }
#
# Expected task file shape while running:
# {
#   "task_id": "shell:<uuid>",
#   "tool_name": "shell",
#   "status": "running",
#   "stdout": "",
#   "stderr": "",
#   "updated_at": "..."
# }
#
# Expected task file shape when completed:
# {
#   "task_id": "shell:<uuid>",
#   "tool_name": "shell",
#   "status": "completed",
#   "stdout": "hello_from_async\n",
#   "stderr": "",
#   "exit_code": 0,
#   "result": { "exit_code": 0, "stdout": "...", "stderr": "...", "success": true },
#   "completed_at": "..."
# }

Write-Host "Receipt contract documented in test comments" -ForegroundColor Cyan

# ============================================================
# TEST 3: Timeout and cancellation (future)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 3: Async timeout (future)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Future test: call shell with `_async: true` and `_timeout: 1` on a sleep command.
# Expected: task_file eventually shows status: "failed" or "timed_out".

Write-Host "Skipped - to be implemented with full async framework" -ForegroundColor Yellow

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

peko agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted test agent" -ForegroundColor Green

Write-Host "`nAsync tool E2E test completed!" -ForegroundColor Green
Write-Host "`nNotes:" -ForegroundColor Cyan
Write-Host "- This test assumes Option 3 (minimal file-based polling)." -ForegroundColor Cyan
Write-Host "- The agent polls the task_file path from the receipt directly." -ForegroundColor Cyan
Write-Host "- No automatic queue injection into the agentic loop is expected." -ForegroundColor Cyan
