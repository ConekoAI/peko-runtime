#!/usr/bin/env pwsh
# MiniMax Provider E2E Test
#
# This test verifies the MiniMax provider integration using Anthropic-compatible API.
# MiniMax API docs: https://platform.minimaxi.com/docs/api-reference/text-anthropic-api

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "MiniMax Provider E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:MINIMAX_API_KEY) {
    Write-Error "MINIMAX_API_KEY environment variable not set"
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

# Set minimax api key
pekobot auth set minimax $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create an agent with minimax provider
$agentName = "testagent-minimax"
$result = pekobot agent create $agentName --provider $Provider 2>&1
Write-Host "Response: $result"
Write-Host "Created agent: $agentName" -ForegroundColor Green

# Send a message to the agent (creates first session)
Write-Host "`nSending first message..." -ForegroundColor Cyan
$message = "Hello, can you tell me a short joke?"
Write-Host "User: $message"
Measure-Command {
    $result = pekobot send $agentName $message 2>&1
}
Write-Host "Response: $result"

# Verify we got a response
if ([string]::IsNullOrWhiteSpace($result)) {
    Write-Error "Empty response from MiniMax provider"
    exit 1
}

# Cleanup
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

pekobot agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted test agent" -ForegroundColor Green

Write-Host "`n✅ MiniMax provider test completed successfully!" -ForegroundColor Green
