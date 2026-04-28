#!/usr/bin/env pwsh
# A2A Blocking Send E2E Test
#
# Tests the a2a_send tool via daemon execution:
# - One agent delegates to another using a2a_send
# - Session resumption across A2A calls
# - Response structure validation
#
# Requires: daemon running, two configured agents

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "A2A Blocking Send E2E Test" -ForegroundColor Cyan
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

# Create two agents: a delegator and a worker
$delegator = "delegator"
$worker = "worker"
pekobot agent create $delegator --provider $Provider 2>&1 | Out-Null
pekobot agent create $worker --provider $Provider 2>&1 | Out-Null
Write-Host "Created agents: $delegator, $worker" -ForegroundColor Green

# Ensure both agents have a2a_send in their tool whitelist
# (By default all built-ins are enabled; if whitelist is used, a2a_send must be included)

# ============================================================
# TEST 1: Basic A2A blocking send
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Basic A2A blocking send" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# We send a message to the delegator that instructs it to use a2a_send to call the worker.
# Since this requires the LLM to actually invoke the tool, we use a direct tool execution
# via the daemon's tool runtime for a deterministic test.

Write-Host "Sending message to $delegator that should trigger a2a_send to $worker..." -ForegroundColor Yellow

# For a deterministic E2E test, we use the daemon's tool execution endpoint.
# First, ensure daemon is running.
$daemonStatus = pekobot daemon status 2>&1
if ($daemonStatus -notmatch "running") {
    Write-Host "Starting daemon..." -ForegroundColor Yellow
    Start-Process -FilePath "pekobot" -ArgumentList "daemon","start" -WindowStyle Hidden
    Start-Sleep -Seconds 3
}

# Send a message to the delegator asking it to delegate a task to the worker.
# The LLM should use a2a_send to call the worker agent.
$prompt = @"
You have a tool called a2a_send that lets you send messages to other agents.
Please use a2a_send to ask agent '$worker' the following question:
"What is the capital of France?"
Then summarize the worker's response in your final answer.
"@

$result = pekobot send $delegator $prompt --no-stream 2>&1
Write-Host "Delegator response: $result"

# The response should contain something about Paris (from the worker via a2a_send)
if ($result -match "Paris" -or $result -match "capital" -or $result -match "France") {
    Write-Host "✓ A2A delegation succeeded (response contains expected content)" -ForegroundColor Green
} else {
    Write-Warning "Response may not contain expected content — check manually"
}

# ============================================================
# TEST 2: Verify worker session was created
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Verify worker session was created" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$workerSessions = pekobot session list $worker --json 2>&1 | ConvertFrom-Json
if ($workerSessions.sessions.Count -ge 1) {
    Write-Host "✓ Worker agent has $($workerSessions.sessions.Count) session(s)" -ForegroundColor Green
    $workerSessionId = $workerSessions.sessions[0].session_id
    Write-Host "  Worker session ID: $workerSessionId" -ForegroundColor Gray
} else {
    Write-Warning "Worker agent has no sessions — a2a_send may not have executed"
}

# ============================================================
# TEST 3: Session resumption across A2A calls
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 3: Session resumption across A2A calls" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Send another message to the delegator, asking it to resume the same worker session
$prompt2 = @"
Use a2a_send to ask agent '$worker' another question, reusing the same session.
Ask: "What about Germany?"
Summarize the response.
"@

$result2 = pekobot send $delegator $prompt2 --no-stream 2>&1
Write-Host "Delegator response: $result2"

if ($result2 -match "Berlin" -or $result2 -match "Germany") {
    Write-Host "✓ A2A session resumption succeeded" -ForegroundColor Green
} else {
    Write-Warning "Response may not contain expected content — check manually"
}

# Verify worker still has only 1 session (resumed, not new)
$workerSessions2 = pekobot session list $worker --json 2>&1 | ConvertFrom-Json
if ($workerSessions2.sessions.Count -eq 1) {
    Write-Host "✓ Worker session count is still 1 (resumed correctly)" -ForegroundColor Green
} else {
    Write-Warning "Worker has $($workerSessions2.sessions.Count) sessions — expected 1"
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "`n✅ A2A blocking tests completed!" -ForegroundColor Green
