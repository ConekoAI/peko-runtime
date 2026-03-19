#!/usr/bin/env pwsh
# E2E Test: Session Resumption
# Following the test_session_res.sh flow

$ErrorActionPreference = "Stop"

$PEKOBOT_DIR = "D:\Workplace\pekobot\pekobot"
$PEKOBOT = "$PEKOBOT_DIR\target\release\pekobot.exe"

# Get KIMI_API_KEY from environment
$KIMI_API_KEY = $env:KIMI_API_KEY
if (-not $KIMI_API_KEY) {
    Write-Error "KIMI_API_KEY not set in environment"
    exit 1
}

Write-Host "========================================"
Write-Host "E2E Test: Session Resumption"
Write-Host "========================================"
Write-Host ""
Write-Host "KIMI_API_KEY: $($KIMI_API_KEY.Substring(0, [Math]::Min(15, $KIMI_API_KEY.Length)))..."
Write-Host ""

# Clean up previous test agent
Write-Host "Cleaning up previous test agent..."
$testAgentDir = "$env:USERPROFILE\.pekobot\teams\default\agents\testagent"
$testWorkspaceDir = "$env:LOCALAPPDATA\pekobot\workspaces\testagent"
if (Test-Path $testAgentDir) { Remove-Item -Recurse -Force $testAgentDir }
if (Test-Path $testWorkspaceDir) { Remove-Item -Recurse -Force $testWorkspaceDir }
Write-Host ""

# Create agent with kimi_code provider
Write-Host "Creating test agent with kimi_code provider..."
& $PEKOBOT agent create testagent --provider kimi_code
Write-Host ""

# Set API key
Write-Host "Setting API key..."
& $PEKOBOT auth set kimi $KIMI_API_KEY
Write-Host ""

# List agents
Write-Host "Listing agents..."
& $PEKOBOT agent list
Write-Host ""

# Test 1: First message
Write-Host "========================================"
Write-Host "Test 1: First message"
Write-Host "Prompt: 'What's USA's Capital?'"
Write-Host "========================================"
Write-Host ""
& $PEKOBOT agent start testagent -M "What's USA's Capital?"
Write-Host ""

# Test 2: Follow-up (tests session resumption)
Write-Host "========================================"
Write-Host "Test 2: Follow-up (session resumption)"
Write-Host "Prompt: 'What about France?'"
Write-Host "========================================"
Write-Host ""
& $PEKOBOT agent start testagent -M "What about France?"
Write-Host ""

# Test 3: New session
Write-Host "========================================"
Write-Host "Test 3: New session (--new flag)"
Write-Host "Prompt: 'What about France?' (should not remember context)"
Write-Host "========================================"
Write-Host ""
& $PEKOBOT agent start testagent --new -M "What about France?"
Write-Host ""

Write-Host "========================================"
Write-Host "All tests completed!"
Write-Host "========================================"
