#!/usr/bin/env pwsh
# Universal Tool _async Reserved Parameter E2E Test
#
# Tests that a universal tool (slow_calculator) can be executed asynchronously
# via the _async reserved parameter. The tool sleeps for N seconds, so when
# _async: true is passed, the agent should receive an immediate receipt instead
# of blocking.
#
# Design: ADR-018a / Issue 012
#
# Expected behavior:
# 1. Agent calls slow_calculator with _async: true
# 2. Tool returns a receipt immediately containing task_id and task_file
# 3. Agent polls task_file and eventually sees the completed result
# 4. Sync execution (no _async) blocks and returns the actual result

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Universal Tool _async E2E Test (Python)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Error "MINIMAX_API_KEY environment variable not set"
    exit 1
}

# Check Python
$pythonCmd = if (Get-Command "python3" -ErrorAction SilentlyContinue) { "python3" } elseif (Get-Command "python" -ErrorAction SilentlyContinue) { "python" } else { $null }
if (-not $pythonCmd) {
    Write-Error "Python not found in PATH"
    exit 1
}
Write-Host "Using Python: $pythonCmd" -ForegroundColor Green

# Build peko
Write-Host "Building peko..." -ForegroundColor Cyan
pushd "$PSScriptRoot/../../../../"
$env:RUSTFLAGS = "-A warnings"
cargo build --quiet
if ($LASTEXITCODE -ne 0) {
    Write-Error "Build failed"
    exit 1
}
popd

# Reset peko config data
$pekoDir = "$env:USERPROFILE/.peko"
if (Test-Path $pekoDir) {
    Remove-Item -Recurse -Force $pekoDir
    Write-Host "Reset .peko directory" -ForegroundColor Yellow
}

# Reset peko data
$dataDir = "$env:USERPROFILE/AppData/Roaming/peko"
if (Test-Path $dataDir) {
    Remove-Item -Recurse -Force $dataDir
    Write-Host "Reset data directory" -ForegroundColor Yellow
}

# Set API key
peko auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# ============================================================
# STEP 1: Create agent
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 1: Create agent" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$agentName = "async_calc_agent"

Write-Host "Creating agent: $agentName" -ForegroundColor Yellow
peko agent create $agentName --provider $Provider --force 2>&1 | Out-Null
Write-Host "Created agent" -ForegroundColor Green

# ============================================================
# STEP 2: Install tool as extension
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 2: Install tool as extension" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$toolDir = "$PSScriptRoot"
Write-Host "Installing slow_calculator as universal-tool extension..." -ForegroundColor Yellow

$installResult = peko ext install $toolDir --type universal-tool 2>&1
Write-Host $installResult

# Verify installation
$extList = peko ext list --type universal-tool 2>&1
if ($extList -match "slow_calculator") {
    Write-Host "Tool extension installed successfully" -ForegroundColor Green
} else {
    Write-Error "Tool extension installation failed"
}

# ============================================================
# STEP 3: Enable tool extension
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 3: Enable tool extension" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Enabling slow_calculator extension..." -ForegroundColor Yellow
peko ext enable slow_calculator --target default/$agentName 2>&1 | Out-Null
Write-Host "Enabled tool extension" -ForegroundColor Green

# Verify
$extInfo = peko ext info slow_calculator 2>&1
Write-Host "`nExtension status:" -ForegroundColor Cyan
Write-Host $extInfo

# Ensure cleanup runs even if tests fail
$script:failed = $false

