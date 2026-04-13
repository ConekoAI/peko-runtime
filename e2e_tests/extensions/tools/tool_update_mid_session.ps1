#!/usr/bin/env pwsh
# ADR-019 Phase 1: Tool Permission Enforcement E2E Test
#
# Tests the "Session-Level Dynamic Tool Registration" capability:
# https://github.com/moonshot-ai/Kimi-Chat/blob/main/docs/architecture/ADR-019-Dynamic-Tool-Registration.md
#
# Scenario A: Tool disabled at session start → enabled mid-session
# - System prompt injected without tool X description
# - Provider tool schema doesn't include tool X
# - Even if enabled later, LLM doesn't know about it
# - User must restart session
#
# Scenario B: Tool enabled at session start → disabled mid-session  
# - System prompt has tool X description
# - Provider has tool X schema
# - Tool is disabled in execution layer
# - LLM can still "call" the tool, but it will fail/get rejected


param(
    [string]$Provider = "minimax"
)

$ErrorActionPreference = "Stop"

Write-Host "========================================" -ForegroundColor Cyan
Write-Host "Scenario: Dynamic Tool Registration" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Check prerequisites
if (-not $env:MINIMAX_API_KEY -and $Provider -eq "minimax") {
    Write-Error "MINIMAX_API_KEY environment variable not set"
    exit 1
}

# Build pekobot
Write-Host "Building pekobot..." -ForegroundColor Cyan
pushd "$PSScriptRoot/../.."
$env:RUSTFLAGS = "-A warnings"
cargo build --quiet
if ($LASTEXITCODE -ne 0) {
    Write-Error "Build failed"
    exit 1
}
popd

# Reset pekobot config data
$pekobotDir = "$env:USERPROFILE/.pekobot"
$DataDir = "$env:APPDATA/pekobot"
if (Test-Path $pekobotDir) { Remove-Item -Recurse -Force $pekobotDir }
if (Test-Path $DataDir) { Remove-Item -Recurse -Force $DataDir }

# Set API key
pekobot auth set $Provider $env:MINIMAX_API_KEY 2>&1 | Out-Null
Write-Host "Set API key for $Provider" -ForegroundColor Green

Write-Host ""
Write-Host "========================================" -ForegroundColor Cyan
Write-Host "TEST: Whitelist Enforcement" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

# Create agent WITHOUT shell (only glob and read_file)
$agentName = "adr019_test"
pekobot agent create $agentName --provider $Provider 2>&1 | Out-Null
Write-Host "Created agent: $agentName" -ForegroundColor Green

# Enable glob and read_file and disable shell for this agent
pekobot ext enable glob --target default/$agentName 2>&1 | Out-Null
pekobot ext enable read_file --target default/$agentName 2>&1 | Out-Null
pekobot ext disable shell --target default/$agentName 2>&1 | Out-Null
Write-Host "Enabled for agent: glob, read_file" -ForegroundColor Green
Write-Host "NOT enabled: shell" -ForegroundColor Red

# Test 1: Try to use allowed tool (glob)
Write-Host ""
Write-Host "TEST 1: Using allowed tool (glob)..." -ForegroundColor Cyan
$response1 = pekobot send $agentName "I'm testing tool calling accessibility update mid-session. invoke the tools as requested and report on their availability faithfully. First, try using the glob tool to find all files in the current directory. If you don't have glob tool access, reply exactly GLOB_BLOCKED, otherwise reply GLOB_SUCCESS." 2>&1
Write-Host "Response: $response1" -ForegroundColor Gray

$globWorked = $response1 -match "GLOB_SUCCESS"
$globBlocked = $response1 -match "GLOB_BLOCKED"
if ($globWorked) {
    Write-Host "✅ PASS: glob tool worked (in whitelist)" -ForegroundColor Green
} elseif ($globBlocked) {
    Write-Host "❌ FAIL: glob tool was blocked" -ForegroundColor Red
} else {
    Write-Host "⚠️ glob result unclear" -ForegroundColor Yellow
}

# Test 2: Try to use blocked tool (shell)
Write-Host ""
Write-Host "TEST 2: Using blocked tool (shell - not enabled)..." -ForegroundColor Cyan
$response2 = pekobot send $agentName "Try to use the shell tool to list files. If you don't have shell tool access, reply exactly SHELL_BLOCKED, otherwise reply SHELL_SUCCESS" 2>&1
Write-Host "Response: $response2" -ForegroundColor Gray

# Check if LLM knows it doesn't have shell
$shellBlocked = $response2 -match "SHELL_BLOCKED"
$shellWorked = $response2 -match "SHELL_SUCCESS"

if ($shellBlocked) {
    Write-Host "✅ PASS: LLM knows shell is not available" -ForegroundColor Green
} elseif ($shellWorked) {
    Write-Host "❌ FAIL: LLM thinks shell is available" -ForegroundColor Red
}
else {
    Write-Host "⚠️ shell result unclear" -ForegroundColor Yellow
}

# disable glob mid-session and enable shell mid-session
pekobot ext disable glob --target default/$agentName 2>&1 | Out-Null
pekobot ext enable shell --target default/$agentName 2>&1 | Out-Null
Write-Host "Mid-session update: Disabled glob, Enabled shell" -ForegroundColor Cyan

# Test 3: Try to use newly enabled tool (shell)
Write-Host ""
Write-Host "TEST 3: Using newly enabled tool (shell)..." -ForegroundColor Cyan
$response3 = pekobot send $agentName "Your tool access has been updated. Try to use the shell tool to list files in your workspace. If you don't have shell tool access, reply exactly SHELL_BLOCKED, otherwise reply SHELL_SUCCESS" 2>&1
Write-Host "Response: $response3" -ForegroundColor Gray

$shellWorked = $response3 -match "SHELL_SUCCESS"
$shellBlocked = $response3 -match "SHELL_BLOCKED"
if ($shellWorked) {
    Write-Host "✅ PASS: shell tool worked after mid-session enable" -ForegroundColor Green
} elseif ($shellBlocked) {
    Write-Host "❌ FAIL: shell tool was blocked" -ForegroundColor Red
} else {
    Write-Host "⚠️ shell result unclear" -ForegroundColor Yellow
}

# Test 4: Verify glob is now blocked (disabled mid-session)
Write-Host ""
Write-Host "TEST 4: Verify previously enabled tool is now blocked (glob disabled)..." -ForegroundColor Cyan
$response4 = pekobot send $agentName "Try to use the glob tool to list files. If glob is not available, reply exactly: GLOB_BLOCKED, otherwise reply GLOB_SUCCESS" 2>&1
Write-Host "Response: $response4" -ForegroundColor Gray

$globWorked = $response4 -match "GLOB_SUCCESS"
$globBlocked = $response4 -match "GLOB_BLOCKED|not have glob|no glob access"
if ($globBlocked) {
    Write-Host "✅ PASS: glob tool correctly blocked after mid-session disable" -ForegroundColor Green
} elseif ($globWorked) {
    Write-Host "❌ FAIL: glob tool was not blocked" -ForegroundColor Red
} else {
    Write-Host "⚠️ glob result unclear" -ForegroundColor Yellow
}

#========================================
# Cleanup
#========================================
Write-Host "`n========================================" -ForegroundColor Cyan
Write-Host "Test Complete - Cleaning up" -ForegroundColor Cyan
Write-Host "========================================" -ForegroundColor Cyan

pekobot agent delete $agentName --force 2>&1 | Out-Null
Write-Host "Deleted test agent: $agentName" -ForegroundColor Green

Write-Host "`n✅ Dynamic Tool Registration mid-session test completed!" -ForegroundColor Green
