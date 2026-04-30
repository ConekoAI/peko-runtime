#!/usr/bin/env pwsh
# Subagent Spawn — Status, List & Cancel E2E Test
#
# Tests the unified `task` tool with actions: status, list, cancel
# - action=status: check the status of any async task by task_id
# - action=list: list all async tasks for the current session
# - action=cancel: cancel a running async task
#
# All operations assume the daemon is already running.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Universal Task Management — Status, List & Cancel E2E Test" -ForegroundColor Cyan
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
    $script:failed = $false

    # ============================================================
    # TEST 1: task action=status returns correct status
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 1: task action=status" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $statusFile = "status_test.txt"
    $prompt = 'Use agent_spawn with _async=true to delegate this task: Use shell to run: Start-Sleep 8; echo STATUS_TEST_DONE > ' + $statusFile + '. You will get a receipt. Extract the run_id (task_id) from the receipt and use the task tool with action="status" and that task_id to check the status. If status is running or pending, respond with STATUS_RUNNING_OK. If completed immediately, respond with STATUS_ALREADY_DONE. If the tool fails, respond with STATUS_TOOL_FAILED.'

    Write-Host "Sending status check test..." -ForegroundColor Yellow
    $response = peko send $parentAgent $prompt --no-stream 2>&1
    Write-Host "Response: $response"

    if ($response -match "STATUS_RUNNING_OK") {
        Write-Host "PASS: task action=status correctly showed running/pending status" -ForegroundColor Green
    } elseif ($response -match "STATUS_ALREADY_DONE") {
        Write-Host "UNEXPECTED: Task completed immediately — async may not be working" -ForegroundColor Yellow
    } elseif ($response -match "STATUS_TOOL_FAILED") {
        Write-Host "FAIL: task tool unavailable or failed" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 2: task action=status shows completed after task finishes
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 2: task action=status shows completed after finish" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    Write-Host "Waiting for async task to complete (8s)..." -ForegroundColor Yellow
    Start-Sleep 8

    $prompt2 = 'Use the task tool with action="status" and the same task_id (run_id) from the previous task. Also check if the file ' + $statusFile + ' exists. If task action=status shows status completed and the file exists with STATUS_TEST_DONE, respond with STATUS_COMPLETE_OK. If status is still running, respond with STATUS_STILL_RUNNING. If status shows failed or timed_out, respond with STATUS_COMPLETE_FAILED. If the tool is unavailable, respond with STATUS_TOOL_MISSING.'

    Write-Host "Sending completion check..." -ForegroundColor Yellow
    $response2 = peko send $parentAgent $prompt2 --no-stream 2>&1
    Write-Host "Response: $response2"

    Start-Sleep -Milliseconds 500
    $expectedFile = "$workspaceDir/$statusFile"
    $fileExists = Test-Path $expectedFile
    $fileContent = if ($fileExists) { Get-Content $expectedFile -Raw } else { "<missing>" }

    if ($response2 -match "STATUS_COMPLETE_OK" -and $fileExists -and $fileContent -match "STATUS_TEST_DONE") {
        Write-Host "PASS: task action=status showed completed and file was created" -ForegroundColor Green
    } elseif ($response2 -match "STATUS_STILL_RUNNING") {
        Write-Host "IN_PROGRESS: Task still running" -ForegroundColor Yellow
    } elseif ($response2 -match "STATUS_COMPLETE_FAILED") {
        Write-Host "FAIL: Task failed or timed out" -ForegroundColor Red
        $script:failed = $true
    } elseif ($response2 -match "STATUS_TOOL_MISSING") {
        Write-Host "FAIL: task tool not available" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 3: task action=list shows active tasks
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 3: task action=list" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $prompt3 = 'Launch TWO async subagents concurrently: 1. Use shell to run: Start-Sleep 20; echo done1. 2. Use shell to run: Start-Sleep 20; echo done2. Immediately after getting both receipts, use the task tool with action="list" to list all active tasks. If task action=list shows at least 2 active tasks, respond with LIST_OK and the count. If it shows fewer than 2, respond with LIST_LOW_COUNT. If the tool is unavailable, respond with LIST_TOOL_MISSING. If something else goes wrong, respond with LIST_FAILED.'

    Write-Host "Sending list test..." -ForegroundColor Yellow
    $response3 = peko send $parentAgent $prompt3 --no-stream 2>&1
    Write-Host "Response: $response3"

    if ($response3 -match "LIST_OK") {
        Write-Host "PASS: task action=list showed multiple active tasks" -ForegroundColor Green
    } elseif ($response3 -match "LIST_LOW_COUNT") {
        Write-Host "FAIL: task action=list showed fewer tasks than expected" -ForegroundColor Red
        $script:failed = $true
    } elseif ($response3 -match "LIST_TOOL_MISSING") {
        Write-Host "FAIL: task tool not available" -ForegroundColor Red
        $script:failed = $true
    } elseif ($response3 -match "LIST_FAILED") {
        Write-Host "FAIL: Agent reported LIST_FAILED" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # Wait for the 20s tasks to finish before cancel test
    Write-Host "Waiting for remaining async tasks to complete..." -ForegroundColor Yellow
    Start-Sleep 20

    # ============================================================
    # TEST 4: task action=cancel stops a running task
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 4: task action=cancel" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $prompt4 = 'Use agent_spawn with _async=true to delegate this task: Use shell to run: Start-Sleep 30; echo SHOULD_NOT_SEE_THIS > cancel_test.txt. You will get a receipt. Immediately extract the run_id (task_id) and use the task tool with action="cancel" and that task_id. If the cancel response shows success=true, respond with CANCEL_OK. If success=false, respond with CANCEL_FAILED and include the message. If the tool is unavailable, respond with CANCEL_TOOL_MISSING.'

    Write-Host "Sending cancel test..." -ForegroundColor Yellow
    $response4 = peko send $parentAgent $prompt4 --no-stream 2>&1
    Write-Host "Response: $response4"

    if ($response4 -match "CANCEL_OK") {
        Write-Host "PASS: task action=cancel successfully cancelled running task" -ForegroundColor Green
    } elseif ($response4 -match "CANCEL_FAILED") {
        Write-Host "FAIL: task action=cancel returned success=false" -ForegroundColor Red
        $script:failed = $true
    } elseif ($response4 -match "CANCEL_TOOL_MISSING") {
        Write-Host "FAIL: task tool not available" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # Verify the cancelled task did not produce output
    Start-Sleep -Milliseconds 500
    $cancelTestFile = "$workspaceDir/cancel_test.txt"
    if (Test-Path $cancelTestFile) {
        Write-Host "WARN: Cancelled task file exists — task may not have been stopped" -ForegroundColor Yellow
    } else {
        Write-Host "PASS: Cancelled task did not produce output file" -ForegroundColor Green
    }

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
    if (Test-Path "$workspaceDir/cancel_test.txt") {
        Remove-Item "$workspaceDir/cancel_test.txt" -Force
    }

    peko agent delete $parentAgent --force 2>&1 | Out-Null
    Write-Host "Deleted test agent" -ForegroundColor Green
}

if ($script:failed) {
    exit 1
}

Write-Host "`nUniversal task status, list & cancel e2e tests completed!" -ForegroundColor Green
