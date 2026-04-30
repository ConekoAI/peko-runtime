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
        Write-Error "Build failed"
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
# Reset pekobot data (Windows)
$DataDir = "$env:USERPROFILE/AppData/Roaming/pekobot"
if (Test-Path $DataDir) {
    Remove-Item -Recurse -Force $DataDir
    Write-Host "Reset data directory" -ForegroundColor Yellow
}


# Set minimax api key
pekobot auth set minimax $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create an agent with minimax provider
$agentName = "testagent"
pekobot agent create $agentName --provider $Provider 2>&1 | Out-Null
Write-Host "Created agent: $agentName" -ForegroundColor Green


# send a message to the agent (creates first session)
Write-Host "`nSending first message..." -ForegroundColor Cyan
pekobot send $agentName "hi" 2>&1

# print the sessions.json
Write-Host "`nSession JSON:" -ForegroundColor Cyan
cat "$env:USERPROFILE/AppData/Roaming/pekobot/sessions/default/$agentName/sessions.json"

# Cleanup
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

pekobot agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted test agent" -ForegroundColor Green

Write-Host "`n✅ Test completed successfully!" -ForegroundColor Green
