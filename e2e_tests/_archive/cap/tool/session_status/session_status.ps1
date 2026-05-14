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

# Build peko
Write-Host "Building peko..." -ForegroundColor Cyan
pushd "$PSScriptRoot/../../.."
$env:RUSTFLAGS = "-A warnings"
cargo build --quiet
if ($LASTEXITCODE -ne 0) {
    Write-Error "Build failed"
    exit 1
}
popd

# Reset peko config data
$pekoDir = "$env:USERPROFILE/.peko"
if (Test-Path $pekoDir) {
    Remove-Item -Recurse -Force $pekoDir
    Write-Host "Reset .peko directory" -ForegroundColor Yellow
}
$DataDir = "$env:APPDATA/peko"
if (Test-Path $DataDir) {
    Remove-Item -Recurse -Force $DataDir
    Write-Host "Reset data directory" -ForegroundColor Yellow
}

# Set API key
peko auth set $Provider $env:KIMI_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# Create agent with coding template
$agentName = "sessionstatus_test"
peko agent create $agentName --provider $Provider 2>&1 | Out-Null
Write-Host "Created agent: $agentName" -ForegroundColor Green

# Enable session_status tool via extension framework
peko ext enable session_status 2>&1 | Out-Null
Write-Host "Enabled session_status tool via extension framework" -ForegroundColor Green

# ============================================================
# TEST 1: Basic session status call
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "TEST 1: Basic session_status call" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending request to call session_status..." -ForegroundColor Yellow
$result = peko send $agentName "Use your session_status tool to get information about the current session. Report the session_id and agent_id from the result." --no-stream 2>&1
Write-Host "Response: $result"

if ($result -match "session" -or $result -match "agent") {
    Write-Host "✓ Session status retrieved successfully" -ForegroundColor Green
} else {
    Write-Warning "⚠ Could not verify session status output"
}

# Get session id
$jsonOutput = peko session list $agentName --json 2>&1 | ConvertFrom-Json
$sessionId = $jsonOutput.sessions[0].session_id

# print the session jsonl
Write-Host "`nSession JSONL (last 5 lines):" -ForegroundColor Cyan
Get-Content "$env:APPDATA/peko/sessions/default/$agentName/$sessionId.jsonl" | Select-Object -Last 5 | ForEach-Object { Write-Host $_ -ForegroundColor Gray }

# Cleanup
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

peko agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted test agent" -ForegroundColor Green

Write-Host "`n✅ SessionStatus tool e2e tests completed!" -ForegroundColor Green
