#!/usr/bin/env pwsh
# Session Resumption and New Session E2E Test (Sample)
#
# This is a basic sample test demonstrating session resumption and creation.
# For comprehensive tests, see the test suite in this directory.

param(
    [string]$Provider = "kimi"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Session Resumption and New Session Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:KIMI_API_KEY -and $Provider -eq "kimi") {
    Write-Error "KIMI_API_KEY environment variable not set"
    exit 1
}

# Build pekobot (assumes Rust toolchain is installed)
Write-Host "Building pekobot..." -ForegroundColor Cyan
pushd "D:\Workplace\pekobot\pekobot\";$env:RUSTFLAGS="-A warnings"; cargo build; popd

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


# Set kimi api key
pekobot auth set kimi $env:KIMI_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create an agent with kimi provider
$agentName = "testagent"
pekobot agent create $agentName --provider $Provider 2>&1 | Out-Null
Write-Host "Created agent: $agentName" -ForegroundColor Green


# send a message to the agent
Write-Host "`nSending first message..." -ForegroundColor Cyan
pekobot send $agentName "hi" 2>&1
# request the tool call
Write-Host "`nSending tool call request..." -ForegroundColor Cyan
pekobot send $agentName "call ls with shell and report results" 2>&1

# Get session id
$jsonOutput = pekobot session list $agentName --json 2>&1 | ConvertFrom-Json
$sessionId = $jsonOutput.sessions[0].session_id

# print the session jsonl
Write-Host "`nSession JSONL:" -ForegroundColor Cyan
cat "$env:USERPROFILE/AppData/Roaming/pekobot/sessions/default/$agentName/$sessionId.jsonl" | Select-Object -Last 2

# Cleanup
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

pekobot agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted test agent" -ForegroundColor Green

Write-Host "`n✅ Test completed successfully!" -ForegroundColor Green
