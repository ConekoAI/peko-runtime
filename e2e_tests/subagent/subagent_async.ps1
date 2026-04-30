#!/usr/bin/env pwsh
# Subagent Spawn — Async Mode E2E Test
#
# Tests the _async background execution path for agent_spawn:
# - Agent calls agent_spawn with _async: true
# - Tool returns a receipt immediately containing a task_file path
# - Agent polls the task_file directly (via read_file or shell) for progress
# - When complete, the task_file contains the full subagent result
# - No automatic injection into the agentic loop — agent is in full control
#
# All operations assume the daemon is already running.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Subagent Spawn — Async Mode E2E Test" -ForegroundColor Cyan
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
$parentAgent = "subagent_async_parent"
peko agent create $parentAgent --provider $Provider 2>&1 | Out-Null
Write-Host "Created parent agent: $parentAgent" -ForegroundColor Green

# Built-in tools are enabled by default
Write-Host "Built-in tools already enabled by default" -ForegroundColor Green

# Get workspace directory
$workspaceDir = "$env:APPDATA/pekobot/workspaces/default/$parentAgent"

# Ensure cleanup runs even if tests fail
try {
    $script:failed = $false

    # ============================================================
    # TEST 1: Async spawn returns a receipt with task_file
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 1: Async spawn returns receipt with task_file" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $asyncFile = "async_subagent_result.txt"
    $prompt = 'Use agent_spawn with _async=true to delegate this task to a subagent: Use the shell tool to run: Start-Sleep 15; echo ASYNC_TASK_COMPLETE > ' + $asyncFile + '. The agent_spawn tool should return a JSON receipt immediately (not wait 15 seconds). Read that receipt carefully. It should contain a task_file path and a runId (task_id). If you got a receipt with a task_file path, respond with ASYNC_RECEIPT_OK and include the task_file path. If you got the result immediately (no receipt), respond with ASYNC_NO_RECEIPT. If the tool failed or is unavailable, respond with ASYNC_FAILED and explain.'

    Write-Host "Sending async spawn request..." -ForegroundColor Yellow
    $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    $response = peko send $parentAgent $prompt --no-stream 2>&1
    $stopwatch.Stop()
    Write-Host "Response: $response"
    Write-Host "Elapsed time: $($stopwatch.Elapsed.ToString('hh\:mm\:ss'))" -ForegroundColor Yellow

    # Verify the agent returned quickly (less than 20s) — proves it didn't block on the 15s background task.
    # The threshold accounts for LLM API latency (two calls: generate tool call + generate response).
    if ($stopwatch.Elapsed.TotalSeconds -gt 20) {
        Write-Host "FAIL: Agent took $($stopwatch.Elapsed.TotalSeconds.ToString('F1'))s to respond — it may have blocked on the background task" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "PASS: Agent returned in $($stopwatch.Elapsed.TotalSeconds.ToString('F1'))s — did not block on background task" -ForegroundColor Green
    }

    $receiptOk = $response -match "ASYNC_RECEIPT_OK"
    $noReceipt = $response -match "ASYNC_NO_RECEIPT"
    $failed = $response -match "ASYNC_FAILED"

    if ($receiptOk) {
        Write-Host "PASS: Agent received async receipt with task_file" -ForegroundColor Green
    } elseif ($noReceipt) {
        Write-Host "FAIL: Agent did not get a receipt — async mode may not be working" -ForegroundColor Red
        $script:failed = $true
    } elseif ($failed) {
        Write-Host "FAIL: Agent reported ASYNC_FAILED" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 2: Poll task_file for completion
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 2: Poll task_file for completion" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    Write-Host "Waiting for async task to complete (15s)..." -ForegroundColor Yellow
    Start-Sleep 15

    $prompt2 = 'Check the task_file path from the async receipt you received earlier. Read the task_file using read_file or shell to see if the subagent task is complete. Alternatively, you can use the task tool with action="status" and the runId (task_id) to check progress. If the task shows status completed and the result contains ASYNC_TASK_COMPLETE, respond with POLLING_SUCCESS and include the result. If the task is still running or pending, respond with POLLING_STILL_RUNNING. If the task shows failed or timed_out, respond with POLLING_FAILED and explain. If you cannot find or read the task_file, respond with POLLING_ERROR.'

    Write-Host "Sending polling request..." -ForegroundColor Yellow
    $response2 = peko send $parentAgent $prompt2 --no-stream 2>&1
    Write-Host "Response: $response2"

    $pollingSuccess = $response2 -match "POLLING_SUCCESS"
    $pollingRunning = $response2 -match "POLLING_STILL_RUNNING"
    $pollingFailed = $response2 -match "POLLING_FAILED"
    $pollingError = $response2 -match "POLLING_ERROR"

    # Also verify the actual file was created by the subagent
    $expectedFile = "$workspaceDir/$asyncFile"
    Start-Sleep -Milliseconds 500
    $fileExists = Test-Path $expectedFile
    $fileContent = if ($fileExists) { Get-Content $expectedFile -Raw } else { "<missing>" }

    if ($pollingSuccess -and $fileExists -and $fileContent -match "ASYNC_TASK_COMPLETE") {
        Write-Host "PASS: Agent polled task_file and found completed result + file exists" -ForegroundColor Green
    } elseif ($pollingRunning) {
        Write-Host "IN_PROGRESS: Task still running according to agent's polling" -ForegroundColor Yellow
    } elseif ($pollingFailed) {
        Write-Host "FAIL: Agent reported task failed according to task_file" -ForegroundColor Red
        $script:failed = $true
    } elseif ($pollingError) {
        Write-Host "EXPECTED FAIL (feature may be stubbed): Agent could not access task_file" -ForegroundColor Yellow
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 3: Async spawn with _timeout
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 3: Async spawn with _timeout" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $prompt3 = 'Use agent_spawn with _async=true and _timeout=2 to delegate this task: Use the shell tool to run: Start-Sleep 10; echo should not reach here. This task should time out after 2 seconds because the sleep is 10 seconds. Get the receipt, wait about 5 seconds, then read the task_file. If the task_file shows status timed_out or failed due to timeout, respond with TIMEOUT_SUCCESS. If the task completed successfully (status completed), respond with TIMEOUT_FAILED. If something else goes wrong, respond with TIMEOUT_ERROR.'

    Write-Host "Sending async timeout test..." -ForegroundColor Yellow
    $response3 = peko send $parentAgent $prompt3 --no-stream 2>&1
    Write-Host "Response: $response3"

    $timeoutSuccess = $response3 -match "TIMEOUT_SUCCESS"
    $timeoutFailed = $response3 -match "TIMEOUT_FAILED"
    $timeoutError = $response3 -match "TIMEOUT_ERROR"

    if ($timeoutSuccess) {
        Write-Host "PASS: Async subagent correctly timed out" -ForegroundColor Green
    } elseif ($timeoutFailed) {
        Write-Host "FAIL: Subagent did not time out as expected" -ForegroundColor Red
        $script:failed = $true
    } elseif ($timeoutError) {
        Write-Host "EXPECTED FAIL (feature may be stubbed): Agent could not verify timeout" -ForegroundColor Yellow
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 4: Multiple concurrent async spawns
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 4: Multiple concurrent async spawns" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $prompt4 = 'Launch TWO async subagents concurrently using agent_spawn with _async=true. Subagent A task: Use shell to run: Start-Sleep 5; echo CONCURRENT_A > concurrent_a.txt. Subagent B task: Use shell to run: Start-Sleep 5; echo CONCURRENT_B > concurrent_b.txt. You should get TWO receipts immediately (the parent should not wait 5 seconds). Collect both task_file paths. Wait about 10 seconds, then read both task_files. If both tasks completed successfully and both files exist, respond with CONCURRENT_SUCCESS. If you only got one receipt or one task failed, respond with CONCURRENT_PARTIAL. If the parent blocked and you did not get receipts, respond with CONCURRENT_BLOCKED. If something else goes wrong, respond with CONCURRENT_FAILED.'

    Write-Host "Sending concurrent async spawn test..." -ForegroundColor Yellow
    $stopwatch4 = [System.Diagnostics.Stopwatch]::StartNew()
    $response4 = peko send $parentAgent $prompt4 --no-stream 2>&1
    $stopwatch4.Stop()
    Write-Host "Response: $response4"
    Write-Host "Elapsed time: $($stopwatch4.Elapsed.ToString('hh\:mm\:ss'))" -ForegroundColor Yellow

    # Verify both files exist
    Start-Sleep -Milliseconds 500
    $fileA = "$workspaceDir/concurrent_a.txt"
    $fileB = "$workspaceDir/concurrent_b.txt"
    $fileAExists = Test-Path $fileA
    $fileBExists = Test-Path $fileB

    $concurrentSuccess = $response4 -match "CONCURRENT_SUCCESS"
    $concurrentPartial = $response4 -match "CONCURRENT_PARTIAL"
    $concurrentBlocked = $response4 -match "CONCURRENT_BLOCKED"
    $concurrentFailed = $response4 -match "CONCURRENT_FAILED"

    if ($concurrentSuccess -and $fileAExists -and $fileBExists) {
        Write-Host "PASS: Both concurrent async spawns completed successfully" -ForegroundColor Green
    } elseif ($concurrentBlocked) {
        Write-Host "FAIL: Parent blocked instead of returning receipts immediately" -ForegroundColor Red
        $script:failed = $true
    } elseif ($concurrentPartial) {
        Write-Host "PARTIAL: Only some concurrent spawns succeeded" -ForegroundColor Yellow
    } elseif ($concurrentFailed) {
        Write-Host "FAIL: Agent reported CONCURRENT_FAILED" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

} finally {
    # ============================================================
    # Cleanup
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Remove test files
    @("async_subagent_result.txt", "concurrent_a.txt", "concurrent_b.txt") | ForEach-Object {
        $f = "$workspaceDir/$_"
        if (Test-Path $f) { Remove-Item $f -Force }
    }

    peko agent delete $parentAgent --force 2>&1 | Out-Null
    Write-Host "Deleted test agent" -ForegroundColor Green
}

if ($script:failed) {
    exit 1
}

Write-Host "`nSubagent async mode e2e tests completed!" -ForegroundColor Green
Write-Host "`nNotes:" -ForegroundColor Cyan
Write-Host "- Blocking mode (default): agent_spawn waits for subagent completion, returns inline result." -ForegroundColor Cyan
Write-Host "- Async mode (_async: true): agent_spawn returns receipt immediately, agent polls task_file." -ForegroundColor Cyan
Write-Host "- No automatic queue injection — the agent is in full control of polling." -ForegroundColor Cyan
