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
pekobot send $agentName "What color is an orange?" 2>&1

# send a message to the agent with no-stream flag (creates second session)
Write-Host "`nSending second message..." -ForegroundColor Cyan
pekobot send $agentName "What's USA's Capital?" --new --no-stream 2>&1

Write-Host "`nSending a follow-up message..." -ForegroundColor Cyan
pekobot send $agentName "What about Canada?" --no-stream 2>&1

# Get session id
$jsonOutput = pekobot session list $agentName --json 2>&1 | ConvertFrom-Json
$sessionId1 = $jsonOutput.sessions[1].session_id
$sessionId2 = $jsonOutput.sessions[0].session_id

# print the session jsonl's last few lines to verify different sessions
Write-Host "`nSession 1 JSONL:" -ForegroundColor Cyan
cat "$env:USERPROFILE/AppData/Roaming/pekobot/sessions/default/$agentName/$sessionId1.jsonl" | Select-Object -Last 1
Write-Host "`nSession 2 JSONL:" -ForegroundColor Cyan
cat "$env:USERPROFILE/AppData/Roaming/pekobot/sessions/default/$agentName/$sessionId2.jsonl" | Select-Object -Last 3

# Verify usage tracking for both sessions
Write-Host "`nVerifying usage tracking..." -ForegroundColor Cyan

foreach ($sessionId in @($sessionId1, $sessionId2)) {
    $jsonlPath = "$env:USERPROFILE/AppData/Roaming/pekobot/sessions/default/$agentName/$sessionId.jsonl"
    $content = Get-Content $jsonlPath -Raw
    $events = $content -split "`n" | Where-Object { $_ } | ForEach-Object { $_ | ConvertFrom-Json }
    
    # Find assistant message
    $assistantMsg = $events | Where-Object { 
        $_.type -eq "message.v2" -and $_.role -eq "assistant" 
    } | Select-Object -First 1
    
    if (-not $assistantMsg) {
        Write-Error "No assistant message found in session $sessionId"
        exit 1
    }
    
    # Verify usage is non-zero
    $usage = $assistantMsg.role_metadata.Assistant.usage
    if ($usage.total_tokens -eq 0) {
        Write-Error "Usage tracking failed for $sessionId`: total_tokens is 0"
        exit 1
    }
    Write-Host "  ✓ Usage: input=$($usage.input_tokens), output=$($usage.output_tokens), total=$($usage.total_tokens)" -ForegroundColor Green
    
    # Verify provider and model are non-empty
    $provider = $assistantMsg.role_metadata.Assistant.provider
    $model = $assistantMsg.role_metadata.Assistant.model
    
    if ([string]::IsNullOrWhiteSpace($provider)) {
        Write-Error "Provider is empty for $sessionId"
        exit 1
    }
    if ([string]::IsNullOrWhiteSpace($model)) {
        Write-Error "Model is empty for $sessionId"
        exit 1
    }
    Write-Host "  ✓ Provider: $provider, Model: $model" -ForegroundColor Green
}

# print the session list to verify 2 sessions
Write-Host "`nSession list:" -ForegroundColor Cyan
pekobot session list $agentName 2>&1

# Cleanup
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

pekobot agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted test agent" -ForegroundColor Green

Write-Host "`n✅ Test completed successfully!" -ForegroundColor Green
