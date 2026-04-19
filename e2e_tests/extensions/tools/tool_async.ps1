#!/usr/bin/env pwsh
# Async Tool Execution E2E Test
#
# Tests the async execution path for tools using the _async reserved parameter.
# Design: see ADR-20.
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

# Start daemon
Write-Host "`nStarting pekobot daemon..." -ForegroundColor Cyan
peko daemon start

# Wait for daemon to be ready
$daemonReady = $false
for ($i = 0; $i -lt 30; $i++) {
    Write-Host "Checking if daemon is running..." -ForegroundColor Yellow
    $status = peko daemon status 2>&1
    if ($status -match "running") {
        $daemonReady = $true
        break
    }
    Start-Sleep -Milliseconds 200
}

if (-not $daemonReady) {
    Write-Error "Daemon failed to start"
    exit 1
}
Write-Host "Daemon is running" -ForegroundColor Green

# Ensure cleanup runs even if tests fail
try {
    # ============================================================
    # TEST 1: Async shell execution returns a receipt
    # ============================================================
    # This test checks that when the agent calls the shell tool with _async: true, it receives a receipt containing a task_file path, and that the tool continues to run in the background while the agent's own process ends. The agent should be able to read the task_file anytime later to check for progress and final result.'

    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 1: Async shell returns receipt" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $prompt = @"
Use the shell tool to run the command 'echo async_start; Start-Sleep 6; echo async_complete' with async mode enabled.
To do this, include `_async: true` in the tool parameters.

The tool should return a JSON receipt immediately. Read that receipt carefully.
The receipt should contain a 'task_file' path where you can check for progress and results. Read the task_file path from the receipt and check its contents. If you see a header json identical to your receipt, and the output contains 'async_start', Respond with ASYNC_SUCCESS and include the task_file path from the receipt.
If you don't get a receipt, or the receipt doesn't contain a task_file, or you get the result of the command right away instead of a receipt, or the file contents don't match expectations, respond with ASYNC_FAILED and explain what you got back when you called the tool.
"@

    Write-Host "Sending async shell request..." -ForegroundColor Yellow
    $response = peko send $agentName $prompt --no-stream 2>&1
    Write-Host "Response: $response"

    $success = $response -match "ASYNC_SUCCESS"
    $failed = $response -match "ASYNC_FAILED"

    if ($success) {
        Write-Host "PASS: Agent successfully used async shell and retrieved result" -ForegroundColor Green
    } elseif ($failed) {
        Write-Host "EXPECTED FAIL (feature may be stubbed): Agent could not complete async flow" -ForegroundColor Red
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    Write-Host "`nWaiting for async task to complete..." -ForegroundColor Yellow
    Start-Sleep 6
    # ============================================================
    # TEST 2: Direct progress polling via task_file
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 2: Direct progress polling via task_file" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # This test will test the agent's ability to poll the task_file path for updates. The prompt instructs the agent to read the task_file directly to check for progress and final result, without expecting any automatic injection into the agentic loop.
    Write-Host "Sending async shell request..." -ForegroundColor Yellow
    $response = peko send $agentName "Check the task_file path for the async shell command you ran earlier. If the task is still in progress, respond with POLLING_IN_PROGRESS. If the task is complete and you can read the result from the task_file, respond with POLLING_SUCCESS and include the result. If you cannot access the task_file or something goes wrong, respond with POLLING_FAILED and explain why." --no-stream 2>&1
    Write-Host "Response: $response"

    $pollingSuccess = $response -match "POLLING_SUCCESS"
    $pollingInProgress = $response -match "POLLING_IN_PROGRESS"
    $pollingFailed = $response -match "POLLING_FAILED"
    if ($pollingSuccess) {
        Write-Host "PASS: Agent successfully polled task_file and retrieved result" -ForegroundColor Green
    } elseif ($pollingInProgress) {
        Write-Host "IN_PROGRESS: Task is still running according to agent's polling" -ForegroundColor Red
    } elseif ($pollingFailed) {
        Write-Host "EXPECTED FAIL (feature may be stubbed): Agent could not poll task_file successfully" -ForegroundColor Red
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 3: Timeout and cancellation
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 3: Async timeout" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # test: call shell with `_async: true` and `_timeout: 1` on a sleep 10 command.
    # Expected: task_file eventually shows status: "failed" or "timed_out".

    Write-Host "Sending async shell request with timeout..." -ForegroundColor Yellow
    $response = peko send $agentName "Use the shell tool to run 'Start-Sleep 10' with `_async: true` and `_timeout: 2`. Sleep for a moment e.g. 3 seconds and then check the task_file for the result. If the task times out, respond with TIMEOUT_SUCCESS. If it doesn't time out and completes successfully, respond with TIMEOUT_FAILED. If something goes wrong, respond with TIMEOUT_ERROR and explain." --no-stream 2>&1
    Write-Host "Response: $response"

    $timeoutSuccess = $response -match "TIMEOUT_SUCCESS"
    $timeoutFailed = $response -match "TIMEOUT_FAILED"
    $timeoutError = $response -match "TIMEOUT_ERROR"
    if ($timeoutSuccess) {
        Write-Host "PASS: Agent correctly handled async tool timeout" -ForegroundColor Green
    } elseif ($timeoutFailed) {
        Write-Host "FAIL: Agent did not time out as expected" -ForegroundColor Red
    } elseif ($timeoutError) {
        Write-Host "EXPECTED FAIL (feature may be stubbed): Agent could not handle timeout scenario" -ForegroundColor Yellow
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }


    # future test: call shell with `_async: true` on a sleep 100 command and ask agent to cancel it after a few seconds by using the process id from the receipt.
    # Expected: task_file disrupted mid-execution.
} finally {
    # ============================================================
    # Cleanup
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    peko agent delete $agentName --force 2>&1 | Out-Null
    Write-Host "Deleted test agent" -ForegroundColor Green

    peko daemon stop 2>&1 | Out-Null
    Write-Host "Stopped daemon" -ForegroundColor Green
}

Write-Host "`nAsync tool E2E test completed!" -ForegroundColor Green
Write-Host "`nNotes:" -ForegroundColor Cyan
Write-Host "- This test assumes Option 3 (minimal file-based polling)." -ForegroundColor Cyan
Write-Host "- The agent polls the task_file path from the receipt directly." -ForegroundColor Cyan
Write-Host "- No automatic queue injection into the agentic loop is expected." -ForegroundColor Cyan
