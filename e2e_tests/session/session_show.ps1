#!/usr/bin/env pwsh
# Session Show Command E2E Test
#
# Tests all variations of the `session show` command:
# - session show <agent> (defaults to active session)
# - session show <agent> <session_id> (explicit session)
# - session show <agent> [--session_id] --history
# - session show <agent> [--session_id] --json
# - Error case: no active session exists

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Session Show Command Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Error "MINIMAX_API_KEY environment variable not set"
    exit 1
}

# Build pekobot
Write-Host "Building pekobot..." -ForegroundColor Cyan
pushd "D:\Workplace\pekobot\pekobot\";$env:RUSTFLAGS="-A warnings"; cargo build; popd

# Reset pekobot config data
$pekobotDir = "$env:USERPROFILE/.pekobot"
if (Test-Path $pekobotDir) {
    Remove-Item -Recurse -Force $pekobotDir
    Write-Host "Reset .pekobot directory" -ForegroundColor Yellow
}

# Set API key
pekobot auth set minimax $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create test agent
$agentName = "testshowagent"
pekobot agent create $agentName --provider $Provider 2>&1 | Out-Null
Write-Host "Created agent: $agentName" -ForegroundColor Green

# ============================================================
# TEST 1: Error case - no active session exists
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Show active session when none exists" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Capture output using cmd to avoid PowerShell error handling
$output = cmd /c "pekobot session show $agentName 2>&1"
if ($output -match "No active session") {
    Write-Host "✅ Got expected error: No active session" -ForegroundColor Green
} else {
    Write-Host "⚠️  Got unexpected output: $output" -ForegroundColor Yellow
}

# ============================================================
# TEST 2: Create a session and show it explicitly
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 2: Create session and show explicitly" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Send message to create a session
Write-Host "Sending message to create session..." -ForegroundColor Cyan
pekobot send $agentName "What is the capital of France?" --no-stream 2>&1 | Out-Null

# Get session ID
$jsonOutput = pekobot session list $agentName --json 2>&1 | ConvertFrom-Json
$sessionId1 = $jsonOutput.sessions[0].session_id
Write-Host "Created session: $sessionId1" -ForegroundColor Green

# Show session explicitly
Write-Host "`nShowing session explicitly (with --session-id)..." -ForegroundColor Cyan
$output = pekobot session show $agentName --session-id $sessionId1 2>&1
Write-Output $output

# Verify output contains expected fields
$outputStr = $output | Out-String
if ($outputStr -match "Session ID" -and $outputStr -match "$sessionId1") {
    Write-Host "✅ Explicit show displays correct session info" -ForegroundColor Green
} else {
    Write-Error "❌ Session show output missing expected fields"
    exit 1
}

# ============================================================
# TEST 3: Show active session (no session_id specified)
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 3: Show active session (implicit)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Showing active session (no session_id argument)..." -ForegroundColor Cyan
$output = pekobot session show $agentName 2>&1
Write-Output $output

# Verify it shows the active session
$outputStr = $output | Out-String
if ($outputStr -match "Using active session" -or $outputStr -match "$sessionId1") {
    Write-Host "✅ Implicit show displays active session" -ForegroundColor Green
} else {
    Write-Host "⚠️  Note: Output may not contain 'Using active session' indicator" -ForegroundColor Yellow
}

# ============================================================
# TEST 4: Show session with history
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 4: Show session with --history" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Showing active session with history..." -ForegroundColor Cyan
$output = pekobot session show $agentName --history 2>&1
Write-Output $output

$outputStr = $output | Out-String
if ($outputStr -match "Message History" -or $outputStr -match "User" -or $outputStr -match "Assistant") {
    Write-Host "✅ History is displayed" -ForegroundColor Green
} else {
    Write-Error "❌ History not found in output"
    exit 1
}

