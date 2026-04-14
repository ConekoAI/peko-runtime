#!/usr/bin/env pwsh
# SessionStatus Tool E2E Test
#
# Tests the SessionStatus tool for retrieving session information.

param(
    [string]$Provider = "kimi"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "SessionStatus Tool E2E Test" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:KIMI_API_KEY -and $Provider -eq "kimi") {
    Write-Error "KIMI_API_KEY environment variable not set"
    exit 1
}

# Build pekobot
Write-Host "Building pekobot..." -ForegroundColor Cyan
pushd "$PSScriptRoot/../../.."
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
$DataDir = "$env:APPDATA/pekobot"
if (Test-Path $DataDir) {
    Remove-Item -Recurse -Force $DataDir
    Write-Host "Reset data directory" -ForegroundColor Yellow
}

# Set API key
pekobot auth set $Provider $env:KIMI_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create agent
$agentName = "sessionstatus_test"
pekobot agent create $agentName --provider $Provider 2>&1 | Out-Null
Write-Host "Created agent: $agentName" -ForegroundColor Green

# Enable session_status tool via extension framework
pekobot ext enable session_status --target default/$agentName 2>&1 | Out-Null
Write-Host "Enabled session_status tool via extension framework" -ForegroundColor Green

# ============================================================
# TEST 1: Basic session status call
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Basic session_status call" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending request to call session_status..." -ForegroundColor Yellow
$response = pekobot send $agentName "Use your session_status tool to get information about the current session. After getting the result, respond TOOL_SUCCESS if you see a session_id and agent_id in the result, otherwise respond TOOL_FAILED." --no-stream 2>&1
Write-Host "Response: $response"

$toolSuccess = $response -match "TOOL_SUCCESS"
$toolFailed = $response -match "TOOL_FAILED"
if ($toolSuccess) {
    Write-Host "✅ PASS: Session status retrieved successfully" -ForegroundColor Green
} elseif ($toolFailed) {
    Write-Host "❌ FAIL: SessionStatus did not work" -ForegroundColor Red
} else {
    Write-Host "⚠️ Result unclear" -ForegroundColor Yellow
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

pekobot agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted test agent" -ForegroundColor Green

Write-Host "`n✅ SessionStatus tool e2e tests completed!" -ForegroundColor Green