try {
    # ============================================================
    # TEST 1: Sync execution blocks and returns result
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 1: Sync execution returns result directly" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $syncPrompt = @"
Use the slow_calculator tool to calculate 10 plus 20 with delay_seconds=3. Do NOT use _async. Respond with SYNC_RESULT followed by the result number you received from the tool. If the tool fails or you don't get a numeric result, respond with SYNC_FAILED and explain.
"@

    Write-Host "Sending sync request (should block ~3s)..." -ForegroundColor Yellow
    $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    $response = peko send $agentName $syncPrompt --no-stream 2>&1
    $stopwatch.Stop()
    Write-Host "Response: $response"
    Write-Host "Elapsed time: $($stopwatch.Elapsed.ToString('hh\:mm\:ss'))" -ForegroundColor Yellow

    $syncResult = $response -match "SYNC_RESULT"
    $syncFailed = $response -match "SYNC_FAILED"

    if ($syncResult -and $response -match "30") {
        Write-Host "PASS: Sync execution returned correct result (30)" -ForegroundColor Green
    } elseif ($syncFailed) {
        Write-Host "FAIL: Sync execution failed" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # Verify it actually blocked (took at least 2 seconds)
    if ($stopwatch.Elapsed.TotalSeconds -lt 2) {
        Write-Host "FAIL: Sync execution returned too quickly — may not have actually executed" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "PASS: Sync execution blocked for ~$($stopwatch.Elapsed.TotalSeconds.ToString('F1'))s as expected" -ForegroundColor Green
    }

    # ============================================================
    # TEST 2: Async execution returns receipt immediately
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 2: Async execution returns receipt immediately" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $asyncPrompt = @"
Use the slow_calculator tool to calculate 7 multiplied by 8 with delay_seconds=5. Include `_async: true` in the tool parameters. The tool should return a JSON receipt immediately instead of the calculation result. Read the receipt carefully. If the receipt contains _async_status, task_id, and task_file, respond with ASYNC_RECEIPT_OK followed by the task_id. If you get the actual calculation result immediately instead of a receipt, respond with ASYNC_NO_RECEIPT. If something else goes wrong, respond with ASYNC_FAILED and explain.
"@

    Write-Host "Sending async request (should return immediately)..." -ForegroundColor Yellow
    $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    $response = peko send $agentName $asyncPrompt --no-stream 2>&1
    $stopwatch.Stop()
    Write-Host "Response: $response"
    Write-Host "Elapsed time: $($stopwatch.Elapsed.ToString('hh\:mm\:ss'))" -ForegroundColor Yellow

    $asyncReceiptOk = $response -match "ASYNC_RECEIPT_OK"
    $asyncNoReceipt = $response -match "ASYNC_NO_RECEIPT"
    $asyncFailed = $response -match "ASYNC_FAILED"

    if ($asyncReceiptOk) {
        Write-Host "PASS: Agent received async receipt" -ForegroundColor Green
    } elseif ($asyncNoReceipt) {
        Write-Host "FAIL: Agent did not receive an async receipt (got result directly)" -ForegroundColor Red
        $script:failed = $true
    } elseif ($asyncFailed) {
        Write-Host "FAIL: Agent reported ASYNC_FAILED" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # Verify the agent returned quickly (less than 20s) — proves it didn't block on the 5s background task
    if ($stopwatch.Elapsed.TotalSeconds -gt 20) {
        Write-Host "FAIL: Agent took $($stopwatch.Elapsed.TotalSeconds.ToString('F1'))s to respond — it may have blocked on the background task" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "PASS: Agent returned in $($stopwatch.Elapsed.TotalSeconds.ToString('F1'))s — did not block on background task" -ForegroundColor Green
    }

    # ============================================================
    # TEST 3: Poll task_file for async result
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 3: Poll task_file for completed async result" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    Write-Host "Waiting for async task to complete (5s)..." -ForegroundColor Yellow
    Start-Sleep 5

    $pollPrompt = @"
You previously ran slow_calculator with _async: true to calculate 7 multiplied by 8 with delay_seconds=5. Check the task_file from that receipt. If the task is complete and the result shows 56, respond with POLL_RESULT_56. If the task is still running, respond with POLL_STILL_RUNNING. If you cannot find the task_file or the result is wrong, respond with POLL_FAILED and explain.
"@

    Write-Host "Sending polling request..." -ForegroundColor Yellow
    $response = peko send $agentName $pollPrompt --no-stream 2>&1
    Write-Host "Response: $response"

    $pollResult56 = $response -match "POLL_RESULT_56"
    $pollStillRunning = $response -match "POLL_STILL_RUNNING"
    $pollFailed = $response -match "POLL_FAILED"

    if ($pollResult56) {
        Write-Host "PASS: Agent polled task_file and found correct result (56)" -ForegroundColor Green
    } elseif ($pollStillRunning) {
        Write-Host "Task still running — may need more time" -ForegroundColor Yellow
    } elseif ($pollFailed) {
        Write-Host "FAIL: Agent reported POLL_FAILED" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 4: Async with custom _timeout
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 4: Async with custom _timeout" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $timeoutPrompt = @"
Use the slow_calculator tool to calculate 100 divided by 4 with delay_seconds=3. Include `_async: true` and `_timeout: 600` in the tool parameters. You should get a receipt. Check the receipt for timeout_requested. If it shows timeout_requested=600, respond with TIMEOUT_SET_600. If the timeout is different, respond with TIMEOUT_WRONG followed by the value you saw. If something else goes wrong, respond with TIMEOUT_FAILED and explain.
"@

    Write-Host "Sending async request with custom timeout..." -ForegroundColor Yellow
    $response = peko send $agentName $timeoutPrompt --no-stream 2>&1
    Write-Host "Response: $response"

    $timeoutSet600 = $response -match "TIMEOUT_SET_600"
    $timeoutWrong = $response -match "TIMEOUT_WRONG"
    $timeoutFailed = $response -match "TIMEOUT_FAILED"

    if ($timeoutSet600) {
        Write-Host "PASS: Custom timeout (600s) was correctly set in receipt" -ForegroundColor Green
    } elseif ($timeoutWrong) {
        Write-Host "FAIL: Timeout value in receipt did not match expected 600s" -ForegroundColor Red
        $script:failed = $true
    } elseif ($timeoutFailed) {
        Write-Host "FAIL: Agent reported TIMEOUT_FAILED" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # Wait for the timeout test task to finish before cleanup
    Write-Host "Waiting for timeout test task to complete (3s)..." -ForegroundColor Yellow
    Start-Sleep 3

} finally {
    # ============================================================
    # Cleanup
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Uninstall tool extension
    peko ext uninstall slow_calculator 2>&1 | Out-Null
    Write-Host "Uninstalled tool extension" -ForegroundColor Green

    # Delete agent
    peko agent delete $agentName --force 2>&1 | Out-Null
    Write-Host "Deleted agent" -ForegroundColor Green
}

if ($script:failed) {
    exit 1
}

Write-Host "`nUniversal Tool _async reserved parameter E2E tests completed!" -ForegroundColor Green
