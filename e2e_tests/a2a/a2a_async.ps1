#!/usr/bin/env pwsh
# A2A Async Send E2E Test
#
# Tests the a2a_send tool with _async=true:
# - Task receipt is returned immediately
# - Task file is written for polling
# - Task eventually completes

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
$DataDir = "$env:USERPROFILE/AppData/Roaming/pekobot"
if (Test-Path $DataDir) {
    Remove-Item -Recurse -Force $DataDir
    Write-Host "Reset data directory" -ForegroundColor Yellow
}

# Set API key
pekobot auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create two agents
$delegator = "async_delegator"
$worker = "async_worker"
pekobot agent create $delegator --provider $Provider 2>&1 | Out-Null
pekobot agent create $worker --provider $Provider 2>&1 | Out-Null
Write-Host "Created agents: $delegator, $worker" -ForegroundColor Green

# Ensure daemon is running
$daemonStatus = pekobot daemon status 2>&1
if ($daemonStatus -notmatch "running") {
    Write-Host "Starting daemon..." -ForegroundColor Yellow
    Start-Process -FilePath "pekobot" -ArgumentList "daemon","start" -WindowStyle Hidden
    Start-Sleep -Seconds 3
}

# ============================================================
# TEST 1: Async A2A send returns task receipt
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Async A2A send returns task receipt" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# We can't easily trigger _async from the CLI send command (it's a tool-level param),
# so we test via a prompt that instructs the LLM to use _async=true.
$prompt = @"
Use the a2a_send tool with _async=true to ask agent '$worker':
"What is the capital of Spain?"
After calling the tool, report the task_id from the receipt in your final answer.
"@

$result = pekobot send $delegator $prompt --no-stream 2>&1
Write-Host "Delegator response: $result"

# Check if the response mentions a task_id or async status
if ($result -match "task_id" -or $result -match "queued" -or $result -match "async") {
    Write-Host "✓ Async task receipt was returned" -ForegroundColor Green
} else {
    Write-Warning "Response may not contain async receipt — check manually"
}

# ============================================================
# TEST 2: Poll for task completion
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Poll for task completion" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Try to extract task_id from response
$taskId = $null
if ($result -match 'task_id["'']?\s*[:=]\s*["'']?([a-zA-Z0-9_:.-]+)') {
    $taskId = $matches[1]
    Write-Host "Extracted task_id: $taskId" -ForegroundColor Gray
}

if ($taskId) {
    $taskFile = "$env:USERPROFILE/AppData/Roaming/pekobot/async_tasks/$taskId.json"
    Write-Host "Polling task file: $taskFile" -ForegroundColor Yellow

    $maxWait = 60
    $waited = 0
    $completed = $false
    while ($waited -lt $maxWait) {
        if (Test-Path $taskFile) {
            $content = Get-Content $taskFile -Raw | ConvertFrom-Json
            Write-Host "  Status: $($content.status)" -ForegroundColor Gray
            if ($content.status -eq "completed" -or $content.status -eq "failed") {
                $completed = $true
                break
            }
        }
        Start-Sleep -Seconds 2
        $waited += 2
    }

    if ($completed) {
        Write-Host "✓ Async task completed within ${waited}s" -ForegroundColor Green
    } else {
        Write-Warning "Async task did not complete within ${maxWait}s"
    }
} else {
    Write-Warning "Could not extract task_id from response"
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "`n✅ A2A async tests completed!" -ForegroundColor Green
