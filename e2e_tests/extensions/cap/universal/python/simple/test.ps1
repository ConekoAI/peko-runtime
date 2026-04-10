#!/usr/bin/env pwsh
# Simplified Universal Tool E2E Test - Python
#
# This test demonstrates the simple CLI-based workflow:
# 1. Create agent
# 2. Install tool with 'cap universal install'
# 3. Enable tool with 'cap enable'
# 4. Test tool via agent

param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Simple Universal Tool E2E Test (Python)" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Error "MINIMAX_API_KEY environment variable not set"
    exit 1
}

# Check Python
$pythonCmd = if (Get-Command "python3" -ErrorAction SilentlyContinue) { "python3" } elseif (Get-Command "python" -ErrorAction SilentlyContinue) { "python" } else { $null }
if (-not $pythonCmd) {
    Write-Error "Python not found in PATH"
    exit 1
}
Write-Host "Using Python: $pythonCmd" -ForegroundColor Green

# Build pekobot
Write-Host "Building pekobot..." -ForegroundColor Cyan
pushd "$PSScriptRoot/../../../"
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

# Reset pekobot data
$dataDir = "$env:USERPROFILE/AppData/Roaming/pekobot"
if (Test-Path $dataDir) {
    Remove-Item -Recurse -Force $dataDir
    Write-Host "Reset data directory" -ForegroundColor Yellow
}

# Set API key
pekobot auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

# ============================================================
# STEP 1: Create agent
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 1: Create agent" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$agentName = "calc_agent"
$teamName = "default"

Write-Host "Creating agent: $agentName" -ForegroundColor Yellow
pekobot agent create $agentName --provider $Provider --force 2>&1 | Out-Null
Write-Host "Created agent" -ForegroundColor Green

# # Update AGENT.md to document the tool
# $agentDir = "$env:USERPROFILE/.pekobot/teams/default/agents/$agentName"
# $agentMd = @"
# # Calculator Agent

# An agent that can perform arithmetic calculations.

# ## Available Tools

# - shell: Execute shell commands
# - calculator_simple: Perform arithmetic calculations (add, subtract, multiply, divide)
# "@
# $agentMd | Out-File -FilePath "$agentDir/AGENT.md" -Encoding utf8
# Write-Host "Updated AGENT.md" -ForegroundColor Green

# ============================================================
# STEP 2: Install tool as extension
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 2: Install tool as extension" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

$toolDir = "$PSScriptRoot"
Write-Host "Installing calculator_simple as universal-tool extension..." -ForegroundColor Yellow

# Install the tool as a universal-tool extension
$installResult = pekobot ext install $toolDir --type universal-tool 2>&1
Write-Host $installResult

# Verify installation
$extList = pekobot ext list --type universal-tool 2>&1
if ($extList -match "calculator_simple") {
    Write-Host "✓ Tool extension installed successfully" -ForegroundColor Green
} else {
    Write-Error "Tool extension installation failed"
}

# ============================================================
# STEP 3: Enable tool extension
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 3: Enable tool extension" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Enabling calculator_simple extension..." -ForegroundColor Yellow
pekobot ext enable calculator_simple 2>&1 | Out-Null
Write-Host "Enabled tool extension" -ForegroundColor Green

# Verify
$extInfo = pekobot ext info calculator_simple 2>&1
Write-Host "`nExtension status:" -ForegroundColor Cyan
Write-Host $extInfo

# ============================================================
# STEP 4: Test tool via agent
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "STEP 4: Test tool via agent" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

Write-Host "Sending calculation request to agent..." -ForegroundColor Yellow
Measure-Command {
    $response = pekobot send $agentName "Calculate 25 multiplied by 4 using calculator_simple" --no-stream 2>&1
}
Write-Host "Agent response: $response"

# Check session
$sessions = pekobot session list $agentName --json 2>&1 | ConvertFrom-Json
if ($sessions.sessions.Count -ge 1) {
    Write-Host "✓ Session created" -ForegroundColor Green
    $sessionId = $sessions.sessions[0].session_id
    
    # Check session for tool call
    $sessionFile = "$env:USERPROFILE/AppData/Roaming/pekobot/sessions/default/$agentName/${sessionId}.jsonl"
    if (Test-Path $sessionFile) {
        $content = Get-Content $sessionFile -Raw
        if ($content -match "calculator_simple") {
            Write-Host "✓ Tool was called in session" -ForegroundColor Green
        }
    }
} else {
    Write-Host "⚠ No session found" -ForegroundColor Yellow
}

# ============================================================
# Cleanup
# ============================================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Cleanup" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Uninstall tool extension
pekobot ext uninstall calculator_simple --force 2>&1 | Out-Null
Write-Host "Uninstalled tool extension" -ForegroundColor Green

# Delete agent
pekobot agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted agent" -ForegroundColor Green

Write-Host "`n✅ Simple E2E test completed!" -ForegroundColor Green