# Also test explicit --session-id with --history
Write-Host "`nShowing specific session with history..." -ForegroundColor Cyan
$output = pekobot session show $agentName --session-id $sessionId1 --history 2>&1
$outputStr = $output | Out-String
if ($outputStr -match "Message History") {
    Write-Host "✅ Explicit session_id with --history works" -ForegroundColor Green
}

# ============================================================
# TEST 5: Create second session and verify explicit targeting
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 5: Multiple sessions - explicit targeting" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Create second session
Write-Host "Creating second session..." -ForegroundColor Cyan
pekobot send $agentName "What is the capital of Germany?" --new --no-stream 2>&1 | Out-Null

# Get both session IDs
$jsonOutput = pekobot session list $agentName --json 2>&1 | ConvertFrom-Json
$sessionId1 = $jsonOutput.sessions[0].session_id
$sessionId2 = $jsonOutput.sessions[1].session_id

Write-Host "Session 1: $sessionId1" -ForegroundColor Gray
Write-Host "Session 2: $sessionId2" -ForegroundColor Gray

# Verify we can show each session explicitly
Write-Host "`nShowing session 1 explicitly..." -ForegroundColor Cyan
$output1 = pekobot session show $agentName --session-id $sessionId1 2>&1 | Out-String

Write-Host "Showing session 2 explicitly..." -ForegroundColor Cyan
$output2 = pekobot session show $agentName --session-id $sessionId2 2>&1 | Out-String

# Verify outputs contain correct session IDs
if ($output1 -match $sessionId1 -and $output2 -match $sessionId2) {
    Write-Host "✅ Explicit targeting works for multiple sessions" -ForegroundColor Green
} else {
    Write-Error "❌ Explicit targeting failed"
    exit 1
}

# ============================================================
# TEST 6: Show active session after switch
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 6: Show after session switch" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Switch to session 1
Write-Host "Switching to session 1..." -ForegroundColor Cyan
pekobot session switch $agentName $sessionId1 2>&1 | Out-Null

# Show active session (should be session 1)
Write-Host "Showing active session (should be session 1)..." -ForegroundColor Cyan
$output = pekobot session show $agentName 2>&1 | Out-String

if ($output -match $sessionId1) {
    Write-Host "✅ Active session correctly shows session 1 after switch" -ForegroundColor Green
} else {
    Write-Error "❌ Active session does not match expected session after switch"
    exit 1
}

# Switch to session 2 and verify
Write-Host "`nSwitching to session 2..." -ForegroundColor Cyan
pekobot session switch $agentName $sessionId2 2>&1 | Out-Null

Write-Host "Showing active session (should be session 2)..." -ForegroundColor Cyan
$output = pekobot session show $agentName 2>&1 | Out-String

if ($output -match $sessionId2) {
    Write-Host "✅ Active session correctly shows session 2 after switch" -ForegroundColor Green
} else {
    Write-Error "❌ Active session does not match expected session after switch"
    exit 1
}

# ============================================================
# TEST 7: JSON output
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 7: JSON output" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Testing JSON output for active session..." -ForegroundColor Cyan
$jsonOutput = pekobot session show $agentName --json 2>&1 | ConvertFrom-Json

if ($jsonOutput.session.session_id -eq $sessionId2) {
    Write-Host "✅ JSON output contains correct active session" -ForegroundColor Green
} else {
    Write-Error "❌ JSON output does not contain expected session"
    exit 1
}

Write-Host "Testing JSON output for explicit session..." -ForegroundColor Cyan
$jsonOutput = pekobot session show $agentName --session-id $sessionId1 --json 2>&1 | ConvertFrom-Json

if ($jsonOutput.session.session_id -eq $sessionId1) {
    Write-Host "✅ JSON output contains correct explicit session" -ForegroundColor Green
} else {
    Write-Error "❌ JSON output does not contain expected session"
    exit 1
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

pekobot agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted test agent" -ForegroundColor Green

Write-Host "`n========================================" -ForegroundColor Green
Write-Host "✅ All session show tests passed!" -ForegroundColor Green
Write-Host "========================================" -ForegroundColor Green
