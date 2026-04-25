#!/usr/bin/env pwsh
# Subagent Spawn — Status & List Tools E2E Test
#
# Tests the agent_spawn_status and agent_spawn_list tools:
# - agent_spawn_status: check the status of a previously spawned subagent by run_id
# - agent_spawn_list: list all active subagent runs for the current session
#
# All operations assume the daemon is already running.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Subagent Spawn — Status & List E2E Test" -ForegroundColor Cyan
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

# Create parent agent
$parentAgent = "subagent_status_parent"
peko agent create $parentAgent --provider $Provider 2>&1 | Out-Null
Write-Host "Created parent agent: $parentAgent" -ForegroundColor Green

# Built-in tools are enabled by default
Write-Host "Built-in tools already enabled by default" -ForegroundColor Green

# Get workspace directory
$workspaceDir = "$env:APPDATA/pekobot/workspaces/default/$parentAgent"

# Ensure cleanup runs even if tests fail
try {
    # ============================================================
    # TEST 1: agent_spawn_status returns correct status
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 1: agent_spawn_status tool" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $statusFile = "status_test.txt"
    $prompt = @"
Use agent_spawn with `_async: true` to delegate this task:
"Use shell to run: Start-Sleep 8; echo STATUS_TEST_DONE > $statusFile"

You should get a receipt with a run_id. 
Then IMMEDIATELY (don't wait) use the agent_spawn_status tool with that run_id to check the status.

If agent_spawn_status shows status "running" or "pending", respond with STATUS_RUNNING_OK.
If it shows "completed" immediately (which would be unexpected for an 8s task), respond with STATUS_ALREADY_DONE.
If the tool is unavailable or fails, respond with STATUS_TOOL_FAILED.
"@

    Write-Host "Sending status check test..." -ForegroundColor Yellow
    $response = peko send $parentAgent $prompt --no-stream 2>&1
    Write-Host "Response: $response"

    if ($response -match "STATUS_RUNNING_OK") {
        Write-Host "PASS: agent_spawn_status correctly showed running/pending status" -ForegroundColor Green
    } elseif ($response -match "STATUS_ALREADY_DONE") {
        Write-Host "UNEXPECTED: Task completed immediately — async may not be working" -ForegroundColor Yellow
    } elseif ($response -match "STATUS_TOOL_FAILED") {
        Write-Host "FAIL: agent_spawn_status tool unavailable or failed" -ForegroundColor Red
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 2: agent_spawn_status shows completed after task finishes
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 2: agent_spawn_status shows completed after finish" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    Write-Host "Waiting for async task to complete (8s)..." -ForegroundColor Yellow
    Start-Sleep 8

    $prompt2 = @"
Use the agent_spawn_status tool again with the same run_id from the previous task.
Also check if the file '$statusFile' exists.

If agent_spawn_status shows status "completed" and the file exists with STATUS_TEST_DONE, respond with STATUS_COMPLETE_OK.
If status is still "running", respond with STATUS_STILL_RUNNING.
If status shows "failed" or "timed_out", respond with STATUS_COMPLETE_FAILED.
If the tool is unavailable, respond with STATUS_TOOL_MISSING.
"@

    Write-Host "Sending completion check..." -ForegroundColor Yellow
    $response2 = peko send $parentAgent $prompt2 --no-stream 2>&1
    Write-Host "Response: $response2"

    Start-Sleep -Milliseconds 500
    $expectedFile = "$workspaceDir/$statusFile"
    $fileExists = Test-Path $expectedFile
    $fileContent = if ($fileExists) { Get-Content $expectedFile -Raw } else { "<missing>" }

    if ($response2 -match "STATUS_COMPLETE_OK" -and $fileExists -and $fileContent -match "STATUS_TEST_DONE") {
        Write-Host "PASS: agent_spawn_status showed completed and file was created" -ForegroundColor Green
    } elseif ($response2 -match "STATUS_STILL_RUNNING") {
        Write-Host "IN_PROGRESS: Task still running" -ForegroundColor Yellow
    } elseif ($response2 -match "STATUS_COMPLETE_FAILED") {
        Write-Host "FAIL: Task failed or timed out" -ForegroundColor Red
    } elseif ($response2 -match "STATUS_TOOL_MISSING") {
        Write-Host "FAIL: agent_spawn_status tool not available" -ForegroundColor Red
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 3: agent_spawn_list shows active runs
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 3: agent_spawn_list tool" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $prompt3 = @"
Launch TWO async subagents concurrently:
1. "Use shell to run: Start-Sleep 10; echo done1"
2. "Use shell to run: Start-Sleep 10; echo done2"

Immediately after getting both receipts, use the agent_spawn_list tool to list all active runs.

If agent_spawn_list shows at least 2 active runs, respond with LIST_OK and the count.
If it shows fewer than 2, respond with LIST_LOW_COUNT.
If the tool is unavailable, respond with LIST_TOOL_MISSING.
If something else goes wrong, respond with LIST_FAILED.
"@

    Write-Host "Sending list test..." -ForegroundColor Yellow
    $response3 = peko send $parentAgent $prompt3 --no-stream 2>&1
    Write-Host "Response: $response3"

    if ($response3 -match "LIST_OK") {
        Write-Host "PASS: agent_spawn_list showed multiple active runs" -ForegroundColor Green
    } elseif ($response3 -match "LIST_LOW_COUNT") {
        Write-Host "FAIL: agent_spawn_list showed fewer runs than expected" -ForegroundColor Red
    } elseif ($response3 -match "LIST_TOOL_MISSING") {
        Write-Host "FAIL: agent_spawn_list tool not available" -ForegroundColor Red
    } elseif ($response3 -match "LIST_FAILED") {
        Write-Host "FAIL: Agent reported LIST_FAILED" -ForegroundColor Red
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # Wait for the 10s tasks to finish before cleanup
    Write-Host "Waiting for remaining async tasks to complete..." -ForegroundColor Yellow
    Start-Sleep 10

} finally {
    # ============================================================
    # Cleanup
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    if (Test-Path "$workspaceDir/status_test.txt") {
        Remove-Item "$workspaceDir/status_test.txt" -Force
    }

    peko agent delete $parentAgent --force 2>&1 | Out-Null
    Write-Host "Deleted test agent" -ForegroundColor Green
}

Write-Host "`nSubagent status & list e2e tests completed!" -ForegroundColor Green
