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

# Build peko (assumes Rust toolchain is installed)
Write-Host "Building peko..." -ForegroundColor Cyan
pushd "D:\Workplace\\peko\peko\";$env:RUSTFLAGS="-A warnings"; cargo build; popd

# Reset peko config data (Windows)
$pekoDir = "$env:USERPROFILE/.peko"
if (Test-Path $pekoDir) {
    Remove-Item -Recurse -Force $pekoDir
    Write-Host "Reset .peko directory" -ForegroundColor Yellow
}
# Reset peko data (Windows)
$DataDir = "$env:USERPROFILE/AppData/Roaming/peko"
if (Test-Path $DataDir) {
    Remove-Item -Recurse -Force $DataDir
    Write-Host "Reset data directory" -ForegroundColor Yellow
}


# Set minimax api key
peko auth set minimax $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create an agent with minimax provider
$agentName = "testagent"
peko agent create $agentName --provider $Provider 2>&1 | Out-Null
Write-Host "Created agent: $agentName" -ForegroundColor Green


# send a message to the agent (creates first session)
Write-Host "`nSending first message with streaming..." -ForegroundColor Cyan
peko send $agentName "A ball is thrown horizontally at 10 m/s from a height of 20 m.
Questions:

How long does it take to hit the ground?" 2>&1



Write-Host "`nFirst message sent. Now sending second message no streaming..." -ForegroundColor Cyan
peko send $agentName "How far horizontally does it travel?" --no-stream 2>&1

# # Get session id
# $jsonOutput = peko session list $agentName --json 2>&1 | ConvertFrom-Json
# $sessionId = $jsonOutput.sessions[0].session_id

# # print the session jsonl
# Write-Host "`nSession JSONL:" -ForegroundColor Cyan
# cat "$env:USERPROFILE/AppData/Roaming/peko/sessions/default/$agentName/$sessionId.jsonl"

# Cleanup
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

peko agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted test agent" -ForegroundColor Green

Write-Host "`n✅ Test completed successfully!" -ForegroundColor Green
