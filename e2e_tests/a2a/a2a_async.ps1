#!/usr/bin/env pwsh
# A2A Async Send E2E Test
#
# Tests the a2a_send tool with _async=true for asynchronous agent-to-agent messaging.
# Following the deterministic pattern from e2e_tests/extensions/tools/:
# - Prompts instruct the LLM to reply with exact keywords
# - Structural verification (task files, session list) confirms side effects
#
# Scenario:
#   1. async_delegator uses a2a_send with _async=true to message async_worker
#   2. Tool returns an async receipt immediately
#   3. Task runs in background
#   4. async_worker session is created
#   5. Task file can be polled for completion

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "A2A Async Send E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Error "MINIMAX_API_KEY environment variable not set"
    exit 1
}

# Build peko (skip if daemon is running since it locks the binary)
$daemonRunning = $false
try {
    $status = peko daemon status 2>&1
    if ($status -match "Running") { $daemonRunning = $true }
} catch {}

if (-not $daemonRunning) {
    Write-Host "Building peko..." -ForegroundColor Cyan
    pushd "$PSScriptRoot/../.."
    $env:RUSTFLAGS = "-A warnings"
    cargo build --quiet
    if ($LASTEXITCODE -ne 0) {
        Write-Error "Build failed"
        exit 1
    }
    popd
} else {
    Write-Host "Daemon already running, skipping build..." -ForegroundColor Cyan
}

# Reset peko config data
$pekoDir = "$env:USERPROFILE/.peko"
$DataDir = "$env:APPDATA/peko"
if (Test-Path $pekoDir) { Remove-Item -Recurse -Force $pekoDir }
if (Test-Path $DataDir) { Remove-Item -Recurse -Force $DataDir }

# Set API key
peko auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create agents
$delegator = "a2a_async_delegator"
$worker = "a2a_async_worker"
peko agent create $delegator --provider $Provider 2>&1 | Out-Null
peko agent create $worker --provider $Provider 2>&1 | Out-Null
Write-Host "Created agents: $delegator, $worker" -ForegroundColor Green

# Enable tools
peko ext enable read_file --target default/$worker 2>&1 | Out-Null
peko ext enable a2a_send --target default/$delegator 2>&1 | Out-Null
Write-Host "Enabled read_file for worker, a2a_send for delegator" -ForegroundColor Green

# Create a test file in the worker's per-agent workspace
# (AgentService sets config.workspace to per-agent dir when creating agents)
$workerWorkspace = "$env:APPDATA/peko/workspaces/default/$worker"
New-Item -ItemType Directory -Path $workerWorkspace -Force | Out-Null
"A2A_ASYNC_SECRET_99" | Set-Content -Path "$workerWorkspace/test_async.txt" -NoNewline
Write-Host "Created test file in worker workspace: $workerWorkspace" -ForegroundColor Green

# Track pass/fail
$allPassed = $true

