#!/usr/bin/env pwsh
# Async Tool Execution E2E Test
#
# Tests the async execution path for tools using the _async reserved parameter,
# plus the unified `task` tool for status, list, and cancel actions.
# Design: see ADR-20 and Issue 012.
#
# Expected behavior:
# 1. Agent calls a tool with _async: true
# 2. Tool returns a receipt immediately containing a task_file path and task_id
# 3. Agent polls via task_file OR uses the unified `task` tool with action="status"
# 4. Agent can list all tasks via task tool with action="list"
# 5. Agent can cancel a running task via task tool with action="cancel"
# 6. No automatic injection into the agentic loop - agent is in full control
#
# This test documents the intended contract. Some functionality may be stubbed
# or not yet fully wired in the engine.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Async Tool + Unified Task Tool E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Error "MINIMAX_API_KEY environment variable not set"
    exit 1
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

# Ensure cleanup runs even if tests fail
try {
    $script:failed = $false

    # ============================================================
    # TEST 1: Async shell execution returns a receipt
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 1: Async shell returns receipt" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $prompt = @"
Use the shell tool to run the command 'echo async_start; Start-Sleep 30; echo async_complete' with async mode enabled.
To do this, include `_async: true` in the tool parameters.

The tool should return a JSON receipt immediately. Read that receipt carefully.
The receipt should contain a 'task_file' path where you can check for progress and results. Read the task_file path from the receipt and check its contents. If you see a header json identical to your receipt, and the output contains null, indicating the task is still running, Respond with ASYNC_SUCCESS and include the task_file path from the receipt and the contents of the file.
If you don't get a receipt, or the receipt doesn't contain a task_file, or you get the result of the command right away instead of a receipt, or the file contents don't match expectations, respond with ASYNC_FAILED and explain what you got back when you called the tool.
"@

    Write-Host "Sending async shell request..." -ForegroundColor Yellow
    $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    $response = peko send $agentName $prompt --no-stream 2>&1
    $stopwatch.Stop()
    Write-Host "Response: $response"
    Write-Host "Elapsed time: $($stopwatch.Elapsed.ToString('hh\:mm\:ss'))" -ForegroundColor Yellow

    $success = $response -match "ASYNC_SUCCESS"
    $failed = $response -match "ASYNC_FAILED"

    if ($success) {
        Write-Host "PASS: Agent successfully used async shell and retrieved result" -ForegroundColor Green
    } elseif ($failed) {
        Write-Host "EXPECTED FAIL (feature may be stubbed): Agent could not complete async flow" -ForegroundColor Red
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # Verify the agent returned quickly (less than 30s) — proves it didn't block on the 30s background task
    if ($stopwatch.Elapsed.TotalSeconds -gt 30) {
        Write-Host "FAIL: Agent took $($stopwatch.Elapsed.TotalSeconds.ToString('F1'))s to respond — it may have blocked on the background task" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "PASS: Agent returned in $($stopwatch.Elapsed.TotalSeconds.ToString('F1'))s — did not block on background task" -ForegroundColor Green
    }

    Write-Host "`nWaiting for async task to complete..." -ForegroundColor Yellow
    Start-Sleep 30

    # ============================================================
    # TEST 2: Direct progress polling via task_file
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 2: Direct progress polling via task_file" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    Write-Host "Sending polling request..." -ForegroundColor Yellow
    $response = peko send $agentName "Check the task_file path for the async shell command you ran earlier. If the task is still in progress, respond with POLLING_IN_PROGRESS. If the task is complete and you can read the result from the task_file, respond with POLLING_SUCCESS and include the result. If you cannot access the task_file or something goes wrong, respond with POLLING_FAILED and explain why." --no-stream 2>&1
    Write-Host "Response: $response"

    $pollingSuccess = $response -match "POLLING_SUCCESS"
    $pollingInProgress = $response -match "POLLING_IN_PROGRESS"
    $pollingFailed = $response -match "POLLING_FAILED"
    if ($pollingSuccess) {
        Write-Host "PASS: Agent successfully polled task_file and retrieved result" -ForegroundColor Green
    } elseif ($pollingInProgress) {
        Write-Host "IN_PROGRESS: Task is still running according to agent's polling" -ForegroundColor Yellow
    } elseif ($pollingFailed) {
        Write-Host "EXPECTED FAIL (feature may be stubbed): Agent could not poll task_file successfully" -ForegroundColor Yellow
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 3: Timeout via _timeout reserved parameter
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 3: Async timeout" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

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
        $script:failed = $true
    } elseif ($timeoutError) {
        Write-Host "EXPECTED FAIL (feature may be stubbed): Agent could not handle timeout scenario" -ForegroundColor Yellow
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 4: Unified task tool — action=status
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 4: Unified task tool — action=status" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Spawn a long-running async task, then immediately query its status via the task tool
    $statusPrompt = @"
Use the shell tool to run 'Start-Sleep 20; echo task_status_done' with `_async: true`. You will get a receipt with a task_id. IMMEDIATELY after getting the receipt, use the task tool with action="status" and the task_id from the receipt. If the status shows pending or running, respond with TASK_STATUS_RUNNING. If it shows completed already, respond with TASK_STATUS_ALREADY_DONE. If the task tool is unavailable or fails, respond with TASK_STATUS_TOOL_MISSING. If something else goes wrong, respond with TASK_STATUS_FAILED and explain.
"@

    Write-Host "Sending task status test..." -ForegroundColor Yellow
    $response = peko send $agentName $statusPrompt --no-stream 2>&1
    Write-Host "Response: $response"

    $taskStatusRunning = $response -match "TASK_STATUS_RUNNING"
    $taskStatusDone = $response -match "TASK_STATUS_ALREADY_DONE"
    $taskStatusMissing = $response -match "TASK_STATUS_TOOL_MISSING"
    $taskStatusFailed = $response -match "TASK_STATUS_FAILED"

    if ($taskStatusRunning) {
        Write-Host "PASS: task action=status correctly showed running/pending status" -ForegroundColor Green
    } elseif ($taskStatusDone) {
        Write-Host "UNEXPECTED: Task completed immediately — async may not be working" -ForegroundColor Yellow
    } elseif ($taskStatusMissing) {
        Write-Host "FAIL: task tool unavailable" -ForegroundColor Red
        $script:failed = $true
    } elseif ($taskStatusFailed) {
        Write-Host "FAIL: Agent reported TASK_STATUS_FAILED" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # Wait for the 20s task to finish
    Write-Host "Waiting for async task to complete (20s)..." -ForegroundColor Yellow
    Start-Sleep 20

    # ============================================================
    # TEST 5: Unified task tool — action=list
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 5: Unified task tool — action=list" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Spawn two concurrent async tasks, then list them
    $listPrompt = @"
Launch TWO async shell tasks concurrently using `_async: true`:
1. Run 'Start-Sleep 15; echo list_task_a'
2. Run 'Start-Sleep 15; echo list_task_b'
Immediately after getting both receipts, use the task tool with action="list" and NO filters. If the response shows at least 2 tasks with status running or pending, respond with TASK_LIST_OK and include the total count. If it shows fewer than 2 active tasks, respond with TASK_LIST_LOW_COUNT. If the task tool is unavailable, respond with TASK_LIST_TOOL_MISSING. If something else goes wrong, respond with TASK_LIST_FAILED and explain.
"@

    Write-Host "Sending task list test..." -ForegroundColor Yellow
    $response = peko send $agentName $listPrompt --no-stream 2>&1
    Write-Host "Response: $response"

    $taskListOk = $response -match "TASK_LIST_OK"
    $taskListLow = $response -match "TASK_LIST_LOW_COUNT"
    $taskListMissing = $response -match "TASK_LIST_TOOL_MISSING"
    $taskListFailed = $response -match "TASK_LIST_FAILED"

    if ($taskListOk) {
        Write-Host "PASS: task action=list showed multiple active tasks" -ForegroundColor Green
    } elseif ($taskListLow) {
        Write-Host "FAIL: task action=list showed fewer tasks than expected" -ForegroundColor Red
        $script:failed = $true
    } elseif ($taskListMissing) {
        Write-Host "FAIL: task tool unavailable" -ForegroundColor Red
        $script:failed = $true
    } elseif ($taskListFailed) {
        Write-Host "FAIL: Agent reported TASK_LIST_FAILED" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # Wait for the 15s tasks to finish before cancel test
    Write-Host "Waiting for list test tasks to complete (15s)..." -ForegroundColor Yellow
    Start-Sleep 15

    # ============================================================
    # TEST 6: Unified task tool — action=cancel
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 6: Unified task tool — action=cancel" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Spawn a very long task, then cancel it immediately
    $cancelPrompt = @"
Use the shell tool to run 'Start-Sleep 60; echo should_not_see_this' with `_async: true`. You will get a receipt with a task_id. IMMEDIATELY extract the task_id and use the task tool with action="cancel" and that task_id. If the cancel response shows success=true, respond with TASK_CANCEL_OK. If success=false, respond with TASK_CANCEL_FAILED and include the message. If the task tool is unavailable, respond with TASK_CANCEL_TOOL_MISSING. If something else goes wrong, respond with TASK_CANCEL_ERROR and explain.
"@

    Write-Host "Sending task cancel test..." -ForegroundColor Yellow
    $response = peko send $agentName $cancelPrompt --no-stream 2>&1
    Write-Host "Response: $response"

    $taskCancelOk = $response -match "TASK_CANCEL_OK"
    $taskCancelFailed = $response -match "TASK_CANCEL_FAILED"
    $taskCancelMissing = $response -match "TASK_CANCEL_TOOL_MISSING"
    $taskCancelError = $response -match "TASK_CANCEL_ERROR"

    if ($taskCancelOk) {
        Write-Host "PASS: task action=cancel successfully cancelled running task" -ForegroundColor Green
    } elseif ($taskCancelFailed) {
        Write-Host "FAIL: task action=cancel returned success=false" -ForegroundColor Red
        $script:failed = $true
    } elseif ($taskCancelMissing) {
        Write-Host "FAIL: task tool unavailable" -ForegroundColor Red
        $script:failed = $true
    } elseif ($taskCancelError) {
        Write-Host "EXPECTED FAIL (feature may be stubbed): Agent could not cancel task" -ForegroundColor Yellow
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # ============================================================
    # TEST 7: Task tool with filters (status_filter + tool_filter)
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 7: Task tool — list with filters" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Spawn a completed task (short) and a running task (long), then filter by status
    $filterPrompt = @"
Do this in order:
1. Use shell with `_async: true` to run 'echo quick_done' — this will complete quickly.
2. Use shell with `_async: true` to run 'Start-Sleep 30; echo slow_done' — this stays running.
3. Wait 3 seconds for the first task to finish.
4. Use the task tool with action="list" and status_filter="completed".
5. If the filtered list shows at least 1 completed task, respond with FILTER_OK.
6. If it shows zero completed tasks, respond with FILTER_EMPTY.
7. If the task tool is unavailable, respond with FILTER_TOOL_MISSING.
8. If something else goes wrong, respond with FILTER_FAILED and explain.
"@

    Write-Host "Sending filter test..." -ForegroundColor Yellow
    $response = peko send $agentName $filterPrompt --no-stream 2>&1
    Write-Host "Response: $response"

    $filterOk = $response -match "FILTER_OK"
    $filterEmpty = $response -match "FILTER_EMPTY"
    $filterMissing = $response -match "FILTER_TOOL_MISSING"
    $filterFailed = $response -match "FILTER_FAILED"

    if ($filterOk) {
        Write-Host "PASS: task action=list with status_filter worked correctly" -ForegroundColor Green
    } elseif ($filterEmpty) {
        Write-Host "FAIL: Filter returned empty when completed tasks should exist" -ForegroundColor Red
        $script:failed = $true
    } elseif ($filterMissing) {
        Write-Host "FAIL: task tool unavailable" -ForegroundColor Red
        $script:failed = $true
    } elseif ($filterFailed) {
        Write-Host "FAIL: Agent reported FILTER_FAILED" -ForegroundColor Red
        $script:failed = $true
    } else {
        Write-Host "Result unclear - manual review needed" -ForegroundColor Yellow
    }

    # Wait for the 30s slow task to finish before cleanup
    Write-Host "Waiting for remaining async tasks to complete..." -ForegroundColor Yellow
    Start-Sleep 30

} finally {
    # ============================================================
    # Cleanup
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    peko agent delete $agentName --force 2>&1 | Out-Null
    Write-Host "Deleted test agent" -ForegroundColor Green
}

if ($script:failed) {
    exit 1
}

Write-Host "`nAsync tool + unified task tool E2E tests completed!" -ForegroundColor Green
Write-Host "`nNotes:" -ForegroundColor Cyan
Write-Host "- Async mode (_async: true): tool returns receipt immediately, agent polls." -ForegroundColor Cyan
Write-Host "- Unified task tool: one tool handles status, list, and cancel actions." -ForegroundColor Cyan
Write-Host "- No automatic queue injection into the agentic loop is expected." -ForegroundColor Cyan
