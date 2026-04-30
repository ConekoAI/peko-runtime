#!/usr/bin/env pwsh
# Session Resumption and New Session E2E Test (Sample)
#
# This is a basic sample test demonstrating session resumption and creation.
# For comprehensive tests, see the test suite in this directory.

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Session Resumption and New Session Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Error "MINIMAX_API_KEY environment variable not set"
    exit 1
}

# Build pekobot (skip if daemon is running since it locks the binary)
$daemonRunning = $false
try {
    $status = peko daemon status 2>&1
    if ($status -match "Running") { $daemonRunning = $true }
} catch {}

if (-not $daemonRunning) {
    Write-Host "Building pekobot..." -ForegroundColor Cyan
    pushd "$PSScriptRoot/../.."
    $env:RUSTFLAGS = "-A warnings"
    cargo build --quiet
    if ($LASTEXITCODE -ne 0) {
        Write-Host "Build failed"
        exit 1
    }
    popd
} else {
    Write-Host "Daemon already running, skipping build..." -ForegroundColor Cyan
}

# Reset pekobot config data (Windows)
$pekobotDir = "$env:USERPROFILE/.pekobot"
if (Test-Path $pekobotDir) {
    Remove-Item -Recurse -Force $pekobotDir
    Write-Host "Reset .pekobot directory" -ForegroundColor Yellow
}

# Set minimax api key
pekobot auth set minimax $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create an agent with minimax provider
$agentName = "testagent"
pekobot agent create $agentName --provider $Provider 2>&1 | Out-Null
Write-Host "Created agent: $agentName" -ForegroundColor Green

# list agents
Write-Host "`nAgent list:" -ForegroundColor Cyan
pekobot agent list 2>&1

# send a message to the agent (creates first session)
Write-Host "`nSending first message..." -ForegroundColor Cyan
pekobot send $agentName "what's USA's capital" 2>&1

# send a new message to the agent (start a new session)
Write-Host "`nSending new message (new session)..." -ForegroundColor Cyan
pekobot send $agentName "how many legs does a spider have?"--new 2>&1

# Get session list - should show 1 session with more messages
Write-Host "`nSession list (should show 2 sessions):" -ForegroundColor Cyan
pekobot session list $agentName 2>&1

# Get session ids
$jsonOutput = pekobot session list $agentName --json 2>&1 | ConvertFrom-Json
$sessionId1 = $jsonOutput.sessions[1].session_id
$sessionId2 = $jsonOutput.sessions[0].session_id

# switch to first session and send a follow-up message (resumes first session)
Write-Host "`nSwitching to first session and sending follow-up message..." -ForegroundColor Cyan
pekobot session switch $agentName $sessionId1 2>&1
pekobot send $agentName "what about France" 2>&1

# Get session list - should show 2 sessions with updated timestamps
Write-Host "`nSession list after switching and sending message:" -ForegroundColor Cyan
pekobot session list $agentName 2>&1


# switch to second session and send a follow-up message (resumes second session)
Write-Host "`nSwitching to second session and sending follow-up message..." -ForegroundColor Cyan
pekobot session switch $agentName $sessionId2 2>&1
pekobot send $agentName "what about a crab?" 2>&1

# Get session list - should show 2 sessions with updated timestamps
Write-Host "`nSession list after switching and sending message:" -ForegroundColor Cyan
pekobot session list $agentName 2>&1


# Cleanup
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

pekobot agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted test agent" -ForegroundColor Green

Write-Host "`n✅ Test completed successfully!" -ForegroundColor Green