try {
    # ============================================================
    # TEST 1: Async A2A send returns a task receipt
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 1: Async A2A send returns receipt" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    # Verify worker has no sessions before
    $workerSessionsBefore = peko session list $worker --json 2>&1 | ConvertFrom-Json
    $sessionCountBefore = $workerSessionsBefore.sessions.Count
    Write-Host "Worker sessions before: $sessionCountBefore" -ForegroundColor Gray

    $prompt = @"
Use the a2a_send tool with _async=true to send this message to agent '$worker':
Read the file test_async.txt and report its exact contents.
The tool should return a JSON receipt immediately containing a task_file path and a task_id. Read the receipt carefully. If you received a receipt with task_file and task_id, reply exactly ASYNC_RECEIPT_OK and include the task_file path. If you did not get a receipt, reply exactly ASYNC_RECEIPT_FAIL and explain what happened.
"@

    $stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
    $response = peko send $delegator $prompt --no-stream 2>&1
    $stopwatch.Stop()
    Write-Host "Response: $response" -ForegroundColor Gray
    Write-Host "Elapsed: $($stopwatch.Elapsed.TotalSeconds.ToString('F1'))s" -ForegroundColor Gray

    $receiptOk = $response -match "ASYNC_RECEIPT_OK"
    $receiptFail = $response -match "ASYNC_RECEIPT_FAIL"

    if ($receiptOk) {
        Write-Host "PASS: Async receipt was returned" -ForegroundColor Green
    } elseif ($receiptFail) {
        Write-Host "FAIL: Async receipt was NOT returned" -ForegroundColor Red
        $allPassed = $false
    } else {
        Write-Host "Result unclear" -ForegroundColor Yellow
        $allPassed = $false
    }

    # ============================================================
    # TEST 2: Task file exists and is pollable
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 2: Task file written for polling" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $asyncTasksDir = "$env:APPDATA/peko/async_tasks"
    $taskFiles = @()
    if (Test-Path $asyncTasksDir) {
        $taskFiles = Get-ChildItem -Path $asyncTasksDir -Filter "*.json" -ErrorAction SilentlyContinue
    }

    if ($taskFiles.Count -gt 0) {
        $latestTask = $taskFiles | Sort-Object LastWriteTime -Descending | Select-Object -First 1
        Write-Host "Latest task file: $($latestTask.Name)" -ForegroundColor Gray

        $taskContent = Get-Content $latestTask.FullName -Raw | ConvertFrom-Json
        Write-Host "Task status: $($taskContent.status)" -ForegroundColor Gray
        Write-Host "Task tool: $($taskContent.tool_name)" -ForegroundColor Gray

        if ($taskContent.tool_name -eq "a2a_send") {
            Write-Host "PASS: a2a_send task file was written" -ForegroundColor Green
        } else {
            Write-Host "FAIL: Latest task is not a2a_send (tool: $($taskContent.tool_name))" -ForegroundColor Red
            $allPassed = $false
        }
    } else {
        Write-Host "FAIL: No task files found" -ForegroundColor Red
        $allPassed = $false
    }

    # ============================================================
    # TEST 3: Async task eventually completes (worker session created)
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 3: Async task completion" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $maxWait = 60
    $waited = 0
    $completed = $false
    while ($waited -lt $maxWait) {
        $workerSessions = peko session list $worker --json 2>&1 | ConvertFrom-Json
        if ($workerSessions.sessions.Count -gt $sessionCountBefore) {
            $completed = $true
            break
        }
        Start-Sleep -Seconds 2
        $waited += 2
    }

    if ($completed) {
        Write-Host "PASS: Async task completed — worker session created within ${waited}s" -ForegroundColor Green
    } else {
        Write-Host "FAIL: Worker session not created within ${maxWait}s" -ForegroundColor Red
        $allPassed = $false
    }

    # ============================================================
    # TEST 4: Caller annotation in async target session
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "TEST 4: Caller annotation in async target session" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    $workerSessions = peko session list $worker --json 2>&1 | ConvertFrom-Json
    if ($workerSessions.sessions.Count -gt 0) {
        $workerSessionId = $workerSessions.sessions[0].session_id
        $historyOutput = peko session show $worker --session-id $workerSessionId --history --json 2>&1
        # Handle case where command outputs error text before JSON
        $jsonStart = $historyOutput.IndexOf('{')
        if ($jsonStart -ge 0) {
            $historyJson = $historyOutput.Substring($jsonStart) | ConvertFrom-Json
        } else {
            $historyJson = $historyOutput | ConvertFrom-Json
        }

        $hasAnnotation = $false
        foreach ($entry in $historyJson.history) {
            $msg = $entry.Message
            if ($msg.role -eq "user" -and $msg.content -match "\[Message from agent: $delegator\]") {
                $hasAnnotation = $true
                break
            }
        }

        if ($hasAnnotation) {
            Write-Host "PASS: Caller annotation found in target session" -ForegroundColor Green
        } else {
            Write-Host "FAIL: Caller annotation not found in session history" -ForegroundColor Red
            $allPassed = $false
        }
    } else {
        Write-Host "FAIL: No worker sessions to check for annotation" -ForegroundColor Red
        $allPassed = $false
    }

} finally {
    # ============================================================
    # Cleanup
    # ============================================================
    Write-Host "`n========================================" -ForegroundColor Cyan
    Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
    Write-Host "========================================" -ForegroundColor Cyan

    peko agent delete $delegator --force 2>&1 | Out-Null
    peko agent delete $worker --force 2>&1 | Out-Null
    Write-Host "Deleted test agents" -ForegroundColor Green
}

if ($allPassed) {
    Write-Host "`nA2A async test passed!" -ForegroundColor Green
    exit 0
} else {
    Write-Host "`nA2A async test failed!" -ForegroundColor Red
    exit 1
}
